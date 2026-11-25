//! Shared read operations for an already selected repository.

use futures_util::StreamExt;
use serde_json::Value;
use worker::{
    Error, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Response, Result,
};

use crate::{git, s3::uri_encode};

pub const INLINE_MAX: usize = 1024 * 1024;

fn bad(why: &str) -> Error {
    Error::RustError(why.into())
}

/// Resolve HEAD, an unambiguous branch name, or an exact Git commit.
pub fn revision(value: &str) -> std::result::Result<&str, String> {
    if value.is_empty()
        || value.len() > 255
        || value.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.ends_with('.')
                || part.ends_with(".lock")
        })
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
        || (value.starts_with("refs/") && !value.starts_with("refs/heads/"))
        || value == "refs/heads"
    {
        return Err("expected HEAD, a branch name, or a Git commit SHA".into());
    }
    Ok(value)
}

/// Repository-relative paths. Reject dot segments before any URL construction.
pub fn file_path(value: &str) -> std::result::Result<&str, String> {
    if value.len() > 1024
        || value.contains('\\')
        || value.chars().any(char::is_control)
        || value.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err("expected a repository-relative file path without dot segments".into());
    }
    Ok(value)
}

fn github_repo(repo: &str) -> Result<(&str, &str)> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| bad("invalid GitHub repository"))?;
    if [owner, name].iter().any(|s| {
        matches!(*s, "" | "." | "..")
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    }) {
        return Err(bad("invalid GitHub repository"));
    }
    Ok((owner, name))
}

fn github_url(repo: &str, path: &str) -> Result<String> {
    let (owner, name) = github_repo(repo)?;
    Ok(format!(
        "https://api.github.com/repos/{owner}/{name}/{path}"
    ))
}

async fn get(
    url: &str,
    token: Option<&str>,
    accept: &str,
    redirect: RequestRedirect,
) -> Result<Response> {
    let headers = Headers::new();
    headers.set("User-Agent", "pigeon")?;
    headers.set("Accept", accept)?;
    if let Some(t) = token {
        headers.set("Authorization", &format!("Bearer {t}"))?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_redirect(redirect);
    Fetch::Request(Request::new_with_init(url, &init)?)
        .send()
        .await
}

fn check(response: &Response) -> Result<()> {
    if response.status_code() == 200 {
        Ok(())
    } else {
        Err(Error::RustError(format!(
            "GitHub repository read answered {}",
            response.status_code()
        )))
    }
}

async fn bounded(response: &mut Response, max: usize) -> Result<Vec<u8>> {
    let mut contents = Vec::new();
    let mut stream = response.stream()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > max - contents.len() {
            return Err(bad("repository response exceeds the inline limit"));
        }
        contents.extend_from_slice(&chunk);
    }
    Ok(contents)
}

pub async fn github_resolve(repo: &str, rev: &str, token: Option<&str>) -> Result<String> {
    let rev = revision(rev).map_err(Error::RustError)?;
    let path = if rev == "HEAD" {
        "commits?per_page=1".into()
    } else if git::is_id(rev) {
        format!("commits/{rev}")
    } else {
        let branch = rev.strip_prefix("refs/heads/").unwrap_or(rev);
        format!("git/ref/heads/{}", uri_encode(branch, false))
    };
    let mut response = get(
        &github_url(repo, &path)?,
        token,
        "application/vnd.github+json",
        RequestRedirect::Manual,
    )
    .await?;
    check(&response)?;
    let value: Value = serde_json::from_slice(&bounded(&mut response, INLINE_MAX).await?)?;
    let sha = if rev == "HEAD" {
        &value[0]["sha"]
    } else if git::is_id(rev) {
        &value["sha"]
    } else if value["object"]["type"] == "commit" {
        &value["object"]["sha"]
    } else {
        return Err(bad("GitHub did not return a branch commit"));
    };
    sha.as_str()
        .filter(|sha| git::is_id(sha))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| bad("GitHub did not return a commit SHA"))
}

pub async fn github_read_file(
    repo: &str,
    commit: &str,
    path: &str,
    token: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    let path = file_path(path).map_err(Error::RustError)?;
    if !git::is_id(commit) {
        return Err(bad("expected a resolved Git commit SHA"));
    }
    let endpoint = format!("contents/{}?ref={commit}", uri_encode(path, false));
    let mut response = get(
        &github_url(repo, &endpoint)?,
        token,
        "application/vnd.github+json",
        RequestRedirect::Manual,
    )
    .await?;
    if response.status_code() == 404 {
        return Ok(None);
    }
    check(&response)?;
    let value: Value = serde_json::from_slice(&bounded(&mut response, 2 * INLINE_MAX).await?)?;
    if value["type"] != "file" {
        return Ok(None);
    }
    if value["size"]
        .as_u64()
        .is_none_or(|size| size > INLINE_MAX as u64)
        || value["encoding"] != "base64"
    {
        return Err(bad(
            "repository file exceeds the inline limit or is unavailable",
        ));
    }
    let content = value["content"]
        .as_str()
        .ok_or_else(|| bad("GitHub did not return file content"))?;
    let base64url: String = content
        .bytes()
        .filter_map(|b| match b {
            b'+' => Some('-'),
            b'/' => Some('_'),
            b'=' => None,
            b if b.is_ascii_whitespace() => None,
            b => Some(char::from(b)),
        })
        .collect();
    let bytes = crate::webauthn::unb64url(&base64url)?;
    if bytes.len() > INLINE_MAX {
        return Err(bad("repository file exceeds the inline limit"));
    }
    Ok(Some(bytes))
}

/// Existing site publishing archive flow. Public repositories use codeload.
pub async fn github_archive(repo: &str, rev: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let (owner, name) = github_repo(repo)?;
    // Preserve the archive's accepted ref syntax while preventing URL path normalization.
    file_path(rev).map_err(Error::RustError)?;
    let rev = uri_encode(rev, false);
    let url = if token.is_some() {
        github_url(repo, &format!("tarball/{rev}"))?
    } else {
        format!("https://codeload.github.com/{owner}/{name}/tar.gz/{rev}")
    };
    let mut response = get(
        &url,
        token,
        "application/vnd.github+json",
        RequestRedirect::Follow,
    )
    .await?;
    if response.status_code() != 200 {
        return Err(Error::RustError(format!(
            "GitHub answered {} for {repo} at {rev}",
            response.status_code()
        )));
    }
    response.bytes().await
}

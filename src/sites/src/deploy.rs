//! `site_deploy`: an instance's site, deployed from its GitHub repository at a commit. What it
//! must not hold itself comes from core, as the caller it was called for: the instance's
//! record (core's `instances_list`), an installation token for the repository (`github`) and the
//! linked account's token (`linked`). The repository is read as one tarball
//! ([`pigeon_core::tarball`]), sorted ([`pigeon_blog::repo`]), rendered ([`pigeon_blog`]), and
//! written to the record's bucket in the layout the site Worker serves: the blobs it lacks, then
//! `deploys/{commit}.json`, then `live`. It only adds objects and moves `live`, so the previous
//! deploy stays whole until the last write.

use std::collections::BTreeSet;

use pigeon_contract::{Answer, Signed};
use pigeon_core::r2::{self, Bucket, Manifest};
use pigeon_core::{Account, tarball};
use serde_json::{Value, json};
use worker::{Env, Error, Fetch, Headers, Method, Request, RequestInit, Result};

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Asks core for `op` as the caller this call came with.
async fn core(env: &Env, key: &[u8], caller: &Signed, op: &str, body: &Value) -> Result<Value> {
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service("PLATFORM")?;
    match pigeon_contract::ask(
        &platform,
        "https://core",
        op,
        body,
        Some((key, &caller.caller)),
        &nonce,
    )
    .await?
    {
        Answer::Ok(v) => Ok(v),
        Answer::Error(f) => Err(bad(format!("core's {op}: {}", f.message))),
    }
}

/// What a deploy needs from an instance's record.
pub struct Target {
    pub repo: String,
    pub bucket: Bucket,
    pub base_url: String,
}

/// Reads [`Target`] from a record as core's `instances_list` answers it. Pure.
pub fn target(record: &Value) -> std::result::Result<Target, String> {
    let id = record["id"].as_str().unwrap_or("?");
    if record["repo"]["kind"] != "github" {
        return Err(format!("instance {id}'s repository is not on GitHub"));
    }
    let repo = record["repo"]["full_name"]
        .as_str()
        .ok_or_else(|| format!("instance {id} names no repository"))?
        .to_string();
    let site = &record["sites"][0];
    let bucket = site["bucket"]
        .as_str()
        .ok_or_else(|| format!("instance {id} has no site"))?;
    let host = site["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["kind"] == "domain" || t["kind"] == "zone")
        .and_then(|t| t["hostname"].as_str())
        .ok_or_else(|| format!("instance {id}'s site has no domain to be served at"))?;
    Ok(Target {
        repo,
        bucket: Bucket {
            name: bucket.to_string(),
            jurisdiction: site["jurisdiction"].as_str().map(str::to_string),
        },
        base_url: format!("https://{host}"),
    })
}

/// The repository's tarball at `rev`: through the API with the App's token when there is one,
/// else from codeload, which serves public repositories.
async fn tarball_of(repo: &str, rev: &str, token: Option<&str>) -> Result<Vec<u8>> {
    let headers = Headers::new();
    headers.set("User-Agent", "pigeon")?;
    let url = match token {
        Some(t) => {
            headers.set("Authorization", &format!("Bearer {t}"))?;
            headers.set("Accept", "application/vnd.github+json")?;
            format!("https://api.github.com/repos/{repo}/tarball/{rev}")
        }
        None => format!("https://codeload.github.com/{repo}/tar.gz/{rev}"),
    };
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let mut res = Fetch::Request(Request::new_with_init(&url, &init)?)
        .send()
        .await?;
    if res.status_code() != 200 {
        return Err(bad(format!(
            "GitHub answered {} for {repo} at {rev}",
            res.status_code()
        )));
    }
    res.bytes().await
}

/// Deploys `instance` at `rev`.
pub async fn deploy(
    env: &Env,
    key: &[u8],
    caller: &Signed,
    instance: &str,
    rev: &str,
) -> Result<Value> {
    let records = core(
        env,
        key,
        caller,
        "dispatch",
        &json!({ "tool": "instances_list" }),
    )
    .await?;
    let record = records
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["id"] == instance)
        .ok_or_else(|| bad(format!("there is no instance {instance}")))?;
    let t = target(record).map_err(bad)?;

    let ask = |kind: &str| json!({ "kind": kind, "instance": instance });
    let gh = core(env, key, caller, "credential", &ask("github"))
        .await
        .ok();
    let gh_token = gh.as_ref().and_then(|c| c["token"].as_str());
    let gz = tarball_of(&t.repo, rev, gh_token).await?;
    let archive = tarball::read(&gz).map_err(bad)?;
    drop(gz);
    let commit = archive
        .commit
        .clone()
        .ok_or_else(|| bad("GitHub's archive does not name its commit"))?;
    let year = tarball::year_of(archive.mtime);
    let s = pigeon_blog::repo::split(archive.files).map_err(bad)?;
    let input = pigeon_blog::Input {
        site_toml: s
            .site_toml
            .ok_or_else(|| bad("the repository has no .pigeon/site.toml"))?,
        posts: s.posts,
        pages: s.pages,
        statics: s.statics.iter().map(|(p, _)| p.clone()).collect(),
        base_url: t.base_url.clone(),
        commit: commit.chars().take(7).collect(),
        commit_year: year,
    };
    let out = match pigeon_blog::render(&input) {
        Ok(o) => o,
        Err(problems) => {
            return Ok(json!({
                "commit": commit, "deployed": false,
                "problems": problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
            }));
        }
    };

    let linked = core(env, key, caller, "credential", &ask("linked")).await?;
    let account = Account {
        id: linked["account_id"]
            .as_str()
            .ok_or_else(|| bad("core's linked credential has no account"))?
            .to_string(),
        parent: String::new(),
        secrets_store: String::new(),
        bearer: Some(
            linked["token"]
                .as_str()
                .ok_or_else(|| bad("core's linked credential has no token"))?
                .to_string(),
        ),
    };
    let present: BTreeSet<String> = r2::list(&account, &t.bucket, "blobs/")
        .await?
        .iter()
        .filter_map(|k| r2::blob_of_key(k).map(str::to_string))
        .collect();

    let mut manifest = Manifest::new();
    let mut fresh: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut add = |path: String, bytes: Vec<u8>, ctype: String| {
        let pair = [(path, bytes)];
        let (p, mut e) = r2::manifest_of(&pair)
            .into_iter()
            .next()
            .expect("one file gives one entry");
        let [(_, bytes)] = pair;
        e.content_type = ctype.clone();
        let new = !present.contains(&e.blob) && !manifest.values().any(|x| x.blob == e.blob);
        if new {
            fresh.push((e.blob.clone(), ctype, bytes));
        }
        manifest.insert(p, e);
    };
    for (path, (bytes, ctype)) in out.files {
        add(path, bytes, ctype.to_string());
    }
    for (path, bytes) in s.statics {
        let ctype = r2::content_type(&path).to_string();
        add(path, bytes, ctype);
    }
    let uploaded = fresh.len();
    for (blob, ctype, bytes) in fresh {
        r2::put(&account, &t.bucket, &r2::blob_key(&blob), &ctype, bytes).await?;
    }
    let files = manifest.len();
    r2::put(
        &account,
        &t.bucket,
        &r2::manifest_key(&commit),
        "application/json",
        serde_json::to_vec(&manifest).map_err(|e| bad(e.to_string()))?,
    )
    .await?;
    r2::put(
        &account,
        &t.bucket,
        r2::LIVE,
        "text/plain",
        commit.clone().into_bytes(),
    )
    .await?;
    Ok(json!({
        "instance": instance,
        "commit": commit,
        "deployed": true,
        "files": files,
        "uploaded": uploaded,
        "read_with": if gh_token.is_some() { "the GitHub App" } else { "codeload (public)" },
        "template": out.template,
        "template_version": out.version,
        "template_digest": out.digest,
    }))
}

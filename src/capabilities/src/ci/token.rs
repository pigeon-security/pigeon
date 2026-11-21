//! Job tokens. A job token lets one CI runner fetch one commit of one repository and report back
//! for one job, until the job's deadline, and nothing else. It is shown only to that runner and
//! kept only as its hash, beside its grant, in the scheduler; it is dropped as the job ends.

use pigeon_core::oauth::{hash, token};
use serde::{Deserialize, Serialize};

/// What every job token starts with.
pub const PREFIX: &str = "pgn_ci_";

/// How many base64url characters follow the prefix: 32 random bytes.
const SECRET_LEN: usize = 43;

/// The most characters a job id in a runner path may have.
const MAX_JOB_LEN: usize = 64;

/// What a job token allows, kept beside its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The job it reports for.
    pub job: String,
    /// The repository it may fetch, as `namespace/name`.
    pub repo: String,
    /// The one commit it may fetch.
    pub commit: String,
    /// When it dies, in milliseconds since the epoch: the job's deadline.
    pub expires_at: i64,
}

/// A new job token and the hash it is kept as.
pub fn mint() -> worker::Result<(String, String)> {
    token(PREFIX)
}

/// The hash a presented token is looked up by, when it has a job token's shape; `None`
/// otherwise, so junk is never looked up.
pub fn lookup_hash(presented: &str) -> Option<String> {
    let secret = presented.strip_prefix(PREFIX)?;
    if secret.len() != SECRET_LEN || !secret.bytes().all(is_base64url) {
        return None;
    }
    Some(hash(presented))
}

/// Whether `grant` is live at `now_ms` for reporting on `job`.
pub fn allows_job(grant: &Grant, now_ms: i64, job: &str) -> bool {
    now_ms < grant.expires_at && grant.job == job
}

/// Whether `grant` is live at `now_ms` for fetching `commit` of `repo`.
pub fn allows_fetch(grant: &Grant, now_ms: i64, repo: &str, commit: &str) -> bool {
    now_ms < grant.expires_at
        && grant.repo == repo
        && is_commit(commit)
        && grant.commit.eq_ignore_ascii_case(commit)
}

/// Whether a job token may be presented on `path` (its query string removed) at all: a
/// runner's report on `/ci/runner/{job}`, or a fetch from the git proxy. Never a push, never
/// `/cf/` or `/mcp`.
pub fn reaches(path: &str) -> bool {
    if path.contains('%') || path.contains('\\') {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        ["", "ci", "runner", job] => is_job(job),
        ["", "git", ns, repo, "info", "refs"] | ["", "git", ns, repo, "git-upload-pack"] => {
            is_name(ns) && is_repo(repo)
        }
        _ => false,
    }
}

fn is_base64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_job(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_JOB_LEN && s.bytes().all(is_base64url)
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn is_repo(s: &str) -> bool {
    match s.strip_suffix(".git") {
        Some(name) => !name.is_empty() && is_name(s),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn grant() -> Grant {
        Grant {
            job: "j1".into(),
            repo: "pigeon-security/pigeon".into(),
            commit: COMMIT.into(),
            expires_at: 1_000,
        }
    }

    #[test]
    fn only_a_job_token_shape_is_looked_up() {
        let good = format!("{PREFIX}{}", "A".repeat(SECRET_LEN));
        assert_eq!(lookup_hash(&good), Some(hash(&good)));
        for bad in [
            format!("pgn_px_{}", "A".repeat(SECRET_LEN)),
            format!("{PREFIX}{}", "A".repeat(SECRET_LEN - 1)),
            format!("{PREFIX}{}", "A".repeat(SECRET_LEN + 1)),
            format!("{PREFIX}{}+", "A".repeat(SECRET_LEN - 1)),
            String::new(),
        ] {
            assert_eq!(lookup_hash(&bad), None, "{bad}");
        }
    }

    #[test]
    fn a_grant_reports_for_its_job_until_it_expires() {
        let g = grant();
        assert!(allows_job(&g, 999, "j1"));
        assert!(!allows_job(&g, 1_000, "j1"));
        assert!(!allows_job(&g, 0, "j2"));
    }

    #[test]
    fn a_grant_fetches_its_one_commit_of_its_one_repo() {
        let g = grant();
        assert!(allows_fetch(&g, 0, "pigeon-security/pigeon", COMMIT));
        assert!(allows_fetch(
            &g,
            0,
            "pigeon-security/pigeon",
            &COMMIT.to_uppercase()
        ));
        assert!(!allows_fetch(&g, 1_000, "pigeon-security/pigeon", COMMIT));
        assert!(!allows_fetch(&g, 0, "pigeon-security/other", COMMIT));
        assert!(!allows_fetch(
            &g,
            0,
            "pigeon-security/pigeon",
            "1123456789abcdef0123456789abcdef01234567"
        ));
        assert!(!allows_fetch(&g, 0, "pigeon-security/pigeon", "main"));
    }

    #[test]
    fn a_job_token_reaches_only_its_report_and_fetches() {
        for ok in [
            "/ci/runner/abc",
            "/git/ns/r.git/info/refs",
            "/git/ns/r.git/git-upload-pack",
        ] {
            assert!(reaches(ok), "{ok}");
        }
        for no in [
            "/ci/runner/",
            "/ci/runner/a/b",
            "/ci/hooks/x",
            "/ci/token",
            "/git/ns/r.git/git-receive-pack",
            "/git/ns/../r.git/info/refs",
            "/git/ns/r.git/info/refs%2F",
            "/git/ns/r.git/info/refs/",
            "/git//r.git/info/refs",
            "/git/ns/.git/info/refs",
            "/git/ns/r/info/refs",
            "/cf/x",
            "/mcp",
            "/tokens",
        ] {
            assert!(!reaches(no), "{no}");
        }
        assert!(!reaches(&format!("/ci/runner/{}", "a".repeat(65))));
    }
}

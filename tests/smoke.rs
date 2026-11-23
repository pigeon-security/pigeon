//! pigeon's smoke suite, its only tests: one end-to-end scenario per path, each walking that path
//! through public interfaces with its externals stubbed (git is a local repository; the Workers
//! runtime, Cloudflare, the providers and GitHub answer with recorded or made-up values). Each
//! scenario folds in the security cases of its path: forged signatures and tokens, malformed
//! packs, refused pushes. The scenarios live in the area modules under `tests/smoke/`, one file
//! per area; a new scenario goes in its area's file. The shared stubs are here. The policy
//! (`.pigeon/policy.toml`, rule `smoke-suite-fixed`) keeps every test in the suite;
//! `ci/runner/test.sh` fails on a `#[test]` anywhere else and on a suite slower than 60 seconds.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

// ---- shared stubs ----

/// A fresh local repository standing in for the source repository in Artifacts.
fn repo(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pigeon-smoke-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "smoke@example.com"],
        &["config", "user.name", "smoke"],
        &["config", "commit.gpgsign", "false"],
    ] {
        git(&dir, args, None);
    }
    dir
}

/// Runs git in `dir` with `input` on stdin; its stdout.
fn git(dir: &Path, args: &[&str], input: Option<&str>) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        if let Some(i) = input {
            stdin.write_all(i.as_bytes()).unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    out.stdout
}

/// Commits `body` as file `f`; the commit's id.
fn commit(dir: &Path, body: &str) -> String {
    std::fs::write(dir.join("f"), body).unwrap();
    git(dir, &["add", "f"], None);
    git(dir, &["commit", "-q", "-m", body], None);
    String::from_utf8(git(dir, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_string()
}

/// What Artifacts answers a fetch wanting `new` and having `old`: a pack of what `old` lacks.
fn pack(dir: &Path, new: &str, old: &str) -> Vec<u8> {
    git(
        dir,
        &["pack-objects", "--stdout", "--revs", "-q"],
        Some(&format!("{new}\n^{old}\n")),
    )
}

/// The policy as the platform is configured: sessions push under `claude/`, main is protected.
fn vars() -> pigeon_core::policy::Vars {
    pigeon_capabilities::git_proxy::vars("refs/heads/main", "refs/heads/claude/")
}

fn request(action: &str, target: &str, approval: Option<&str>) -> pigeon_core::policy::Request {
    serde_json::from_value(json!({
        "actor": "smoke", "action": action, "target": target,
        "context": { "approval": approval },
    }))
    .unwrap()
}

/// The id of a blob `git hash-object` stores in `dir`.
fn blob_id(dir: &Path) -> String {
    String::from_utf8(git(dir, &["hash-object", "-w", "--stdin"], Some("x\n")))
        .unwrap()
        .trim()
        .to_string()
}

/// Accounts: secrets, expiry, storage, and linked Cloudflare and GitHub accounts.
#[path = "smoke/accounts.rs"]
mod accounts;
/// The AI agents Worker: loop, budgets, cost and the sandbox.
#[path = "smoke/ai_agents.rs"]
mod ai_agents;
/// The Apple platform: App Store Connect, signing, releases, APNs and design tokens.
#[path = "smoke/apple.rs"]
mod apple;
/// CI routing, connectors and the Buildkite pipelines and setup.
#[path = "smoke/ci.rs"]
mod ci;
/// The console, mail, changelog and architecture views of the core Worker.
#[path = "smoke/core.rs"]
mod core;
/// Instances and versions: pins, plans and upgrades.
#[path = "smoke/instances.rs"]
mod instances;
/// Landing: checks, deploy lanes, rebases, attribution and the lane map.
#[path = "smoke/land.rs"]
mod land;
/// The model gateway: requests, turns, cache tokens and cost.
#[path = "smoke/models.rs"]
mod models;
/// Observability settings and monitors on every Worker.
#[path = "smoke/observability.rs"]
mod observability;
/// Policy decisions, the git proxy's push rules and tool-call approvals.
#[path = "smoke/policy.rs"]
mod policy;
/// The proxy Worker: contracts, dispatch, deploy allowlist, mirror, webhooks, OIDC and tokens.
#[path = "smoke/proxy.rs"]
mod proxy;
/// Sites and blogs: the apex, rendering, auth and deploys.
#[path = "smoke/sites.rs"]
mod sites;

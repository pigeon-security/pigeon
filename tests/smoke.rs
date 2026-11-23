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

/// A number in `v`, or NaN when it is none.
fn num_of(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

#[test]
fn models_billing_parsers_and_answer() {
    use pigeon_models::billing::{add_cost, add_usage, answer, cost_url, usage_url};
    use std::collections::BTreeMap;
    let url = usage_url(
        "2026-11-22T00:00:00Z",
        "2026-11-24T00:00:00Z",
        31,
        Some("a b"),
    );
    assert!(url.starts_with("https://api.anthropic.com/v1/organizations/usage_report/messages?"));
    assert!(url.contains("bucket_width=1d&group_by%5B%5D=model&limit=31"));
    assert!(url.contains("starting_at=2026-11-22T00%3A00%3A00Z"));
    assert!(url.ends_with("&page=a%20b"));
    let url = cost_url("2026-11-22T00:00:00Z", "2026-11-24T00:00:00Z", None);
    assert!(url.contains("/cost_report?"));
    assert!(url.contains("group_by%5B%5D=description"));
    assert!(!url.contains("page="));

    let mut days = BTreeMap::new();
    // Numbers as strings, missing fields, and unknown fields are all tolerated.
    let more = add_usage(
        &mut days,
        &json!({
            "data": [{ "starting_at": "2026-11-22T00:00:00Z", "ending_at": "2026-11-23T00:00:00Z",
                "results": [
                    { "model": "claude-opus-5-5", "uncached_input_tokens": "100", "output_tokens": 50,
                      "cache_read_input_tokens": 10,
                      "cache_creation": { "ephemeral_5m_input_tokens": 5, "ephemeral_1h_input_tokens": 5 },
                      "extra": true },
                    { "model": "anthropic/claude-opus-5.5", "output_tokens": 1 },
                ] }],
            "has_more": true, "next_page": "p2",
        }),
    );
    assert!(more.has_more);
    assert_eq!(more.next_page.as_deref(), Some("p2"));
    let more = add_cost(
        &mut days,
        &json!({
            "data": [{ "starting_at": "2026-11-22T00:00:00Z", "results": [
                { "currency": "USD", "amount": "123.45", "description": "x", "model": "claude-opus-5-5" },
                { "currency": "USD", "amount": "50", "description": "web search" },
            ] }],
            "has_more": false,
        }),
    );
    assert!(!more.has_more && more.next_page.is_none());
    let list = vec!["2026-11-22".to_string(), "2026-11-23".to_string()];
    let a = answer(&list, &days, &[json!({ "source": "x", "why": "y" })]);
    assert_eq!(a["provider"], "anthropic");
    assert_eq!(a["by_day"].as_object().map(|m| m.len()), Some(1));
    let d = &a["by_day"]["2026-11-22"];
    assert!((num_of(&d["cost_usd"]) - 1.7345).abs() < 1e-9);
    assert!((num_of(&a["total_usd"]) - 1.7345).abs() < 1e-9);
    assert_eq!(d["tokens"]["uncached_input"], 100);
    assert_eq!(d["tokens"]["output"], 51);
    assert_eq!(d["tokens"]["cache_write"], 10);
    assert!((num_of(&d["by_model"]["claude-opus-5-5"]["cost_usd"]) - 1.2345).abs() < 1e-9);
    assert!((num_of(&d["by_model"]["other"]["cost_usd"]) - 0.5).abs() < 1e-9);
    assert_eq!(a["unavailable"][0]["source"], "x");
}

/// A gateway Anthropic figure for `reconcile`: one model, the given days.
fn recon_gateway(rows: &[(&str, f64, u64, u64)]) -> Value {
    let mut by_day = serde_json::Map::new();
    for (day, usd, tin, tout) in rows {
        by_day.insert(
            day.to_string(),
            json!({ "cost_usd": usd, "tokens_in": tin, "tokens_out": tout, "requests": 3 }),
        );
    }
    json!({ "claude-opus-5-5": by_day })
}

/// Anthropic's billed answer for `reconcile`.
fn recon_anthropic(rows: &[(&str, f64, u64, u64)]) -> Value {
    let mut by_day = serde_json::Map::new();
    for (day, usd, tin, tout) in rows {
        by_day.insert(
            day.to_string(),
            json!({ "cost_usd": usd, "tokens": { "uncached_input": tin, "output": tout } }),
        );
    }
    json!({ "by_day": by_day })
}

#[test]
fn models_billing_reconcile_statuses() {
    use pigeon_models::billing::reconcile;
    let days: Vec<String> = ["19", "20", "21", "22", "23", "24"]
        .iter()
        .map(|d| format!("2026-11-{d}"))
        .collect();
    let gateway = recon_gateway(&[
        ("2026-11-19", 1.0, 100, 50),
        ("2026-11-20", 3.70, 1000, 500),
        ("2026-11-21", 3.70, 1000, 500),
        ("2026-11-22", 1.0, 100, 50),
        ("2026-11-23", 0.5, 10, 10),
    ]);
    let anthropic = recon_anthropic(&[
        ("2026-11-20", 3.72, 1000, 500),
        ("2026-11-21", 4.10, 1010, 500),
        ("2026-11-22", 4.0, 1000, 500),
    ]);
    let r = reconcile(&gateway, false, &anthropic, &days, "2026-11-24");
    let status = |r: &Value, day: &str| {
        r["days"]
            .as_array()
            .and_then(|a| a.iter().find(|d| d["day"] == day))
            .map(|d| d["status"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(status(&r, "2026-11-19"), "gateway_only");
    assert_eq!(status(&r, "2026-11-20"), "match");
    assert_eq!(status(&r, "2026-11-21"), "pricing");
    assert_eq!(status(&r, "2026-11-22"), "outside_gateway");
    assert_eq!(status(&r, "2026-11-23"), "anthropic_pending");
    // Today has no data on either side, yet Anthropic may still report it: pending.
    assert_eq!(status(&r, "2026-11-24"), "anthropic_pending");
    let day = &r["days"][2];
    assert!((num_of(&day["difference_usd"]) - 0.4).abs() < 1e-9);
    assert!((num_of(&day["tokens_ratio"]) - 1.510 / 1.5).abs() < 1e-6);
    assert!(day["anthropic_usd"].is_number());
    let pending = &r["days"][4];
    assert!(pending["anthropic_usd"].is_null() && pending["tokens_ratio"].is_null());
    let notes = r["notes"].as_array().cloned().unwrap_or_default();
    assert_eq!(notes.len(), 3);
    let want =
        "2026-11-21: Anthropic billed $4.10, the gateway recorded $3.70 (+$0.40; tokens agree";
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().is_some_and(|n| n.starts_with(want)))
    );
    assert!((num_of(&r["anthropic_usd"]) - 11.82).abs() < 1e-9);
    // The same listing cut short: more Anthropic tokens are the listing's fault.
    let cut = reconcile(&gateway, true, &anthropic, &days, "2026-11-24");
    assert_eq!(status(&cut, "2026-11-22"), "gateway_truncated");
    assert_eq!(status(&cut, "2026-11-21"), "pricing");
}

#[test]
fn models_daily_by_provider() {
    use pigeon_models::daily::{daily_by_run, family_of};
    let log = |at: &str, provider: &str, model: &str, cost: f64| -> pigeon_core::ai_gateway::Log {
        serde_json::from_value(json!({
            "created_at": at, "provider": provider, "model": model, "cost": cost,
            "tokens_in": 10, "tokens_out": 5,
        }))
        .unwrap_or_default()
    };
    let logs = vec![
        log(
            "2026-03-10T01:00:00Z",
            "anthropic",
            "anthropic/claude-opus-5.5",
            1.0,
        ),
        log("2026-03-10T02:00:00Z", "anthropic", "claude-opus-5-5", 0.5),
        log("2026-03-10T03:00:00Z", "openai", "gpt-5", 0.25),
        log("2026-03-09T03:00:00Z", "", "mystery", 0.125),
    ];
    let folded = daily_by_run(&logs, "2026-03-10", 7);
    assert_eq!(folded.by_provider["anthropic"]["2026-03-10"], 1.5);
    assert_eq!(folded.by_provider["openai"]["2026-03-10"], 0.25);
    assert_eq!(folded.by_provider["other"]["2026-03-09"], 0.125);
    let m = &folded.by_provider_model["anthropic"]["claude-opus-5-5"]["2026-03-10"];
    assert_eq!((m.requests, m.tokens_in, m.tokens_out), (2, 20, 10));
    assert_eq!(m.cost_usd, 1.5);
    // The existing fields hold, and OpenAI is a family of its own.
    assert_eq!(folded.by_family["Opus"]["2026-03-10"], 1.5);
    assert_eq!(folded.by_family["OpenAI"]["2026-03-10"], 0.25);
    assert_eq!(family_of("openai", "claude-opus-5-5"), "OpenAI");
    assert_eq!(family_of("anthropic", "claude-opus-5-5"), "Opus");
}

/// A `models_daily` answer with Anthropic and OpenAI cost, for the `cost_daily` scenarios: the
/// gateway recorded Anthropic $3 on 03-08 and $2 on 03-10, and OpenAI $1 on 03-10.
fn cost_models() -> Value {
    json!({
        "days": ["2026-03-08", "2026-03-09", "2026-03-10"],
        "by_family": { "Sonnet": { "2026-03-08": 3.0, "2026-03-10": 2.0 }, "OpenAI": { "2026-03-10": 1.0 } },
        "by_run": {}, "unattributed": { "2026-03-08": 3.0, "2026-03-10": 3.0 },
        "by_provider": { "anthropic": { "2026-03-08": 3.0, "2026-03-10": 2.0 }, "openai": { "2026-03-10": 1.0 } },
        "by_provider_model": { "anthropic": { "claude-sonnet-5": {
            "2026-03-08": { "cost_usd": 3.0, "tokens_in": 1000, "tokens_out": 500, "requests": 2 },
            "2026-03-10": { "cost_usd": 2.0, "tokens_in": 800, "tokens_out": 400, "requests": 1 } } } },
    })
}

#[test]
fn platform_cost_daily_anthropic_total_without_double_counting() {
    use pigeon_capabilities::cost::{assemble, live_days};
    use std::collections::BTreeMap;
    let today = "2026-03-10";
    let roles = BTreeMap::new();
    let anthropic = json!({ "by_day": { "2026-03-08": {
        "cost_usd": 3.5,
        "tokens": { "uncached_input": 1000, "output": 500, "cache_read": 0, "cache_write": 0 },
        "by_model": { "claude-sonnet-5": { "cost_usd": 3.5 } } } } });
    let live = live_days(None, Some(&cost_models()), Some(&roles), Some(&anthropic));
    let d = &live["2026-03-08"];
    assert_eq!(d["gateway_providers"]["anthropic"], 3.0);
    assert_eq!(
        d["gateway_anthropic"]["claude-sonnet-5"]["tokens_out"],
        500.0
    );
    assert_eq!(d["anthropic"]["cost_usd"], 3.5);
    assert!(live["2026-03-10"]["anthropic"].is_null());
    let a = assemble(
        "7d",
        today,
        &BTreeMap::new(),
        &live,
        false,
        false,
        vec![],
        vec![],
    );
    // The gateway's whole cost is $6; Anthropic's $3.5 replaces the gateway's $3 on 03-08 only.
    assert_eq!(num_of(&a["ai_gateway"]["total"]), 6.0);
    assert_eq!(num_of(&a["total"]), 6.5);
    let p = &a["ai_gateway"]["by_provider"];
    assert_eq!(p[0]["name"], "Anthropic");
    assert_eq!(num_of(&p[0]["total"]), 5.0);
    assert_eq!(p[1]["name"], "OpenAI");
    assert_eq!(num_of(&p[1]["total"]), 1.0);
    assert_eq!(p.as_array().map(Vec::len), Some(2));
    assert_eq!(num_of(&a["anthropic"]["total"]), 3.5);
    assert_eq!(a["anthropic"]["by_model"][0]["name"], "claude-sonnet-5");
    assert_eq!(a["anthropic"]["source"], "Anthropic Admin API");
    let rows = &a["anthropic"]["reconciliation"]["days"];
    let status = |day: &str| {
        rows.as_array()
            .and_then(|r| r.iter().find(|d| d["day"] == day))
            .map(|d| d["status"].clone())
    };
    // 3.5 against 3.0 with the same tokens is a pricing difference, and the day is noted.
    assert_eq!(status("2026-03-08"), Some(json!("pricing")));
    assert!(a["notes"].as_array().is_some_and(|n| n.iter().any(|x| {
        x.as_str()
            .is_some_and(|x| x.starts_with("2026-03-08: Anthropic billed $3.50"))
    })));
}

#[test]
fn platform_cost_daily_anthropic_pending_day() {
    use pigeon_capabilities::cost::{assemble, live_days};
    use std::collections::BTreeMap;
    let roles = BTreeMap::new();
    // Anthropic answered, but has nothing for today yet: the gateway's cost stands for today.
    let anthropic = json!({ "by_day": { "2026-03-08": {
        "cost_usd": 3.0, "tokens": { "uncached_input": 1000, "output": 500 },
        "by_model": { "claude-sonnet-5": { "cost_usd": 3.0 } } } } });
    let live = live_days(None, Some(&cost_models()), Some(&roles), Some(&anthropic));
    let a = assemble(
        "today",
        "2026-03-10",
        &BTreeMap::new(),
        &live,
        false,
        false,
        vec![],
        vec![],
    );
    assert_eq!(num_of(&a["total"]), 3.0);
    assert_eq!(
        a["anthropic"]["reconciliation"]["days"][0]["status"],
        "anthropic_pending"
    );
    assert_eq!(num_of(&a["anthropic"]["total"]), 0.0);
}

#[test]
fn platform_cost_daily_anthropic_unavailable() {
    use pigeon_capabilities::cost::{assemble, live_days};
    use std::collections::BTreeMap;
    let roles = BTreeMap::new();
    let live = live_days(None, Some(&cost_models()), Some(&roles), None);
    let a = assemble(
        "7d",
        "2026-03-10",
        &BTreeMap::new(),
        &live,
        false,
        true,
        vec![],
        vec![json!({ "source": "Anthropic", "why": "down" })],
    );
    assert!(a["anthropic"].is_null());
    assert_eq!(num_of(&a["total"]), 6.0);
    assert_eq!(a["unavailable"][0]["source"], "Anthropic");
}

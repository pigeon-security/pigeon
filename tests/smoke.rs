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
/// The cloud gateway: Scaleway keys, buckets and objects, plans and approvals, with a mocked
/// Scaleway.
#[path = "smoke/cloud.rs"]
mod cloud;
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
    assert_eq!(p[1]["name"], "Anthropic (credits)");
    assert_eq!(p[3]["name"], "Other");
    assert_eq!(num_of(&p[3]["total"]), 1.0);
    assert_eq!(p.as_array().map(Vec::len), Some(4));
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
}

#[test]
fn platform_cost_daily_sources_status() {
    use pigeon_capabilities::cost::{assemble, billing_days, live_days, with_status};
    use std::collections::BTreeMap;
    let roles = BTreeMap::new();
    let answer = |live: &_| {
        assemble(
            "7d",
            "2026-03-10",
            &BTreeMap::new(),
            live,
            false,
            false,
            vec![],
            vec![],
        )
    };

    // Rows with only billing-period columns and bare-date ends used to fold to nothing or spread
    // one day too far; a single row object and a `rows` wrapper are read too.
    let rows = json!({ "rows": [
        { "ServiceName": "Workers Paid", "BilledCost": "3.0", "BillingPeriodStart": "2026-03-10", "BillingPeriodEnd": "2026-03-11" },
    ] });
    let billing = billing_days(&rows, "2026-03-10");
    assert_eq!(billing.days.len(), 1);
    assert_eq!(
        num_of(&billing.days["2026-03-10"]["cloudflare"]["Workers"]),
        3.0
    );
    let one = json!({ "ServiceName": "R2", "BilledCost": 1.0, "ChargePeriodStart": "2026-03-09T00:00:00Z" });
    assert_eq!(billing_days(&one, "2026-03-10").days.len(), 1);

    // Both sources have data: ok.
    let live = live_days(Some(&billing), Some(&cost_models()), Some(&roles), None);
    let mut a = answer(&live);
    with_status(&mut a, None, None);
    assert_eq!(a["cloudflare"]["status"], "ok");
    assert_eq!(a["ai_gateway"]["status"], "ok");
    assert!(a["cloudflare"]["why"].is_null());

    // Neither answered anything: empty, with a reason.
    let mut a = answer(&live_days(None, None, Some(&roles), None));
    with_status(&mut a, None, None);
    assert_eq!(a["cloudflare"]["status"], "empty");
    assert_eq!(a["ai_gateway"]["status"], "empty");
    assert!(a["ai_gateway"]["why"].is_string());

    // A failed source is unavailable, with its reason.
    with_status(&mut a, Some("timed out"), Some("down"));
    assert_eq!(a["cloudflare"]["status"], "unavailable");
    assert_eq!(a["ai_gateway"]["why"], "down");
}

#[test]
fn models_openai_request_translation() {
    use pigeon_models::openai;
    let req = json!({
        "model": "gpt-6-luna",
        "max_tokens": 500,
        "stream": true,
        "system": [
            { "type": "text", "text": "rules", "cache_control": { "type": "ephemeral" } },
            { "type": "text", "text": "more" }
        ],
        "tools": [
            { "name": "run", "description": "Run it", "input_schema": { "type": "object" }, "cache_control": { "type": "ephemeral" } },
            { "type": "web_search_20260101", "name": "web_search" }
        ],
        "tool_choice": { "type": "any" },
        "thinking": { "type": "adaptive" },
        "context_management": { "edits": [] },
        "metadata": { "user_id": "u" },
        "top_k": 5,
        "temperature": 0.5,
        "top_p": 0.9,
        "stop_sequences": ["a", "b", "c", "d", "e"],
        "output_config": { "effort": "max" },
        "messages": [
            { "role": "user", "content": "go" },
            { "role": "assistant", "content": [
                { "type": "thinking", "thinking": "hm", "signature": "s" },
                { "type": "redacted_thinking", "data": "x" },
                { "type": "tool_use", "id": "t1", "name": "run", "input": { "cmd": "ls" } },
                { "type": "tool_use", "id": "t2", "name": "run", "input": { "cmd": "pwd" } }
            ] },
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "a.txt" },
                { "type": "tool_result", "tool_use_id": "t2", "is_error": true,
                  "content": [{ "type": "text", "text": "no" }, { "type": "text", "text": "way" }] },
                { "type": "text", "text": "now?", "cache_control": { "type": "ephemeral" } },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "" } }
            ] },
            { "role": "assistant", "content": [{ "type": "text", "text": "done" }] }
        ]
    });
    let out = openai::request(&req).unwrap();
    assert_eq!(out["model"], "gpt-6-luna");
    assert_eq!(out["max_completion_tokens"], 500);
    assert!(out.get("max_tokens").is_none());
    assert_eq!(out["stream"], true);
    assert_eq!(out["stream_options"], json!({ "include_usage": true }));
    // An effort OpenAI does not take, with tools on a GPT-6 row: chat, effort `none`.
    assert_eq!(out["reasoning_effort"], "none");
    assert_eq!(out["stop"], json!(["a", "b", "c", "d"]));
    assert_eq!(out["temperature"], 0.5);
    assert_eq!(out["top_p"], 0.9);
    for k in [
        "thinking",
        "context_management",
        "metadata",
        "top_k",
        "cache_control",
    ] {
        assert!(out.get(k).is_none(), "{k}");
    }
    assert_eq!(out["tool_choice"], "required");
    assert_eq!(
        out["tools"],
        json!([{ "type": "function", "function": { "name": "run", "description": "Run it", "parameters": { "type": "object" } } }])
    );
    let m = out["messages"].as_array().unwrap();
    assert_eq!(m[0], json!({ "role": "system", "content": "rules\nmore" }));
    assert_eq!(m[1], json!({ "role": "user", "content": "go" }));
    assert_eq!(m[2]["role"], "assistant");
    assert!(m[2]["content"].is_null());
    assert_eq!(m[2]["tool_calls"][0]["id"], "t1");
    assert_eq!(m[2]["tool_calls"][0]["type"], "function");
    assert_eq!(
        m[2]["tool_calls"][1]["function"],
        json!({ "name": "run", "arguments": "{\"cmd\":\"pwd\"}" })
    );
    assert_eq!(
        m[3],
        json!({ "role": "tool", "tool_call_id": "t1", "content": "a.txt" })
    );
    assert_eq!(
        m[4],
        json!({ "role": "tool", "tool_call_id": "t2", "content": "Error: no\nway" })
    );
    assert_eq!(
        m[5],
        json!({ "role": "user", "content": "now?\n[image omitted]" })
    );
    assert_eq!(m[6], json!({ "role": "assistant", "content": "done" }));
    assert_eq!(m.len(), 7);

    // The other tool choices, an unknown effort and a string system prompt.
    let small = |extra: Value| {
        let mut r = json!({
            "model": "gpt-6-luna",
            "system": "s",
            "tools": [{ "name": "f", "input_schema": {} }],
            "messages": [{ "role": "user", "content": "x" }]
        });
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        openai::request(&r).unwrap()
    };
    assert_eq!(
        small(json!({ "tool_choice": { "type": "auto" } }))["tool_choice"],
        "auto"
    );
    assert_eq!(
        small(json!({ "tool_choice": { "type": "tool", "name": "f" } }))["tool_choice"],
        json!({ "type": "function", "function": { "name": "f" } })
    );
    let odd = small(json!({ "output_config": { "effort": "max" } }));
    assert_eq!(odd["reasoning_effort"], "none");
    assert_eq!(odd["messages"][0]["content"], "s");

    // A tool turn that asks for an effort goes to the Responses API on every GPT-6 row, luna
    // too, dated or prefixed names included.
    for m in [
        "gpt-6-luna",
        "gpt-6-luna-2026-10-01",
        "cf-openai/gpt-6.1-terra",
        "openai/gpt-6.2-astra",
    ] {
        let r = small(json!({ "model": m, "output_config": { "effort": "low" } }));
        assert_eq!(r["reasoning"], json!({ "effort": "low" }), "{m}");
        assert!(
            r.get("messages").is_none() && r.get("reasoning_effort").is_none(),
            "{m}"
        );
    }
    // With tools and no effort asked they stay on chat, with effort `none`.
    let luna = small(json!({ "model": "gpt-6-luna" }));
    assert_eq!(luna["reasoning_effort"], "none");
    // Without tools, chat takes the effort as asked.
    let bare = openai::request(&json!({
        "model": "gpt-6-luna", "output_config": { "effort": "low" },
        "messages": [{ "role": "user", "content": "x" }]
    }))
    .unwrap();
    assert_eq!(bare["reasoning_effort"], "low");
}

#[test]
fn models_openai_stream() {
    use pigeon_models::Dialect;
    use pigeon_models::stream::Reading;
    let lines = |chunks: &[Value]| {
        let mut s = String::new();
        for c in chunks {
            s.push_str(&format!("data: {c}\n\n"));
        }
        s
    };
    let sse = lines(&[
        json!({ "model": "gpt-6.2-sol", "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "He" } }] }),
        json!({ "model": "gpt-6.2-sol", "choices": [{ "index": 0, "delta": { "content": "llo" } }] }),
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
            { "index": 0, "id": "c1", "type": "function", "function": { "name": "run", "arguments": "" } },
            { "index": 1, "id": "c2", "type": "function", "function": { "name": "read", "arguments": "" } }
        ] } }] }),
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "{\"cmd\":" } }] } }] }),
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 1, "function": { "arguments": "{\"path\":\"a\"}" } }] } }] }),
        json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [{ "index": 0, "function": { "arguments": "\"ls\"}" } }] } }] }),
        json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }] }),
        json!({ "model": "gpt-6.2-sol", "choices": [], "usage": {
            "prompt_tokens": 1000, "completion_tokens": 40, "total_tokens": 1040,
            "prompt_tokens_details": { "cached_tokens": 600 } } }),
    ]) + "data: [DONE]\n\n";
    let mut r = Reading::default();
    // Split anywhere: the reading does not care.
    for part in sse.as_bytes().chunks(7) {
        r.feed(Dialect::OpenAi, part);
    }
    r.end(Dialect::OpenAi);
    assert!(r.done);
    assert_eq!(r.error, None);
    assert_eq!(r.model, "gpt-6.2-sol");
    assert_eq!(r.text, "Hello");
    assert_eq!(r.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        r.content(),
        json!([
            { "type": "text", "text": "Hello" },
            { "type": "tool_use", "id": "c1", "name": "run", "input": { "cmd": "ls" } },
            { "type": "tool_use", "id": "c2", "name": "read", "input": { "path": "a" } }
        ])
    );
    assert_eq!(r.tokens_in, Some(400));
    assert_eq!(r.cache_read, Some(600));
    assert_eq!(r.tokens_out, Some(40));
    assert_eq!(r.cache_write, Some(0));
    let u = r.usage();
    assert_eq!(u["input_tokens"], 400);
    assert_eq!(u["cache_read_input_tokens"], 600);
    assert_eq!(u["cache_creation_input_tokens"], 0);

    // Finish reasons, and cached tokens above the prompt saturate.
    for (from, to) in [
        ("stop", "end_turn"),
        ("length", "max_tokens"),
        ("content_filter", "refusal"),
        ("function_call", "function_call"),
    ] {
        let mut r = Reading::default();
        let s = lines(&[
            json!({ "choices": [{ "delta": { "content": "x" }, "finish_reason": from }] }),
            json!({ "choices": [], "usage": { "prompt_tokens": 5, "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 9 } } }),
        ]);
        r.feed(Dialect::OpenAi, s.as_bytes());
        r.end(Dialect::OpenAi);
        assert_eq!(r.stop_reason.as_deref(), Some(to));
        assert_eq!(r.tokens_in, Some(0));
        assert!(!r.done);
    }

    // Bad tool arguments become {} and an error note; an error object is the error.
    let mut r = Reading::default();
    let s = lines(&[
        json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": "c", "function": { "name": "f", "arguments": "{oops" } }] } }] }),
        json!({ "choices": [{ "delta": {}, "finish_reason": "tool_calls" }] }),
    ]);
    r.feed(Dialect::OpenAi, s.as_bytes());
    assert_eq!(r.content()[0]["input"], json!({}));
    assert!(r.error.as_deref().unwrap().contains("not JSON"));
    let mut r = Reading::default();
    r.feed(
        Dialect::OpenAi,
        b"data: {\"error\": {\"type\": \"server_error\", \"message\": \"boom\"}}\n\n",
    );
    assert_eq!(r.error.as_deref(), Some("server_error: boom"));
}

#[test]
fn models_openai_dialect_and_model_check() {
    use pigeon_models::{Ask, Dialect, ModelIds, turn};
    assert_eq!(Dialect::OpenAi.path(), "chat/completions");
    assert!(Dialect::OpenAi.headers().is_empty());
    assert_eq!(serde_json::to_value(Dialect::OpenAi).unwrap(), "openai");
    let a = Ask {
        model: "gpt-6.2-sol".into(),
        system: Some("be brief".into()),
        messages: json!([{ "role": "user", "content": "hi" }]),
        max_tokens: 50,
    };
    let b = Dialect::OpenAi.body(&a);
    assert_eq!(b["max_completion_tokens"], 50);
    assert_eq!(
        b["messages"][0],
        json!({ "role": "system", "content": "be brief" })
    );
    assert_eq!(b["messages"][1]["content"], "hi");
    let reply = Dialect::OpenAi
        .reply(&json!({ "model": "gpt-6.2-sol",
            "choices": [{ "message": { "content": "yo" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 2 } }))
        .unwrap();
    assert_eq!(reply.text, "yo");
    assert_eq!(reply.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(reply.tokens_in, Some(7));
    assert_eq!(reply.tokens_out, Some(2));
    assert!(Dialect::OpenAi.reply(&json!({})).is_err());

    // The model check depends on the provider.
    let msgs = json!([{ "role": "user", "content": "x" }]);
    let with = |model: &str, d: Dialect| {
        turn::turn(
            &json!({ "model": model, "messages": msgs }),
            &Value::Null,
            "gpt-6.2-sol",
            d,
            if d == Dialect::Anthropic {
                ModelIds::Claude
            } else {
                ModelIds::Plain
            },
        )
    };
    assert!(with("gpt-6.2-sol", Dialect::OpenAi).is_ok());
    assert!(with("ft:gpt-6_mini.v2", Dialect::OpenAi).is_ok());
    assert!(with("bad model!", Dialect::OpenAi).is_err());
    assert!(with(&"a".repeat(101), Dialect::OpenAi).is_err());
    assert!(with("gpt-6.2-sol", Dialect::Anthropic).is_err());
    assert!(with("claude-haiku-5-5", Dialect::Anthropic).is_ok());
    let t = turn::turn(
        &json!({ "messages": msgs }),
        &Value::Null,
        "gpt-6.2-sol",
        Dialect::OpenAi,
        ModelIds::Plain,
    )
    .unwrap();
    assert_eq!(t.model, "gpt-6.2-sol");
    let betas = turn::turn(
        &json!({ "messages": msgs }),
        &json!(["compact-2026-01-12"]),
        "gpt-6.2-sol",
        Dialect::OpenAi,
        ModelIds::Plain,
    );
    assert!(betas.unwrap_err().contains("betas"));
}

#[test]
fn models_openai_prices() {
    use pigeon_core::ai_gateway::canonical_model;
    use pigeon_core::model_prices::{cost_micros, estimate_micros, price};
    assert_eq!(canonical_model("openai/gpt-6.2-sol"), "gpt-6.2-sol");
    assert_eq!(
        canonical_model("anthropic/claude-haiku-5.5"),
        "claude-haiku-5-5"
    );
    let sol = price("openai/gpt-6.2-sol");
    assert_eq!(
        (sol.input, sol.cache_read, sol.output),
        (2_000_000, 100_000, 10_000_000)
    );
    assert_eq!(
        (sol.cache_write_5m, sol.cache_write_1h),
        (sol.input, sol.input)
    );
    assert_eq!(price("gpt-6.2-astra").input, 8_000_000);
    assert_eq!(price("gpt-6.1-terra").output, 5_000_000);
    assert_eq!(price("gpt-6-luna").cache_read, 10_000);
    // A dash suffix is the model; any other longer name is unknown, so the dearest.
    assert_eq!(price("gpt-6.2-sol-mini"), sol);
    assert_eq!(price("gpt-6.2-solar"), price("claude-fable-5-2"));
    assert_eq!(price("gpt-9"), price("claude-fable-5-2"));
    // Short context.
    let short = json!({ "input_tokens": 100_000, "cache_read_input_tokens": 100_000,
        "output_tokens": 1_000 });
    assert_eq!(
        cost_micros("gpt-6.2-sol", &short),
        200_000 + 10_000 + 10_000
    );
    // Long context: input plus cache read over 272,000 uses the long price for everything.
    let long = json!({ "input_tokens": 200_000, "cache_read_input_tokens": 100_000,
        "output_tokens": 1_000 });
    assert_eq!(cost_micros("gpt-6.2-sol", &long), 800_000 + 20_000 + 15_000);
    let edge = json!({ "input_tokens": 272_000, "output_tokens": 0 });
    assert_eq!(cost_micros("gpt-6.2-sol", &edge), 544_000);
    assert_eq!(estimate_micros("gpt-6.2-sol", 272_001, 0), 1_088_004);
    // Claude has no long price.
    let big = json!({ "input_tokens": 500_000 });
    assert_eq!(cost_micros("claude-sonnet-5-5", &big), 1_000_000);
}

#[test]
fn models_openai_provider_declared_with_secret() {
    use pigeon_models::{Billing, Dialect, Gateway, Models, Provider, Route};
    // The declaration is in the wasm-only Worker crate: read it as text, then build the same.
    let src = include_str!("../src/model-gateway/src/lib.rs");
    for needle in [
        "slug: \"openai\"",
        "dialect: Dialect::OpenAi",
        "default_model: \"gpt-6.2-sol\"",
    ] {
        assert!(src.contains(needle), "{needle}");
    }
    assert!(src.find("slug: \"anthropic\"") < src.find("slug: \"openai\""));
    let provider = |name, slug, route, dialect, billing| Provider {
        name,
        gateway: "pigeon",
        route,
        slug,
        alias: "default",
        dialect,
        ids: pigeon_models::ModelIds::Plain,
        default_model: "m",
        key_help: "help",
        billing,
    };
    let models = Models {
        gateways: vec![Gateway {
            id: "pigeon".into(),
            kind: pigeon_core::ai_gateway::GatewayKind::Credits,
        }],
        providers: vec![
            provider(
                "anthropic",
                "anthropic",
                Route::Byok,
                Dialect::Anthropic,
                Some(Billing::AnthropicAdmin),
            ),
            provider("cf-openai", "openai", Route::Credits, Dialect::OpenAi, None),
        ],
        staged: Vec::new(),
        secrets_page: "https://x/secrets".into(),
        host: "h".into(),
        proxy_host: "p".into(),
    };
    // Only the BYOK provider has a key slot: credits need none.
    let slots = models.secrets();
    assert!(!slots.iter().any(|s| s.name == "pigeon_openai_default"));
    assert_eq!(slots[0].name, "pigeon_anthropic_default");
    assert_eq!(
        slots[0].scopes,
        vec![pigeon_core::ai_gateway::SECRET_SCOPE.to_string()]
    );
    let tools = models.tools();
    for name in ["models_run", "models_submit"] {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(
            t.input_schema["properties"]["provider"]["enum"],
            json!(["anthropic", "cf-openai"])
        );
    }
    // `models_billing` offers only a provider that has a billing source.
    let billing = tools.iter().find(|t| t.name == "models_billing").unwrap();
    assert_eq!(
        billing.input_schema["properties"]["provider"]["enum"],
        json!(["anthropic"])
    );
}

// ---- AI agents: model choice per run (docs/plans/model-providers.md, phase 5) ----

/// A choice is read from `provider/model`, a bare `claude-…` name or a bare provider; bad ones
/// are refused with words; tiers and the built-in defaults are today's.
#[test]
fn ai_agents_models_choice_tiers_and_shorthand() {
    use pigeon_ai_agents::models::{Choice, Slot, Tier, builtin, tier_model};
    let c = |t: &str, s: Slot| Choice::parse(t, s);
    let ok = |p: &str, m: &str| Choice {
        provider: p.into(),
        model: m.into(),
    };
    assert_eq!(
        c("cf-openai/gpt-6.2-sol", Slot::Executor),
        Ok(ok("cf-openai", "gpt-6.2-sol"))
    );
    assert_eq!(
        c("anthropic/claude-opus-5-5", Slot::Planner),
        Ok(ok("anthropic", "claude-opus-5-5"))
    );
    assert_eq!(
        c("claude-haiku-5-5", Slot::Planner),
        Ok(ok("anthropic", "claude-haiku-5-5"))
    );
    assert_eq!(
        c("cf-openai", Slot::Planner),
        Ok(ok("cf-openai", "gpt-6.2-astra"))
    );
    assert_eq!(
        c("cf-openai", Slot::Executor),
        Ok(ok("cf-openai", "gpt-6.2-sol"))
    );
    assert_eq!(
        c("cf-openai", Slot::Mechanical),
        Ok(ok("cf-openai", "gpt-6.1-terra"))
    );
    assert_eq!(
        c("cf-openai", Slot::Summary),
        Ok(ok("cf-openai", "gpt-6.1-terra"))
    );
    assert_eq!(
        c("anthropic", Slot::Mechanical),
        Ok(ok("anthropic", "claude-haiku-5-5"))
    );
    assert_eq!(
        c("cf-openai/gpt-6-luna", Slot::Mechanical),
        Ok(ok("cf-openai", "gpt-6-luna"))
    );
    assert!(
        c("gemini/x", Slot::Executor)
            .unwrap_err()
            .contains("unknown provider")
    );
    assert!(c("gpt-6.2-sol", Slot::Executor).is_err());
    assert!(c("cf-openai/", Slot::Executor).is_err());
    assert!(c("cf-openai/gpt 6", Slot::Executor).is_err());
    assert!(c(&format!("cf-openai/{}", "a".repeat(101)), Slot::Executor).is_err());
    assert!(c(&format!("cf-openai/{}", "a".repeat(100)), Slot::Executor).is_ok());
    assert_eq!(
        c("cf-openai/ft:gpt_6.2-sol", Slot::Executor).unwrap().model,
        "ft:gpt_6.2-sol"
    );
    assert_eq!(tier_model("cf-openai", Tier::Top), Some("gpt-6.2-astra"));
    assert_eq!(tier_model("mistral", Tier::Top), None);
    assert_eq!(builtin(Slot::Planner).text(), "anthropic/claude-opus-5-5");
    assert_eq!(
        builtin(Slot::Executor).text(),
        "anthropic/claude-sonnet-5-5"
    );
    assert_eq!(
        builtin(Slot::Mechanical).text(),
        "anthropic/claude-haiku-5-5"
    );
    assert_eq!(builtin(Slot::Summary).text(), "anthropic/claude-haiku-5-5");
}

/// The fallback chain stays in the provider and mirrors `Role::models()` for Anthropic.
#[test]
fn ai_agents_models_fallback_chains() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Choice, Slot, chain, chain_of};
    let ids = |v: Vec<Choice>| v.iter().map(Choice::text).collect::<Vec<_>>();
    for role in [Role::Planner, Role::Executor, Role::Mechanical] {
        let from_models: Vec<String> = role
            .models()
            .iter()
            .map(|m| format!("anthropic/{m}"))
            .collect();
        assert_eq!(ids(chain_of(None, role)), from_models);
    }
    let p = |t: &str, s| Choice::parse(t, s).unwrap();
    assert_eq!(
        ids(chain(&p("cf-openai", Slot::Planner), Role::Planner)),
        ["cf-openai/gpt-6.2-astra", "cf-openai/gpt-6.2-sol"]
    );
    assert_eq!(
        ids(chain(&p("cf-openai", Slot::Executor), Role::Executor)),
        ["cf-openai/gpt-6.2-sol", "cf-openai/gpt-6.2-astra"]
    );
    assert_eq!(
        ids(chain(&p("cf-openai", Slot::Mechanical), Role::Mechanical)),
        ["cf-openai/gpt-6.1-terra", "cf-openai/gpt-6.2-sol"]
    );
    // An unknown model has no fallback; a model that is its own fallback has none to add.
    assert_eq!(
        ids(chain(&p("cf-openai/gpt-9", Slot::Executor), Role::Executor)),
        ["cf-openai/gpt-9"]
    );
    assert_eq!(
        ids(chain(
            &p("anthropic/claude-x-1", Slot::Executor),
            Role::Executor
        )),
        ["anthropic/claude-x-1"]
    );
    assert_eq!(
        ids(chain(
            &p("cf-openai/gpt-6.2-astra", Slot::Executor),
            Role::Executor
        )),
        ["cf-openai/gpt-6.2-astra"]
    );
    // A stored model that cannot be read behaves as no model.
    assert_eq!(
        ids(chain_of(Some("nonsense"), Role::Executor)),
        ids(chain_of(None, Role::Executor))
    );
}

/// The merge order, the stored defaults and the answer of `ai_agents_models`.
#[test]
fn ai_agents_models_merge_and_stored_defaults() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{
        ChildModels, Choice, Defaults, Slot, check_set, defaults_answer, merge,
    };
    let p = |t: &str, s| Choice::parse(t, s).unwrap();
    let mut d = Defaults::default();
    // Built in, then stored, then the planner's, then the run's own.
    assert_eq!(
        merge(Role::Executor, None, None, &d).text(),
        "anthropic/claude-sonnet-5-5"
    );
    d.put(Slot::Executor, p("cf-openai/gpt-6.2-sol", Slot::Executor));
    assert_eq!(
        merge(Role::Executor, None, None, &d).text(),
        "cf-openai/gpt-6.2-sol"
    );
    let kids =
        ChildModels::parse(&json!({ "executor": "anthropic", "mechanical": "cf-openai" })).unwrap();
    assert_eq!(
        merge(Role::Executor, None, Some(&kids), &d).text(),
        "anthropic/claude-sonnet-5-5"
    );
    assert_eq!(
        merge(Role::Mechanical, None, Some(&kids), &d).text(),
        "cf-openai/gpt-6.1-terra"
    );
    let own = p("cf-openai/gpt-6-luna", Slot::Executor);
    assert_eq!(
        merge(Role::Executor, Some(&own), Some(&kids), &d).text(),
        "cf-openai/gpt-6-luna"
    );
    // A planner ignores `models`.
    assert_eq!(
        merge(Role::Planner, None, Some(&kids), &d).text(),
        "anthropic/claude-opus-5-5"
    );

    // What is set is checked.
    let set = check_set(&json!({ "summary": "cf-openai", "planner": "claude-opus-5-5" })).unwrap();
    assert_eq!(set.len(), 2);
    assert!(check_set(&json!({ "boss": "cf-openai" })).is_err());
    assert!(check_set(&json!({ "executor": "nope/x" })).is_err());
    assert!(check_set(&json!({ "executor": 3 })).is_err());
    assert!(check_set(&json!("cf-openai")).is_err());

    // Stored as JSON and read back; the answer marks each default.
    assert_eq!(Defaults::from_json(&d.to_json()), d);
    let a = defaults_answer(&d, &[]);
    assert_eq!(
        a["defaults"]["executor"],
        json!({ "model": "cf-openai/gpt-6.2-sol", "route": "credits", "source": "stored", "effort": "medium" })
    );
    assert_eq!(a["defaults"]["planner"]["source"], "built_in");
    assert_eq!(
        a["defaults"]["summary"]["model"],
        "anthropic/claude-haiku-5-5"
    );
    assert_eq!(a["tiers"]["cf-openai"]["middle"], "gpt-6.2-sol");
    assert_eq!(a["tiers"]["anthropic"]["small"], "claude-haiku-5-5");
    assert_eq!(a["tiers"]["cf-openai"]["other"], json!(["gpt-6-luna"]));
    assert!(a["example"]["args"]["set"].is_object());
}

/// `ai_agents_start` and `spawn_executor` check their model arguments; the request carries
/// thinking only for Claude, effort for every model that takes it; the tools are offered.
#[test]
fn ai_agents_models_start_spawn_and_request() {
    use pigeon_ai_agents::models::{spawn_model, start_models};
    use pigeon_ai_agents::transcript::{self, Spec};
    use pigeon_ai_agents::{Role, service, tools};
    // The old shape of start_args still works.
    assert!(service::start_args(&json!({ "brief": "x", "model": "cf-openai" })).is_ok());
    let (own, kids) = start_models(
        Role::Planner,
        &json!({ "model": "cf-openai", "models": { "executor": "anthropic" } }),
    )
    .unwrap();
    assert_eq!(own.unwrap().text(), "cf-openai/gpt-6.2-astra");
    assert_eq!(kids.executor.unwrap().text(), "anthropic/claude-sonnet-5-5");
    let (own, kids) = start_models(Role::Executor, &json!({ "brief": "x" })).unwrap();
    assert!(own.is_none() && kids.is_empty());
    assert!(
        start_models(
            Role::Executor,
            &json!({ "models": { "executor": "cf-openai" } })
        )
        .is_err()
    );
    assert!(
        start_models(
            Role::Planner,
            &json!({ "models": { "planner": "cf-openai" } })
        )
        .is_err()
    );
    assert!(start_models(Role::Planner, &json!({ "model": "bogus/x" })).is_err());
    assert!(start_models(Role::Planner, &json!({ "model": 4 })).is_err());

    // spawn_executor.
    let spawn = |v: serde_json::Value| tools::check(Role::Planner, "spawn_executor", &v);
    assert!(spawn(json!({ "brief": "b", "model": "cf-openai/gpt-6.2-sol" })).is_ok());
    assert!(spawn(json!({ "brief": "b", "model": "cf-openai/bad model" })).is_err());
    assert!(spawn(json!({ "brief": "b", "model": 3 })).is_err());
    assert_eq!(
        spawn_model(Role::Mechanical, &json!({ "model": "cf-openai" }))
            .unwrap()
            .unwrap()
            .model,
        "gpt-6.1-terra"
    );
    assert!(spawn_model(Role::Executor, &json!({})).unwrap().is_none());

    // The request: Claude thinks; OpenAI carries effort only.
    let defs = tools::definitions(Role::Planner, &[]);
    let body = |model: &str| {
        let spec = Spec {
            role: Role::Planner,
            model,
            system: "s",
            tools: &defs,
        };
        transcript::request(&spec, &[json!({ "role": "user", "content": "hi" })])
    };
    let claude = body("claude-opus-5-5");
    assert_eq!(claude["thinking"]["type"], "adaptive");
    assert_eq!(claude["output_config"]["effort"], "high");
    let gpt = body("gpt-6.2-astra");
    assert_eq!(gpt["model"], "gpt-6.2-astra");
    assert!(gpt.get("thinking").is_none());
    assert_eq!(gpt["output_config"]["effort"], "high");
    assert!(!transcript::thinks("gpt-6.2-astra") && transcript::thinks("claude-sonnet-5-5"));
    assert!(body("claude-haiku-5-5").get("output_config").is_none());
    assert!(service::TOOLS.contains(&"ai_agents_models"));
    assert!(service::TOOLS.contains(&"ai_agents_compare"));
}

/// `ai_agents_compare` groups ended runs by role and model, with percentiles by nearest rank.
#[test]
fn ai_agents_models_compare_grouping() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::compare::{days_arg, groups, percentile, role_arg};
    let run = |role: &str, model: Option<&str>, state: &str, micros: u64, turns: u64, min: i64| {
        json!({
            "role": role, "model": model, "state": state,
            "spent": { "cost_micros": micros, "turns": turns },
            "created_at": 0, "started_at": 0, "ended_at": min * 60_000,
        })
    };
    let sonnet = Some("anthropic/claude-sonnet-5-5");
    let sol = Some("cf-openai/gpt-6.2-sol");
    let rows = vec![
        run("executor", None, "done", 1_000_000, 10, 5),
        run("executor", sonnet, "failed", 3_000_000, 30, 15),
        run("executor", sonnet, "stopped", 2_000_000, 20, 10),
        run("executor", sol, "done", 500_000, 5, 2),
        run("executor", sol, "over_budget", 1_500_000, 8, 4),
        run("mechanical", None, "done", 100_000, 3, 1),
        run("executor", sol, "running", 9, 9, 9),
    ];
    let all = groups(&rows, None);
    assert_eq!(all.len(), 3);
    // Sorted by role, then model; a run with no model is its role's built-in default.
    assert_eq!(all[0]["model"], "anthropic/claude-sonnet-5-5");
    assert_eq!(all[0]["runs"], 3);
    assert_eq!(all[0]["done"], 1);
    assert_eq!(all[0]["failed"], 1);
    assert_eq!(all[0]["stopped"], 1);
    assert_eq!(all[0]["cost_usd"]["total"], 6.0);
    assert_eq!(all[0]["cost_usd"]["median"], 2.0);
    assert_eq!(all[0]["cost_usd"]["high"], 3.0);
    assert_eq!(all[0]["turns"]["median"], 20.0);
    assert_eq!(all[0]["minutes"]["median"], 10.0);
    assert_eq!(all[1]["model"], "cf-openai/gpt-6.2-sol");
    assert_eq!(all[1]["runs"], 2);
    assert_eq!(all[1]["over_budget"], 1);
    assert_eq!(all[2]["role"], "mechanical");
    assert_eq!(all[2]["model"], "anthropic/claude-haiku-5-5");
    assert_eq!(groups(&rows, Some(Role::Executor)).len(), 2);
    assert!(groups(&[], None).is_empty());
    assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 50.0), 2.0);
    assert_eq!(percentile(&[], 90.0), 0.0);
    assert_eq!(days_arg(&json!(null)), Ok(30));
    assert!(days_arg(&json!(0)).is_err() && days_arg(&json!(366)).is_err());
    assert_eq!(days_arg(&json!(365)), Ok(365));
    assert_eq!(role_arg(&json!(null)), Ok(None));
    assert!(role_arg(&json!("boss")).is_err());
}

// ---- ai agents: names and structured questions ----

/// Run names, the structured `ask_person` input, old stored waits and the assumed entry shape.
#[test]
fn ai_agents_run_names_and_structured_questions() {
    use pigeon_ai_agents::object::{assumed_entry, run_name};
    use pigeon_ai_agents::run::Wait;
    use pigeon_ai_agents::{Role, tools};

    // Names: given wins, else the title, cut on a word boundary at 40 characters.
    assert_eq!(
        run_name(Some("  Cost tab fix "), "Brief: x"),
        "Cost tab fix"
    );
    assert_eq!(run_name(None, "Brief: Fix the page\nmore"), "Fix the page");
    assert_eq!(run_name(Some("  "), "Fix it"), "Fix it");
    let long = run_name(None, &"word ".repeat(30));
    assert!(long.ends_with('…') && long.chars().count() <= 41, "{long}");
    assert!(long.trim_end_matches('…').ends_with("word"));
    // A name that is wrong is refused with words.
    let spawn = |n: Value| {
        tools::check(
            Role::Planner,
            "spawn_executor",
            &json!({ "brief": "b", "name": n }),
        )
    };
    assert!(spawn(json!("Cost tab fix")).is_ok());
    assert!(spawn(json!("")).is_err() && spawn(json!("a\nb")).is_err());
    assert!(spawn(json!("x".repeat(61))).is_err() && spawn(json!(3)).is_err());
    assert!(
        tools::check_run_name(Some(&json!(" ok ")))
            .unwrap()
            .as_deref()
            == Some("ok")
    );

    // ask_person validation.
    let ask = |v: Value| tools::check(Role::Executor, "ask_person", &v);
    let good = json!({
        "question": "Which?", "headline": "Pick one", "options": ["A", "B"], "conventional": 1,
        "facts": [{ "label": "Blocked", "value": "x", "mono": true }]
    });
    assert!(ask(good.clone()).is_ok());
    assert!(ask(json!({ "question": "Which?" })).is_ok());
    let mut bad = good.clone();
    bad["conventional"] = json!(2);
    assert!(ask(bad).unwrap_err().contains("conventional"));
    assert!(ask(json!({ "question": "q", "conventional": 0 })).is_err());
    let mut many = good;
    many["facts"] = json!(
        (0..7)
            .map(|_| json!({ "label": "l", "value": "v" }))
            .collect::<Vec<_>>()
    );
    assert!(ask(many).unwrap_err().contains("facts"));

    // A wait stored before the new fields still reads.
    let w: Wait =
        serde_json::from_value(json!({ "for": "answer", "call": "t", "question": "q" })).unwrap();
    assert!(matches!(
        w,
        Wait::Answer {
            headline: None,
            conventional: None,
            answer_by: 0,
            ..
        }
    ));

    // The assumed entry: with a chosen option, and an old one.
    let e = json!({ "question": "q", "headline": "h", "facts": [{ "label": "a", "value": "b" }],
        "options": ["A", "B", "C"], "conventional": 1, "chose": "B", "at": 5 });
    let v = assumed_entry("r1", "Name", "Title", Some("br"), &e);
    assert_eq!(v["chose"], "B");
    assert_eq!(v["instead"], json!(["A", "C"]));
    assert_eq!(v["name"], "Name");
    assert_eq!(v["branch"], "br");
    let old = assumed_entry(
        "r1",
        "Name",
        "Title",
        None,
        &json!({ "question": "q", "at": 3 }),
    );
    assert_eq!(old["facts"], json!([]));
    assert_eq!(old["options"], json!([]));
    assert_eq!(old["instead"], json!([]));
    assert!(old["chose"].is_null() && old["headline"].is_null() && old["conventional"].is_null());
}

/// The model gateway's two gateways: specs, drift, the credits guard, routes, names and the daily
/// answer over both gateways' logs (docs/plans/model-providers-cloudflare.md, phase 1).
#[test]
fn models_two_gateways_specs_guard_routes_and_daily() {
    use pigeon_core::ai_gateway::{
        GatewayKind, ProviderConfig, credits_guard, drift, spec, stored_key_drift,
    };
    use pigeon_models::daily::{GatewayLogs, daily_by_gateways, family_of};
    use pigeon_models::{Billing, Dialect, Gateway, Models, Provider, Route, route_of};

    // The spec of each kind.
    let byok = spec("g", "store", GatewayKind::Byok);
    assert_eq!(byok["byok_only"], true);
    assert_eq!(byok["store_id"], "store");
    let credits = spec("g", "store", GatewayKind::Credits);
    assert_eq!(credits["byok_only"], false);
    assert_eq!(credits["authentication"], true);
    assert_eq!(credits["zdr"], true);
    assert!(credits.get("store_id").is_none());
    // Pigeon neither sets nor requires a spend limit (the person's decision).
    assert!(credits.get("spend_limits").is_none());

    // Drift: none for a gateway as wanted; lines for one that is not. A spend limit, on or off,
    // is the person's own and never drift.
    assert!(drift(&credits, &credits, GatewayKind::Credits).is_empty());
    assert!(drift(&byok, &byok, GatewayKind::Byok).is_empty());
    let found = json!({ "authentication": true, "byok_only": true, "collect_logs": true,
        "zdr": true, "workers_ai_billing_mode": "unified", "spend_limits": { "enabled": false, "rules": [] } });
    let lines = drift(&credits, &found, GatewayKind::Credits);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("byok_only"));

    // Stored keys.
    let config = |slug: &str| ProviderConfig {
        id: "i".into(),
        provider_slug: slug.into(),
        alias: "default".into(),
        default_config: true,
        secret_id: None,
        secret_preview: None,
        last_used_at: None,
        modified_at: None,
    };
    assert_eq!(
        stored_key_drift(GatewayKind::Credits, &[config("anthropic")], &[]),
        vec![
            "holds a stored key for anthropic: credits requests for it would bill that key instead"
        ]
    );
    assert!(stored_key_drift(GatewayKind::Credits, &[], &["anthropic"]).is_empty());
    assert_eq!(
        stored_key_drift(GatewayKind::Byok, &[], &["anthropic"]).len(),
        1
    );
    assert!(stored_key_drift(GatewayKind::Byok, &[config("anthropic")], &["anthropic"]).is_empty());

    // The credits guard names the gateway it was given and refuses any stored key.
    let open = json!({ "byok_only": false });
    assert!(credits_guard("pigeon-cloudflare", &open, &[]).is_ok());
    let e = credits_guard("pigeon-cloudflare", &json!({ "byok_only": true }), &[]).unwrap_err();
    assert!(e.contains("`pigeon-cloudflare`") && e.contains("requires provider keys"));
    let e = credits_guard("pigeon-cloudflare", &open, &[config("openai")]).unwrap_err();
    assert!(e.contains("`pigeon-cloudflare`") && e.contains("stored openai key"));
    assert!(e.contains("delete it in the dashboard"));

    // Routes do not depend on time.
    assert_eq!(
        route_of(GatewayKind::Credits, "anthropic"),
        ("cf-anthropic".to_string(), Route::Credits)
    );
    assert_eq!(
        route_of(GatewayKind::Credits, "openai"),
        ("cf-openai".to_string(), Route::Credits)
    );
    assert_eq!(
        route_of(GatewayKind::Byok, "anthropic"),
        ("anthropic".to_string(), Route::Byok)
    );
    assert_eq!(
        route_of(GatewayKind::Retired, "openai"),
        ("openai".to_string(), Route::Byok)
    );
    assert_eq!(family_of("cf-openai", "gpt-6.2-sol"), "OpenAI");
    assert_eq!(family_of("cf-anthropic", "claude-opus-5-5"), "Opus");

    // Selection by name, and the key slots.
    let provider = |name, gateway, route, slug, dialect, billing| Provider {
        name,
        gateway,
        route,
        slug,
        alias: "default",
        dialect,
        ids: pigeon_models::ModelIds::Plain,
        default_model: "m",
        key_help: "help",
        billing,
    };
    let models = Models {
        gateways: vec![
            Gateway {
                id: "pigeon".into(),
                kind: GatewayKind::Credits,
            },
            Gateway {
                id: "pigeon-anthropic".into(),
                kind: GatewayKind::Byok,
            },
        ],
        providers: vec![
            provider(
                "anthropic",
                "pigeon",
                Route::Byok,
                "anthropic",
                Dialect::Anthropic,
                Some(Billing::AnthropicAdmin),
            ),
            provider(
                "cf-anthropic",
                "pigeon",
                Route::Credits,
                "anthropic",
                Dialect::Anthropic,
                None,
            ),
            provider(
                "cf-openai",
                "pigeon",
                Route::Credits,
                "openai",
                Dialect::OpenAi,
                None,
            ),
        ],
        staged: vec![("pigeon-anthropic", "anthropic", "default")],
        secrets_page: "https://x/secrets".into(),
        host: "h".into(),
        proxy_host: "p".into(),
    };
    let names: Vec<String> = models.secrets().into_iter().map(|s| s.name).collect();
    assert_eq!(
        &names[..2],
        [
            "pigeon_anthropic_default",
            "pigeon-anthropic_anthropic_default"
        ]
    );
    assert!(!names.iter().any(|n| n.contains("openai")));
    let tools = models.tools();
    let run = tools.iter().find(|t| t.name == "models_run").unwrap();
    assert_eq!(
        run.input_schema["properties"]["provider"]["enum"],
        json!(["anthropic", "cf-anthropic", "cf-openai"])
    );

    // Daily over two gateways.
    let log = |at: &str, provider: &str, model: &str, cost: f64| -> pigeon_core::ai_gateway::Log {
        serde_json::from_value(json!({
            "created_at": at, "provider": provider, "model": model, "cost": cost,
        }))
        .unwrap_or_default()
    };
    let gateways = vec![
        GatewayLogs {
            id: "pigeon-anthropic".into(),
            kind: GatewayKind::Byok,
            logs: vec![log(
                "2026-06-02T01:00:00Z",
                "anthropic",
                "claude-opus-5-5",
                1.0,
            )],
            truncated: false,
        },
        GatewayLogs {
            id: "pigeon".into(),
            kind: GatewayKind::Credits,
            logs: vec![
                log("2026-05-31T01:00:00Z", "anthropic", "claude-opus-5-5", 0.5),
                log("2026-06-02T02:00:00Z", "openai", "gpt-6.2-sol", 0.25),
            ],
            truncated: true,
        },
    ];
    let out = daily_by_gateways(&gateways, "2026-06-02", 4);
    assert_eq!(out.by_route["byok"]["2026-06-02"], 1.0);
    assert_eq!(out.by_route["credits"]["2026-05-31"], 0.5);
    assert_eq!(out.by_route["credits"]["2026-06-02"], 0.25);
    assert_eq!(out.by_provider["cf-openai"]["2026-06-02"], 0.25);
    assert_eq!(out.by_provider["anthropic"]["2026-06-02"], 1.0);
    assert_eq!(out.by_family["OpenAI"]["2026-06-02"], 0.25);
    assert!(!out.gateways["pigeon-anthropic"].truncated);
    assert!(out.gateways["pigeon"].truncated);
    assert_eq!(out.gateways["pigeon"].logs, 2);
}

/// Models by who pays: cf-anthropic and cf-openai tiers, the route on models and compare, the
/// refusal of `openai/…`, a stored openai default read as unset, and a
/// fallback that stays within its provider.
#[test]
fn ai_agents_models_credits_routes_and_openai_accepted() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::compare::groups;
    use pigeon_ai_agents::models::{
        Choice, Defaults, Slot, Tier, chain, defaults_answer, ignored_notes, route, tier_model,
    };
    let p = |t: &str, s| Choice::parse(t, s).unwrap();
    // Tiers of the two credits providers.
    assert_eq!(
        tier_model("cf-anthropic", Tier::Top),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        tier_model("cf-anthropic", Tier::Middle),
        Some("claude-sonnet-5-5")
    );
    assert_eq!(
        tier_model("cf-anthropic", Tier::Small),
        Some("claude-haiku-5-5")
    );
    assert_eq!(tier_model("cf-openai", Tier::Top), Some("gpt-6.2-astra"));
    assert_eq!(tier_model("cf-openai", Tier::Middle), Some("gpt-6.2-sol"));
    assert_eq!(tier_model("cf-openai", Tier::Small), Some("gpt-6.1-terra"));
    assert_eq!(
        p("cf-anthropic", Slot::Planner).text(),
        "cf-anthropic/claude-opus-5-5"
    );
    assert_eq!(
        p("claude-sonnet-5-5", Slot::Executor).text(),
        "anthropic/claude-sonnet-5-5"
    );
    assert_eq!(route("anthropic"), "byok");
    assert_eq!(route("cf-anthropic"), "credits");
    assert_eq!(route("cf-openai"), "credits");
    // `openai/...` is accepted now: the gateway has a BYOK `openai` provider.
    for t in ["openai/gpt-6.2-sol", "openai"] {
        assert!(Choice::parse(t, Slot::Executor).is_ok(), "{t}");
    }
    // Workers AI is built (phase 6); its choices are checked in
    // ai_agents_workers_ai_choices.
    assert_eq!(
        p("workers-ai", Slot::Executor).text(),
        "workers-ai/@cf/moonshotai/kimi-k2.7-code"
    );
    // A stored default of an unknown provider reads as unset, and the answer says so.
    let raw = json!({ "executor": "mistral/large", "planner": "cf-openai/gpt-6.2-astra" });
    let d = Defaults::from_json(&raw);
    assert!(d.stored_for(Slot::Executor).is_none());
    let notes = ignored_notes(&raw);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("mistral/large"));
    let a = defaults_answer(&d, &notes);
    assert_eq!(
        a["defaults"]["executor"]["model"],
        "anthropic/claude-sonnet-5-5"
    );
    assert_eq!(a["defaults"]["executor"]["source"], "built_in");
    assert_eq!(a["defaults"]["executor"]["route"], "byok");
    assert_eq!(a["defaults"]["planner"]["route"], "credits");
    assert_eq!(a["notes"].as_array().unwrap().len(), 1);
    // Fallback stays within the provider, so within the route.
    for (text, want) in [
        (
            "cf-anthropic/claude-sonnet-5-5",
            "cf-anthropic/claude-opus-5-5",
        ),
        ("anthropic/claude-sonnet-5-5", "anthropic/claude-opus-5-5"),
        ("cf-openai/gpt-6.2-sol", "cf-openai/gpt-6.2-astra"),
    ] {
        let ids: Vec<String> = chain(&p(text, Slot::Executor), Role::Executor)
            .iter()
            .map(Choice::text)
            .collect();
        assert_eq!(ids, [text, want]);
    }
    // Compare: the two payers are two rows, each with its route.
    let run = |m: &str| {
        json!({
            "role": "executor", "model": m, "state": "done",
            "spent": { "cost_micros": 1, "turns": 1 },
            "created_at": 0, "started_at": 0, "ended_at": 60_000,
        })
    };
    let rows = vec![
        run("cf-anthropic/claude-sonnet-5-5"),
        run("anthropic/claude-sonnet-5-5"),
    ];
    let g = groups(&rows, None);
    assert_eq!(g.len(), 2);
    assert_eq!(g[0]["model"], "anthropic/claude-sonnet-5-5");
    assert_eq!(g[0]["route"], "byok");
    assert_eq!(g[1]["model"], "cf-anthropic/claude-sonnet-5-5");
    assert_eq!(g[1]["route"], "credits");
}

/// `cost_daily` with the credits route: AI Gateway billing rows are left out and shown as
/// `credits.billing_usd`, Workers AI has its own line, credits usage and a 5% fee join the total,
/// Anthropic's report is reconciled on `anthropic` only, and old closed days read as BYOK.
#[test]
fn platform_cost_daily_credits_route_and_ai_gateway_rows() {
    use pigeon_capabilities::cost::{assemble, billing_days, live_days};
    use std::collections::BTreeMap;
    let today = "2026-03-10";
    let day = |d: &str| (format!("{d}T00:00:00Z"), format!("{d}T23:59:59Z"));
    let row = |name: &str, usd: f64, d: &str| {
        let (a, b) = day(d);
        json!({ "ServiceName": name, "BilledCost": usd, "ChargePeriodStart": a, "ChargePeriodEnd": b })
    };
    let rows = json!([
        row("Workers Paid", 1.0, "2026-03-10"),
        row("Workers AI", 0.5, "2026-03-10"),
        row("AI Gateway Credits", 20.0, "2026-03-10"),
        row("AI Gateway", 3.0, "2026-03-10"),
    ]);
    let billing = billing_days(&rows, today);
    let cf = &billing.days[today]["cloudflare"];
    assert_eq!(num_of(&cf["Workers"]), 1.0);
    assert_eq!(num_of(&cf["Workers AI"]), 0.5);
    assert!(cf.get("AI Gateway").is_none() && cf.get("Other").is_none());

    // 03-08: BYOK Anthropic $3 and credits cf-anthropic $2. 03-10: credits cf-openai $10.
    let models = json!({
        "days": ["2026-03-08", "2026-03-09", "2026-03-10"],
        "by_family": { "Sonnet": { "2026-03-08": 5.0 }, "OpenAI": { "2026-03-10": 10.0 } },
        "by_run": {}, "unattributed": { "2026-03-08": 5.0, "2026-03-10": 10.0 },
        "by_provider": {
            "anthropic": { "2026-03-08": 3.0 },
            "cf-anthropic": { "2026-03-08": 2.0 },
            "cf-openai": { "2026-03-10": 10.0 } },
        "by_provider_model": {
            "anthropic": { "claude-sonnet-5": { "2026-03-08": { "cost_usd": 3.0, "tokens_in": 1000, "tokens_out": 500 } } },
            "cf-anthropic": { "claude-sonnet-5": { "2026-03-08": { "cost_usd": 2.0, "tokens_in": 700, "tokens_out": 300 } } } },
        "by_route": {
            "byok": { "2026-03-08": 3.0 },
            "credits": { "2026-03-08": 2.0, "2026-03-10": 10.0 } },
    });
    let anthropic = json!({ "by_day": { "2026-03-08": {
        "cost_usd": 3.5, "tokens": { "uncached_input": 1000, "output": 500 },
        "by_model": { "claude-sonnet-5": { "cost_usd": 3.5 } } } } });
    let roles = BTreeMap::new();
    let live = live_days(
        Some(&billing),
        Some(&models),
        Some(&roles),
        Some(&anthropic),
    );
    // An old closed day: stored with no route, $4 of gateway cost.
    let stored = BTreeMap::from([(
        "2026-03".to_string(),
        json!({ "2026-03-07": { "models": { "Opus": 4.0 } } }),
    )]);
    let a = assemble("7d", today, &stored, &live, false, false, vec![], vec![]);

    // Cloudflare: 1.0 + 0.5, without the $23 of AI Gateway rows.
    assert_eq!(num_of(&a["cloudflare"]["total"]), 1.5);
    assert_eq!(num_of(&a["credits"]["billing_usd"]), 23.0);
    assert_eq!(num_of(&a["credits"]["usage_usd"]), 12.0);
    assert_eq!(num_of(&a["credits"]["fee_usd"]), 0.6);
    let cp = &a["credits"]["by_provider"];
    assert_eq!(cp[0]["name"], "Anthropic");
    assert_eq!(num_of(&cp[0]["total"]), 2.0);
    assert_eq!(cp[1]["name"], "OpenAI");
    assert_eq!(num_of(&cp[1]["total"]), 10.0);

    // Gateway logs: 4 (old) + 5 + 10 = 19. Anthropic's 3.5 replaces only the BYOK 3.0.
    assert_eq!(num_of(&a["ai_gateway"]["total"]), 19.0);
    // 1.5 + 19 + (3.5 - 3.0) + 0.6
    assert_eq!(num_of(&a["total"]), 21.6);
    let routes = &a["ai_gateway"]["by_route"];
    assert_eq!(routes[0]["name"], "BYOK");
    assert_eq!(num_of(&routes[0]["total"]), 7.0);
    assert_eq!(num_of(&routes[0]["by_day"]["2026-03-07"]), 4.0);
    assert_eq!(routes[1]["name"], "Credits");
    assert_eq!(num_of(&routes[1]["total"]), 12.0);
    let p = &a["ai_gateway"]["by_provider"];
    let names: Vec<&str> = p
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x["name"].as_str())
        .collect();
    assert_eq!(
        names,
        ["Anthropic", "Anthropic (credits)", "OpenAI (credits)"]
    );
    // Reconciled on BYOK only: 3.5 against the gateway's 3.0, not 5.0.
    let recon = &a["anthropic"]["reconciliation"]["days"];
    let d8 = recon
        .as_array()
        .and_then(|r| r.iter().find(|d| d["day"] == "2026-03-08"));
    assert_eq!(d8.map(|d| d["status"].clone()), Some(json!("pricing")));

    // A gateway with no `by_route` reads as all BYOK.
    let mut old_models = models.clone();
    old_models.as_object_mut().map(|m| m.remove("by_route"));
    let live = live_days(None, Some(&old_models), Some(&roles), None);
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
    assert_eq!(num_of(&a["credits"]["usage_usd"]), 0.0);
    assert_eq!(num_of(&a["credits"]["fee_usd"]), 0.0);
    assert_eq!(num_of(&a["ai_gateway"]["by_route"][0]["total"]), 15.0);
    assert_eq!(num_of(&a["total"]), 15.0);
}

/// The three gateways of option B: the spec of the credits gateway, Retired's spec, drift and
/// stored keys, the routes of every kind, and the daily answer with a retired gateway's logs
/// counted as BYOK (docs/plans/model-providers-cloudflare.md, phase 8).
#[test]
fn model_gateway_option_b_gateways() {
    use pigeon_core::ai_gateway::{GatewayKind, ProviderConfig, drift, spec, stored_key_drift};
    use pigeon_models::daily::{GatewayLogs, daily_by_gateways};
    use pigeon_models::{Route, route_of};

    let credits = spec("pigeon-cloudflare", "store", GatewayKind::Credits);
    assert_eq!(credits["id"], "pigeon-cloudflare");
    assert_eq!(credits["authentication"], true);
    assert_eq!(credits["byok_only"], false);
    assert_eq!(credits["collect_logs"], true);
    assert_eq!(credits["zdr"], true);
    assert!(credits.get("store_id").is_none() && credits.get("spend_limits").is_none());

    let retired = spec("pigeon", "store", GatewayKind::Retired);
    assert_eq!(retired["authentication"], true);
    assert_eq!(retired["byok_only"], true);
    assert_eq!(retired["collect_logs"], true);
    assert_eq!(retired["store_id"], "store");
    assert!(drift(&retired, &retired, GatewayKind::Retired).is_empty());
    // Retired never compares zdr or store_id, and never asks for credits.
    let found = json!({ "authentication": true, "byok_only": true, "collect_logs": true,
        "zdr": false, "store_id": "other" });
    assert!(drift(&retired, &found, GatewayKind::Retired).is_empty());
    let found = json!({ "authentication": false, "byok_only": false, "collect_logs": true });
    let lines = drift(&retired, &found, GatewayKind::Retired);
    assert_eq!(lines.len(), 2);
    assert!(!lines.iter().any(|l| l.contains("credits")));

    let config = |slug: &str| ProviderConfig {
        id: "i".into(),
        provider_slug: slug.into(),
        alias: "default".into(),
        default_config: true,
        secret_id: None,
        secret_preview: None,
        last_used_at: None,
        modified_at: None,
    };
    assert_eq!(
        stored_key_drift(
            GatewayKind::Retired,
            &[config("anthropic"), config("openai")],
            &[]
        ),
        vec![
            "holds a stored key for anthropic: delete it to retire this gateway",
            "holds a stored key for openai: delete it to retire this gateway",
        ]
    );
    assert!(stored_key_drift(GatewayKind::Retired, &[], &["anthropic"]).is_empty());

    assert_eq!(
        route_of(GatewayKind::Credits, "anthropic"),
        ("cf-anthropic".into(), Route::Credits)
    );
    assert_eq!(
        route_of(GatewayKind::Byok, "anthropic"),
        ("anthropic".into(), Route::Byok)
    );
    assert_eq!(
        route_of(GatewayKind::Retired, "openai"),
        ("openai".into(), Route::Byok)
    );

    let log = |at: &str, provider: &str, cost: f64| -> pigeon_core::ai_gateway::Log {
        serde_json::from_value(json!({
            "created_at": at, "provider": provider, "model": "gpt-6.2-sol", "cost": cost,
        }))
        .unwrap_or_default()
    };
    let gateways = vec![
        GatewayLogs {
            id: "pigeon".into(),
            kind: GatewayKind::Retired,
            logs: vec![log("2026-06-02T01:00:00Z", "openai", 2.0)],
            truncated: false,
        },
        GatewayLogs {
            id: "pigeon-cloudflare".into(),
            kind: GatewayKind::Credits,
            logs: vec![log("2026-06-02T02:00:00Z", "openai", 0.25)],
            truncated: false,
        },
    ];
    let out = daily_by_gateways(&gateways, "2026-06-02", 2);
    assert_eq!(out.by_route["byok"]["2026-06-02"], 2.0);
    assert_eq!(out.by_route["credits"]["2026-06-02"], 0.25);
    assert_eq!(out.by_provider["openai"]["2026-06-02"], 2.0);
    assert_eq!(out.by_provider["cf-openai"]["2026-06-02"], 0.25);
}

/// BYOK readiness: a gateway that does not exist, one that is not byok_only, one with no key for
/// the slug, and one that is ready.
#[test]
fn model_gateway_byok_readiness() {
    use pigeon_core::ai_gateway::{ProviderConfig, byok_ready};
    let config = |slug: &str| ProviderConfig {
        id: "i".into(),
        provider_slug: slug.into(),
        alias: "default".into(),
        default_config: true,
        secret_id: None,
        secret_preview: None,
        last_used_at: None,
        modified_at: None,
    };
    let g = "pigeon-anthropic";
    let e = byok_ready(None, &[], g, "anthropic").unwrap_err();
    assert!(e.contains("`pigeon-anthropic` does not exist: run models_setup"));
    let open = json!({ "byok_only": false });
    let e = byok_ready(Some(&open), &[config("anthropic")], g, "anthropic").unwrap_err();
    assert!(e.contains("byok_only"));
    let strict = json!({ "byok_only": true });
    let e = byok_ready(Some(&strict), &[config("openai")], g, "anthropic").unwrap_err();
    assert!(e.contains("holds no key for anthropic"));
    assert!(
        e.contains("pigeon-anthropic_anthropic_default on the Access tab, then run models_setup")
    );
    assert!(byok_ready(Some(&strict), &[config("anthropic")], g, "anthropic").is_ok());
}

/// Workers AI through `pigeon-cloudflare`: the REST request and its headers, the billing-mode
/// guard, the spec and its drift, log labelling, family, prices and
/// the missing-gateway test. Pure.
#[test]
fn model_gateway_workers_ai() {
    use pigeon_core::ai_gateway::{
        GatewayKind, WORKERS_AI, credits_guard, drift, endpoint, spec, workers_ai_guard,
        workers_ai_headers,
    };
    use pigeon_core::model_prices::price;
    use pigeon_models::daily::family_of;
    use pigeon_models::{Route, route_of};

    let gw = "pigeon-cloudflare";
    assert_eq!(
        endpoint("acct", gw, WORKERS_AI, "chat/completions"),
        "https://api.cloudflare.com/client/v4/accounts/acct/ai/v1/chat/completions"
    );
    assert_eq!(
        endpoint("acct", gw, "openai", "chat/completions"),
        "https://gateway.ai.cloudflare.com/v1/acct/pigeon-cloudflare/openai/chat/completions"
    );
    let h = workers_ai_headers(gw, "tok");
    assert!(h.contains(&("cf-aig-gateway-id".into(), "pigeon-cloudflare".into())));
    assert!(h.contains(&("Authorization".into(), "Bearer tok".into())));

    // The declaration: provider `workers-ai` on the credits gateway, never a stored key.
    let src = include_str!("../src/model-gateway/src/lib.rs");
    let at = src.find("name: \"workers-ai\"").expect("declared");
    let decl = &src[at..src[at..]
        .find("billing: None")
        .map_or(src.len(), |n| at + n)];
    assert!(decl.contains("gateway: CREDITS_GATEWAY"));
    assert!(decl.contains("route: Route::Credits"));
    assert!(decl.contains("default_model: \"@cf/zai-org/glm-5.3\""));

    // The spec, its drift and the guard.
    let credits = spec(gw, "store", GatewayKind::Credits);
    assert_eq!(credits["workers_ai_billing_mode"], "unified");
    assert!(spec(gw, "store", GatewayKind::Byok)["workers_ai_billing_mode"].is_null());
    let mut found = credits.clone();
    found["workers_ai_billing_mode"] = json!("postpaid");
    let lines = drift(&credits, &found, GatewayKind::Credits);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("workers_ai_billing_mode"));
    assert!(workers_ai_guard(gw, &credits).is_ok());
    for bad in [json!({ "workers_ai_billing_mode": "postpaid" }), json!({})] {
        let e = workers_ai_guard(gw, &bad).unwrap_err();
        assert!(
            e.contains("`pigeon-cloudflare`") && e.contains("Unified Billing"),
            "{e}"
        );
    }
    // credits_guard itself does not look at the billing mode.
    assert!(credits_guard(gw, &json!({ "byok_only": false }), &[]).is_ok());

    // Labelling: the log is `workers-ai`, by credits, with family `Workers AI`.
    assert_eq!(
        route_of(GatewayKind::Credits, "workers-ai"),
        ("workers-ai".to_string(), Route::Credits)
    );
    assert_eq!(route_of(GatewayKind::Credits, "openai").0, "cf-openai");
    assert_eq!(family_of("workers-ai", "@cf/zai-org/glm-5.3"), "Workers AI");

    // A missing gateway: a 404, or Cloudflare's "not found" in the body.
    assert!(pigeon_models::is_missing("call: 404: x"));
    assert!(pigeon_models::is_missing(
        "call: 400: {\"errors\":[{\"message\":\"Gateway Not Found\"}]}"
    ));
    assert!(!pigeon_models::is_missing("call: 500: boom"));

    for (model, input, cached, output) in [
        ("@cf/zai-org/glm-5.3", 1_400_000, 260_000, 4_400_000),
        ("@cf/moonshotai/kimi-k2.7-code", 950_000, 190_000, 4_000_000),
        (
            "@cf/deepseek-ai/deepseek-v4-pro-0813",
            1_320_000,
            44_000,
            3_960_000,
        ),
    ] {
        let p = price(model);
        assert_eq!(
            (p.input, p.cache_read, p.output),
            (input, cached, output),
            "{model}"
        );
    }
}

/// AI agent choices for Workers AI: tiers, the small-slot refusal, `@cf/` names only, the route
/// and a fallback within the provider.
#[test]
fn ai_agents_workers_ai_choices() {
    use pigeon_ai_agents::Role;
    use pigeon_ai_agents::models::{Choice, Slot, Tier, chain, route, tier_model};

    assert_eq!(
        tier_model("workers-ai", Tier::Top),
        Some("@cf/zai-org/glm-5.3")
    );
    assert_eq!(
        tier_model("workers-ai", Tier::Middle),
        Some("@cf/moonshotai/kimi-k2.7-code")
    );
    assert_eq!(tier_model("workers-ai", Tier::Small), None);
    assert_eq!(
        Choice::parse("workers-ai", Slot::Planner).unwrap().text(),
        "workers-ai/@cf/zai-org/glm-5.3"
    );
    for slot in [Slot::Mechanical, Slot::Summary] {
        let e = Choice::parse("workers-ai", slot).unwrap_err();
        assert!(e.contains("no small model"), "{e}");
    }
    let named = "workers-ai/@cf/deepseek-ai/deepseek-v4-pro-0813";
    assert_eq!(Choice::parse(named, Slot::Summary).unwrap().text(), named);
    assert!(Choice::stored(named).is_some());
    assert!(Choice::parse("workers-ai/claude-opus-5-5", Slot::Executor).is_err());
    assert!(Choice::parse("anthropic/@cf/zai-org/glm-5.3", Slot::Executor).is_err());
    assert_eq!(route("workers-ai"), "credits");
    let kimi = Choice::parse("workers-ai", Slot::Executor).unwrap();
    let ids: Vec<String> = chain(&kimi, Role::Executor)
        .iter()
        .map(Choice::text)
        .collect();
    assert_eq!(
        ids,
        [kimi.text(), "workers-ai/@cf/zai-org/glm-5.3".to_string()]
    );
}

/// `cost_daily` counts Workers AI once: the Cloudflare "Workers AI" line is billing minus the
/// gateway's workers-ai cost for the day, never below 0, and the amount taken off is
/// `credits.workers_ai_billing_usd`.
#[test]
fn platform_cost_daily_workers_ai_once() {
    use pigeon_capabilities::cost::{assemble, billing_days, live_days};
    use std::collections::BTreeMap;
    let today = "2026-03-10";
    let row = |name: &str, usd: f64, d: &str| {
        json!({ "ServiceName": name, "BilledCost": usd,
            "ChargePeriodStart": format!("{d}T00:00:00Z"), "ChargePeriodEnd": format!("{d}T23:59:59Z") })
    };
    // 03-09: billing Workers AI $1.00. 03-10: no Workers AI row, only Workers $0.50.
    let billing = billing_days(
        &json!([
            row("Workers AI", 1.0, "2026-03-09"),
            row("Workers Paid", 0.5, "2026-03-10")
        ]),
        today,
    );
    // The gateway logged $0.80 of workers-ai on 03-09 and $0.30 on 03-10.
    let models = json!({
        "days": ["2026-03-09", "2026-03-10"],
        "by_family": { "Workers AI": { "2026-03-09": 0.8, "2026-03-10": 0.3 } },
        "by_run": {}, "unattributed": { "2026-03-09": 0.8, "2026-03-10": 0.3 },
        "by_provider": { "workers-ai": { "2026-03-09": 0.8, "2026-03-10": 0.3 } },
        "by_route": { "byok": {}, "credits": { "2026-03-09": 0.8, "2026-03-10": 0.3 } },
    });
    let roles = BTreeMap::new();
    let live = live_days(Some(&billing), Some(&models), Some(&roles), None);
    let a = assemble(
        "7d",
        today,
        &BTreeMap::new(),
        &live,
        false,
        true,
        vec![],
        vec![],
    );
    let line = a["cloudflare"]["lines"]
        .as_array()
        .and_then(|l| l.iter().find(|x| x["name"] == "Workers AI"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(num_of(&line["total"]), 0.2);
    assert_eq!(num_of(&line["by_day"]["2026-03-09"]), 0.2);
    assert!(line["by_day"].get("2026-03-10").is_none());
    assert_eq!(num_of(&a["credits"]["workers_ai_billing_usd"]), 0.8);
    assert_eq!(num_of(&a["credits"]["usage_usd"]), 1.1);
    // Cloudflare 0.2 + 0.5, gateway logs 1.1, fee 0.055: Workers AI's $1.00 counts once.
    assert_eq!(num_of(&a["cloudflare"]["total"]), 0.7);
    assert_eq!(num_of(&a["ai_gateway"]["total"]), 1.1);
    assert_eq!(num_of(&a["total"]), 1.855);
    let cp = &a["credits"]["by_provider"];
    assert_eq!(cp[0]["name"], "Workers AI");
    assert_eq!(num_of(&cp[0]["total"]), 1.1);
}

/// Workers AI holds no stored token: core's `workers_ai` purpose is Workers AI Read only, the
/// call's headers carry the minted token, no slot or binding is left, and core revokes only a
/// token it minted for a capability Worker. Pure.
#[test]
fn model_gateway_workers_ai_minted_token() {
    use pigeon_capabilities::split::{purpose_permissions, scoped_token_revocable};
    use pigeon_core::ai_gateway::workers_ai_headers;

    assert_eq!(
        purpose_permissions("workers_ai"),
        Some(&["Workers AI Read"][..])
    );
    let h = workers_ai_headers("pigeon-cloudflare", "minted");
    assert!(h.contains(&("cf-aig-gateway-id".into(), "pigeon-cloudflare".into())));
    assert!(h.contains(&("Authorization".into(), "Bearer minted".into())));

    for src in [
        include_str!("../src/models/src/lib.rs"),
        include_str!("../src/models/src/runner.rs"),
        include_str!("../src/model-gateway/src/lib.rs"),
        include_str!("../src/model-gateway/wrangler.toml"),
        include_str!("../src/core/src/ai_gateway.rs"),
    ] {
        assert!(!src.contains("pigeon_workers_ai_token"));
        assert!(!src.contains("WORKERS_AI_TOKEN"));
    }

    // A token minted for a capability Worker is revocable; no other is.
    assert!(scoped_token_revocable(
        "pigeon:worker-workers_ai:2026-01-01T00:00:00Z"
    ));
    for name in [
        "",
        "pigeon:deploy:2026-01-01T00:00:00Z",
        "worker-workers_ai",
        "parent",
    ] {
        assert!(!scoped_token_revocable(name), "{name}");
    }
}

/// The `ANTHROPIC_ADMIN_KEY` value check takes both Admin API key shapes: the Console's
/// organization-scoped user keys (`sk-ant-usr-`) and the older admin keys (`sk-ant-admin`), and
/// refuses any other shape naming both. The slot's purpose says how to make the key and that it
/// expires. Pure.
#[test]
fn models_admin_key_prefixes() {
    use pigeon_models::billing::{ADMIN_KEY_PREFIXES, admin_key_shape};

    assert_eq!(ADMIN_KEY_PREFIXES, ["sk-ant-usr-", "sk-ant-admin"]);
    for ok in ["sk-ant-usr-abc123", "sk-ant-admin01-abc123", "sk-ant-admin"] {
        assert_eq!(admin_key_shape(ok), Ok(()), "{ok}");
    }
    for no in [
        "",
        "sk-ant-api03-abc123",
        "sk-ant-usr",
        "sk-ant-oat01-abc",
        "SK-ANT-USR-abc",
        " sk-ant-usr-abc",
    ] {
        let why = admin_key_shape(no).expect_err(no);
        assert!(why.contains("`sk-ant-usr-`"), "{why}");
        assert!(why.contains("`sk-ant-admin`"), "{why}");
    }

    let src = include_str!("../src/models/src/lib.rs");
    assert!(src.contains("Create API key, Scope Organization"));
    assert!(src.contains("It expires on the"));
    assert!(!src.contains("Anthropic reports no expiry for admin keys"));
}

/// A refused `ANTHROPIC_ADMIN_KEY` shows Anthropic's own reason: `billing::anthropic_reason`
/// reads `error.type` and `error.message` from the answer's body, collapses whitespace, cuts any
/// `sk-ant-` word so no key is shown, and keeps at most 300 characters. The check and `Expiry`
/// both use it, and a 403 names the linked account's admin role. Pure.
#[test]
fn models_admin_key_refusal_reason() {
    use pigeon_models::billing::{REASON_MAX_CHARS, anthropic_reason};
    use serde_json::json;

    let body = json!({"type": "error", "error": {"type": "permission_error",
        "message": "This key  does not have\npermission to read usage."}, "request_id": "req_1"});
    assert_eq!(
        anthropic_reason(&body).as_deref(),
        Some("permission_error: This key does not have permission to read usage.")
    );
    let only_type = json!({"error": {"type": "authentication_error"}});
    assert_eq!(
        anthropic_reason(&only_type).as_deref(),
        Some("authentication_error")
    );
    for none in [json!(null), json!({}), json!({"error": {}}), json!("text")] {
        assert_eq!(anthropic_reason(&none), None, "{none}");
    }

    let leaky = json!({"error": {"type": "authentication_error",
        "message": "invalid key 'sk-ant-usr-SECRET123' given"}});
    let why = anthropic_reason(&leaky).unwrap();
    assert!(!why.contains("SECRET123"), "{why}");
    assert!(why.contains("'sk-ant-…"), "{why}");

    let long = json!({"error": {"type": "permission_error", "message": "x".repeat(1000)}});
    let why = anthropic_reason(&long).unwrap();
    assert_eq!(why.chars().count(), REASON_MAX_CHARS + 1, "{why}");
    assert!(why.ends_with('…'));

    let src = include_str!("../src/models/src/lib.rs");
    assert!(src.contains("Ok((s @ (401 | 403), body)) => Expiry::Refused"));
    assert!(src.contains("(s @ (401 | 403), body) => Err(bad(&admin_refusal(s, &body)))"));
    assert!(src.contains("admin role: an organization admin gives it"));
}

/// Shared model IDs stay separate by gateway, and configured zero-spend gateways still show.
#[test]
fn platform_cost_daily_gateway_models() {
    use pigeon_capabilities::cost::{assemble, live_days};
    use pigeon_core::ai_gateway::{GatewayKind, Log};
    use pigeon_models::daily::{GatewayLogs, daily_by_gateways};
    use std::collections::BTreeMap;

    let today = "2026-03-10";
    let yesterday = "2026-03-09";
    let claude = "anthropic/claude-sonnet-5.5";
    let workers = "@cf/zai-org/glm-5.3";
    let log = |day: &str, provider: &str, model: &str, cost: f64| -> Log {
        serde_json::from_value(json!({
            "created_at": format!("{day}T01:00:00Z"), "provider": provider,
            "model": model, "cost": cost,
        }))
        .unwrap()
    };
    let gateway = |id: &str, kind, logs| GatewayLogs {
        id: id.into(),
        kind,
        logs,
        truncated: false,
    };
    let models = daily_by_gateways(
        &[
            gateway(
                "pigeon-anthropic",
                GatewayKind::Byok,
                vec![
                    log(yesterday, "anthropic", claude, 2.0),
                    log(today, "anthropic", claude, 3.0),
                ],
            ),
            gateway(
                "pigeon-cloudflare",
                GatewayKind::Credits,
                vec![
                    log(today, "anthropic", claude, 4.0),
                    log(today, "workers-ai", workers, 1.0),
                ],
            ),
            gateway(
                "pigeon",
                GatewayKind::Retired,
                vec![log(yesterday, "anthropic", claude, 1.0)],
            ),
        ],
        today,
        7,
    );
    assert_eq!(
        models.by_gateway["pigeon-anthropic"].by_provider_model["anthropic"][claude][today],
        3.0
    );
    assert_eq!(models.by_gateway["pigeon-cloudflare"].by_day[today], 5.0);
    let models = serde_json::to_value(models).unwrap();
    let billed = json!({"by_day": { (yesterday): {"cost_usd": 6.0}}});
    let live = live_days(None, Some(&models), None, Some(&billed));
    let weekly = assemble(
        "7d",
        today,
        &BTreeMap::new(),
        &live,
        false,
        false,
        vec![],
        vec![],
    );
    let get = |answer: &Value, id: &str| -> Value {
        answer["ai_gateway"]["by_gateway"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        get(&weekly, "pigeon-openai"),
        json!({
            "id": "pigeon-openai", "billing": "byok", "provider": "openai", "label": "Models via OpenAI BYOK",
            "total": 0.0, "by_day": {}, "by_model": [],
        })
    );
    let anth = get(&weekly, "pigeon-anthropic");
    assert_eq!(anth["total"], 5.0);
    assert_eq!(
        anth["by_model"],
        json!([{"name": claude, "provider": "anthropic", "total": 5.0,
        "by_day": {(yesterday): 2.0, (today): 3.0}}])
    );
    let unified = get(&weekly, "pigeon-cloudflare");
    assert_eq!(unified["billing"], "unified");
    assert_eq!(unified["by_model"][0]["name"], claude);
    assert_eq!(unified["by_model"][0]["total"], 4.0);
    assert_eq!(unified["by_model"][1]["name"], workers);
    assert_eq!(get(&weekly, "pigeon")["total"], 1.0);
    assert_eq!(weekly["ai_gateway"]["total"], 11.0);
    assert_eq!(weekly["total"], 14.25); // Raw logs + Anthropic adjustment + estimated fee.
    assert_eq!(weekly["net_total"], weekly["total"]);
    assert_eq!(weekly["credits"]["grants"], json!([]));
    let daily = assemble(
        "today",
        today,
        &BTreeMap::new(),
        &live,
        false,
        false,
        vec![],
        vec![],
    );
    assert_eq!(
        get(&daily, "pigeon-anthropic")["by_day"],
        json!({(today): 3.0})
    );
}

/// Legacy days stay unattributed and truncated snapshots cannot overwrite existing detail.
#[test]
fn platform_cost_daily_gateway_rollups() {
    use pigeon_capabilities::cost::{assemble, closed_by_month, live_days, merge_month};
    use std::collections::BTreeMap;

    let old_day = "2026-01-05";
    let day = "2026-01-06";
    let today = "2026-01-07";
    let old = json!({
        (old_day): {"models": {"Sonnet": 9.0}, "gateway_providers": {"anthropic": 9.0}},
        (day): {"models": {"Sonnet": 7.0}, "gateway_details": {"pigeon-anthropic": {
            "total": 7.0, "models": {"anthropic": {"claude-sonnet": 7.0}}, "truncated": false
        }}},
    });
    let partial = json!({
        "days": [day], "truncated": true, "by_family": {"Sonnet": {(day): 5.0}},
        "gateways": {"pigeon-anthropic": {"truncated": true}, "pigeon-cloudflare": {"truncated": false}},
        "by_gateway": {
            "pigeon-anthropic": {"by_day": {(day): 2.0}, "by_provider_model": {"anthropic": {"claude-sonnet": {(day): 2.0}}}},
            "pigeon-cloudflare": {"by_day": {(day): 3.0}, "by_provider_model": {"anthropic": {"claude-sonnet": {(day): 3.0}}}},
        },
    });
    let live = live_days(None, Some(&partial), None, None);
    let split = closed_by_month(&live, today);
    let persisted = merge_month(&old, &split["2026-01"], true);
    assert_eq!(persisted[old_day], old[old_day]);
    assert_eq!(
        persisted[day]["gateway_details"]["pigeon-anthropic"],
        old[day]["gateway_details"]["pigeon-anthropic"]
    );
    let before = BTreeMap::from([("2026-01".into(), old)]);
    let after = BTreeMap::from([("2026-01".into(), persisted)]);
    let current = assemble("7d", today, &before, &live, true, true, vec![], vec![]);
    let historical = assemble(
        "all",
        "2026-03-10",
        &after,
        &BTreeMap::new(),
        false,
        true,
        vec![],
        vec![],
    );
    let historical_gateways = &historical["ai_gateway"]["by_gateway"];
    assert_eq!(&current["ai_gateway"]["by_gateway"], historical_gateways);
    assert_eq!(historical["ai_gateway"]["total"], 16.0);
    assert_eq!(historical_gateways[0]["by_day"], json!({ (day): 7.0 }));
    assert_eq!(historical_gateways[1]["by_day"], json!({ (day): 3.0 }));
}

/// Only dated promotional applications inside the returned period change net cost.
#[test]
fn platform_cost_daily_promotional_grants() {
    use pigeon_capabilities::cost::{apply_promotional_grants, assemble, billing_days, live_days};
    use std::collections::BTreeMap;

    let today = "2026-03-10";
    let billing = billing_days(
        &json!([
            {"ServiceName": "Workers", "BilledCost": 10, "ChargePeriodStart": today},
            {"ServiceName": "AI Gateway prepaid credits", "BilledCost": 50, "ChargePeriodStart": today},
        ]),
        today,
    );
    let live = live_days(Some(&billing), None, None, None);
    let stored = BTreeMap::from([(
        "2026-01".into(),
        json!({"2026-01-01": {"cloudflare": {"Workers": 1}}}),
    )]);
    let config = r#"
        [[grants]]
        name = "Test promotion"
        amount_cents = 1000
        expiry = "2026-03-31"
        applications = [
            { day = "2025-12-31", amount_cents = 25 },
            { day = "2026-01-01", amount_cents = 50 },
            { day = "2026-03-04", amount_cents = 75 },
            { day = "2026-03-10", amount_cents = 125 },
            { day = "2026-03-11", amount_cents = 100 },
        ]
        [[grants]]
        name = "Expired test promotion"
        amount_cents = 100
        expiry = "2026-01-02"
        applications = []
    "#;
    for (range, gross, net) in [("today", 10.0, 8.75), ("7d", 10.0, 8.0), ("all", 11.0, 8.5)] {
        let mut answer = assemble(range, today, &stored, &live, false, true, vec![], vec![]);
        assert_eq!(answer["credits"]["billing_usd"], 50.0);
        assert_eq!(answer["net_total"], gross);
        apply_promotional_grants(&mut answer, config);
        assert_eq!(answer["total"], gross);
        assert_eq!(answer["net_total"], net);
        assert_eq!(
            answer["credits"]["grants"],
            json!([
                {"name": "Test promotion", "amount": 10.0, "used": 2.75, "remaining": 7.25, "expiry": "2026-03-31"},
                {"name": "Expired test promotion", "amount": 1.0, "used": 0.0, "remaining": 0.0, "expiry": "2026-01-02"},
            ])
        );
        apply_promotional_grants(&mut answer, &config.replace("2026-03-04", "2026-02-30"));
        assert_eq!(answer["credits"]["grants"], json!([]));
        assert_eq!(answer["net_total"], gross);
    }
}

#[test]
fn sites_publishing_profile_binds_the_scope_and_preserves_legacy_records() {
    use pigeon_core::site_publish::site;
    let mut record = json!({"sites": [{"name":"first"}, {"name":"second"}]});
    assert_eq!(site(&record).unwrap()["name"], "first");
    record["publishing"] = json!({"state":"enabled", "site":"second"});
    assert_eq!(site(&record).unwrap()["name"], "second");
    record["publishing"]["site"] = json!("unknown");
    assert!(site(&record).is_err());
    record["publishing"] = json!({"state":"disabled"});
    assert!(site(&record).is_err());
}

#[test]
fn instance_tool_profiles_share_discovery_and_binding() {
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_core::mcp::Tool;

    let site: Instance = serde_json::from_value(json!({
        "id": "blog", "title": "Blog", "account": {"kind":"home"},
        "sites": [{"name":"blog", "bucket":"site", "targets":[]}]
    }))
    .unwrap();
    let archive: Instance = serde_json::from_value(json!({
        "id": "archive", "title": "Archive", "account": {"kind":"home"},
        "storage": {"state":"enabled", "store":"archive-store", "bucket":"archive", "prefix":"published/"}
    })).unwrap();
    let catalog = || {
        ["site_deploy", "storage_get_object", "storage_list_objects", "land", "call"]
        .into_iter()
        .map(|name| Tool {
            name,
            description: "",
            input_schema: json!({
                "properties": {"instance": {}, "account": {}, "bucket": {}, "presign": {}, "expires_s": {}, "key": {}},
                "required": ["instance", "account", "bucket", "key"]
            })
        }).collect::<Vec<_>>()
    };
    let site = Scope::new(&site).unwrap();
    let mut listed = catalog();
    site.restrict_tools(&mut listed);
    assert_eq!(
        listed.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        ["site_deploy", "call"]
    );
    assert_eq!(
        site.bind("site_deploy", &json!({"rev":"main"})).unwrap(),
        json!({"instance":"blog","rev":"main"})
    );
    assert!(
        site.bind("site_deploy", &json!({"instance":"other"}))
            .is_err()
    );
    assert!(
        site.bind("storage_get_object", &json!({"key":"note"}))
            .is_err()
    );
    assert!(site.bind("land", &json!({})).is_err());
    assert!(site.bind("call", &json!({"tool":"land"})).is_err());

    let archive = Scope::new(&archive).unwrap();
    let mut listed = catalog();
    archive.restrict_tools(&mut listed);
    assert_eq!(
        listed.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        ["storage_get_object", "storage_list_objects", "call"]
    );
    assert_eq!(listed[0].input_schema["required"], json!(["key"]));
    assert_eq!(listed[0].input_schema["properties"], json!({"key":{}}));
    assert_eq!(
        archive
            .bind("storage_get_object", &json!({"key":"note"}))
            .unwrap(),
        json!({"instance":"archive","key":"note"})
    );
    assert_eq!(
        archive
            .bind("call", &json!({"tool":"storage_list_objects"}))
            .unwrap(),
        json!({"tool":"storage_list_objects","args":{"instance":"archive"}})
    );
    assert!(
        archive
            .bind("call", &json!({"tool":"site_deploy"}))
            .is_err()
    );
    assert!(
        archive
            .bind("call", &json!({"tool":"call","args":{"tool":"land"}}))
            .is_err()
    );
    for args in [
        json!({"key":"note","instance":"blog"}),
        json!({"key":"note","account":"elsewhere"}),
        json!({"key":"note","bucket":"site"}),
        json!({"key":"note","presign":true}),
    ] {
        assert!(archive.bind("storage_get_object", &args).is_err());
        assert!(
            archive
                .bind("call", &json!({"tool":"storage_get_object","args":args}))
                .is_err()
        );
    }
}

#[test]
fn sites_release_identity_changes_with_theme_and_manifest_at_the_same_commit() {
    use pigeon_core::{r2, site_publish};
    let mut release = json!({"commit":"same", "template_version":"1.0.0", "renderer_digest": pigeon_blog::renderer_digest()});
    let manifest = r2::manifest_of(&[("/index.html".into(), b"page".to_vec())]);
    let original = site_publish::release_id(&release, &manifest);
    assert!(site_publish::is_release_id(&original));
    assert_eq!(site_publish::release_id(&release, &manifest), original);
    release["template_version"] = json!("1.1.0");
    let upgraded = site_publish::release_id(&release, &manifest);
    assert_ne!(upgraded, original);
    assert_ne!(r2::manifest_key(&upgraded), r2::manifest_key(&original));
    assert_ne!(
        upgraded,
        site_publish::release_id(&release, &r2::Manifest::new())
    );
    assert_eq!(
        r2::manifest_key("legacy-commit"),
        "deploys/legacy-commit.json"
    );
}

#[test]
fn instance_storage_binds_keys_and_cursors_to_the_approved_scope() {
    use pigeon_capabilities::storage::scope::Profile;
    use pigeon_core::s3::{Listing, object_path};

    let profile: Profile = serde_json::from_value(json!({
        "state": "enabled",
        "store": "scaleway:archive",
        "bucket": "archive",
        "prefix": "published/"
    }))
    .unwrap();
    let scope = profile.scope().unwrap();
    assert_eq!(scope.key("report.txt").unwrap(), "published/report.txt");
    assert!(scope.key("../private.txt").is_err());
    let literal = scope.key("%2e%2e/private.txt").unwrap();
    assert_eq!(
        object_path(scope.bucket, &literal),
        "/archive/published/%252e%252e/private.txt"
    );
    let endpoint = "https://s3.fr-par.scw.cloud";
    let prefix = scope.listing_prefix("reports/").unwrap();
    let page = scope.page("one", endpoint, "fr-par", &prefix, Some("/"));
    let cursor = page.encode(b"fake secret", "provider-cursor");
    assert_eq!(
        page.decode(b"fake secret", &cursor).unwrap(),
        "provider-cursor"
    );
    assert!(page.decode(b"fake secret", "provider-cursor").is_err());
    assert!(
        scope
            .page("two", endpoint, "fr-par", &prefix, Some("/"))
            .decode(b"fake secret", &cursor)
            .is_err()
    );
    assert!(
        scope
            .page("one", endpoint, "fr-par", "published/other/", Some("/"))
            .decode(b"fake secret", &cursor)
            .is_err()
    );

    let mut result = Listing {
        objects: vec![],
        prefixes: vec![
            "published/reports/2026/".into(),
            "published-other/".into(),
            "private/".into(),
        ],
        next: None,
    };
    scope.filter_listing(&prefix, &mut result);
    assert_eq!(result.prefixes, ["reports/2026/"]);
}

#[test]
fn instance_source_requires_an_approved_binding_for_direct_and_generic_calls() {
    use pigeon_capabilities::{
        instances::{record::Instance, tools::Scope},
        source::read::{FILE, RESOLVE, approved_repo},
    };
    use pigeon_core::mcp::Tool;

    let mut record = json!({
        "id": "project",
        "title": "Project",
        "account": {"kind": "home"},
        "repo": {"kind": "github", "full_name": "owner/project"}
    });
    let parse = |record: &Value| serde_json::from_value::<Instance>(record.clone()).unwrap();
    let scope = Scope::new(&parse(&record)).unwrap();
    assert!(scope.bind(RESOLVE, &json!({})).is_err());
    record["source"] = json!({"state":"enabled"});
    let instance = parse(&record);
    assert!(approved_repo(&instance).is_ok());
    let scope = Scope::new(&instance).unwrap();
    let mut tools: Vec<Tool> = [FILE, RESOLVE, "source_token", "site_deploy", "call"]
        .into_iter()
        .map(|name| Tool {
            name,
            description: "",
            input_schema: json!({
                "properties":{"instance": {}, "revision": {}},
                "required":["instance"]
            }),
        })
        .collect();
    scope.restrict_tools(&mut tools);
    assert_eq!(
        tools.iter().map(|tool| tool.name).collect::<Vec<_>>(),
        [FILE, RESOLVE, "call"]
    );
    assert_eq!(tools[0].input_schema["required"], json!([]));
    assert_eq!(
        scope.bind(FILE, &json!({"path":"README.md"})).unwrap(),
        json!({"instance":"project","path":"README.md"})
    );
    assert_eq!(
        scope.bind("call", &json!({"tool": RESOLVE})).unwrap(),
        json!({"tool":RESOLVE,"args":{"instance":"project"}})
    );
    for args in [
        json!({"instance":"other"}),
        json!({"repo":"owner/other"}),
        json!({"account":"other"}),
        json!({"provider":"artifacts"}),
    ] {
        assert!(scope.bind(RESOLVE, &args).is_err());
        assert!(
            scope
                .bind("call", &json!({"tool":RESOLVE,"args":args}))
                .is_err()
        );
    }
    assert!(scope.bind("call", &json!({"tool":"source_token"})).is_err());
    record["repo"] = Value::Null;
    assert!(approved_repo(&parse(&record)).is_err());
    record["repo"] = json!({"kind":"artifacts","namespace":"owner","repo":"project"});
    record["account"] = json!({"kind":"linked"});
    assert!(approved_repo(&parse(&record)).is_err());
}

#[test]
fn repository_read_arguments_are_safe_for_provider_requests() {
    use pigeon_core::repository::{file_path, revision};
    for value in [
        "HEAD",
        "feature/landing",
        "refs/heads/feature/landing",
        "0123456789abcdef0123456789abcdef01234567",
    ] {
        assert_eq!(revision(value).unwrap(), value);
    }
    for value in [
        "../other",
        "main?ref=x",
        "refs/tags/one",
        "main@{1}",
        "refs/heads",
        "",
    ] {
        assert!(revision(value).is_err());
    }
    for value in [
        "/README.md",
        "../README.md",
        "src/../README.md",
        "src\\README.md",
        "src//README.md",
        "src/",
    ] {
        assert!(file_path(value).is_err());
    }
    assert_eq!(
        file_path("docs/Notes à lire.md").unwrap(),
        "docs/Notes à lire.md"
    );
}

#[test]
fn ci_instance_scope_fails_closed_on_ambiguous_jobs_and_foreign_owners() {
    use pigeon_capabilities::ci::{ScopeRequest, resolve_scope};
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    let instance = |id: &str| -> Instance {
        serde_json::from_value(json!({
            "id":id,"title":"CI","account":{"kind":"home"},
            "repo":{"kind":"artifacts","namespace":"example","repo":"project"},
            "ci":{"state":"enabled","jobs":["test"]}
        }))
        .unwrap()
    };
    let first = instance("one");
    let request = ScopeRequest::Attribute {
        repo: "example/project".into(),
        job: "test".into(),
    };
    let resolved = resolve_scope(std::slice::from_ref(&first), &request);
    let job_scope: pigeon_ci::scope::Scope = serde_json::from_value(resolved).unwrap();
    assert!(job_scope.allows(Some("one"), "example/project", "test"));
    assert!(!job_scope.allows(Some("two"), "example/project", "test"));
    assert!(!job_scope.allows(None, "example/project", "test"));
    assert!(!job_scope.allows(Some("one"), "example/project", "deploy"));
    assert!(resolve_scope(&[first.clone(), instance("two")], &request).is_null());
    let scope = Scope::new(&first).unwrap();
    assert_eq!(
        scope.bind("ci_status", &json!({"id":"job"})).unwrap(),
        json!({"instance":"one","id":"job"})
    );
    assert!(scope.bind("ci_jobs", &json!({"repo":"other"})).is_err());
    assert!(
        scope
            .bind("call", &json!({"tool":"ci_cancel","args":{"id":"job"}}))
            .is_err()
    );
}

#[test]
fn instance_site_sessions_cannot_follow_a_reassigned_hostname() {
    use pigeon_core::site_auth::{binding, issue, session_key, verify};
    let host = "blog.example.test";
    let secret = b"fixture";
    let now = 1_000;
    let old = session_key(secret, &binding("first", "blog", host));
    let cookie = issue(&old, host, "visitor", now);
    assert!(verify(&old, host, &cookie, now).is_some());
    for (instance, site) in [("second", "blog"), ("first", "different")] {
        let key = session_key(secret, &binding(instance, site, host));
        assert!(verify(&key, host, &cookie, now).is_none());
    }
    assert!(verify(&session_key(secret, host), host, &cookie, now).is_none());
}

#[test]
fn instance_worker_metrics_ignore_other_workloads_even_if_returned_by_the_provider() {
    use pigeon_capabilities::observability::instance::{query, worker_result};
    let rows = |foreign_requests: u32| {
        json!({
            "data":{"viewer":{"accounts":[{"workersInvocationsAdaptive":[
                {"sum":{"requests":1},"dimensions":{"scriptName":"allowed","status":"ok"}},
                {"sum":{"requests":foreign_requests},"dimensions":{"scriptName":"foreign","status":"ok"}}
            ]}]}}
        })
    };
    assert_eq!(
        query("account", "allowed", "from", "to")["variables"]["script"],
        "allowed"
    );
    let result = worker_result(&rows(50), "allowed").unwrap();
    assert_eq!(result, worker_result(&rows(5_000), "allowed").unwrap());
    assert_eq!(result["metrics"]["requests"].as_f64(), Some(1.0));
    assert!(worker_result(&json!({}), "allowed").is_err());
}

#[test]
fn instances_bind_background_publishing_and_ingest_to_approved_targets() {
    use pigeon_capabilities::github::{allows_queued_publish, instance_for, publishing_binding};
    use pigeon_capabilities::ingest::{Profile, check_job_binding, hosts};
    use pigeon_capabilities::instances::record::{AccountRef, Instance};
    use pigeon_capabilities::storage::Account;

    let record: Instance = serde_json::from_value(json!({
        "id": "blog", "title": "Blog", "version": 4,
        "account": {"kind":"linked", "account_id":"fictional-account"},
        "repo": {"kind":"github", "full_name":"willow/blog", "installation":12},
        "sites":[{"name":"primary","bucket":"blog-site"}],
        "publishing":{"state":"enabled","site":"primary"},
        "ingest":{"state":"enabled","source":"scaleway:archive","bucket":"archive",
                  "prefix":"approved/","destination_bucket":"blog-data"}
    }))
    .unwrap();
    let queued = json!({
        "commit": "a".repeat(40), "repo": "Willow/Blog", "installation":12,
        "binding": publishing_binding(&record).unwrap()
    });
    assert!(allows_queued_publish(&record, &queued));
    assert!(!allows_queued_publish(
        &record,
        &json!({"commit":"a".repeat(40)})
    ));
    assert!(instance_for(&[record.clone(), record.clone()], "willow/blog", 12).is_none());
    let mut changed = record.clone();
    changed.account = AccountRef::Linked {
        account_id: Some("another-account".into()),
    };
    assert!(!allows_queued_publish(&changed, &queued));
    changed = record.clone();
    changed.publishing = Some(pigeon_core::site_publish::Profile::Disabled);
    assert!(publishing_binding(&changed).is_none());
    assert!(!allows_queued_publish(&changed, &queued));

    let profile = record.ingest.as_ref().unwrap();
    let args = json!({});
    let binding = profile.bind(&args).unwrap();
    assert_eq!(binding.destination_bucket, "blog-data");
    for args in [
        json!({"source":"scaleway:other"}),
        json!({"bucket":"private"}),
        json!({"prefix":""}),
    ] {
        assert!(profile.bind(&args).is_err());
    }
    assert!(Profile::Disabled.bind(&args).is_err());
    let mut source = Account::scaleway("archive", "fr-par");
    let mut job = hosts::job_spec(&hosts::JobInputs {
        instance: &record.id,
        account: &source,
        bucket: binding.bucket,
        prefix: binding.prefix,
        dest_bucket: binding.destination_bucket,
        account_id: "fictional-account",
        dry_run: true,
        keep_archives: true,
        fingerprints: false,
        commit: &"a".repeat(40),
        day: "20261125",
        nonce: 0,
    });
    assert!(check_job_binding(&job, "fictional-account", "blog-data", &source).is_ok());
    assert!(check_job_binding(&job, "new-account", "blog-data", &source).is_err());
    assert!(check_job_binding(&job, "fictional-account", "new-bucket", &source).is_err());
    job.dest.bucket = "another-bucket".into();
    assert!(check_job_binding(&job, "fictional-account", "blog-data", &source).is_err());
    job.dest.bucket = "blog-data".into();
    source.buckets = vec!["different-source".into()];
    assert!(check_job_binding(&job, "fictional-account", "blog-data", &source).is_err());
}

#[test]
fn worker_context_verifies_recipient_instance_and_legacy_compatibility() {
    use pigeon_contract::{Refused, WorkerContext, sign, sign_with_context, verify, verify_for};

    let key = b"fixture worker key";
    let caller = json!({"user":{"id":"fixture"}});
    let body = br#"{"args":{"instance":"sample"}}"#;
    let now = 10_000;
    let context = WorkerContext::new("pigeon-sites", Some("sample"));
    let header = sign_with_context(key, &caller, "call", body, now, "fixture-1", &context);
    let verified = verify_for(key, Some(&header), "call", body, now, "pigeon-sites").unwrap();
    assert_eq!(verified.context, Some(context));
    assert!(verified.require_instance("sample").is_ok());
    assert_eq!(
        verified.require_instance("other"),
        Err(Refused::InstanceMismatch)
    );
    assert_eq!(
        verified.check_instance(None),
        Err(Refused::InstanceMismatch)
    );
    assert_eq!(
        verify_for(key, Some(&header), "call", body, now, "pigeon-ci-gateway"),
        Err(Refused::WrongRecipient),
    );
    assert!(verify_for(key, Some(&header), "call", b"{}", now, "pigeon-sites").is_err());
    assert!(
        verify_for(
            key,
            Some(&header),
            "call",
            body,
            now + 600_000,
            "pigeon-sites"
        )
        .is_err()
    );
    assert!(verify(key, Some(&header), "call", body, now).is_err());

    let old = sign(key, &caller, "call", body, now, "fixture-2");
    let legacy = verify_for(key, Some(&old), "call", body, now, "pigeon-sites").unwrap();
    assert!(legacy.context.is_none());
    assert!(legacy.require_instance("sample").is_err());
}

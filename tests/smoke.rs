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

#[test]
fn models_openai_request_translation() {
    use pigeon_models::openai;
    let req = json!({
        "model": "gpt-6.2-sol",
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
        "output_config": { "effort": "high" },
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
    assert_eq!(out["model"], "gpt-6.2-sol");
    assert_eq!(out["max_completion_tokens"], 500);
    assert!(out.get("max_tokens").is_none());
    assert_eq!(out["stream"], true);
    assert_eq!(out["stream_options"], json!({ "include_usage": true }));
    assert_eq!(out["reasoning_effort"], "high");
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
    assert!(odd.get("reasoning_effort").is_none());
    assert_eq!(odd["messages"][0]["content"], "s");
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
    use pigeon_models::{Ask, Dialect, turn};
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
    )
    .unwrap();
    assert_eq!(t.model, "gpt-6.2-sol");
    let betas = turn::turn(
        &json!({ "messages": msgs }),
        &json!(["compact-2026-01-12"]),
        "gpt-6.2-sol",
        Dialect::OpenAi,
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
    use pigeon_models::{Billing, Dialect, Models, Provider};
    // The declaration is in the wasm-only Worker crate: read it as text, then build the same.
    let src = include_str!("../src/model-gateway/src/lib.rs");
    for needle in [
        "slug: \"openai\"",
        "dialect: Dialect::OpenAi",
        "default_model: \"gpt-6.2-sol\"",
        "An OpenAI API key, from the OpenAI platform's API keys page.",
    ] {
        assert!(src.contains(needle), "{needle}");
    }
    assert!(src.find("slug: \"anthropic\"") < src.find("slug: \"openai\""));
    let provider = |slug, dialect, billing| Provider {
        slug,
        alias: "default",
        dialect,
        default_model: "m",
        key_help: "help",
        billing,
    };
    let models = Models {
        gateway: "pigeon".into(),
        providers: vec![
            provider(
                "anthropic",
                Dialect::Anthropic,
                Some(Billing::AnthropicAdmin),
            ),
            provider("openai", Dialect::OpenAi, None),
        ],
        secrets_page: "https://x/secrets".into(),
        host: "h".into(),
        proxy_host: "p".into(),
    };
    let slots = models.secrets();
    let open = slots
        .iter()
        .find(|s| s.name == "pigeon_openai_default")
        .unwrap();
    assert_eq!(
        open.scopes,
        vec![pigeon_core::ai_gateway::SECRET_SCOPE.to_string()]
    );
    assert_eq!(slots[0].name, "pigeon_anthropic_default");
    let tools = models.tools();
    for name in ["models_run", "models_submit"] {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        assert_eq!(
            t.input_schema["properties"]["provider"]["enum"],
            json!(["anthropic", "openai"])
        );
    }
}

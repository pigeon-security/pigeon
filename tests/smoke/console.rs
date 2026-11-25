//! The console's one write path (`console::write`), its one read path (`/app/op`'s allowlist),
//! the shell's contract with both and with `console_manifest` (P15), and the guard that the
//! first console is gone for good (`console_decommissioned`).

use super::*;

use pigeon_core::oauth::hash;

use pigeon_capabilities::console::ops::{INSTANCE_OPS, READS, allowed, inbox_for, read_keys};
use pigeon_capabilities::console::write::{
    ACTIONS, Handler, Policy, Step, Stored, action, digest, purpose, redeem, route,
};

/// The purpose `caller` gets for `id` with `target` and `change` at `step`, or why not.
fn bound_purpose(
    id: &str,
    caller: &str,
    target: Value,
    change: Value,
    step: Step,
) -> Result<String, String> {
    let a = action(id).unwrap();
    let target = a.target(&target)?;
    let change = a.change(&change, step)?;
    Ok(purpose(
        a,
        &digest(a, caller, &target, &a.bound(&change), None),
    ))
}

#[test]
fn console_write_actions_each_have_a_handler_and_policy() {
    const OWNERS: &[&str] = &[
        "secrets",
        "proxy",
        "linked",
        "instances",
        "console",
        "apple",
    ];
    let mut ids = std::collections::BTreeSet::new();
    for a in ACTIONS {
        assert!(ids.insert(a.id), "{} twice", a.id);
        assert!(!a.title.is_empty(), "{}", a.id);
        match a.handler {
            Handler::Capability(name) => {
                assert!(OWNERS.contains(&name), "{}: {name}", a.id);
                assert_ne!(a.policy, Policy::ToolGate, "{}", a.id);
            }
            Handler::Tool(tool) => {
                assert!(!tool.is_empty());
                assert_eq!(
                    a.policy,
                    Policy::ToolGate,
                    "{} goes through MCP's gate",
                    a.id
                );
            }
        }
        for (step, word) in [(Step::Begin, "begin"), (Step::Finish, "finish")] {
            let path = format!("/console/write/{}/{word}", a.id);
            assert_eq!(route(&path), Some((a, step)), "{path}");
            assert_eq!(
                pigeon_capabilities::console::Post::route(&path),
                Some(pigeon_capabilities::console::Post::Write)
            );
        }
    }
    for gone in [
        "/console/write/nope/begin",
        "/console/write/secret-set/go",
        "/console/write/secret-set",
    ] {
        assert_eq!(route(gone), None, "{gone}");
        assert_eq!(
            pigeon_capabilities::console::Post::route(gone),
            None,
            "{gone}"
        );
    }
    // Every write the redesign names.
    for id in [
        "secret-set",
        "token-issue",
        "token-revoke",
        "linked-unlink",
        "instance-put",
        "visitor-invite",
        "visitor-revoke",
        "approval-approve",
        "approval-deny",
        "land-approve",
        "model-defaults",
        "agents-config",
        "site-rollback",
        "site-deploy",
        "apple-adopt",
        "apple-identity",
    ] {
        assert!(action(id).is_some(), "{id}");
    }
    // Rollback and deploy run the sites tools through the same policy gate as MCP.
    assert_eq!(
        action("site-rollback").unwrap().handler,
        Handler::Tool("site_rollback")
    );
    assert_eq!(
        action("site-deploy").unwrap().handler,
        Handler::Tool("site_deploy")
    );
    assert_eq!(
        action("model-defaults").unwrap().handler,
        Handler::Tool("ai_agents_models")
    );
    assert_eq!(action("approval-approve").unwrap().policy, Policy::Approval);
    // Destructive: the dialog's danger button.
    let destructive: Vec<&str> = ACTIONS
        .iter()
        .filter(|a| a.destructive)
        .map(|a| a.id)
        .collect();
    for id in [
        "token-revoke",
        "linked-unlink",
        "site-rollback",
        "visitor-revoke",
        "passkey-remove",
        "app-disconnect",
        "run-stop",
    ] {
        assert!(destructive.contains(&id), "{id} is destructive");
    }
    for id in [
        "secret-set",
        "token-issue",
        "instance-put",
        "approval-approve",
        "site-deploy",
    ] {
        assert!(!destructive.contains(&id), "{id} is not destructive");
    }
    // Only what the iOS app makes may come from it by bearer: setting a secret, deciding
    // approvals and approving a landing.
    let bearer: Vec<&str> = ACTIONS
        .iter()
        .filter(|a| a.by_bearer)
        .map(|a| a.id)
        .collect();
    assert_eq!(
        bearer,
        [
            "secret-set",
            "land-approve",
            "approval-approve",
            "approval-deny"
        ]
    );
}

/// The iOS app's `secret-set` by bearer: `/console/write/secret-set/{begin,finish}` with
/// `{target, change: {digest | value}}` binds begin and finish to one purpose for the person the
/// app acts for, and to nobody else's passkey or another value; writes outside the bearer
/// allowance are refused by bearer at their own route.
#[test]
fn console_bearer_secret_set_binds_the_person_and_value() {
    let (a, step) = route("/console/write/secret-set/begin").unwrap();
    assert_eq!((a.id, step), ("secret-set", Step::Begin));
    assert!(a.by_bearer);
    let (a, step) = route("/console/write/secret-set/finish").unwrap();
    assert_eq!((a.id, step), ("secret-set", Step::Finish));

    let begin = |caller: &str, value: &str| {
        bound_purpose(
            "secret-set",
            caller,
            json!("X"),
            json!({ "digest": hash(value) }),
            Step::Begin,
        )
        .unwrap()
    };
    let finish = |caller: &str, value: &str| {
        bound_purpose(
            "secret-set",
            caller,
            json!("X"),
            json!({ "value": value }),
            Step::Finish,
        )
        .unwrap()
    };
    assert_eq!(begin("willow", "v"), finish("willow", "v"));
    assert!(begin("willow", "v").starts_with("console:write:secret-set:"));
    assert_ne!(begin("willow", "v"), finish("alex", "v"), "another person");
    assert_ne!(begin("willow", "v"), finish("willow", "w"), "another value");
    assert_ne!(
        begin("willow", "v"),
        bound_purpose(
            "secret-set",
            "willow",
            json!("Y"),
            json!({ "value": "v" }),
            Step::Finish
        )
        .unwrap(),
        "another secret"
    );

    // Every other write keeps needing the console's own session.
    for id in [
        "token-issue",
        "token-revoke",
        "passkey-remove",
        "app-disconnect",
        "instance-put",
        "land-approval",
    ] {
        let (a, _) = route(&format!("/console/write/{id}/begin")).unwrap();
        assert!(!a.by_bearer, "{id} is not allowed by bearer");
    }
}

#[test]
fn console_write_binds_the_exact_change_caller_and_action() {
    let release = format!("release-{}", "a".repeat(43));
    let other = format!("release-{}", "b".repeat(43));
    let p = |caller: &str, target: &str, rel: &str| {
        bound_purpose(
            "site-rollback",
            caller,
            json!(target),
            json!({ "release": rel }),
            Step::Finish,
        )
        .unwrap()
    };
    let begun = bound_purpose(
        "site-rollback",
        "u1",
        json!(" blog "),
        json!({ "release": release }),
        Step::Begin,
    )
    .unwrap();
    assert_eq!(
        begun,
        p("u1", "blog", &release),
        "finish with the same body"
    );
    assert_ne!(
        begun,
        p("u1", "blog", &other),
        "a changed change is refused"
    );
    assert_ne!(
        begun,
        p("u1", "docs", &release),
        "a changed target is refused"
    );
    assert_ne!(
        begun,
        p("u2", "blog", &release),
        "another caller is refused"
    );
    // Another action with the same target and change does not share it.
    let stop = bound_purpose("run-stop", "u1", json!("r1"), json!({}), Step::Begin).unwrap();
    let note = bound_purpose(
        "run-note",
        "u1",
        json!("r1"),
        json!({ "text": "hi" }),
        Step::Begin,
    )
    .unwrap();
    assert_ne!(stop, note);

    // A secret's value is bound by its digest and sent once, at finish; never in the purpose.
    let value = "ghp_new";
    let by_digest = bound_purpose(
        "secret-set",
        "u1",
        json!("GITHUB_MIRROR_TOKEN"),
        json!({ "digest": hash(value) }),
        Step::Begin,
    )
    .unwrap();
    let by_value = bound_purpose(
        "secret-set",
        "u1",
        json!("GITHUB_MIRROR_TOKEN"),
        json!({ "value": format!(" {value} ") }),
        Step::Finish,
    )
    .unwrap();
    assert_eq!(by_digest, by_value);
    assert!(!by_value.contains(value));
    let other_value = bound_purpose(
        "secret-set",
        "u1",
        json!("GITHUB_MIRROR_TOKEN"),
        json!({ "value": "ghp_other" }),
        Step::Finish,
    )
    .unwrap();
    assert_ne!(by_digest, other_value);
    assert!(
        bound_purpose(
            "secret-set",
            "u1",
            json!("X"),
            json!({ "digest": hash(value) }),
            Step::Finish
        )
        .is_err(),
        "finish needs the value"
    );

    // An instance record is bound by its digest; an edited one is another change.
    let record = r#"{"id":"blog","version":3}"#;
    let a = bound_purpose(
        "instance-put",
        "u1",
        json!("blog"),
        json!({ "record": record, "digest": hash(record) }),
        Step::Begin,
    )
    .unwrap();
    assert_eq!(
        a,
        bound_purpose(
            "instance-put",
            "u1",
            json!("blog"),
            json!({ "record": record }),
            Step::Finish
        )
        .unwrap()
    );
    assert_ne!(
        a,
        bound_purpose(
            "instance-put",
            "u1",
            json!("blog"),
            json!({ "record": r#"{"id":"blog","version":4}"# }),
            Step::Finish
        )
        .unwrap()
    );
    assert!(
        bound_purpose(
            "instance-put",
            "u1",
            json!("blog"),
            json!({ "record": record, "digest": "x" }),
            Step::Begin
        )
        .is_err()
    );

    // Approving binds the approval's own digest too: a different call under the same id differs.
    let ap = action("approval-approve").unwrap();
    let t = json!("a1");
    assert_ne!(
        digest(ap, "u1", &t, &json!({}), Some("console:approval:a1:d1")),
        digest(ap, "u1", &t, &json!({}), Some("console:approval:a1:d2"))
    );
}

#[test]
fn console_write_checks_targets_and_changes() {
    let check = |id: &str, target: Value, change: Value| {
        let a = action(id).unwrap();
        a.target(&target)
            .and_then(|_| a.change(&change, Step::Begin))
    };
    assert!(check("run-stop", json!("r1"), json!({})).is_ok());
    assert!(
        check("run-stop", json!("r1"), json!({ "all": true })).is_err(),
        "no other field"
    );
    assert!(check("run-stop", json!("r/1"), json!({})).is_err());
    assert!(check("site-rollback", json!("blog"), json!({ "release": "main" })).is_err());
    assert!(
        check("site-deploy", json!("blog"), json!({})).is_ok(),
        "main by default"
    );
    assert!(check("site-deploy", json!("blog"), json!({ "rev": "a b" })).is_err());
    assert!(
        check(
            "model-defaults",
            Value::Null,
            json!({ "set": { "executor": "cf-openai" } })
        )
        .is_ok()
    );
    assert!(
        check(
            "model-defaults",
            json!("models"),
            json!({ "set": { "root": "x" } })
        )
        .is_err()
    );
    assert!(
        check(
            "model-defaults",
            json!("other"),
            json!({ "set": { "executor": "x" } })
        )
        .is_err()
    );
    assert!(check("model-defaults", Value::Null, json!({ "set": {} })).is_err());
    assert!(check("agents-config", Value::Null, json!({ "max_live_runs": 8 })).is_ok());
    assert!(check("agents-config", Value::Null, json!({ "max_live_runs": -1 })).is_err());
    assert!(
        check("agents-config", Value::Null, json!({})).is_err(),
        "it sets something"
    );
    assert!(check("token-issue", json!(" cloud env "), json!({})).is_ok());
    assert!(check("token-issue", json!("x"), json!({ "days": 91 })).is_err());
    assert!(check("token-revoke", json!("short"), json!({})).is_err());
    assert!(check("land-approval", json!("yes"), json!({})).is_err());
    assert!(check("invite", json!("a\nb"), json!({})).is_err());
    let site = json!({ "instance": "alpha", "site": "blog", "host": "blog.example.test" });
    assert!(check("visitor-invite", site.clone(), json!({ "name": "Alex" })).is_ok());
    assert!(
        check(
            "visitor-invite",
            json!({ "instance": "alpha" }),
            json!({ "name": "Alex" })
        )
        .is_err()
    );
    assert!(check("visitor-revoke", site, json!({ "user": "u/1" })).is_err());

    // A tool write's arguments: the target is its instance or run.
    let rel = format!("release-{}", "a".repeat(43));
    let a = action("site-rollback").unwrap();
    assert_eq!(
        a.args(&json!("blog"), &json!({ "release": rel })),
        json!({ "instance": "blog", "release": rel })
    );
    let a = action("run-note").unwrap();
    assert_eq!(
        a.args(&json!("r1"), &json!({ "text": "hi" })),
        json!({ "run": "r1", "text": "hi" })
    );

    // The summary says what changes, never a secret's value.
    let a = action("secret-set").unwrap();
    let s = a.summary(&json!("X"), &a.bound(&json!({ "value": "ghp_new" })));
    assert!(s.change.iter().all(|l| !l.contains("ghp_new")));
    assert!(!s.destructive && s.target == "X");
    let a = action("site-rollback").unwrap();
    let s = a.summary(&json!("blog"), &json!({ "release": rel }));
    assert!(s.destructive && s.change == [format!("release: {rel}")]);
}

#[test]
fn console_write_challenges_are_single_use_and_expire() {
    let kept = || {
        Some(Stored {
            purpose: "console:write:run-stop:d".into(),
            challenge: "c".into(),
            expires_ms: 1_000,
        })
    };
    let mut s = kept();
    assert_eq!(
        redeem(&mut s, "console:write:run-stop:d", 999),
        Ok("c".into())
    );
    assert!(
        redeem(&mut s, "console:write:run-stop:d", 999).is_err(),
        "used once"
    );
    assert!(
        redeem(&mut kept(), "console:write:run-stop:d", 1_000).is_err(),
        "expired"
    );
    let mut s = kept();
    assert!(
        redeem(&mut s, "console:write:run-stop:other", 1).is_err(),
        "another change"
    );
    assert!(s.is_none(), "and gone after a wrong answer");
    assert!(redeem(&mut None, "x", 0).is_err());
}

#[test]
fn console_ops_allow_the_reads_the_console_needs_and_nothing_else() {
    for op in [
        "app_inbox",
        "approvals_list",
        "land_list",
        "instances_list",
        "app_linked_details",
        "capabilities_versions",
        "capabilities_upgrade_plan",
        "site_releases",
        "app_site_hosts",
        "app_site_visitors",
        "storage_list_objects",
        "source_read_file",
        "ci_jobs",
        "observability_worker_metrics",
        "ingest_report",
        "ai_agents_status",
        "ai_agents_compare",
        "cost_daily",
        "secrets_list",
        "credentials_expiry",
        "app_proxy_tokens",
        "app_people",
        "tokens_status",
        "users_people",
        "oauth_clients",
        "architecture_describe",
        "ai_agents_models",
        "ai_agents_config",
    ] {
        assert!(read_keys(op).is_some(), "{op}");
        assert!(allowed(op, &json!({})), "{op}");
    }
    assert!(allowed(
        "site_releases",
        &json!({ "instance": "blog", "limit": 5 })
    ));
    assert!(allowed("app_inbox", &json!({ "instance": "blog" })));
    assert_eq!(read_keys("console_manifest"), Some(&["instance"][..]));
    // The instance areas' reads take the instance, and are bound to its record when they do.
    for (op, args) in [
        (
            "storage_list_objects",
            json!({ "instance": "blog", "prefix": "img/", "delimiter": "/", "max_keys": 100, "token": "t" }),
        ),
        (
            "ci_jobs",
            json!({ "instance": "blog", "state": "failed", "limit": 50 }),
        ),
        ("ci_status", json!({ "instance": "blog", "id": "j1" })),
        (
            "observability_worker_metrics",
            json!({ "instance": "blog", "hours": 24 }),
        ),
        ("ingest_report", json!({ "instance": "blog", "run": "r1" })),
        ("app_ingest_runs", json!({ "instance": "blog" })),
    ] {
        assert!(allowed(op, &args), "{op} {args}");
    }
    assert!(
        !allowed(
            "app_ingest_runs",
            &json!({ "instance": "blog", "run": "r1" })
        ),
        "listing runs never names one"
    );
    for op in INSTANCE_OPS {
        assert!(
            read_keys(op).is_some_and(|keys| keys.contains(&"instance")),
            "{op}"
        );
    }
    // A bound storage read cannot also name a raw account or bucket.
    let record: pigeon_capabilities::instances::record::Instance = serde_json::from_value(json!({
        "id": "blog", "title": "Blog", "account": { "kind": "home" },
        "storage": { "state": "enabled", "store": "scaleway:archive", "bucket": "blog-assets" },
    }))
    .unwrap();
    let bind = |tool: &str, args: Value| {
        pigeon_capabilities::instances::tools::bind_global("blog", Some(&record), tool, &args)
    };
    assert_eq!(
        bind(
            "storage_list_objects",
            json!({ "instance": "blog", "prefix": "img/" })
        ),
        Ok(json!({ "instance": "blog", "prefix": "img/" }))
    );
    assert!(
        bind(
            "storage_list_objects",
            json!({ "instance": "blog", "bucket": "other" })
        )
        .is_err()
    );
    assert!(
        bind("ci_jobs", json!({ "instance": "blog" })).is_err(),
        "CI is off"
    );
    // site_releases is instance-targeted like deploy and rollback, so a global call is bound to
    // the approved record and signed with its instance context.
    assert!(
        pigeon_capabilities::instances::tools::global_target(
            "sites",
            "site_releases",
            &json!({ "instance": "blog" })
        )
        .unwrap()
        .is_some()
    );
    // Unlisted ops, and listed ones with arguments that would write, are refused.
    for (op, args) in [
        ("site_rollback", json!({ "instance": "blog" })),
        ("secrets_request", json!({})),
        ("ingest_status", json!({ "instance": "blog" })),
        ("ai_agents_models", json!({ "set": { "executor": "x" } })),
        ("ai_agents_config", json!({ "max_live_runs": 1 })),
        (
            "storage_get_object",
            json!({ "account": "a", "bucket": "b", "key": "k", "presign": true }),
        ),
        ("cost_daily", json!({ "range": "7d", "extra": 1 })),
        ("cost_daily", json!([])),
        // A run's stop, note and extension are writes now (run-stop, run-note, run-extend).
        ("ai_agents_stop", json!({ "run": "r1" })),
        ("ai_agents_note", json!({ "run": "r1", "text": "hi" })),
        ("ai_agents_extend", json!({ "run": "r1", "turns": 5 })),
        ("app_people", json!({ "user_id": "u1" })),
    ] {
        assert!(!allowed(op, &args), "{op} {args}");
    }
    let mut names: Vec<&str> = READS.iter().map(|(n, _)| *n).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), READS.len(), "each read once");

    // The inbox for one instance: its calls, drafts and plans; landings only on the home one.
    let listing = json!({
        "calls": [{ "id": "c1", "args": { "instance": "blog" } }, { "id": "c2", "args": {} }],
        "drafts": [{ "id": "d1", "instance": "blog" }, { "id": "d2", "instance": "docs" }],
        "plans": [], "other": [{ "id": "o1", "summary": { "instance": "blog" } }],
        "landings": [{ "id": "l1" }],
    });
    let ids = |v: &Value, list: &str| -> Vec<String> {
        v[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let blog = inbox_for(listing.clone(), "blog", false);
    assert_eq!(ids(&blog, "calls"), ["c1"]);
    assert_eq!(ids(&blog, "drafts"), ["d1"]);
    assert_eq!(ids(&blog, "other"), ["o1"]);
    assert!(ids(&blog, "landings").is_empty());
    assert_eq!(ids(&inbox_for(listing, "home", true), "landings"), ["l1"]);
}

// ---- P15: the new console shell's contract (`console/web/shell.js`, `views.js`) ----

/// Every area and view `console_manifest` can send: its answer for a home instance with every
/// instance capability enabled, given every platform capability's sections (core's own, and
/// the AI Agents Worker's, which core reads from its manifest as `remote.rs` does).
fn console_every_view() -> std::collections::BTreeMap<String, Vec<String>> {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::console::sections::{Section, area_named};
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};
    use pigeon_contract::Service;

    let record: Instance = serde_json::from_value(json!({
        "id": "all", "title": "All", "account": { "kind": "home" },
        "zones": ["example.com"], "worker": "all-worker",
        "repo": { "kind": "artifacts", "namespace": "example", "repo": "all" },
        "sites": [{ "name": "blog", "bucket": "all-site", "auth": "passkey", "targets": [] }],
        "publishing": { "state": "enabled", "site": "blog" },
        "storage": { "state": "enabled", "store": "s", "bucket": "b", "prefix": "p/" },
        "source": { "state": "enabled" },
        "ci": { "state": "enabled", "jobs": ["test"] },
        "observability": { "state": "enabled" },
        "ingest": { "state": "enabled", "source": "r2:all", "bucket": "src",
            "destination_bucket": "all-site" }
    }))
    .unwrap();
    let caps: Vec<Box<dyn Capability>> = vec![
        Box::new(pigeon_capabilities::architecture::Architecture),
        Box::new(pigeon_capabilities::users::Users { host: "h".into() }),
        Box::new(pigeon_capabilities::secrets::Secrets {
            host: "h".into(),
            parent: "p".into(),
        }),
        Box::new(pigeon_capabilities::proxy::Tokens {
            proxy_host: "p".into(),
        }),
        Box::new(pigeon_capabilities::observability::Observability {
            map: pigeon_core::lanes::Map {
                docs: vec![],
                shared: vec![],
                checks: vec![],
                core_first: None,
                workers: vec![],
            },
        }),
        Box::new(pigeon_models::Models {
            gateways: Vec::new(),
            providers: Vec::new(),
            staged: Vec::new(),
            secrets_page: "https://x/secrets".into(),
            host: "h".into(),
            proxy_host: "p".into(),
        }),
    ];
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let mut platform: Vec<(&'static str, Section)> = caps
        .iter()
        .flat_map(|c| c.sections().into_iter().map(|s| (c.name(), s)))
        .chain(
            pigeon_capabilities::instances::instances_sections()
                .into_iter()
                .map(|s| ("instances", s)),
        )
        .collect();
    for cap in pigeon_ai_agents::service::Agents.manifest().capabilities {
        for s in cap.sections.into_iter().filter(|s| s.view) {
            let area = area_named(&s.tab).unwrap().id;
            platform.push((
                "ai_agents",
                Section {
                    area,
                    id: leak(&s.id),
                    title: leak(&s.title),
                    description: "",
                    order: s.order,
                },
            ));
        }
    }
    let scope = Scope::new(&record).unwrap();
    let m = manifest(&record, &scope, &platform, &[], Some(0));
    m["areas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let views = a["views"].as_array().unwrap();
            (
                a["id"].as_str().unwrap().to_string(),
                views
                    .iter()
                    .map(|v| v["id"].as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// The double-quoted string literals in `src`, each with the text that follows it.
fn js_literals(src: &str) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(start) = rest.find('"') {
        let body = &rest[start + 1..];
        let mut end = None;
        let mut escaped = false;
        for (i, c) in body.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    end = Some(i);
                    break;
                }
                '\n' => break,
                _ => escaped = false,
            }
        }
        let Some(end) = end else {
            rest = body;
            continue;
        };
        out.push((body[..end].to_string(), &body[end + 1..]));
        rest = &body[end + 1..];
    }
    out
}

/// The literal arguments of every call of `func` in `src` (not a method of another name): the
/// first, and the second when it is a literal (`None` for `null` or anything else).
fn js_calls(src: &str, func: &str) -> Vec<(String, Option<String>)> {
    let pat = format!("{func}(");
    let mut out = Vec::new();
    for (at, _) in src.match_indices(&pat) {
        if src[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let args = src[at + pat.len()..].trim_start();
        if !args.starts_with('"') {
            continue;
        }
        let lits = js_literals(args);
        let (first, after) = &lits[0];
        let after = after.trim_start();
        let second = after
            .strip_prefix(',')
            .map(str::trim_start)
            .filter(|a| a.starts_with('"'))
            .map(|_| lits[1].0.clone());
        out.push((first.clone(), second));
    }
    out
}

/// The literals in the array `decl` declares in `src`: `const NAME = Object.freeze([...])`.
fn js_array(src: &str, decl: &str) -> Vec<String> {
    let at = src
        .find(decl)
        .unwrap_or_else(|| panic!("{decl} not in shell.js"));
    let rest = &src[at..];
    let open = rest.find('[').unwrap();
    let close = rest.find(']').unwrap();
    js_literals(&rest[open..close])
        .into_iter()
        .map(|(s, _)| s)
        .collect()
}

/// P15, the console contract: every area and view `console_manifest` can send has a renderer in
/// the shell, and the shell registers
/// none it cannot send; every op the shell reads is in `/app/op`'s `READS`; every write it makes
/// is an action of the one write path; the shell's one stylesheet keeps colours in tokens and no
/// script sets an inline style or HTML; and fixture mode is never served.
#[test]
fn console_shell_contract() {
    use pigeon_capabilities::console::sections::{AREAS, area_named};
    use pigeon_capabilities::console::{Get, page};
    use std::collections::BTreeSet;

    let scripts: Vec<(&str, &str)> = page::ASSETS
        .iter()
        .filter(|(path, ..)| path.ends_with(".js"))
        .map(|(path, _, body)| (*path, *body))
        .collect();
    let shell = scripts
        .iter()
        .find(|(path, _)| path.ends_with("/shell.js"))
        .unwrap()
        .1;
    let all: String = scripts.iter().map(|(_, body)| *body).collect();

    // What the manifest can send: every area, on a fully enabled home instance.
    let every = console_every_view();
    let ids: Vec<&str> = every.keys().map(String::as_str).collect();
    let mut stable: Vec<&str> = AREAS.iter().map(|a| a.id).collect();
    stable.sort_unstable();
    assert_eq!(
        ids, stable,
        "a fully enabled home instance shows every area"
    );

    // Renderers: register(area, view|null, render).
    let mut renderers = BTreeSet::new();
    for (area, view) in js_calls(&all, "register") {
        let key = format!("{area}/{}", view.as_deref().unwrap_or("*"));
        assert!(renderers.insert(key.clone()), "{key} has two renderers");
    }
    let rendered = |area: &str, view: Option<&str>| {
        view.is_some_and(|v| renderers.contains(&format!("{area}/{v}")))
            || renderers.contains(&format!("{area}/*"))
    };
    for (area, views) in &every {
        if views.is_empty() {
            assert!(rendered(area, None), "{area} has no renderer");
        }
        for view in views {
            assert!(rendered(area, Some(view)), "{area}/{view} has no renderer");
        }
    }
    for key in &renderers {
        let (area, view) = key.split_once('/').unwrap();
        let known = every
            .get(area)
            .is_some_and(|views| view == "*" || views.iter().any(|v| v == view));
        assert!(known, "{key} is registered but no manifest sends it");
    }

    // The recorded manifests: areas as AREAS declares them, Inbox first, platform areas only
    // on home, and only views the manifest can send.
    for (name, body) in [
        (
            "home",
            include_str!(
                "../../src/capabilities/src/console/web/fixtures/console_manifest.home.json"
            ),
        ),
        (
            "docs",
            include_str!(
                "../../src/capabilities/src/console/web/fixtures/console_manifest.docs.json"
            ),
        ),
        (
            "notes",
            include_str!(
                "../../src/capabilities/src/console/web/fixtures/console_manifest.notes.json"
            ),
        ),
    ] {
        let m: Value = serde_json::from_str(body).unwrap();
        assert_eq!(m["instance"]["id"], name);
        assert!(m["inbox"]["count"].is_u64(), "{name}: inbox.count");
        let home = m["instance"]["home"].as_bool().unwrap();
        let areas = m["areas"].as_array().unwrap();
        assert_eq!(areas[0]["id"], "inbox", "{name}: Inbox comes first");
        for a in areas {
            let id = a["id"].as_str().unwrap();
            let declared = area_named(id).unwrap_or_else(|| panic!("{name}: no area {id}"));
            assert_eq!(
                a,
                &json!({ "id": id, "title": declared.title, "group": declared.group,
                "key": declared.key, "views": a["views"] }),
                "{name}: {id}"
            );
            assert!(
                a["group"] != "platform" || home,
                "{name}: {id} is platform-wide"
            );
            for v in a["views"].as_array().unwrap() {
                let view = v["id"].as_str().unwrap();
                assert!(
                    every[id].iter().any(|w| w == view),
                    "{name}: {id}/{view} is not sent"
                );
                assert!(v["upgrade"].is_null() || v["upgrade"]["since"].is_string());
            }
        }
    }

    // Reads: what the shell declares covers every literal op call, and is in READS.
    let reads = js_array(shell, "const READS");
    for (name, _) in js_calls(&all, "op") {
        assert!(
            reads.contains(&name),
            "op {name} is not in the shell's READS"
        );
    }
    for name in &reads {
        assert!(
            read_keys(name).is_some(),
            "{name} is read by the shell but not allowed by /app/op"
        );
    }

    // Writes: every action the views name is declared, and is an action of the write path.
    let writes = js_array(shell, "const WRITES");
    for at in all
        .match_indices("action: ")
        .map(|(at, _)| at + "action: ".len())
    {
        let segment = all[at..].split([',', '}', '\n']).next().unwrap();
        for (name, _) in js_literals(segment) {
            assert!(writes.contains(&name), "write {name} is not in WRITES");
        }
    }
    for name in &writes {
        assert!(
            action(name).is_some(),
            "{name} is not a /console/write action"
        );
    }

    // P13: colours only in the stylesheet's tokens; no inline styles or HTML from scripts.
    let css = page::ASSETS
        .iter()
        .find(|(path, ..)| path.ends_with("/shell.css"))
        .unwrap()
        .2;
    let has_colour = |line: &str| {
        line.contains("rgb(")
            || line.contains("rgba(")
            || line.match_indices('#').any(|(at, _)| {
                let hex = line[at + 1..]
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .count();
                matches!(hex, 3 | 4 | 6 | 8)
                    && !line[at + 1 + hex..]
                        .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
    };
    for line in css.lines() {
        assert!(
            !has_colour(line) || line.trim_start().starts_with("--"),
            "a literal colour outside the tokens: {line}"
        );
    }
    for (path, body) in &scripts {
        for line in body.lines() {
            assert!(!has_colour(line), "{path}: a literal colour: {line}");
        }
        for banned in [
            ".style",
            "style:",
            "\"style\"",
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
        ] {
            assert!(!body.contains(banned), "{path} uses {banned}");
        }
    }

    // Served at /app and every page below it; fixture mode never.
    for path in [
        "/app",
        "/app/",
        "/app/home/inbox",
        "/app/docs/site/visitors",
    ] {
        assert_eq!(Get::route(path, None), Some(Get::App), "GET {path}");
    }
    for path in [
        "/app/fixtures.html",
        "/app/fixtures/console_manifest.json",
        "/app/views/nope.js",
        "/app/op",
    ] {
        assert_eq!(Get::route(path, None), None, "GET {path}");
    }
    let page = page::shell("Site <x>", Some("Ada"));
    assert!(!page.contains("data-fixtures"));
    assert!(page.contains("data-base=\"/app\"") && page.contains("data-person=\"Ada\""));
    assert!(page.contains("Site &lt;x&gt;") && !page.contains("<script>"));
    for (path, ..) in page::ASSETS {
        assert!(page.contains(path), "the page loads {path}");
        assert!(
            matches!(Get::route(path, None), Some(Get::Asset(..))),
            "GET {path}"
        );
    }
}

/// The first console is gone for good (docs/plans/console-redesign.md, decommission
/// checklist): none of its files comes back, no source names its routes, panels or tools, its
/// assets and APIs answer 404, and each old bookmark is either one redirect to a page of the
/// console at `/app/<instance>/<area>/<view>` or a 404.
#[test]
fn console_decommissioned() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::console::page::{self, BOOKMARKS, Bookmark, OLD_TABS};
    use pigeon_capabilities::console::sections::area_named;
    use pigeon_capabilities::console::{Console, Get, Post};

    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    for gone in [
        "console/approvals.js",
        "console/instances.js",
        "console/keys.js",
        "console/users.js",
        "console/console.css",
        "console/snapshot.rs",
        "console/web/legacy-shell.js",
        "console/web/legacy-shell.css",
        "console/web/management.js",
        "console/web/management.css",
        "console/web/sessions.js",
        "console/web/sessions.css",
        "console/web/cost.js",
        "console/web/cost.css",
        "console/web/settings.js",
        "console/web/settings.css",
        "console/web/connections.js",
        "console/web/connections.css",
        "console/web/architecture.js",
        "console/web/architecture.css",
        "site_passkeys/visitor.js",
    ] {
        let path = root.join("src/capabilities/src").join(gone);
        assert!(!path.exists(), "{gone} is back");
    }
    assert!(!root.join("src/ai-agents/src/page.rs").exists());

    // No source names the first console's routes, panels, tools or shims.
    const NAMES: &[&str] = &[
        "/console/api/section",
        "/console/site-access",
        "/secrets/put",
        "/tokens/issue",
        "/tokens/revoke",
        "/linked/unlink",
        "/instance/put",
        "/apple/adopt",
        "/apple/identity",
        "/app/next",
        "pigeonConsole",
        "legacy-shell",
        "legacyLink",
        "instances_tabs",
        "section_data",
        "SectionRequest",
        "visitor.js",
        "CONSOLE_TODO_PORT",
        "tab=management",
    ];
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| {
                ["rs", "js", "css", "html", "json", "toml", "swift"]
                    .contains(&e.to_str().unwrap_or(""))
            }) {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    walk(&root.join("ios"), &mut files);
    assert!(files.len() > 100, "the walk found the sources");
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        for name in NAMES {
            assert!(!text.contains(name), "{} names {name}", file.display());
        }
    }

    // Its assets and APIs answer 404.
    for gone in [
        "/console/api/section/credentials",
        "/console/api/section/approvals",
        "/console/keys.js",
        "/console/users.js",
        "/console/approvals.js",
        "/console/instances.js",
        "/console/console.css",
        "/app/legacy-shell.js",
        "/app/legacy-shell.css",
        "/app/sessions.js",
        "/app/cost.css",
        "/app/settings.js",
        "/app/connections.js",
        "/app/architecture.js",
        "/app/management.js",
        "/app/visitor.js",
        "/app/next/shell.js",
        "/app/next/views.js",
    ] {
        assert_eq!(Get::route(gone, None), None, "GET {gone}");
    }
    for gone in [
        "/console/site-access/hosts",
        "/console/site-access/list",
        "/console/site-access/invite/begin",
        "/secrets/put/begin",
        "/tokens/issue/begin",
        "/tokens/revoke/finish",
        "/linked/unlink/begin",
        "/instance/put/finish",
        "/apple/adopt/begin",
        "/app",
    ] {
        assert_eq!(Post::route(gone), None, "POST {gone}");
    }
    assert!(
        pigeon_capabilities::secrets::Secrets {
            host: "h".into(),
            parent: "p".into(),
        }
        .routes()
        .is_empty()
    );
    assert!(
        pigeon_capabilities::proxy::Tokens {
            proxy_host: "p".into()
        }
        .routes()
        .is_empty()
    );

    // Each old bookmark: one redirect to a page of the console, at a view the manifest sends.
    let every = console_every_view();
    let lands = |area: &str, view: Option<&str>| {
        assert!(area_named(area).is_some(), "{area}");
        if let Some(v) = view {
            assert!(every[area].iter().any(|w| w == v), "{area}/{v}");
        }
        let to = page::url("home", area, view);
        assert!(to.starts_with("/app/home/") && !to.contains('?'), "{to}");
        // The page it lands on is the console's, not another bookmark.
        assert_eq!(Get::route(&to, None), Some(Get::App), "{to}");
    };
    for (_, area, view) in BOOKMARKS {
        lands(area, *view);
    }
    for (.., area, view) in OLD_TABS {
        lands(area, *view);
    }
    let area = |path: &str, query: Option<&str>| match Get::route(path, query) {
        Some(Get::Bookmark(Bookmark::Area {
            instance,
            area,
            view,
        })) => Some((instance, area, view)),
        _ => None,
    };
    assert_eq!(Get::route("/", None), Some(Get::Bookmark(Bookmark::Root)));
    for (path, to) in [
        ("/approvals", ("inbox", None)),
        ("/instances", ("instances", Some("list"))),
        ("/architecture", ("platform", Some("architecture"))),
        ("/access", ("access", None)),
        ("/settings/", ("access", None)),
        ("/secrets", ("access", Some("secrets"))),
        ("/tokens", ("access", Some("tokens"))),
        ("/cost", ("cost", None)),
        ("/gateways", ("cost", None)),
    ] {
        assert_eq!(area(path, None), Some((None, to.0, to.1)), "{path}");
    }
    for (query, instance, to) in [
        ("tab=approvals", None, ("inbox", None)),
        (
            "tab=management&subtab=credentials",
            None,
            ("access", Some("secrets")),
        ),
        ("tab=management", None, ("access", Some("secrets"))),
        (
            "tab=management&subtab=delegation",
            None,
            ("access", Some("apps")),
        ),
        ("tab=cost&subtab=today", None, ("cost", Some("today"))),
        ("tab=cost&subtab=bogus", None, ("cost", None)),
        ("tab=sessions&subtab=main", None, ("sessions", Some("live"))),
        (
            "instance=docs&tab=settings&subtab=site",
            Some("docs"),
            ("site", Some("sites")),
        ),
        ("instance=docs&tab=home", Some("docs"), ("overview", None)),
        ("instance=docs", Some("docs"), ("overview", None)),
        // An instance id that is not one is dropped, not carried into the Location.
        ("instance=a%0d%0aX:1&tab=cost", None, ("cost", None)),
    ] {
        assert_eq!(
            area("/app", Some(query)),
            Some((instance.map(String::from), to.0, to.1)),
            "/app?{query}"
        );
    }
    // The console's own pages are no bookmark, with or without a query.
    assert_eq!(Get::route("/app", Some("fixtures=1")), Some(Get::App));
    assert_eq!(
        Get::route("/app/home/inbox", Some("tab=cost")),
        Some(Get::App)
    );
    // The console answers exactly these paths: its own, and the bookmarks.
    let console = Console {
        host: "h".into(),
        passkeys: pigeon_capabilities::passkeys::Passkeys {
            host: "h".into(),
            site_name: "s".into(),
        },
    };
    assert_eq!(
        console.routes(),
        [
            "/console/",
            "/app",
            "/approvals",
            "/instances",
            "/architecture",
            "/access",
            "/settings",
            "/secrets",
            "/tokens",
            "/cost",
            "/gateways",
        ]
    );
}

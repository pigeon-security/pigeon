//! The console's one write path (`console::write`) and one read path (`/app/op`'s allowlist).

use super::*;

use pigeon_core::oauth::hash;

use pigeon_capabilities::console::ops::{READS, allowed, inbox_for, read_keys};
use pigeon_capabilities::console::write::{
    ACTIONS, ANY_PASSKEY, Handler, LEGACY, Policy, Step, Stored, action, digest, legacy, purpose,
    redeem, route,
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
    const OWNERS: &[&str] = &["secrets", "proxy", "linked", "instances", "console"];
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
    // Only deciding approvals and approving a landing may come from the app by bearer.
    let bearer: Vec<&str> = ACTIONS
        .iter()
        .filter(|a| a.by_bearer)
        .map(|a| a.id)
        .collect();
    assert_eq!(
        bearer,
        ["land-approve", "approval-approve", "approval-deny"]
    );
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
    assert_ne!(
        begun,
        p(ANY_PASSKEY, "blog", &release),
        "any passkey is not this person"
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
fn console_old_write_routes_reach_the_same_actions() {
    let expected = [
        ("/secrets/put/begin", "secret-set", true),
        ("/tokens/issue/begin", "token-issue", true),
        ("/tokens/revoke/begin", "token-revoke", true),
        ("/linked/unlink/begin", "linked-unlink", false),
        ("/instance/put/begin", "instance-put", false),
        ("/console/site-access/invite/begin", "visitor-invite", false),
    ];
    assert_eq!(LEGACY.len(), expected.len() * 2);
    for (path, id) in LEGACY {
        assert!(action(id).is_some(), "{path}");
    }
    let record = r#"{"id":"blog","version":3}"#;
    let bodies = |id: &str| match id {
        "secret-set" => (
            json!({ "name": "X", "digest": hash("v") }),
            json!({ "name": "X", "value": "v" }),
        ),
        "token-issue" => (
            json!({ "label": "env", "days": 7 }),
            json!({ "label": "env", "days": 7 }),
        ),
        "token-revoke" => (
            json!({ "id": "aaaaaaaaaaaa" }),
            json!({ "id": "aaaaaaaaaaaa" }),
        ),
        "linked-unlink" => (
            json!({ "instance": "alpha" }),
            json!({ "instance": "alpha" }),
        ),
        "instance-put" => (
            json!({ "record": record, "digest": hash(record) }),
            json!({ "record": record }),
        ),
        _ => {
            let b = json!({ "instance": "alpha", "site": "blog", "host": "blog.example.test", "name": "Alex" });
            (b.clone(), b)
        }
    };
    for (begin, id, any) in expected {
        let finish = begin.replace("/begin", "/finish");
        let (b, f) = bodies(id);
        let lb = legacy(begin, &b).unwrap();
        let lf = legacy(&finish, &f).unwrap();
        assert_eq!(
            (lb.action.id, lb.step, lb.any_passkey),
            (id, Step::Begin, any),
            "{begin}"
        );
        assert_eq!((lf.action.id, lf.step), (id, Step::Finish), "{finish}");
        assert_eq!(lb.action.handler, action(id).unwrap().handler);
        // The old begin and finish bodies bind the same purpose as the new path does.
        let caller = if any { ANY_PASSKEY } else { "u1" };
        assert_eq!(
            bound_purpose(id, caller, lb.target, lb.change, Step::Begin),
            bound_purpose(id, caller, lf.target, lf.change, Step::Finish),
            "{begin}"
        );
    }
    assert!(legacy("/secrets/put/other", &json!({})).is_none());
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
        ("ai_agents_stop", json!({})),
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

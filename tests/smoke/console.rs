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
        "digitalocean",
        "proxy",
        "linked",
        "instances",
        "console",
        "apple",
        "users",
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
        "app-group-disconnect",
        "user-remove",
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
    // Exactly what the iOS app makes may come from it by bearer (bearer parity).
    let bearer: Vec<&str> = ACTIONS
        .iter()
        .filter(|a| a.by_bearer)
        .map(|a| a.id)
        .collect();
    assert_eq!(bearer, APP_WRITES);
}

/// Every write the iOS app makes, in [`ACTIONS`] order (the mock-up's "Ops each screen uses").
const APP_WRITES: &[&str] = &[
    "secret-set",
    "token-issue",
    "token-revoke",
    "linked-unlink",
    "instance-put",
    "visitor-invite",
    "visitor-revoke",
    "passkey-remove",
    "app-disconnect",
    "invite",
    "land-approve",
    "approval-approve",
    "approval-deny",
    "model-defaults",
    "agents-config",
    "site-rollback",
    "site-deploy",
    "run-stop",
    "run-note",
    "run-extend",
];

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

    // The writes the app does not make keep needing the console's own session.
    for id in ["land-approval", "apple-adopt", "apple-identity"] {
        let (a, _) = route(&format!("/console/write/{id}/begin")).unwrap();
        assert!(!a.by_bearer, "{id} is not allowed by bearer");
    }
}

/// Bearer parity: the iOS app reads through `/app/op` and writes through
/// `/console/write/<action>/{begin,finish}` by bearer, exactly as the person it acts for would
/// from the console. Every app write binds the same purpose for the same person, target and
/// change (the purpose has no input for how the person was found), and none for another
/// person; the cookie still needs this site's Origin; a bearer read meets the same allowlist
/// and instance binding.
#[test]
fn console_app_bearer_parity() {
    use pigeon_capabilities::console::session::{admitted, bearer};
    use pigeon_capabilities::console::{Post, app_person};
    use pigeon_capabilities::users::Caller;

    // Writes: a valid target and change (begin, finish) for every write the app makes.
    let release = format!("release-{}", "a".repeat(43));
    let site = json!({ "instance": "blog", "site": "blog", "host": "blog.example.org" });
    let same = |v: Value| (v.clone(), v);
    let samples: Vec<(&str, Value, (Value, Value))> = vec![
        (
            "secret-set",
            json!("X"),
            (json!({ "digest": hash("v") }), json!({ "value": "v" })),
        ),
        ("token-issue", json!("laptop"), same(json!({ "days": 30 }))),
        ("token-revoke", json!("abcdefghijkl"), same(json!({}))),
        ("linked-unlink", json!("blog"), same(json!({}))),
        (
            "instance-put",
            json!("blog"),
            same(json!({ "record": "{\"id\":\"blog\"}" })),
        ),
        (
            "visitor-invite",
            site.clone(),
            same(json!({ "name": "Lee" })),
        ),
        ("visitor-revoke", site, same(json!({ "user": "u1" }))),
        ("passkey-remove", json!("k1"), same(json!({}))),
        ("app-disconnect", json!("app1"), same(json!({}))),
        ("invite", json!("Sam Ito"), same(json!({}))),
        ("land-approve", json!("l1"), same(json!({}))),
        ("approval-approve", json!("a1"), same(json!({}))),
        ("approval-deny", json!("a1"), same(json!({}))),
        (
            "model-defaults",
            Value::Null,
            same(json!({ "set": { "executor": "m" } })),
        ),
        (
            "agents-config",
            Value::Null,
            same(json!({ "max_live_runs": 2 })),
        ),
        (
            "site-rollback",
            json!("blog"),
            same(json!({ "release": release })),
        ),
        ("site-deploy", json!("blog"), same(json!({ "rev": "main" }))),
        ("run-stop", json!("r1"), same(json!({}))),
        ("run-note", json!("r1"), same(json!({ "text": "hi" }))),
        ("run-extend", json!("r1"), same(json!({ "turns": 5 }))),
    ];
    let ids: Vec<&str> = samples.iter().map(|(id, ..)| *id).collect();
    assert_eq!(ids, APP_WRITES);
    for (id, target, (begun, finished)) in samples {
        let a = action(id).unwrap();
        assert!(a.by_bearer, "{id}");
        let p = |who: &str, change: &Value, step| {
            bound_purpose(id, who, target.clone(), change.clone(), step).unwrap()
        };
        // Begun by bearer and finished from the console (or the other way): one purpose.
        let begin = p("willow", &begun, Step::Begin);
        assert_eq!(begin, p("willow", &finished, Step::Finish), "{id}");
        assert!(begin.starts_with(&format!("console:write:{id}:")), "{id}");
        // A bearer for another person finds no challenge begun for this one.
        assert_ne!(begin, p("alex", &finished, Step::Finish), "{id}");
        // A tool write still goes through MCP's policy gate, whoever starts it.
        if let Handler::Tool(_) = a.handler {
            assert_eq!(a.policy, Policy::ToolGate, "{id}");
        }
    }
    for gone in ["land-approval", "apple-adopt", "apple-identity"] {
        assert!(!action(gone).unwrap().by_bearer, "{gone}");
    }

    // The person is the one the app's OAuth token acts for, and only a person.
    let user =
        |id: &str, kind: &str| json!({ "id": id, "kind": kind, "name": id, "created_at": 0 });
    let caller = |acting: Value| -> Caller {
        serde_json::from_value(json!({ "user": user("ios", "service"), "acting_for": acting }))
            .unwrap()
    };
    assert_eq!(
        app_person(caller(user("willow", "standard"))).map(|u| u.id),
        Some("willow".into())
    );
    assert!(app_person(caller(Value::Null)).is_none(), "acts for no one");
    assert!(
        app_person(caller(user("bot", "service"))).is_none(),
        "acts for a service user"
    );

    // Which POSTs take a bearer: the reads and the writes, never signing in or out.
    for (path, yes) in [
        ("/app/op", true),
        ("/console/write/secret-set/begin", true),
        ("/console/write/land-approval/finish", true),
        ("/console/sign-in/begin", false),
        ("/console/sign-in/finish", false),
        ("/console/sign-out", false),
    ] {
        assert_eq!(Post::route(path).unwrap().takes_bearer(), yes, "{path}");
    }
    assert_eq!(bearer(Some("Bearer  t0k ")), Some("t0k"));
    for no in [None, Some("Bearer "), Some("Basic dTpw"), Some("bearer t")] {
        assert_eq!(bearer(no), None, "{no:?}");
    }

    // CSRF: a cookie request still needs this site's Origin; a bearer one needs none. Both are
    // JSON only.
    let host = "platform.pigeon.dev";
    let json_type = Some("application/json");
    assert!(admitted(
        Some("https://platform.pigeon.dev"),
        host,
        json_type,
        false
    ));
    for origin in [None, Some("https://evil.example"), Some("null")] {
        assert!(!admitted(origin, host, json_type, false), "{origin:?}");
        assert!(admitted(origin, host, json_type, true), "{origin:?}");
    }
    for by_bearer in [false, true] {
        assert!(!admitted(
            Some("https://platform.pigeon.dev"),
            host,
            Some("text/plain"),
            by_bearer
        ));
    }

    // Reads: the one allowlist, whoever asks; no op or argument more for the app.
    for op in ["instances_list", "console_manifest", "app_inbox"] {
        assert!(allowed(op, &json!({})), "{op}");
    }
    assert!(allowed("console_manifest", &json!({ "instance": "blog" })));
    assert!(!allowed("ios_releases", &json!({})), "not a console read");
    assert!(!allowed("site_rollback", &json!({ "instance": "blog" })));
    assert!(!allowed("instances_list", &json!({ "extra": 1 })));
    assert!(!allowed(
        "console_manifest",
        &json!({ "instance": "blog", "x": 1 })
    ));
    // And the same instance binding: a bound read cannot widen past its record.
    let record: pigeon_capabilities::instances::record::Instance = serde_json::from_value(json!({
        "id": "blog", "title": "Blog", "account": { "kind": "home" },
        "storage": { "state": "enabled", "store": "scaleway:archive", "bucket": "blog-assets" },
    }))
    .unwrap();
    let bind = |tool: &str, args: Value| {
        pigeon_capabilities::instances::tools::bind_global("blog", Some(&record), tool, &args)
    };
    assert!(INSTANCE_OPS.contains(&"storage_list_objects"));
    assert!(bind("storage_list_objects", json!({ "instance": "blog" })).is_ok());
    assert!(
        bind(
            "storage_list_objects",
            json!({ "instance": "blog", "bucket": "other" })
        )
        .is_err()
    );
    assert!(
        bind("source_read_file", json!({ "instance": "blog" })).is_err(),
        "source is off"
    );
}

/// Who may manage people (docs/plans/capability-scoping.md, "Platform admin"): one's own
/// passkey, or anyone's as an admin, never a last one; only an admin invites, disconnects an
/// app or changes a role, and the last admin stays one. The console's writes for these are the
/// `users` capability's, which asks the same rules as the MCP tools at `begin` and `finish`;
/// the iOS app makes them by bearer as its person, under the same rules.
#[test]
fn console_people_admin_rules() {
    use pigeon_capabilities::users::{
        Caller, LAST_ADMIN, LAST_PASSKEY, User, admin_only, passkey_removal, person_of, role_change,
    };

    // Each people write is made by `users`, not the console's plumbing.
    for id in [
        "passkey-remove",
        "app-disconnect",
        "app-group-disconnect",
        "invite",
        "person-admin",
    ] {
        let a = action(id).unwrap();
        assert_eq!(a.handler, Handler::Capability("users"), "{id}");
        assert_eq!(a.policy, Policy::Passkey, "{id}");
    }
    // Making admins is the console's only: the app does not.
    assert!(!action("person-admin").unwrap().by_bearer);
    for id in ["passkey-remove", "app-disconnect", "invite"] {
        assert!(action(id).unwrap().by_bearer, "{id}");
    }

    // passkey-remove: your own, with another left.
    assert_eq!(passkey_removal("willow", false, "willow", 2), Ok(()));
    // Someone else's, as a member (standard user who is not an admin): refused.
    let refused = passkey_removal("sam", false, "willow", 2).unwrap_err();
    assert!(refused.contains("only an admin"), "{refused}");
    // Someone else's, as an admin: allowed.
    assert_eq!(passkey_removal("willow", true, "sam", 2), Ok(()));
    // Nobody's last, not even an admin's own or by an admin.
    assert_eq!(
        passkey_removal("willow", true, "willow", 1).unwrap_err(),
        LAST_PASSKEY
    );
    assert_eq!(
        passkey_removal("willow", true, "sam", 1).unwrap_err(),
        LAST_PASSKEY
    );
    // A member is refused another's last passkey for not being an admin first.
    assert!(
        passkey_removal("sam", false, "willow", 1)
            .unwrap_err()
            .contains("only an admin")
    );

    // invite, app-disconnect, users_remove, oauth_revoke: an admin's only.
    assert_eq!(admin_only(true, "invite a person"), Ok(()));
    assert_eq!(
        admin_only(false, "invite a person").unwrap_err(),
        "only an admin can invite a person"
    );

    // person-admin / users_admin: the last admin stays one; more may be made, and one of two
    // may step down.
    let one = ["willow".to_string()];
    let two = ["willow".to_string(), "sam".to_string()];
    assert_eq!(role_change(&one, "sam", true), Ok(()));
    assert_eq!(role_change(&one, "willow", false).unwrap_err(), LAST_ADMIN);
    assert_eq!(role_change(&one, "willow", true), Ok(()));
    assert_eq!(role_change(&two, "willow", false), Ok(()));
    assert_eq!(
        role_change(&one, "sam", false),
        Ok(()),
        "a member made a member"
    );

    // The person a caller acts as: a person themselves, or the person an app acts for; an app
    // acting for no one is no person, so it is no admin and owns no passkey.
    let user = |id: &str, kind: &str| User {
        id: id.into(),
        kind: kind.into(),
        name: id.into(),
        created_at: 0,
    };
    let me = Caller {
        user: user("willow", "standard"),
        acting_for: None,
        run: None,
    };
    assert_eq!(person_of(&me).unwrap().id, "willow");
    let app = Caller {
        user: user("ios", "service"),
        acting_for: Some(user("willow", "standard")),
        run: None,
    };
    assert_eq!(person_of(&app).unwrap().id, "willow");
    let loose = Caller {
        user: user("ios", "service"),
        acting_for: None,
        run: None,
    };
    assert!(person_of(&loose).is_none());

    // The change person-admin takes: {admin} as a boolean, and nothing else.
    let a = action("person-admin").unwrap();
    assert!(a.target(&json!("u_sam")).is_ok());
    assert_eq!(
        a.change(&json!({ "admin": true }), Step::Begin),
        Ok(json!({ "admin": true }))
    );
    assert!(a.change(&json!({ "admin": "yes" }), Step::Begin).is_err());
    assert!(a.change(&json!({}), Step::Begin).is_err());
    assert!(
        a.change(&json!({ "admin": true, "role": "x" }), Step::Begin)
            .is_err()
    );
    // Bound to the exact change: making and unmaking are different purposes.
    assert_ne!(
        bound_purpose(
            "person-admin",
            "willow",
            json!("u_sam"),
            json!({ "admin": true }),
            Step::Begin
        ),
        bound_purpose(
            "person-admin",
            "willow",
            json!("u_sam"),
            json!({ "admin": false }),
            Step::Begin
        ),
    );
}

/// Removing a person (Access › People's Remove, `users_remove`): only an admin may, never
/// themselves, and never the last admin. The console's `user-remove` is a `users` write like
/// `person-admin`: console only, passkey-bound to the caller and target at `begin` and `finish`
/// alike, its rules asked at both. Removal deletes the person's passkeys, sessions, roles and
/// tokens, disconnects the apps acting for them, and leaves everyone else's rows.
#[test]
fn console_user_remove() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::membership::{Need, write_need};
    use pigeon_capabilities::users::{
        LAST_ADMIN, MIGRATIONS, REMOVE_USER, SELF_REMOVAL, Users, person_removal, removal_lines,
    };

    // The action: the `users` capability's, behind the person's passkey, a danger dialog, and
    // the console's only (the iOS app does not remove people).
    let a = action("user-remove").unwrap();
    assert_eq!(a.handler, Handler::Capability("users"));
    assert_eq!(a.policy, Policy::Passkey);
    assert!(a.destructive);
    assert!(!a.by_bearer);
    assert_eq!(write_need(a, &json!("u_sam")), Need::Own);
    for (path, step) in [
        ("/console/write/user-remove/begin", Step::Begin),
        ("/console/write/user-remove/finish", Step::Finish),
    ] {
        assert_eq!(route(path), Some((a, step)), "{path}");
    }
    // Parity with the other writes: begin and finish bind one purpose for the same caller and
    // target, and none for another caller or another person.
    let p = |caller: &str, target: &str, step| {
        bound_purpose("user-remove", caller, json!(target), json!({}), step).unwrap()
    };
    let begun = p("willow", "u_sam", Step::Begin);
    assert!(begun.starts_with("console:write:user-remove:"));
    assert_eq!(begun, p("willow", " u_sam ", Step::Finish));
    assert_ne!(begun, p("rowan", "u_sam", Step::Finish), "another caller");
    assert_ne!(begun, p("willow", "u_kai", Step::Finish), "another person");
    assert_ne!(
        begun,
        bound_purpose(
            "passkey-remove",
            "willow",
            json!("u_sam"),
            json!({}),
            Step::Finish
        )
        .unwrap(),
        "another action"
    );
    assert!(a.target(&json!("u sam")).is_err());
    assert!(a.change(&json!({ "admin": true }), Step::Begin).is_err());

    // The rules, the same for the tool and the console.
    let one = ["willow".to_string()];
    let two = ["willow".to_string(), "rowan".to_string()];
    let refused = person_removal("sam", false, &one, "kai").unwrap_err();
    assert!(refused.contains("only an admin"), "{refused}");
    assert!(
        person_removal("sam", false, &one, "willow")
            .unwrap_err()
            .contains("only an admin")
    );
    assert_eq!(person_removal("willow", true, &one, "sam"), Ok(()));
    assert_eq!(
        person_removal("willow", true, &one, "willow").unwrap_err(),
        SELF_REMOVAL
    );
    assert_eq!(
        person_removal("willow", true, &two, "willow").unwrap_err(),
        SELF_REMOVAL,
        "not even with another admin left"
    );
    assert_eq!(person_removal("willow", true, &two, "rowan"), Ok(()));
    // The last admin stays: the remover is an admin, so this is a race (`begin` saw two), which
    // `finish` and the Directory's own check catch.
    assert_eq!(
        person_removal("rowan", true, &one, "willow").unwrap_err(),
        LAST_ADMIN
    );

    // The tools: removing, and seeing what removing would delete first.
    let tools = Users { host: "h".into() }.tools();
    let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap();
    let remove = tool("users_remove").description;
    for word in ["Only an admin", "last admin", "themselves", "disconnected"] {
        assert!(remove.contains(word), "users_remove: {word}");
    }
    assert_eq!(
        tool("users_remove_preview").input_schema["required"],
        json!(["id"])
    );

    // What goes, on the Directory's real schema: Sam's passkeys, sessions, roles, tokens, push
    // registrations and the link and tokens of the app acting for him; not Willow's or Kai's,
    // and not the app itself, which acts for no one after.
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for m in MIGRATIONS {
        db.execute_batch(m).unwrap();
    }
    let run = |sql: &str| db.execute_batch(sql).unwrap();
    run("INSERT INTO users (id, kind, name, created_at, role) VALUES
           ('willow', 'standard', 'Willow', 1, 'admin'),
           ('sam', 'standard', 'Sam', 2, NULL),
           ('kai', 'standard', 'Kai', 3, NULL),
           ('app', 'service', 'Claude', 4, NULL),
           ('kai_app', 'service', 'Agent', 5, NULL)");
    run(
        "INSERT INTO passkeys (id, user_id, point, sign_count, name, created_at) VALUES
           ('k1', 'sam', 'p', 0, 'Phone', 1), ('k2', 'sam', 'p', 0, 'Key', 1),
           ('k3', 'willow', 'p', 0, 'Mac', 1), ('k4', 'kai', 'p', 0, 'Pixel', 1)",
    );
    run(
        "INSERT INTO sessions (hash, user_id, created_at, last_seen_at) VALUES
           ('s1', 'sam', 1, 1), ('s2', 'willow', 1, 1)",
    );
    run("INSERT INTO members (instance, person_id, role) VALUES
           ('sandbox', 'sam', 'owner'), ('pigeon.dev', 'sam', 'viewer'),
           ('pigeon.dev', 'kai', 'operator')");
    run(
        "INSERT INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES
           ('app', 'sam', 1, 'k1'), ('kai_app', 'kai', 1, 'k4')",
    );
    run(
        "INSERT INTO oauth_clients (id, name, redirect_uris, service_user_id, created_at) VALUES
           ('c1', 'Claude', '[]', 'app', 1)",
    );
    run("INSERT INTO oauth_tokens (hash, kind, client_id, service_user_id, expires_at, created_at) VALUES
           ('t1', 'access', 'c1', 'app', 9, 1), ('t2', 'refresh', 'c1', 'app', 9, 1),
           ('t3', 'proxy', NULL, 'sam', 9, 1), ('t4', 'access', NULL, 'kai_app', 9, 1)");
    run("INSERT INTO apns_devices (id, app, person_id, service_user_id, token, environment, created_at) VALUES
           ('d1', 'ios', 'sam', 'app', 'x', 'prod', 1), ('d2', 'ios', 'kai', 'kai_app', 'y', 'prod', 1)");
    run("INSERT INTO enrolments (code, user_id, expires_at) VALUES ('e1', 'sam', 9)");
    for q in REMOVE_USER {
        let n = q.matches('?').count();
        db.execute(q, rusqlite::params_from_iter(vec!["sam"; n]))
            .unwrap();
    }
    let ids = |sql: &str| -> Vec<String> {
        let mut q = db.prepare(sql).unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        ids("SELECT id FROM users ORDER BY id"),
        ["app", "kai", "kai_app", "willow"]
    );
    assert_eq!(ids("SELECT id FROM passkeys ORDER BY id"), ["k3", "k4"]);
    assert_eq!(ids("SELECT hash FROM sessions"), ["s2"]);
    assert_eq!(
        ids("SELECT person_id || '@' || instance FROM members"),
        ["kai@pigeon.dev"]
    );
    assert_eq!(ids("SELECT service_user_id FROM links"), ["kai_app"]);
    assert_eq!(ids("SELECT hash FROM oauth_tokens"), ["t4"]);
    assert_eq!(ids("SELECT id FROM oauth_clients"), ["c1"], "the app stays");
    assert_eq!(ids("SELECT id FROM apns_devices"), ["d2"]);
    assert!(ids("SELECT code FROM enrolments").is_empty());

    // The dialog's lines: what goes, counted, and that it cannot be undone.
    let lines = removal_lines(&json!({
        "user": { "id": "sam", "kind": "standard", "name": "Sam Ito", "created_at": 2 },
        "admin": false, "passkeys": 2, "sessions": 1, "tokens": 1,
        "roles": [{ "instance": "pigeon.dev", "role": "viewer" }, { "instance": "sandbox", "role": "owner" }],
        "last_owner_of": ["sandbox"],
        "apps": [{ "id": "app", "name": "Claude", "kind": "app", "tokens": 2 }],
    }));
    assert_eq!(
        lines,
        [
            "Sam Ito is removed and can no longer sign in or approve.",
            "Deletes 2 passkeys.",
            "Ends 1 console session.",
            "Removes their roles: pigeon.dev (viewer), sandbox (owner).",
            "They are the last owner of sandbox; admins stay owners there.",
            "Disconnects 1 app acting for them, revoking their tokens: Claude.",
            "Revokes 1 token they hold.",
            "This cannot be undone.",
        ]
    );

    // Access › People: Remove for an admin, never on one's own row nor for the last admin, in
    // a danger dialog confirmed with the passkey, and the list read again after.
    let view = include_str!("../../src/capabilities/src/console/web/views/access.js");
    for needle in [
        "action: \"user-remove\"",
        "title: \"Remove person\", danger: true",
        "confirmLabel: \"Remove with passkey\"",
        "This cannot be undone.",
        "meAdmin && !p.me",
        "p.admin && admins <= 1",
    ] {
        assert!(view.contains(needle), "access.js: {needle}");
    }
}

/// Apps acting for a person (`delegates`): the platform's clients and service users as they
/// are live, rolled up one per real app and person; an app disconnected as one, on the
/// Directory's real schema, leaving its other people's, its proxy tokens and its clients; a
/// person's removal still clearing everything; the clean-up plan's checks; and Access › People's
/// data and view.
#[test]
fn console_app_delegates() {
    use pigeon_capabilities::delegates::{
        self as d, DISCONNECT_GRANT, GRANTS_SQL, Grant, app_of, app_with, group, still_members,
    };
    use pigeon_capabilities::users::{MIGRATIONS, REMOVE_USER, disconnect_lines, removal_lines};

    // The live platform's 18 service users, as `oauth_clients` and `users_list` answered on
    // 26 November (Willow is `w`): (id, name, redirect URI or none, service id, linked).
    let ios = "https://platform.pigeon.dev/app/oauth";
    type Row<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a str>, bool);
    let rows: [Row; 18] = [
        ("pj7dM_Jf7VnquzzOXnea2v", "Claude", None, None, false),
        ("3IQ5uYePQwgzxNxk7QUoU8", "probe", None, None, false),
        (
            "Zn1_T1mNC90uA4yAG7lb6n",
            "Cloudflare MCP Portal",
            Some(
                "https://dash.cloudflare.com/4f94/one/access-controls/ai-controls/mcp-server/oauth-callback/pigeon",
            ),
            None,
            true,
        ),
        (
            "S7J12_Q6DL9PT5ZBP0qSG2",
            "Cloudflare MCP Portal",
            Some("https://mcp.pigeon.dev/servers-callback"),
            None,
            true,
        ),
        (
            "7J_ufHKtBGlOJQmJDChv71",
            "Claude",
            Some("https://claude.ai/api/mcp/auth_callback"),
            None,
            true,
        ),
        (
            "ncLywA4Jiv2pH_Kyf4L1I0",
            "Pigeon planner",
            None,
            Some("ai-agent:planner"),
            true,
        ),
        (
            "IHxWvKcYpS_JkIRYQR8UOd",
            "Pigeon executor",
            None,
            Some("ai-agent:executor"),
            true,
        ),
        (
            "fDg0dvKXY2HcmYia6BgsuJ",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "ltBeEvXJ9i5AZecpvCtlQu",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "4mZ5esml0A6a-2dllvvoke",
            "x",
            Some("https://chatgpt.com/connector_platform_oauth_redirect"),
            None,
            false,
        ),
        (
            "sL---g8C_wojNooXL_3bMz",
            "Codex",
            Some("http://127.0.0.1:59061/callback/j9pVkEBFs4kI"),
            None,
            true,
        ),
        (
            "ohF3WqOpr2_-g5HbhjeNqq",
            "Pigeon ChatGPT connectivity probe (no sign-in)",
            Some("https://chatgpt.com/connector_platform_oauth_redirect"),
            None,
            false,
        ),
        (
            "-IxPdyQgcA7Vw6TjDcdWm_",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "8Rdkjl0eb1gkqhV2Ys2fob",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "BykU0BgsA2eAHshI89ci2O",
            "ChatGPT",
            Some("https://chatgpt.com/connector/oauth/WJXcr2Iya52_"),
            None,
            true,
        ),
        (
            "JioMsMHqiQ5Pa4tPb7zow4",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "LGJ-TWs8KQPqgBiT6KiOWL",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
        (
            "CExKpvhCUjT6H9L8S3fVoW",
            "Pigeon for iOS",
            Some(ios),
            None,
            true,
        ),
    ];
    let grants: Vec<Grant> = rows
        .iter()
        .enumerate()
        .map(|(i, (id, name, uri, key, linked))| {
            Grant::from_row(&json!({
                "id": id, "name": name, "created_at": i as i64,
                "client_id": uri.map(|_| format!("c{i}")),
                "redirect_uris": uri.map(|u| json!([u]).to_string()),
                "service_key": key, "person": linked.then_some("w"),
                "linked_at": linked.then_some(100 + i as i64),
                "tokens": 1, "last_used_at": 1000 + i as i64,
            }))
        })
        .collect();

    // How each one is told: redirect host, name with a known path or loopback, service id.
    let by = |id: &str| app_of(grants.iter().find(|g| g.id == id).unwrap());
    assert_eq!(by("7J_ufHKtBGlOJQmJDChv71").by, "redirect");
    assert_eq!(
        by("sL---g8C_wojNooXL_3bMz").key,
        "openai",
        "Codex: loopback, by name"
    );
    assert_eq!(by("sL---g8C_wojNooXL_3bMz").by, "name");
    assert_eq!(by("S7J12_Q6DL9PT5ZBP0qSG2").key, "cloudflare-portal");
    assert_eq!(by("ncLywA4Jiv2pH_Kyf4L1I0").role.unwrap().id, "planner");
    assert_eq!(by("IHxWvKcYpS_JkIRYQR8UOd").by, "service id");
    // `x` registered with ChatGPT's callback: OpenAI's, not Grokbot (grok.com).
    assert_eq!(by("4mZ5esml0A6a-2dllvvoke").key, "openai");
    assert_eq!(
        by("pj7dM_Jf7VnquzzOXnea2v").key,
        "claude",
        "a revoked client's leftover"
    );
    assert_eq!(by("pj7dM_Jf7VnquzzOXnea2v").by, "name");
    assert_eq!(grants[0].kind(), "leftover");
    assert_eq!(by("3IQ5uYePQwgzxNxk7QUoU8").key, "name:probe");
    // A name alone never claims a known app when the client redirects to a host of its own.
    let impostor = Grant::from_row(&json!({
        "id": "i", "name": "ChatGPT", "client_id": "ci",
        "redirect_uris": "[\"https://chatgpt.example.net/cb\"]",
    }));
    assert_eq!(app_of(&impostor).key, "host:chatgpt.example.net");
    let grok = Grant::from_row(&json!({
        "id": "g", "name": "Grok", "client_id": "cg",
        "redirect_uris": "[\"https://grok.com/connectors-oauth-exchange-code/\"]",
    }));
    assert_eq!(app_of(&grok).label, "Grokbot");

    // Willow's 14 grants are 6 apps; the 4 acting for no one, 3 entries.
    let apps = group(&grants);
    let willow: Vec<(&str, i64)> = apps
        .iter()
        .filter(|a| a["person"] == "w")
        .map(|a| (a["label"].as_str().unwrap(), a["clients"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        willow,
        [
            ("AI Gateway", 2),
            ("Claude", 1),
            ("Cloudflare MCP Portal", 2),
            ("OpenAI", 2),
            ("Pigeon for iOS", 7),
        ]
        .iter()
        .map(|(l, n)| (*l, *n))
        .collect::<Vec<_>>()
    );
    let gateway = apps.iter().find(|a| a["app"] == "ai-gateway").unwrap();
    assert_eq!(gateway["roles"][0]["id"], "planner");
    assert_eq!(gateway["roles"][1]["id"], "executor");
    assert!(
        gateway["roles"][0]["note"]
            .as_str()
            .unwrap()
            .contains("land")
    );
    assert_eq!(gateway["kinds"], json!(["service"]));
    let ios_app = apps.iter().find(|a| a["app"] == "pigeon-ios").unwrap();
    assert_eq!(ios_app["tokens"], 7);
    assert_eq!(ios_app["last_used_at"], 1017);
    assert_eq!(
        ios_app["members"][0]["id"], "CExKpvhCUjT6H9L8S3fVoW",
        "newest first"
    );
    let none: Vec<&str> = apps
        .iter()
        .filter(|a| a["person"].is_null())
        .map(|a| a["label"].as_str().unwrap())
        .collect();
    assert_eq!(none, ["Claude", "OpenAI", "probe"]);

    // An admin's override wins: one of the known apps, or its own entry.
    let mut moved = grants[1].clone();
    moved.app_override = Some("grokbot".into());
    assert_eq!(app_of(&moved).label, "Grokbot");
    assert_eq!(app_of(&moved).by, "override");
    moved.app_override = Some("own".into());
    assert_eq!(app_of(&moved).key, "own:3IQ5uYePQwgzxNxk7QUoU8");
    assert!(d::override_allowed("openai") && !d::override_allowed("nope"));

    // Disconnecting the app as the dialog listed it: a client that joined since stays.
    let openai = app_with(&apps, "BykU0BgsA2eAHshI89ci2O").unwrap();
    assert_eq!(
        still_members(openai, &["sL---g8C_wojNooXL_3bMz".into(), "gone".into()]),
        ["sL---g8C_wojNooXL_3bMz"]
    );
    let lines = disconnect_lines(
        openai,
        &[
            "BykU0BgsA2eAHshI89ci2O".into(),
            "sL---g8C_wojNooXL_3bMz".into(),
        ],
    );
    assert_eq!(lines[0], "Disconnects OpenAI: 2 clients of 2 clients.");
    assert!(lines[1].starts_with("ChatGPT (BykU0BgsA2eAHshI89ci2O): revokes 1 live token"));
    // The write: any one client as its target, the clients the dialog showed as its change.
    let a = action("app-group-disconnect").unwrap();
    assert_eq!(a.handler, Handler::Capability("users"));
    assert!(a.destructive && !a.by_bearer);
    assert_eq!(
        a.change(&json!({ "members": ["b", " a", "b"] }), Step::Begin),
        Ok(json!({ "members": ["a", "b"] }))
    );
    for bad in [
        json!({}),
        json!({ "members": [] }),
        json!({ "members": ["a b"] }),
    ] {
        assert!(a.change(&bad, Step::Begin).is_err(), "{bad}");
    }
    assert_ne!(
        bound_purpose(
            "app-group-disconnect",
            "w",
            json!("x"),
            json!({ "members": ["a"] }),
            Step::Begin
        ),
        bound_purpose(
            "app-group-disconnect",
            "w",
            json!("x"),
            json!({ "members": ["a", "b"] }),
            Step::Begin
        ),
        "the passkey covers exactly the clients listed"
    );

    // On the Directory's real schema.
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for m in MIGRATIONS {
        db.execute_batch(m).unwrap();
    }
    let run = |sql: &str| db.execute_batch(sql).unwrap();
    run("INSERT INTO users (id, kind, name, created_at, role) VALUES
           ('w', 'standard', 'Willow', 1, 'admin'), ('sam', 'standard', 'Sam', 2, NULL),
           ('gpt', 'service', 'ChatGPT', 3, NULL), ('codex', 'service', 'Codex', 4, NULL),
           ('sam_gpt', 'service', 'ChatGPT', 5, NULL), ('plan', 'service', 'Pigeon planner', 6, NULL),
           ('x', 'service', 'x', 7, NULL)");
    run("INSERT INTO service_ids (access_client_id, user_id) VALUES ('ai-agent:planner', 'plan')");
    run(
        r#"INSERT INTO oauth_clients (id, name, redirect_uris, service_user_id, created_at) VALUES
           ('c1', 'ChatGPT', '["https://chatgpt.com/connector/oauth/a"]', 'gpt', 3),
           ('c2', 'Codex', '["http://127.0.0.1:1/cb"]', 'codex', 4),
           ('c3', 'ChatGPT', '["https://chatgpt.com/connector/oauth/b"]', 'sam_gpt', 5),
           ('c4', 'x', '["https://chatgpt.com/connector_platform_oauth_redirect"]', 'x', 7)"#,
    );
    run("INSERT INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES
           ('gpt', 'w', 10, 'k'), ('codex', 'w', 11, 'k'), ('sam_gpt', 'sam', 12, 'k'), ('plan', 'w', 13, 'k')");
    run("INSERT INTO oauth_tokens (hash, kind, client_id, service_user_id, expires_at, created_at) VALUES
           ('t1', 'access', 'c1', 'gpt', 99, 20), ('t2', 'refresh', 'c2', 'codex', 99, 21),
           ('t3', 'access', 'c3', 'sam_gpt', 99, 22), ('t4', 'proxy', NULL, 'gpt', 99, 23),
           ('t5', 'access', 'c2', 'codex', 5, 30)");
    run("INSERT INTO apns_devices (id, app, person_id, service_user_id, token, environment, created_at) VALUES
           ('d1', 'ios', 'w', 'gpt', 'tk', 'prod', 1)");
    let grants_now = |db: &rusqlite::Connection| -> Vec<Grant> {
        let mut q = db.prepare(GRANTS_SQL).unwrap();
        let names: Vec<String> = q.column_names().iter().map(|n| n.to_string()).collect();
        q.query_map([10], |r| {
            let mut o = serde_json::Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(f) => json!(f),
                    rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => Value::Null,
                };
                o.insert(n.clone(), v);
            }
            Ok(Grant::from_row(&Value::Object(o)))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let read = grants_now(&db);
    let codex = read.iter().find(|g| g.id == "codex").unwrap();
    assert_eq!(codex.tokens, 1, "live tokens only, at now = 10");
    assert_eq!(
        codex.last_used_at,
        Some(30),
        "the last token issued, live or not"
    );
    assert_eq!(
        read.iter().find(|g| g.id == "gpt").unwrap().tokens,
        1,
        "proxy aside"
    );
    assert_eq!(
        read.iter()
            .find(|g| g.id == "plan")
            .unwrap()
            .service_key
            .as_deref(),
        Some("ai-agent:planner")
    );
    let apps = group(&read);
    let mine = app_with(&apps, "gpt").unwrap();
    assert_eq!(mine["label"], "OpenAI");
    assert_eq!(mine["person"], "w");
    let going = still_members(mine, &["gpt".into(), "codex".into(), "sam_gpt".into()]);
    assert_eq!(
        going,
        ["codex", "gpt"],
        "Sam's ChatGPT is his own grant, not Willow's"
    );
    for id in &going {
        for q in DISCONNECT_GRANT {
            db.execute(q, [id]).unwrap();
        }
    }
    let ids = |sql: &str| -> Vec<String> {
        let mut q = db.prepare(sql).unwrap();
        q.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        ids("SELECT service_user_id FROM links ORDER BY 1"),
        ["plan", "sam_gpt"]
    );
    assert_eq!(
        ids("SELECT hash FROM oauth_tokens ORDER BY 1"),
        ["t3", "t4"],
        "a person's proxy token stays"
    );
    assert!(ids("SELECT id FROM apns_devices").is_empty());
    assert_eq!(
        ids("SELECT id FROM oauth_clients ORDER BY 1"),
        ["c1", "c2", "c3", "c4"],
        "clients stay"
    );
    let after = group(&grants_now(&db));
    assert_eq!(
        app_with(&after, "gpt").unwrap()["person"],
        Value::Null,
        "now acting for no one"
    );

    // The clean-up plan: what it may name, and its digest moving when a grant changes.
    let read = grants_now(&db);
    assert!(
        d::steps(&json!({}), &read)
            .unwrap_err()
            .contains("changes nothing")
    );
    assert!(d::steps(&json!({ "remove": ["nobody"] }), &read).is_err());
    assert!(d::steps(&json!({ "assign": [{ "id": "x", "app": "nope" }] }), &read).is_err());
    assert!(
        d::steps(
            &json!({ "remove": ["x"], "assign": [{ "id": "x", "app": "own" }] }),
            &read
        )
        .is_err()
    );
    let steps = d::steps(
        &json!({ "remove": ["x", "x"], "assign": [{ "id": "plan", "app": "ai-gateway" }] }),
        &read,
    )
    .unwrap();
    assert_eq!(
        steps,
        [
            d::Step::Remove("x".into()),
            d::Step::Assign("plan".into(), "ai-gateway".into())
        ]
    );
    let lines = d::plan_lines(&steps, &read, &|id| {
        if id == "w" {
            "Willow".into()
        } else {
            id.into()
        }
    });
    assert_eq!(
        lines,
        [
            "Remove x (OAuth app, x), acting for no one, with its 0 live tokens.",
            "Show Pigeon planner (plan) under AI Gateway.",
        ]
    );
    let before = d::payload(&steps, &read);
    let mut relinked = read.clone();
    relinked.iter_mut().find(|g| g.id == "x").unwrap().person = Some("w".into());
    assert_ne!(
        before,
        d::payload(&steps, &relinked),
        "an approval no longer matches"
    );
    let suggested = d::suggest(&read);
    let names: Vec<&str> = suggested["remove"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["gpt", "codex", "x"], "those acting for no one");
    // Applying it: removing a grant as removing a user does, and an override.
    for q in REMOVE_USER {
        db.execute(
            q,
            rusqlite::params_from_iter(vec!["x"; q.matches('?').count()]),
        )
        .unwrap();
    }
    db.execute(d::ASSIGN, rusqlite::params!["plan", "ai-gateway", 50, "w"])
        .unwrap();
    assert!(ids("SELECT id FROM users WHERE id = 'x'").is_empty());
    assert_eq!(
        ids("SELECT id FROM oauth_clients ORDER BY 1"),
        ["c1", "c2", "c3"]
    );
    assert_eq!(
        grants_now(&db)
            .iter()
            .find(|g| g.id == "plan")
            .unwrap()
            .app_override
            .as_deref(),
        Some("ai-gateway")
    );

    // Removing a person still clears everything acting for them, through the same grants; a
    // removed service user takes its override with it.
    for q in REMOVE_USER {
        db.execute(
            q,
            rusqlite::params_from_iter(vec!["w"; q.matches('?').count()]),
        )
        .unwrap();
    }
    assert_eq!(ids("SELECT service_user_id FROM links"), ["sam_gpt"]);
    for q in REMOVE_USER {
        db.execute(
            q,
            rusqlite::params_from_iter(vec!["plan"; q.matches('?').count()]),
        )
        .unwrap();
    }
    assert!(ids("SELECT service_user_id FROM app_assignments").is_empty());
    // The removal dialog names the apps rolled up.
    let lines = removal_lines(&json!({
        "user": { "name": "Willow" }, "passkeys": 1, "sessions": 0, "tokens": 0, "roles": [],
        "apps": [{ "name": "Pigeon for iOS" }, { "name": "Pigeon for iOS" }],
        "app_groups": [
            { "label": "AI Gateway", "clients": 2, "roles": 2 },
            { "label": "Pigeon for iOS", "clients": 7, "roles": 0 },
            { "label": "Claude", "clients": 1, "roles": 0 },
        ],
    }));
    assert!(lines.contains(&"Disconnects 3 apps acting for them, revoking their tokens: AI Gateway (2 roles), Pigeon for iOS (7 clients), Claude.".to_string()), "{lines:?}");

    // Access › People's data: the fixture's apps roll up into its app groups, one per real
    // app and person, and every client in a group is one of its `apps` rows.
    let fixture: Value = serde_json::from_str(include_str!(
        "../../src/capabilities/src/console/web/fixtures/app_people.json"
    ))
    .unwrap();
    let groups = fixture["app_groups"].as_array().unwrap();
    for a in fixture["apps"].as_array().unwrap() {
        let g = app_with(groups, a["id"].as_str().unwrap()).unwrap();
        assert_eq!(g["app"], a["app"], "{a}");
        assert_eq!(g["person"], a["person"], "{a}");
    }
    for g in groups {
        assert_eq!(
            g["clients"].as_u64().unwrap() as usize,
            g["members"].as_array().unwrap().len()
        );
    }
    // And its view: apps in each person's row, the app's clients inside it, those acting for
    // no one at the bottom for an admin, and no Apps page of their own.
    let view = include_str!("../../src/capabilities/src/console/web/views/access.js");
    for needle in [
        "action: \"app-group-disconnect\"",
        "change: { members: ids(a) }",
        "action: \"app-disconnect\"",
        "appsTable(ctx, p.apps",
        "Apps acting for no one",
        "meAdmin && none.length",
        "String(p.apps.length)",
    ] {
        assert!(view.contains(needle), "access.js: {needle}");
    }
    assert!(!view.contains("register(\"access\", \"apps\""));
    let shell = include_str!("../../src/capabilities/src/console/web/shell.js");
    assert!(shell.contains("\"app-group-disconnect\""));
}

/// The role column's migration makes the platform's first person (the oldest standard user)
/// its first admin, and no one else: not a later person, an older agent or a site visitor.
#[test]
fn console_first_admin_migration() {
    use pigeon_capabilities::users::{FIRST_ADMIN, ROLE_COLUMN};
    use rusqlite::Connection;

    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE users (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL)",
    )
    .unwrap();
    for (id, kind, at) in [
        ("agent", "service", 1),
        ("visitor", "site:notes.example.org", 2),
        ("willow", "standard", 10),
        ("sam", "standard", 20),
    ] {
        db.execute(
            "INSERT INTO users (id, kind, name, created_at) VALUES (?1, ?2, ?1, ?3)",
            rusqlite::params![id, kind, at],
        )
        .unwrap();
    }
    db.execute_batch(ROLE_COLUMN).unwrap();
    db.execute_batch(FIRST_ADMIN).unwrap();
    let mut q = db
        .prepare("SELECT id FROM users WHERE role = 'admin'")
        .unwrap();
    let admins: Vec<String> = q
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(admins, ["willow"]);

    // On a platform with no one yet it changes nothing; `users_bootstrap` makes the first admin.
    let empty = Connection::open_in_memory().unwrap();
    empty
        .execute_batch(
            "CREATE TABLE users (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL)",
        )
        .unwrap();
    empty.execute_batch(ROLE_COLUMN).unwrap();
    empty.execute_batch(FIRST_ADMIN).unwrap();
}

/// Per-instance membership's migration (docs/plans/membership.md): every person who exists is
/// owner of every instance in the index, so nobody loses access on deploy; agents and site
/// visitors get nothing, and the home instance's old id (R1) is kept under its new one.
#[test]
fn console_membership_migration_grants_owner() {
    use pigeon_capabilities::membership::{MEMBERS_SEED, MEMBERS_TABLE};
    use rusqlite::Connection;

    let open = |index: Option<&str>| {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE users (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, expires_at INTEGER NOT NULL);",
        )
        .unwrap();
        for (id, kind) in [
            ("willow", "standard"),
            ("sam", "standard"),
            ("agent", "service"),
            ("visitor", "site:notes.example.org"),
        ] {
            db.execute(
                "INSERT INTO users (id, kind, name, created_at) VALUES (?1, ?2, ?1, 1)",
                rusqlite::params![id, kind],
            )
            .unwrap();
        }
        if let Some(index) = index {
            db.execute(
                "INSERT INTO settings (key, value, expires_at) VALUES ('instances:index', ?1, 9)",
                [index],
            )
            .unwrap();
        }
        db.execute_batch(MEMBERS_TABLE).unwrap();
        db.execute_batch(MEMBERS_SEED).unwrap();
        let mut q = db
            .prepare("SELECT instance, person_id, role FROM members ORDER BY instance, person_id")
            .unwrap();
        q.query_map([], |r| {
            Ok(format!(
                "{} {} {}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<String>>()
    };
    assert_eq!(
        open(Some(
            r#"["noisypigeon","pigeon.dev","sandbox","willowgraysen"]"#
        )),
        [
            "noisypigeon sam owner",
            "noisypigeon willow owner",
            "pigeon.dev sam owner",
            "pigeon.dev willow owner",
            "sandbox sam owner",
            "sandbox willow owner",
            "willowgraysen sam owner",
            "willowgraysen willow owner",
        ]
    );
    // Before R1 has run, home's old id is kept under its new one, as every lookup keys it.
    assert_eq!(
        open(Some(r#"["pigeon-security/platform"]"#)),
        ["pigeon.dev sam owner", "pigeon.dev willow owner"]
    );
    assert_eq!(
        pigeon_capabilities::membership::key("pigeon-security/platform"),
        "pigeon.dev"
    );
    // A platform with no instances yet gets no rows.
    assert!(open(None).is_empty());
}

/// Who may do what on an instance: a viewer reads but neither writes nor approves; an operator
/// writes and approves; an owner also manages members; a platform admin is owner everywhere.
/// Console writes and approvals ask `write_need`; every tool from outside asks `tool_need`
/// (a console read with its read arguments is a read, anything else a write). A platform-wide
/// request is about the home instance. Service users and apps act as the person they act for.
#[test]
fn console_membership_roles() {
    use pigeon_capabilities::console::ops::{app_read_instance, with_roles};
    use pigeon_capabilities::membership::{
        Access, LAST_OWNER, Need, Role, member_change, parse_setting, tool_need, write_need,
    };
    use pigeon_capabilities::users::{Caller, User, person_of};

    let instances = ["noisypigeon", "pigeon.dev", "sandbox", "willowgraysen"];
    let access = |person: &str, admin: bool, roles: &[(&str, Role)]| Access {
        person: person.into(),
        admin,
        roles: roles.iter().map(|(i, r)| (i.to_string(), *r)).collect(),
        home: Some("pigeon.dev".into()),
        instances: instances.iter().map(|i| i.to_string()).collect(),
    };
    let viewer = access("sam", false, &[("noisypigeon", Role::Viewer)]);
    let operator = access(
        "kai",
        false,
        &[
            ("noisypigeon", Role::Operator),
            ("pigeon.dev", Role::Viewer),
        ],
    );
    let admin = access("willow", true, &[]);

    // A viewer reads, and neither writes nor approves there; nothing at all elsewhere.
    assert_eq!(viewer.check(Some("noisypigeon"), Role::Viewer), Ok(()));
    let refused = viewer
        .check(Some("noisypigeon"), Role::Operator)
        .unwrap_err();
    assert!(
        refused.contains("operator or above on instance noisypigeon"),
        "{refused}"
    );
    assert!(refused.contains("a viewer"), "{refused}");
    let none = viewer.check(Some("sandbox"), Role::Viewer).unwrap_err();
    assert!(none.contains("not a member"), "{none}");
    // A platform-wide request (no instance, or one not made yet) is about home.
    assert!(viewer.check(None, Role::Viewer).is_err());
    assert!(viewer.check(Some("brand-new"), Role::Viewer).is_err());
    // An operator writes and approves there; reads home, writes nothing platform-wide.
    assert_eq!(operator.check(Some("noisypigeon"), Role::Operator), Ok(()));
    assert_eq!(operator.check(None, Role::Viewer), Ok(()));
    assert!(operator.check(None, Role::Operator).is_err());
    // An admin is owner everywhere, home and new instances included, with no rows.
    for i in [
        None,
        Some("sandbox"),
        Some("brand-new"),
        Some("pigeon-security/platform"),
    ] {
        assert_eq!(admin.check(i, Role::Owner), Ok(()), "{i:?}");
    }
    assert_eq!(admin.role("willowgraysen"), Some(Role::Owner));
    // Before R1, home's old id names its membership too.
    assert_eq!(
        operator.role("pigeon-security/platform"),
        Some(Role::Viewer)
    );

    // Members: an owner or an admin manages them; an operator does not.
    let owner = access("rowan", false, &[("sandbox", Role::Owner)]);
    assert_eq!(owner.may_manage("sandbox"), Ok(()));
    assert!(owner.may_manage("noisypigeon").is_err());
    assert!(operator.may_manage("noisypigeon").is_err());
    assert_eq!(admin.may_manage("noisypigeon"), Ok(()));
    assert!(
        admin
            .may_manage("nowhere")
            .unwrap_err()
            .contains("no instance")
    );
    assert_eq!(parse_setting("none"), Ok(None));
    assert_eq!(parse_setting(" operator "), Ok(Some(Role::Operator)));
    assert!(parse_setting("admin").is_err());

    // The last owner stays one, unless the platform has an admin.
    let one = ["rowan".to_string()];
    let two = ["rowan".to_string(), "kai".to_string()];
    assert_eq!(
        member_change(&one, 0, "rowan", None).unwrap_err(),
        LAST_OWNER
    );
    assert_eq!(
        member_change(&one, 0, "rowan", Some(Role::Operator)).unwrap_err(),
        LAST_OWNER
    );
    assert_eq!(member_change(&one, 0, "rowan", Some(Role::Owner)), Ok(()));
    assert_eq!(
        member_change(&one, 1, "rowan", None),
        Ok(()),
        "an admin exists"
    );
    assert_eq!(member_change(&two, 0, "rowan", Some(Role::Viewer)), Ok(()));
    assert_eq!(member_change(&one, 0, "sam", None), Ok(()), "not an owner");

    // Console writes: operator on the instance they target; a site's instance; home for a
    // platform-wide one; an approval's own instance; the `users` writes ask their own rules.
    let need = |id: &str, target: Value| write_need(action(id).unwrap(), &target);
    for id in [
        "instance-put",
        "site-deploy",
        "site-rollback",
        "linked-unlink",
    ] {
        assert_eq!(
            need(id, json!("noisypigeon")),
            Need::On(Some("noisypigeon".into()), Role::Operator),
            "{id}"
        );
    }
    let site = json!({ "instance": "noisypigeon", "site": "blog", "host": "noisypigeon.com" });
    for id in ["visitor-invite", "visitor-revoke"] {
        assert_eq!(
            need(id, site.clone()),
            Need::On(Some("noisypigeon".into()), Role::Operator)
        );
    }
    for id in [
        "secret-set",
        "token-issue",
        "land-approve",
        "model-defaults",
        "run-stop",
    ] {
        assert_eq!(need(id, json!("x")), Need::On(None, Role::Operator), "{id}");
    }
    for id in ["approval-approve", "approval-deny"] {
        assert_eq!(need(id, json!("a1")), Need::Approval);
    }
    for id in [
        "passkey-remove",
        "invite",
        "person-admin",
        "member-set",
        "app-disconnect",
        "app-group-disconnect",
        "user-remove",
    ] {
        assert_eq!(need(id, json!("x")), Need::Own, "{id}");
    }

    // Tools from outside: a read with its read arguments needs viewer; with more, or any other
    // tool, operator; people, passkeys, apps, approvals and the switcher ask their own rules.
    assert_eq!(
        tool_need(
            "sites",
            "site_releases",
            &json!({ "instance": "noisypigeon" })
        ),
        Some(Role::Viewer)
    );
    assert_eq!(
        tool_need(
            "sites",
            "site_deploy",
            &json!({ "instance": "noisypigeon" })
        ),
        Some(Role::Operator)
    );
    assert_eq!(
        tool_need("ai_agents", "ai_agents_config", &json!({})),
        Some(Role::Viewer)
    );
    assert_eq!(
        tool_need(
            "ai_agents",
            "ai_agents_config",
            &json!({ "max_live_runs": 2 })
        ),
        Some(Role::Operator),
        "a read that also sets is a write"
    );
    assert_eq!(
        tool_need("land", "land", &json!({ "branch": "claude/x" })),
        Some(Role::Operator)
    );
    for (cap, tool) in [
        ("users", "instance_member_set"),
        ("users", "users_whoami"),
        ("approvals", "approvals_list"),
        ("passkeys", "passkeys_remove"),
        ("dispatch", "call"),
        ("instances", "instances_list"),
        ("instances", "console_manifest"),
    ] {
        assert_eq!(tool_need(cap, tool, &json!({})), None, "{tool}");
    }
    // The console's own reads: viewer on their instance, or home; the whole inbox filters.
    assert_eq!(
        app_read_instance("app_site_hosts", &json!({ "instance": "noisypigeon" })),
        Some(Some("noisypigeon"))
    );
    assert_eq!(app_read_instance("app_people", &json!({})), Some(None));
    assert_eq!(app_read_instance("app_inbox", &json!({})), None);
    assert_eq!(
        app_read_instance("app_inbox", &json!({ "instance": "sandbox" })),
        Some(Some("sandbox"))
    );
    assert_eq!(
        app_read_instance("site_releases", &json!({ "instance": "x" })),
        None
    );

    // The switcher shows only the person's instances; an admin sees all.
    assert_eq!(viewer.visible(instances), ["noisypigeon"]);
    assert_eq!(operator.visible(instances), ["noisypigeon", "pigeon.dev"]);
    assert_eq!(admin.visible(instances), instances);
    // Access › People: each person's roles on the instances the one asking may see.
    let people = vec![
        json!({ "id": "willow", "admin": true }),
        json!({ "id": "kai", "admin": false }),
        json!({ "id": "sam", "admin": false }),
    ];
    let rows = [
        json!({ "instance": "noisypigeon", "person_id": "kai", "role": "operator" }),
        json!({ "instance": "pigeon.dev", "person_id": "kai", "role": "viewer" }),
        json!({ "instance": "noisypigeon", "person_id": "sam", "role": "viewer" }),
        json!({ "instance": "sandbox", "person_id": "sam", "role": "owner" }),
    ];
    let (seen, people) = with_roles(&viewer, people, &rows);
    assert_eq!(seen, ["noisypigeon"]);
    assert_eq!(
        people[0]["roles"],
        json!({ "noisypigeon": "owner" }),
        "an admin"
    );
    assert_eq!(people[1]["roles"], json!({ "noisypigeon": "operator" }));
    assert_eq!(
        people[2]["roles"],
        json!({ "noisypigeon": "viewer" }),
        "sandbox is not shown to a viewer of noisypigeon only"
    );

    // member-set: the console's only, bound to the instance, the person and the role.
    let a = action("member-set").unwrap();
    assert!(!a.by_bearer);
    assert_eq!(a.handler, Handler::Capability("users"));
    assert_eq!(
        a.change(&json!({ "person": "u_sam", "role": "viewer" }), Step::Begin),
        Ok(json!({ "person": "u_sam", "role": "viewer" }))
    );
    assert!(
        a.change(&json!({ "person": "u_sam", "role": "admin" }), Step::Begin)
            .is_err()
    );
    assert!(a.change(&json!({ "role": "viewer" }), Step::Begin).is_err());
    assert_ne!(
        bound_purpose(
            "member-set",
            "willow",
            json!("noisypigeon"),
            json!({ "person": "u_sam", "role": "viewer" }),
            Step::Begin
        ),
        bound_purpose(
            "member-set",
            "willow",
            json!("noisypigeon"),
            json!({ "person": "u_sam", "role": "owner" }),
            Step::Begin
        ),
    );

    // An agent's instance_member_set, waiting for the person's passkey, in plain words.
    use pigeon_capabilities::approvals::plain_call;
    assert_eq!(
        plain_call(
            "instance_member_set",
            &json!({ "instance": "sandbox", "person": "u_sam", "role": "operator" }),
            &Value::Null
        ),
        "Make u_sam operator on instance sandbox"
    );
    assert_eq!(
        plain_call(
            "instance_member_set",
            &json!({ "instance": "sandbox", "person": "u_sam", "role": "none" }),
            &Value::Null
        ),
        "Remove u_sam from instance sandbox"
    );

    // Acting for a person: an app or agent is checked as the person it acts for, by bearer or
    // MCP alike; one acting for no one is not a person, so membership does not ask about it
    // (the platform's own automation, as before).
    let user = |id: &str, kind: &str| User {
        id: id.into(),
        kind: kind.into(),
        name: id.into(),
        created_at: 0,
    };
    let app = Caller {
        user: user("ios", "service"),
        acting_for: Some(user("sam", "standard")),
        run: None,
    };
    let person = person_of(&app).unwrap();
    assert_eq!(person.id, viewer.person);
    assert!(viewer.check(Some("noisypigeon"), Role::Operator).is_err());
    let agent = Caller {
        user: user("agent", "service"),
        acting_for: Some(user("kai", "standard")),
        run: Some("r1".into()),
    };
    assert_eq!(person_of(&agent).unwrap().id, operator.person);
    let loose = Caller {
        user: user("lander", "service"),
        acting_for: None,
        run: None,
    };
    assert!(person_of(&loose).is_none());
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

    let audit_purpose = |intent, step| {
        bound_purpose(
            "instance-put",
            "u1",
            json!("blog"),
            json!({ "record": record, "jobs_audit": intent }),
            step,
        )
    };
    let create = audit_purpose("create", Step::Begin).unwrap();
    assert_eq!(create, audit_purpose("create", Step::Finish).unwrap());
    assert_ne!(create, audit_purpose("reuse", Step::Finish).unwrap());
    assert_ne!(create, a);
    assert!(audit_purpose("automatic", Step::Begin).is_err());

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
        "app_jobs_audit_plan",
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
    // Platform › Architecture reads the selected instance's own (capability scoping, M1).
    assert!(allowed(
        "architecture_describe",
        &json!({ "svg": true, "instance": "blog" })
    ));
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

/// Every hosted capability's console views, each with the capability that declares it, as
/// `console_manifest` collects them from the platform Worker: core's own, and the AI Agents and
/// model gateway Workers', which core reads from their manifests as `remote.rs` does.
pub(super) fn hosted_views() -> Vec<(
    &'static str,
    pigeon_capabilities::console::sections::Section,
)> {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::console::sections::{Section, area_named};
    use pigeon_contract::Service;

    let caps: Vec<Box<dyn Capability>> = vec![
        Box::new(pigeon_capabilities::architecture::Architecture),
        Box::new(pigeon_capabilities::users::Users { host: "h".into() }),
        Box::new(pigeon_capabilities::secrets::Secrets {
            host: "h".into(),
            parent: "p".into(),
        }),
        Box::new(pigeon_capabilities::proxy::Tokens {
            proxy_host: "p".into(),
            backup: None,
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
    ];
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let mut platform: Vec<(&'static str, Section)> = caps
        .iter()
        .flat_map(|c| c.sections().into_iter().map(|s| (c.name(), s)))
        .chain(
            pigeon_capabilities::instances::multi_instance_sections()
                .into_iter()
                .map(|s| ("multi-instance", s)),
        )
        .collect();
    // Models: the model gateway's capability, as its manifest carries it.
    let models = pigeon_models::Models {
        gateways: Vec::new(),
        providers: Vec::new(),
        staged: Vec::new(),
        secrets_page: "https://x/secrets".into(),
        host: "h".into(),
        proxy_host: "p".into(),
    };
    let remote = pigeon_ai_agents::service::Agents.manifest().capabilities;
    for cap in remote.into_iter().chain([models.capability()]) {
        let name = leak(&cap.name);
        for s in cap.sections.into_iter().filter(|s| s.view) {
            let area = area_named(&s.tab).unwrap().id;
            platform.push((
                name,
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
    platform
}

#[test]
fn jobs_view_and_reads_are_opt_in_per_instance() {
    use pigeon_capabilities::digitalocean::health::{JOB, JOBS};
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};

    let mut raw = json!({"id":"linked", "title":"Linked", "account":{"kind":"linked"}});
    let scope_of = |raw: &serde_json::Value| {
        let record = serde_json::from_value::<Instance>(raw.clone()).unwrap();
        let scope = Scope::new(&record).unwrap();
        (record, scope)
    };

    let (_, scope) = scope_of(&raw);
    assert!(scope.bind(JOBS, &json!({})).is_err());

    raw["capabilities"] = json!({"jobs": {}});
    let (record, scope) = scope_of(&raw);
    assert_eq!(
        scope.bind(JOBS, &json!({})).unwrap(),
        json!({"instance":"linked"})
    );
    assert_eq!(
        scope.bind(JOB, &json!({"id":"older"})).unwrap()["id"],
        "older"
    );
    assert!(
        scope
            .bind(JOB, &json!({"id":"older", "instance":"other"}))
            .is_err()
    );
    assert!(scope.bind(JOBS, &json!({"id":"older"})).is_err());

    let value = manifest(&record, &scope, &[], &[], None);
    let area = value["areas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|area| area["id"] == "jobs")
        .unwrap();
    assert_eq!(area["views"][0]["id"], "runs");
    assert_eq!(area["views"][0]["capability"], "jobs");
}

/// Every area and view `console_manifest` can send: its answer for a home instance with every
/// instance capability enabled, given every hosted capability's views ([`hosted_views`]).
fn console_every_view() -> std::collections::BTreeMap<String, Vec<String>> {
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};

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
            "destination_bucket": "all-site" },
        "capabilities": { "jobs": {}, "data": { "tier": "lite" }, "email": { "addresses": [{ "local": "a" }] } }
    }))
    .unwrap();
    let platform = hosted_views();
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

#[test]
fn data_ingest_scope_requires_data_and_uses_the_approved_spaces_prefix() {
    use pigeon_capabilities::{catalogue, ingest, instances::tools::Scope};

    let mut record: pigeon_capabilities::instances::record::Instance =
        serde_json::from_value(json!({
            "id": "pigeon.dev",
            "title": "pigeon",
            "account": { "kind": "home" },
            "capabilities": { "data-ingest": {} }
        }))
        .unwrap();
    assert!(
        catalogue::problems(&record)
            .iter()
            .any(|problem| problem.contains("needs \"data\""))
    );
    assert!(
        Scope::new(&record)
            .unwrap()
            .bind("ingest_plan", &json!({}))
            .is_err()
    );

    record
        .capabilities
        .insert("data".into(), Default::default());
    assert!(catalogue::problems(&record).is_empty());
    let scope = Scope::new(&record).unwrap();
    assert!(scope.capabilities().contains(&ingest::NAME));
    let args = json!({ "prefix": "approved/canary/" });
    assert!(scope.bind("ingest_plan", &args).is_ok());
    assert!(
        scope
            .bind("ingest_plan", &json!({"bucket": "other"}))
            .is_err()
    );
    assert!(
        scope
            .bind("ingest_start", &json!({"plan": "approval"}))
            .is_err()
    );
    for tool in ["ingest_status", "ingest_report", "ingest_stop"] {
        assert!(scope.bind(tool, &json!({"run": "legacy"})).is_ok());
    }
    assert_eq!(
        ingest::spaces_prefix("approved/", &json!({})).unwrap(),
        "approved/"
    );
    assert_eq!(
        ingest::spaces_prefix("approved/", &args).unwrap(),
        "approved/canary/"
    );
    assert!(ingest::spaces_prefix("approved/", &json!({"prefix": "approved-other/"})).is_err());
    assert!(ingest::spaces_prefix("approved/", &json!({"prefix": "approved/\n"})).is_err());
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

    // The recorded manifests: areas as AREAS declares them, Inbox first, home-only
    // capabilities' views only on home, and only views the manifest can send.
    use pigeon_capabilities::catalogue;
    let hosted: Vec<&str> = hosted_views().into_iter().map(|(c, _)| c).collect();
    let capabilities: Vec<&str> = hosted
        .iter()
        .copied()
        .chain(
            pigeon_capabilities::instances::tools::CAPABILITIES
                .iter()
                .map(|c| c.name),
        )
        .collect();
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
            for v in a["views"].as_array().unwrap() {
                let view = v["id"].as_str().unwrap();
                assert!(
                    every[id].iter().any(|w| w == view),
                    "{name}: {id}/{view} is not sent"
                );
                // Each view names the capability that declares it, and a home-only one shows
                // on home alone (docs/plans/capability-scoping.md).
                let cap = v["capability"].as_str().unwrap();
                assert!(
                    capabilities.contains(&cap),
                    "{name}: {id}/{view} names an unknown capability {cap}"
                );
                let home_only = catalogue::find(cap)
                    .is_none_or(|e| e.availability == catalogue::Availability::HomeOnly);
                assert!(
                    home || !home_only || !hosted.contains(&cap),
                    "{name}: {id}/{view} is home-only"
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

    // The sign-in mark: the apex pages' artwork, served from this origin (the policy allows no
    // other) at the path the stylesheet names relative to itself, and found by fixture mode
    // beside shell.css through a link to the same file.
    assert_eq!(page::MARK, format!("{}/mark/head.webp", page::BASE));
    assert!(
        css.contains("url(\"mark/head.webp\")"),
        "shell.css draws the mark"
    );
    assert_eq!(Get::route(page::MARK, None), Some(Get::Mark));
    assert!(!page::is_page(page::MARK));
    for near in ["/app/mark", "/app/mark/", "/app/mark/head.png"] {
        assert_ne!(Get::route(near, None), Some(Get::Mark), "GET {near}");
    }
    let fixture = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/console/web/mark/head.webp"
    ))
    .unwrap();
    assert_eq!(
        fixture,
        pigeon_core::home::Asset::MarkHead.bytes(),
        "fixture mode's mark is the served one"
    );
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
            proxy_host: "p".into(),
            backup: None,
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
            ("access", Some("people")),
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

/// An instance id with a `/`, such as the home instance `pigeon-security/platform`, stays one
/// path segment in the console URLs the server redirects to, as the shell encodes it.
#[test]
fn console_urls_encode_an_instance_id_with_a_slash() {
    use pigeon_capabilities::console::page;
    assert_eq!(
        page::url("pigeon-security/platform", "inbox", None),
        "/app/pigeon-security%2Fplatform/inbox"
    );
    assert_eq!(
        page::url("sandbox", "site", Some("deployments")),
        "/app/sandbox/site/deployments"
    );
    assert_eq!(page::segment("a b/c?d"), "a%20b%2Fc%3Fd");
}

/// Email and the lite data store (docs/plans/email.md): the data Worker's handler on a
/// stand-in (R2 as a map, D1 as SQLite, failures on demand), its read queries, the plan steps
/// with their DNS review, the record's rules, the tools' roles and the Email tab's fixtures.
mod email {
    use super::*;

    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use crate::data_worker_mail as mail;
    use pigeon_capabilities::instances::record::Instance;
    use pigeon_ingest::datastore::{self, EmailConfig, ForwardScope, Listed};
    use pigeon_ingest::index::Statement;

    /// Runs a future whose stand-ins answer at once.
    fn ready<F: std::future::Future>(f: F) -> F::Output {
        use std::task::{Context, Poll, Waker};
        let mut f = std::pin::pin!(f);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    fn sql_value(v: &Value) -> rusqlite::types::Value {
        use rusqlite::types::Value as S;
        match v {
            Value::Null => S::Null,
            Value::Bool(b) => S::Integer(i64::from(*b)),
            Value::Number(n) => n
                .as_i64()
                .map(S::Integer)
                .unwrap_or_else(|| S::Real(n.as_f64().unwrap_or(0.0))),
            Value::String(s) => S::Text(s.clone()),
            other => S::Text(other.to_string()),
        }
    }

    /// The data store with its bindings stood in: R2 a map, D1 an in-memory SQLite with the lite
    /// migrations applied; `fail_put` and `fail_db` make writes fail.
    struct Store {
        db: RefCell<rusqlite::Connection>,
        blobs: RefCell<BTreeMap<String, Vec<u8>>>,
        fail_put: Cell<bool>,
        fail_db: Cell<bool>,
    }

    impl Store {
        fn new(config: Option<&EmailConfig>) -> Store {
            let db = rusqlite::Connection::open_in_memory().unwrap();
            db.execute(datastore::MIGRATIONS_TABLE, []).unwrap();
            let store = Store {
                db: RefCell::new(db),
                blobs: RefCell::new(BTreeMap::new()),
                fail_put: Cell::new(false),
                fail_db: Cell::new(false),
            };
            for m in datastore::MIGRATIONS {
                store.exec(datastore::migration_statements(m, "2026-11-26T00:00:00Z"));
            }
            if let Some(c) = config {
                store.exec(vec![datastore::config_statement(c, "2026-11-26T00:00:00Z")]);
            }
            store
        }

        fn exec(&self, statements: Vec<Statement>) {
            let mut db = self.db.borrow_mut();
            let tx = db.transaction().unwrap();
            for s in statements {
                let params: Vec<rusqlite::types::Value> = s.params.iter().map(sql_value).collect();
                tx.execute(&s.sql, rusqlite::params_from_iter(params))
                    .unwrap_or_else(|e| panic!("{}: {e}", s.sql));
            }
            tx.commit().unwrap();
        }

        fn rows(&self, s: &Statement) -> Vec<Value> {
            let db = self.db.borrow();
            let mut q = db
                .prepare(&s.sql)
                .unwrap_or_else(|e| panic!("{}: {e}", s.sql));
            let names: Vec<String> = q.column_names().iter().map(|n| n.to_string()).collect();
            let params: Vec<rusqlite::types::Value> = s.params.iter().map(sql_value).collect();
            q.query_map(rusqlite::params_from_iter(params), |row| {
                let mut out = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    use rusqlite::types::ValueRef;
                    let v = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(f) => json!(f),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(_) => Value::Null,
                    };
                    out.insert(name.clone(), v);
                }
                Ok(Value::Object(out))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        }

        fn count(&self, sql: &str) -> i64 {
            self.db.borrow().query_row(sql, [], |r| r.get(0)).unwrap()
        }
    }

    impl mail::Bindings for Store {
        async fn has(&self, key: &str) -> Result<bool, String> {
            Ok(self.blobs.borrow().contains_key(key))
        }

        async fn put(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<(), String> {
            if self.fail_put.get() {
                return Err("R2 put failed".into());
            }
            // R2 checks the SHA-256 it is given.
            assert_eq!(datastore::sha256_hex(bytes), sha256, "{key}");
            self.blobs.borrow_mut().insert(key.into(), bytes.to_vec());
            Ok(())
        }

        async fn run(&self, statements: Vec<Statement>) -> Result<(), String> {
            if self.fail_db.get() {
                return Err("D1 batch failed".into());
            }
            self.exec(statements);
            Ok(())
        }

        async fn config(&self) -> Result<Option<EmailConfig>, String> {
            Ok(self
                .rows(&datastore::config_query())
                .first()
                .and_then(|r| serde_json::from_str(r["value"].as_str()?).ok()))
        }
    }

    /// pigeon.dev as the person set it up: the three listed addresses, cloudflare forwarded.
    fn pigeon_config() -> EmailConfig {
        EmailConfig {
            instance: "pigeon.dev".into(),
            domain: "pigeon.dev".into(),
            addresses: ["cloudflare", "buildkite", "apple"]
                .iter()
                .map(|l| Listed {
                    local: l.to_string(),
                    forward: (*l == "cloudflare").then(|| "person@example.com".to_string()),
                })
                .collect(),
            scope: ForwardScope::Listed,
            forward_to: None,
        }
    }

    /// A message with a trusted Authentication-Results (Cloudflare's, first) and a forged one, an
    /// HTML part and a PDF attachment.
    fn message(subject: &str, from: &str) -> Vec<u8> {
        format!(
            "Authentication-Results: mx.cloudflare.net; spf=pass smtp.mailfrom={from}; \
             dkim=pass header.d=example.com; dmarc=pass header.from=example.com\r\n\
             Authentication-Results: mx.cloudflare.net.evil.example; dmarc=fail\r\n\
             From: Sender <{from}>\r\n\
             To: cloudflare@pigeon.dev\r\n\
             Subject: {subject}\r\n\
             Date: Thu, 26 Nov 2026 09:00:00 +0000\r\n\
             Message-ID: <{subject}@example.com>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n\
             --b1\r\nContent-Type: multipart/alternative; boundary=\"b2\"\r\n\r\n\
             --b2\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello from {subject}.\r\n\
             --b2\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
             <p>Hello <img src=\"https://tracker.example/p.gif\"></p>\r\n--b2--\r\n\
             --b1\r\nContent-Type: application/pdf\r\n\
             Content-Disposition: attachment; filename=\"invoice.pdf\"\r\n\
             Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b1--\r\n"
        )
        .into_bytes()
    }

    fn incoming<'a>(raw: &'a [u8], to: &'a str, at: &str, marks: &[&str]) -> mail::Incoming<'a> {
        let mut h = mail::Hasher::default();
        for chunk in raw.chunks(7) {
            h.update(chunk);
        }
        assert_eq!(h.size(), raw.len() as u64);
        mail::Incoming {
            envelope_from: "sender@example.com",
            envelope_to: to,
            raw,
            content: h.finish(),
            loop_marks: marks.iter().map(|m| m.to_string()).collect(),
            received_at: at.into(),
        }
    }

    fn stored(v: mail::Verdict) -> mail::Stored {
        match v {
            mail::Verdict::Stored(s) => s,
            mail::Verdict::Reject(why) => panic!("rejected: {why}"),
        }
    }

    /// The handler on a stand-in: a listed address lands in its mailbox and anything else in
    /// catch-all; the bytes are stored once, by their SHA-256 hashed as they stream, with the
    /// record rows of ingest.md decision 3; a duplicate adds only a sighting; the trusted sender
    /// checks are recorded; a store failure rejects; forwarding is listed-only by default, `all`
    /// when set, and never loops; the log names no content.
    #[test]
    fn email_handler_stores_files_and_forwards() {
        let config = pigeon_config();
        let store = Store::new(Some(&config));
        let raw = message("first", "billing@example.com");

        // A listed address, its case and a +tag ignored.
        let s = stored(ready(mail::receive(
            &store,
            &incoming(
                &raw,
                "Cloudflare+billing@PIGEON.dev",
                "2026-11-26T09:00:01Z",
                &[],
            ),
        )));
        assert_eq!(s.mailbox, "cloudflare");
        assert!(!s.duplicate && !s.looped);
        assert_eq!(s.item_id, datastore::sha256_hex(&raw));
        assert_eq!(s.size, raw.len() as u64);
        assert_eq!(s.forward.as_deref(), Some("person@example.com"));
        let key = pigeon_ingest::blob_key(&s.item_id);
        assert_eq!(store.blobs.borrow()[&key], raw);
        assert!(
            store
                .blobs
                .borrow()
                .contains_key(&format!("derived/{}/text.txt", s.item_id))
        );
        // The PDF and the HTML are blobs of their own; the PDF an item whose parent is the message.
        assert_eq!(store.count("SELECT COUNT(*) FROM items"), 2);
        assert_eq!(
            store.count("SELECT COUNT(*) FROM items WHERE kind = 'email' AND status = 'stored'"),
            1
        );
        assert_eq!(
            store.count(&format!(
                "SELECT COUNT(*) FROM items WHERE parent_id = '{}' AND filename = 'invoice.pdf' \
                 AND kind = 'document'",
                s.item_id
            )),
            1
        );
        let sighting = &store.rows(&Statement {
            sql: "SELECT * FROM sightings".into(),
            params: vec![],
        })[0];
        assert_eq!(sighting["source"], "email_routing");
        assert_eq!(sighting["locator"], "cloudflare+billing@pigeon.dev");
        let extra: Value = serde_json::from_str(sighting["extra"].as_str().unwrap()).unwrap();
        // Cloudflare's own header, not the forged one that only looks like it.
        assert_eq!(
            extra,
            json!({ "mailbox": "cloudflare", "auth": { "spf": "pass", "dkim": "pass", "dmarc": "pass" } })
        );
        let email = &store.rows(&Statement {
            sql: "SELECT subject, \"from\", fingerprint, html_ref, text_ref FROM emails".into(),
            params: vec![],
        })[0];
        assert_eq!(email["subject"], "first");
        assert!(
            email["from"]
                .as_str()
                .unwrap()
                .contains("billing@example.com")
        );
        assert_eq!(email["fingerprint"].as_str().unwrap().len(), 64);
        assert!(store.blobs.borrow().contains_key(&pigeon_ingest::blob_key(
            email["html_ref"].as_str().unwrap()
        )));

        // An unknown address, and another domain, land in catch-all, which forwards nothing by
        // default.
        let other = message("second", "news@example.org");
        let s2 = stored(ready(mail::receive(
            &store,
            &incoming(&other, "nobody@pigeon.dev", "2026-11-26T09:00:02Z", &[]),
        )));
        assert_eq!(
            (s2.mailbox.as_str(), s2.forward.as_deref()),
            ("catch-all", None)
        );
        assert_eq!(config.mailbox_for("apple@another.example"), "catch-all");
        assert_eq!(config.mailbox_for("not an address"), "catch-all");

        // The same bytes again, to another address: no new item, no new email row, one more
        // sighting.
        let blobs_before = store.blobs.borrow().len();
        let s3 = stored(ready(mail::receive(
            &store,
            &incoming(&raw, "apple@pigeon.dev", "2026-11-26T09:00:03Z", &[]),
        )));
        assert!(s3.duplicate);
        assert_eq!(s3.mailbox, "apple");
        assert_eq!(s3.forward, None, "apple has no destination");
        assert_eq!(store.blobs.borrow().len(), blobs_before);
        // Three items: both messages and the PDF they both carry, stored once.
        assert_eq!(store.count("SELECT COUNT(*) FROM items"), 3);
        assert_eq!(store.count("SELECT COUNT(*) FROM emails"), 2);
        assert_eq!(store.count("SELECT COUNT(*) FROM sightings"), 3);

        // Forwarding scope `all` with a default destination forwards catch-all too.
        let all = EmailConfig {
            scope: ForwardScope::All,
            forward_to: Some("inbox@example.com".into()),
            ..config.clone()
        };
        assert_eq!(
            all.forward_for("catch-all").as_deref(),
            Some("inbox@example.com")
        );
        assert_eq!(
            all.forward_for("apple").as_deref(),
            Some("inbox@example.com")
        );
        assert_eq!(
            all.forward_for("cloudflare").as_deref(),
            Some("person@example.com")
        );
        assert_eq!(config.forward_for("catch-all"), None);
        assert_eq!(
            all.destinations(),
            ["inbox@example.com", "person@example.com"]
        );
        let all_store = Store::new(Some(&all));
        let s4 = stored(ready(mail::receive(
            &all_store,
            &incoming(&other, "x@pigeon.dev", "2026-11-26T09:00:04Z", &[]),
        )));
        assert_eq!(s4.forward.as_deref(), Some("inbox@example.com"));

        // A message that already passed through this instance's forward is stored, not forwarded.
        let back = message("looped", "billing@example.com");
        let s5 = stored(ready(mail::receive(
            &store,
            &incoming(
                &back,
                "cloudflare@pigeon.dev",
                "2026-11-26T09:00:05Z",
                &["pigeon.dev"],
            ),
        )));
        assert!(s5.looped && s5.forward.is_none());
        // Another instance's mark is not this one's.
        let s6 = stored(ready(mail::receive(
            &store,
            &incoming(
                &back,
                "cloudflare@pigeon.dev",
                "2026-11-26T09:00:06Z",
                &["noisypigeon"],
            ),
        )));
        assert!(!s6.looped && s6.forward.is_some());

        // What became of each is recorded; a failed forward too, with a reason and no content.
        store.exec(vec![
            mail::delivery_row(
                &s,
                "cloudflare@pigeon.dev",
                "2026-11-26T09:00:07Z",
                "forward_failed",
                Some("Email Routing refused the forward"),
            ),
            mail::delivery_row(
                &s5,
                "cloudflare@pigeon.dev",
                "2026-11-26T09:00:08Z",
                "loop",
                None,
            ),
        ]);
        let q = datastore::status_queries();
        let outcomes = store.rows(&q[1]);
        assert_eq!(outcomes[0]["outcome"], "forward_failed");
        assert_eq!(outcomes[1]["outcome"], "loop");
        let line = mail::log_line(&s.item_id, s.size, &s.mailbox, "forwarded");
        assert!(!line.contains("billing") && !line.contains("Hello") && !line.contains("first"));

        // Failures: storing the bytes or the rows rejects, so the sender gets a bounce; nothing is
        // half-written as the record.
        let fresh = message("third", "a@example.com");
        store.fail_put.set(true);
        let v = ready(mail::receive(
            &store,
            &incoming(&fresh, "apple@pigeon.dev", "2026-11-26T09:01:00Z", &[]),
        ));
        assert_eq!(v, mail::Verdict::Reject(mail::NOT_STORED.into()));
        store.fail_put.set(false);
        store.fail_db.set(true);
        let v = ready(mail::receive(
            &store,
            &incoming(&fresh, "apple@pigeon.dev", "2026-11-26T09:01:01Z", &[]),
        ));
        assert_eq!(v, mail::Verdict::Reject(mail::NOT_STORED.into()));
        store.fail_db.set(false);
        assert_eq!(
            store.count("SELECT COUNT(*) FROM emails WHERE subject = 'third'"),
            0
        );

        // Off by default: without the configuration the plan writes, every message is rejected.
        let off = Store::new(None);
        let v = ready(mail::receive(
            &off,
            &incoming(&raw, "cloudflare@pigeon.dev", "2026-11-26T09:02:00Z", &[]),
        ));
        assert_eq!(v, mail::Verdict::Reject(mail::NOT_SET_UP.into()));
        assert!(off.blobs.borrow().is_empty());

        // Not MIME: kept as received, marked failed, never bounced for a parser bug.
        let junk = b"\x00\x01 not mail".to_vec();
        let s7 = stored(ready(mail::receive(
            &store,
            &incoming(&junk, "apple@pigeon.dev", "2026-11-26T09:03:00Z", &[]),
        )));
        assert_eq!(
            store.count(&format!(
                "SELECT COUNT(*) FROM items WHERE item_id = '{}' AND status = 'failed'",
                s7.item_id
            )),
            1
        );
    }

    /// The signed read ops' queries on the stand-in: mailboxes with counts, a page of mail newest
    /// first with a cursor, filters by mailbox and sender, one message with its HTML marked for
    /// sandboxed rendering and its attachments; and the ops refuse what is not theirs to read.
    #[test]
    fn email_read_ops_answer_from_the_record() {
        use datastore::{DataCall, ListQuery, call_problem};
        let config = pigeon_config();
        let store = Store::new(Some(&config));
        let mut ids = Vec::new();
        for (i, (to, from)) in [
            ("cloudflare@pigeon.dev", "billing@example.com"),
            ("buildkite@pigeon.dev", "ci@buildkite.example"),
            ("spam@pigeon.dev", "win@prize.example"),
        ]
        .iter()
        .enumerate()
        {
            let raw = message(&format!("m{i}"), from);
            let s = stored(ready(mail::receive(
                &store,
                &incoming(&raw, to, &format!("2026-11-26T10:00:0{i}Z"), &[]),
            )));
            ids.push(s.item_id);
        }
        let boxes = store.rows(&datastore::mailboxes_query());
        let merged = pigeon_capabilities::email::merge_mailboxes(&config, &json!(boxes));
        let counts: Vec<(String, u64)> = merged
            .iter()
            .map(|m| {
                (
                    m["mailbox"].as_str().unwrap().to_string(),
                    m["count"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            counts,
            [
                ("cloudflare".into(), 1),
                ("buildkite".into(), 1),
                ("apple".into(), 0),
                ("catch-all".into(), 1)
            ]
        );

        // Newest first, a page of two, then the rest by cursor.
        let page =
            |q: &ListQuery| datastore::list_answer(&store.rows(&datastore::list_query(q)), q.limit);
        let first = page(&ListQuery {
            limit: Some(2),
            ..Default::default()
        });
        let got: Vec<&str> = first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(got, [ids[2].as_str(), ids[1].as_str()]);
        let next = first["next"].as_str().unwrap().to_string();
        let second = page(&ListQuery {
            limit: Some(2),
            cursor: Some(next),
            ..Default::default()
        });
        assert_eq!(second["messages"][0]["id"], ids[0].as_str());
        assert!(second["next"].is_null());
        let m = &second["messages"][0];
        assert_eq!(m["sender_check"], "pass");
        assert_eq!(m["attachments"], 1);
        assert_eq!(m["from"]["address"], "billing@example.com");
        assert_eq!(m["mailbox"], "cloudflare");

        let by_box = page(&ListQuery {
            mailbox: Some("catch-all".into()),
            ..Default::default()
        });
        assert_eq!(by_box["messages"].as_array().unwrap().len(), 1);
        let by_domain = page(&ListQuery {
            from: Some("BUILDKITE.example".into()),
            ..Default::default()
        });
        assert_eq!(by_domain["messages"][0]["id"], ids[1].as_str());

        // One message.
        let qs = datastore::get_queries(&ids[0]);
        let html_ref = store.rows(&qs[1])[0]["html_ref"]
            .as_str()
            .unwrap()
            .to_string();
        let html =
            String::from_utf8(store.blobs.borrow()[&pigeon_ingest::blob_key(&html_ref)].clone())
                .unwrap();
        let text = String::from_utf8(
            store.blobs.borrow()[&format!("derived/{}/text.txt", ids[0])].clone(),
        )
        .unwrap();
        let got = datastore::get_answer(
            &store.rows(&qs[0]),
            &store.rows(&qs[1]),
            &store.rows(&qs[2]),
            &store.rows(&qs[3]),
            Some(text),
            Some(html),
        )
        .unwrap();
        assert_eq!(got["subject"], "m0");
        assert_eq!(got["html"]["render"], "sandboxed");
        assert_eq!(got["html"]["remote_images"], "blocked");
        assert!(got["text"].as_str().unwrap().contains("Hello from m0"));
        assert_eq!(got["attachments"][0]["filename"], "invoice.pdf");
        assert!(
            got["attachments"][0]["key"]
                .as_str()
                .unwrap()
                .starts_with("blobs/sha256/")
        );
        assert_eq!(got["sightings"][0]["sender_check"], "pass");
        assert!(
            datastore::get_answer(&[], &[], &[], &[], None, None).is_none(),
            "no such item"
        );

        // What the ops refuse before any query.
        for (call, why) in [
            (DataCall::Get { id: "../x".into() }, "64 hex"),
            (
                DataCall::Object {
                    key: "staging/x".into(),
                },
                "blobs/",
            ),
            (
                DataCall::Object {
                    key: "blobs/../config".into(),
                },
                "blobs/",
            ),
            (
                DataCall::Objects {
                    prefix: "ingest/".into(),
                    cursor: None,
                    limit: None,
                },
                "blobs/",
            ),
            (
                DataCall::List(ListQuery {
                    limit: Some(500),
                    ..Default::default()
                }),
                "limit",
            ),
            (
                DataCall::List(ListQuery {
                    cursor: Some("x".into()),
                    ..Default::default()
                }),
                "cursor",
            ),
        ] {
            assert!(
                call_problem(&call).is_some_and(|p| p.contains(why)),
                "{call:?}"
            );
        }
        assert!(
            call_problem(&DataCall::Objects {
                prefix: "derived/".into(),
                cursor: None,
                limit: None
            })
            .is_none()
        );
        // The call core signs names its instance and op.
        let body = serde_json::to_value(datastore::CallRequest {
            instance: "pigeon.dev".into(),
            call: DataCall::List(ListQuery {
                mailbox: Some("apple".into()),
                ..Default::default()
            }),
        })
        .unwrap();
        assert_eq!(
            body,
            json!({ "instance": "pigeon.dev", "call": { "op": "list", "mailbox": "apple" } })
        );

        // The D1 schema is ingest.md decision 3's, column for column.
        let columns = |table: &str| -> Vec<String> {
            store
                .rows(&Statement {
                    sql: format!("SELECT name FROM pragma_table_info('{table}')"),
                    params: vec![],
                })
                .iter()
                .map(|r| r["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            columns("items"),
            [
                "item_id",
                "sha256",
                "blake3",
                "size",
                "media_type",
                "kind",
                "filename",
                "text_sha256",
                "parent_id",
                "first_seen",
                "status",
                "error",
                "version"
            ]
        );
        assert_eq!(
            columns("sightings"),
            [
                "item_id", "source", "account", "locator", "seen_at", "extra"
            ]
        );
        assert_eq!(
            columns("emails"),
            [
                "item_id",
                "schema",
                "fingerprint",
                "message_id",
                "in_reply_to",
                "references",
                "subject",
                "date",
                "from",
                "to",
                "cc",
                "bcc",
                "reply_to",
                "sender",
                "delivered_to",
                "headers",
                "signals",
                "text_ref",
                "html_ref"
            ]
        );
        // And it refuses a database holding the ingest runner's dedupe index.
        assert!(datastore::migration_refusal(&["blobs".into(), "sightings".into()]).is_some());
        assert!(datastore::migration_refusal(&["items".into()]).is_none());
        assert_eq!(
            datastore::pending_migrations(&["0001_record".into()]).len(),
            1
        );
    }

    /// pigeon.dev's record with the lite store and email.
    fn pigeon_record(extra: Value) -> Instance {
        let mut v = json!({
            "id": "pigeon.dev", "title": "pigeon.dev", "account": { "kind": "home" },
            "zones": ["pigeon.dev"],
            "capabilities": {
                "data": { "tier": "lite" },
                "email": { "addresses": [
                    { "local": "cloudflare", "forward": "person@example.com" },
                    { "local": "buildkite" }, { "local": "apple" }
                ] }
            }
        });
        if let (Some(base), Some(more)) = (v.as_object_mut(), extra.as_object()) {
            for (k, x) in more {
                base.insert(k.clone(), x.clone());
            }
        }
        serde_json::from_value(v).unwrap()
    }

    fn rec(
        id: &str,
        name: &str,
        kind: &str,
        content: &str,
    ) -> pigeon_capabilities::instances::data_plan::DnsRecord {
        pigeon_capabilities::instances::data_plan::DnsRecord {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            content: content.into(),
        }
    }

    /// The data store's and email's plan steps: the dry run reads the zone first. With no MX the
    /// plan makes everything and routes the catch-all; a live foreign MX stops it; old Fastmail and
    /// Proton records are listed one by one as stale, to replace, and nothing not shown is touched;
    /// what was made is the ledger's; applying is refused for anything but the store's own Worker.
    #[test]
    fn email_plan_steps_read_the_zone_first() {
        use pigeon_capabilities::instances::data_plan::{
            SeenData, SeenDestination, data_refusal, data_steps, dns_review,
        };
        use pigeon_capabilities::instances::plan::{
            Ledger, PlatformOwned, Step, describe, digest, record,
        };
        let r = pigeon_record(json!({}));
        let owned = PlatformOwned {
            workers: vec!["pigeon-platform".into(), "pigeon-sites".into()],
            hosts: vec!["platform.pigeon.dev".into()],
            apex: "pigeon.dev".into(),
        };
        let fresh = SeenData {
            artifact: Some("a".repeat(64)),
            zone_id: Some("zone1".into()),
            ..Default::default()
        };

        // Nothing there and no MX: every resource, then routing.
        let steps = data_steps(&r, &fresh, &Ledger::default(), Some(&owned));
        let kinds: Vec<&str> = steps
            .iter()
            .map(|s| match s {
                Step::CreateBucket { .. } => "bucket",
                Step::CreateDatabase { .. } => "database",
                Step::Migrate { .. } => "migrate",
                Step::UploadDataWorker { .. } => "worker",
                Step::Pending { .. } => "pending",
                Step::WriteEmailConfig { .. } => "config",
                Step::EnableEmailRouting { .. } => "routing",
                Step::SetCatchAll { .. } => "catch-all",
                Step::AddDestination { .. } => "destination",
                _ => "other",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "bucket",
                "database",
                "migrate",
                "worker",
                "pending",
                "config",
                "routing",
                "catch-all",
                "destination",
                "pending"
            ]
        );
        assert!(steps.iter().all(|s| !s.stops()));
        assert!(matches!(&steps[3], Step::UploadDataWorker { name, .. } if name == "pigeon-data"));
        assert!(
            matches!(&steps[7], Step::SetCatchAll { worker, zone, .. } if worker == "pigeon-data" && zone == "pigeon.dev")
        );
        assert!(data_refusal(&r, &steps, Some(&owned)).is_none());
        let lines = describe(&steps);
        assert!(lines[4].as_str().unwrap().contains("B2 backup"));
        assert!(
            lines[9]
                .as_str()
                .unwrap()
                .starts_with("pending: forward destination person@example.com")
        );

        // A live foreign MX: stop, change nothing about mail.
        let foreign = SeenData {
            dns: vec![rec(
                "mx1",
                "pigeon.dev",
                "MX",
                "in1-smtp.messagingengine.com",
            )],
            ..fresh.clone()
        };
        let steps = data_steps(&r, &foreign, &Ledger::default(), Some(&owned));
        let stop = steps.iter().find(|s| s.stops()).unwrap();
        assert!(
            matches!(stop, Step::Blocked { why } if why.contains("live MX") && why.contains("messagingengine"))
        );
        assert!(!steps.iter().any(|s| matches!(
            s,
            Step::EnableEmailRouting { .. }
                | Step::RemoveStaleDns { .. }
                | Step::SetCatchAll { .. }
        )));
        assert!(data_refusal(&r, &steps, Some(&owned)).is_some());

        // The old MX are gone; their TXT and DKIM remain: each listed, stale, to replace.
        let zone = vec![
            rec(
                "t1",
                "pigeon.dev",
                "TXT",
                "\"v=spf1 include:spf.messagingengine.com include:_spf.protonmail.ch ~all\"",
            ),
            rec(
                "t2",
                "fm1._domainkey.pigeon.dev",
                "CNAME",
                "fm1.pigeon.dev.dkim.fmhosted.com",
            ),
            rec(
                "t3",
                "protonmail._domainkey.pigeon.dev",
                "CNAME",
                "protonmail.domainkey.abc.domains.proton.ch",
            ),
            rec("t4", "pigeon.dev", "TXT", "protonmail-verification=abc123"),
            rec(
                "t5",
                "_dmarc.pigeon.dev",
                "TXT",
                "v=DMARC1; p=quarantine; rua=mailto:x@dmarc.example",
            ),
            rec("t6", "pigeon.dev", "TXT", "google-site-verification=keep"),
            rec("t7", "www.pigeon.dev", "CNAME", "pigeon.dev"),
        ];
        let review = dns_review("pigeon.dev", &zone);
        let stale: Vec<&str> = review.stale.iter().map(|(r, _)| r.id.as_str()).collect();
        assert_eq!(stale, ["t1", "t2", "t3", "t4"]);
        assert_eq!(
            review.kept.len(),
            1,
            "the DMARC policy naming no old provider stays"
        );
        assert!(review.foreign_mx.is_empty());
        let with_stale = SeenData {
            dns: zone,
            ..fresh.clone()
        };
        let steps = data_steps(&r, &with_stale, &Ledger::default(), Some(&owned));
        let removals: Vec<&str> = steps
            .iter()
            .filter_map(|s| match s {
                Step::RemoveStaleDns { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            removals,
            ["t1", "t2", "t3", "t4"],
            "only what the plan shows"
        );
        let shown = describe(&steps);
        assert!(shown.iter().any(|l| {
            l.as_str()
                .unwrap()
                .starts_with("stale, to replace: delete TXT pigeon.dev \"v=spf1")
        }));
        assert!(
            shown
                .iter()
                .any(|l| l.as_str().unwrap().contains("_dmarc.pigeon.dev")
                    && l.as_str().unwrap().contains("kept"))
        );
        // Removals come before routing is turned on, so Cloudflare's SPF is the only one.
        let at = |f: &dyn Fn(&Step) -> bool| steps.iter().position(f).unwrap();
        assert!(
            at(&|s| matches!(s, Step::RemoveStaleDns { .. }))
                < at(&|s| matches!(s, Step::EnableEmailRouting { .. }))
        );
        // A Cloudflare MX means routing is already on: not foreign.
        assert!(
            dns_review(
                "pigeon.dev",
                &[rec("m", "pigeon.dev", "MX", "route1.mx.cloudflare.net")]
            )
            .foreign_mx
            .is_empty()
        );

        // Applied once, recorded in the ledger: the next plan is all in place.
        let mut ledger = Ledger::default();
        let first = data_steps(&r, &fresh, &ledger, Some(&owned));
        for s in &first {
            record(&mut ledger, s);
        }
        assert_eq!(ledger.databases, ["pigeon-data"]);
        assert_eq!(ledger.email_zones, ["pigeon.dev"]);
        let module = match &first[3] {
            Step::UploadDataWorker { module, .. } => module.clone(),
            _ => unreachable!(),
        };
        let config = pigeon_capabilities::email::config(&r).unwrap();
        let done = SeenData {
            bucket_exists: true,
            database: Some("db1".into()),
            tables: vec![
                "items".into(),
                "sightings".into(),
                "emails".into(),
                "migrations".into(),
            ],
            applied: datastore::MIGRATIONS
                .iter()
                .map(|m| m.id.to_string())
                .collect(),
            worker_exists: true,
            config_digest: Some(config.digest()),
            routing_enabled: true,
            catch_all: Some("pigeon-data".into()),
            destinations: vec![SeenDestination {
                email: "person@example.com".into(),
                verified: true,
            }],
            ..fresh.clone()
        };
        let again = data_steps(&r, &done, &ledger, Some(&owned));
        assert!(
            again.iter().all(|s| !s.changes() && !s.stops()),
            "{again:?}"
        );
        assert!(
            ledger
                .workers
                .contains(&("pigeon-data".to_string(), module))
        );
        // An unverified destination is pending, and does not stop the plan.
        let pending = SeenData {
            destinations: vec![SeenDestination {
                email: "person@example.com".into(),
                verified: false,
            }],
            ..done.clone()
        };
        assert!(data_steps(&r, &pending, &ledger, Some(&owned)).iter().any(
            |s| matches!(s, Step::Pending { what, .. } if what.contains("person@example.com"))
        ));
        // Adding an address changes only the configuration row: no DNS, no routing.
        let more = pigeon_capabilities::email::with_address(&r, "github", false).unwrap();
        let changed: Vec<Step> = data_steps(&more, &done, &ledger, Some(&owned))
            .into_iter()
            .filter(|s| s.changes())
            .collect();
        assert!(
            matches!(changed.as_slice(), [Step::WriteEmailConfig { .. }]),
            "{changed:?}"
        );
        // Someone else's resources are conflicts; the catch-all of another Worker too.
        let theirs = SeenData {
            catch_all: Some("other".into()),
            ..done.clone()
        };
        assert!(
            data_steps(&r, &theirs, &Ledger::default(), Some(&owned))
                .iter()
                .any(|s| matches!(s, Step::Conflict { what, .. } if what == "catch-all"))
        );
        // No build yet: blocked, saying how to make one.
        let unbuilt = SeenData {
            artifact: None,
            ..fresh.clone()
        };
        assert!(
            data_steps(&r, &unbuilt, &Ledger::default(), Some(&owned))
                .iter()
                .any(|s| matches!(s, Step::Blocked { why } if why.contains("no build")))
        );
        // The store's Worker is never a platform Worker.
        let clash = PlatformOwned {
            workers: vec!["pigeon-data".into()],
            ..owned.clone()
        };
        assert!(matches!(
            data_steps(&r, &fresh, &Ledger::default(), Some(&clash)).as_slice(),
            [Step::Blocked { .. }]
        ));
        // A record without the store plans nothing, so its digest is as before.
        let plain = pigeon_record(json!({ "capabilities": {} }));
        assert!(data_steps(&plain, &fresh, &Ledger::default(), Some(&owned)).is_empty());
        assert_eq!(
            digest(&plain, "acc", &[]),
            digest(
                &plain,
                "acc",
                &data_steps(&plain, &fresh, &Ledger::default(), None)
            )
        );

        // The backup, once the record names its bucket: queues, the notification, the key.
        let backed = pigeon_record(json!({ "capabilities": {
            "data": { "tier": "lite", "backup": { "bucket": "pigeon-backup-pigeon-data",
                "endpoint": "https://s3.eu-central-003.backblazeb2.com", "region": "eu-central-003" } },
            "email": {}
        } }));
        let steps = data_steps(&backed, &fresh, &Ledger::default(), Some(&owned));
        let queues: Vec<(&str, Option<&str>)> = steps
            .iter()
            .filter_map(|s| match s {
                Step::CreateQueue { name, consumer, .. } => {
                    Some((name.as_str(), consumer.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            queues,
            [
                ("pigeon-data-backup-dlq", None),
                ("pigeon-data-backup", Some("pigeon-data"))
            ]
        );
        assert!(steps.iter().any(|s| matches!(s, Step::AddBucketNotification { queue, .. } if queue == "pigeon-data-backup")));
        assert!(steps.iter().any(
            |s| matches!(s, Step::Pending { why, .. } if why.contains("DATA_B2_PIGEON_DATA"))
        ));

        // The upload's metadata and body: the bindings the Worker reads, every module.
        use pigeon_capabilities::data_store::Backup;
        use pigeon_capabilities::instances::data_plan::{
            WorkerBindings, artifact_digest, multipart, worker_metadata,
        };
        let b = Backup {
            bucket: "pigeon-backup-pigeon-data".into(),
            endpoint: "https://s3.eu-central-003.backblazeb2.com".into(),
            region: "eu-central-003".into(),
        };
        let meta = worker_metadata(
            "shim.mjs",
            &WorkerBindings {
                instance: "pigeon.dev",
                bucket: "pigeon-data",
                database_id: "db1",
                data_key: "k",
                backup: Some((&b, "store1", "DATA_B2_PIGEON_DATA")),
            },
        );
        let names: Vec<&str> = meta["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["DATA", "DB", "INSTANCE", "DATA_KEY", "BACKUP", "B2_KEYS"]
        );
        let parts = vec![
            (
                "shim.mjs".to_string(),
                "application/javascript+module".to_string(),
                b"js".to_vec(),
            ),
            (
                "index.wasm".to_string(),
                "application/wasm".to_string(),
                vec![0, 97, 115, 109],
            ),
        ];
        let body = multipart("XYZ", &meta, &parts);
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"index.wasm\"") && text.ends_with("--XYZ--\r\n"));
        assert_ne!(
            artifact_digest(&[("a", b"1")]),
            artifact_digest(&[("a", b"2")])
        );

        // The home token for these steps never reaches past its zone for DNS and routing.
        use pigeon_capabilities::instances::data_apply::{PURPOSE, policies};
        let perms = pigeon_capabilities::split::purpose_permissions(PURPOSE).unwrap();
        let p = policies(Some(perms), "zone1").unwrap();
        assert_eq!(p.len(), 2);
        assert!(p[1].permissions.iter().any(|x| x == "DNS Write"));
        assert!(!p[0].permissions.iter().any(|x| x.starts_with("DNS")));
        assert!(pigeon_capabilities::split::CORE_ONLY_PURPOSES.contains(&PURPOSE));
        assert!(policies(Some(perms), "").is_err());
    }

    /// The record's rules for `data` and `email`, the record changes the two write tools propose,
    /// and the roles the tools need: reads are viewer's, writes operator's.
    #[test]
    fn email_record_rules_and_tool_roles() {
        use pigeon_capabilities::catalogue::problems;
        use pigeon_capabilities::email::{self, with_address, with_forward};
        use pigeon_capabilities::membership::{Role, tool_need};

        let r = pigeon_record(json!({}));
        assert!(problems(&r).is_empty(), "{:?}", problems(&r));
        assert!(email::enabled(&r));
        let c = email::config(&r).unwrap();
        assert_eq!(c.domain, "pigeon.dev");
        assert_eq!(
            email::mailbox_names(&c),
            ["cloudflare", "buildkite", "apple", "catch-all"]
        );
        assert_eq!(email::FIRST_ADDRESSES, ["cloudflare", "buildkite", "apple"]);
        assert_eq!(
            pigeon_capabilities::data_store::store_name(&r),
            "pigeon-data"
        );

        for (extra, why) in [
            (json!({ "capabilities": { "email": {} } }), "needs \"data\""),
            (
                json!({ "capabilities": { "data": { "tier": "full" } } }),
                "not built yet",
            ),
            (
                json!({ "capabilities": { "data": {}, "email": { "addresses": [{ "local": "catch-all" }] } } }),
                "not a listed address",
            ),
            (
                json!({ "capabilities": { "data": {}, "email": { "addresses": [{ "local": "a" }, { "local": "a" }] } } }),
                "listed twice",
            ),
            (
                json!({ "capabilities": { "data": {}, "email": { "addresses": [{ "local": "a", "forward": "nope" }] } } }),
                "not a forward destination",
            ),
            (
                json!({ "capabilities": { "data": {}, "email": { "domain": "other.example" } } }),
                "not one of the instance's zones",
            ),
            (
                json!({ "capabilities": { "data": { "addresses": [{ "local": "a" }] } } }),
                "does not accept a domain",
            ),
            (
                json!({ "capabilities": { "data": { "backup": { "bucket": "b2b", "endpoint": "https://evil.example", "region": "x" } } } }),
                "backblazeb2.com",
            ),
        ] {
            let got = problems(&pigeon_record(extra.clone()));
            assert!(got.iter().any(|p| p.contains(why)), "{extra}: {got:?}");
        }
        // On a linked instance with a zone (docs/plans/linked-data.md): either placement, the
        // store named for the instance, mail on its zone.
        let linked = |extra: Value| {
            let mut base = json!({ "id": "noisypigeon", "worker": "noisypigeon",
                "account": { "kind": "linked", "account_id": null }, "zones": ["noisypigeon.com"] });
            if let (Some(b), Some(more)) = (base.as_object_mut(), extra.as_object()) {
                for (k, x) in more {
                    b.insert(k.clone(), x.clone());
                }
            }
            pigeon_record(base)
        };
        for placement in ["inline", "own_worker"] {
            let r = linked(json!({ "capabilities": {
                "data": { "placement": placement }, "email": {} } }));
            assert!(problems(&r).is_empty(), "{placement}: {:?}", problems(&r));
            assert_eq!(email::config(&r).unwrap().domain, "noisypigeon.com");
            assert_eq!(
                pigeon_capabilities::data_store::store_name(&r),
                "noisypigeon-data"
            );
        }
        for (extra, why) in [
            // No zone: no mail. Data alone needs none.
            (
                json!({ "zones": [], "capabilities": { "data": {}, "email": {} } }),
                "Email needs a domain",
            ),
            // Inline runs in the instance's Worker, which the record must name.
            (
                json!({ "worker": null, "capabilities": { "data": { "placement": "inline" } } }),
                "choose own_worker",
            ),
            // Email runs where the store runs.
            (
                json!({ "capabilities": { "data": { "placement": "inline" },
                    "email": { "placement": "own_worker" } } }),
                "Email runs where the data store runs",
            ),
            // No B2 backup on a linked instance yet.
            (
                json!({ "capabilities": { "data": { "backup": { "bucket": "b2-bucket",
                    "endpoint": "https://s3.eu-central-003.backblazeb2.com", "region": "eu-central-003" } } } }),
                "not built for a linked instance yet",
            ),
        ] {
            let got = problems(&linked(extra.clone()));
            assert!(got.iter().any(|p| p.contains(why)), "{extra}: {got:?}");
        }
        let alone = linked(json!({ "zones": [], "capabilities": { "data": {} } }));
        assert!(problems(&alone).is_empty(), "{:?}", problems(&alone));
        // Home's placement stays read-only: its own Worker, pigeon-data.
        let moved = pigeon_record(json!({ "capabilities": { "data": { "placement": "inline" } } }));
        assert!(
            problems(&moved).iter().any(|p| p.contains("read-only")),
            "{:?}",
            problems(&moved)
        );
        // Off by default: a record without them has neither, and every view and tool is hidden.
        let off = pigeon_record(json!({ "capabilities": {} }));
        assert!(!email::enabled(&off) && email::config(&off).is_none());
        assert!(email::instance_tools(&off).unwrap().views.is_empty());
        assert_eq!(email::instance_tools(&r).unwrap().views[0].area, "email");
        // The settings read back as written, so record digests are stable.
        let back: Instance = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back, r);

        // An address is a record change; a forward change too, and scope only through it.
        let added = with_address(&r, "GitHub", false).unwrap();
        assert!(
            added.capabilities["email"]
                .addresses
                .iter()
                .any(|a| a.local == "github")
        );
        assert!(with_address(&r, "apple", false).is_err());
        assert!(with_address(&r, "catch-all", false).is_err());
        let removed = with_address(&r, "apple", true).unwrap();
        assert_eq!(removed.capabilities["email"].addresses.len(), 2);
        let fwd = with_forward(&r, Some("apple"), Some("me@example.com"), None).unwrap();
        assert_eq!(
            email::config(&fwd).unwrap().forward_for("apple").as_deref(),
            Some("me@example.com")
        );
        let all = with_forward(&r, None, Some("me@example.com"), Some(ForwardScope::All)).unwrap();
        let c = email::config(&all).unwrap();
        assert_eq!(
            (c.scope, c.forward_for("catch-all").as_deref()),
            (ForwardScope::All, Some("me@example.com"))
        );
        let cleared = with_forward(&r, Some("cloudflare"), None, None).unwrap();
        assert_eq!(
            email::config(&cleared).unwrap().forward_for("cloudflare"),
            None
        );
        assert!(with_forward(&r, Some("nobody"), Some("me@example.com"), None).is_err());
        assert!(with_forward(&r, None, Some("not-an-address"), None).is_err());
        assert!(problems(&all).is_empty());

        // Roles: the reads are viewer's with exactly their arguments; the writes operator's.
        let i = json!({ "instance": "pigeon.dev" });
        assert_eq!(tool_need("email", "email_status", &i), Some(Role::Viewer));
        assert_eq!(
            tool_need("email", "email_mailboxes", &i),
            Some(Role::Viewer)
        );
        assert_eq!(
            tool_need(
                "email",
                "email_list",
                &json!({ "instance": "pigeon.dev", "mailbox": "apple", "cursor": "x" })
            ),
            Some(Role::Viewer)
        );
        assert_eq!(
            tool_need(
                "email",
                "email_get",
                &json!({ "instance": "pigeon.dev", "id": "a" })
            ),
            Some(Role::Viewer)
        );
        assert_eq!(
            tool_need(
                "email",
                "email_list",
                &json!({ "instance": "pigeon.dev", "account": "x" })
            ),
            Some(Role::Operator),
            "an argument outside the read's is not a read"
        );
        assert_eq!(
            tool_need(
                "email",
                "email_address_set",
                &json!({ "instance": "pigeon.dev", "local": "x" })
            ),
            Some(Role::Operator)
        );
        assert_eq!(
            tool_need(
                "email",
                "email_forward_set",
                &json!({ "instance": "pigeon.dev", "scope": "all" })
            ),
            Some(Role::Operator)
        );
        // The storage data-store source is a read too, bound to the instance.
        assert_eq!(
            tool_need(
                "storage",
                "storage_get_object",
                &json!({ "instance": "pigeon.dev", "key": "blobs/x", "source": "data" })
            ),
            Some(Role::Viewer)
        );
        use pigeon_capabilities::storage::{Source, source_of};
        assert_eq!(source_of(&r, &json!({})), Ok(Source::Data));
        assert_eq!(source_of(&r, &json!({ "source": "s3" })), Ok(Source::S3));
        assert!(source_of(&r, &json!({ "source": "r2" })).is_err());
        // On the instance endpoint, the email tools take only their own arguments.
        use pigeon_capabilities::instances::tools::bind_global;
        assert!(
            bind_global(
                "pigeon.dev",
                Some(&r),
                "email_list",
                &json!({ "mailbox": "apple" })
            )
            .is_ok()
        );
        assert!(
            bind_global(
                "pigeon.dev",
                Some(&r),
                "email_list",
                &json!({ "account": "x" })
            )
            .is_err()
        );
        assert!(bind_global("pigeon.dev", Some(&off), "email_list", &json!({})).is_err());
    }

    /// The Email tab's fixtures are what the read ops answer for the stand-in's mail, so fixture
    /// mode shows the shapes the code sends.
    #[test]
    fn email_tab_fixtures_match_the_read_ops() {
        let config = pigeon_config();
        let store = Store::new(Some(&config));
        let mut ids = Vec::new();
        for (i, (to, from)) in [
            ("cloudflare@pigeon.dev", "billing@example.com"),
            ("hello@pigeon.dev", "win@prize.example"),
        ]
        .iter()
        .enumerate()
        {
            let raw = message(&format!("Fixture {i}"), from);
            let s = stored(ready(mail::receive(
                &store,
                &incoming(&raw, to, &format!("2026-11-26T12:00:0{i}Z"), &[]),
            )));
            ids.push(s.item_id);
        }
        let mailboxes = json!({
            "instance": "home",
            "mailboxes": pigeon_capabilities::email::merge_mailboxes(&config, &json!(store.rows(&datastore::mailboxes_query()))),
        });
        let mut list = datastore::list_answer(
            &store.rows(&datastore::list_query(&Default::default())),
            None,
        );
        list["instance"] = json!("home");
        let qs = datastore::get_queries(&ids[0]);
        let html_ref = store.rows(&qs[1])[0]["html_ref"]
            .as_str()
            .unwrap()
            .to_string();
        let html =
            String::from_utf8(store.blobs.borrow()[&pigeon_ingest::blob_key(&html_ref)].clone())
                .unwrap();
        let text = String::from_utf8(
            store.blobs.borrow()[&format!("derived/{}/text.txt", ids[0])].clone(),
        )
        .unwrap();
        let get = datastore::get_answer(
            &store.rows(&qs[0]),
            &store.rows(&qs[1]),
            &store.rows(&qs[2]),
            &store.rows(&qs[3]),
            Some(text),
            Some(html),
        )
        .unwrap();
        // `PIGEON_WRITE_FIXTURES=1` rewrites them from this scenario.
        if std::env::var_os("PIGEON_WRITE_FIXTURES").is_some() {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/console/web/fixtures");
            for (name, v) in [
                ("email_mailboxes.home.json", &mailboxes),
                ("email_list.home.json", &list),
                ("email_get.home.json", &get),
            ] {
                std::fs::write(
                    dir.join(name),
                    serde_json::to_string_pretty(v).unwrap() + "\n",
                )
                .unwrap();
            }
        }
        for (name, want, recorded) in [
            (
                "email_mailboxes.home.json",
                &mailboxes,
                include_str!(
                    "../../src/capabilities/src/console/web/fixtures/email_mailboxes.home.json"
                ),
            ),
            (
                "email_list.home.json",
                &list,
                include_str!(
                    "../../src/capabilities/src/console/web/fixtures/email_list.home.json"
                ),
            ),
            (
                "email_get.home.json",
                &get,
                include_str!("../../src/capabilities/src/console/web/fixtures/email_get.home.json"),
            ),
        ] {
            let recorded: Value = serde_json::from_str(recorded).unwrap();
            assert_eq!(&recorded, want, "{name}: regenerate it from this scenario");
        }
        // The home fixture record enables the tab, and its manifest sends it.
        let homes: Value = serde_json::from_str(include_str!(
            "../../src/capabilities/src/console/web/fixtures/instances_list.json"
        ))
        .unwrap();
        let home: Instance = serde_json::from_value(homes[0].clone()).unwrap();
        assert!(pigeon_capabilities::email::enabled(&home));
        let manifest: Value = serde_json::from_str(include_str!(
            "../../src/capabilities/src/console/web/fixtures/console_manifest.home.json"
        ))
        .unwrap();
        assert!(
            manifest["areas"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["id"] == "email" && a["views"][0]["id"] == "mailboxes")
        );
    }
}

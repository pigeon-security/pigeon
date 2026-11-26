//! The proxy Worker: contracts, dispatch, deploy allowlist, mirror, webhooks, OIDC and tokens.

use super::*;

#[test]
fn contract_signed_callers_and_skew() {
    use pigeon_contract::{
        Answer, CONTRACT, FailureKind, Manifest, Refused, accepts, sign, verify, wants,
    };

    // Core signs who calls, for one op and one body; the capability Worker checks all three.
    let key = b"caller key";
    let who = json!({ "user": { "name": "willow", "kind": "standard" }, "acting_for": null });
    let body = br#"{"tool":"instance_plan","args":{}}"#;
    let now = 1_800_000_000_000;
    let header = sign(key, &who, "call", body, now, "n1");
    let signed = verify(key, Some(&header), "call", body, now + 1_000).unwrap();
    assert_eq!(signed.caller, who);
    assert_eq!(signed.exp - signed.iat, pigeon_contract::MAX_AGE_MS);
    for (key2, header2, op, body2, at, why) in [
        (&key[..], None, "call", &body[..], now, Refused::Missing),
        (
            &key[..],
            Some("no-dot"),
            "call",
            &body[..],
            now,
            Refused::Malformed,
        ),
        (
            &b"another key"[..],
            Some(header.as_str()),
            "call",
            &body[..],
            now,
            Refused::BadSignature,
        ),
        (
            &key[..],
            Some(header.as_str()),
            "secret",
            &body[..],
            now,
            Refused::BadSignature,
        ),
        (
            &key[..],
            Some(header.as_str()),
            "call",
            &b"{}"[..],
            now,
            Refused::BadSignature,
        ),
        (
            &key[..],
            Some(header.as_str()),
            "call",
            &body[..],
            now + 61_000,
            Refused::Expired,
        ),
        (
            &key[..],
            Some(header.as_str()),
            "call",
            &body[..],
            now - 6_000,
            Refused::FromTheFuture,
        ),
    ] {
        assert_eq!(verify(key2, header2, op, body2, at), Err(why), "{why}");
    }
    // A signature that claims a longer life than a minute is refused, however it was made.
    let forged = {
        let (_, tag) = header.split_once('.').unwrap();
        let long = json!({ "caller": who, "iat": now, "exp": now + 3_600_000, "nonce": "n" });
        let first = pigeon_core::webauthn::b64url(long.to_string().as_bytes());
        format!("{first}.{tag}")
    };
    assert!(verify(key, Some(&forged), "call", body, now).is_err());

    // Skew: this version and the one before are spoken; fields are added, never required.
    assert!(accepts(CONTRACT) && !accepts(CONTRACT + 1));
    let old: Manifest = serde_json::from_value(json!({
        "contract": CONTRACT, "worker": "pigeon-sites",
        "capabilities": [{ "name": "instances", "tools": [{ "name": "instance_plan" }] }],
        "added_later": true,
    }))
    .unwrap();
    assert_eq!(old.capabilities[0].tools[0].name, "instance_plan");
    assert!(old.capabilities[0].routes.is_empty() && old.commit.is_empty());
    let newer: Answer = serde_json::from_value(
        json!({ "error": { "message": "m", "kind": "some_kind_from_later" } }),
    )
    .unwrap();
    assert!(matches!(newer, Answer::Error(f) if f.kind == FailureKind::Failed));
    assert_eq!(
        serde_json::to_value(Answer::error(FailureKind::UnknownOp, "x")).unwrap(),
        json!({ "error": { "message": "x", "kind": "unknown_op" } })
    );
    assert_eq!(
        serde_json::to_value(Answer::Ok(json!(1))).unwrap(),
        json!({ "ok": 1 })
    );

    // Event patterns: every key the pattern names, equal.
    let event = json!({ "type": "push", "repo": "pigeon", "ref": "main" });
    assert!(wants(&json!({ "type": "push" }), &event));
    assert!(!wants(&json!({ "type": "push", "repo": "other" }), &event));
    assert!(!wants(&json!("push"), &event));
}

#[test]
fn core_contract_per_worker_caller_keys() {
    use pigeon_capabilities::split::{verify_caller, worker_key_name};
    use pigeon_contract::{Refused, sign};

    let (shared, sites, ci) = (&b"shared"[..], &b"sites key"[..], &b"ci key"[..]);
    let (body, now) = (&b"{}"[..], 1_800_000_000_000);
    let by = |key: &[u8]| sign(key, &json!(null), "dispatch", body, now, "n");
    let check = |own: Option<&[u8]>, header: &str, at: i64| {
        verify_caller(own, shared, Some(header), "dispatch", body, at)
    };
    // The Worker's own key, and the shared key as before.
    assert!(check(Some(sites), &by(sites), now).is_ok());
    assert!(check(Some(sites), &by(shared), now).is_ok());
    assert!(check(None, &by(shared), now).is_ok());
    // Another Worker's key, or a wrong one, is refused.
    assert_eq!(check(Some(sites), &by(ci), now), Err(Refused::BadSignature));
    assert_eq!(check(None, &by(ci), now), Err(Refused::BadSignature));
    assert_eq!(
        check(Some(sites), &by(b"wrong"), now),
        Err(Refused::BadSignature)
    );
    // The skew checks still apply to the per-Worker key.
    assert_eq!(
        check(Some(sites), &by(sites), now + 61_000),
        Err(Refused::Expired)
    );
    assert_eq!(
        check(Some(sites), &by(sites), now - 6_000),
        Err(Refused::FromTheFuture)
    );
    assert_eq!(worker_key_name("sites"), Some("CALLER_KEY_SITES"));
    assert_eq!(worker_key_name("root"), None);
    assert_eq!(
        worker_key_name("model-gateway"),
        Some("CALLER_KEY_MODEL_GATEWAY")
    );
    assert_eq!(worker_key_name("ci-gateway"), Some("CALLER_KEY_CI_GATEWAY"));
    assert_eq!(
        worker_key_name("cloud-gateway"),
        Some("CALLER_KEY_CLOUD_GATEWAY")
    );
    assert_eq!(worker_key_name("models"), None);
}

/// A capability Worker names itself once, and that name is what core looks its key up by
/// (docs/plans/caller-keys.md, step 2). Nothing in this suite names itself before this test,
/// and core never does, so core's requests send no header.
#[test]
fn contract_worker_names_itself() {
    use pigeon_capabilities::split::{WORKER_HEADER, worker_key_name};

    assert_eq!(pigeon_contract::WORKER_HEADER, WORKER_HEADER);
    assert_eq!(pigeon_contract::identity(), None);
    pigeon_contract::identify("sites");
    // The first name stays.
    pigeon_contract::identify("proxy");
    assert_eq!(pigeon_contract::identity(), Some("sites"));
    assert_eq!(
        worker_key_name(pigeon_contract::identity().unwrap()),
        Some("CALLER_KEY_SITES")
    );
}

/// Core bounds what each capability Worker may ask once its own key signed the request
/// (docs/plans/caller-keys.md, step 5); the shared key keeps today's behaviour, and a Worker
/// claiming another's name never gets that Worker's table.
#[test]
fn core_bounds_each_workers_ops_with_its_own_key() {
    use pigeon_capabilities::split::{
        KeyUsed, WORKER_KEYS, WORKER_OPS, verify_caller_keyed, worker_may,
    };
    use pigeon_contract::{CORE_OPS, Refused, sign};

    let may = |w: &str, op: &str, body: Value| worker_may(w, op, body.to_string().as_bytes());
    // Each Worker's real calls pass.
    let real = [
        ("sites", "sites_scope", json!({"action": "record"})),
        ("sites", "dispatch", json!({"tool": "instance_subdomains"})),
        ("sites", "dispatch", json!({"tool": "site_passkey"})),
        ("ci-gateway", "credential", json!({"kind": "scoped"})),
        ("ci-gateway", "ci_scope", json!({})),
        ("model-gateway", "credential", json!({"kind": "scoped"})),
        ("cloud-gateway", "credential", json!({"kind": "scoped"})),
        ("cloud-gateway", "approval", json!({"action": "create"})),
        ("proxy", "verify", json!({})),
        ("proxy", "grant", json!({})),
        ("proxy", "deploy_grant", json!({})),
        ("proxy", "credential", json!({"kind": "scoped"})),
        ("ai-agents", "dispatch", json!({"tool": "models_submit"})),
        (
            "ai-agents",
            "dispatch",
            json!({"tool": "anything_a_model_picks"}),
        ),
        ("ai-agents", "credential", json!({"kind": "run_grant"})),
        (
            "ai-agents",
            "credential_revoke",
            json!({"kind": "run_grant"}),
        ),
        ("ai-agents", "credential_release", json!({"kind": "scoped"})),
        (
            "ai-agents",
            "service_user",
            json!({"key": "ai-agent:planner"}),
        ),
    ];
    for (w, op, body) in real {
        assert_eq!(may(w, op, body.clone()), Ok(()), "{w} {op} {body}");
    }
    // Ops, kinds and tools outside a Worker's table are refused.
    let refused = [
        ("sites", "approval", json!({})),
        ("sites", "credential", json!({"kind": "scoped"})),
        // The retired legacy deploy callbacks: Sites reaches them only through sites_scope.
        ("sites", "credential", json!({"kind": "linked"})),
        ("sites", "credential", json!({"kind": "github"})),
        ("sites", "dispatch", json!({"tool": "instances_list"})),
        ("sites", "dispatch", json!({"tool": "land"})),
        // The apex's architecture read went with apex.rs after S1.
        (
            "sites",
            "dispatch",
            json!({"tool": "architecture_describe"}),
        ),
        ("sites", "dispatch", json!({})),
        (
            "model-gateway",
            "dispatch",
            json!({"tool": "models_submit"}),
        ),
        ("model-gateway", "credential", json!({"kind": "run_grant"})),
        ("model-gateway", "credential", json!({})),
        ("ci-gateway", "sites_scope", json!({})),
        ("cloud-gateway", "service_user", json!({})),
        ("proxy", "credential", json!({"kind": "github"})),
        ("proxy", "dispatch", json!({"tool": "deploy_token"})),
        ("ai-agents", "credential", json!({"kind": "scoped"})),
        ("ai-agents", "credential", json!({"kind": "linked"})),
        ("ai-agents", "credential_revoke", json!({"kind": "scoped"})),
        ("ai-agents", "verify", json!({})),
        ("root", "dispatch", json!({"tool": "land"})),
    ];
    for (w, op, body) in refused {
        assert!(may(w, op, body.clone()).is_err(), "{w} {op} {body}");
    }
    assert_eq!(
        may("sites", "dispatch", json!({"tool": "land"})),
        Err("Worker `sites` may not ask core `dispatch` for tool `land`".into())
    );
    // The table names exactly the Workers with keys, and only core's ops.
    let mut tabled: Vec<&str> = WORKER_OPS.iter().map(|(w, _)| *w).collect();
    let mut keyed: Vec<&str> = WORKER_KEYS.iter().map(|(w, _)| *w).collect();
    tabled.sort_unstable();
    keyed.sort_unstable();
    assert_eq!(tabled, keyed);
    for (_, allowed) in WORKER_OPS {
        assert!(allowed.iter().all(|a| CORE_OPS.contains(&a.op)));
    }

    // Which key verified decides whether the table applies.
    let (shared, sites, ci) = (&b"shared"[..], &b"sites key"[..], &b"ci key"[..]);
    let (body, now) = (&b"{}"[..], 1_800_000_000_000);
    let by = |key: &[u8]| sign(key, &json!(null), "approval", body, now, "n");
    let check = |own: Option<&[u8]>, header: &str| {
        verify_caller_keyed(own, shared, Some(header), "approval", body, now).map(|(_, k)| k)
    };
    assert_eq!(check(Some(sites), &by(sites)), Ok(KeyUsed::Own));
    // Signed with the shared key: today's behaviour, no table, whatever name was claimed.
    assert_eq!(check(Some(sites), &by(shared)), Ok(KeyUsed::Shared));
    assert_eq!(check(None, &by(shared)), Ok(KeyUsed::Shared));
    // Sites claiming to be ci-gateway, signing with its own key: core checks ci-gateway's key,
    // then the shared one, and both fail.
    assert_eq!(check(Some(ci), &by(sites)), Err(Refused::BadSignature));
}

/// Step 2 of docs/plans/caller-keys.md: core binds every Worker's key and each Worker sends
/// `X-Pigeon-Worker`, but still signs with the shared key. Core's signed endpoint, as it runs
/// (`verify_request`, with the recipient check), tries the named Worker's key, gets a bad
/// signature, falls back to the shared key and accepts the request as `Shared`, so no
/// Worker's table applies and nothing is refused that was accepted before.
#[test]
fn core_accepts_a_named_worker_signing_with_the_shared_key() {
    use pigeon_capabilities::split::{KeyUsed, verify_request, worker_may};
    use pigeon_contract::{Refused, WorkerContext, sign, sign_with_context};

    let (shared, sites) = (&b"shared"[..], &b"sites key"[..]);
    let now = 1_800_000_000_000;
    let core = "pigeon-platform";
    let check = |header: &str, op: &str, body: &[u8], at: i64, recipient: &str| {
        verify_request(Some(sites), shared, Some(header), op, body, at, recipient)
    };
    // An op outside sites' table: accepted all the same, since the shared key verified it.
    let body = br#"{"action":"create"}"#;
    assert!(worker_may("sites", "approval", body).is_err());
    let header = sign(shared, &json!(null), "approval", body, now, "n");
    let (_, used) = check(&header, "approval", body, now, core).unwrap();
    assert_eq!(used, KeyUsed::Shared);

    // A contextual callback (sites_scope), signed with the shared key for core.
    let body = br#"{"instance":"willow"}"#;
    let who = json!({ "user": { "name": "willow" } });
    let context = WorkerContext::new(core, Some("willow"));
    let header = sign_with_context(shared, &who, "sites_scope", body, now, "n", &context);
    let (signed, used) = check(&header, "sites_scope", body, now, core).unwrap();
    assert_eq!(used, KeyUsed::Shared);
    assert_eq!(signed.instance(), Some("willow"));
    // The recipient and age checks still apply after the fall-back.
    let used = |at: i64, recipient: &str| {
        check(&header, "sites_scope", body, at, recipient).map(|(_, k)| k)
    };
    assert_eq!(used(now, "other"), Err(Refused::WrongRecipient));
    assert_eq!(used(now + 61_000, core), Err(Refused::Expired));
    // Once sites signs with its own key (step 3), the same request is `Own`.
    let header = sign_with_context(sites, &who, "sites_scope", body, now, "n", &context);
    assert_eq!(
        check(&header, "sites_scope", body, now, core).map(|(_, k)| k),
        Ok(KeyUsed::Own)
    );
}

/// Step 3 of docs/plans/caller-keys.md, the receivers first: a capability Worker checks core's
/// call with its own key (`CALLER_KEY_OWN`), then the shared `CALLER_KEY`, so core can switch to
/// signing with each Worker's own key with no call refused in between.
#[test]
fn worker_checks_cores_calls_with_its_own_key_then_the_shared_one() {
    use pigeon_contract::{Keys, Refused, WorkerContext, sign, sign_with_context};
    use pigeon_core::webauthn::b64url;

    let (shared, sites, ci) = (&b"shared"[..], &b"sites key"[..], &b"ci key"[..]);
    let keys = Keys {
        own: sites.to_vec(),
        shared: Some(shared.to_vec()),
    };
    let (body, now) = (&br#"{"tool":"site_deploy"}"#[..], 1_800_000_000_000);
    let who = json!({ "user": { "name": "willow" } });
    let me = "pigeon-sites";
    let by = |key: &[u8]| sign(key, &who, "call", body, now, "n");
    let check =
        |keys: &Keys, header: &str, at: i64| keys.verify(Some(header), "call", body, at, me);

    // Signed with the Worker's own key: verifies.
    assert_eq!(check(&keys, &by(sites), now).unwrap().caller, who);
    // Signed with the shared key: still verifies during the transition.
    assert!(check(&keys, &by(shared), now).is_ok());
    // Another Worker's key, or a wrong one, is refused.
    assert_eq!(check(&keys, &by(ci), now), Err(Refused::BadSignature));
    assert_eq!(check(&keys, &by(b"wrong"), now), Err(Refused::BadSignature));
    // A tampered call is refused: another body, another op, or an edited envelope.
    let header = by(sites);
    assert_eq!(
        keys.verify(Some(&header), "call", b"{}", now, me),
        Err(Refused::BadSignature)
    );
    assert_eq!(
        keys.verify(Some(&header), "secret", body, now, me),
        Err(Refused::BadSignature)
    );
    let (_, tag) = header.split_once('.').unwrap();
    let forged = json!({
        "caller": { "user": { "name": "root" } },
        "iat": now,
        "exp": now + 60_000,
        "nonce": "n",
    });
    let forged = b64url(forged.to_string().as_bytes());
    assert_eq!(
        check(&keys, &format!("{forged}.{tag}"), now),
        Err(Refused::BadSignature)
    );
    assert_eq!(check(&keys, "nonsense", now), Err(Refused::Malformed));
    assert_eq!(
        keys.verify(None, "call", body, now, me),
        Err(Refused::Missing)
    );
    // A good own signature that is too old is refused as such, not retried with the shared key.
    assert_eq!(
        check(&keys, &by(sites), now + 61_000),
        Err(Refused::Expired)
    );
    assert_eq!(
        check(&keys, &by(sites), now - 6_000),
        Err(Refused::FromTheFuture)
    );

    // Contextual calls: the recipient check applies whichever key signed.
    let context = WorkerContext::new(me, Some("willow"));
    let in_context = |key: &[u8]| sign_with_context(key, &who, "call", body, now, "n", &context);
    for key in [sites, shared] {
        let got = check(&keys, &in_context(key), now).unwrap();
        assert_eq!(got.instance(), Some("willow"));
        assert_eq!(
            keys.verify(
                Some(&in_context(key)),
                "call",
                body,
                now,
                "pigeon-ci-gateway"
            ),
            Err(Refused::WrongRecipient)
        );
    }
    assert_eq!(
        check(&keys, &in_context(ci), now),
        Err(Refused::BadSignature)
    );

    // Once the shared key is unbound (step 4), only the own key passes.
    let own_only = Keys {
        own: sites.to_vec(),
        shared: None,
    };
    assert!(check(&own_only, &by(sites), now).is_ok());
    assert_eq!(
        check(&own_only, &by(shared), now),
        Err(Refused::BadSignature)
    );
    assert_eq!(pigeon_contract::OWN_KEY, "CALLER_KEY_OWN");
    assert_eq!(pigeon_contract::SHARED_KEY, "CALLER_KEY");
}

/// `workers_setup` makes exactly the caller keys the store lacks, and keeps the rest.
#[test]
fn workers_setup_plans_only_missing_caller_keys() {
    use pigeon_capabilities::split::{caller_key_names, keys_to_make};

    assert_eq!(
        caller_key_names(),
        [
            "CALLER_KEY",
            "CALLER_KEY_CI_GATEWAY",
            "CALLER_KEY_MODEL_GATEWAY",
            "CALLER_KEY_SITES",
            "CALLER_KEY_PROXY",
            "CALLER_KEY_AI_AGENTS",
            "CALLER_KEY_CLOUD_GATEWAY",
        ]
    );
    // Today's store: only the shared key, among unrelated secrets.
    assert_eq!(
        keys_to_make(&["CALLER_KEY", "GITHUB_APP_KEY"]),
        [
            "CALLER_KEY_CI_GATEWAY",
            "CALLER_KEY_MODEL_GATEWAY",
            "CALLER_KEY_SITES",
            "CALLER_KEY_PROXY",
            "CALLER_KEY_AI_AGENTS",
            "CALLER_KEY_CLOUD_GATEWAY",
        ]
    );
    assert_eq!(
        keys_to_make(&["CALLER_KEY_SITES", "CALLER_KEY_PROXY"]),
        [
            "CALLER_KEY",
            "CALLER_KEY_CI_GATEWAY",
            "CALLER_KEY_MODEL_GATEWAY",
            "CALLER_KEY_AI_AGENTS",
            "CALLER_KEY_CLOUD_GATEWAY",
        ]
    );
    assert!(keys_to_make(&caller_key_names()).is_empty());
}

/// Core's `dispatch` must see the bound Workers' tools (`models_submit` is the model gateway's),
/// while a clashing remote tool is left out and the other ops use core's own capabilities.
#[test]
fn core_dispatch_reaches_bound_workers_tools() {
    use pigeon_capabilities::remote::{Binding, Remote};
    use pigeon_capabilities::{Call, Capability, Context, owner, split};
    use pigeon_contract::{CORE_OPS, CapabilityManifest};
    use pigeon_core::mcp::Tool;

    struct Fake;
    impl Capability for Fake {
        fn name(&self) -> &'static str {
            "lander"
        }
        fn tools(&self) -> Vec<Tool> {
            vec![
                Tool::bare("land", "Land."),
                Tool::bare("land_status", "Status."),
            ]
        }
        fn call<'a>(&'a self, _cx: &'a Context<'a>, _tool: &'a str, _args: &'a Value) -> Call<'a> {
            Box::pin(async { Ok(Value::Null) })
        }
    }
    let manifest = |v: Value| -> CapabilityManifest { serde_json::from_value(v).unwrap() };
    let models = Remote::new(
        &Binding {
            binding: "MODELS",
            worker: "pigeon-model-gateway",
        },
        manifest(json!({
            "name": "models",
            "tools": [{
                "name": "models_submit",
                "description": "Submit a model request.",
                "inputSchema": { "type": "object" }
            }]
        })),
    );
    let clash = Remote::new(
        &Binding {
            binding: "OTHER",
            worker: "pigeon-other",
        },
        manifest(json!({
            "name": "other",
            "tools": [{ "name": "land", "description": "Shadows core.", "inputSchema": { "type": "object" } }]
        })),
    );

    let local_only: Vec<Box<dyn Capability>> = vec![Box::new(Fake)];
    assert!(owner(&local_only, "models_submit").is_none());
    let merged = split::merge(
        vec![Box::new(Fake)],
        vec![Box::new(models), Box::new(clash)],
    );
    let names: Vec<&str> = merged.iter().map(|c| c.name()).collect();
    assert_eq!(names, ["lander", "models"]);
    let found = owner(&merged, "models_submit").expect("models_submit is reachable");
    assert_eq!(found.name(), "models");
    assert_eq!(owner(&merged, "land").unwrap().name(), "lander");

    assert!(split::op_needs_bound("dispatch"));
    for op in [
        "credential",
        "service_user",
        "verify",
        "grant",
        "deploy_grant",
        "nope",
    ] {
        assert!(!split::op_needs_bound(op), "{op}");
    }
    for op in CORE_OPS.iter().filter(|o| **o != "dispatch") {
        assert!(!split::op_needs_bound(op), "{op}");
    }
}

#[test]
fn deploy_proxy_body_max_fits_core() {
    const { assert!(pigeon_proxies::cf_proxy::BODY_MAX >= 48 * 1024 * 1024) };
}

#[test]
fn deploy_proxy_allowlist() {
    use pigeon_capabilities::cf_proxy::{
        ALLOWED, GRANT_PATH, PREFIX, Scope, allowed, body_ok, grant_answer, unlisted,
    };
    let (a, s) = ("4f94676cb2cbe46fd985b089ca97ea27", "pigeon-platform");
    let scope = |account, script| Scope {
        account,
        script,
        queues: &["pigeon-ci-events"],
        namespaces: &["ns1"],
    };
    let build = |pattern: &str, account: &str, script: &str| {
        format!(
            "{PREFIX}{}",
            pattern
                .replace("{account}", account)
                .replace("{script}", script)
                .replace("{version}", "01234567-89ab-cdef-0123-456789abcdef")
                .replace("{namespace}", "ns1")
                .replace("{bucket}", "pigeon-changelog")
        )
    };
    let query =
        |q: &[&str]| (!q.is_empty()).then(|| q.join("&").replace("{queue}", "pigeon-ci-events"));
    // An R2 bucket's own record may be read; not a name that is no bucket's, not its objects,
    // and not by any other method.
    let r2 = |method: &str, rest: &str| {
        allowed(
            method,
            &format!("{PREFIX}/accounts/{a}/r2/buckets/{rest}"),
            None,
            &scope(a, s),
        )
        .is_ok()
    };
    assert!(r2("GET", "pigeon-changelog"));
    assert!(!r2("GET", "Pigeon_Changelog"));
    assert!(!r2("GET", "-x-"));
    assert!(!r2("GET", "pigeon-changelog/objects/main/state.json"));
    assert!(!r2("PUT", "pigeon-changelog"));
    assert!(!r2("DELETE", "pigeon-changelog"));
    // Every call wrangler's deploy makes is allowed, for this account and Worker only.
    for e in &ALLOWED {
        let path = build(e.path, a, s);
        let q = query(e.query);
        assert_eq!(allowed(e.method, &path, q.as_deref(), &scope(a, s)), Ok(e));
        let other = build(e.path, "0000", s);
        assert!(allowed(e.method, &other, q.as_deref(), &scope(a, s)).is_err());
        for (m, p, q2) in [
            ("DELETE", path.clone(), q.clone()),
            (e.method, format!("{path}/extra"), q.clone()),
            (e.method, format!("{path}/.."), q.clone()),
            (e.method, path.replace(PREFIX, "/cf/client/v4x"), q.clone()),
            (e.method, path.clone(), Some("foo=bar".to_string())),
        ] {
            assert!(
                allowed(m, &p, q2.as_deref(), &scope(a, s)).is_err(),
                "{m} {p}"
            );
        }
    }
    // Nothing else on the account: other Workers, tokens, DNS, R2.
    for (m, p) in [
        (
            "GET",
            format!("{PREFIX}/accounts/{a}/workers/scripts/other"),
        ),
        ("POST", format!("{PREFIX}/user/tokens")),
        ("GET", format!("{PREFIX}/zones")),
        ("PUT", format!("{PREFIX}/accounts/{a}/r2/buckets/x")),
        ("GET", format!("{PREFIX}/accounts/{a}/secrets_store/stores")),
    ] {
        assert!(allowed(m, &p, None, &scope(a, s)).is_err(), "{m} {p}");
    }
    // A recorded deploy passes whole; a call off the list is named.
    let recorded: Vec<String> = ALLOWED
        .iter()
        .map(|e| {
            let q = query(e.query).map(|q| format!("?{q}")).unwrap_or_default();
            let path = build(e.path, a, s);
            format!("{} {}{q}", e.method, &path[PREFIX.len()..])
        })
        .collect();
    let mut calls: Vec<&str> = recorded.iter().map(String::as_str).collect();
    assert!(
        unlisted(&calls, &scope(a, s)).is_empty(),
        "{:?}",
        unlisted(&calls, &scope(a, s))
    );
    let extra = format!("DELETE /accounts/{a}/workers/scripts/{s}");
    calls.push(&extra);
    assert_eq!(unlisted(&calls, &scope(a, s)).len(), 1);
    // Bodies a rule checks: junk is refused.
    for e in ALLOWED
        .iter()
        .filter(|e| !matches!(e.body, pigeon_capabilities::cf_proxy::BodyRule::Any))
    {
        assert!(
            body_ok(e.body, b"not json", &scope(a, s)).is_err(),
            "{}",
            e.path
        );
    }
    // /cf/grant: only a GET with the job token of a landing that is deploying its commit.
    assert!(!GRANT_PATH.starts_with(PREFIX));
    assert!(allowed("GET", GRANT_PATH, None, &scope(a, s)).is_err());
    assert_eq!(grant_answer("GET", true, true).0, 200);
    assert_eq!(grant_answer("POST", true, true).0, 405);
    assert_eq!(grant_answer("GET", false, true).0, 403);
    assert_eq!(grant_answer("GET", true, false).0, 403);
}

#[test]
fn mirror_push() {
    use pigeon_capabilities::mirror::{Source, is_github, plan, rejected};
    use std::collections::BTreeMap;
    let source = Source {
        namespace: "pigeon-security".into(),
        repo: "pigeon".into(),
        branches: vec!["main".into()],
    };
    let refs = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect()
    };
    let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
    // The source (stubbed Artifacts) and the copy (stubbed GitHub).
    let theirs = refs(&[
        ("refs/heads/main", &b),
        ("refs/heads/claude/x", &c),
        ("refs/tags/v1", &a),
        ("refs/tags/v2", &b),
    ]);
    let ours = refs(&[("refs/heads/main", &a), ("refs/tags/v1", &c)]);
    let (report, updates) = plan(&source, &theirs, &ours);
    let outcomes: Vec<(&str, &str)> = report
        .iter()
        .map(|r| (r.name.as_str(), r.outcome.as_str()))
        .collect();
    // Session branches are never mirrored; a tag is never moved.
    assert_eq!(
        outcomes,
        [
            ("refs/heads/main", "updated"),
            ("refs/tags/v1", "diverged"),
            ("refs/tags/v2", "created")
        ]
    );
    let pushed: Vec<(&str, &str, &str)> = updates
        .iter()
        .map(|u| (u.name.as_str(), u.old.as_str(), u.new.as_str()))
        .collect();
    assert_eq!(
        pushed,
        [
            ("refs/heads/main", a.as_str(), b.as_str()),
            ("refs/tags/v2", pigeon_core::git::ZERO, b.as_str())
        ]
    );
    // Current already: nothing to push.
    let (_, none) = plan(
        &source,
        &refs(&[("refs/heads/main", &b)]),
        &refs(&[("refs/heads/main", &b)]),
    );
    assert!(none.is_empty());
    // The copy refuses one ref: it is reported, the rest stand.
    let mut report = report;
    rejected(
        &mut report,
        &[
            "ok refs/tags/v2".to_string(),
            "ng refs/heads/main non-fast-forward".to_string(),
        ],
    );
    assert_eq!(report[0].outcome, "rejected: non-fast-forward");
    assert_eq!(report[2].outcome, "created");
    // Only a GitHub copy is asked for its token's expiry.
    assert!(is_github("https://github.com/pigeon-security/pigeon.git"));
    for url in [
        "https://gitlab.com/a/b",
        "https://github.com.evil.example/a",
        "http://github.com/a/b",
    ] {
        assert!(!is_github(url), "{url}");
    }
}

#[test]
fn webhook_and_oidc() {
    use pigeon_ci::buildkite::{ISSUER, audience, vouched};
    use pigeon_core::buildkite_hook::{Event, read, verify};
    use pigeon_core::oidc::{self, Expect};
    use pigeon_core::webauthn::b64url;

    // (1) A webhook delivery: only a fresh signature over this body with this token verifies.
    let key = b"hook-secret";
    let body = br#"{"event":"build.finished","build":{"number":7}}"#;
    let now = 1_700_000_000_i64;
    let good = webhook_and_oidc_header(key, body, now - 10);
    assert!(verify(Some(good.as_str()), body, key, now, 300).is_ok());
    assert!(verify(Some(good.as_str()), b"{}", key, now, 300).is_err());
    assert!(verify(Some(good.as_str()), body, b"other-secret", now, 300).is_err());
    let old = webhook_and_oidc_header(key, body, now - 301);
    assert!(verify(Some(old.as_str()), body, key, now, 300).is_err());
    let ahead = webhook_and_oidc_header(key, body, now + 301);
    assert!(verify(Some(ahead.as_str()), body, key, now, 300).is_err());
    assert_eq!(
        verify(None, body, key, now, 300),
        Err("no signature".to_string())
    );
    let non_hex = format!("timestamp={},signature=zz{}", now - 10, "0".repeat(62));
    assert!(verify(Some(non_hex.as_str()), body, key, now, 300).is_err());
    assert!(verify(Some(good.as_str()), body, b"", now, 300).is_err());
    let (ts, sig) = good.split_once(",signature=").unwrap();
    let upper = format!("{ts},signature={}", sig.to_uppercase());
    assert!(verify(Some(upper.as_str()), body, key, now, 300).is_ok());
    assert!(matches!(
        read(Some("ping"), br#"{"zen":"hi"}"#),
        Ok(Event::Ping)
    ));

    // (2) A runner's OIDC token: verified, then vouched for by the connector.
    let aud = audience("platform.example");
    let header = json!({ "alg": "RS256", "kid": "k1", "typ": "JWT" });
    let claims = json!({
        "iss": ISSUER, "aud": aud, "iat": 1_000, "exp": 1_300,
        "organization_slug": "acme", "pipeline_slug": "pigeon", "build_number": 7,
        "build_id": "0190f5a8-8f8e-7c3a-9b1e-2f3c4d5e6f70",
        "job_id": "0190f5a8-9a9b-7c3a-9b1e-2f3c4d5e6f71",
        "agent_id": "0190f5a8-aaaa-7c3a-9b1e-2f3c4d5e6f72"
    });
    let jwks = webhook_and_oidc_jwks();
    let expect = Expect {
        issuer: ISSUER,
        audience: &aud,
        now: 1_100,
        leeway: 30,
        max_lifetime: 600,
    };
    let token = webhook_and_oidc_sign(&header, &claims);
    let got = oidc::verify(&token, &jwks, &expect).unwrap();
    assert_eq!(got.get("build_number"), Some(&json!(7)));
    let v = vouched(&got, "acme", "pigeon", false).unwrap();
    assert_eq!(v.build_id, "0190f5a8-8f8e-7c3a-9b1e-2f3c4d5e6f70");
    assert_eq!(v.build_number, 7);
    assert_eq!(v.job_id, "0190f5a8-9a9b-7c3a-9b1e-2f3c4d5e6f71");
    assert!(vouched(&got, "evil", "pigeon", false).is_err());
    assert!(vouched(&got, "acme", "other", false).is_err());
    let mut bad_id = got.clone();
    bad_id.insert("build_id".into(), json!("not-a-uuid"));
    assert!(vouched(&bad_id, "acme", "pigeon", false).is_err());
    let mut zero = got.clone();
    zero.insert("build_number".into(), json!(0));
    assert!(vouched(&zero, "acme", "pigeon", false).is_err());

    // Forged: another payload under the original signature, cut, or the wrong algorithm or key.
    let mut forged_claims = claims.clone();
    forged_claims["build_id"] = Value::from("0190f5a8-0000-7c3a-9b1e-2f3c4d5e6f99");
    let parts: Vec<&str> = token.split('.').collect();
    let forged = format!(
        "{}.{}.{}",
        parts[0],
        b64url(forged_claims.to_string().as_bytes()),
        parts[2]
    );
    assert_eq!(
        oidc::verify(&forged, &jwks, &expect),
        Err("the signature does not verify".to_string())
    );
    assert!(oidc::verify(&token[..token.len() - 10], &jwks, &expect).is_err());
    for h in [
        json!({ "alg": "none", "kid": "k1", "typ": "JWT" }),
        json!({ "alg": "HS256", "kid": "k1", "typ": "JWT" }),
        json!({ "alg": "RS256", "kid": "k2", "typ": "JWT" }),
    ] {
        assert!(oidc::verify(&webhook_and_oidc_sign(&h, &claims), &jwks, &expect).is_err());
    }
    let other_aud = audience("elsewhere.example");
    assert!(
        oidc::verify(
            &token,
            &jwks,
            &Expect {
                audience: &other_aud,
                ..expect
            }
        )
        .is_err()
    );
    assert!(
        oidc::verify(
            &token,
            &jwks,
            &Expect {
                issuer: "https://evil.example",
                ..expect
            }
        )
        .is_err()
    );
    assert!(
        oidc::verify(
            &token,
            &jwks,
            &Expect {
                now: 1_331,
                ..expect
            }
        )
        .is_err()
    );
}

fn webhook_and_oidc_header(key: &[u8], body: &[u8], secs: i64) -> String {
    use pigeon_core::sign::{hex, hmac_sha256};
    let mut message = format!("{secs}.").into_bytes();
    message.extend_from_slice(body);
    format!(
        "timestamp={secs},signature={}",
        hex(&hmac_sha256(key, &message))
    )
}

pub(super) fn webhook_and_oidc_key() -> &'static rsa::RsaPrivateKey {
    use std::sync::OnceLock;
    static KEY: OnceLock<rsa::RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
}

fn webhook_and_oidc_jwks() -> Value {
    use pigeon_core::webauthn::b64url;
    use rsa::traits::PublicKeyParts;
    let public = webhook_and_oidc_key().to_public_key();
    json!({ "keys": [{
        "kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig",
        "n": b64url(&public.n().to_bytes_be()), "e": b64url(&public.e().to_bytes_be())
    }] })
}

fn webhook_and_oidc_sign(header: &Value, claims: &Value) -> String {
    use pigeon_core::webauthn::b64url;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use sha2::Sha256;
    let signed = format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes())
    );
    let sig = SigningKey::<Sha256>::new(webhook_and_oidc_key().clone()).sign(signed.as_bytes());
    format!("{signed}.{}", b64url(&sig.to_bytes()))
}

#[test]
fn proxy_and_job_tokens() {
    use pigeon_capabilities::proxy::{self, Audit, DEFAULT_DAYS, Outcome, ProxyCaller};
    use pigeon_ci::token::{self, Grant, PREFIX};

    // A session's proxy credential reaches the proxies only; an access token only /mcp.
    let paths = [
        "/git/x",
        "/cf/client/v4/x",
        "/git",
        "/gitx/",
        "/cf",
        "/cfx",
        "/mcp",
        "/mcp/",
        "/tokens",
        "/git/a%2Fb",
        "/git/a%2fb",
        "/git/a/../b",
        "/git/./a",
        "/cf/a\\b",
        "/git/%2E%2E/x",
    ];
    for p in paths {
        assert_eq!(
            proxy::reaches("proxy", p),
            p == "/git/x" || p == "/cf/client/v4/x",
            "proxy {p}"
        );
        assert_eq!(proxy::reaches("access", p), p == "/mcp", "access {p}");
        for k in ["refresh", "", "Proxy"] {
            assert!(!proxy::reaches(k, p), "{k} {p}");
        }
    }
    let (px, pl) = ("proxy.pigeon.dev", "platform.pigeon.dev");
    for (h, p) in [
        (px, "/git/ns/r.git/info/refs"),
        (px, "/cf/client/v4/accounts"),
        (px, "/health"),
        ("PROXY.pigeon.dev", "/git/x"),
        (pl, "/mcp"),
        (pl, "/tokens"),
    ] {
        assert!(proxy::host_serves(h, px, p), "{h} {p}");
    }
    for (h, p) in [
        (px, "/mcp"),
        (px, "/tokens"),
        (px, "/oauth/token"),
        (px, "/"),
        (pl, "/git/ns/r.git/info/refs"),
        (pl, "/cf/client/v4/x"),
    ] {
        assert!(!proxy::host_serves(h, px, p), "{h} {p}");
    }
    for bad in [
        String::new(),
        "   ".into(),
        "a".repeat(65),
        "a\nb".into(),
        "a\u{7}".into(),
    ] {
        assert!(proxy::label(&bad).is_err(), "label {bad:?}");
    }
    assert_eq!(proxy::label(" cloud env ").as_deref(), Ok("cloud env"));
    assert!(proxy::label(&"é".repeat(64)).is_ok());
    assert_eq!(proxy::days(&Value::Null), Ok(DEFAULT_DAYS));
    assert_eq!(proxy::days(&json!(90)), Ok(90));
    for bad in [json!(0), json!(91), json!(-1), json!(1.5), json!("30")] {
        assert!(proxy::days(&bad).is_err(), "days {bad}");
    }
    assert_eq!(proxy::id("abcdefghij_-"), Ok("abcdefghij_-"));
    for bad in [
        "abcdefghijk",
        "abcdefghijklm",
        "abcdefghij+/",
        "abcdefghij%2",
    ] {
        assert!(proxy::id(bad).is_err(), "id {bad}");
    }
    // The audit line drops the query and names the caller, never a secret.
    let audit = Audit {
        route: "git",
        method: "GET",
        path: "/git/x/info/refs?service=git-upload-pack",
        outcome: Outcome::Forwarded,
        status: Some(200),
        why: None,
    };
    let anon: Value = serde_json::from_str(&proxy::audit_line(None, &audit)).unwrap();
    assert_eq!(anon["path"], "/git/x/info/refs");
    assert!(anon["caller"].is_null() && anon["token"].is_null());
    assert_eq!(anon["outcome"], "forwarded");
    let caller = ProxyCaller {
        id: "abcdefghijkl".into(),
        label: "env".into(),
        person: "Ana".into(),
        script: None,
        branch_prefix: None,
    };
    let named: Value = serde_json::from_str(&proxy::audit_line(Some(&caller), &audit)).unwrap();
    // Logs are kept: the credential's id, never its label or the person's name.
    assert!(named.get("caller").is_none() && !named.to_string().contains("Ana"));
    assert_eq!(named["token"], "abcdefghijkl");
    assert_eq!(named["status"], 200);

    // A CI job's token: its shape, then its grant, then where it reaches.
    let a = token::lookup_hash(&format!("{PREFIX}{}", "A".repeat(43)));
    assert!(a.is_some());
    assert_ne!(
        a,
        token::lookup_hash(&format!("{PREFIX}{}", "B".repeat(43)))
    );
    for bad in [
        format!("pgn_px_{}", "A".repeat(43)),
        format!("{PREFIX}{}", "A".repeat(42)),
        format!("{PREFIX}{}", "A".repeat(44)),
        format!("{PREFIX}{}+", "A".repeat(42)),
        String::new(),
    ] {
        assert!(token::lookup_hash(&bad).is_none(), "{bad}");
    }
    let c = "0123456789abcdef0123456789abcdef01234567";
    let repo = "pigeon-security/pigeon";
    let g = Grant {
        job: "j1".into(),
        repo: repo.into(),
        commit: c.into(),
        expires_at: 1000,
    };
    assert!(token::allows_job(&g, 999, "j1"));
    assert!(!token::allows_job(&g, 1000, "j1") && !token::allows_job(&g, 0, "j2"));
    assert!(token::allows_fetch(&g, 0, repo, c));
    assert!(token::allows_fetch(&g, 0, repo, &c.to_uppercase()));
    assert!(!token::allows_fetch(&g, 1000, repo, c));
    assert!(!token::allows_fetch(&g, 0, "pigeon-security/other", c));
    assert!(!token::allows_fetch(
        &g,
        0,
        repo,
        "1123456789abcdef0123456789abcdef01234567"
    ));
    assert!(!token::allows_fetch(&g, 0, repo, "main"));
    for p in [
        "/ci/runner/abc",
        "/git/ns/r.git/info/refs",
        "/git/ns/r.git/git-upload-pack",
    ] {
        assert!(token::reaches(p), "{p}");
    }
    let long = format!("/ci/runner/{}", "a".repeat(65));
    for p in [
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
        &long,
    ] {
        assert!(!token::reaches(p), "{p}");
    }
}

/// The proxy Worker's per-isolate memory: a scoped credential reused until a minute before it
/// expires; a verified proxy credential kept a minute, by its hash, never its value; and the
/// deploy proxy it builds matching core's. Grants are never kept, so there is nothing to keep.
#[test]
fn proxy_worker_caches_and_core_ops() {
    use pigeon_capabilities::split::purpose_permissions;
    use pigeon_contract::{CALLERLESS_OPS, CORE_OPS, Credential};
    use pigeon_proxies::cache::{
        Credentials, RENEW_BEFORE_MS, VERIFIED_FOR_MS, VERIFIED_MAX, Verified,
    };
    use pigeon_proxies::{ProxyCaller, Purpose};

    // Scoped credentials, one per purpose, renewed a minute before they expire.
    let mut creds = Credentials::new();
    assert!(creds.get(Purpose::Git, 0).is_none());
    let minted = |token: &str, expires_at: i64| Credential {
        token: token.into(),
        account_id: "acct".into(),
        expires_at,
        token_id: None,
    };
    creds.put(Purpose::Git, minted("git-1", 900_000));
    assert_eq!(creds.get(Purpose::Git, 0).unwrap().token, "git-1");
    assert_eq!(
        creds
            .get(Purpose::Git, 900_000 - RENEW_BEFORE_MS - 1)
            .unwrap()
            .token,
        "git-1"
    );
    assert!(creds.get(Purpose::Git, 900_000 - RENEW_BEFORE_MS).is_none());
    assert!(creds.get(Purpose::Deploy, 0).is_none(), "kept per purpose");
    creds.put(Purpose::Git, minted("git-2", 2_000_000));
    assert_eq!(creds.get(Purpose::Git, 1_000_000).unwrap().token, "git-2");
    assert_eq!(Purpose::Git.scoped(), "artifacts");
    assert_eq!(Purpose::Deploy.scoped(), "cf_deploy");

    // Verified proxy credentials: kept a minute, by hash; an unknown one is never kept.
    let holder = |id: &str| ProxyCaller {
        id: id.into(),
        label: "env".into(),
        person: "Ana".into(),
        script: None,
        branch_prefix: None,
    };
    let (a, b) = (
        format!("pgn_px_{}", "a".repeat(43)),
        format!("pgn_px_{}", "b".repeat(43)),
    );
    let mut verified = Verified::new();
    assert!(verified.get(&a, 0).is_none());
    verified.put(&a, holder("aaaaaaaaaaaa"), 1_000);
    assert_eq!(verified.get(&a, 1_000).unwrap().id, "aaaaaaaaaaaa");
    assert_eq!(
        verified.get(&a, 1_000 + VERIFIED_FOR_MS - 1).unwrap().id,
        "aaaaaaaaaaaa"
    );
    assert!(
        verified.get(&a, 1_000 + VERIFIED_FOR_MS).is_none(),
        "revocation within a minute"
    );
    assert!(verified.get(&b, 1_000).is_none(), "keyed by the credential");
    assert_ne!(Verified::key(&a), a, "never the credential itself");
    assert_eq!(
        Verified::key(&a),
        pigeon_core::oauth::hash(&a),
        "the Directory's hash"
    );
    assert!(!Verified::key(&a).contains("pgn_px_"));
    // Expired ones go on the next put, and the cache stays bounded.
    verified.put(&b, holder("bbbbbbbbbbbb"), 1_000 + VERIFIED_FOR_MS);
    assert_eq!(verified.len(), 1);
    for i in 0..VERIFIED_MAX + 10 {
        verified.put(
            &format!("pgn_px_{i:043}"),
            holder("cccccccccccc"),
            2_000_000 + i as i64,
        );
    }
    assert_eq!(verified.len(), VERIFIED_MAX);
    assert!(
        verified
            .get(&format!("pgn_px_{:043}", 0), 2_000_100)
            .is_none(),
        "oldest went first"
    );
    assert!(
        verified
            .get(&format!("pgn_px_{:043}", VERIFIED_MAX + 9), 2_000_100)
            .is_some()
    );

    // The ops core answers the proxy Worker: only its checks go without a caller.
    for op in CALLERLESS_OPS {
        assert!(CORE_OPS.contains(&op), "{op}");
    }
    assert!(!CALLERLESS_OPS.contains(&"credential") && !CALLERLESS_OPS.contains(&"dispatch"));
    // `cf_deploy` is exactly the union of what the deploy proxy's allowlist names.
    let mut union: Vec<&str> = pigeon_proxies::cf_proxy::ALLOWED
        .iter()
        .flat_map(|e| e.permissions.iter().copied())
        .collect();
    union.sort_unstable();
    union.dedup();
    let mut deploy: Vec<&str> = purpose_permissions("cf_deploy").unwrap().to_vec();
    deploy.sort_unstable();
    assert_eq!(deploy, union);

    // The proxy Worker's deploy proxy, from the files built into it, is core's.
    let cf = pigeon_proxies::cf_proxy::CfProxy::platform(
        "proxy.pigeon.dev",
        "pigeon-platform",
        include_str!("../../src/platform/wrangler.toml"),
        include_str!("../../ci/workers.toml"),
        "pigeon-security/pigeon",
    );
    let core = pigeon_capabilities::architecture::Wrangler::read(include_str!(
        "../../src/platform/wrangler.toml"
    ));
    assert_eq!(cf.queues, core.producers);
    assert_eq!(
        cf.containers,
        core.containers
            .iter()
            .map(|c| c.class.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(cf.queues, ["pigeon-ci-events"]);
    assert_eq!(cf.containers, ["CiRunner"]);
    let others: Vec<&str> = cf.others.iter().map(|o| o.script.as_str()).collect();
    assert_eq!(
        others,
        [
            "pigeon-sites",
            "pigeon-model-gateway",
            "pigeon-cloud-gateway",
            "pigeon-ci-gateway",
            "pigeon-proxy",
            "pigeon-ai-agents"
        ]
    );
    let broken = pigeon_proxies::cf_proxy::CfProxy::platform("h", "s", "not toml [", "nope", "r");
    assert!(broken.others.is_empty() && broken.queues.is_empty());

    // The cutover moves the proxy host in one call, `setup_proxy_host {worker}`, and back.
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::public_host::PublicHost;
    let host = |also: Vec<String>| PublicHost {
        zone: "pigeon.dev".into(),
        host: "proxy.pigeon.dev".into(),
        worker: "pigeon-platform".into(),
        also,
        tool: "setup_proxy_host",
    };
    let tools = host(vec!["pigeon-proxy".into()]).tools();
    assert_eq!(
        tools[0].input_schema["properties"]["worker"]["enum"],
        json!(["pigeon-platform", "pigeon-proxy"])
    );
    assert_eq!(
        host(Vec::new()).tools()[0].input_schema["properties"],
        json!({})
    );
}

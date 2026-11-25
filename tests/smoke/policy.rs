//! Policy decisions, the git proxy's push rules and tool-call approvals.

use super::*;

#[test]
fn policy_decisions() {
    use pigeon_core::policy::{self, Approval, Decision, NO_MATCH};
    let text = pigeon_capabilities::policy::TEXT;
    let p = policy::parse(text).unwrap();
    let ids: Vec<&str> = p.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "agents-coordinate",
            "land-and-deploy",
            "deploy-after-landing",
            "session-branches",
            "main-forward-only",
            "policy-is-yours",
            "smoke-suite-fixed",
            "smoke-suite-grows",
            "gate-changes",
            "risky-tools-need-passkey",
            "tools-run",
        ]
    );
    // The rules name no provider: the session prefix is a placeholder.
    assert!(
        !text
            .lines()
            .any(|l| !l.starts_with('#') && l.contains("claude"))
    );
    let decide = |a: &str, t: &str, ap: Option<&str>| {
        let got = p.evaluate(&vars(), &request(a, t, ap));
        // The platform's own entry point answers the same.
        assert_eq!(
            got,
            pigeon_capabilities::policy::decide(&vars(), &request(a, t, ap))
        );
        got
    };
    let allow = |a: &str, t: &str, ap: Option<&str>, rule: &str| {
        let got = decide(a, t, ap);
        assert_eq!(
            (got.decision, got.rule.as_deref()),
            (Decision::Allow, Some(rule)),
            "{a} {t}"
        );
    };
    allow("message_session", "", None, "agents-coordinate");
    allow("handoff_branch", "claude/x", None, "agents-coordinate");
    allow("land", "claude/x", None, "land-and-deploy");
    allow("deploy", "claude/x", None, "land-and-deploy");
    allow("rewrite_branch", "claude/a/b", None, "session-branches");
    allow("add_test", "tests/smoke.rs", None, "smoke-suite-grows");
    allow("delete_branch", "claude/x", None, "session-branches");
    allow(
        "change_policy",
        ".pigeon/policy.toml",
        Some("passkey"),
        "policy-is-yours",
    );
    allow(
        "change_gate",
        "ci/runner/test.sh",
        Some("delegate"),
        "gate-changes",
    );
    allow(
        "change_gate",
        ".pigeon/ci.toml",
        Some("passkey"),
        "gate-changes",
    );
    // Denied by a rule, or needing an approval the request lacks.
    let main = decide("move_main", "main", Some("passkey"));
    assert_eq!(
        (main.decision, main.rule.as_deref()),
        (Decision::Deny, Some("main-forward-only"))
    );
    let policy = decide("change_policy", ".pigeon/policy.toml", Some("delegate"));
    assert_eq!(
        (policy.decision, policy.needs),
        (Decision::Deny, Some(Approval::Passkey))
    );
    let gate = decide("change_gate", "ci/", None);
    assert_eq!(
        (gate.decision, gate.needs),
        (Decision::Deny, Some(Approval::Delegate))
    );
    let test = decide("add_test", "src/core/src/git.rs", None);
    assert_eq!(test.rule.as_deref(), Some("smoke-suite-fixed"));
    let deploy = decide("deploy", "main", None);
    assert_eq!(
        (deploy.decision, deploy.rule.as_deref()),
        (Decision::Deny, Some("deploy-after-landing"))
    );
    // No match: deny, "not in policy".
    for (a, t) in [
        ("land", "main"),
        ("push_branch", "main"),
        ("push_branch", "refs/tags/v1"),
        ("push_branch", "claudex/y"),
        ("change_gate", "src/core/src/lib.rs"),
    ] {
        let got = decide(a, t, None);
        assert_eq!(
            (got.decision, got.rule, got.reason.as_str()),
            (Decision::Deny, None, NO_MATCH),
            "{a} {t}"
        );
    }
    // An unknown kind, key or shape refuses the request or the whole file.
    assert!(
        serde_json::from_value::<policy::Request>(json!({ "actor": "a", "action": "steer" }))
            .is_err()
    );
    assert!(
        serde_json::from_value::<policy::Request>(
            json!({ "actor": "a", "action": "land", "context": { "approval": "admin" } })
        )
        .is_err()
    );
    let rule = |extra: &str| {
        format!(
            "version = 1\n[[rule]]\nid = \"r\"\nactions = [\"land\"]\ndecision = \"allow\"\nreason = \"x\"\n{extra}"
        )
    };
    assert!(policy::parse(&rule("")).is_ok());
    for bad in [
        rule("when = \"always\""),
        rule("targets = [\"a*b\"]"),
        rule("targets = [\"@other/*\"]"),
        rule("targets = [\"../x\"]"),
        rule("approval = \"admin\""),
        rule("[[rule]]\nid = \"r\"\nactions = [\"land\"]\ndecision = \"deny\"\nreason = \"y\""),
        rule("").replace("\"land\"", "\"steer\""),
        rule("").replace("\"land\"", "\"land\", \"land\""),
        rule("").replace("version = 1", "version = 2"),
        rule("").replace(
            "decision = \"allow\"",
            "decision = \"deny\"\napproval = \"passkey\"",
        ),
        "version = 1".to_string(),
        "not toml [".to_string(),
    ] {
        assert!(policy::parse(&bad).is_err(), "{bad}");
    }
    // Each decision's audit line names the rule and never more than the request.
    let req = request("land", "claude/x", None);
    let line: Value = serde_json::from_str(&pigeon_capabilities::policy::audit_line(
        "policy_check",
        &req,
        &p.evaluate(&vars(), &req),
    ))
    .unwrap();
    assert_eq!(line["audit"], "policy");
    assert_eq!(line["rule"], "land-and-deploy");
    assert_eq!(line["decision"], "allow");
    assert!(line.get("actor").is_none());
}

#[test]
fn git_proxy_push_rules() {
    use pigeon_capabilities::git_proxy::{
        Call, Rule, Service, check_all, moves_forward, refname_ok, route,
    };
    use pigeon_core::git::{self, Command as Update, ZERO};
    const P: &str = "refs/heads/main";
    const X: &str = "refs/heads/claude/";
    // Only git's four calls on this one repository are routed, compared exactly.
    let r = |m: &str, p: &str, q: Option<&str>| route(m, p, q, "pigeon-security", "pigeon");
    let base = "/git/pigeon-security/pigeon.git";
    assert_eq!(
        r(
            "GET",
            &format!("{base}/info/refs"),
            Some("service=git-receive-pack")
        ),
        Ok(Call::Refs(Service::ReceivePack))
    );
    assert_eq!(
        r("POST", &format!("{base}/git-upload-pack"), None),
        Ok(Call::Rpc(Service::UploadPack))
    );
    for (m, p, q) in [
        (
            "GET",
            format!("{base}/info/refs"),
            Some("service=git-other"),
        ),
        ("PUT", format!("{base}/git-receive-pack"), None),
        (
            "POST",
            "/git/pigeon-security/other.git/git-receive-pack".into(),
            None,
        ),
        ("POST", format!("{base}/../x.git/git-receive-pack"), None),
        ("POST", format!("{base}/git-receive-pack%2F"), None),
    ] {
        assert!(r(m, &p, q).is_err(), "{m} {p}");
    }
    assert!(refname_ok("refs/heads/claude/a/b-1.2_x"));
    for bad in [
        "refs/heads/claude/../main",
        "refs/heads/a..b",
        "heads/x",
        "refs/heads/x.lock",
        "refs/heads/x/",
    ] {
        assert!(!refname_ok(bad), "{bad}");
    }

    // A real push to a session branch, as git sends it, read back by the proxy.
    let dir = repo("push");
    let a = commit(&dir, "a");
    let b = commit(&dir, "a\nb");
    let line = format!("{a} {b} refs/heads/claude/x\0report-status side-band-64k\n");
    let mut body = format!("{:04x}{line}0000", line.len() + 4).into_bytes();
    body.extend(pack(&dir, &b, &a));
    let pushed = git::commands(&body).unwrap().unwrap();
    assert_eq!(check_all(&pushed.commands, P, X), Ok(vec![Rule::Stream]));
    // Truncated or junk ref updates are not read as a push.
    assert!(git::commands(b"00zzjunk").is_err());

    let cmd = |old: &str, new: &str, name: &str| Update {
        old: old.into(),
        new: new.into(),
        name: name.into(),
    };
    // Allowed by `session-branches`: create, rewrite (any old) and delete a session branch.
    for c in [
        cmd(ZERO, &a, "refs/heads/claude/new"),
        cmd(&b, &a, "refs/heads/claude/x"),
        cmd(&a, ZERO, "refs/heads/claude/x"),
    ] {
        assert_eq!(check_all(&[c], P, X), Ok(vec![Rule::Stream]));
    }
    // Refused: main never moves by a push (`main-forward-only`), forward or not; anything else
    // is not in policy. Every refused update is named, with the rule.
    let tried = [
        cmd(&a, &b, P),
        cmd(&b, &a, P),
        cmd(&a, ZERO, P),
        cmd(ZERO, &a, "refs/tags/v1"),
        cmd(ZERO, &a, "refs/heads/other"),
        cmd(ZERO, &a, "refs/heads/claude/ok"),
    ];
    let refused = check_all(&tried, P, X).unwrap_err();
    let names: Vec<&str> = refused.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, [P, P, P, "refs/tags/v1", "refs/heads/other"]);
    assert!(
        refused[0].1.contains("main-forward-only"),
        "{}",
        refused[0].1
    );
    assert!(refused[3].1.contains("not in policy"), "{}", refused[3].1);
    // git reads the refusal as a refused ref, naming why.
    let answer = String::from_utf8_lossy(&git::refusal(&tried, &pushed.caps, &refused)).to_string();
    assert!(answer.contains("ng refs/heads/main"), "{answer}");

    // The landing's fast-forward, judged from the pack Artifacts answers: forward only.
    let c = commit(&dir, "a\nb\nc");
    git(&dir, &["checkout", "-q", "-b", "side", &a], None);
    let d = commit(&dir, "side");
    assert_eq!(moves_forward(&a, &c, &pack(&dir, &c, &a)), Ok(true));
    assert_eq!(moves_forward(&c, &c, &[]), Ok(true));
    assert_eq!(moves_forward(&c, &a, &pack(&dir, &a, &c)), Ok(false));
    assert_eq!(moves_forward(&c, &d, &pack(&dir, &d, &c)), Ok(false));
    // A malformed pack is refused, not misread.
    assert!(moves_forward(&a, &c, b"PACK junk").is_err());
    let mut cut = pack(&dir, &c, &a);
    cut.truncate(cut.len() / 2);
    assert!(moves_forward(&a, &c, &cut).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A run's git grant pushes, rewrites (a force-push after a rebase) and deletes only its own
/// branch and `{branch}-…`: never main, a tag, another session branch, or another run's branch
/// whose name starts with this run's (run `ab`, branch `claude/ai-agent-abc`). The policy
/// still allows the rewrite itself (`session-branches`), so the grant's scope is what keeps a
/// force-push on the run's own branch.
#[test]
fn git_proxy_run_grant_rewrites_only_its_own_branch() {
    use pigeon_capabilities::git_proxy::{Rule, check_all};
    use pigeon_capabilities::proxy::run_branch_prefix;
    use pigeon_core::git::{self, Command as Update, ZERO};
    use pigeon_core::proxy_caller::within_prefix;
    const P: &str = "refs/heads/main";
    const X: &str = "refs/heads/claude/";
    let prefix = run_branch_prefix("ab").unwrap();
    let own = format!("refs/heads/{prefix}");

    // A real rewrite of the run's branch, as `git push --force-with-lease` sends it: the old
    // tip is not in the new one's history.
    let dir = repo("run-rewrite");
    let a = commit(&dir, "a");
    let b = commit(&dir, "a\nb");
    git(&dir, &["checkout", "-q", "-b", "rebased", &a], None);
    let c = commit(&dir, "a\nc");
    let line = format!("{b} {c} {own}\0report-status side-band-64k\n");
    let mut body = format!("{:04x}{line}0000", line.len() + 4).into_bytes();
    body.extend(pack(&dir, &c, &a));
    let pushed = git::commands(&body).unwrap().unwrap();
    assert!(within_prefix(pushed.commands.iter().map(|u| u.name.as_str()), &prefix).is_ok());
    assert_eq!(check_all(&pushed.commands, P, X), Ok(vec![Rule::Stream]));

    // Inside the grant: the branch itself and `{branch}-…`, pushed, rewritten or deleted.
    let wip = format!("{own}-wip");
    assert!(within_prefix([own.as_str(), wip.as_str()], &prefix).is_ok());
    let cmd = |old: &str, new: &str, name: &str| Update {
        old: old.into(),
        new: new.into(),
        name: name.into(),
    };
    for u in [cmd(ZERO, &a, &own), cmd(&b, &c, &own), cmd(&c, ZERO, &wip)] {
        assert_eq!(check_all(&[u], P, X), Ok(vec![Rule::Stream]));
    }

    // Outside it, each named: another run's branch that shares the prefix, other session
    // branches, main, a tag, and a path that climbs out.
    let tried = [
        own.as_str(),
        "refs/heads/claude/ai-agent-abc",
        "refs/heads/claude/ai-agent-abc-wip",
        "refs/heads/claude/ai-agent-a",
        "refs/heads/claude/ai-agent-ab/x",
        "refs/heads/claude/someone",
        P,
        "refs/tags/claude/ai-agent-ab",
        "refs/heads/claude/ai-agent-ab-../main",
    ];
    assert_eq!(within_prefix(tried, &prefix).unwrap_err(), tried[1..]);
    // Main stays refused by the policy too, forced or not.
    assert!(check_all(&[cmd(&b, &c, P)], P, X).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A run's git grant is revoked when the run ends: the op is core's and signed for a caller,
/// both ends agree on whose grant it is, and one run's scope never reaches another's.
#[test]
fn core_run_grant_revoked_when_the_run_ends() {
    use pigeon_capabilities::proxy::run_branch_prefix;
    use pigeon_capabilities::split::grant_person;
    use pigeon_capabilities::users::Caller;
    use pigeon_contract::{CALLERLESS_OPS, CORE_OPS, CredentialRequest};

    assert!(CORE_OPS.contains(&"credential_revoke"));
    assert!(!CALLERLESS_OPS.contains(&"credential_revoke"));

    let user =
        |id: &str, kind: &str| json!({ "id": id, "kind": kind, "name": id, "created_at": 0 });
    let caller = |user: Value, acting_for: Value| -> Caller {
        serde_json::from_value(json!({ "user": user, "acting_for": acting_for })).unwrap()
    };
    let standard = caller(user("ana", "standard"), Value::Null);
    assert_eq!(grant_person(Some(&standard)), Ok("ana".to_string()));
    let acting = caller(user("bot", "service"), user("ben", "standard"));
    assert_eq!(grant_person(Some(&acting)), Ok("ben".to_string()));
    let alone = caller(user("bot", "service"), Value::Null);
    assert!(grant_person(Some(&alone)).is_err());
    assert!(grant_person(None).is_err());

    let body = pigeon_ai_agents::object::revoke_request("r1");
    let parsed: CredentialRequest = serde_json::from_value(body).unwrap();
    assert_eq!(parsed.kind, "run_grant");
    assert_eq!(parsed.instance.as_deref(), Some("r1"));

    assert_ne!(run_branch_prefix("ab"), run_branch_prefix("abc"));
}

/// The six tools the person gates with their passkey (docs/plans/tool-approvals.md).
const GATED: [&str; 6] = [
    "deploy_token",
    "source_token",
    "setup_platform",
    "users_remove",
    "passkeys_remove",
    "oauth_revoke",
];

#[test]
fn tool_call_approvals() {
    use pigeon_capabilities::approvals;
    use pigeon_capabilities::console::write::{Policy, action};
    use pigeon_core::approval::{self, Kind, Refusal, State};
    use pigeon_core::policy::{self, Approval, Decision};

    // The person's rules: each of the six needs the passkey; every other tool runs as before.
    let p = policy::parse(pigeon_capabilities::policy::TEXT).unwrap();
    for tool in GATED {
        let a = p.evaluate(&vars(), &request("call_tool", tool, None));
        assert_eq!(
            (a.decision, a.needs, a.rule.as_deref()),
            (
                Decision::Deny,
                Some(Approval::Passkey),
                Some("risky-tools-need-passkey")
            ),
            "{tool}"
        );
        let a = p.evaluate(&vars(), &request("call_tool", tool, Some("passkey")));
        assert_eq!(a.decision, Decision::Allow, "{tool} with the passkey");
    }
    for tool in ["land", "policy_check", "revoke_api_token", "call"] {
        let a = p.evaluate(&vars(), &request("call_tool", tool, None));
        assert_eq!(
            (a.decision, a.rule.as_deref()),
            (Decision::Allow, Some("tools-run")),
            "{tool}"
        );
    }

    // The policy built into the Worker has the person's rules: every call is asked about, the
    // six are gated, and only their schemas gain the approval argument.
    assert!(approvals::asked());
    for tool in GATED {
        assert!(approvals::gated(tool), "{tool}");
    }
    assert!(!approvals::gated("land") && !approvals::gated("call"));
    let mut tools = vec![
        pigeon_core::mcp::Tool::bare("deploy_token", "mint"),
        pigeon_core::mcp::Tool::bare("land", "land"),
    ];
    approvals::annotate(&mut tools);
    assert!(
        tools[0].input_schema["properties"]
            .get("approval")
            .is_some()
    );
    assert!(
        tools[1].input_schema["properties"]
            .get("approval")
            .is_none()
    );

    // A call's digest is of the call: key order does not matter; arguments and caller do.
    let args = json!({ "ttl": 300, "permissions": ["Workers Scripts Write"] });
    let digest = approval::call_digest("deploy_token", &args, "svc");
    assert_eq!(
        digest,
        approval::call_digest(
            "deploy_token",
            &json!({ "permissions": ["Workers Scripts Write"], "ttl": 300 }),
            "svc"
        )
    );
    let other_args = approval::call_digest("deploy_token", &json!({ "ttl": 301 }), "svc");
    assert_ne!(digest, other_args);
    assert_ne!(digest, approval::call_digest("deploy_token", &args, "svc2"));
    assert_ne!(digest, approval::call_digest("source_token", &args, "svc"));

    // The person sees the call with secrets redacted; the digest is of the call as asked.
    let shown =
        approval::redact(&json!({ "ttl": 1, "api_token": "t", "nested": [{ "Secret": 1 }] }));
    assert_eq!(
        shown,
        json!({ "ttl": 1, "api_token": approval::REDACTED, "nested": [{ "Secret": approval::REDACTED }] })
    );

    let t0 = 1_000_000;
    let fresh = || {
        approval::new(
            "ap1".into(),
            Kind::ToolCall,
            "deploy_token".into(),
            digest.clone(),
            json!({ "tool": "deploy_token", "args": args, "caller": "svc" }),
            Value::Null,
            "svc".into(),
            Some("willow".into()),
            t0,
        )
    };

    // Pending: it does not run, and only its person approves it, for its own digest.
    let mut r = fresh();
    assert_eq!(
        (r.state, r.expires_ms),
        (State::Pending, t0 + approval::TOOL_CALL_TTL_MS)
    );
    assert_eq!(
        approval::take(&mut r, &digest, "svc", t0 + 1),
        Err(Refusal::NotApproved("ap1".into()))
    );
    assert_eq!(
        approval::decide(&mut r, true, &other_args, "willow", t0 + 2),
        Err(Refusal::OtherCall("ap1".into()))
    );
    assert_eq!(
        approval::decide(&mut r, true, &digest, "mallory", t0 + 2),
        Err(Refusal::NotYours("ap1".into()))
    );
    assert_eq!(
        approval::purpose("ap1", &digest),
        format!("console:approval:ap1:{digest}")
    );
    approval::decide(&mut r, true, &digest, "willow", t0 + 3).unwrap();
    assert_eq!(r.state, State::Approved);

    // Approved: another caller or other arguments do not run it; the call itself runs once.
    assert_eq!(
        approval::take(&mut r, &digest, "svc2", t0 + 4),
        Err(Refusal::OtherCaller("ap1".into()))
    );
    assert_eq!(
        approval::take(&mut r, &other_args, "svc", t0 + 4),
        Err(Refusal::OtherCall("ap1".into()))
    );
    approval::take(&mut r, &digest, "svc", t0 + 5).unwrap();
    assert_eq!(r.state, State::Used);
    assert_eq!(
        approval::take(&mut r, &digest, "svc", t0 + 6),
        Err(Refusal::Used("ap1".into()))
    );

    // Denied: it never runs.
    let mut r = fresh();
    approval::decide(&mut r, false, "", "willow", t0 + 1).unwrap();
    assert_eq!(
        approval::take(&mut r, &digest, "svc", t0 + 2),
        Err(Refusal::Denied("ap1".into()))
    );

    // Expired: 10 minutes pending, or 10 minutes approved and unused; expiry is noted once.
    let mut r = fresh();
    let late = t0 + approval::TOOL_CALL_TTL_MS;
    assert!(approval::expire(&mut r, late));
    assert!(!approval::expire(&mut r, late + 1));
    assert_eq!(
        approval::decide(&mut r, true, &digest, "willow", late),
        Err(Refusal::Expired("ap1".into()))
    );
    let mut r = fresh();
    approval::decide(&mut r, true, &digest, "willow", t0 + 1).unwrap();
    assert_eq!(
        approval::take(&mut r, &digest, "svc", t0 + 1 + approval::USE_TTL_MS),
        Err(Refusal::Expired("ap1".into()))
    );
    assert_eq!(r.state, State::Expired);

    // A draft approved after its record moved on is not applied.
    let mut d = approval::new(
        "d1".into(),
        Kind::InstanceDraft,
        "noisypigeon".into(),
        approval::payload_digest(&Kind::InstanceDraft, "noisypigeon", &json!({ "v": 1 })),
        json!({}),
        json!({ "v": 1 }),
        "svc".into(),
        None,
        t0,
    );
    let dd = d.digest.clone();
    approval::decide(&mut d, true, &dd, "anyone", t0 + 1).unwrap();
    assert!(approval::stale_draft(&mut d, 2, 1));
    assert_eq!(d.state, State::Expired);

    // A kind this build does not know survives a read and write.
    let k: Kind = serde_json::from_value(json!("later_kind")).unwrap();
    assert_eq!(serde_json::to_value(&k).unwrap(), json!("later_kind"));

    // The console's write is bound to the digest; a deny is its own write.
    let approve = action("approval-approve").unwrap();
    assert_eq!(approve.policy, Policy::Approval);
    assert_ne!(action("approval-deny").unwrap().policy, Policy::Approval);
    assert!(approve.target(&json!("not an id!")).is_err());
}

#[test]
fn platform_approvals_list_and_bearer_writes() {
    use pigeon_capabilities::approvals::listing;
    use pigeon_capabilities::console::write::action;
    use pigeon_core::approval::{self, Kind, State};

    for (name, yes) in [
        ("approval-approve", true),
        ("approval-deny", true),
        ("land-approve", true),
        ("land-approval", false),
        ("invite", false),
        ("remove-passkey", false),
        ("disconnect-app", false),
    ] {
        if let Some(w) = action(name) {
            assert_eq!(w.by_bearer, yes, "{name}");
        }
    }
    assert!(action("approval-approve").unwrap().by_bearer);
    assert!(action("land-approve").unwrap().by_bearer);
    assert!(!action("passkey-remove").unwrap().by_bearer);
    assert!(!action("app-disconnect").unwrap().by_bearer);

    let make = |id: &str, kind: Kind, subject: &str| {
        approval::new(
            id.into(),
            kind,
            subject.into(),
            "0123456789abcdef0123".into(),
            json!({ "args": { "a": 1 }, "caller": "svc", "acting_for": "willow" }),
            Value::Null,
            "svc".into(),
            None,
            1_000,
        )
    };
    let mut used = make("u1", Kind::ToolCall, "old_tool");
    used.state = State::Used;
    let open = vec![
        make("c1", Kind::ToolCall, "deploy_token"),
        make("d1", Kind::InstanceDraft, "noisypigeon"),
        make("p1", Kind::LinkedPlan, "noisypigeon"),
        used,
    ];
    let out = listing(&open);
    assert_eq!(out["calls"].as_array().unwrap().len(), 1);
    assert_eq!(out["calls"][0]["id"], "c1");
    assert_eq!(out["calls"][0]["tool"], "deploy_token");
    assert_eq!(out["calls"][0]["digest"], "0123456789ab");
    assert_eq!(out["calls"][0]["acting_for"], "willow");
    assert_eq!(out["drafts"][0]["instance"], "noisypigeon");
    assert_eq!(out["plans"][0]["id"], "p1");
    assert_eq!(out["drafts"].as_array().unwrap().len(), 1);
    assert_eq!(out["plans"].as_array().unwrap().len(), 1);
}

#[test]
fn platform_approvals_list_shows_run() {
    use pigeon_capabilities::approvals::listing;
    use pigeon_core::approval::{self, Kind};

    let make = |id: &str, summary: Value| {
        approval::new(
            id.into(),
            Kind::ToolCall,
            "deploy_token".into(),
            "0123456789abcdef0123".into(),
            summary,
            Value::Null,
            "svc".into(),
            None,
            1_000,
        )
    };
    let out = listing(&[
        make("r1", json!({ "caller": "svc", "run": "MXgiXQgySZx77QbT" })),
        make("r2", json!({ "caller": "svc" })),
    ]);
    assert_eq!(out["calls"][0]["run"], "MXgiXQgySZx77QbT");
    assert!(out["calls"][1]["run"].is_null());
}

/// Area: source / break-glass. `branch_patch`'s pure part (`pigeon_core::patch`): a unified diff
/// of modified, added, deleted, mode-changed and no-trailing-newline files that `git apply`
/// takes byte for byte, the files it leaves out and why, the sorted whole patch and its bound.
#[test]
fn platform_branch_patch() {
    use pigeon_core::patch::{self, File, PATCH_MAX, Side, assemble, branch_ref, file_diff};

    let id = |dir: &Path, text: &str| {
        String::from_utf8(git(dir, &["hash-object", "--stdin"], Some(text)))
            .unwrap()
            .trim()
            .to_string()
    };
    let lines = |n: usize, tweak: &dyn Fn(usize) -> Option<String>| {
        (1..=n)
            .filter_map(|i| {
                tweak(i)
                    .or(Some(format!("line {i}")))
                    .filter(|l| !l.is_empty())
            })
            .map(|l| format!("{l}\n"))
            .collect::<String>()
    };

    // The old and the new tree.
    let a_old = lines(30, &|_| None);
    let a_new = lines(30, &|i| match i {
        2 => Some("line two, changed".into()),
        15 => Some(String::new()),
        25 => Some("line 25\nline 25b".into()),
        _ => None,
    });
    let b_old = "gone\nfor good\n".to_string();
    let c_old = "#!/bin/sh\necho hi\n".to_string();
    let n_old = "one\ntwo".to_string();
    let n_new = "one\ntwo\nthree".to_string();
    let d_new = "brand\nnew\n".to_string();
    let dir = repo("branch-patch");
    for (name, body) in [
        ("a.txt", &a_old),
        ("b.txt", &b_old),
        ("c.sh", &c_old),
        ("n.txt", &n_old),
    ] {
        std::fs::write(dir.join(name), body).unwrap();
    }
    git(&dir, &["add", "-A"], None);
    git(&dir, &["commit", "-q", "-m", "old"], None);

    let (ia_old, ia_new) = (id(&dir, &a_old), id(&dir, &a_new));
    let ib = id(&dir, &b_old);
    let ic = id(&dir, &c_old);
    let (in_old, in_new) = (id(&dir, &n_old), id(&dir, &n_new));
    let id_d = id(&dir, &d_new);
    let files = [
        File {
            path: "a.txt",
            old: Some(Side {
                mode: "100644",
                id: &ia_old,
                data: a_old.as_bytes(),
            }),
            new: Some(Side {
                mode: "100644",
                id: &ia_new,
                data: a_new.as_bytes(),
            }),
        },
        File {
            path: "b.txt",
            old: Some(Side {
                mode: "100644",
                id: &ib,
                data: b_old.as_bytes(),
            }),
            new: None,
        },
        File {
            path: "c.sh",
            old: Some(Side {
                mode: "100644",
                id: &ic,
                data: c_old.as_bytes(),
            }),
            new: Some(Side {
                mode: "100755",
                id: &ic,
                data: c_old.as_bytes(),
            }),
        },
        File {
            path: "d dir/d.txt",
            old: None,
            new: Some(Side {
                mode: "100644",
                id: &id_d,
                data: d_new.as_bytes(),
            }),
        },
        File {
            path: "n.txt",
            old: Some(Side {
                mode: "100644",
                id: &in_old,
                data: n_old.as_bytes(),
            }),
            new: Some(Side {
                mode: "100644",
                id: &in_new,
                data: n_new.as_bytes(),
            }),
        },
    ];
    let one = |i: usize| file_diff(&files[i]).unwrap();
    let modified = one(0);
    assert!(modified.starts_with("diff --git a/a.txt b/a.txt\nindex "));
    assert!(modified.contains(&format!("index {ia_old}..{ia_new} 100644\n")));
    assert!(modified.contains("--- a/a.txt\n+++ b/a.txt\n@@ -"));
    assert_eq!(modified.matches("\n@@ -").count(), 3, "{modified}");
    let deleted = one(1);
    assert!(deleted.contains("deleted file mode 100644\n"));
    assert!(deleted.contains("--- a/b.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-gone\n-for good\n"));
    let moded = one(2);
    assert!(moded.contains("old mode 100644\nnew mode 100755\n"));
    assert!(!moded.contains("---"), "{moded}");
    let added = one(3);
    assert!(added.contains("new file mode 100644\n"));
    assert!(added.contains("--- /dev/null\n+++ b/d dir/d.txt\t\n@@ -0,0 +1,2 @@\n+brand\n+new\n"));
    let no_newline = one(4);
    assert!(no_newline.contains("-two\n\\ No newline at end of file\n+two\n+three\n\\ No newline"));

    // Round trip: `git apply` on the old tree gives the new tree.
    let built = assemble(&files).unwrap();
    assert_eq!(built.files, 5);
    assert!(built.skipped.is_empty());
    let (at_a, at_b, at_n) = (
        built.patch.find("a/a.txt").unwrap(),
        built.patch.find("a/b.txt").unwrap(),
        built.patch.find("a/n.txt").unwrap(),
    );
    assert!(at_a < at_b && at_b < at_n, "paths are sorted");
    std::fs::write(dir.join("branch.patch"), &built.patch).unwrap();
    git(&dir, &["apply", "branch.patch"], None);
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), a_new);
    assert_eq!(std::fs::read_to_string(dir.join("n.txt")).unwrap(), n_new);
    assert_eq!(
        std::fs::read_to_string(dir.join("d dir/d.txt")).unwrap(),
        d_new
    );
    assert!(!dir.join("b.txt").exists());
    assert_eq!(std::fs::read_to_string(dir.join("c.sh")).unwrap(), c_old);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("c.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    // What is left out, and why.
    let skip = |path: &'static str,
                old: Option<(&'static str, &'static [u8])>,
                new: Option<(&'static str, &'static [u8])>| {
        let side = |s: (&'static str, &'static [u8])| Side {
            mode: s.0,
            id: "",
            data: s.1,
        };
        file_diff(&File {
            path,
            old: old.map(side),
            new: new.map(side),
        })
        .unwrap_err()
    };
    let text = ("100644", &b"x\n"[..]);
    assert!(skip("img.png", None, Some(("100644", b"a\0b"))).contains("binary"));
    assert!(skip("latin.txt", None, Some(("100644", b"\xff\xfe\n"))).contains("binary"));
    assert!(skip("sub", None, Some(("160000", b""))).contains("submodule"));
    assert!(skip("link", Some(text), Some(("120000", b"a"))).contains("symlink"));
    for path in [
        ".env",
        "app/.env.local",
        ".dev.vars",
        "w/.dev.vars.prod",
        "k.pem",
        "a/b.KEY",
        "c.p12",
        "c.pfx",
        "id_rsa",
        "id_rsa.pub",
        "id_ed25519",
    ] {
        assert!(skip(path, None, Some(text)).contains("secret"), "{path}");
    }
    assert!(
        file_diff(&File {
            path: "src/env.rs",
            old: None,
            new: Some(Side {
                mode: "100644",
                id: "",
                data: b"x\n"
            })
        })
        .is_ok()
    );
    assert!(
        skip(
            "notes.txt",
            None,
            Some(("100644", b"a\n-----BEGIN RSA PRIVATE KEY-----\nabc\n"))
        )
        .contains("private key")
    );
    let big = vec![b'a'; patch::FILE_MAX + 1];
    let big = &*Box::leak(big.into_boxed_slice());
    assert!(skip("big.txt", None, Some(("100644", big))).contains("over"));
    let mixed = assemble(&[
        File {
            path: "z.txt",
            old: None,
            new: Some(Side {
                mode: "100644",
                id: "",
                data: b"z\n",
            }),
        },
        File {
            path: "a.pem",
            old: None,
            new: Some(Side {
                mode: "100644",
                id: "",
                data: b"x\n",
            }),
        },
        File {
            path: "m.bin",
            old: None,
            new: Some(Side {
                mode: "100644",
                id: "",
                data: b"\0",
            }),
        },
    ])
    .unwrap();
    assert_eq!(mixed.files, 1);
    let paths: Vec<&str> = mixed.skipped.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["a.pem", "m.bin"]);

    // The whole patch has a bound, and is never cut short.
    let chunk: String = (0..25_000).map(|i| format!("l{i:06}\n")).collect();
    assert!(chunk.len() < patch::FILE_MAX);
    let names: Vec<String> = (0..5).map(|i| format!("big{i}.txt")).collect();
    let many: Vec<File> = names
        .iter()
        .map(|n| File {
            path: n,
            old: None,
            new: Some(Side {
                mode: "100644",
                id: "",
                data: chunk.as_bytes(),
            }),
        })
        .collect();
    let why = assemble(&many).unwrap_err();
    assert!(
        why.contains(&PATCH_MAX.to_string()) && why.contains("bytes"),
        "{why}"
    );
    assert!(assemble(&many[..3]).is_ok());

    // Branch names.
    assert_eq!(branch_ref("claude/x").unwrap(), "refs/heads/claude/x");
    assert_eq!(
        branch_ref("refs/heads/claude/x").unwrap(),
        "refs/heads/claude/x"
    );
    for bad in ["", "refs/heads/", "-x", "a..b", "a b", "a\tb", "a\nb"] {
        assert!(branch_ref(bad).is_err(), "{bad:?}");
    }
}

/// Area: approvals / deploys. The `deploy` preset of `deploy_token` is the deploy proxy's
/// permission table plus Account Settings Read, holds what a direct core deploy needed live, and
/// shows on the approval card.
#[test]
fn platform_deploy_token_preset() {
    use pigeon_capabilities::approvals::listing;
    use pigeon_capabilities::deploys::{PERMISSIONS, grants, preset};
    use pigeon_core::approval::{self, Kind};

    let deploy = preset("deploy").unwrap();
    let mut table: Vec<&str> = pigeon_proxies::cf_proxy::ALLOWED
        .iter()
        .flat_map(|a| a.permissions.iter().copied())
        .chain(["Account Settings Read"])
        .collect();
    table.sort_unstable();
    table.dedup();
    let mut sorted = deploy.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, table, "the preset is the proxy's table");
    let mut scoped: Vec<&str> = pigeon_capabilities::split::purpose_permissions("cf_deploy")
        .unwrap()
        .iter()
        .copied()
        .chain(["Account Settings Read"])
        .collect();
    scoped.sort_unstable();
    assert_eq!(
        sorted, scoped,
        "the preset is what the proxy Worker's cf_deploy holds"
    );
    // What a direct core deploy held when it worked (2026-11-23).
    for p in PERMISSIONS
        .iter()
        .chain(&["Workers Containers Write", "Queues Read"])
    {
        assert!(deploy.contains(p), "{p}");
    }
    assert!(preset("core-deploy").is_none());

    let (p, ttl) = grants(&json!({})).unwrap();
    assert_eq!((p, ttl), (PERMISSIONS.map(String::from).to_vec(), 1_800));
    let (p, ttl) = grants(&json!({ "preset": "deploy", "ttl": 9_999 })).unwrap();
    assert_eq!(p.len(), deploy.len());
    assert_eq!(ttl, 3_600);
    let (p, _) = grants(&json!({ "permissions": ["Workers Scripts Read"] })).unwrap();
    assert_eq!(p, vec!["Workers Scripts Read".to_string()]);
    let both = grants(&json!({ "preset": "deploy", "permissions": ["Workers Scripts Read"] }));
    assert!(both.unwrap_err().contains("not both"));
    let unknown = grants(&json!({ "preset": "everything" })).unwrap_err();
    assert!(unknown.contains("no preset `everything`") && unknown.contains("deploy"));

    let card = approval::new(
        "g1".into(),
        Kind::ToolCall,
        "deploy_token".into(),
        "0123456789abcdef0123".into(),
        json!({
            "args": { "preset": "deploy", "ttl": 900 },
            "caller": "svc",
            "grants": { "permissions": deploy, "ttl_secs": 900 },
        }),
        Value::Null,
        "svc".into(),
        None,
        1_000,
    );
    let out = listing(&[card]);
    assert_eq!(out["calls"][0]["grants"]["ttl_secs"], 900);
    assert_eq!(
        out["calls"][0]["grants"]["permissions"]
            .as_array()
            .unwrap()
            .len(),
        deploy.len()
    );
    assert_eq!(out["calls"][0]["args"]["preset"], "deploy");
}

#[test]
fn platform_approvals_list_shows_other_kinds() {
    use pigeon_capabilities::approvals::listing;
    use pigeon_core::approval::{self, Kind, State};

    let make = |id: &str, kind: Kind| {
        approval::new(
            id.into(),
            kind,
            "monitors".into(),
            "0123456789abcdef0123".into(),
            json!({ "plan": ["create policy"] }),
            Value::Null,
            "svc".into(),
            None,
            1_000,
        )
    };
    let mut used = make("o2", Kind::Other("observability_monitors".into()));
    used.state = State::Used;
    let out = listing(&[
        make("o1", Kind::Other("observability_monitors".into())),
        used,
    ]);
    assert_eq!(out["other"].as_array().unwrap().len(), 1);
    assert_eq!(out["other"][0]["id"], "o1");
    assert_eq!(out["other"][0]["kind"], "observability_monitors");
    assert_eq!(out["other"][0]["digest"], "0123456789ab");
    assert!(out["calls"].as_array().unwrap().is_empty());
    assert!(out["plans"].as_array().unwrap().is_empty());
}

#[test]
fn platform_approvals_list_plain_sentences() {
    use pigeon_capabilities::approvals::{listing, plain_call, with_plain};
    use pigeon_core::approval::{self, Kind};

    let make = |kind: Kind, subject: &str, summary: Value| {
        approval::new(
            "i1".into(),
            kind,
            subject.into(),
            "0123456789abcdef0123".into(),
            summary,
            Value::Null,
            "svc".into(),
            None,
            1_000,
        )
    };
    let who = json!("Sample Person");
    assert_eq!(
        plain_call("deploy_token", &json!({}), &who),
        "Give a deploy token to Sample Person"
    );
    assert_eq!(
        plain_call("passkeys_remove", &json!({ "id": "pk1" }), &Value::Null),
        "Remove passkey pk1"
    );
    assert_eq!(
        plain_call("odd_tool", &json!({ "a": 1, "b": 2 }), &Value::Null),
        "Run odd_tool with 2 argument(s)"
    );
    let out = listing(&[
        make(
            Kind::ToolCall,
            "users_remove",
            json!({ "args": { "id": "u9" } }),
        ),
        make(Kind::InstanceDraft, "example", json!({})),
        make(Kind::LinkedPlan, "example", json!({})),
        make(Kind::Other("monitors_plan".into()), "", json!({})),
    ]);
    assert_eq!(out["calls"][0]["plain"], "Remove user u9");
    assert_eq!(out["calls"][0]["created_ms"], 1_000);
    assert_eq!(
        out["drafts"][0]["plain"],
        "Save the changes to instance example"
    );
    assert_eq!(out["plans"][0]["plain"], "Apply the plan for example");
    assert_eq!(out["other"][0]["plain"], "Approve monitors_plan");
    let l = with_plain(vec![
        json!({ "branch": "claude/x", "commit": "0123456789abcdef", "created_at": 5 }),
    ]);
    assert_eq!(l[0]["plain"], "Land claude/x at 0123456");
    assert_eq!(l[0]["created_ms"], 5);
}

/// Core's audit lines can name an AI agent run: `run` is optional on the caller (old callers read
/// as before), only a run id shape is kept, and a line has a `run` key only when one is given.
#[test]
fn core_audit_lines_name_the_run() {
    use pigeon_capabilities::policy::{audit_line, audit_line_for, decide, run_label};
    use pigeon_capabilities::users::Caller;
    let user = json!({ "id": "u", "kind": "service", "name": "Exec", "created_at": 1 });
    let old: Caller = serde_json::from_value(json!({ "user": user, "acting_for": null })).unwrap();
    assert!(old.run.is_none());
    assert!(serde_json::to_value(&old).unwrap().get("run").is_none());
    let named: Caller =
        serde_json::from_value(json!({ "user": user, "acting_for": null, "run": "r1" })).unwrap();
    assert_eq!(named.run.as_deref(), Some("r1"));

    let long = "a".repeat(33);
    assert_eq!(run_label(Some("r1")), Some("r1"));
    assert_eq!(run_label(Some("../x")), None);
    assert_eq!(run_label(Some("")), None);
    assert_eq!(run_label(Some(&long)), None);
    assert_eq!(run_label(Some(&long[..32])), Some(&long[..32]));
    assert_eq!(run_label(None), None);

    let req = request("land", "claude/x", None);
    let answer = decide(&vars(), &req);
    let plain: Value = serde_json::from_str(&audit_line("b", &req, &answer)).unwrap();
    assert!(plain.get("run").is_none());
    let with: Value =
        serde_json::from_str(&audit_line_for("b", &req, &answer, Some("r1"))).unwrap();
    assert_eq!(with["run"], "r1");
    let mut without = with.clone();
    without.as_object_mut().unwrap().remove("run");
    assert_eq!(without, plain);
    let none: Value = serde_json::from_str(&audit_line_for("b", &req, &answer, None)).unwrap();
    assert_eq!(none, plain);

    let starter = json!({ "user": user, "acting_for": null });
    let signed = pigeon_ai_agents::object::chosen_caller(&starter, None, "r1");
    let c: Caller = serde_json::from_value(signed).unwrap();
    assert_eq!(c.run.as_deref(), Some("r1"));
}

/// `users_people` lists standard users with their passkeys and what acts for them.
#[test]
fn platform_users_people() {
    use pigeon_capabilities::users::{User, initials, people};

    assert_eq!(initials("Sample Person"), "SP");
    assert_eq!(initials("sample"), "S");
    assert_eq!(initials("ann b cole"), "AC");
    let user = |id: &str, kind: &str, name: &str, at: i64| User {
        id: id.into(),
        kind: kind.into(),
        name: name.into(),
        created_at: at,
    };
    let users = [
        user("p2", "standard", "Second Person", 20),
        user("p1", "standard", "Sample Person", 10),
        user("s1", "service", "Sample App", 30),
        user("s2", "service", "Sample Agent", 40),
        user("s3", "service", "Loose Agent", 50),
    ];
    let links = [
        json!({ "service_user_id": "s1", "standard_user_id": "p1" }),
        json!({ "service_user_id": "s2", "standard_user_id": "p1" }),
    ];
    let keys = [json!({ "id": "k1", "user_id": "p1", "name": "Laptop",
        "created_at": 11, "last_used_at": null })];
    let out = people(&users, &links, &keys, &["s1".to_string()]);
    assert_eq!(out["unlinked"], 1);
    let list = out["people"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], "p1");
    assert_eq!(list[0]["initials"], "SP");
    assert!(list[0]["picture"].is_null());
    assert_eq!(list[0]["role"], "Person");
    assert_eq!(list[0]["added_ms"], 10);
    assert_eq!(list[0]["passkeys"][0]["name"], "Laptop");
    assert_eq!(list[0]["acting_for_me"][0]["kind"], "app");
    assert_eq!(list[0]["acting_for_me"][1]["kind"], "service");
    assert_eq!(list[1]["id"], "p2");
    assert!(list[1]["acting_for_me"].as_array().unwrap().is_empty());
}

/// Approval push is on, and its alert carries the link and collapse id.
#[test]
fn platform_approval_push_on() {
    use pigeon_capabilities::apple::push::{NOTIFY_APPROVALS, alert_parts};

    const { assert!(NOTIFY_APPROVALS) };
    let (payload, headers) = alert_parts(
        "Approval waiting",
        "`tool` from svc waits for your passkey.",
        "pigeon://approvals",
        "approval:abc",
    )
    .unwrap();
    let v: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(v["link"], "pigeon://approvals");
    assert!(headers.contains(&("apns-collapse-id", "approval:abc".to_string())));
}

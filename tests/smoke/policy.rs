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
    use pigeon_core::git::{self, Command as Update, ZERO};
    use pigeon_proxies::git_proxy::{
        Call, Rule, Service, check_all, moves_forward, refname_ok, route,
    };
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
    use pigeon_capabilities::proxy::run_branch_prefix;
    use pigeon_core::git::{self, Command as Update, ZERO};
    use pigeon_core::proxy_caller::within_prefix;
    use pigeon_proxies::git_proxy::{Rule, check_all};
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
const GATED: [&str; 8] = [
    "source_token",
    "setup_platform",
    "users_remove",
    "users_admin",
    "instance_member_set",
    "passkeys_remove",
    "oauth_revoke",
    "data_email_forward_set",
];

#[test]
fn tool_call_approvals() {
    use pigeon_capabilities::approvals;
    use pigeon_capabilities::console::write::{Policy, action};
    use pigeon_core::approval::{self, Kind, Refusal, State};
    use pigeon_core::policy::{self, Approval, Decision};

    // The person's rules: each of the five needs the passkey; every other tool runs as before.
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
    for tool in ["land", "policy_check", "source_revoke", "call"] {
        let a = p.evaluate(&vars(), &request("call_tool", tool, None));
        assert_eq!(
            (a.decision, a.rule.as_deref()),
            (Decision::Allow, Some("tools-run")),
            "{tool}"
        );
    }

    // The policy built into the Worker has the person's rules: every call is asked about, the
    // five are gated, and only their schemas gain the approval argument.
    assert!(approvals::asked());
    for tool in GATED {
        assert!(approvals::gated(tool), "{tool}");
    }
    assert!(!approvals::gated("land") && !approvals::gated("call"));
    let mut tools = vec![
        pigeon_core::mcp::Tool::bare("source_token", "mint"),
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
    let args = json!({ "ttl": 300, "access": "write" });
    let digest = approval::call_digest("source_token", &args, "svc");
    assert_eq!(
        digest,
        approval::call_digest(
            "source_token",
            &json!({ "access": "write", "ttl": 300 }),
            "svc"
        )
    );
    let other_args = approval::call_digest("source_token", &json!({ "ttl": 301 }), "svc");
    assert_ne!(digest, other_args);
    assert_ne!(digest, approval::call_digest("source_token", &args, "svc2"));
    assert_ne!(
        digest,
        approval::call_digest("setup_platform", &args, "svc")
    );

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
            "source_token".into(),
            digest.clone(),
            json!({ "tool": "source_token", "args": args, "caller": "svc" }),
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

    // Writes on an instance take its real id: the home instance's dotted `pigeon.dev` and an
    // `owner/name` one too, but nothing that could walk out of the id.
    for name in [
        "instance-put",
        "linked-unlink",
        "site-deploy",
        "site-rollback",
    ] {
        let a = action(name).unwrap();
        for ok in ["pigeon.dev", "noisypigeon", "owner/name", "a_b-c"] {
            assert_eq!(a.target(&json!(ok)).unwrap(), json!(ok), "{name} {ok}");
        }
        for no in [
            "",
            ".",
            "a/b/c",
            "../x",
            "a..b",
            "/a",
            "a/",
            "a b",
            "pigeon.dev.",
            &"x".repeat(65),
        ] {
            assert!(a.target(&json!(no)).is_err(), "{name} {no:?}");
        }
    }
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
        ("invite", true),
        ("passkey-remove", true),
        ("app-disconnect", true),
    ] {
        assert_eq!(action(name).unwrap().by_bearer, yes, "{name}");
    }

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
        make("c1", Kind::ToolCall, "source_token"),
        make("d1", Kind::InstanceDraft, "noisypigeon"),
        make("p1", Kind::LinkedPlan, "noisypigeon"),
        used,
    ];
    let out = listing(&open);
    assert_eq!(out["calls"].as_array().unwrap().len(), 1);
    assert_eq!(out["calls"][0]["id"], "c1");
    assert_eq!(out["calls"][0]["tool"], "source_token");
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
            "source_token".into(),
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
        plain_call("source_token", &json!({}), &who),
        "Give a source token to Sample Person"
    );
    assert_eq!(
        plain_call("passkeys_remove", &json!({ "id": "pk1" }), &Value::Null),
        "Remove passkey pk1"
    );
    assert_eq!(
        plain_call(
            "users_admin",
            &json!({ "id": "u2", "admin": true }),
            &Value::Null
        ),
        "Make user u2 an admin"
    );
    assert_eq!(
        plain_call(
            "users_admin",
            &json!({ "id": "u2", "admin": false }),
            &Value::Null
        ),
        "Make user u2 a member"
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
    let grant = |id: &str, name: &str, person: Option<&str>| {
        pigeon_capabilities::delegates::Grant::from_row(&json!({
            "id": id, "name": name, "created_at": 30, "person": person,
            "client_id": (id == "s1").then_some("c1"),
            "redirect_uris": (id == "s1").then_some("[\"https://claude.ai/api/mcp/auth_callback\"]"),
        }))
    };
    let grants = [
        grant("s1", "Sample App", Some("p1")),
        grant("s2", "Sample Agent", Some("p1")),
        grant("s3", "Loose Agent", None),
    ];
    let out = people(
        &users,
        &links,
        &keys,
        &["s1".to_string()],
        &["p1".to_string()],
        &grants,
    );
    // The same service users rolled up into apps: s1 is Claude by its redirect.
    let apps = out["people"][0]["apps"].as_array().unwrap();
    assert_eq!(apps.len(), 2);
    assert_eq!(apps[0]["label"], "Claude");
    assert_eq!(apps[1]["label"], "Sample Agent");
    assert_eq!(out["unassigned_apps"][0]["label"], "Loose Agent");
    assert_eq!(out["unlinked"], 1);
    let list = out["people"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["id"], "p1");
    assert_eq!(list[0]["initials"], "SP");
    assert!(list[0]["picture"].is_null());
    assert_eq!(list[0]["role"], "Admin");
    assert_eq!(list[0]["admin"], true);
    assert_eq!(list[0]["added_ms"], 10);
    assert_eq!(list[0]["passkeys"][0]["name"], "Laptop");
    assert_eq!(list[0]["acting_for_me"][0]["kind"], "app");
    assert_eq!(list[0]["acting_for_me"][1]["kind"], "service");
    assert_eq!(list[1]["id"], "p2");
    assert_eq!(list[1]["role"], "Person");
    assert_eq!(list[1]["admin"], false);
    assert!(list[1]["acting_for_me"].as_array().unwrap().is_empty());
}

/// Approval push is on, and its alert carries the link, collapse id, approval id and the
/// Review category.
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
    assert_eq!(v["approval"], "abc");
    assert_eq!(v["aps"]["category"], "APPROVAL");

    // Any other alert has neither.
    let (other, _) = alert_parts("Title", "Body", "pigeon://land", "land:x").unwrap();
    let o: Value = serde_json::from_str(&other).unwrap();
    assert!(o.get("approval").is_none());
    assert!(o["aps"].get("category").is_none());
}

/// The policy with the change the person is asked to make for Codex (docs/plans/agent-git.md):
/// `codex/*` beside `@session/*` in `land-and-deploy` and `session-branches`. Once the person
/// has made it, the policy is used as it is.
pub(crate) fn policy_with_codex() -> pigeon_core::policy::Policy {
    let text = pigeon_capabilities::policy::TEXT;
    let text = if text.contains("\"codex/*\"") {
        text.to_string()
    } else {
        let with = text.replace(
            "targets = [\"@session/*\"]",
            "targets = [\"@session/*\", \"codex/*\"]",
        );
        assert_eq!(
            with.matches("\"codex/*\"").count(),
            2,
            "the diff adds codex/* to land-and-deploy and session-branches"
        );
        with
    };
    pigeon_core::policy::parse(&text).unwrap()
}

/// A caller: service user `id` acting for Willow (`w`), or a person on their own.
fn agent_caller(id: &str, kind: &str) -> pigeon_capabilities::users::Caller {
    let user =
        |id: &str, kind: &str| json!({ "id": id, "kind": kind, "name": id, "created_at": 0 });
    let acting = if kind == "service" {
        json!({ "id": "w", "kind": "standard", "name": "Willow", "created_at": 0 })
    } else {
        Value::Null
    };
    serde_json::from_value(json!({ "user": user(id, kind), "acting_for": acting })).unwrap()
}

/// A grant as the Directory reads it: an OAuth client named `name` redirecting to `redirect`,
/// linked to Willow.
fn agent_grant(id: &str, name: &str, redirect: &str) -> pigeon_capabilities::delegates::Grant {
    pigeon_capabilities::delegates::Grant {
        id: id.into(),
        name: name.into(),
        client_id: Some(format!("c-{id}")),
        redirect_uris: vec![redirect.into()],
        person: Some("w".into()),
        ..Default::default()
    }
}

/// Runs git in `dir` with binary `input` on stdin; its stdout.
fn git_bytes(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    out.stdout
}

/// `git bundle create` of `range` in `dir`, base64-encoded as `branch_push` takes it.
fn bundle_of(dir: &Path, range: &str) -> String {
    let file = dir.join(format!("b-{}.bundle", range.replace(['.', '/', '^'], "_")));
    git(
        dir,
        &["bundle", "create", "-q", file.to_str().unwrap(), range],
        None,
    );
    pigeon_core::git::base64(&std::fs::read(&file).unwrap())
}

/// Commits `body` as file `f` with the message `message` (trailers and all); the commit's id.
fn commit_msg(dir: &Path, body: &str, message: &str) -> String {
    std::fs::write(dir.join("f"), body).unwrap();
    git(dir, &["add", "f"], None);
    git(dir, &["commit", "-q", "-F", "-"], Some(message));
    String::from_utf8(git(dir, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_string()
}

/// `branch_push` (docs/plans/agent-git.md): which app writes which namespace, the bundle read
/// and checked against the repository (git is a local repository, and Artifacts' receive-pack a
/// bare one taking the bundle's pack as it is), fast-forward unless forced, a bad base, input
/// over the limit, the policy's say, and commit messages kept to the byte.
#[test]
fn agent_git_branch_push() {
    use pigeon_capabilities::source::push::{
        self, Move, agent_of, audit_line, branch_in, check, namespace_of, read,
    };
    use pigeon_core::bundle;
    use pigeon_core::policy::Decision;
    const MAIN: &str = "refs/heads/main";

    // Which app: Claude (its redirect host), Codex (its name, loopback), the portal Claude Code
    // reaches pigeon through, and an app with no namespace.
    let claude = agent_grant("cl", "Claude", "https://claude.ai/api/mcp/auth_callback");
    let codex = agent_grant("cx", "Codex", "http://127.0.0.1:1455/auth/callback");
    let portal = agent_grant(
        "pt",
        "Cloudflare MCP Portal",
        "https://mcp.pigeon.dev/servers-callback",
    );
    let grok = agent_grant("gk", "Grok", "https://grok.com/cb");
    let ns = |g: &pigeon_capabilities::delegates::Grant| {
        agent_of(&agent_caller(&g.id, "service"), Some(g)).map(|a| a.namespace)
    };
    assert_eq!(ns(&claude), Ok("claude/"));
    assert_eq!(ns(&codex), Ok("codex/"));
    assert_eq!(ns(&portal), Ok("claude/"));
    let none = ns(&grok).unwrap_err();
    assert!(none.contains("no branch namespace"), "{none}");
    assert_eq!(
        namespace_of("ai-gateway"),
        None,
        "AI agent runs keep their own grants"
    );
    // An admin's override moves a grant to that app's namespace.
    let mut moved = grok.clone();
    moved.app_override = Some("openai".into());
    assert_eq!(ns(&moved), Ok("codex/"));
    // A person's own session, an app acting for no one, and a grant linked to someone else.
    assert!(agent_of(&agent_caller("w", "standard"), None).is_err());
    let alone: pigeon_capabilities::users::Caller = serde_json::from_value(json!({
        "user": { "id": "cx", "kind": "service", "name": "Codex", "created_at": 0 },
        "acting_for": null,
    }))
    .unwrap();
    assert!(agent_of(&alone, Some(&codex)).is_err());
    let mut elsewhere = codex.clone();
    elsewhere.person = Some("sam".into());
    assert!(agent_of(&agent_caller("cx", "service"), Some(&elsewhere)).is_err());

    // Which branches: Claude writes claude/x, never codex/x or main; Codex the other way.
    assert_eq!(
        branch_in("claude/", "claude/x", MAIN).as_deref(),
        Ok("refs/heads/claude/x")
    );
    assert_eq!(
        branch_in("codex/", "refs/heads/codex/x", MAIN).as_deref(),
        Ok("refs/heads/codex/x")
    );
    for (namespace, bad) in [
        ("claude/", "codex/x"),
        ("claude/", "main"),
        ("claude/", "refs/heads/main"),
        ("claude/", "claude/"),
        ("claude/", "claude/../main"),
        ("claude/", "claudex/y"),
        ("claude/", "refs/tags/claude/x"),
        ("codex/", "claude/x"),
        ("codex/", "main"),
    ] {
        assert!(
            branch_in(namespace, bad, MAIN).is_err(),
            "{namespace} {bad}"
        );
    }

    // The repository: main, and Artifacts as a bare repository holding it.
    let dir = repo("agent-git");
    let m1 = commit(&dir, "one");
    let m2 = commit(&dir, "one\ntwo");
    let remote = dir.with_extension("remote");
    let _ = std::fs::remove_dir_all(&remote);
    git(
        &dir,
        &["clone", "-q", "--bare", ".", remote.to_str().unwrap()],
        None,
    );
    // What Artifacts answers a fetch wanting `wants` and having `have`, as the tool asks.
    let history = |wants: Vec<String>, have: String| {
        let dir = dir.clone();
        async move {
            let input = format!("{}\n^{have}\n", wants.join("\n"));
            let got = git(
                &dir,
                &["pack-objects", "--stdout", "--revs", "-q"],
                Some(&input),
            );
            Ok(pigeon_core::pack::commit_parents(&got)?.parents)
        }
    };
    let block = crate::core::token_block_on;

    // An agent's work: two commits whose messages carry trailers, bundled against main.
    git(&dir, &["checkout", "-q", "-b", "work"], None);
    let message = "Add the thing\n\nWhy it is needed.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\nClaude-Session: https://claude.ai/code/session_x\n";
    let w1 = commit_msg(&dir, "one\ntwo\nthree", message);
    let w2 = commit_msg(
        &dir,
        "one\ntwo\nthree\nfour",
        "Second\n\nCo-Authored-By: Codex <codex@openai.com>\n",
    );
    let sent = bundle_of(&dir, "main..work");
    let data = bundle::decode(&sent).unwrap();
    let checked = read(&data).unwrap();
    assert_eq!(checked.tip, w2);
    assert_eq!(checked.bundle.prerequisites, vec![m2.clone()]);
    assert_eq!(
        checked.known.len(),
        2,
        "both commits, from the bundle's pack"
    );
    // A new branch is created at the tip.
    assert_eq!(
        block(check(&checked, &m2, None, false, history)),
        Ok(Move::Create)
    );
    // The pack goes to receive-pack as it is: every commit keeps its bytes, trailers included.
    git_bytes(
        &remote,
        &["index-pack", "--stdin", "--fix-thin"],
        checked.bundle.pack,
    );
    for c in [&w1, &w2] {
        assert_eq!(
            git(&remote, &["cat-file", "commit", c], None),
            git(&dir, &["cat-file", "commit", c], None),
            "commit {c} arrives byte for byte"
        );
    }
    let kept = String::from_utf8(git(&remote, &["log", "-1", "--format=%B", &w1], None)).unwrap();
    assert!(kept.contains("Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"));
    assert!(kept.contains("Claude-Session: https://claude.ai/code/session_x"));
    // Pushed again unchanged, it is already there.
    assert_eq!(
        block(check(&checked, &m2, Some(&w2), false, history)),
        Ok(Move::Already)
    );

    // Built on the branch's tip: a fast-forward.
    let w3 = commit(&dir, "one\ntwo\nthree\nfour\nfive");
    let more = bundle::decode(&bundle_of(&dir, &format!("{w2}..work"))).unwrap();
    let more = read(&more).unwrap();
    assert_eq!(more.tip, w3);
    assert_eq!(
        block(check(&more, &m2, Some(&w2), false, history)),
        Ok(Move::Forward)
    );
    // Built on main while the branch moved on (an amend): refused without force, a rewrite with.
    git(&dir, &["checkout", "-q", "-b", "amended", &m2], None);
    let a1 = commit(&dir, "one\ntwo\nredone");
    let amended = bundle::decode(&bundle_of(&dir, "main..amended")).unwrap();
    let amended = read(&amended).unwrap();
    assert_eq!(amended.tip, a1);
    let refused = block(check(&amended, &m2, Some(&w2), false, history)).unwrap_err();
    assert!(refused.contains("force"), "{refused}");
    assert_eq!(
        block(check(&amended, &m2, Some(&w2), true, history)),
        Ok(Move::Rewrite)
    );
    // A base older than main is still on it.
    git(&dir, &["checkout", "-q", "-b", "old-base", &m1], None);
    commit(&dir, "one\nolder");
    let old_base = bundle::decode(&bundle_of(&dir, &format!("{m1}..old-base"))).unwrap();
    let old_base = read(&old_base).unwrap();
    assert_eq!(
        block(check(&old_base, &m2, None, false, history)),
        Ok(Move::Create)
    );

    // A bad base: a commit on neither main nor the branch.
    git(&dir, &["checkout", "-q", "-b", "side", &m2], None);
    let s1 = commit(&dir, "side");
    commit(&dir, "side\nmore");
    let side = bundle::decode(&bundle_of(&dir, &format!("{s1}..side"))).unwrap();
    let side = read(&side).unwrap();
    let why = block(check(&side, &m2, None, false, history)).unwrap_err();
    assert!(why.contains("not on main or on the branch"), "{why}");
    assert!(block(check(&side, &m2, Some(&w2), false, history)).is_err());

    // Malformed input: no base, refs at two commits, a changed byte, not base64.
    let whole = bundle::decode(&bundle_of(&dir, "work")).unwrap();
    assert!(read(&whole).unwrap_err().contains("no base"));
    let mut two = data.clone();
    let at = two.windows(2).position(|w| w == b"\n\n").unwrap() + 1;
    two.splice(at..at, format!("{m1} refs/heads/other\n").into_bytes());
    assert!(read(&two).unwrap_err().contains("different commits"));
    let mut changed = data.clone();
    let at = changed.len() - 30;
    changed[at] ^= 1;
    assert!(read(&changed).unwrap_err().contains("checksum"));
    assert!(bundle::decode("not base64!").is_err());
    assert!(bundle::parse(b"# v3 git bundle\n@filter=blob:none\n").is_err());

    // Over the limit: refused before it is decoded, and decoded.
    let big = "A".repeat(bundle::MAX / 3 * 4 + 8);
    assert!(bundle::decode(&big).unwrap_err().contains("8 MiB"));
    assert!(
        bundle::parse(&vec![0u8; bundle::MAX + 1])
            .unwrap_err()
            .contains("8 MiB")
    );

    // The policy's say: Claude's pushes and rewrites under claude/ are allowed, and Codex's
    // under codex/ (the person added it to the live policy); main never.
    let vars = vars();
    let live = pigeon_core::policy_file::current().unwrap();
    let decide = |p: &pigeon_core::policy::Policy, m: Move, b: &str| {
        push::ask(p, &vars, "Claude for Willow", m, b).decision
    };
    assert_eq!(
        decide(live, Move::Create, "refs/heads/claude/x"),
        Decision::Allow
    );
    assert_eq!(
        decide(live, Move::Rewrite, "refs/heads/claude/x"),
        Decision::Allow
    );
    for p in [live, &policy_with_codex()] {
        for m in [Move::Create, Move::Forward, Move::Rewrite] {
            let codex = push::ask(p, &vars, "Codex for Willow", m, "refs/heads/codex/x");
            assert_eq!(codex.decision, Decision::Allow);
        }
        assert_eq!(decide(p, Move::Forward, MAIN), Decision::Deny, "main never");
    }

    // The audit line names the app, the person (by id), the branch and the tip; nothing else.
    let who = agent_of(&agent_caller("cx", "service"), Some(&codex)).unwrap();
    let line: Value = serde_json::from_str(&audit_line(
        &who,
        "refs/heads/codex/x",
        Some(&w2),
        &w3,
        "pushed",
    ))
    .unwrap();
    assert_eq!(line["audit"], "branch_push");
    assert_eq!(line["app"], "openai");
    assert_eq!(line["person"], "w");
    assert_eq!(line["branch"], "codex/x");
    assert_eq!(line["tip"], w3.as_str());
    assert!(
        !line.to_string().contains("Willow"),
        "names stay out of kept logs"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&remote);
}

/// `session_credential` (docs/plans/agent-git.md): a git-only proxy credential held by the
/// app's grant, on the Directory's real schema: its lifetime, who the proxies see behind it,
/// its namespace at the git proxy, its expiry, and its revocation by the app's Disconnect and
/// by removing the person.
#[test]
fn agent_git_session_credential() {
    use pigeon_capabilities::delegates::{DISCONNECT_GRANT, GRANTS_SQL};
    use pigeon_capabilities::proxy::{
        APP_GIT_ADD_SQL, APP_GIT_LINKED_SQL, APP_GIT_SWEEP_SQL, ProxyCaller, TOKEN_GET_SQL,
        app_git_ttl_ms,
    };
    use pigeon_capabilities::users::{MIGRATIONS, REMOVE_USER};
    use pigeon_core::git::Command as Update;
    use pigeon_core::proxy_caller::within_prefix;
    use pigeon_proxies::git_proxy::{check_all, outside_why};

    // Its lifetime: an hour unless asked, four at most, never cut short silently.
    assert_eq!(app_git_ttl_ms(&Value::Null), Ok(3_600_000));
    assert_eq!(app_git_ttl_ms(&json!(14_400)), Ok(14_400_000));
    for bad in [json!(14_401), json!(59), json!(-1), json!("1h")] {
        assert!(app_git_ttl_ms(&bad).is_err(), "{bad}");
    }

    let db = rusqlite::Connection::open_in_memory().unwrap();
    for m in MIGRATIONS {
        db.execute_batch(m).unwrap();
    }
    let run = |sql: &str| db.execute_batch(sql).unwrap();
    run("INSERT INTO users (id, kind, name, created_at, role) VALUES
           ('w', 'standard', 'Willow', 1, 'admin'), ('sam', 'standard', 'Sam', 2, NULL),
           ('cx', 'service', 'Codex', 3, NULL), ('cl', 'service', 'Claude', 4, NULL)");
    run(
        r#"INSERT INTO oauth_clients (id, name, redirect_uris, service_user_id, created_at) VALUES
           ('c1', 'Codex', '["http://127.0.0.1:1/cb"]', 'cx', 3),
           ('c2', 'Claude', '["https://claude.ai/api/mcp/auth_callback"]', 'cl', 4)"#,
    );
    run(
        "INSERT INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES
           ('cx', 'w', 10, 'k'), ('cl', 'w', 11, 'k')",
    );
    let linked = |grant: &str, person: &str| -> i64 {
        db.query_row(APP_GIT_LINKED_SQL, [grant, person], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(linked("cx", "w"), 1);
    assert_eq!(
        linked("cx", "sam"),
        0,
        "only the person the app is linked to"
    );
    assert_eq!(linked("w", "w"), 0);
    let add = |hash: &str, grant: &str, scope: &str, expires: i64| {
        db.execute(
            APP_GIT_ADD_SQL,
            rusqlite::params![hash, grant, "Codex session credential", expires, 20, scope],
        )
        .unwrap();
    };
    let get = |hash: &str, now: i64| -> Option<ProxyCaller> {
        let mut q = db.prepare(TOKEN_GET_SQL).unwrap();
        let mut rows = q
            .query_map(rusqlite::params![hash, now], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?, "label": r.get::<_, String>(1)?,
                    "person": r.get::<_, String>(2)?, "branch_prefix": r.get::<_, Option<String>>(3)?,
                }))
            })
            .unwrap();
        rows.next()
            .map(|r| serde_json::from_value(r.unwrap()).unwrap())
    };
    add("h_codex_one_x", "cx", "codex/", 1_000);
    let held = get("h_codex_one_x", 30).unwrap();
    assert_eq!(held.person, "Willow", "named for the person, not the app");
    assert_eq!(held.branch_prefix.as_deref(), Some("codex/"));
    assert_eq!(held.id, "h_codex_one_x"[..12]);
    // Shown under the app: its live tokens count it.
    let tokens: i64 = db
        .query_row(
            &format!(
                "SELECT tokens FROM ({}) WHERE id = 'cx'",
                GRANTS_SQL.trim_end_matches(" ORDER BY u.created_at, u.id")
            ),
            [30],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tokens, 1);

    // At the git proxy: git only, and pushes only under its namespace, never main.
    let prefix = held.branch_prefix.unwrap();
    assert!(within_prefix(["refs/heads/codex/x", "refs/heads/codex/a/b"], &prefix).is_ok());
    let tried = [
        "refs/heads/codex/x",
        "refs/heads/main",
        "refs/heads/claude/x",
        "refs/heads/codex/",
        "refs/heads/codex",
        "refs/heads/codexx/y",
        "refs/tags/codex/x",
        "refs/heads/codex/../main",
    ];
    assert_eq!(within_prefix(tried, &prefix).unwrap_err(), tried[1..]);
    assert!(outside_why(&prefix).contains("only branches under codex/"));
    assert!(outside_why("claude/ai-agent-ab").contains("run's git grant"));
    // Claude's credential and the session branch it pushes: inside, and allowed by the policy.
    let up = Update {
        old: pigeon_core::git::ZERO.into(),
        new: "1".repeat(40),
        name: "refs/heads/claude/x".into(),
    };
    assert!(within_prefix([up.name.as_str()], "claude/").is_ok());
    assert!(check_all(&[up], "refs/heads/main", "refs/heads/claude/").is_ok());
    // Never the Cloudflare API or MCP.
    assert!(pigeon_proxies::cf_proxy::GIT_ONLY.contains("/git/ only"));
    assert!(!pigeon_capabilities::proxy::reaches("proxy", "/mcp"));

    // It expires, and expired ones are swept.
    assert!(get("h_codex_one_x", 1_000).is_none());
    db.execute(APP_GIT_SWEEP_SQL, [1_000]).unwrap();
    let left: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM oauth_tokens WHERE kind = 'app_git'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);

    // Disconnecting the app revokes it; Claude's stays.
    add("h_codex_two_x", "cx", "codex/", 9_999);
    add("h_claude_one_x", "cl", "claude/", 9_999);
    for q in DISCONNECT_GRANT {
        db.execute(q, ["cx"]).unwrap();
    }
    assert!(get("h_codex_two_x", 30).is_none());
    assert!(get("h_claude_one_x", 30).is_some());
    // A link gone by any other way stops it at once too.
    add("h_claude_two_x", "cl", "claude/", 9_999);
    run("DELETE FROM links WHERE service_user_id = 'cl'");
    assert!(get("h_claude_two_x", 30).is_none());
    run(
        "INSERT INTO links (service_user_id, standard_user_id, approved_at, approved_with) VALUES ('cl', 'w', 12, 'k')",
    );
    assert!(get("h_claude_two_x", 30).is_some());
    // Removing the person revokes every app's.
    for q in REMOVE_USER {
        db.execute(
            q,
            rusqlite::params_from_iter(vec!["w"; q.matches('?').count()]),
        )
        .unwrap();
    }
    assert!(get("h_claude_one_x", 30).is_none());
    assert!(get("h_claude_two_x", 30).is_none());
}

/// What an approval stands for runs once the person approves it, once, as its caller, for
/// exactly what was approved (docs/plans/approvals-auto-run.md); the old "call again" flow
/// answers the recorded result; a chain record change → plan → apply continues by itself.
#[test]
fn approvals_auto_run() {
    use pigeon_capabilities::approvals::{self, FollowUp};
    use pigeon_capabilities::approver::acting_refusal;
    use pigeon_capabilities::users::{Caller, User};
    use pigeon_core::approval::{self, Kind, Refusal, RunStatus, State};
    use pigeon_core::policy::{Answer, Approval, Decision};

    let t0 = 5_000_000;
    let args = json!({ "instance": "docs", "local": "hello", "to": "me@example.com" });
    let digest = approval::call_digest("data_email_forward_set", &args, "svc");
    let call = |payload: Value| {
        approval::new(
            "ap1".into(),
            Kind::ToolCall,
            "data_email_forward_set".into(),
            digest.clone(),
            json!({ "tool": "data_email_forward_set", "args": args, "caller": "Claude", "acting_for": "Willow" }),
            payload,
            "svc".into(),
            Some("willow".into()),
            t0,
        )
    };
    let kept = json!({ "args": args, "scope": "docs" });

    // A gated call runs on approval with the arguments approved, through the endpoint it came
    // through, carrying its own id for the gate to take.
    let r = call(kept.clone());
    assert_eq!(
        approvals::follow_up(&r).unwrap(),
        Some(FollowUp {
            tool: "data_email_forward_set".into(),
            args: json!({ "instance": "docs", "local": "hello", "to": "me@example.com", "approval": "ap1" }),
            scope: Some("docs".into()),
        })
    );
    // Arguments that are not the ones signed for never run; nor a secret's tool, a console
    // write's own approval, or a record from before arguments were kept.
    let mut forged =
        call(json!({ "args": { "instance": "docs", "local": "hello", "to": "evil@example.com" } }));
    assert!(
        approvals::follow_up(&forged)
            .unwrap_err()
            .contains("not the ones approved")
    );
    forged.digest = approval::call_digest("data_email_forward_set", &forged.payload["args"], "svc");
    assert_ne!(forged.digest, digest, "another argument is another call");
    let mut secret = call(kept.clone());
    secret.subject = "source_token".into();
    assert_eq!(approvals::follow_up(&secret).unwrap(), None);
    let mut own = call(kept.clone());
    own.summary["via"] = json!("console");
    assert_eq!(approvals::follow_up(&own).unwrap(), None);
    assert_eq!(approvals::follow_up(&call(Value::Null)).unwrap(), None);

    // Pending, denied and expired approvals are never claimed, so never run.
    let mut pending = call(kept.clone());
    assert_eq!(
        approval::claim(&mut pending, "data_email_forward_set", t0 + 1),
        Err(Refusal::NotApproved("ap1".into()))
    );
    let mut denied = call(kept.clone());
    approval::decide(&mut denied, false, &digest, "willow", t0 + 1).unwrap();
    assert_eq!(
        approval::claim(&mut denied, "data_email_forward_set", t0 + 2),
        Err(Refusal::Denied("ap1".into()))
    );
    let mut late = call(kept.clone());
    approval::decide(&mut late, true, &digest, "willow", t0 + 1).unwrap();
    assert_eq!(
        approval::claim(
            &mut late,
            "data_email_forward_set",
            t0 + 2 + approval::USE_TTL_MS
        ),
        Err(Refusal::Expired("ap1".into()))
    );
    assert_eq!(late.state, State::Expired);
    assert!(late.outcome.is_none());

    // Approved: claimed once; a second claim (a retried alarm, a second runner) is refused.
    let mut r = call(kept.clone());
    approval::decide(&mut r, true, &digest, "willow", t0 + 1).unwrap();
    assert!(approval::runnable(&r, t0 + 2).is_ok());
    approval::claim(&mut r, "data_email_forward_set", t0 + 2).unwrap();
    assert_eq!(r.outcome.as_ref().unwrap().status, RunStatus::Running);
    assert_eq!(
        approval::claim(&mut r, "data_email_forward_set", t0 + 3),
        Err(Refusal::Claimed("ap1".into()))
    );
    // While it runs, the caller calling again is told so, and nothing runs; another caller,
    // or another tool naming the id, goes on to the tool's own refusal.
    let running = approvals::recall_answer(&r, "data_email_forward_set", "svc").unwrap();
    assert!(running.unwrap_err().contains("running now"));
    assert!(approvals::recall_answer(&r, "data_email_forward_set", "svc2").is_none());
    assert!(approvals::recall_answer(&r, "users_remove", "svc").is_none());
    // The run's own call takes the approval once, for exactly the digest approved.
    let other = approval::call_digest("data_email_forward_set", &json!({ "to": "x" }), "svc");
    assert_eq!(
        approval::take(&mut r, &other, "svc", t0 + 4),
        Err(Refusal::OtherCall("ap1".into()))
    );
    approval::take(&mut r, &digest, "svc", t0 + 4).unwrap();
    assert_eq!(
        approval::take(&mut r, &digest, "svc", t0 + 5),
        Err(Refusal::Used("ap1".into()))
    );
    // It ran and asked for a record change: done, with the next approval; the arguments are
    // cleared, the answer kept (redacted) for a caller that asks again.
    let answer =
        json!({ "approval": "ap2", "changes": [{ "path": "email.forward" }], "api_token": "t" });
    assert_eq!(approvals::next_of(&answer).as_deref(), Some("ap2"));
    let summary = approvals::summarize("data_email_forward_set", &answer);
    assert!(summary.contains("next approval"), "{summary}");
    assert!(approval::finish(
        &mut r,
        Ok(&answer),
        Some(summary),
        Some("ap2".into()),
        t0 + 6
    ));
    assert!(
        !approval::finish(&mut r, Err("again"), None, None, t0 + 7),
        "an outcome ends once"
    );
    let o = r.outcome.clone().unwrap();
    assert_eq!(
        (o.status, o.next.as_deref()),
        (RunStatus::Done, Some("ap2"))
    );
    assert_eq!(o.result["api_token"], json!(approval::REDACTED));
    assert!(r.payload.get("args").is_none());
    let again = approvals::recall_answer(&r, "data_email_forward_set", "svc")
        .unwrap()
        .unwrap();
    assert_eq!(again["approval"], json!("ap2"));
    let view = approvals::recent_item(&r, &[]).unwrap();
    assert_eq!(
        (view["status"].clone(), view["next_waits"].clone()),
        (json!("done"), json!(false))
    );
    assert!(
        view.get("result").is_none(),
        "the tab shows the summary, not the answer"
    );

    // The chain: the record change, once applied, has its instance planned...
    let mut draft = approval::new(
        "ap2".into(),
        Kind::InstanceDraft,
        "docs".into(),
        "d2".into(),
        json!({ "instance": "docs" }),
        json!({ "record": {}, "base_version": 3 }),
        "svc".into(),
        Some("willow".into()),
        t0,
    );
    assert_eq!(
        approval::runnable(&draft, t0),
        Err(Refusal::NotApproved("ap2".into()))
    );
    approval::decide(&mut draft, true, "d2", "willow", t0 + 1).unwrap();
    draft.state = State::Used; // as the Directory's apply leaves it
    assert_eq!(
        approvals::follow_up(&draft)
            .unwrap()
            .map(|f| (f.tool, f.args)),
        Some(("instance_plan".to_string(), json!({ "instance": "docs" })))
    );
    approval::claim(&mut draft, "instance_plan", t0 + 2).unwrap();
    // ...an apply that did not hold (the record moved on) plans nothing.
    let mut stale = draft.clone();
    stale.outcome = None;
    stale.note = Some("the record changed since this draft".into());
    assert!(approval::runnable(&stale, t0 + 2).is_err());
    // A plan with something to do asks for the plan's approval, which applies it by its tool.
    let planned = json!({ "instance": "docs", "plan": ["Create bucket", "Add route"], "applies": true, "approval": "ap3" });
    assert_eq!(approvals::next_of(&planned).as_deref(), Some("ap3"));
    let mut plan = approval::new(
        "ap3".into(),
        Kind::LinkedPlan,
        "docs".into(),
        "p3".into(),
        json!({ "instance": "docs" }),
        json!({ "steps": [] }),
        "svc".into(),
        Some("willow".into()),
        t0,
    );
    assert_eq!(
        approvals::follow_up(&plan)
            .unwrap()
            .map(|f| (f.tool, f.args)),
        Some(("instance_apply".to_string(), json!({ "plan": "ap3" })))
    );
    plan.payload = json!({ "retire": true });
    assert_eq!(
        approvals::follow_up(&plan).unwrap().unwrap().tool,
        "instance_retire_apply"
    );
    plan.payload = json!({ "rename": true });
    assert_eq!(
        approvals::follow_up(&plan).unwrap().unwrap().tool,
        "instance_rename_home"
    );
    for (kind, tool) in [
        ("apps_cleanup", "apps_cleanup_apply"),
        ("portal_oauth_redirects", "portal_oauth_redirects_apply"),
        ("observability_monitors", "observability_monitors_apply"),
        ("ingest_run", "ingest_start"),
        ("cloud_change", "cloud_apply"),
    ] {
        let mut other = plan.clone();
        other.kind = Kind::named(kind);
        assert_eq!(
            approvals::follow_up(&other).unwrap().unwrap().tool,
            tool,
            "{kind}"
        );
        assert!(approvals::PLAN_TOOLS.contains(&tool), "{tool}");
    }
    let mut unknown = plan.clone();
    unknown.kind = Kind::named("someday");
    assert_eq!(approvals::follow_up(&unknown).unwrap(), None);
    // The previous run's item says its next approval still waits, for the tab's Approve.
    let ran_draft = {
        let mut d = draft.clone();
        approval::finish(&mut d, Ok(&planned), None, Some("ap3".into()), t0 + 3);
        d
    };
    let mut waiting = plan.clone();
    waiting.payload = json!({ "steps": [] });
    assert_eq!(
        approvals::recent_item(&ran_draft, &[waiting]).unwrap()["next_waits"],
        json!(true)
    );

    // A plan with nothing to do, or something in the way, ends the chain, saying so.
    for note in [
        "Everything is already in place.",
        "Something is in the way; nothing was asked or changed.",
    ] {
        let ended = json!({ "instance": "docs", "plan": [], "applies": false, "note": note });
        assert_eq!(approvals::next_of(&ended), None);
        assert_eq!(approvals::summarize("instance_plan", &ended), note);
    }

    // An apply whose plan no longer matches its digest is refused by its take, and recorded as
    // failed; the caller calling again is told why, and nothing runs.
    approval::decide(&mut plan, true, "p3", "willow", t0 + 4).unwrap();
    approval::claim(&mut plan, "instance_apply", t0 + 5).unwrap();
    assert_eq!(
        approval::take(&mut plan, "p3-changed", "svc", t0 + 6),
        Err(Refusal::OtherCall("ap3".into()))
    );
    let why = "the account no longer gives the approved plan; nothing was changed";
    approval::finish(&mut plan, Err(why), None, None, t0 + 7);
    let told = approvals::recall_answer(&plan, "instance_apply", "svc")
        .unwrap()
        .unwrap_err();
    assert!(told.contains("failed") && told.contains(why), "{told}");
    // A failed retire is the one run calling again resumes.
    let mut retire = plan.clone();
    retire.outcome.as_mut().unwrap().tool = "instance_retire_apply".into();
    assert!(approvals::recall_answer(&retire, "instance_retire_apply", "svc").is_none());
    // A run that died is taken to have, after a while, and never runs again.
    let mut died = draft.clone();
    assert!(!approval::run_stale(&died, t0 + 3));
    assert!(approval::run_stale(&died, t0 + 2 + approval::RUN_STALE_MS));
    assert!(approval::finish(
        &mut died,
        Err("interrupted"),
        None,
        None,
        t0 + 9
    ));
    assert_eq!(
        approval::claim(&mut died, "instance_plan", t0 + 10),
        Err(Refusal::Claimed("ap2".into()))
    );

    // The policy is asked again at run time: a tool it now refuses outright does not run; one
    // it gates on the passkey runs, the approval being that passkey.
    let gated = Answer {
        decision: Decision::Deny,
        rule: Some("risky-tools-need-passkey".into()),
        reason: "r".into(),
        needs: Some(Approval::Passkey),
    };
    let refused = Answer {
        decision: Decision::Deny,
        rule: None,
        reason: "not in policy".into(),
        needs: None,
    };
    let allowed = Answer {
        decision: Decision::Allow,
        rule: Some("tools-run".into()),
        reason: "r".into(),
        needs: None,
    };
    assert!(approvals::runs_under(&gated, "data_email_forward_set").is_ok());
    assert!(approvals::runs_under(&allowed, "instance_apply").is_ok());
    assert!(
        approvals::runs_under(&refused, "data_email_forward_set")
            .unwrap_err()
            .contains("now refuses")
    );
    assert!(approvals::recheck("Claude", "data_email_forward_set").is_ok());
    assert!(approvals::recheck("Claude", "instance_apply").is_ok());

    // As the original caller only: still acting for the person who approved.
    let user = |id: &str, kind: &str| User {
        id: id.into(),
        kind: kind.into(),
        name: id.into(),
        created_at: 0,
    };
    let as_svc = |acting: Option<&str>| Caller {
        user: user("svc", "service"),
        acting_for: acting.map(|a| user(a, "standard")),
        run: None,
    };
    let r = call(kept);
    assert_eq!(acting_refusal(&r, &as_svc(Some("willow"))), None);
    assert!(acting_refusal(&r, &as_svc(Some("mallory"))).is_some());
    assert!(acting_refusal(&r, &as_svc(None)).is_some());
    let mut anyone = r.clone();
    anyone.approver_id = None;
    assert_eq!(acting_refusal(&anyone, &as_svc(None)), None);
    assert!(acting_refusal(&anyone, &as_svc(Some("willow"))).is_some());

    // An answer too long to keep is kept as a note.
    let long = json!({ "lines": vec!["x".repeat(100); 200] });
    assert_eq!(approval::kept_result(&long)["truncated"], json!(true));
}

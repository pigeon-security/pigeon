//! pigeon's smoke suite, its only tests: one end-to-end scenario per path, each walking that path
//! through public interfaces with its externals stubbed (git is a local repository; the Workers
//! runtime, Cloudflare, the providers and GitHub answer with recorded or made-up values). Each
//! scenario folds in the security cases of its path: forged signatures and tokens, malformed
//! packs, refused pushes. The policy (`.pigeon/policy.toml`, rule `smoke-suite-fixed`) keeps
//! every test here; `ci/runner/test.sh` fails on a `#[test]` anywhere else and on a suite
//! slower than 60 seconds.

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

// ---- policy ----

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
}

// ---- git proxy ----

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

// ---- land ----

#[test]
fn land_from_check_to_landed() {
    use pigeon_capabilities::land::state::{self, Seen, State};
    use pigeon_capabilities::land::{expected_commit, notes_commit, tip_refusal};
    // The branch, the commit named and the tip, as `land` checks them.
    assert_eq!(
        state::branch_ref("claude/policy").as_deref(),
        Ok("refs/heads/claude/policy")
    );
    for bad in [
        "main",
        "refs/heads/main",
        "claude/../main",
        "claude/",
        "other/x",
    ] {
        assert!(state::branch_ref(bad).is_err(), "{bad}");
    }
    let dir = repo("land");
    let main_at = commit(&dir, "one");
    let tip = commit(&dir, "one\ntwo");
    assert_eq!(expected_commit(Some(&tip)), Ok(Some(tip.clone())));
    assert!(expected_commit(Some("main")).is_err());
    assert!(expected_commit(Some(&tip[..12])).is_err());
    assert_eq!(tip_refusal("claude/policy", Some(&tip), Some(&tip)), None);
    assert!(tip_refusal("claude/policy", None, None).is_some());
    assert!(tip_refusal("claude/policy", Some(&main_at), Some(&tip)).is_some());

    // The gate's own files changed: the policy says who approves (stubbed tree ids).
    let on_main: Vec<Option<String>> = state::GUARDED
        .iter()
        .map(|p| Some(format!("{p}@1")))
        .collect();
    let mut at_commit = on_main.clone();
    at_commit[4] = Some("ci@2".into());
    let changed = state::guarded_changes(&on_main, &at_commit);
    assert_eq!(changed, ["ci"]);
    let decide = |r: &pigeon_core::policy::Request| pigeon_capabilities::policy::decide(&vars(), r);
    let gate = state::approvals(&changed, decide);
    assert!(gate.passkey.is_empty());
    assert_eq!(gate.delegate, ["ci/ (gate-changes)"]);
    assert_eq!(state::approval_reason(false, &gate.passkey), None);
    at_commit[0] = Some("policy@2".into());
    let changed = state::guarded_changes(&on_main, &at_commit);
    let policy = state::approvals(&changed, decide);
    assert_eq!(policy.passkey.len(), 1);
    assert!(state::approval_reason(false, &policy.passkey).is_some());

    // checking → testing → awaiting_approval → merging → deploying → landed.
    let passed = Seen::JobEnded { passed: true };
    let mut at = State::Checking;
    for (seen, to) in [
        (&Seen::Checked, State::Testing),
        (&passed, State::AwaitingApproval),
        (&Seen::Approved, State::Merging),
        (&Seen::Merged, State::Deploying),
        (&passed, State::Landed),
    ] {
        let next = state::next(at, true, seen).unwrap();
        assert_eq!(next, to);
        assert!(state::allowed(at, next));
        at = next;
    }
    assert!(at.terminal());
    // Without an approval to wait for, tests passing go straight to the merge.
    assert_eq!(
        state::next(State::Testing, false, &passed),
        Some(State::Merging)
    );
    // Each step fails its own way, and nothing moves a final state or skips a step.
    let failed = Seen::JobEnded { passed: false };
    assert_eq!(
        state::next(State::Checking, false, &Seen::CheckRefused),
        Some(State::Refused)
    );
    assert_eq!(
        state::next(State::Testing, false, &failed),
        Some(State::TestFailed)
    );
    assert_eq!(
        state::next(State::AwaitingApproval, true, &Seen::ApprovalLapsed),
        Some(State::Expired)
    );
    assert_eq!(
        state::next(State::Merging, false, &Seen::MergeRefused),
        Some(State::MergeRefused)
    );
    assert_eq!(
        state::next(State::Deploying, false, &failed),
        Some(State::DeployFailed)
    );
    assert_eq!(state::next(State::Landed, false, &Seen::Cancel), None);
    assert!(!state::allowed(State::Checking, State::Merging));
    assert!(State::Testing.cancellable() && !State::Merging.cancellable());

    // The merge: main moves only forward, judged from the pack (stubbed Artifacts).
    let pack_fwd = pack(&dir, &tip, &main_at);
    assert_eq!(
        pigeon_capabilities::git_proxy::moves_forward(&main_at, &tip, &pack_fwd),
        Ok(true)
    );
    assert_eq!(
        pigeon_capabilities::git_proxy::moves_forward(&tip, &main_at, &pack(&dir, &main_at, &tip)),
        Ok(false)
    );

    // The note the landing leaves on its commit: one line, in a notes commit git reads.
    let note = state::note(
        "L1",
        State::Landed,
        "Claude for Willow",
        Some("t1"),
        Some("d1"),
        Some("delegate"),
        "2026-11-21T10:00:00Z",
    );
    assert_eq!(note.lines().count(), 1);
    assert!(note.starts_with("landing L1 state=landed"));
    let (objects, id) = notes_commit(&[(tip.clone(), note.clone())], None, 1_795_000_000).unwrap();
    assert!(!objects.is_empty());
    assert_eq!(id.len(), 40);
    assert!(notes_commit(&[("main".into(), note)], None, 0).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- deploy proxy ----

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
        )
    };
    let query =
        |q: &[&str]| (!q.is_empty()).then(|| q.join("&").replace("{queue}", "pigeon-ci-events"));
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

// ---- CI ----

#[test]
fn ci_route_and_run_on_a_fake_connector() {
    use pigeon_capabilities::ci::Dispatched;
    use pigeon_capabilities::ci::class::{Class, Offered, Size};
    use pigeon_capabilities::ci::config::{self, Arch, Os};
    use pigeon_capabilities::ci::route::{
        Ask, Availability, Candidate, choose, fallback_due, route,
    };
    use pigeon_capabilities::ci::scheduler::{self, Outcome};
    use pigeon_capabilities::ci::state::{self, Next, Report, State};
    // The repository's own jobs parse, and `test` asks for a medium machine.
    let cfg = config::parse(include_str!("../.pigeon/ci.toml")).unwrap();
    let test = &cfg.jobs["test"];
    assert_eq!(test.run, "ci/runner/test.sh");
    assert!(cfg.jobs["deploy"].token && !test.token);
    for bad in [
        "[jobs.x]\nrun = \"ci/x.sh\"\nsurprise = 1\n",
        "[jobs.x]\nrun = \"../x.sh\"\n",
        "[jobs.X]\nrun = \"ci/x.sh\"\n",
        "[jobs.x]\nrun = \"ci/x.sh\"\ntimeout_min = 100000\n",
        "junk",
    ] {
        assert!(config::parse(bad).is_err(), "{bad}");
    }
    // Two fake connectors: `a` is full, `b` has room.
    let offer = |size| Offered {
        class: Class {
            os: Os::Linux,
            arch: Arch::Amd64,
            size,
            docker: false,
            gpu: false,
        },
        cpu: 8,
        memory_gb: 32,
        disk_gb: 100,
        max_timeout_min: 360,
        network_none: false,
    };
    let offers = [offer(Size::Medium), offer(Size::Large)];
    let small = [offer(Size::Small)];
    let up = |free: Option<u32>| Availability {
        configured: true,
        healthy: true,
        free,
        queued: 0,
    };
    let cands = [
        Candidate {
            name: "a",
            rank: 0,
            offers: &offers,
            availability: up(Some(0)),
        },
        Candidate {
            name: "b",
            rank: 1,
            offers: &offers,
            availability: up(None),
        },
        Candidate {
            name: "tiny",
            rank: 2,
            offers: &small,
            availability: up(None),
        },
    ];
    let ask = Ask {
        needs: &test.needs,
        timeout_min: test.timeout_min,
        pin: None,
        exclude: &[],
    };
    let routed = route(&ask, &cands).unwrap();
    assert!(routed.ruled_out.iter().any(|(n, _)| n == "tiny"));
    // The operator's first choice is full: the job waits for it, until the fallback is due.
    assert!(choose(&routed, false).is_none());
    let pick = choose(&routed, true).unwrap();
    assert_eq!(pick.connector, "b");
    assert_eq!(pick.class.size, Size::Medium);
    // A bad pin is refused; a job nothing can run is refused with every reason.
    assert!(
        route(
            &Ask {
                pin: Some("nowhere"),
                ..ask
            },
            &cands
        )
        .is_err()
    );
    let huge = config::Needs {
        cpu: 512,
        ..Default::default()
    };
    assert!(
        route(
            &Ask {
                needs: &huge,
                ..ask
            },
            &cands
        )
        .is_err()
    );
    assert!(!fallback_due(0, 1_000, 60_000) && fallback_due(0, 60_000, 60_000));

    // Dispatched to `b`, then run: queued → dispatched → running → succeeded.
    let out = scheduler::after_dispatch(
        1,
        "b",
        Ok(Dispatched {
            handle: "b-1".into(),
            url: None,
        }),
    );
    assert!(matches!(out, Outcome::Dispatched(_)));
    assert!(matches!(
        scheduler::after_dispatch(1, "b", Err("no room".into())),
        Outcome::Requeue { .. }
    ));
    assert!(matches!(
        scheduler::after_dispatch(2, "b", Err("no room".into())),
        Outcome::Fail { .. }
    ));
    let mut s = State::Dispatched;
    assert!(state::allowed(State::Queued, s));
    for (report, to) in [
        (Report::Started, State::Running),
        (Report::Finished { passed: true }, State::Succeeded),
    ] {
        match state::on_report(s, 1, &report) {
            Next::Move(next) => {
                assert!(state::allowed(s, next));
                s = next;
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s, to);
    }
    assert!(s.terminal());
    assert!(matches!(
        state::on_report(s, 1, &Report::Started),
        Next::Ignore
    ));
    // An infrastructure failure retries once; a failed run never does.
    assert!(matches!(
        state::on_report(State::Running, 1, &Report::Infra { why: "lost".into() }),
        Next::Retry
    ));
    assert!(matches!(
        state::on_report(
            State::Running,
            state::MAX_ATTEMPTS,
            &Report::Infra { why: "lost".into() }
        ),
        Next::Move(State::Error)
    ));
    assert!(matches!(
        state::on_report(State::Running, 1, &Report::Finished { passed: false }),
        Next::Move(State::Failed)
    ));
    assert_eq!(scheduler::deadline(0, 60), (60 + 10) * 60_000);
}

// ---- models ----

#[test]
fn models_request() {
    use pigeon_capabilities::models::jobs::{self, State};
    use pigeon_capabilities::models::stream::Reading;
    use pigeon_capabilities::models::{Dialect, Provider, ask};
    let provider = Provider {
        slug: "anthropic",
        alias: "default",
        dialect: Dialect::Anthropic,
        default_model: "claude-haiku-5-5",
        key_help: "",
    };
    // The request, in the common shape and then as the provider takes it.
    let a = ask(
        &json!({ "prompt": "hi", "system": "be brief", "max_tokens": 99_999 }),
        &provider,
    )
    .unwrap();
    assert_eq!(a.model, "claude-haiku-5-5");
    assert_eq!(a.max_tokens, 64_000);
    let body = Dialect::Anthropic.body(&a);
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": "hi" }])
    );
    assert_eq!(body["system"], "be brief");
    assert!(ask(&json!({}), &provider).is_err());
    assert_eq!(Dialect::Anthropic.path(), "v1/messages");

    // The stubbed upstream streams its answer, split anywhere.
    let stream = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-haiku-5-5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hidden\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    for chunk in [1, 7, 4096] {
        let mut r = Reading::default();
        for piece in stream.as_bytes().chunks(chunk) {
            r.feed(Dialect::Anthropic, piece);
        }
        r.end(Dialect::Anthropic);
        let reply = r.reply();
        assert_eq!(reply.text, "Hello", "chunks of {chunk}");
        assert_eq!((reply.tokens_in, reply.tokens_out), (Some(12), Some(7)));
        assert!(r.done && r.error.is_none());
    }
    // A mid-stream error is kept; junk is ignored.
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b": c\ndata: not json\n\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\r\n\r\n");
    assert_eq!(r.error.as_deref(), Some("overloaded_error: Overloaded"));
    // A whole (non-streamed) answer reads the same; a non-answer is an error.
    let whole = json!({ "model": "m", "content": [{ "type": "text", "text": "ok" }], "stop_reason": "end_turn", "usage": { "input_tokens": 1, "output_tokens": 1 } });
    assert_eq!(Dialect::Anthropic.reply(&whole).unwrap().text, "ok");
    assert!(Dialect::Anthropic.reply(&json!({ "error": {} })).is_err());

    // As an async job: queued, running, succeeded; kept, and read only by its sender.
    assert_eq!(jobs::timeout_ms(&Value::Null), Ok(10 * 60_000));
    assert!(jobs::timeout_ms(&json!(15)).is_err());
    assert!(jobs::may_submit(jobs::MAX_OPEN).is_err());
    assert_eq!(jobs::to_start(jobs::MAX_RUNNING, 3), 0);
    assert!(
        jobs::allowed(State::Queued, State::Running)
            && jobs::allowed(State::Running, State::Succeeded)
    );
    assert!(!jobs::allowed(State::Succeeded, State::Running));
    assert!(jobs::lost(State::Running, Some(0), jobs::LOST_AFTER_MS + 1));
    assert!(jobs::may_read(Some("u1"), None, "u1") && jobs::may_read(Some("s1"), Some("p1"), "p1"));
    assert!(!jobs::may_read(Some("u1"), None, "u2"));
    assert_eq!(jobs::expires_at(0, Some(5)), 5 + jobs::KEPT_AFTER_READ_MS);
    let note = jobs::notify_body(
        "j1",
        State::Succeeded,
        "anthropic",
        "m",
        Some("policy-1"),
        Some(1),
        Some(2),
        0,
    );
    assert!(!note.to_string().contains("Hello"));
}

// ---- secrets ----

#[test]
fn secrets_write_and_check() {
    use pigeon_capabilities::Slot;
    use pigeon_capabilities::expiry::Expiry;
    use pigeon_capabilities::secrets::{cloudflare_expiry, listing, write_purpose};
    use pigeon_core::oauth::hash;
    let slot = |name: &str| Slot {
        name: name.into(),
        purpose: format!("{name} purpose"),
        scopes: &[pigeon_core::secrets_store::WORKERS],
    };
    // What the secrets page lists: declared slots first, set or missing, then the others.
    let declared = vec![
        ("secrets".to_string(), slot("CF_PARENT_TOKEN")),
        ("mirror".to_string(), slot("GITHUB_MIRROR_TOKEN")),
    ];
    let stored = vec![
        ("ZETA".to_string(), None),
        (
            "CF_PARENT_TOKEN".to_string(),
            Some("2026-11-01".to_string()),
        ),
        ("ALPHA".to_string(), None),
    ];
    let listed = listing(&declared, &stored);
    let names: Vec<&str> = listed.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        ["CF_PARENT_TOKEN", "GITHUB_MIRROR_TOKEN", "ALPHA", "ZETA"]
    );
    assert!(listed[0].set && !listed[1].set);
    assert_eq!(listed[1].owner, "mirror");
    // A write is approved by a passkey bound to its name and the value's hash, never the value.
    let approval = write_purpose("GITHUB_MIRROR_TOKEN", &hash("ghp_new"));
    assert!(!approval.contains("ghp_new"));
    assert_ne!(
        approval,
        write_purpose("GITHUB_MIRROR_TOKEN", &hash("ghp_other"))
    );
    assert_ne!(approval, write_purpose("CF_PARENT_TOKEN", &hash("ghp_new")));
    // The check before it is written: the issuer's answer (stubbed Cloudflare verify).
    assert_eq!(
        cloudflare_expiry(&json!({ "status": "active", "expires_on": "2026-12-21T00:00:00Z" })),
        Expiry::At {
            at_ms: 1_797_811_200_000
        }
    );
    assert_eq!(
        cloudflare_expiry(&json!({ "status": "active", "expires_on": null })),
        Expiry::Never
    );
    for status in [json!("expired"), json!("disabled"), Value::Null] {
        let v = json!({ "status": status, "expires_on": "2026-12-21T00:00:00Z" });
        assert!(
            matches!(cloudflare_expiry(&v), Expiry::Refused { .. }),
            "{v}"
        );
    }
}

// ---- expiry ----

#[test]
fn expiry_readings() {
    use pigeon_capabilities::expiry::{self, Expiry, Row, Status};
    const DAY: i64 = 86_400_000;
    let at = 1_797_811_200_000; // 2026-12-21T00:00:00Z
    // The issuers' time shapes; junk is refused rather than misread.
    for t in [
        "2026-12-21T00:00:00Z",
        "2026-12-21T00:00:00.000Z",
        "2026-12-21 00:00:00 UTC",
        "2026-12-21T02:00:00+02:00",
        "2026-12-20 17:00:00 -0700",
        "2026-12-21",
    ] {
        assert_eq!(expiry::parse_time(t), Some(at), "{t}");
    }
    for t in [
        "",
        "2026-13-01",
        "2026-12-21T25:00:00Z",
        "2026-12-21T00:00:00 PST",
        "tomorrow",
        "2026-12-21T00:00:00+2",
    ] {
        assert_eq!(expiry::parse_time(t), None, "{t}");
    }
    assert_eq!(expiry::from_field(None), Expiry::Never);
    assert!(matches!(
        expiry::from_field(Some("soon")),
        Expiry::Unknown { .. }
    ));
    assert_eq!(expiry::date(at), "2026-12-21");
    // Read against now: ok, soon (a week ahead), expired, refused, unknown.
    let now = at - 30 * DAY;
    assert_eq!(expiry::status(&Expiry::At { at_ms: at }, now), Status::Ok);
    assert_eq!(
        expiry::status(&Expiry::At { at_ms: at }, at - 7 * DAY),
        Status::Soon
    );
    assert_eq!(
        expiry::status(&Expiry::At { at_ms: at }, at),
        Status::Expired
    );
    assert_eq!(
        expiry::status(&Expiry::Refused { why: "401".into() }, now),
        Status::Expired
    );
    assert_eq!(expiry::days_left(at, at - DAY - 1), 1);
    let row = Row::new(
        "GITHUB_MIRROR_TOKEN",
        "secret",
        "mirror",
        Some(Expiry::At { at_ms: at }),
        Some(now),
        at - 3 * DAY,
    );
    assert_eq!(
        (row.status, row.expires.as_deref(), row.days_left),
        (Status::Soon, Some("2026-12-21"), Some(3))
    );
    assert!(row.needs_you());
    let missing = Row::new("X", "secret", "x", None, None, now);
    assert_eq!(missing.status, Status::Missing);
    // A row never carries a value.
    let shown = serde_json::to_value(&row).unwrap();
    assert!(shown.get("value").is_none() && !shown.to_string().contains("ghp_"));
    // GitHub's answer for the mirror's token (stubbed header).
    use pigeon_capabilities::mirror::github_expiry;
    assert_eq!(
        github_expiry(200, Some("2026-12-21 00:00:00 UTC")),
        Expiry::At { at_ms: at }
    );
    assert_eq!(github_expiry(200, None), Expiry::Never);
    assert!(matches!(github_expiry(401, None), Expiry::Refused { .. }));
    assert!(matches!(github_expiry(502, None), Expiry::Unknown { .. }));
}

// ---- mirror ----

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

// ---- webhooks and OIDC ----

#[test]
fn webhook_and_oidc() {
    use pigeon_capabilities::ci::buildkite::{ISSUER, audience, vouched};
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

fn webhook_and_oidc_key() -> &'static rsa::RsaPrivateKey {
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

// ---- console ----

fn console_sign_in_and_a_write_assertion(
    key: &p256::ecdsa::SigningKey,
    challenge: &str,
    origin: &str,
    tamper: bool,
) -> pigeon_core::webauthn::Assertion {
    use p256::ecdsa::{Signature, signature::Signer};
    use pigeon_core::webauthn::{Assertion, b64url};
    use sha2::{Digest, Sha256};
    let client =
        format!(r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"{origin}"}}"#);
    let mut auth = Sha256::digest(b"platform.pigeon.dev").to_vec();
    auth.extend_from_slice(&[0x05, 0, 0, 0, 1]);
    let mut msg = auth.clone();
    msg.extend_from_slice(&Sha256::digest(client.as_bytes()));
    let sig: Signature = key.sign(&msg);
    if tamper {
        auth[36] ^= 0xff;
    }
    Assertion {
        id: b64url(b"cred-1"),
        client_data: b64url(client.as_bytes()),
        authenticator_data: b64url(&auth),
        signature: b64url(sig.to_der().as_bytes()),
    }
}

#[test]
fn console_sign_in_and_a_write() {
    use p256::ecdsa::SigningKey;
    use pigeon_capabilities::console::session::*;
    use pigeon_capabilities::console::writes::Write;
    use pigeon_core::webauthn::{Credential, RelyingParty, assert as verify, b64url};

    // Sign-in: the passkey's assertion verifies; a forged one does not.
    let key = SigningKey::random(&mut rand::rngs::OsRng);
    let other = SigningKey::random(&mut rand::rngs::OsRng);
    let cred = Credential {
        id: b64url(b"cred-1"),
        point: key
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec(),
        sign_count: 0,
    };
    let rp = RelyingParty {
        id: "platform.pigeon.dev",
        origin: "https://platform.pigeon.dev",
    };
    let good = "https://platform.pigeon.dev";
    let attempt = |k: &SigningKey, signed: &str, origin: &str, tamper: bool, expect: &str| {
        let a = console_sign_in_and_a_write_assertion(k, signed, origin, tamper);
        verify(&a, &cred, expect, &rp).ok()
    };
    let ch = "Y2hhbGxlbmdl";
    assert_eq!(attempt(&key, ch, good, false, ch), Some(1));
    assert_eq!(attempt(&other, ch, good, false, ch), None);
    assert_eq!(attempt(&key, "b3RoZXI", good, false, ch), None);
    assert_eq!(attempt(&key, ch, "https://evil.example", false, ch), None);
    assert_eq!(attempt(&key, ch, good, true, ch), None);

    // The session cookie, and the ones ignored.
    let c = cookie("pgn_ss_AbC-12_xyz");
    assert!(c.starts_with("__Host-pigeon_session=pgn_ss_AbC-12_xyz; "));
    for attr in [
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        "Max-Age=28800",
    ] {
        assert!(c.split("; ").any(|part| part == attr), "{attr}");
    }
    assert!(!c.contains("Domain"));
    assert!(clear().starts_with("__Host-pigeon_session=; ") && clear().ends_with("; Max-Age=0"));
    let t = "pgn_ss_AbC-12_xyz";
    assert_eq!(
        token_from(&format!("theme=dark;  {COOKIE}={t};lang=en")),
        Some(t)
    );
    let long = format!("{PREFIX}{}", "a".repeat(100 - PREFIX.len()));
    assert_eq!(token_from(&format!("{COOKIE}={long}")), Some(long.as_str()));
    assert_eq!(token_from(&format!("{COOKIE}={long}a")), None);
    for bad in [
        "pigeon_session=pgn_ss_AbC123".to_string(),
        format!("{COOKIE}=pgn_xx_AbC123"),
        format!("{COOKIE}=pgn_ss_AbC+123"),
        format!("{COOKIE}={PREFIX}"),
    ] {
        assert_eq!(token_from(&bad), None, "{bad}");
    }

    // Idle, too old, or from the future: not fresh.
    let now = 10 * MAX_MS;
    assert!(fresh(now - 1000, now - IDLE_MS + 1, now));
    assert!(!fresh(now - IDLE_MS, now - IDLE_MS, now));
    assert!(fresh(now - MAX_MS + 1, now, now));
    assert!(!fresh(now - MAX_MS, now, now));
    assert!(fresh(now + 60_000, now + 60_000, now));
    assert!(!fresh(now, now + 60_001, now));

    // A write comes from this origin, as JSON.
    let host = "platform.pigeon.dev";
    assert!(same_origin(Some(good), host));
    assert!(same_origin(Some("https://PLATFORM.pigeon.dev"), host));
    for bad in [
        "null",
        "http://platform.pigeon.dev",
        "https://platform.pigeon.dev:8443",
        "https://platform.pigeon.dev/",
        "https://evilplatform.pigeon.dev",
        "https://platform.pigeon.dev.evil.example",
    ] {
        assert!(!same_origin(Some(bad), host), "{bad}");
    }
    assert!(!same_origin(None, host));
    assert!(json_content(Some("Application/JSON; charset=utf-8")));
    assert!(!json_content(Some("application/jsonp")) && !json_content(None));

    // The write: approved by a passkey over its own purpose, bound to one write and one target.
    assert_eq!(Write::named("land-approve"), Some(Write::LandApprove));
    assert_eq!(Write::named("users-remove"), None);
    let (p1, p2) = (
        Write::LandApprove.purpose("L1"),
        Write::LandApprove.purpose("L2"),
    );
    assert_ne!(p1, p2);
    assert_ne!(p1, Write::DisconnectApp.purpose("L1"));
    assert!(!Write::Invite.purpose("Ada").contains("Ada"));
    let signed = b64url(p1.as_bytes());
    assert_eq!(attempt(&key, &signed, good, false, &signed), Some(1));
    assert_eq!(
        attempt(&key, &signed, good, false, &b64url(p2.as_bytes())),
        None
    );
    assert_eq!(
        Write::DisconnectApp.target(" a_B-9 "),
        Ok("a_B-9".to_string())
    );
    assert!(
        Write::RemovePasskey.target("a/b").is_err() && Write::RemovePasskey.target("").is_err()
    );
    assert_eq!(
        Write::Invite.target("  Ada Lovelace "),
        Ok("Ada Lovelace".to_string())
    );
    assert!(
        Write::Invite.target("a\nb").is_err() && Write::Invite.target(&"x".repeat(65)).is_err()
    );
    assert_eq!(Write::LandApproval.target(" on "), Ok("on".to_string()));
    assert!(
        Write::LandApproval.target("yes").is_err() && Write::LandApprove.target("L1/../x").is_err()
    );
}

// ---- proxy and job tokens ----

#[test]
fn proxy_and_job_tokens() {
    use pigeon_capabilities::ci::token::{self, Grant, PREFIX};
    use pigeon_capabilities::proxy::{self, Audit, DEFAULT_DAYS, Outcome, ProxyCaller};

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
    // Issuing and revoking one are approved for that one credential.
    assert_ne!(
        proxy::issue_purpose("h1", 30),
        proxy::issue_purpose("h2", 30)
    );
    assert_ne!(
        proxy::issue_purpose("h1", 30),
        proxy::issue_purpose("h1", 31)
    );
    assert_ne!(
        proxy::revoke_purpose("aaaaaaaaaaaa"),
        proxy::revoke_purpose("aaaaaaaaaaab")
    );
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
    };
    let named: Value = serde_json::from_str(&proxy::audit_line(Some(&caller), &audit)).unwrap();
    assert_eq!(named["caller"], "env (Ana)");
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

/// The apex (pigeon.dev) answers only the site's pages; the platform's paths never answer there.
#[test]
fn site_on_the_apex() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::site::{PAGES, Site, is_apex, page};

    assert!(is_apex("pigeon.dev", "pigeon.dev"), "the apex");
    assert!(is_apex("PIGEON.dev", "pigeon.dev"), "ignoring case");
    for host in [
        "platform.pigeon.dev",
        "proxy.pigeon.dev",
        "www.pigeon.dev",
        "pigeon.dev.evil.com",
    ] {
        assert!(!is_apex(host, "pigeon.dev"), "{host} is not the apex");
    }

    for (i, path) in PAGES.iter().enumerate() {
        assert_eq!(page("GET", path), Some(i), "GET {path}");
        assert_eq!(page("HEAD", path), Some(i), "HEAD {path}");
    }
    for path in [
        "/mcp",
        "/auth/",
        "/oauth/token",
        "/.well-known/oauth-authorization-server",
        "/secrets",
        "/tokens",
        "/health",
        "/git/x",
        "/cf/client/v4",
        "/settings",
        "/architecture.json/",
        "//",
    ] {
        assert_eq!(page("GET", path), None, "GET {path} on the apex");
    }
    for (method, path) in [
        ("POST", "/"),
        ("PUT", "/architecture.json"),
        ("DELETE", "/"),
    ] {
        assert_eq!(page(method, path), None, "{method} {path} on the apex");
    }

    let site = Site {
        zone: "pigeon.dev".into(),
        worker: "pigeon-platform".into(),
    };
    let fp = site.footprint();
    assert_eq!(fp.hostnames.len(), 1, "one hostname");
    let h = &fp.hostnames[0];
    assert_eq!(h.host, "pigeon.dev", "the apex");
    assert_eq!(h.worker, "pigeon-platform", "served by the platform");
    assert!(h.access.is_empty(), "in public");
    assert!(fp.workers.is_empty(), "no Worker of its own");
    let tools: Vec<String> = site.tools().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(tools, ["site_publish"], "site_publish only");
}

#[test]
fn linked_account_github_app_and_site_layout() {
    use std::collections::BTreeSet;

    use pigeon_core::cloudflare_oauth::{
        Client, authorize_url, callback_params, challenge, needs_refresh, read_tokens,
    };
    use pigeon_core::github_app::{
        jwt_signing_input, pkcs1_to_pkcs8, private_key_der, read_push, status_body, token_request,
        verify_webhook,
    };
    use pigeon_core::r2::{self, Manifest};
    use pigeon_core::sign::{hex, hmac_sha256};
    use pigeon_core::webauthn::unb64url;
    use pigeon_core::workers::{Route, RoutePlan, plan_route};
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::pkcs8::EncodePrivateKey;

    // (1) Linking: the authorize URL carries PKCE and state; the callback refuses errors and a
    // missing state; tokens are read without echoing them, and refresh keeps the old refresh
    // token when none comes back.
    let client = Client {
        client_id: "cid".into(),
        redirect_uri: "https://platform.example/linked/callback".into(),
        scopes: vec!["account.read".into(), "zone.read".into()],
    };
    let verifier = "v".repeat(43);
    let url = authorize_url(&client, "st8", &challenge(&verifier));
    assert!(
        url.starts_with("https://dash.cloudflare.com/oauth2/auth?"),
        "{url}"
    );
    for part in [
        "response_type=code",
        "client_id=cid",
        "state=st8",
        "code_challenge_method=S256",
        "scope=account.read%20zone.read",
        "redirect_uri=https%3A%2F%2Fplatform.example%2Flinked%2Fcallback",
    ] {
        assert!(url.contains(part), "{part} in {url}");
    }
    assert!(
        pigeon_core::oauth::pkce(&verifier, &challenge(&verifier)),
        "S256"
    );
    assert_eq!(
        callback_params("code=c1&state=st8"),
        Ok(("c1".to_string(), "st8".to_string()))
    );
    assert!(
        callback_params("error=access_denied&state=st8").is_err(),
        "denied"
    );
    assert!(callback_params("code=c1").is_err(), "no state");
    let t = read_tokens(
        r#"{"access_token":"at1","token_type":"Bearer","expires_in":3600,"refresh_token":"rt1"}"#,
        1_000.0,
        None,
    )
    .unwrap();
    assert_eq!(t.refresh.as_deref(), Some("rt1"));
    assert!(!needs_refresh(&t, 1_000.0), "fresh");
    assert!(
        needs_refresh(&t, 1_000.0 + 3_400_000.0),
        "under five minutes left"
    );
    let again = read_tokens(
        r#"{"access_token":"at2","token_type":"bearer","expires_in":60}"#,
        0.0,
        Some("rt1"),
    )
    .unwrap();
    assert_eq!(
        again.refresh.as_deref(),
        Some("rt1"),
        "the old refresh token is kept"
    );
    let refused = read_tokens(
        r#"{"error":"invalid_grant","error_description":"reused"}"#,
        0.0,
        Some("rt-secret"),
    )
    .unwrap_err();
    assert!(
        refused.contains("invalid_grant") && !refused.contains("rt-secret"),
        "{refused}"
    );
    assert!(
        !format!("{t:?}").contains("at1"),
        "Debug never shows a token"
    );

    // (2) The GitHub App: its key as GitHub hands it out (PKCS#1) becomes the PKCS#8 that
    // WebCrypto imports; a JWT is short-lived; only a correct signature over these bytes passes.
    let key = webhook_and_oidc_key();
    let pkcs1 = key.to_pkcs1_der().unwrap();
    let pkcs8 = key.to_pkcs8_der().unwrap();
    assert_eq!(
        pkcs1_to_pkcs8(pkcs1.as_bytes()),
        pkcs8.as_bytes(),
        "wrapped as rsa does"
    );
    let pem = |label: &str, der: &[u8]| {
        let b64 = pigeon_core::git::base64(der);
        let lines: Vec<&str> = b64
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            lines.join("\n")
        )
    };
    assert_eq!(
        private_key_der(&pem("RSA PRIVATE KEY", pkcs1.as_bytes())).as_deref(),
        Some(pkcs8.as_bytes())
    );
    assert_eq!(
        private_key_der(&pem("PRIVATE KEY", pkcs8.as_bytes())).as_deref(),
        Some(pkcs8.as_bytes())
    );
    assert!(private_key_der(&pem("PUBLIC KEY", pkcs8.as_bytes())).is_none());
    let input = jwt_signing_input("Iv1.abc", 1_000_000);
    let (h, c) = input.split_once('.').unwrap();
    let header: Value = serde_json::from_slice(&unb64url(h).unwrap()).unwrap();
    let claims: Value = serde_json::from_slice(&unb64url(c).unwrap()).unwrap();
    assert_eq!(header, json!({ "alg": "RS256", "typ": "JWT" }));
    assert_eq!(
        claims,
        json!({ "iat": 999_940, "exp": 1_000_540, "iss": "Iv1.abc" })
    );

    let secret = b"gh-hook";
    let body = br#"{"ref":"refs/heads/main","after":"abc123","deleted":false,"repository":{"full_name":"noisypigeon/pigeon"},"installation":{"id":42},"head_commit":{"message":"post"}}"#;
    let good = format!("sha256={}", hex(&hmac_sha256(secret, body)));
    assert!(verify_webhook(secret, body, Some(&good)));
    assert!(!verify_webhook(secret, b"{}", Some(&good)), "other bytes");
    assert!(!verify_webhook(b"other", body, Some(&good)), "other secret");
    assert!(!verify_webhook(secret, body, None), "unsigned");
    assert!(
        !verify_webhook(secret, body, Some(&good[7..])),
        "no sha256= prefix"
    );
    let push = read_push(Some("push"), body).unwrap().unwrap();
    assert_eq!(
        (
            push.repo.as_str(),
            push.git_ref.as_str(),
            push.after.as_str(),
            push.installation
        ),
        ("noisypigeon/pigeon", "refs/heads/main", "abc123", 42)
    );
    assert_eq!(read_push(Some("ping"), b"{}"), Ok(None));
    assert!(
        read_push(Some("push"), b"{}").is_err(),
        "a push without its fields"
    );
    assert_eq!(
        token_request(&["pigeon"], &[("contents", "read")]),
        json!({ "repositories": ["pigeon"], "permissions": { "contents": "read" } })
    );
    let long = "x".repeat(300);
    let status = status_body("success", Some("https://noisypigeon.com"), None, &long);
    assert_eq!(status["description"].as_str().unwrap().chars().count(), 140);
    assert!(status.get("log_url").is_none(), "absent urls are left out");

    // (3) The site's layout: content-addressed blobs, a manifest per commit, directory paths
    // resolved, nothing uploaded twice, and the live deploy never pruned.
    let files = vec![
        ("index.html".to_string(), b"<h1>home</h1>".to_vec()),
        ("/posts/a/index.html".to_string(), b"<p>a</p>".to_vec()),
        ("/img/p.jpg".to_string(), b"jpeg".to_vec()),
        ("/img/copy.jpg".to_string(), b"jpeg".to_vec()),
    ];
    let manifest: Manifest = r2::manifest_of(&files);
    assert!(manifest.contains_key("/index.html"), "paths start with /");
    assert_eq!(
        manifest["/img/p.jpg"].blob, manifest["/img/copy.jpg"].blob,
        "same bytes, one blob"
    );
    assert_eq!(
        manifest["/index.html"].content_type,
        "text/html; charset=utf-8"
    );
    let present: BTreeSet<String> = [manifest["/img/p.jpg"].blob.clone()].into();
    assert_eq!(
        r2::missing(&manifest, &present).len(),
        2,
        "only what the bucket lacks"
    );
    for (request, served) in [
        ("/", Some("/index.html")),
        ("/posts/a", Some("/posts/a/index.html")),
        ("/posts/a/", Some("/posts/a/index.html")),
        ("/img/p.jpg", Some("/img/p.jpg")),
        ("/nope", None),
        ("/posts/../index.html", None),
    ] {
        assert_eq!(
            r2::resolve(&manifest, request).as_deref(),
            served,
            "{request}"
        );
    }
    let deploys: Vec<(String, u64)> = (0..5).map(|i| (format!("c{i}"), i)).collect();
    let mut pruned = r2::prune(&deploys, "c0", 2);
    pruned.sort();
    assert_eq!(
        pruned,
        ["c1", "c2"],
        "the oldest beyond two, never the live one"
    );

    // (4) The cutover route: created once, never taking a pattern another Worker serves.
    let routes = vec![Route {
        id: "r1".into(),
        pattern: "noisypigeon.com/*".into(),
        script: Some("old-site".into()),
    }];
    assert_eq!(
        plan_route(&routes, "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Conflict("old-site".into())
    );
    assert_eq!(
        plan_route(&[], "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Create
    );
    let ours = vec![Route {
        script: Some("pigeon-site-blog".into()),
        ..routes[0].clone()
    }];
    assert_eq!(
        plan_route(&ours, "noisypigeon.com/*", "pigeon-site-blog"),
        RoutePlan::Present("r1".into())
    );
}

#[test]
fn instances_are_data_a_person_approves() {
    use pigeon_capabilities::instances::record::{
        self, AccountRef, Instance, Rules, Target, diff, reserved_labels, site_for_host, validate,
    };
    use pigeon_capabilities::instances::{template, write_purpose};

    // The noisypigeon record as the person will approve it: a linked account, its zone, and the
    // blog with a preview, the zone (off until the cutover) and the subdomain `noisypigeon`.
    let mut noisy: Instance = serde_json::from_value(template()).unwrap();
    noisy.id = "noisypigeon".into();
    noisy.sites[0].bucket = "noisypigeon-site".into();
    noisy.worker = Some("noisypigeon".into());
    noisy.title = "noisypigeon".into();
    noisy.zones = vec!["noisypigeon.com".into()];
    noisy.sites[0].build = Some("workloads/noisypigeon.com/blog".into());
    noisy.sites[0].targets = vec![
        Target::Preview,
        Target::Zone {
            hostname: "noisypigeon.com".into(),
            route: "noisypigeon.com/*".into(),
            enabled: false,
        },
        Target::Subdomain {
            label: "noisypigeon".into(),
        },
    ];
    let platform: Instance = serde_json::from_value(json!({
        "id": "pigeon-security/platform", "title": "pigeon",
        "account": { "kind": "home" }, "zones": ["pigeon.dev"],
        "repo": { "kind": "artifacts", "namespace": "pigeon-security", "repo": "pigeon" }
    }))
    .unwrap();
    let reserved = reserved_labels(
        &[
            "platform.pigeon.dev".into(),
            "proxy.pigeon.dev".into(),
            "pigeon.dev".into(),
        ],
        "pigeon.dev",
    );
    assert!(reserved.contains(&"platform".to_string()) && reserved.contains(&"mcp".to_string()));
    let workers = vec!["pigeon-platform".to_string()];
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
    };
    assert_eq!(
        validate(&noisy, std::slice::from_ref(&platform), &rules),
        Vec::<String>::new()
    );
    assert_eq!(
        validate(&platform, &[noisy.clone()], &rules),
        Vec::<String>::new()
    );

    // What pigeon may be given is checked: its own names, hostnames in the instance's zones,
    // no platform hostnames, no subdomain taken twice, no unknown fields.
    let refused = |change: &dyn Fn(&mut Instance), why: &str| {
        let mut bad = noisy.clone();
        change(&mut bad);
        let problems = validate(&bad, std::slice::from_ref(&platform), &rules);
        assert!(
            problems.iter().any(|p| p.contains(why)),
            "{why}: {problems:?}"
        );
    };
    // The bucket is named after the instance, like its Worker; another name needs the
    // person's approved override, and no two instances share one.
    assert_eq!(noisy.sites[0].bucket, "noisypigeon-site");
    assert_eq!(
        record::bucket_for(&noisy).as_deref(),
        Some("noisypigeon-site")
    );
    refused(
        &|i| i.sites[0].bucket = "blog-noisypigeon-com".into(),
        "should be \"noisypigeon-site\"",
    );
    let mut kept = noisy.clone();
    kept.sites[0].bucket = "blog-noisypigeon-com".into();
    kept.sites[0].bucket_override = true;
    assert_eq!(
        validate(&kept, std::slice::from_ref(&platform), &rules),
        Vec::<String>::new(),
        "an approved override"
    );
    let mut foo = noisy.clone();
    foo.id = "foo".into();
    foo.zones.clear();
    foo.worker = Some("foo".into());
    foo.sites[0].targets = vec![Target::Subdomain {
        label: "foo".into(),
    }];
    assert_eq!(
        record::bucket_for(&foo).as_deref(),
        Some("foo-site"),
        "a subdomain-only instance"
    );
    let mut same_bucket = foo.clone();
    same_bucket.sites[0].bucket = "noisypigeon-site".into();
    same_bucket.sites[0].bucket_override = true;
    let problems = validate(&same_bucket, std::slice::from_ref(&noisy), &rules);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("already used by instance noisypigeon")),
        "{problems:?}"
    );
    // One Worker per instance, named after its zone (or its subdomain when it has no zone),
    // never the platform's own or another instance's.
    assert_eq!(noisy.worker.as_deref(), Some("noisypigeon"));
    refused(
        &|i| i.worker = Some("my-worker".into()),
        "should be \"noisypigeon\"",
    );
    refused(&|i| i.worker = None, "worker is missing");
    refused(
        &|i| {
            i.worker = Some("pigeon-platform".into());
            i.worker_override = true;
        },
        "platform's own",
    );
    refused(
        &|i| i.sites[0].targets[2] = Target::Subdomain { label: "np".into() },
        "make them the same",
    );
    refused(
        &|i| {
            i.sites[0].targets[1] = Target::Zone {
                hostname: "evil.example".into(),
                route: "evil.example/*".into(),
                enabled: true,
            }
        },
        "not in this instance's zones",
    );
    refused(
        &|i| {
            i.sites[0].targets[2] = Target::Subdomain {
                label: "platform".into(),
            }
        },
        "reserved",
    );
    refused(&|i| i.zones.push("pigeon.dev".into()), "platform zone");
    refused(
        &|i| {
            i.account = AccountRef::Linked {
                account_id: Some("not-hex".into()),
            }
        },
        "32 lowercase hex",
    );
    refused(
        &|i| i.sites[0].build = Some("../etc".into()),
        "relative path",
    );
    let mut taken = platform.clone();
    taken.sites = noisy.sites.clone();
    taken.sites[0].targets = vec![Target::Subdomain {
        label: "noisypigeon".into(),
    }];
    let problems = validate(&taken, &[noisy.clone()], &rules);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("already used by instance noisypigeon")),
        "{problems:?}"
    );
    let unknown = json!({ "id": "x", "title": "x", "account": { "kind": "home" }, "extra": 1 });
    assert!(
        serde_json::from_value::<Instance>(unknown).is_err(),
        "unknown fields refused"
    );

    // The person sees exactly what changes, and the approval names the record and its hash.
    let mut on = noisy.clone();
    on.sites[0].targets[1] = Target::Zone {
        hostname: "noisypigeon.com".into(),
        route: "noisypigeon.com/*".into(),
        enabled: true,
    };
    on.version = 7;
    let changes = diff(Some(&noisy), &on);
    assert_eq!(changes.len(), 1, "the version is not a change: {changes:?}");
    assert_eq!(changes[0].path, "sites[0].targets[1].enabled");
    assert_eq!(
        (changes[0].from.clone(), changes[0].to.clone()),
        (json!(false), json!(true))
    );
    assert!(
        diff(None, &noisy).len() > 10,
        "a new record lists every value"
    );
    assert_ne!(record::digest(&noisy), record::digest(&on));
    assert_eq!(
        write_purpose("noisypigeon", "abc"),
        "instance:noisypigeon:abc"
    );

    // The subdomain is served by host, from the data alone.
    let all = [platform, noisy];
    let (i, site) = site_for_host(&all, "NoisyPigeon.pigeon.dev", "pigeon.dev").unwrap();
    assert_eq!((i.id.as_str(), site.name.as_str()), ("noisypigeon", "blog"));
    assert!(site_for_host(&all, "platform.pigeon.dev", "pigeon.dev").is_none());
    assert!(site_for_host(&all, "noisypigeon.pigeon.dev.evil.com", "pigeon.dev").is_none());
}

// ---- tool call approvals ----

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
    use pigeon_capabilities::console::writes::Write;
    use pigeon_core::approval::{self, Kind, Refusal, State};
    use pigeon_core::policy::{self, Approval, Decision};

    // The person's rules, as phase 2 adds them: each of the six needs the passkey; every other
    // tool runs as before.
    let text = format!(
        "{}\n{}",
        pigeon_capabilities::policy::TEXT,
        r#"
[[rule]]
id = "risky-tools-need-passkey"
actions = ["call_tool"]
targets = ["deploy_token", "source_token", "setup_platform", "users_remove", "passkeys_remove", "oauth_revoke"]
decision = "allow"
approval = "passkey"
reason = "the person approves each call"

[[rule]]
id = "tools-run"
actions = ["call_tool"]
decision = "allow"
reason = "every other tool runs as it does today"
"#
    );
    let p = policy::parse(&text).unwrap();
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

    // The policy built into the Worker gates nothing until the person's rule lands: calls are
    // not asked about, and no tool's schema changes.
    assert!(!approvals::asked());
    let mut tools = vec![pigeon_core::mcp::Tool::bare("deploy_token", "mint")];
    approvals::annotate(&mut tools);
    assert!(
        tools[0].input_schema["properties"]
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
    assert_eq!(
        Write::named("approval-approve"),
        Some(Write::ApprovalApprove)
    );
    assert!(Write::ApprovalApprove.bound_to_digest());
    assert!(!Write::ApprovalDeny.bound_to_digest());
    assert!(Write::ApprovalApprove.target("not an id!").is_err());
}

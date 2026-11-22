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

    // The guarded paths a landing changed, from the paths it changed since its base.
    assert_eq!(
        state::guarded_in(&["ci/runner/test.sh", "src/x.rs", ".pigeon/policy.toml"]),
        [".pigeon/policy.toml", ".pigeon", "ci"]
    );
    assert!(state::guarded_in(&["cifar.rs", "docs/ci/x.md"]).is_empty());

    // checking → testing → awaiting_approval → ready → merging → merged → deploying → landed.
    let passed = Seen::JobEnded { passed: true };
    let mut at = State::Checking;
    for (seen, to) in [
        (&Seen::Checked, State::Testing),
        (&passed, State::AwaitingApproval),
        (&Seen::Approved, State::Ready),
        (&Seen::Start, State::Merging),
        (&Seen::Merged, State::Merged),
        (&Seen::Start, State::Deploying),
        (&passed, State::Landed),
    ] {
        let next = state::next(at, true, seen).unwrap();
        assert_eq!(next, to);
        assert!(state::allowed(at, next));
        at = next;
    }
    assert!(at.terminal());
    // Without an approval to wait for, tests passing wait for the merge lock.
    assert_eq!(
        state::next(State::Testing, false, &passed),
        Some(State::Ready)
    );
    // Main moved under a landing: tested again, refused, or (docs) landed with no deploy.
    assert_eq!(
        state::next(State::Merging, false, &Seen::Retest),
        Some(State::Testing)
    );
    assert_eq!(
        state::next(State::Merging, false, &Seen::RebaseRefused),
        Some(State::RebaseRefused)
    );
    assert_eq!(
        state::next(State::Merging, false, &Seen::DocsMerged),
        Some(State::Landed)
    );
    // A newer landing's deploy carries the older merged ones, or fails for them.
    assert_eq!(
        state::next(State::Merged, false, &Seen::Superseded),
        Some(State::Landed)
    );
    // Each step fails its own way, and nothing moves a final state or skips a step.
    let failed = Seen::JobEnded { passed: false };
    assert_eq!(
        state::next(State::Merged, false, &failed),
        Some(State::DeployFailed)
    );
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
    assert!(!state::allowed(State::Testing, State::Merging));
    assert!(State::Ready.cancellable() && !State::Merging.cancellable());
    assert!(!State::Merged.cancellable() && !State::Merged.terminal());

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

#[test]
fn land_deploys_in_lanes_per_worker() {
    use pigeon_capabilities::land::lanes::{self, Deploy, DeployState, Merged, Start};
    use pigeon_capabilities::land::state::{self, Seen, State};
    let m = |id: &str, at: i64, workers: &[&str]| Merged {
        id: id.into(),
        merged_at: at,
        commit: format!("c{at}"),
        workers: workers.iter().map(|w| w.to_string()).collect(),
        ordered: true,
    };
    let d = |worker: &str, covers: i64, state: DeployState| Deploy {
        worker: worker.into(),
        covers,
        state,
        incident: false,
    };
    let start = |worker: &str, at: i64, carries: &[&str]| Start {
        worker: worker.into(),
        landing: carries.last().unwrap().to_string(),
        commit: format!("c{at}"),
        covers: at,
        carries: carries.iter().map(|c| c.to_string()).collect(),
    };
    use DeployState::{Error, Failed, Running, Succeeded};
    let passed = Some(Seen::JobEnded { passed: true });
    let failed = Some(Seen::JobEnded { passed: false });

    // A shared landing (core and sites), then sites alone, then core alone: core deploys the
    // newest commit for it, carrying the shared one; sites waits for core to deploy that.
    let (l1, l2, l3) = (
        m("L1", 1, &["core", "sites"]),
        m("L2", 2, &["sites"]),
        m("L3", 3, &["core"]),
    );
    let all = [l1.clone(), l2.clone(), l3.clone()];
    assert_eq!(
        lanes::starts("core", &all, &[], &[]),
        [start("core", 3, &["L1", "L3"])]
    );
    let mut deploys = vec![d("core", 3, Running)];
    assert!(lanes::starts("core", &all, &deploys, &[]).is_empty());
    assert_eq!(lanes::outcome(&l1, &deploys), None);
    // Core deployed: core alone is landed; sites now deploys its newest, carrying the shared.
    deploys[0].state = Succeeded;
    assert_eq!(lanes::outcome(&l3, &deploys), passed);
    assert_eq!(lanes::outcome(&l1, &deploys), None);
    let live = [l1.clone(), l2.clone()];
    assert_eq!(
        lanes::starts("core", &live, &deploys, &[]),
        [start("sites", 2, &["L1", "L2"])]
    );
    // One deploy per lane: a sites landing merged meanwhile waits, then goes alone.
    deploys.push(d("sites", 2, Running));
    let l4 = m("L4", 4, &["sites"]);
    let live = [l1.clone(), l2.clone(), l4.clone()];
    assert!(lanes::starts("core", &live, &deploys, &[]).is_empty());
    deploys[1].state = Succeeded;
    assert_eq!(lanes::outcome(&l1, &deploys), passed);
    assert_eq!(lanes::outcome(&l2, &deploys), passed);
    assert_eq!(lanes::outcome(&l4, &deploys), None);
    assert_eq!(
        lanes::starts("core", std::slice::from_ref(&l4), &deploys, &[]),
        [start("sites", 4, &["L4"])]
    );

    // Lanes run in parallel: core and models start together; sites waits behind the shared
    // landing, since its newer commit holds the shared change.
    let deploys = vec![d("core", 3, Succeeded), d("sites", 4, Succeeded)];
    let (l5, l6, l7) = (
        m("L5", 5, &["core", "sites"]),
        m("L6", 6, &["sites"]),
        m("L7", 7, &["models"]),
    );
    let live = [l7.clone(), l6.clone(), l5.clone()];
    assert_eq!(
        lanes::starts("core", &live, &deploys, &[]),
        [start("core", 5, &["L5"]), start("models", 7, &["L7"])]
    );

    // A failed deploy fails every landing it carried for that Worker, and only those: a later
    // landing for the Worker deploys alone, and one the Worker had already deployed stays landed.
    let deploys = vec![
        d("core", 5, Succeeded),
        d("sites", 4, Succeeded),
        d("sites", 6, Failed),
    ];
    assert_eq!(lanes::outcome(&l5, &deploys), failed);
    assert_eq!(lanes::outcome(&l6, &deploys), failed);
    assert_eq!(lanes::outcome(&l4, &deploys), passed);
    let l8 = m("L8", 8, &["sites"]);
    let live = [l5.clone(), l6.clone(), l8.clone()];
    assert_eq!(
        lanes::starts("core", &live, &deploys, &[]),
        [start("sites", 8, &["L8"])]
    );
    // A deploy the gateway could not run ends its landings in error.
    assert_eq!(
        lanes::outcome(&l8, &[d("sites", 8, Error)]),
        Some(Seen::Failed)
    );
    assert!(
        lanes::starts(
            "core",
            std::slice::from_ref(&l8),
            &[d("sites", 8, Error)],
            &[]
        )
        .is_empty()
    );
    for s in [Running, Succeeded, Failed, Error] {
        assert_eq!(DeployState::named(s.name()), Some(s));
    }

    // The landing's moves are the ones it had: started, then landed or failed; landed straight
    // from merged when deploys it never saw start covered it.
    assert_eq!(
        state::next(State::Merged, false, &Seen::Start),
        Some(State::Deploying)
    );
    assert_eq!(
        state::next(State::Deploying, false, &passed.clone().unwrap()),
        Some(State::Landed)
    );
    assert_eq!(
        state::next(State::Deploying, false, &failed.clone().unwrap()),
        Some(State::DeployFailed)
    );
    assert_eq!(
        state::next(State::Merged, false, &Seen::Superseded),
        Some(State::Landed)
    );
    assert_eq!(
        state::next(State::Deploying, false, &Seen::Failed),
        Some(State::Error)
    );
}

#[test]
fn ci_retries_a_pinned_job_on_its_connector() {
    use pigeon_ci::scheduler::retry_away;
    // A deploy job pinned to containers whose container stopped before reporting retries on
    // containers (incident 6 failed it: "no connector can run it").
    let (excluded, said) = retry_away(
        Vec::new(),
        "containers",
        Some("containers"),
        "the container stopped before reporting",
    );
    assert!(excluded.is_empty());
    assert!(said.starts_with("retrying on containers"));
    // Unpinned, the connector that lost it is excluded once, and the retry goes elsewhere.
    let (excluded, said) = retry_away(vec!["buildkite".into()], "containers", None, "lost");
    assert_eq!(excluded, ["buildkite", "containers"]);
    assert_eq!(said, "retrying elsewhere: lost");
    let (excluded, _) = retry_away(excluded, "containers", Some("buildkite"), "lost");
    assert_eq!(excluded, ["buildkite", "containers"]);
}

#[test]
fn lanes_scope_paths_and_order_only_the_contract() {
    use pigeon_capabilities::land::lanes::{self, Merged};
    use pigeon_core::lanes::{self as map_lanes, Owner};
    let map = map_lanes::parse(
        r#"
version = 1
[docs]
paths = ["docs/*"]
[shared]
paths = ["src/core/*", "src/contract/*", "Cargo.lock"]
core_first = ["src/contract/*", "ci/workers.toml"]
[checks]
paths = ["ci/runner/test.sh", ".pigeon/ci.toml"]
[[worker]]
name = "core"
script = "pigeon-platform"
dir = "src/platform"
paths = ["src/platform/*", "src/models/*", "ci/workers.toml"]
test_job = "test"
deploy_job = "deploy"
[[worker]]
name = "model-gateway"
script = "pigeon-model-gateway"
dir = "src/model-gateway"
paths = ["src/model-gateway/*", "src/models/*"]
test_job = "test"
deploy_job = "deploy-model-gateway"
[[worker]]
name = "sites"
script = "pigeon-sites"
dir = "src/sites"
paths = ["src/sites/*"]
test_job = "test"
deploy_job = "deploy-sites"
"#,
    )
    .unwrap();
    let names = |a: &map_lanes::Affected| a.workers.iter().cloned().collect::<Vec<_>>();
    // A library both link deploys both, and only them; it is not the contract.
    let models = map.classify(&["src/models/src/runner.rs"]);
    assert_eq!(names(&models), ["core", "model-gateway"]);
    assert!(!models.shared && !models.ordered);
    assert!(matches!(map.owner("src/models/src/lib.rs"), Owner::Workers(ws) if ws.len() == 2));
    // The gate, CI's jobs and the smoke suite: tested, deployed nowhere, never docs-only.
    for checks in [
        &["ci/runner/test.sh"][..],
        &[".pigeon/ci.toml"],
        &["tests/smoke.rs"],
    ] {
        let a = map.classify(checks);
        assert!(
            !a.docs_only && !a.shared && a.workers.is_empty(),
            "{checks:?}"
        );
    }
    // Shared deploys everything; core goes first only for the contract (or the map).
    let core_lib = map.classify(&["src/core/src/git.rs"]);
    assert!(core_lib.shared && !core_lib.ordered && core_lib.workers.len() == 3);
    assert!(map.classify(&["src/contract/src/wire.rs"]).ordered);
    assert!(map.classify(&["ci/workers.toml"]).ordered);
    // Main moving a checks path retests; moving another Worker's path does not.
    assert!(
        map.overlap(&["src/sites/src/lib.rs"], &["ci/runner/test.sh"])
            .retest
    );
    assert!(
        !map.overlap(&["src/sites/src/lib.rs"], &["src/models/src/lib.rs"])
            .retest
    );
    // A map without `core_first` keeps the older rule: whatever affects core is ordered.
    let old = map_lanes::parse(
        "version = 1\n[docs]\npaths = []\n[shared]\npaths = []\n[[worker]]\nname = \"core\"\nscript = \"p\"\ndir = \"src/p\"\npaths = [\"src/p/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy\"\n",
    )
    .unwrap();
    assert!(old.classify(&["src/p/x.rs"]).ordered);

    // The lockfile: a change to the workspace's own entries alone is not shared; a registry
    // package added or bumped is, and so is a lockfile that does not parse.
    let lock = |extra_dep: &str, serde: &str| {
        format!(
            "version = 4\n\n[[package]]\nname = \"pigeon-model-gateway\"\nversion = \"0.1.0\"\ndependencies = [\n \"pigeon-core\",{extra_dep}\n]\n\n[[package]]\nname = \"serde_json\"\nversion = \"{serde}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"00\"\n"
        )
    };
    let before = lock("", "1.0.140");
    assert!(map_lanes::lock_members_only(
        &before,
        &lock("\n \"serde_json\",", "1.0.140")
    ));
    assert!(!map_lanes::lock_members_only(&before, &lock("", "1.0.141")));
    assert!(!map_lanes::lock_members_only(&before, "not a lockfile ["));

    // Lanes: an unordered landing for core and the gateway deploys both at once; an ordered one
    // holds the gateway until core has deployed it.
    let m = |id: &str, at: i64, ordered: bool| Merged {
        id: id.into(),
        merged_at: at,
        commit: format!("c{at}"),
        workers: vec!["core".into(), "model-gateway".into()],
        ordered,
    };
    let starting = |l: Merged| {
        lanes::starts("core", &[l], &[], &[])
            .into_iter()
            .map(|s| s.worker)
            .collect::<Vec<_>>()
    };
    assert_eq!(starting(m("U", 1, false)), ["core", "model-gateway"]);
    assert_eq!(starting(m("O", 1, true)), ["core"]);
}

#[test]
fn land_attributes_a_failed_deploy() {
    use Tried::{Error, Failed, Passed};
    use pigeon_capabilities::land::lanes::{
        self, Deploy, DeployState, Good, Included, Merged, Next, Purpose, REDEPLOYS, Tried,
    };
    let inc = |ids: &[i64]| -> Vec<Included> {
        ids.iter()
            .map(|at| Included {
                id: format!("L{at}"),
                commit: format!("c{at}"),
                merged_at: *at,
            })
            .collect()
    };
    let good = |at: i64| Good {
        commit: format!("c{at}"),
        covers: at,
    };
    let deploy = |purpose: Purpose, at: i64, carries: &[&str]| Next::Deploy {
        purpose,
        commit: format!("c{at}"),
        covers: at,
        carries: carries.iter().map(|c| c.to_string()).collect(),
    };
    let done = |next: Next| match next {
        Next::Done(v) => v,
        other => panic!("not done: {other:?}"),
    };
    let ids = |failed: &[(String, String)]| -> Vec<String> {
        failed.iter().map(|(id, _)| id.clone()).collect()
    };

    // Sites was last good at c3; its deploy of c7 carried L5, L6 and L7, and failed. Roll back
    // at once, then deploy the included commits oldest first. L7's commit is known to fail, so
    // it is never deployed again: when L5 and L6 pass, L7 is the cause.
    let three = inc(&[5, 6, 7]);
    let g3 = good(3);
    let next = |tried: &[Tried]| lanes::bisect("sites", Some(&g3), &three, tried, REDEPLOYS);
    assert_eq!(next(&[]), deploy(Purpose::Rollback, 3, &[]));
    assert_eq!(next(&[Passed]), deploy(Purpose::Bisect, 5, &["L5"]));
    assert_eq!(
        next(&[Passed, Passed]),
        deploy(Purpose::Bisect, 6, &["L5", "L6"])
    );
    let v = done(next(&[Passed, Passed, Passed]));
    assert_eq!(v.guilty.as_deref(), Some("L7"));
    assert_eq!(v.passed, ["L5", "L6"]);
    assert_eq!(ids(&v.failed), ["L7"]);
    assert_eq!(v.on, Some(good(6)));
    assert_eq!(v.redeploys, 3);

    // A commit (L5's: L5 and everything before it) fails: L5 is the first failing one. The
    // Worker goes back to c3 at once; L6 and L7 hold L5's change, so they fail with it.
    assert_eq!(next(&[Passed, Failed]), deploy(Purpose::Restore, 3, &[]));
    let v = done(next(&[Passed, Failed, Passed]));
    assert_eq!(v.guilty.as_deref(), Some("L5"));
    assert!(v.passed.is_empty());
    assert_eq!(ids(&v.failed), ["L5", "L6", "L7"]);
    assert!(v.failed[1].1.contains("holds landing L5"));
    assert_eq!(v.on, Some(good(3)));
    // L6 fails after L5 passed: back to L5's commit, the newest good build.
    assert_eq!(
        next(&[Passed, Passed, Failed]),
        deploy(Purpose::Restore, 5, &["L5"])
    );
    let v = done(next(&[Passed, Passed, Failed, Passed]));
    assert_eq!(v.guilty.as_deref(), Some("L6"));
    assert_eq!(v.passed, ["L5"]);
    assert_eq!(ids(&v.failed), ["L6", "L7"]);
    assert_eq!((v.on, v.redeploys), (Some(good(5)), REDEPLOYS));

    // At most four redeploys: with five landings carried, two pass and the budget no longer
    // holds a step and its restore. The Worker stays on the newest good build; the rest could
    // not be isolated.
    let five = inc(&[5, 6, 7, 8, 9]);
    let v = done(lanes::bisect(
        "sites",
        Some(&g3),
        &five,
        &[Passed, Passed, Passed],
        REDEPLOYS,
    ));
    assert_eq!(v.guilty, None);
    assert_eq!(v.passed, ["L5", "L6"]);
    assert_eq!(ids(&v.failed), ["L7", "L8", "L9"]);
    assert!(v.failed[0].1.starts_with("could not isolate"));
    assert_eq!(v.on, Some(good(6)));
    assert!(v.summary.contains("could not isolate"));

    // One landing carried: the rollback alone attributes it.
    let one = inc(&[5]);
    assert_eq!(
        lanes::bisect("sites", Some(&g3), &one, &[], REDEPLOYS),
        deploy(Purpose::Rollback, 3, &[])
    );
    let v = done(lanes::bisect(
        "sites",
        Some(&g3),
        &one,
        &[Passed],
        REDEPLOYS,
    ));
    assert_eq!((v.guilty.as_deref(), v.redeploys), (Some("L5"), 1));

    // The good build will not deploy again: tried within the budget, then the person is told
    // the Worker may still be on a failing build.
    let v = done(next(&[Failed, Failed, Failed, Failed]));
    assert_eq!((v.guilty.clone(), v.on.clone()), (None, None));
    assert_eq!(ids(&v.failed), ["L5", "L6", "L7"]);
    assert!(v.summary.contains("a person must look"));
    // A redeploy the gateway could not run ends it unisolated, the Worker's build unknown.
    let v = done(next(&[Passed, Error]));
    assert_eq!((v.guilty.clone(), v.on.clone()), (None, None));

    // No earlier good build: bisect from the failing one; the cause is found but the Worker
    // cannot be taken back.
    let two = inc(&[5, 6]);
    assert_eq!(
        lanes::bisect("sites", None, &two, &[], REDEPLOYS),
        deploy(Purpose::Bisect, 5, &["L5"])
    );
    let v = done(lanes::bisect("sites", None, &two, &[Failed], REDEPLOYS));
    assert_eq!((v.guilty.as_deref(), v.on.clone()), (Some("L5"), None));
    let v = done(lanes::bisect("sites", None, &two, &[Passed], REDEPLOYS));
    assert_eq!((v.guilty.as_deref(), v.on), (Some("L6"), Some(good(5))));

    // An incident's failed deploys fail no landing by themselves; its successes cover.
    let l5 = Merged {
        id: "L5".into(),
        merged_at: 5,
        commit: "c5".into(),
        workers: vec!["sites".into()],
        ordered: true,
    };
    let d = |covers: i64, state: DeployState, incident: bool| Deploy {
        worker: "sites".into(),
        covers,
        state,
        incident,
    };
    assert_eq!(
        lanes::outcome(&l5, &[d(7, DeployState::Failed, true)]),
        None
    );
    assert_eq!(
        lanes::outcome(
            &l5,
            &[
                d(7, DeployState::Failed, true),
                d(5, DeployState::Succeeded, true)
            ]
        ),
        Some(pigeon_capabilities::land::state::Seen::JobEnded { passed: true })
    );
    // A lane held by an open incident starts nothing.
    assert!(lanes::starts("core", std::slice::from_ref(&l5), &[], &["sites"]).is_empty());

    // Core runs the Lander: never bisected (each step would restart it mid-incident), and
    // rolled back only to a build that holds this Lander, here one covering merge 3 or later.
    use pigeon_capabilities::land::lanes::CORE_NOT_BISECTED;
    let core = |good: Option<&Good>, since: Option<i64>, tried: &[Tried]| {
        lanes::core_incident(good, since, "c7", &three, tried)
    };
    assert_eq!(
        core(Some(&g3), Some(3), &[]),
        deploy(Purpose::Rollback, 3, &[])
    );
    let v = done(core(Some(&g3), Some(3), &[Passed]));
    assert_eq!(
        (v.guilty.clone(), v.on.clone(), v.redeploys),
        (None, Some(good(3)), 1)
    );
    assert_eq!(ids(&v.failed), ["L5", "L6", "L7"]);
    assert!(v.failed.iter().all(|(_, why)| why == CORE_NOT_BISECTED));
    // Its last good build predates this Lander, or none is known to hold it: stuck, by hand.
    for since in [Some(4), None] {
        let v = done(core(Some(&g3), since, &[]));
        assert_eq!((v.on.clone(), v.redeploys), (None, 0));
        assert!(v.summary.contains("the core deploy at commit c7 failed"));
        assert!(v.summary.contains("left as the job left it"));
        assert!(v.summary.contains("docs/proxies.md, break-glass"));
        assert_eq!(ids(&v.failed), ["L5", "L6", "L7"]);
    }
    assert!(matches!(core(None, Some(3), &[]), Next::Done(_)));
    // A rollback that does not pass is not tried again: stuck.
    let v = done(core(Some(&g3), Some(3), &[Failed]));
    assert_eq!((v.on.clone(), v.redeploys), (None, 1));
    assert!(v.summary.contains("recovery is by hand"));

    // A later green deploy that covers as much closes a stuck or unisolated incident.
    assert!(lanes::incident_covered("stuck", 5, 3, 9, 3));
    assert!(lanes::incident_covered("unisolated", 5, 3, 9, 4));
    // Not an incident in another state, an earlier deploy, or one that covers less.
    for state in ["open", "isolated", "covered"] {
        assert!(!lanes::incident_covered(state, 5, 3, 9, 4));
    }
    assert!(!lanes::incident_covered("stuck", 5, 3, 4, 4));
    assert!(!lanes::incident_covered("stuck", 5, 3, 9, 2));

    // What a deploy included, as the note and land_status name it.
    assert_eq!(
        lanes::deploy_note("sites", Purpose::Lane, "j1", "c7", &inc(&[5, 7])),
        "sites:lane:j1@c7[L5@c5+L7@c7]"
    );
    for p in [
        Purpose::Lane,
        Purpose::Rollback,
        Purpose::Bisect,
        Purpose::Restore,
    ] {
        assert_eq!(Purpose::named(p.name()), Some(p));
    }
}

// ---- deploy proxy ----

#[test]
fn land_rebases_disjoint_branches_and_lanes() {
    use pigeon_core::lanes::{self, Owner};
    use pigeon_core::{merge3, pack, rebase};
    use std::collections::BTreeMap;
    use std::io::Write;

    /// Writes (or deletes, on `None`) `files` and commits them; the commit's id.
    fn change(dir: &Path, files: &[(&str, Option<&str>)], message: &str) -> String {
        for (path, body) in files {
            let at = dir.join(path);
            match body {
                Some(b) => {
                    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                    std::fs::write(&at, b).unwrap();
                }
                None => std::fs::remove_file(&at).unwrap(),
            }
        }
        git(dir, &["add", "-A"], None);
        git(dir, &["commit", "-q", "-m", message], None);
        String::from_utf8(git(dir, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_string()
    }
    let lines = |edit: Option<(usize, &str)>| -> String {
        (1..=12)
            .map(|i| match edit {
                Some((at, text)) if at == i => format!("{text}\n"),
                _ => format!("line {i}\n"),
            })
            .collect()
    };

    // A base; a branch from it; main moving on from it in other paths, and in the smoke suite
    // at another line.
    let dir = repo("rebase");
    let smoke = lines(None);
    let base = change(
        &dir,
        &[
            ("a.txt", Some("a\n")),
            ("dir/b.txt", Some("b\n")),
            ("docs/x.md", Some("x\n")),
            ("tests/smoke.rs", Some(&smoke)),
        ],
        "base",
    );
    git(&dir, &["checkout", "-q", "-b", "claude/x"], None);
    let ours_smoke = lines(Some((2, "branch scenario")));
    let tip = change(
        &dir,
        &[
            ("dir/b.txt", Some("b on the branch\n")),
            ("new/c.txt", Some("c\n")),
            ("tests/smoke.rs", Some(&ours_smoke)),
        ],
        "the branch",
    );
    git(&dir, &["checkout", "-q", "main"], None);
    let theirs_smoke = lines(Some((10, "main scenario")));
    let main = change(
        &dir,
        &[
            ("a.txt", Some("a on main\n")),
            ("docs/x.md", None),
            ("tests/smoke.rs", Some(&theirs_smoke)),
        ],
        "main moved",
    );

    // What Artifacts would send: both tips and their history.
    let packed = git(
        &dir,
        &["pack-objects", "--stdout", "--revs", "-q"],
        Some(&format!("{tip}\n{main}\n")),
    );
    let objects = pack::objects(&packed).unwrap();
    let parents = rebase::parents(&objects);
    assert_eq!(
        rebase::merge_base(&parents, &tip, &main),
        Some(base.clone())
    );
    assert_eq!(
        rebase::merge_base(&parents, &main, &main),
        Some(main.clone())
    );
    let changed = rebase::changes(&objects, &base, &tip).unwrap();
    let moved = rebase::changes(&objects, &base, &main).unwrap();
    assert_eq!(
        changed.keys().collect::<Vec<_>>(),
        ["dir/b.txt", "new/c.txt", "tests/smoke.rs"]
    );
    assert_eq!(
        moved.keys().collect::<Vec<_>>(),
        ["a.txt", "docs/x.md", "tests/smoke.rs"]
    );
    assert_eq!(moved["docs/x.md"], None);

    // Both changed the smoke suite, at different lines: merged line by line. The same line
    // changed two ways, or two different insertions at one place, never merge.
    let joined = merge3::merge(&smoke, &ours_smoke, &theirs_smoke).unwrap();
    assert!(joined.contains("branch scenario\n") && joined.contains("main scenario\n"));
    assert_eq!(
        merge3::merge(&smoke, &lines(Some((5, "x"))), &lines(Some((5, "y")))),
        None
    );
    assert_eq!(
        merge3::merge("a\nb\n", "a\nX\nb\n", "a\nY\nb\n"),
        None,
        "two insertions at one place"
    );
    assert_eq!(
        merge3::merge("a\n", "a\nb\n", "a\nb\n").as_deref(),
        Some("a\nb\n")
    );

    // The branch laid onto main: the same tree git's own cherry-pick makes.
    let blob = pigeon_core::gitobj::Object::blob(joined.as_bytes());
    let mut changes: BTreeMap<String, rebase::Change> = changed.clone();
    changes.insert("tests/smoke.rs".into(), Some(("100644".into(), blob.id())));
    let (tree, trees) = rebase::apply(&objects, &main, &changes).unwrap();
    let original = &objects.by_id[&tip].1;
    let made = rebase::commit(
        original,
        &tree,
        &main,
        pigeon_core::attribution::LANDER,
        1_800_000_000,
    )
    .unwrap();
    let id = made.id();
    let mut new = vec![blob];
    new.extend(trees);
    new.push(made);
    let mut child = Command::new("git")
        .args(["unpack-objects", "-q"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&pigeon_core::gitobj::pack(&new))
        .unwrap();
    assert!(child.wait().unwrap().success());
    let read = |args: &[&str]| String::from_utf8(git(&dir, args, None)).unwrap();
    assert_eq!(read(&["rev-parse", &format!("{id}^")]).trim(), main);
    assert!(read(&["cat-file", "-p", &id]).contains("\nauthor smoke <smoke@example.com>"));
    git(&dir, &["fsck", "--no-progress", "--no-dangling"], None);
    git(&dir, &["checkout", "-q", "-b", "picked", &main], None);
    git(&dir, &["cherry-pick", "--allow-empty", &tip], None);
    assert_eq!(
        read(&["rev-parse", "HEAD^{tree}"]).trim(),
        tree,
        "the Lander's rebase and git's cherry-pick make the same tree"
    );
    // Nothing to apply leaves main's tree as it is; a path under a file is refused.
    assert_eq!(
        rebase::apply(&objects, &main, &BTreeMap::new()).unwrap().0,
        {
            read(&["rev-parse", &format!("{main}^{{tree}}")])
                .trim()
                .to_string()
        }
    );
    let mut under_file = BTreeMap::new();
    under_file.insert(
        "a.txt/x".to_string(),
        Some(("100644".into(), blob_id(&dir))),
    );
    assert!(rebase::apply(&objects, &main, &under_file).is_err());

    // The lanes: the repository's map, read as the Lander reads it from main.
    let map = lanes::parse(include_str!("../ci/workers.toml")).unwrap();
    assert!(map.classify(&["docs/plans/workers-split.md"]).docs_only);
    let core = map.classify(&["src/capabilities/src/land.rs", "tests/smoke.rs"]);
    assert!(!core.docs_only && !core.shared && core.smoke);
    assert_eq!(core.workers.iter().collect::<Vec<_>>(), ["core"]);
    assert!(map.classify(&["src/core/src/git.rs"]).shared);
    assert!(matches!(map.owner("README.md"), Owner::Shared));
    // Main moving in docs never retests; in a path of the same Worker, or shared, it does.
    let branch = ["src/capabilities/src/land.rs"];
    assert!(!map.overlap(&branch, &["docs/proxies.md"]).retest);
    assert!(map.overlap(&branch, &["src/platform/src/lib.rs"]).retest);
    assert!(map.overlap(&branch, &["Cargo.lock"]).retest);
    assert!(!map.overlap(&["docs/a.md"], &["src/core/src/git.rs"]).retest);
    assert_eq!(map.deploy_order(&core).len(), 1);
    assert_eq!(
        map.scripts(),
        [
            "pigeon-platform",
            "pigeon-sites",
            "pigeon-model-gateway",
            "pigeon-ci-gateway",
            "pigeon-proxy",
            "pigeon-ai-agents"
        ]
    );
    assert!(map.worker("sites").unwrap().queues.is_empty());
    // A Worker's deploy scope: its queues and container classes, from the map.
    let ci = map.worker("ci-gateway").unwrap();
    assert_eq!(ci.queues, ["pigeon-ci-events"]);
    assert!(ci.containers.is_empty());
    let scoped = lanes::parse(&format!(
        "{}\n[[worker]]\nname = \"scoped\"\nscript = \"pigeon-scoped\"\ndir = \"src/scoped\"\npaths = [\"src/scoped/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy-scoped\"\nqueues = [\"pigeon-ci-events\"]\ncontainers = [\"CiRunner\"]\n",
        include_str!("../ci/workers.toml")
    ))
    .unwrap();
    let scoped = scoped.worker("scoped").unwrap();
    assert_eq!(scoped.queues, ["pigeon-ci-events"]);
    assert_eq!(scoped.containers, ["CiRunner"]);
    // What a capability Worker may ask core to mint: a fixed table of purposes, nothing else.
    use pigeon_capabilities::split::purpose_permissions;
    assert!(
        purpose_permissions("ai_gateway")
            .unwrap()
            .contains(&"AI Gateway Run")
    );
    assert_eq!(
        purpose_permissions("artifacts"),
        Some(&["Artifacts Read", "Artifacts Write"][..])
    );
    assert_eq!(purpose_permissions("Workers Scripts Write"), None);
    // Each platform Worker deploys alone for its own paths; a shared change deploys core first.
    let names = |a: &lanes::Affected| -> Vec<String> {
        map.deploy_order(a).iter().map(|w| w.name.clone()).collect()
    };
    let sites = map.classify(&["src/sites/src/lib.rs"]);
    assert_eq!(names(&sites), ["sites"]);
    // The blog is built into core (pigeon-capabilities links it) as well as sites.
    assert_eq!(
        names(&map.classify(&["src/blog/src/pages.rs"])),
        ["core", "sites"]
    );
    assert_eq!(map.worker("sites").unwrap().deploy_job, "deploy-sites");
    assert_eq!(
        names(&map.classify(&["src/contract/src/wire.rs"])),
        [
            "core",
            "sites",
            "model-gateway",
            "ci-gateway",
            "proxy",
            "ai-agents"
        ]
    );
    // CI's code deploys the two Workers built with it, core and the gateway, and no other; the
    // gateway alone deploys for its own crate.
    let ci_code = map.classify(&["src/ci/src/lib.rs"]);
    assert!(!ci_code.shared && !ci_code.ordered);
    assert_eq!(names(&ci_code), ["core", "ci-gateway"]);
    // The gate and the image sources deploy nothing; the policy, compiled into pigeon-core,
    // deploys everything; the contract and the map go core first.
    assert!(
        map.classify(&["ci/runner/test.sh", "ci/sandbox/Dockerfile"])
            .workers
            .is_empty()
    );
    assert!(map.classify(&[".pigeon/policy.toml"]).shared);
    assert!(map.classify(&["src/contract/src/wire.rs"]).ordered);
    assert!(!map.classify(&["src/core/src/git.rs"]).ordered);
    assert_eq!(
        names(&map.classify(&["ci/workers.toml"])),
        ["core", "proxy"]
    );
    assert_eq!(
        names(&map.classify(&["src/ci-gateway/src/lib.rs"])),
        ["ci-gateway"]
    );
    assert_eq!(
        map.worker("ci-gateway").unwrap().deploy_job,
        "deploy-ci-gateway"
    );
    // The proxies' code deploys core and the proxy Worker, which both link it; the Worker's own
    // crate deploys it alone.
    assert_eq!(
        names(&map.classify(&["src/proxies/src/git_proxy.rs"])),
        ["core", "proxy"]
    );
    assert_eq!(names(&map.classify(&["src/proxy/src/lib.rs"])), ["proxy"]);
    assert_eq!(map.worker("proxy").unwrap().deploy_job, "deploy-proxy");
    assert!(
        !map.overlap(&["src/sites/src/lib.rs"], &["src/capabilities/src/land.rs"])
            .retest
    );
    assert!(
        map.overlap(&["src/sites/src/lib.rs"], &["src/blog/src/lib.rs"])
            .retest
    );
    // The deploy proxy finds the Worker a call is for in its path.
    use pigeon_capabilities::cf_proxy::target_script;
    assert_eq!(
        target_script("/cf/client/v4/accounts/a/workers/scripts/pigeon-sites/versions"),
        Some("pigeon-sites")
    );
    assert_eq!(
        target_script("/cf/client/v4/accounts/a/workers/services/pigeon-sites"),
        Some("pigeon-sites")
    );
    assert_eq!(target_script("/cf/client/v4/accounts/a/queues"), None);
    // A map that is not right is refused whole; main without one falls back to core alone.
    for bad in [
        "version = 2\n[docs]\npaths=[]\n[shared]\npaths=[]",
        "version = 1\n[docs]\npaths=[\"/x\"]\n[shared]\npaths=[]\n[[worker]]\nname=\"core\"\nscript=\"s\"\ndir=\"d\"\npaths=[]\ntest_job=\"t\"\ndeploy_job=\"d\"",
        "version = 1\n[docs]\npaths=[]\n[shared]\npaths=[]\n[[worker]]\nname=\"sites\"\nscript=\"s\"\ndir=\"d\"\npaths=[]\ntest_job=\"t\"\ndeploy_job=\"d\"",
        "version = 1\nextra = 1\n[docs]\npaths=[]\n[shared]\npaths=[]",
    ] {
        assert!(lanes::parse(bad).is_err(), "{bad}");
    }
    let fallback = pigeon_capabilities::land::merge::fallback();
    assert!(fallback.classify(&["docs/a.md"]).docs_only);
    assert_eq!(
        fallback
            .classify(&["anything/else.rs"])
            .workers
            .iter()
            .collect::<Vec<_>>(),
        ["core"]
    );
}

/// The id of a blob `git hash-object` stores in `dir`.
fn blob_id(dir: &Path) -> String {
    String::from_utf8(git(dir, &["hash-object", "-w", "--stdin"], Some("x\n")))
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn land_commit_attribution() {
    use pigeon_core::gitobj::{self, Object};
    use pigeon_core::{attribution, pack, rebase};
    use std::io::Write;

    let dir = repo("attribution");
    let read = |args: &[&str]| String::from_utf8(git(&dir, args, None)).unwrap();
    let head = || read(&["rev-parse", "HEAD"]).trim().to_string();
    let unpack = |objects: &[Object]| {
        let mut child = Command::new("git")
            .args(["unpack-objects", "-q"])
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&gitobj::pack(objects))
            .unwrap();
        assert!(child.wait().unwrap().success());
    };
    let line = |id: &str, key: &str| -> String {
        read(&["cat-file", "-p", id])
            .lines()
            .find(|l| l.starts_with(&format!("{key} ")))
            .unwrap_or_default()
            .to_string()
    };

    // A base; a branch commit by someone else at a fixed time and zone; main moving on.
    let base = commit(&dir, "base");
    git(&dir, &["checkout", "-q", "-b", "claude/x"], None);
    std::fs::write(dir.join("g"), "branch\n").unwrap();
    git(&dir, &["add", "g"], None);
    let made = Command::new("git")
        .args([
            "commit",
            "-q",
            "--author",
            "Someone Else <someone@example.com>",
            "-m",
            "the branch",
        ])
        .env("GIT_AUTHOR_DATE", "1700000000 +0200")
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(made.success());
    let tip = head();
    git(&dir, &["checkout", "-q", "main"], None);
    let main = commit(&dir, "main moved");

    // The Lander's rebase keeps the author line byte for byte and only sets the committer.
    let packed = git(
        &dir,
        &["pack-objects", "--stdout", "--revs", "-q"],
        Some(&format!("{tip}\n{main}\n")),
    );
    let objects = pack::objects(&packed).unwrap();
    let changes = rebase::changes(&objects, &base, &tip).unwrap();
    let (tree, trees) = rebase::apply(&objects, &main, &changes).unwrap();
    let rebased = rebase::commit(
        &objects.by_id[&tip].1,
        &tree,
        &main,
        attribution::LANDER,
        1_800_000_000,
    )
    .unwrap();
    let id = rebased.id();
    let mut new = trees;
    new.push(rebased);
    unpack(&new);
    assert_eq!(
        line(&id, "author"),
        "author Someone Else <someone@example.com> 1700000000 +0200"
    );
    assert_eq!(line(&id, "author"), line(&tip, "author"));
    assert_eq!(
        line(&id, "committer"),
        "committer pigeon <land@pigeon.dev> 1800000000 +0000"
    );
    assert!(!read(&["cat-file", "-p", &id]).contains("gpgsig"));
    git(&dir, &["fsck", "--no-progress", "--no-dangling"], None);

    // Notes commits pigeon writes name the person as author and pigeon as committer.
    let who = |id: &str| read(&["log", "-1", "--format=%an <%ae>|%cn <%ce>", id]);
    let note = vec![(main.clone(), "landing L1 state=landed\n".to_string())];
    let (objects, land_id) =
        pigeon_capabilities::land::notes_commit(&note, None, 1_795_000_000).unwrap();
    unpack(&objects);
    assert_eq!(
        who(&land_id).trim(),
        "noisypigeon <167243387+noisypigeon@users.noreply.github.com>|pigeon <land@pigeon.dev>"
    );
    let (objects, ci_id) = pigeon_ci::notes::commit(&note, None, 1_795_000_000).unwrap();
    unpack(&objects);
    assert_eq!(
        who(&ci_id).trim(),
        "noisypigeon <167243387+noisypigeon@users.noreply.github.com>|pigeon <ci@pigeon.dev>"
    );
    git(&dir, &["fsck", "--no-progress", "--no-dangling"], None);

    // A forged header line is refused; AI agent runs set the same author in their checkout.
    assert!(
        Object::commit(
            &tree,
            &[],
            "x <x@example.com>\nparent 0",
            attribution::LANDER,
            0,
            "m"
        )
        .is_err()
    );
    assert!(Object::commit(&tree, &[], &attribution::author(), "", 0, "m").is_err());
    assert_eq!(
        pigeon_ai_agents::sandbox::author_config_cmd(),
        "git config author.name 'noisypigeon' && git config author.email \
         '167243387+noisypigeon@users.noreply.github.com'"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

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
            "section",
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

// ---- observability ----

/// Every Worker in the map keeps Cloudflare Observability on with the plan's settings
/// (docs/plans/observability.md): logs on, invocation logs off, traces set, container logs off.
#[test]
fn core_observability_on_every_worker() {
    use pigeon_core::lanes;
    // Workers whose traces are off, and why. Every other Worker traces.
    let traces_off = [(
        "core",
        "serves the enrolment and OAuth URLs, whose codes sit in the query string",
    )];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let map = lanes::parse(include_str!("../ci/workers.toml")).unwrap();
    assert!(!map.workers.is_empty());
    for w in &map.workers {
        let path = root.join(&w.dir).join("wrangler.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let t: toml::Table = toml::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let o = t
            .get("observability")
            .unwrap_or_else(|| panic!("{}: no [observability]", w.name));
        assert_eq!(
            o.get("enabled").and_then(|v| v.as_bool()),
            Some(true),
            "{}",
            w.name
        );
        let rate = o
            .get("head_sampling_rate")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
        assert!(
            rate.is_some_and(|r| r > 0.0 && r <= 1.0),
            "{}: head_sampling_rate {rate:?} is not in (0, 1]",
            w.name
        );
        assert_eq!(
            o.get("logs")
                .and_then(|l| l.get("invocation_logs"))
                .and_then(|v| v.as_bool()),
            Some(false),
            "{}: invocation logs keep whole URLs",
            w.name
        );
        let traces = o
            .get("traces")
            .and_then(|tr| tr.get("enabled"))
            .and_then(|v| v.as_bool());
        let off = traces_off.iter().find(|(n, _)| *n == w.name);
        assert_eq!(traces, Some(off.is_none()), "{}: traces {:?}", w.name, off);
        for c in t
            .get("containers")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
        {
            assert_eq!(
                c.get("observability")
                    .and_then(|o| o.get("enabled"))
                    .and_then(|v| v.as_bool()),
                Some(false),
                "{}: container logs must be off",
                w.name
            );
        }
    }
}

// ---- CI ----

#[test]
fn ci_route_and_run_on_a_fake_connector() {
    use pigeon_ci::Dispatched;
    use pigeon_ci::class::{Class, Offered, Size};
    use pigeon_ci::config::{self, Arch, Os};
    use pigeon_ci::route::{Ask, Availability, Candidate, choose, fallback_due, route};
    use pigeon_ci::scheduler::{self, Outcome};
    use pigeon_ci::state::{self, Next, Report, State};
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

    // The gateway's manifest offers what core serves: the same tools, /ci/, the Buildkite
    // token's slot and the events queue; it acts with scoped credentials, core with the parent.
    use pigeon_contract::Service;
    let core_ci = pigeon_ci::Ci {
        settings: pigeon_ci::pigeon::settings(pigeon_ci::Accounts::Parent("P".into())),
    };
    let gateway = pigeon_ci::CiGateway::new(
        pigeon_ci::pigeon::settings(pigeon_ci::Accounts::Scoped),
        b"key".to_vec(),
    );
    let manifest = gateway.manifest();
    assert_eq!(manifest.worker, "pigeon-ci-gateway");
    let [cap] = &manifest.capabilities[..] else {
        panic!("one capability")
    };
    assert_eq!(cap.name, "ci");
    let names: Vec<&str> = cap.tools.iter().map(|t| t.name.as_str()).collect();
    let local: Vec<&str> = core_ci.tools().iter().map(|t| t.name).collect();
    assert_eq!(names, local);
    assert!(names.contains(&"ci_submit") && names.contains(&"ci_buildkite_plan"));
    assert_eq!(cap.routes, ["/ci/"]);
    assert_eq!(cap.footprint["queues"], json!(["pigeon-ci-events"]));
    let external_names: Vec<&str> = cap.footprint["externals"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(
        external_names,
        ["Cloudflare Containers", "Buildkite", "Buildkite MCP"],
        "the connectors' services, in the operator's order"
    );
    assert_eq!(cap.secrets.len(), 1);
    assert_eq!(cap.secrets[0].name, "BUILDKITE_API_TOKEN");
    assert_eq!(cap.secrets[0].scopes, ["workers"]);
}

// ---- models ----

#[test]
fn models_request() {
    use pigeon_models::jobs::{self, State};
    use pigeon_models::stream::Reading;
    use pigeon_models::{Dialect, Provider, ask};
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
    assert_eq!(jobs::to_start("default", jobs::MAX_RUNNING, 3), 0);
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

/// An AI agent's turn as a model job (docs/plans/ai-agents.md): the whole request checked, its stream
/// read into content blocks and cache usage, its lane and its cancel.
#[test]
fn models_turns_blocks_and_lanes() {
    use pigeon_models::Dialect;
    use pigeon_models::jobs::{self, State};
    use pigeon_models::stream::Reading;
    use pigeon_models::turn::{self, MAX_TOKENS};

    // The request: checked, given its model and bounds, always streamed.
    let req = json!({
        "system": [{ "type": "text", "text": "rules", "cache_control": { "type": "ephemeral" } }],
        "tools": [{ "name": "read_file", "input_schema": { "type": "object" } }],
        "messages": [{ "role": "user", "content": "go" }],
        "thinking": { "type": "adaptive" }
    });
    let t = turn::turn(
        &req,
        &json!(["compact-2026-01-12", "compact-2026-01-12"]),
        "claude-haiku-5-5",
    )
    .unwrap();
    assert_eq!(t.model, "claude-haiku-5-5");
    assert_eq!(t.body["stream"], true);
    assert_eq!(t.body["max_tokens"], turn::DEFAULT_MAX_TOKENS);
    assert_eq!(t.beta.as_deref(), Some("compact-2026-01-12"));
    let refused = |r: Value, betas: Value| turn::turn(&r, &betas, "claude-haiku-5-5").unwrap_err();
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "stream": false }),
            Value::Null
        )
        .contains("stream")
    );
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "service_tier": "x" }),
            Value::Null
        )
        .contains("service_tier")
    );
    assert!(refused(json!({ "messages": [] }), Value::Null).contains("empty"));
    assert!(
        refused(
            json!({ "messages": [{ "role": "system", "content": "x" }] }),
            Value::Null
        )
        .contains("messages[0]")
    );
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }], "model": "gpt-9" }),
            Value::Null
        )
        .contains("Claude")
    );
    assert!(refused(json!({ "messages": [{ "role": "user", "content": "x" }], "max_tokens": MAX_TOKENS + 1 }), Value::Null).contains("max_tokens"));
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": "x" }] }),
            json!(["files-api"])
        )
        .contains("files-api")
    );
    let big = "x".repeat(turn::MAX_BYTES);
    assert!(
        refused(
            json!({ "messages": [{ "role": "user", "content": big }] }),
            Value::Null
        )
        .contains("4 MiB")
    );

    // The stream: thinking with its signature, text, a tool call's input in pieces, cache usage.
    let stream = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1,\"cache_read_input_tokens\":1201,\"cache_creation_input_tokens\":231}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Reading.\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"src/\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"lib.rs\\\"}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":3,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"list\",\"input\":{}}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":3}\n\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":40}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    for chunk in [1, 13, 4096] {
        let mut r = Reading::default();
        for piece in stream.as_bytes().chunks(chunk) {
            r.feed(Dialect::Anthropic, piece);
        }
        r.end(Dialect::Anthropic);
        assert!(
            r.done && r.error.is_none(),
            "chunks of {chunk}: {:?}",
            r.error
        );
        assert_eq!(r.text, "Reading.");
        let c = r.content();
        assert_eq!(
            c[0],
            json!({ "type": "thinking", "thinking": "plan", "signature": "sig" })
        );
        assert_eq!(c[1], json!({ "type": "text", "text": "Reading." }));
        assert_eq!(c[2]["input"], json!({ "path": "src/lib.rs" }));
        assert_eq!(c[3]["input"], json!({}));
        assert_eq!(r.stop_reason.as_deref(), Some("tool_use"));
        let u = r.usage();
        assert_eq!(
            (
                u["cache_read_input_tokens"].as_u64(),
                u["cache_creation_input_tokens"].as_u64()
            ),
            (Some(1201), Some(231))
        );
        assert_eq!(u["output_tokens"], 40);
    }
    // A tool input that is not JSON is an error; an absurd block index too.
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b"data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"x\",\"input\":{}}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{oops\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
    assert!(r.error.as_deref().is_some_and(|e| e.contains("not JSON")));
    let mut r = Reading::default();
    r.feed(Dialect::Anthropic, b"data: {\"type\":\"content_block_start\",\"index\":99999,\"content_block\":{\"type\":\"text\"}}\n\n");
    assert!(r.error.is_some() && r.blocks.is_empty());

    // Lanes: each with its own cap; an unknown one refused.
    assert_eq!(jobs::lane(&Value::Null), Ok("default"));
    assert_eq!(jobs::lane(&json!("ai-agents")), Ok("ai-agents"));
    assert!(jobs::lane(&json!("fast")).is_err());
    assert_eq!(jobs::to_start("ai-agents", jobs::MAX_RUNNING, 10), 10);
    assert_eq!(jobs::to_start("ai-agents", 10, 10), 6);
    assert_eq!(jobs::to_start("default", jobs::MAX_RUNNING, 10), 0);
    assert_eq!(jobs::to_start("nope", 0, 10), 0);
    // Cancel: from queued or running, never from an end.
    assert!(
        jobs::allowed(State::Queued, State::Cancelled)
            && jobs::allowed(State::Running, State::Cancelled)
    );
    assert!(!jobs::allowed(State::Cancelled, State::Running));
    assert!(
        jobs::may_cancel(State::Running).is_ok() && jobs::may_cancel(State::Succeeded).is_err()
    );
    assert_eq!(State::named("cancelled"), Some(State::Cancelled));
    // Retries before the first byte: overload and rate limits, a few times; never a bad request.
    assert!(jobs::retryable(529) && jobs::retryable(429) && !jobs::retryable(400));
    assert_eq!(
        (jobs::retry_wait_ms(1), jobs::retry_wait_ms(4)),
        (Some(1_000), None)
    );
}

/// An AI agent run's logic (docs/plans/ai-agents.md, phase 2): its next action from what it knows,
/// recovery from a failed turn, budgets and prices, the tools each role is offered and their
/// checks, and the turn it sends.
#[test]
fn ai_agents_loop_budgets_tools_and_turns() {
    use pigeon_ai_agents::budget::{self, Budget, Next, Over, Spent};
    use pigeon_ai_agents::run::{self, Action, Call, Job, Recovery, State, View, Wait};
    use pigeon_ai_agents::tools::{self, Kind};
    use pigeon_ai_agents::transcript::{self, Spec};
    use pigeon_ai_agents::{Role, task};

    // ---- the loop ----
    let view = |state: State| View {
        state,
        stop_asked: false,
        paused: false,
        job: None,
        calls: &[],
        turns: 0,
        stop_reason: None,
        reported: None,
        nudges: 0,
        user_last: false,
        waiting: None,
    };
    assert_eq!(run::next(&view(State::Queued)), Action::Prepare);
    assert_eq!(
        run::next(&view(State::Running)),
        Action::Turn,
        "the first turn"
    );
    assert_eq!(
        run::next(&View {
            stop_asked: true,
            ..view(State::Waiting)
        }),
        Action::End(State::Stopped)
    );
    assert_eq!(
        run::next(&View {
            paused: true,
            ..view(State::Running)
        }),
        Action::Check {
            after_ms: run::CHECK_EVERY_MS
        }
    );
    assert_eq!(run::next(&view(State::Done)), Action::Idle);
    // A turn in flight is polled, often at first; read once it succeeds; retried once it ends.
    assert_eq!(
        run::next(&View {
            job: Some((Job::Pending, 0)),
            ..view(State::Running)
        }),
        Action::Poll { after_ms: 1_000 }
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Pending, 60_000)),
            ..view(State::Running)
        }),
        Action::Poll { after_ms: 5_000 }
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Succeeded, 9)),
            ..view(State::Running)
        }),
        Action::Read
    );
    assert_eq!(
        run::next(&View {
            job: Some((Job::Ended, 9)),
            ..view(State::Running)
        }),
        Action::Retry
    );
    // Tool calls run in order; one interrupted is repeated only when that is safe.
    let call = |name: &str, started: bool, done: bool| Call {
        id: format!("t-{name}"),
        name: name.into(),
        started,
        done,
    };
    let calls = [call("read_file", true, true), call("run", false, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Call { index: 1 }
    );
    let calls = [call("grep", true, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Call { index: 0 }
    );
    let calls = [call("commit_push", true, false)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Interrupted { index: 0 }
    );
    // Results in: the next turn. A report ends the run, once its calls are answered.
    let calls = [call("run", true, true)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 1,
            ..view(State::Running)
        }),
        Action::Turn
    );
    let calls = [call("report", true, true)];
    assert_eq!(
        run::next(&View {
            calls: &calls,
            turns: 3,
            reported: Some(State::Done),
            ..view(State::Running)
        }),
        Action::End(State::Done)
    );
    // Stopping without a tool or a report: nudged twice, then failed; cut off by max_tokens: go on.
    let quiet = View {
        turns: 2,
        stop_reason: Some("end_turn"),
        ..view(State::Running)
    };
    assert_eq!(run::next(&quiet), Action::Nudge);
    assert_eq!(
        run::next(&View {
            nudges: run::MAX_NUDGES,
            ..quiet.clone()
        }),
        Action::End(State::Failed)
    );
    assert_eq!(
        run::next(&View {
            stop_reason: Some("max_tokens"),
            ..quiet
        }),
        Action::Turn
    );
    // Waiting: approvals and answers are looked at often, executors less, the provider rarely.
    let approval = Wait::Approval {
        id: "a1".into(),
        call: "t1".into(),
    };
    assert_eq!(
        run::next(&View {
            waiting: Some(&approval),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::CHECK_EVERY_MS
        }
    );
    assert_eq!(
        run::next(&View {
            waiting: Some(&Wait::Executors { since: 0 }),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::EXECUTORS_EVERY_MS
        }
    );
    assert_eq!(
        run::next(&View {
            waiting: Some(&Wait::Provider { since: 0 }),
            ..view(State::Waiting)
        }),
        Action::Check {
            after_ms: run::PROVIDER_EVERY_MS
        }
    );
    // States move forward, and never out of an end.
    assert!(
        run::allowed(State::Running, State::Waiting)
            && run::allowed(State::Waiting, State::Running)
    );
    assert!(
        run::allowed(State::Queued, State::Stopped) && !run::allowed(State::Done, State::Running)
    );
    assert!(!run::allowed(State::Running, State::Queued));
    assert_eq!(State::named("over_budget"), Some(State::OverBudget));

    // ---- a failed turn ----
    let models = Role::Executor.models();
    assert_eq!(
        run::on_failure("failed", "anthropic answered 529: overloaded", 1, 0, models),
        Recovery::Again
    );
    assert_eq!(
        run::on_failure("failed", "overloaded_error: Overloaded", 2, 0, models),
        Recovery::Fallback { index: 1 }
    );
    assert_eq!(
        run::on_failure("lost", "", 2, 1, models),
        Recovery::WaitProvider
    );
    assert_eq!(
        run::on_failure(
            "failed",
            "anthropic answered 400: prompt is too long",
            1,
            0,
            models
        ),
        Recovery::Fail
    );
    assert_eq!(
        run::on_failure("cancelled", "cancelled by its sender", 1, 0, models),
        Recovery::Fail
    );
    // A deploy that changes the tool list makes the transcript's thinking signatures stale
    // (planner Zzn8ssZs4JsC, 2026-11-22): strip them once and go on, rather than end the run.
    let stale = "anthropic answered 400: {\"error\":{\"message\":\"messages.1.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. The `tools` list differs from the one this block was created with.\"}}";
    assert_eq!(
        run::on_failure("failed", stale, 1, 0, models),
        Recovery::StripThinking
    );
    assert_eq!(
        run::on_failure("failed", stale, 2, 0, models),
        Recovery::Fail
    );
    assert_eq!(task("r1", Role::Planner, 7), "ai-agent/r1/planner/7");

    // ---- budgets ----
    let b = Budget::asked(Role::Executor, &Value::Null).unwrap();
    // ai_agents_extend adds to a live run's limits, within the hard caps, and says what it added.
    let planner = Budget::default_for(Role::Planner);
    let (more, added) = budget::extend(
        Role::Planner,
        &planner,
        &json!({ "cost_usd": 10, "turns": 50, "wall_min": 60, "sandbox_min": 30, "executors": 4 }),
    )
    .unwrap();
    assert_eq!(more.cost_micros, planner.cost_micros + 10_000_000);
    assert_eq!(more.turns, planner.turns + 50);
    assert_eq!(more.wall_ms, planner.wall_ms + 60 * 60_000);
    assert_eq!(more.sandbox_ms, planner.sandbox_ms + 30 * 60_000);
    assert_eq!(more.executors, planner.executors + 4);
    assert_eq!(added["executors"], 4);
    let over = budget::extend(Role::Planner, &planner, &json!({ "cost_usd": 90 })).unwrap_err();
    assert!(
        over.contains("over the cap") && over.contains("left"),
        "{over}"
    );
    assert!(budget::extend(Role::Planner, &planner, &json!({ "turns": 301 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "wall_min": 961 })).is_err());
    assert!(budget::extend(Role::Executor, &b, &json!({ "executors": 1 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({})).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "turns": 0 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "cost_usd": -1 })).is_err());
    assert!(budget::extend(Role::Planner, &planner, &json!({ "budget": 1 })).is_err());
    assert_eq!((b.turns, b.cost_micros), (60, 5_000_000));
    assert_eq!(
        Budget::asked(Role::Planner, &Value::Null)
            .unwrap()
            .executors,
        12
    );
    let b = Budget::asked(Role::Executor, &json!({ "cost_usd": 2.5, "wall_min": 30 })).unwrap();
    assert_eq!((b.cost_micros, b.wall_ms), (2_500_000, 30 * 60_000));
    assert!(
        Budget::asked(Role::Executor, &json!({ "cost_usd": 101 }))
            .unwrap_err()
            .contains("cap")
    );
    assert!(
        Budget::asked(Role::Executor, &json!({ "executors": 1 }))
            .unwrap_err()
            .contains("planner")
    );
    assert!(
        Budget::asked(Role::Executor, &json!({ "tokens": 1 }))
            .unwrap_err()
            .contains("tokens")
    );
    assert!(Budget::asked(Role::Executor, &json!({ "turns": 0 })).is_err());
    let planner = Budget::default_for(Role::Planner);
    let left = Spent::left(
        &planner,
        &Spent {
            cost_micros: 24_000_000,
            ..Spent::default()
        },
        0,
        1_000,
    );
    assert!(
        Budget::default_for(Role::Executor)
            .within(&left)
            .unwrap_err()
            .contains("cost")
    );
    let b = Budget::default_for(Role::Executor);
    let spent = Spent {
        turns: 59,
        cost_micros: 4_900_000,
        sandbox_ms: 0,
        executors: 0,
    };
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Turn {
                estimate_micros: 50_000
            }
        ),
        Ok(())
    );
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Turn {
                estimate_micros: 200_000
            }
        ),
        Err(Over::Cost)
    );
    assert_eq!(
        budget::check(&b, &Spent { turns: 60, ..spent }, 0, 1, Next::Tool),
        Ok(())
    );
    assert_eq!(
        budget::check(
            &b,
            &Spent { turns: 60, ..spent },
            0,
            1,
            Next::Turn { estimate_micros: 0 }
        ),
        Err(Over::Turns)
    );
    assert_eq!(
        budget::check(&b, &spent, 0, b.wall_ms, Next::Tool),
        Err(Over::Wall)
    );
    assert_eq!(
        budget::check(
            &b,
            &spent,
            0,
            1,
            Next::Sandbox {
                timeout_ms: b.sandbox_ms + 1
            }
        ),
        Err(Over::Sandbox)
    );
    assert_eq!(
        budget::check(&b, &spent, 0, 1, Next::Spawn { cost_micros: 0 }),
        Err(Over::Executors)
    );
    assert_eq!(Over::Turns.why(&b), "the run used its 60 model turns");
    // Prices: cache reads cheap, writes dear, an unknown model priced as the dearest.
    let usage = json!({ "input_tokens": 1_000_000, "output_tokens": 0, "cache_read_input_tokens": 1_000_000, "cache_creation_input_tokens": 1_000_000 });
    assert_eq!(
        budget::cost_micros("claude-sonnet-5-5", &usage),
        2_000_000 + 100_000 + 2_500_000
    );
    let split = json!({ "cache_creation_input_tokens": 2_000_000, "cache_creation": { "ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 1_000_000 } });
    assert_eq!(
        budget::cost_micros("claude-opus-5-5", &split),
        5_000_000 + 8_000_000
    );
    assert_eq!(
        budget::cost_micros("claude-haiku-5-5", &json!({ "output_tokens": 1 })),
        1,
        "rounded up"
    );
    assert_eq!(
        budget::price("claude-sonnet-5-5-20261101"),
        budget::price("claude-sonnet-5-5")
    );
    assert_eq!(
        budget::price("some-model"),
        budget::price("claude-fable-5-2")
    );
    assert_eq!(
        budget::estimate_micros("claude-opus-5-5", 100_000, 32_000),
        400_000 + 640_000
    );
    assert_eq!(budget::money(2_500_000), "$2.5");

    // ---- tools ----
    assert_eq!(tools::kind(Role::Executor, "run"), Some(Kind::Sandbox));
    assert_eq!(tools::kind(Role::Executor, "ci_submit"), Some(Kind::Pigeon));
    assert_eq!(
        tools::kind(Role::Executor, "land"),
        None,
        "executors never land"
    );
    assert_eq!(tools::kind(Role::Planner, "land"), Some(Kind::Pigeon));
    assert_eq!(tools::kind(Role::Executor, "spawn_executor"), None);
    assert_eq!(
        tools::kind(Role::Planner, "spawn_executor"),
        Some(Kind::Agent)
    );
    assert_eq!(tools::kind(Role::Planner, "deploy_token"), None);
    let pigeon = vec![
        (
            "ci_submit".to_string(),
            "Run a CI job.".to_string(),
            json!({ "type": "object" }),
        ),
        (
            "land".to_string(),
            "Land a branch.".to_string(),
            json!({ "type": "object" }),
        ),
        (
            "source_token".to_string(),
            "A git token.".to_string(),
            json!({ "type": "object" }),
        ),
    ];
    let names = |role| -> Vec<String> {
        tools::definitions(role, &pigeon)
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    let exec = names(Role::Executor);
    assert!(
        exec.contains(&"ci_submit".into())
            && !exec.contains(&"land".into())
            && !exec.contains(&"source_token".into())
    );
    assert!(
        names(Role::Planner).contains(&"land".into())
            && names(Role::Planner).contains(&"executor_stop".into())
    );
    assert!(
        tools::definitions(Role::Executor, &[])
            .iter()
            .all(|t| t["input_schema"]["type"] == "object")
    );
    let refused =
        |name: &str, input: Value| tools::check(Role::Executor, name, &input).unwrap_err();
    assert!(
        refused(
            "write_file",
            json!({ "path": ".pigeon/policy.toml", "content": "" })
        )
        .contains("only the person")
    );
    assert!(
        refused(
            "edit_file",
            json!({ "path": "./.claude//settings.json", "old": "a", "new": "b" })
        )
        .contains("only the person")
    );
    assert!(refused("read_file", json!({ "path": "../etc/passwd" })).contains(".."));
    assert!(refused("read_file", json!({ "path": "/etc/passwd" })).contains("relative"));
    assert!(refused("list", json!({ "path": ".git/config" })).contains(".git"));
    assert!(
        refused(
            "edit_file",
            json!({ "path": "a.rs", "old": "x", "new": "x" })
        )
        .contains("same")
    );
    assert!(refused("run", json!({ "cmd": "cargo test", "timeout_s": 900 })).contains("840"));
    assert!(refused("report", json!({ "state": "maybe", "summary": "s" })).contains("done"));
    assert!(
        refused("land", json!({ "branch": "claude/x" })).contains("not one of this run's tools")
    );
    assert!(
        tools::check(
            Role::Executor,
            "run",
            &json!({ "cmd": "cargo test -p pigeon-ai-agents" })
        )
        .is_ok()
    );
    assert!(
        tools::check(
            Role::Executor,
            "edit_file",
            &json!({ "path": "src/a.rs", "old": "x", "new": "y" })
        )
        .is_ok()
    );
    assert!(
        tools::idempotent("grep") && !tools::idempotent("commit_push") && !tools::idempotent("run")
    );

    // ---- the turn ----
    let defs = tools::definitions(Role::Planner, &pigeon);
    let messages = [json!({ "role": "user", "content": "the brief" })];
    let spec = Spec {
        role: Role::Planner,
        model: "claude-opus-5-5",
        system: "rules",
        tools: &defs,
    };
    let req = transcript::request(&spec, &messages);
    assert_eq!(
        req["system"][0]["cache_control"]["ttl"], "1h",
        "a planner waits: its prefix is kept an hour"
    );
    assert_eq!(
        req["tools"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(req["tools"][0].get("cache_control"), None);
    assert_eq!(
        (
            req["thinking"]["type"].as_str(),
            req["output_config"]["effort"].as_str()
        ),
        (Some("adaptive"), Some("high"))
    );
    assert_eq!(req["cache_control"]["type"], "ephemeral");
    let spec = Spec {
        role: Role::Mechanical,
        model: "claude-haiku-5-5",
        system: "rules",
        tools: &[],
    };
    let req = transcript::request(&spec, &messages);
    assert!(req.get("thinking").is_none() && req.get("tools").is_none());
    assert_eq!(req["system"][0]["cache_control"].get("ttl"), None);
    // The request a turn sends passes the gateway's own checks (pigeon_models::turn).
    let spec = Spec {
        role: Role::Executor,
        model: "claude-sonnet-5-5",
        system: "rules",
        tools: &defs,
    };
    assert!(
        pigeon_models::turn::turn(
            &transcript::request(&spec, &messages),
            &Value::Null,
            "claude-haiku-5-5"
        )
        .is_ok()
    );
    // Tool calls out, results back; long output cut in the middle, at char boundaries.
    let content = json!([{ "type": "text", "text": "x" }, { "type": "tool_use", "id": "t1", "name": "run", "input": { "cmd": "ls" } }, { "type": "tool_use", "name": "nameless" }]);
    assert_eq!(
        transcript::tool_uses(&content),
        vec![("t1".to_string(), "run".to_string(), json!({ "cmd": "ls" }))]
    );
    let msg = transcript::results_message(&[
        ("t1".into(), "ok".into(), false),
        ("t2".into(), "no".into(), true),
    ]);
    assert_eq!(msg["content"][0].get("is_error"), None);
    assert_eq!(msg["content"][1]["is_error"], true);
    let long = "é".repeat(20_000);
    let cut = transcript::cut(&long, Some("/tmp/out/3"));
    assert!(
        cut.len() < transcript::MAX_RESULT_BYTES
            && cut.contains("bytes cut; the whole output is in `/tmp/out/3`")
    );
    assert_eq!(transcript::cut("short", None), "short");
    // Compaction: when the context is large; cut only where no tool result loses its call.
    assert!(transcript::needs_compaction(
        &json!({ "input_tokens": 10, "cache_read_input_tokens": 150_000 })
    ));
    assert!(!transcript::needs_compaction(
        &json!({ "input_tokens": 10 })
    ));
    let mut history = vec![json!({ "role": "user", "content": "brief" })];
    for i in 0..30 {
        history.push(json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": format!("t{i}"), "name": "run", "input": {} }] }));
        history.push(json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": format!("t{i}"), "content": "ok" }] }));
    }
    assert_eq!(
        transcript::cut_point(&history, 20),
        None,
        "no clean place to cut"
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "done?" }] }));
    history.push(
        json!({ "role": "user", "content": "Call `report` to end the run, or keep working." }),
    );
    history.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "ok" }] }));
    let at = transcript::cut_point(&history, 4).unwrap();
    assert_eq!(history[at]["role"], "user");
    let kept = transcript::compacted("did things", &history[at..]);
    assert_eq!(kept[0]["role"], "user");
    assert_eq!(kept[1]["role"], "assistant");
    assert!(
        kept[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("did things")
    );
    let summary = transcript::summary_request("claude-haiku-5-5", &history[..at]);
    assert!(
        summary["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("[tool run: {}]")
    );
    // ---- the run's git grant (core and the proxies) ----
    use pigeon_core::proxy_caller::within_prefix;
    let prefix = pigeon_capabilities::proxy::run_branch_prefix("r1").unwrap();
    assert_eq!(prefix, "claude/ai-agent-r1");
    assert!(pigeon_capabilities::proxy::run_branch_prefix("../main").is_err());
    assert!(pigeon_capabilities::proxy::run_branch_prefix("").is_err());
    assert!(
        within_prefix(
            [
                "refs/heads/claude/ai-agent-r1",
                "refs/heads/claude/ai-agent-r1-wip"
            ],
            &prefix
        )
        .is_ok()
    );
    assert_eq!(
        within_prefix(
            [
                "refs/heads/claude/ai-agent-r1",
                "refs/heads/main",
                "refs/heads/claude/ai-agent-r2"
            ],
            &prefix
        )
        .unwrap_err(),
        ["refs/heads/main", "refs/heads/claude/ai-agent-r2"]
    );
    assert!(within_prefix(["refs/tags/v1"], &prefix).is_err());
    // A grant is a proxy caller with a prefix; an older answer without one reads as none.
    let old: pigeon_core::proxy_caller::ProxyCaller =
        serde_json::from_value(json!({ "id": "x", "label": "l", "person": "p" })).unwrap();
    assert!(old.branch_prefix.is_none());

    // ---- who a run acts as ----
    let starter = json!({ "user": { "id": "portal", "name": "Portal" }, "acting_for": { "id": "willow", "name": "Willow" } });
    let linked = json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": { "id": "willow", "name": "Willow" } });
    let unlinked =
        json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": null });
    let other = json!({ "user": { "id": "svc-exec", "name": "Pigeon executor" }, "acting_for": { "id": "mallory" } });
    let as_svc = pigeon_ai_agents::object::chosen_caller(&starter, Some(&linked), "r1");
    assert_eq!(
        (
            as_svc["user"]["id"].as_str(),
            as_svc["acting_for"]["id"].as_str(),
            as_svc["run"].as_str()
        ),
        (Some("svc-exec"), Some("willow"), Some("r1"))
    );
    for svc in [Some(&unlinked), Some(&other), None] {
        let c = pigeon_ai_agents::object::chosen_caller(&starter, svc, "r1");
        assert_eq!(
            (c["user"]["id"].as_str(), c["run"].as_str()),
            (Some("portal"), Some("r1")),
            "falls back to the starter"
        );
    }
    assert_eq!(
        pigeon_ai_agents::object::person_of(&starter),
        Some("willow")
    );
    assert_eq!(
        pigeon_ai_agents::object::person_of(&json!({ "user": { "id": "willow" } })),
        Some("willow")
    );
    assert_eq!(
        pigeon_capabilities::split::service_user_name("ai-agent:executor"),
        Some("Pigeon executor")
    );
    assert_eq!(pigeon_capabilities::split::service_user_name("root"), None);

    // ---- the Worker side's pure parts ----
    use pigeon_ai_agents::{object, prompts, sandbox, service};
    // Live slots (docs/plans/ai-agents.md, decision 4): one ceiling over every role, no per-role
    // planner cap, executors always keep a reserve, and each role starts oldest first.
    {
        use pigeon_ai_agents::Role;
        let planner_cap = object::MAX_LIVE - object::EXECUTOR_RESERVE;
        assert!(planner_cap > 2 && object::MAX_LIVE >= 32);
        // Not the oldest of its role: waits, whatever the room.
        assert!(!object::may_start(Role::Planner, 0, false));
        assert!(!object::may_start(Role::Executor, 0, false));
        // The index's admission, as `slot` runs it: queued runs, in created order, each taking a
        // slot when it is the oldest queued of its role and `may_start` lets it.
        let admit = |queue: &mut Vec<(Role, u32)>, live: &mut Vec<(Role, u32)>| {
            loop {
                let next = queue.iter().position(|(role, _)| {
                    let oldest = queue
                        .iter()
                        .find(|(r, _)| (*r == Role::Planner) == (*role == Role::Planner))
                        .map(|(_, n)| *n);
                    let me = queue.iter().find(|(r, _)| r == role).map(|(_, n)| *n);
                    object::may_start(*role, live.len() as u32, oldest == me)
                });
                match next {
                    Some(i) => live.push(queue.remove(i)),
                    None => break,
                }
            }
        };
        // Five planners queued at once all start: no more waiting behind two.
        let mut live = vec![];
        let mut queue: Vec<(Role, u32)> = (0..5).map(|n| (Role::Planner, n)).collect();
        admit(&mut queue, &mut live);
        assert_eq!(live.len(), 5);
        assert!(queue.is_empty());
        // Many more planners: they fill up to the ceiling less the reserve, oldest first.
        let mut queue: Vec<(Role, u32)> = (5..40).map(|n| (Role::Planner, n)).collect();
        admit(&mut queue, &mut live);
        assert_eq!(live.len() as u32, planner_cap);
        assert_eq!(
            live.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            (0..planner_cap).collect::<Vec<_>>()
        );
        assert_eq!(queue.first(), Some(&(Role::Planner, planner_cap)));
        // Their executors still start, in the reserve, oldest first, up to the ceiling.
        queue.extend((100..120).map(|n| {
            (
                if n % 2 == 0 {
                    Role::Executor
                } else {
                    Role::Mechanical
                },
                n,
            )
        }));
        admit(&mut queue, &mut live);
        assert_eq!(live.len() as u32, object::MAX_LIVE);
        let started: Vec<u32> = live[planner_cap as usize..]
            .iter()
            .map(|(_, n)| *n)
            .collect();
        assert_eq!(
            started,
            (100..100 + object::EXECUTOR_RESERVE).collect::<Vec<_>>()
        );
        // At the ceiling nothing starts.
        assert!(!object::may_start(Role::Executor, object::MAX_LIVE, true));
        assert!(!object::may_start(Role::Planner, planner_cap, true));
        // A run ends: the oldest queued executor takes its place, not a planner (no room for one
        // while the reserve is in use).
        live.retain(|(_, n)| *n != 3);
        admit(&mut queue, &mut live);
        assert_eq!(
            live.last(),
            Some(&(Role::Executor, 100 + object::EXECUTOR_RESERVE))
        );
        assert_eq!(queue.first(), Some(&(Role::Planner, planner_cap)));
    }
    // A passkey wait, read from dispatch's own words.
    let refusal = "`deploy_token` needs the person's passkey for this call: approval `aP3x_9-k` waits on the console's Approvals tab until 14:32 UTC. Once approved, call `deploy_token` again with the same arguments and \"approval\": \"aP3x_9-k\"; `approvals_status` says where it stands.";
    assert_eq!(object::approval_id(refusal).as_deref(), Some("aP3x_9-k"));
    assert_eq!(object::approval_id("ci_submit: no such job"), None);
    assert_eq!(object::role_named("mechanical"), Role::Mechanical);
    // Starting: an executor by default; planners later; a brief is required; base is checked.
    let (role, brief, base, b) = service::start_args(&json!({ "brief": " fix it " })).unwrap();
    assert_eq!(
        (role, brief.as_str(), base.as_str(), b.turns),
        (Role::Executor, "fix it", "main", 60)
    );
    let (role, _, _, b) = service::start_args(&json!({ "brief": "x", "role": "planner" })).unwrap();
    assert_eq!((role, b.executors), (Role::Planner, 12));
    assert!(service::start_args(&json!({ "brief": "x", "role": "boss" })).is_err());
    assert!(
        service::start_args(&json!({}))
            .unwrap_err()
            .contains("brief")
    );
    assert!(service::start_args(&json!({ "brief": "x", "base": "main..evil" })).is_err());
    assert!(service::start_args(&json!({ "brief": "x", "budget": { "cost_usd": 500 } })).is_err());
    assert_eq!(service::branch("r1"), "claude/ai-agent-r1");
    // The policy's session branches cover a run's branch: no new rule.
    let policy = pigeon_core::policy::parse(pigeon_capabilities::policy::TEXT).unwrap();
    let push = policy.evaluate(
        &vars(),
        &request("push_branch", &service::branch("r1"), None),
    );
    assert_eq!(push.rule.as_deref(), Some("session-branches"));
    // Sandbox tools map onto the server's ops; a commit pushes the run's branch only.
    let (op, body) = sandbox::op_for(
        "commit_push",
        &json!({ "message": "m", "branch": "main" }),
        "claude/ai-agent-r1",
    )
    .unwrap();
    assert_eq!(
        (op, body["branch"].as_str()),
        ("commit_push", Some("claude/ai-agent-r1"))
    );
    assert_eq!(
        sandbox::op_for("run", &json!({ "cmd": "ls" }), "b")
            .unwrap()
            .0,
        "exec"
    );
    // A run's command goes as given: image v2's sandboxd runs `bash -c`, which keeps PATH.
    let (_, ran) = sandbox::op_for("run", &json!({ "cmd": "cargo --version" }), "b").unwrap();
    assert_eq!(ran["cmd"], json!("cargo --version"));
    assert!(sandbox::op_for("land", &json!({}), "b").is_none());
    // A planner reads its executors' branches through sandboxd's `fetch`, which holds the grant.
    assert_eq!(
        sandbox::op_for("fetch", &json!({ "ref": "claude/ai-agent-r2" }), "b"),
        Some(("fetch", json!({ "ref": "claude/ai-agent-r2" })))
    );
    assert_eq!(
        sandbox::shown(
            "fetch",
            &json!({ "commit": "abc", "at": "origin/claude/ai-agent-r2" })
        ),
        "fetched abc at `origin/claude/ai-agent-r2`"
    );
    let shown = sandbox::shown(
        "exec",
        &json!({ "exit": 0, "ms": 1200, "bytes": 3, "saved_at": "/tmp/out/1.log", "output": "ok\n", "timed_out": false }),
    );
    assert!(shown.starts_with("exit 0 after 1200 ms") && shown.contains("/tmp/out/1.log"));
    let found = sandbox::shown(
        "grep",
        &json!({ "count": 2, "matches": ["a.rs:1:x", "b.rs:2:y"], "truncated": false }),
    );
    assert_eq!(found, "2 matches\na.rs:1:x\nb.rs:2:y");
    assert_eq!(sandbox::instance("executor"), "standard-4");
    sandbox_prepare_retry();
    // What a run is told: its branch, the rules, and only tools pigeon has.
    let sys = prompts::system(Role::Executor, "claude/ai-agent-r1");
    assert!(
        sys.contains("claude/ai-agent-r1")
            && sys.contains("tests/smoke.rs")
            && sys.contains("never land")
    );
    assert!(
        prompts::first_message(
            &json!({ "brief": "do x", "base": "main", "branch": "claude/ai-agent-r1" })
        )
        .contains("do x")
    );
    let offered = prompts::pigeon_tools();
    assert!(
        tools::PLANNER_PIGEON
            .iter()
            .all(|t| offered.iter().any(|(n, _, _)| n == t)),
        "every listed tool has a schema"
    );
    assert!(prompts::step_line("run", &json!({ "cmd": "a\nb" })).starts_with("run: a b"));
    assert!(
        prompts::step_line("run", &json!({ "cmd": "x".repeat(500) }))
            .chars()
            .count()
            <= 120
    );

    // The capability: its manifest offers exactly its tools, and its section on Instances.
    use pigeon_contract::Service;
    let manifest = service::Agents.manifest();
    let offered: Vec<&str> = manifest.capabilities[0]
        .tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(offered, service::TOOLS);
    assert_eq!(manifest.capabilities[0].sections[0].tab, "instances");
    assert_eq!(manifest.worker, "pigeon-ai-agents");
    // The section: what waits for the person first, every value escaped.
    use pigeon_ai_agents::page;
    let html = page::section(&json!({
        "paused": false,
        "runs": [
            { "run": "r1", "role": "executor", "runtime": "gateway", "state": "waiting",
              "waiting": { "for": "answer", "call": "t1", "question": "Use <b>A</b> or B?", "options": ["A", "B"] },
              "step": "waiting for the person's answer", "spent": { "cost": "$0.42", "turns": 7 },
              "branch": "claude/ai-agent-r1", "brief": "fix <script>alert(1)</script>" },
            { "run": "r2", "role": "planner", "runtime": "claude_code", "state": "done",
              "report": "landed", "spent": { "cost": "$0", "turns": 0 }, "brief": "plan x" }
        ]
    }));
    assert!(html.contains("Waiting for you") && html.contains("Use &lt;b&gt;A&lt;/b&gt; or B?"));
    assert!(!html.contains("<script>") && html.contains("&lt;script&gt;"));
    assert!(html.contains("Claude chat") && html.contains("$0.42 · 7 turns"));
    assert!(page::section(&json!({ "paused": true, "runs": [] })).contains("paused"));
    // An unanswered question: after 5 minutes the run goes on, and says what it assumed.
    assert_eq!(run::ANSWER_PATIENCE_MS, 5 * 60_000);
    let told = run::unanswered("Use A or B?");
    assert!(told.contains("Use A or B?") && told.contains("Assumed:"));
    let html = page::section(
        &json!({ "runs": [{ "run": "r3", "state": "running", "assumed": [{ "question": "<i>A</i> or B?", "at": 1 }], "spent": {} }] }),
    );
    assert!(
        html.contains("Went on without your answer") && html.contains("&lt;i&gt;A&lt;/i&gt; or B?")
    );
    // A run waiting for an answer keeps its question, as the inbox and the section show it.
    let w: Wait =
        serde_json::from_value(json!({ "for": "answer", "call": "t1", "question": "q" })).unwrap();
    assert!(matches!(w, Wait::Answer { ref question, since: 0, .. } if question == "q"));

    // A planner's executors: how many are live, which ended since its wait began, in words.
    let children = json!([
        { "run": "c1", "state": "done", "branch": "claude/ai-agent-c1", "report": "added x", "ended_at": 200 },
        { "run": "c2", "state": "running", "branch": "claude/ai-agent-c2", "ended_at": null },
        { "run": "c3", "state": "failed", "branch": "claude/ai-agent-c3", "report": "old", "ended_at": 50 }
    ]);
    assert_eq!(object::live_children(&children), 1);
    assert!(object::ended_since(&children, 100) && !object::ended_since(&children, 200));
    let summary = object::children_summary(&children, 100);
    assert!(
        summary.contains("c1 done on `claude/ai-agent-c1`: added x") && !summary.contains("c3")
    );
    assert!(summary.ends_with("1 executor(s) still running; executor_result has each report"));
    assert_eq!(
        tools::kind(Role::Planner, "wait_executors"),
        Some(Kind::Agent)
    );
    assert_eq!(tools::kind(Role::Executor, "wait_executors"), None);

    // Handing a run to the Claude chat: the packet carries its brief, branch and a prompt.
    let packet = object::handoff_packet(
        &json!({ "run": "r9", "role": "executor", "branch": "claude/ai-agent-r9", "step": "run: cargo test", "spent": { "cost": "$1" } }),
        "fix the thing",
        Some("main"),
    );
    let prompt = packet["session_prompt"].as_str().unwrap();
    assert!(
        prompt.contains("fix the thing")
            && prompt.contains("`claude/ai-agent-r9`")
            && prompt.contains("ai_agents_report with run r9")
    );
    assert!(service::TOOLS.contains(&"ai_agents_handoff"));
    // Pause is for every run, and says so: a `run` is refused, not taken as a per-run pause.
    assert!(
        service::pause_args("ai_agents_pause", &json!({ "run": "r1" }))
            .unwrap_err()
            .contains("ai_agents_stop")
    );
    assert!(service::pause_args("ai_agents_pause", &json!({})).is_ok());
    assert_eq!(service::pause_answer(true)["scope"], "every run");
    assert_eq!(service::pause_answer(false)["paused"], false);
    // ai_agents_extend takes its amounts beside `run`; `cost` is `cost_usd`.
    assert_eq!(
        service::extension(&json!({ "run": "r1", "cost": 5, "turns": 20 })),
        json!({ "cost_usd": 5, "turns": 20 })
    );
    // ai_agents_note: a short text, which goes into the run's next turn without breaking the
    // alternation of roles, and reads as the person's note.
    assert!(service::note(&json!({ "run": "r1", "text": "  " })).is_err());
    assert!(service::note(&json!({ "run": "r1", "text": "x".repeat(4001) })).is_err());
    assert_eq!(
        service::note(&json!({ "run": "r1", "text": " work in parallel " })).unwrap(),
        "work in parallel"
    );
    let results = transcript::results_message(&[("t1".into(), "ok".into(), false)]);
    let (merged, replaces) = transcript::with_notes(Some(&results), &["go faster".into()]);
    assert!(replaces);
    assert_eq!(merged["content"][0]["type"], "tool_result");
    assert!(
        merged["content"][1]["text"]
            .as_str()
            .unwrap()
            .ends_with("go faster")
    );
    let brief = json!({ "role": "user", "content": "the brief" });
    let (merged, _) = transcript::with_notes(Some(&brief), &["a".into(), "b".into()]);
    assert_eq!(merged["content"][0]["text"], "the brief");
    assert_eq!(merged["content"].as_array().unwrap().len(), 3);
    let reply = json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] });
    let (alone, replaces) = transcript::with_notes(Some(&reply), &["n".into()]);
    assert!(!replaces && alone["role"] == "user");
    assert!(
        service::TOOLS.contains(&"ai_agents_extend") && service::TOOLS.contains(&"ai_agents_note")
    );

    // Falling back to another model drops thinking it cannot read.
    let thought = [
        json!({ "role": "assistant", "content": [{ "type": "thinking", "thinking": "t", "signature": "s" }] }),
    ];
    assert_eq!(
        transcript::strip_thinking(&thought)[0]["content"][0]["text"],
        "(thinking removed)"
    );
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

// ---- apple ----

/// App Store Connect (docs/plans/ios.md, phase 1): the ids' forms, the token's header and claims,
/// a .p8 told from other PEM, and the four slots the console shows.
#[test]
fn apple_ids_token_and_slots() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::apple::{self, Apple};
    use pigeon_core::apple as asc;

    assert!(asc::is_apple_id("A1B2C3D4E5"));
    for bad in ["a1b2c3d4e5", "A1B2C3D4E", "A1B2C3D4E56", "A1B2C3D4E-"] {
        assert!(!asc::is_apple_id(bad), "{bad}");
    }
    assert!(asc::is_issuer_id("57246542-96fe-1a63-e053-0824d011072a"));
    for bad in [
        "57246542-96fe-1a63-e053",
        "57246542x96fe-1a63-e053-0824d011072a",
        "not-a-uuid-at-all-x",
    ] {
        assert!(!asc::is_issuer_id(bad), "{bad}");
    }

    // The token: ES256 with the key id, ten minutes, App Store Connect's audience.
    let input = asc::jwt_signing_input("A1B2C3D4E5", "57246542-96fe-1a63-e053-0824d011072a", 1_000);
    let (h, c) = input.split_once('.').unwrap();
    let header: Value =
        serde_json::from_slice(&pigeon_core::webauthn::unb64url(h).unwrap()).unwrap();
    let claims: Value =
        serde_json::from_slice(&pigeon_core::webauthn::unb64url(c).unwrap()).unwrap();
    assert_eq!(
        header,
        json!({ "alg": "ES256", "kid": "A1B2C3D4E5", "typ": "JWT" })
    );
    assert_eq!(
        claims,
        json!({ "iss": "57246542-96fe-1a63-e053-0824d011072a", "iat": 1_000, "exp": 1_600, "aud": "appstoreconnect-v1" })
    );

    // Only a PKCS#8 PEM of a .p8's size is taken; a certificate, an RSA key or text is not.
    assert!(asc::p8_der("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----").is_none());
    assert!(
        asc::p8_der("-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----")
            .is_none()
    );
    assert!(asc::p8_der("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----").is_none());
    assert!(asc::p8_der("hello").is_none());

    // An error from Apple names its status and code, never more than 200 characters.
    let e = asc::api_error(
        401,
        r#"{"errors":[{"status":"401","code":"NOT_AUTHORIZED","title":"Authentication credentials are missing or invalid."}]}"#,
    );
    assert_eq!(
        e,
        "App Store Connect 401 NOT_AUTHORIZED: Authentication credentials are missing or invalid."
    );
    assert_eq!(asc::api_error(500, "<html>"), "App Store Connect 500");

    // The console's slots: the four the person fills, in order, all for Workers.
    let apple_cap = Apple {
        passkeys: pigeon_capabilities::passkeys::Passkeys {
            host: "platform.example.com".into(),
            site_name: "pigeon".into(),
        },
    };
    let slots: Vec<String> = apple_cap.secrets().into_iter().map(|s| s.name).collect();
    assert_eq!(slots, apple::SLOTS.map(String::from).to_vec());
    assert_eq!(
        slots,
        [
            "APPLE_TEAM_ID",
            "APPLE_ASC_ISSUER_ID",
            "APPLE_ASC_KEY_ID",
            "APPLE_ASC_KEY"
        ]
    );
    let tools: Vec<&str> = apple_cap.tools().iter().map(|t| t.name).collect();
    assert_eq!(tools, ["ios_releases", "apple_status"]);
    assert_eq!(apple_cap.routes(), ["/apple/", "/app/oauth"]);
    // Phase 5: the AASA names only dev.pigeon.app under the team, for passkeys and for the
    // /app/oauth callback only, and nothing for a bad team id;
    // the return page never echoes what it was called with.
    assert_eq!(
        apple::aasa_document("A1B2C3D4E5").unwrap(),
        json!({
            "applinks": { "details": [{
                "appIDs": ["A1B2C3D4E5.dev.pigeon.app"],
                "components": [{ "/": "/app/oauth*" }],
            }] },
            "webcredentials": { "apps": ["A1B2C3D4E5.dev.pigeon.app"] },
        })
    );
    assert!(apple::aasa_document("not-a-team").is_none());
    assert!(apple::aasa_document("").is_none());
    assert!(!apple::APP_RETURN_PAGE.contains('{'));
    assert_eq!(asc::BUNDLE_ID, "dev.pigeon.app");
}

/// Phase 2 (docs/plans/ios.md): the CSR's DER, laid out byte for byte; the bodies sent to Apple;
/// the console writes' passkey purposes; and an identity's record, sealed key and all, which
/// round-trips through the Directory and is never shown.
#[test]
fn apple_signing_identity_requests_and_records() {
    use pigeon_capabilities::apple::{self, Identity, Profile};
    use pigeon_core::apple::{b64, csr, unb64};

    // A stand-in public key and signature; the layout around them is what is checked.
    let spki = [0x30, 0x03, 0x02, 0x01, 0x05];
    let info = csr::request_info(&spki, apple::COMMON_NAME);
    let mut want = vec![0x30, 0x2c, 0x02, 0x01, 0x00]; // info, version 0
    want.extend([0x30, 0x20, 0x31, 0x1e, 0x30, 0x1c]); // subject: one RDN, one attribute
    want.extend([0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x15]); // commonName, UTF8String
    want.extend(b"pigeon dev.pigeon.app");
    want.extend(spki);
    want.extend([0xa0, 0x00]); // no attributes
    assert_eq!(info, want);

    let req = csr::request(&info, &[0xab; 256]);
    assert_eq!(req[..4], [0x30, 0x82, 0x01, 0x42]); // long-form length, 322 bytes
    assert_eq!(req[4..50], info[..]);
    assert_eq!(
        req[50..65],
        [
            0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b, 0x05,
            0x00
        ]
    ); // sha256WithRSAEncryption, NULL
    assert_eq!(req[65..70], [0x03, 0x82, 0x01, 0x01, 0x00]); // BIT STRING, no unused bits
    assert_eq!(req.len(), 70 + 256);
    assert_eq!(csr::tlv(0x04, &[0; 200])[..3], [0x04, 0x81, 0xc8]);

    // Apple takes standard base64 with padding; base64url is refused when reading.
    assert_eq!(b64(&[0xfb, 0xff, 0x01]), "+/8B");
    assert_eq!(b64(&[1, 2, 3, 4, 5]), "AQIDBAU=");
    assert_eq!(unb64("AQID\nBAU=").unwrap(), [1, 2, 3, 4, 5]);
    assert_eq!(unb64("+/8B").unwrap(), [0xfb, 0xff, 0x01]);
    assert!(unb64("-_8B").is_none());

    // The bodies: a DISTRIBUTION certificate from the CSR, an App Store profile for one
    // certificate and the bundle id.
    let body = apple::certificate_request(&req);
    assert_eq!(body["data"]["type"], "certificates");
    assert_eq!(
        body["data"]["attributes"]["certificateType"],
        "DISTRIBUTION"
    );
    assert_eq!(
        unb64(body["data"]["attributes"]["csrContent"].as_str().unwrap()).unwrap(),
        req
    );
    let body = apple::profile_request("pigeon dev.pigeon.app 2026-11-22", "BUNDLE1", "CERT1");
    assert_eq!(
        body,
        json!({ "data": { "type": "profiles",
            "attributes": { "name": "pigeon dev.pigeon.app 2026-11-22", "profileType": "IOS_APP_STORE" },
            "relationships": {
                "bundleId": { "data": { "type": "bundleIds", "id": "BUNDLE1" } },
                "certificates": { "data": [{ "type": "certificates", "id": "CERT1" }] },
            } } })
    );

    // Adopt adds only what is missing.
    assert_eq!(
        apple::missing(
            &apple::CAPABILITIES,
            &["PUSH_NOTIFICATIONS".into(), "IN_APP_PURCHASE".into()]
        ),
        ["ASSOCIATED_DOMAINS"]
    );
    assert!(
        apple::missing(&apple::CAPABILITIES, &apple::CAPABILITIES.map(String::from)).is_empty()
    );

    // Each write's passkey names it and the bundle id; nothing else is a write.
    assert_eq!(
        apple::purpose("adopt").unwrap(),
        "apple:adopt:dev.pigeon.app"
    );
    assert_eq!(
        apple::purpose("identity").unwrap(),
        "apple:identity:dev.pigeon.app"
    );
    assert!(apple::purpose("revoke").is_none());
    assert_eq!(apple::seal_aad("CERT1"), "apple:identity:CERT1");

    // The record: the sealed key and the bytes round-trip, the expiry is the earlier of the
    // certificate's and the profile's, and what is shown holds none of the secret parts.
    let sealed =
        pigeon_core::webauthn::b64url(&pigeon_core::crypto::join_sealed(&[7; 12], &[9; 40]));
    let (iv, ct) =
        pigeon_core::crypto::split_sealed(&pigeon_core::webauthn::unb64url(&sealed).unwrap())
            .map(|(a, b)| (a.to_vec(), b.to_vec()))
            .unwrap();
    assert_eq!((iv, ct), (vec![7; 12], vec![9; 40]));
    assert!(pigeon_core::crypto::split_sealed(&[0; 27]).is_none());
    let id = Identity {
        id: "CERT1".into(),
        serial: "ABC123".into(),
        expires: "2027-11-22T10:00:00.000+00:00".into(),
        certificate: "CERTIFICATE-BYTES".into(),
        sealed_key: sealed.clone(),
        made_by: "willow".into(),
        made_ms: 1,
        profile: Some(Profile {
            id: "PROF1".into(),
            uuid: "u-1".into(),
            name: "pigeon dev.pigeon.app 2026-11-22".into(),
            expires: "2027-11-20T10:00:00.000+00:00".into(),
            content: "PROFILE-BYTES".into(),
        }),
    };
    let back: Identity = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
    assert_eq!(back, id);
    assert_eq!(
        id.expires_ms(),
        pigeon_core::expiry::parse_time("2027-11-20T10:00:00Z")
    );
    let shown = id.shown().to_string();
    for secret in [sealed.as_str(), "CERTIFICATE-BYTES", "PROFILE-BYTES"] {
        assert!(!shown.contains(secret), "{secret}");
    }
    assert!(shown.contains("ABC123") && shown.contains("PROF1"));
}

/// Phase 4 (docs/plans/ios.md): a release's build number, its steps in order, the bodies that
/// upload the IPA, and the upload parts Apple asks for, checked before a byte is sent.
#[test]
fn apple_release_numbers_steps_and_upload() {
    use pigeon_capabilities::apple::release::{
        self, file_request, may, next_number, upload_request,
    };
    use pigeon_core::apple::{is_version, parts, pkcs8_pem};

    // Above Apple's highest (35 for the old app) and above pigeon's last, whichever is higher.
    assert_eq!(next_number(None, Some(35)), 36);
    assert_eq!(next_number(Some(40), Some(35)), 41);
    assert_eq!(next_number(None, None), 1);

    // Start, then the identity once, then the IPA once.
    assert!(may("started", "identity").is_ok());
    assert!(may("signing", "identity").is_err());
    assert!(may("signing", "upload").is_ok());
    for state in ["started", "uploading", "processing", "complete", "failed"] {
        assert!(may(state, "upload").is_err(), "{state}");
    }
    assert!(release::open("processing") && !release::open("complete"));
    // Apple's upload state is an object; its errors and warnings are kept as the detail.
    let st = json!({ "state": "FAILED", "errors": [{ "code": "90000", "description": "Bad bundle" }],
        "warnings": [{ "code": "W1", "description": "Old SDK" }] });
    assert_eq!(release::upload_state(&st), "FAILED");
    assert_eq!(release::upload_state(&json!("COMPLETE")), "COMPLETE");
    assert_eq!(
        release::upload_problems(&st),
        "errors: 90000 Bad bundle; warnings: W1 Old SDK"
    );
    assert_eq!(release::JOB, "ios-release");

    assert!(is_version("0.2.0") && is_version("1") && is_version("10.4"));
    for bad in ["", "1.2.3.4", "v1", "1..2", "1.2-beta"] {
        assert!(!is_version(bad), "{bad}");
    }

    assert_eq!(
        upload_request("APP1", "0.2.0", 36),
        json!({ "data": { "type": "buildUploads",
            "attributes": { "cfBundleShortVersionString": "0.2.0", "cfBundleVersion": "36", "platform": "IOS" },
            "relationships": { "app": { "data": { "type": "apps", "id": "APP1" } } } } })
    );
    let f = file_request("UP1", 1234);
    assert_eq!(f["data"]["attributes"]["uti"], "com.apple.ipa");
    assert_eq!(f["data"]["attributes"]["assetType"], "ASSET");
    assert_eq!(f["data"]["attributes"]["fileSize"], 1234);
    assert_eq!(
        f["data"]["relationships"]["buildUpload"]["data"]["id"],
        "UP1"
    );

    // Parts must be https, PUT or POST, in order, covering the file exactly.
    let op = |o: u64, l: u64| {
        json!({ "method": "PUT", "url": "https://upload.example/x", "offset": o, "length": l,
        "requestHeaders": [{ "name": "Content-Type", "value": "application/octet-stream" }] })
    };
    let ok = parts(&json!([op(100, 50), op(0, 100)]), 150).unwrap();
    assert_eq!((ok[0].offset, ok[0].length, ok[1].offset), (0, 100, 100));
    assert_eq!(
        ok[0].headers,
        [(
            "Content-Type".to_string(),
            "application/octet-stream".to_string()
        )]
    );
    assert!(parts(&json!([op(0, 100)]), 150).is_err());
    assert!(parts(&json!([op(0, 100), op(90, 60)]), 150).is_err());
    assert!(
        parts(
            &json!([{ "method": "GET", "url": "https://x", "offset": 0, "length": 1 }]),
            1
        )
        .is_err()
    );
    assert!(
        parts(
            &json!([{ "method": "PUT", "url": "http://x", "offset": 0, "length": 1 }]),
            1
        )
        .is_err()
    );
    assert!(parts(&Value::Null, 1).is_err());

    let pem = pkcs8_pem(&[0x30; 100]);
    assert!(
        pem.starts_with("-----BEGIN PRIVATE KEY-----\n")
            && pem.ends_with("-----END PRIVATE KEY-----\n")
    );
    assert!(pem.lines().all(|l| l.len() <= 64));
    assert_eq!(pigeon_core::apple::p8_der(&pem).unwrap(), vec![0x30; 100]);
}

// ---- storage ----

#[test]
fn storage_reads_s3_compatible_accounts() {
    use pigeon_capabilities::storage::{self, Account, Mode, Provider};
    use pigeon_core::s3::{self, Endpoint, Keys};
    let keys = Keys {
        access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
        secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
    };
    let aws = Endpoint {
        base: "https://examplebucket.s3.amazonaws.com".into(),
        region: "us-east-1".into(),
    };
    let date = "20130524T000000Z";

    // AWS's published header-auth example.
    let headers = s3::sign(
        "GET",
        &aws,
        "/test.txt",
        &[],
        &[("range".into(), "bytes=0-9".into())],
        s3::EMPTY_SHA256,
        &keys,
        date,
    );
    let auth = &headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .unwrap()
        .1;
    assert!(
        auth.ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ),
        "{auth}"
    );
    assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    // AWS's published query-string example, past the tool's cap, so the inner function.
    let url = s3::presign_url(&aws, "/test.txt", &keys, date, 86_400);
    assert!(
        url.ends_with(
            "X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        ),
        "{url}"
    );
    assert!(url.contains(
        "X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request"
    ));
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 86_400).is_err());
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 0).is_err());
    assert!(s3::presign_get(&aws, "/test.txt", &keys, date, 3600).is_ok());

    // Encoding.
    assert_eq!(s3::uri_encode("a b/ü.txt", false), "a%20b/%C3%BC.txt");
    assert_eq!(s3::uri_encode("a b/ü~-._", true), "a%20b%2F%C3%BC~-._");
    assert_eq!(s3::object_path("bkt", "dir/a b"), "/bkt/dir/a%20b");

    // Keys.
    let k = s3::parse_keys(" AKID : sec/ret:x \n").unwrap();
    assert_eq!(k.access_key_id, "AKID");
    assert_eq!(k.secret_access_key, "sec/ret:x");
    assert!(!format!("{k:?}").contains("sec/ret"));
    for bad in ["", "nocolon", ":secret", "id:", "id:sec ret", "i d:secret"] {
        let e = s3::parse_keys(bad).unwrap_err().to_string();
        assert!(bad.len() < 4 || !e.contains(bad), "{e}");
    }
    assert!(
        !s3::parse_keys("id:sec ret")
            .unwrap_err()
            .to_string()
            .contains("sec ret")
    );

    // Recorded answers.
    let buckets = s3::parse_list_buckets(
        "<?xml version=\"1.0\"?><ListAllMyBucketsResult><Owner><ID>x</ID></Owner><Buckets>\
         <Bucket><Name>photos</Name><CreationDate>2020-01-02T03:04:05.000Z</CreationDate></Bucket>\
         <Bucket><Name>logs</Name><CreationDate>2021-01-02T03:04:05.000Z</CreationDate></Bucket>\
         </Buckets></ListAllMyBucketsResult>",
    )
    .unwrap();
    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[1].name, "logs");
    assert_eq!(buckets[0].created, "2020-01-02T03:04:05.000Z");
    assert!(s3::parse_list_buckets("<Error/>").is_err());

    let page = s3::parse_list_objects(
        "<ListBucketResult><Name>b</Name><Prefix>a/</Prefix><IsTruncated>true</IsTruncated>\
         <NextContinuationToken>tok/en==</NextContinuationToken>\
         <Contents><Key>a/one.txt</Key><LastModified>2020-01-01T00:00:00.000Z</LastModified>\
         <ETag>&quot;abc&quot;</ETag><Size>12</Size></Contents>\
         <CommonPrefixes><Prefix>a/sub/</Prefix></CommonPrefixes></ListBucketResult>",
    )
    .unwrap();
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].size, 12);
    assert_eq!(page.objects[0].etag, "abc");
    assert_eq!(page.prefixes, vec!["a/sub/".to_string()]);
    assert_eq!(page.next.as_deref(), Some("tok/en=="));
    let last = s3::parse_list_objects(
        "<ListBucketResult><IsTruncated>false</IsTruncated>\
         <Contents><Key>x &amp; y.txt</Key><Size>0</Size></Contents></ListBucketResult>",
    )
    .unwrap();
    assert_eq!(last.objects[0].key, "x & y.txt");
    assert_eq!(last.next, None);

    assert_eq!(
        s3::parse_error(
            403,
            "<Error><Code>AccessDenied</Code><Message>Access &lt;Denied&gt;</Message></Error>"
        ),
        "S3 403 AccessDenied: Access <Denied>"
    );
    assert_eq!(s3::parse_error(404, ""), "S3 404");
    assert!(
        s3::parse_error(
            500,
            &format!(
                "<Error><Code>X</Code><Message>{}</Message></Error>",
                "m".repeat(500)
            )
        )
        .len()
            <= 200
    );

    // Queries and dates.
    let q = s3::list_objects_query(Some("a/"), Some("/"), 5000, Some("t"));
    assert!(q.contains(&("max-keys".into(), "1000".into())));
    assert!(q.contains(&("list-type".into(), "2".into())));
    assert!(q.contains(&("continuation-token".into(), "t".into())));
    let q = s3::list_objects_query(None, None, 0, None);
    assert!(q.contains(&("max-keys".into(), "1".into())));
    assert_eq!(q.len(), 2);
    assert_eq!(s3::amz_date(1_369_353_600_000.0), "20130524T000000Z");
    assert_eq!(s3::amz_date(1_700_000_000_000.0), "20231114T221320Z");

    // The registry.
    let sw = Account::scaleway("personal", "fr-par");
    assert_eq!(sw.endpoint, "https://s3.fr-par.scw.cloud");
    assert_eq!(sw.id(), "scaleway:personal");
    assert_eq!(sw.binding(), "STORAGE_SCALEWAY_PERSONAL");
    let docean = Account::digitalocean("old-one", "nyc3");
    assert_eq!(docean.endpoint, "https://nyc3.digitaloceanspaces.com");
    assert_eq!(docean.mode, Mode::ReadOnly);
    assert_eq!(docean.binding(), "STORAGE_DIGITALOCEAN_OLD_ONE");
    let b2 = Account::backblaze("bk", "us-west-004");
    assert_eq!(b2.endpoint, "https://s3.us-west-004.backblazeb2.com");
    assert_eq!(b2.provider, Provider::Backblaze);
    assert!(storage::validate(&[sw.clone(), docean.clone(), b2.clone()]).is_ok());
    assert!(storage::validate(&[]).is_ok());

    let mut rw = docean.clone();
    rw.mode = Mode::ReadWrite;
    assert!(storage::validate(&[rw]).is_err());
    for name in ["", "Bad", "-x", "x-", "a_b", &"a".repeat(33)] {
        assert!(
            storage::validate(&[Account::scaleway(name, "fr-par")]).is_err(),
            "{name}"
        );
    }
    assert!(storage::validate(&[sw.clone(), sw.clone()]).is_err());
    let mut slash = sw.clone();
    slash.endpoint.push('/');
    assert!(storage::validate(&[slash]).is_err());
    let mut http = sw.clone();
    http.endpoint = "http://s3.fr-par.scw.cloud".into();
    assert!(storage::validate(&[http]).is_err());
    let mut shortb = sw.clone();
    shortb.buckets = vec!["ab".into()];
    assert!(storage::validate(&[shortb]).is_err());

    // Finding, and the setup text with no accounts.
    let e = storage::find(&[], "scaleway:personal")
        .unwrap_err()
        .to_string();
    assert!(e.contains("STORAGE_<PROVIDER>_<NAME>"));
    let e = storage::find(std::slice::from_ref(&sw), "scaleway:other")
        .unwrap_err()
        .to_string();
    assert!(e.contains("scaleway:personal"));
    assert_eq!(
        storage::find(std::slice::from_ref(&sw), "scaleway:personal")
            .unwrap()
            .name,
        "personal"
    );

    // Bucket limits.
    let mut limited = sw.clone();
    assert!(storage::bucket_allowed(&limited, "anything"));
    limited.buckets = vec!["photos".into()];
    assert!(storage::bucket_allowed(&limited, "photos"));
    assert!(!storage::bucket_allowed(&limited, "logs"));
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

// ---- design ----

#[test]
fn platform_design_tokens_for_ios() {
    use pigeon_core::design::swift;

    // The committed Swift file is what the generator writes now.
    let out = swift::generated().unwrap();
    assert!(
        include_str!("../ios/Pigeon/Design/Tokens.swift") == out,
        "ios/Pigeon/Design/Tokens.swift drifted from tokens.css: run the design_swift example"
    );

    // Spot values.
    assert!(out.ends_with("}\n") && !out.ends_with("\n\n"));
    assert!(out.contains(
        "static let background = dynamic(light: (0xF4, 0xEE, 0xE3, 1.0), dark: (0x14, 0x12, 0x1A, 1.0))"
    ));
    assert!(out.contains("static let accentSoft = dynamic("));
    assert!(out.contains("static let radiusCard: CGFloat = 14"));
    assert!(out.contains(
        "static let typeEyebrow = TypeToken(weight: 600, size: 12, lineHeight: 1.3, mono: true)"
    ));

    // Errors name the token.
    let css = |dark: &str, light: &str| {
        format!(":root {{ {dark} }}\n:root[data-theme=\"light\"] {{ {light} }}\n")
    };
    let e = swift::tokens(&css("--a: #000000; --b: #111111;", "--a: #FFFFFF;")).unwrap_err();
    assert!(e.contains("--b"), "{e}");
    let e = swift::tokens(&css("--blur: 4em;", "")).unwrap_err();
    assert!(e.contains("--blur"), "{e}");
    assert!(swift::tokens(&css("--a: #000000;", "--a: #FFFFFF;")).is_ok());
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
        include_str!("../src/platform/wrangler.toml"),
        include_str!("../ci/workers.toml"),
        "pigeon-security/pigeon",
    );
    let core = pigeon_capabilities::architecture::Wrangler::read(include_str!(
        "../src/platform/wrangler.toml"
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
            "pigeon-ci-gateway",
            "pigeon-proxy",
            "pigeon-ai-agents"
        ]
    );
    let broken = pigeon_proxies::cf_proxy::CfProxy::platform("h", "s", "not toml [", "nope", "r");
    assert!(broken.others.is_empty() && broken.queues.is_empty());

    // The cutover moves the proxy host in one call, `setup_proxy_host {worker}`, and back.
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::public_host::{PublicHost, Retire};
    let host = |also: Vec<String>| PublicHost {
        zone: "pigeon.dev".into(),
        host: "proxy.pigeon.dev".into(),
        worker: "pigeon-platform".into(),
        also,
        retire: Retire::default(),
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
        "scope=account.read%20zone.read%20offline_access",
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

    // An agent's proposal is a draft bound to the record and the version it was made from;
    // approving applies it only while the instance is still at that version.
    use pigeon_core::approval::{self as appr, Kind, State};
    let payload = json!({ "record": on, "base_version": 3 });
    let digest = appr::payload_digest(&Kind::InstanceDraft, "noisypigeon", &payload);
    let moved = json!({ "record": on, "base_version": 4 });
    assert_ne!(
        digest,
        appr::payload_digest(&Kind::InstanceDraft, "noisypigeon", &moved)
    );
    assert!(Kind::InstanceDraft.applies_on_approve());
    let mut draft = appr::new(
        "d1".into(),
        Kind::InstanceDraft,
        "noisypigeon".into(),
        digest.clone(),
        json!({ "changes": changes }),
        payload,
        "agent".into(),
        Some("willow".into()),
        0,
    );
    appr::decide(&mut draft, true, &digest, "willow", 1).unwrap();
    assert_eq!(draft.state, State::Approved);
    assert!(
        !appr::stale_draft(&mut draft.clone(), 3, 3),
        "still at its version: applied"
    );
    assert!(
        appr::stale_draft(&mut draft, 4, 3),
        "the record moved on: not applied"
    );
    assert_eq!(draft.state, State::Expired);

    // The subdomain is served by host, from the data alone.
    let all = [platform, noisy];
    let (i, site) = site_for_host(&all, "NoisyPigeon.pigeon.dev", "pigeon.dev").unwrap();
    assert_eq!((i.id.as_str(), site.name.as_str()), ("noisypigeon", "blog"));
    assert!(site_for_host(&all, "platform.pigeon.dev", "pigeon.dev").is_none());
    assert!(site_for_host(&all, "noisypigeon.pigeon.dev.evil.com", "pigeon.dev").is_none());
}

// ---- Buildkite iOS pipelines ----

#[test]
fn ci_buildkite_ios_pipelines() {
    use pigeon_ci::buildkite::ios::{Ios, Pipeline, off_note, pipelines, shape, validate, yaml};

    // Off by default.
    let off = pipelines(None).unwrap_err();
    assert_eq!(off, off_note());
    assert!(off.contains("docs/plans/ios.md"));

    let ios = Ios {
        host: "platform.example".into(),
        build_pipeline: "pigeon-ios".into(),
        release_pipeline: "pigeon-ios-release".into(),
        queue: "pigeon-macos".into(),
        sign_queue: "pigeon-macos-sign".into(),
        release_branch: "main".into(),
    };
    let (build, release) = pipelines(Some(&ios)).unwrap();
    let macos = ["pigeon-macos", "pigeon-macos-sign"];
    let sign = ["pigeon-macos-sign"];
    assert!(validate(&build, &macos, &sign).is_empty());
    assert!(validate(&release, &macos, &sign).is_empty());

    let want_build = "steps:
  - label: \"iOS build and tests\"
    key: \"ios-check\"
    agents:
      queue: \"pigeon-macos\"
    checkout:
      skip: true
    command: \"curl -fsSL --retry 3 https://platform.example/ci/buildkite.sh | sh\"
    env:
      PIGEON_JOB_KIND: \"ios-check\"
    timeout_in_minutes: 60
";
    let want_release = "steps:
  - label: \"iOS archive, unsigned\"
    key: \"ios-archive\"
    branches: \"main\"
    agents:
      queue: \"pigeon-macos\"
    checkout:
      skip: true
    command: \"curl -fsSL --retry 3 https://platform.example/ci/buildkite.sh | sh\"
    env:
      PIGEON_JOB_KIND: \"ios-archive\"
    artifact_paths: [\"ios/build/Pigeon.xcarchive.zip\"]
    timeout_in_minutes: 60
  - label: \"iOS sign and upload to TestFlight\"
    key: \"ios-sign-upload\"
    depends_on: [\"ios-archive\"]
    branches: \"main\"
    agents:
      queue: \"pigeon-macos-sign\"
    checkout:
      skip: true
    command: \"curl -fsSL --retry 3 https://platform.example/ci/buildkite-sign.sh | sh\"
    env:
      PIGEON_JOB_KIND: \"ios-sign-upload\"
    timeout_in_minutes: 30
    concurrency_group: \"pigeon-ios-release/sign\"
    concurrency: 1
";
    assert_eq!(yaml(&build), want_build);
    assert_eq!(yaml(&release), want_release);
    assert!(shape(&yaml(&build)).is_empty());
    assert!(shape(&yaml(&release)).is_empty());

    // The sign step.
    let s = &release.steps[1];
    assert_eq!(s.depends_on, vec!["ios-archive".to_string()]);
    assert_eq!(s.concurrency, Some(1));
    assert_eq!(s.branches.as_deref(), Some("main"));

    // Refusals.
    let problems = |p: &Pipeline| validate(p, &macos, &sign).join("\n");
    let mut p = release.clone();
    p.steps[0].queue = "linux".into();
    assert!(problems(&p).contains("not a macOS queue"));
    let mut p = release.clone();
    p.steps[1].key = "ios-archive".into();
    assert!(problems(&p).contains("duplicate key"));
    let mut p = release.clone();
    p.steps[1].depends_on = vec!["nope".into()];
    assert!(problems(&p).contains("depends_on `nope`"));
    let mut p = release.clone();
    p.steps[0].depends_on = vec!["ios-sign-upload".into()];
    assert!(problems(&p).contains("depends_on `ios-sign-upload`"));
    let mut p = build.clone();
    p.steps[0].command = "echo $HOME".into();
    assert!(problems(&p).contains("holds `$`"));
    let mut p = build.clone();
    p.steps[0].label = "iOS :rocket:".into();
    assert!(problems(&p).contains("label holds `:`"));
    let mut p = build.clone();
    p.steps[0].timeout_min = 0;
    assert!(problems(&p).contains("timeout_min"));
    let mut p = release.clone();
    p.steps[1].concurrency = None;
    p.steps[1].concurrency_group = None;
    assert!(problems(&p).contains("signing step must have concurrency 1"));

    // A label with a quote and a newline stays one escaped scalar.
    let mut p = build.clone();
    p.steps[0].label = "say \"hi\"\nnow".into();
    let y = yaml(&p);
    assert!(y.contains("  - label: \"say \\\"hi\\\"\\nnow\"\n"));
    assert!(shape(&y).is_empty());
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
    assert_eq!(
        Write::named("approval-approve"),
        Some(Write::ApprovalApprove)
    );
    assert!(Write::ApprovalApprove.bound_to_digest());
    assert!(!Write::ApprovalDeny.bound_to_digest());
    assert!(Write::ApprovalApprove.target("not an id!").is_err());
}

// ---- Buildkite setup ----

/// The Buildkite setup's plan: a first run creates everything; the same state seen back is all
/// `ok`; drift is an update naming only what drifted; a resource with pigeon's name that is not
/// pigeon's (not recorded, another id, or its marker gone) is a conflict that blocks what
/// depends on it and is never touched; a missing scope blocks everything; a recorded resource
/// no longer declared is orphaned, never deleted; secrets never appear in a step.
#[test]
fn buildkite_setup_plan() {
    use pigeon_core::buildkite_setup::{
        self as bk, Declared, Kind, Owned, Seen, Step, description, hash, missing_scopes, plan,
        wants,
    };

    let d = Declared {
        org: "acme".into(),
        cluster: "pigeon".into(),
        pipeline: "pigeon".into(),
        queues: vec![
            ("pigeon-linux-small".into(), "LINUX_AMD64_2X4".into()),
            ("pigeon-linux-medium".into(), "LINUX_AMD64_4X16".into()),
        ],
        macos: None,
        repository: "https://github.com/pigeon-security/pigeon.git".into(),
        steps: "steps: []\n".into(),
        hook_url: "https://platform.example/ci/hooks/buildkite".into(),
        host: "platform.example".into(),
        portal: Some((
            "buildkite".into(),
            "https://mcp.buildkite.com/direct".into(),
        )),
    };
    let key = hash("hook-key-secret");
    let token = hash("bkua_token_secret");
    let w = wants(&d, &key, &token);

    // A macOS queue carries its image (docs/plans/ios.md, phase 3); a Linux queue never does.
    let mut mac = d.clone();
    mac.queues
        .push(("pigeon-macos".into(), "MACOS_ARM64_M4_6X28".into()));
    mac.macos = Some(("TAHOE_26_6".into(), "26.6".into()));
    let mw = wants(&mac, &key, &token);
    let q = |k: &str| {
        mw.iter()
            .find(|x| x.kind == Kind::Queue && x.key == k)
            .unwrap()
    };
    assert_eq!(
        q("pigeon-macos").create["hostedAgents"],
        json!({ "instanceShape": "MACOS_ARM64_M4_6X28",
            "platformSettings": { "macos": { "macosVersion": "TAHOE_26_6", "xcodeVersion": "26.6" } } })
    );
    assert!(q("pigeon-macos").expect.contains(&(
        "/hosted_agents/platform_settings/macos/macos_version".into(),
        json!("TAHOE_26_6")
    )));
    assert_eq!(
        q("pigeon-linux-small").create["hostedAgents"],
        json!({ "instanceShape": "LINUX_AMD64_2X4" })
    );
    let all_scopes: Vec<String> = bk::RUNTIME_SCOPES
        .iter()
        .chain(bk::SETUP_SCOPES.iter())
        .map(|s| s.to_string())
        .collect();
    assert!(missing_scopes(&all_scopes).is_empty());
    let names = |steps: &[Step]| -> Vec<String> {
        steps
            .iter()
            .map(|s| {
                let tag = serde_json::to_value(s).unwrap()["step"]
                    .as_str()
                    .unwrap()
                    .to_string();
                format!("{tag} {}:{}", s.kind().name(), s.key())
            })
            .collect()
    };

    // Nothing there, nothing recorded: everything is created, parents first.
    let first = plan(&w, &[], &[], &[]);
    assert_eq!(
        names(&first),
        [
            "create cluster:pigeon",
            "create queue:pigeon-linux-small",
            "create queue:pigeon-linux-medium",
            "create pipeline:pigeon",
            "create service:hook",
            "create portal_server:buildkite",
        ]
    );

    // As Buildkite and Cloudflare would answer once the apply made it all, ids recorded.
    let desc = |k: Kind, key: &str| description(k, key, "platform.example");
    let mut seen = vec![
        Seen {
            kind: Kind::Cluster,
            key: "pigeon".into(),
            id: "c-1".into(),
            description: desc(Kind::Cluster, "pigeon"),
            body: json!({"name": "pigeon", "description": desc(Kind::Cluster, "pigeon")}),
        },
        Seen {
            kind: Kind::Cluster,
            key: "someone-elses".into(),
            id: "c-9".into(),
            description: "theirs".into(),
            body: json!({"name": "someone-elses"}),
        },
    ];
    for (q, shape) in [
        ("pigeon-linux-small", "LINUX_AMD64_2X4"),
        ("pigeon-linux-medium", "LINUX_AMD64_4X16"),
    ] {
        seen.push(Seen {
            kind: Kind::Queue,
            key: q.into(),
            id: format!("q-{q}"),
            description: desc(Kind::Queue, q),
            body: json!({"key": q, "description": desc(Kind::Queue, q),
                         "hosted_agents": {"instance_shape": {"name": shape}}}),
        });
    }
    seen.push(Seen {
        kind: Kind::Pipeline,
        key: "pigeon".into(),
        id: "p-1".into(),
        description: desc(Kind::Pipeline, "pigeon"),
        body: json!({"name": "pigeon", "description": desc(Kind::Pipeline, "pigeon"),
                     "cluster_id": "c-1", "configuration": "steps: []\n",
                     "default_branch": "main",
                     "provider": {"settings": {"trigger_mode": "none"}}}),
    });
    seen.push(Seen {
        kind: Kind::Service,
        key: "hook".into(),
        id: "s-1".into(),
        description: desc(Kind::Service, "hook"),
        body: json!({"description": desc(Kind::Service, "hook"),
                     "settings": {"url": "https://platform.example/ci/hooks/buildkite",
                                  "token": key, "token_mode": "signature",
                                  "events": bk::HOOK_EVENTS}}),
    });
    seen.push(Seen {
        kind: Kind::PortalServer,
        key: "buildkite".into(),
        id: "buildkite".into(),
        description: desc(Kind::PortalServer, "buildkite"),
        body: json!({"hostname": "https://mcp.buildkite.com/direct", "auth_type": "bearer",
                     "description": desc(Kind::PortalServer, "buildkite")}),
    });
    let owned: Vec<Owned> = [
        (Kind::Cluster, "pigeon", "c-1", None),
        (
            Kind::Queue,
            "pigeon-linux-small",
            "q-pigeon-linux-small",
            None,
        ),
        (
            Kind::Queue,
            "pigeon-linux-medium",
            "q-pigeon-linux-medium",
            None,
        ),
        (Kind::Pipeline, "pigeon", "p-1", None),
        (Kind::Service, "hook", "s-1", None),
        (
            Kind::PortalServer,
            "buildkite",
            "buildkite",
            Some(token.clone()),
        ),
    ]
    .into_iter()
    .map(|(kind, key, id, spec_hash)| Owned {
        kind,
        key: key.into(),
        id: id.into(),
        spec_hash,
    })
    .collect();
    let again = plan(&w, &seen, &owned, &[]);
    assert!(
        again.iter().all(|s| matches!(s, Step::Ok { .. })),
        "{again:?}"
    );

    // Drift: someone changed the pipeline's steps and the webhook key, and the token rotated.
    let mut drifted = seen.clone();
    drifted[4].body["configuration"] = json!("steps: [evil]\n");
    drifted[5].body["settings"]["token"] = json!(hash("other"));
    let rotated = wants(&d, &key, &hash("bkua_new_token"));
    let p = plan(&rotated, &drifted, &owned, &[]);
    assert_eq!(
        p[3],
        Step::Update {
            kind: Kind::Pipeline,
            key: "pigeon".into(),
            id: "p-1".into(),
            fields: vec!["configuration".into()]
        }
    );
    assert!(matches!(&p[4], Step::Update { fields, .. } if fields == &["settings.token"]));
    assert!(matches!(&p[5], Step::Update { fields, .. } if fields == &["credentials"]));
    let shown = serde_json::to_string(&p).unwrap();
    assert!(!shown.contains("hook-key-secret") && !shown.contains("bkua_"));

    // Conflicts: a cluster named `pigeon` that pigeon never recorded blocks all below it.
    let p = plan(&w, &seen, &owned[3..], &[]);
    assert!(
        matches!(&p[0], Step::Conflict { id, why, .. } if id == "c-1" && why.contains("not pigeon's"))
    );
    assert!(p[1..5].iter().all(|s| matches!(s, Step::Blocked { .. })));
    assert!(
        matches!(p[5], Step::Ok { .. }),
        "the portal server stands alone"
    );
    // ...recorded with another id, or its marker removed: still a conflict.
    let mut other_id = owned.clone();
    other_id[3].id = "p-2".into();
    assert!(matches!(
        &plan(&w, &seen, &other_id, &[])[3],
        Step::Conflict { why, .. } if why.contains("not the one pigeon made")
    ));
    let mut unmarked = seen.clone();
    unmarked[0].description = "hand-edited".into();
    assert!(matches!(
        &plan(&w, &unmarked, &owned, &[])[0],
        Step::Conflict { why, .. } if why.contains("lost pigeon's marker")
    ));

    // A token without the setup's scopes: every step blocked, nothing attempted.
    let missing = missing_scopes(&["read_builds".to_string(), "write_builds".to_string()]);
    assert_eq!(missing.len(), bk::SETUP_SCOPES.len());
    assert!(
        plan(&w, &[], &[], &missing)
            .iter()
            .all(|s| matches!(s, Step::Blocked { why, .. } if why.contains("write_clusters")))
    );

    // The medium queue dropped from settings: reported orphaned, never deleted.
    let mut fewer = d.clone();
    fewer.queues.truncate(1);
    let p = plan(&wants(&fewer, &key, &token), &seen, &owned, &[]);
    assert_eq!(
        p.last(),
        Some(&Step::Orphaned {
            kind: Kind::Queue,
            key: "pigeon-linux-medium".into(),
            id: "q-pigeon-linux-medium".into()
        })
    );

    // The applier's fill: ids and secrets go into bodies only at apply time.
    let filled = bk::fill(&w[3].create, &[("cluster", "c-1"), ("pipeline", "p-1")]);
    assert_eq!(filled["cluster_id"], "c-1");
    assert_eq!(w[4].create["settings"]["token"], "${hook_key}");

    // Buildkite's list answers: a bare array (clusters, queues), or `{items, links}` with a next
    // page on Buildkite's API only (notification services); anything else is refused.
    use pigeon_core::buildkite::page;
    let (items, next) = page(json!([{"id": "c-1"}]), "clusters").unwrap();
    assert_eq!((items.len(), next), (1, None));
    let (items, next) = page(
        json!({"items": [{"id": "s-1"}, {"id": "s-2"}],
               "links": {"next": "https://api.buildkite.com/v2/organizations/acme/services?after=X"}}),
        "services",
    )
    .unwrap();
    assert_eq!(items.len(), 2);
    assert!(next.unwrap().ends_with("after=X"));
    let (_, next) = page(
        json!({"items": [], "links": {"next": "https://evil.example/v2/x"}}),
        "services",
    )
    .unwrap();
    assert_eq!(
        next, None,
        "a next page off Buildkite's API is not followed"
    );
    assert!(page(json!({"message": "nope"}), "services").is_err());

    // The Lander takes a setup answer only from a scheduler of its own build: one without a
    // build (older than the check) or of another build is waited out; a local build checks
    // nothing.
    use pigeon_ci::same_build;
    assert!(same_build(&json!({"build": "abc", "reports": {}}), "abc"));
    assert!(!same_build(&json!({"build": "old", "reports": {}}), "abc"));
    assert!(!same_build(&json!({"buildkite": {"off": "…"}}), "abc"));
    assert!(same_build(&json!({}), ""));
}

#[test]
fn linking_a_cloudflare_account() {
    // linked_scopes: each data-Worker need matched to Cloudflare's scope names, ignoring case;
    // a need no scope offers comes back empty.
    {
        use pigeon_capabilities::linked::needs_found;
        let listed = json!([
            { "id": "pipelines.write", "name": "Pipelines Write" },
            { "id": "email-routing.write", "name": "Email Routing Rules Write" },
            { "id": "zone.read", "name": "Zone Read" },
        ]);
        let found = needs_found(&listed, &["Pipelines", "email routing", "Workers AI"]);
        assert_eq!(found["Pipelines"][0]["id"], "pipelines.write");
        assert_eq!(
            found["email routing"][0]["name"],
            "Email Routing Rules Write"
        );
        assert_eq!(found["Workers AI"], json!([]));
    }

    use pigeon_capabilities::linked::{Link, PERMISSIONS, choose_account, scope_ids, status};

    // The client asks for exactly the scopes pigeon needs, found by their permission names.
    let listed = json!([
        { "id": "account-settings.read", "name": "Account Settings Read" },
        { "id": "zone.read", "name": "Zone Read" },
        { "id": "dns.write", "name": "DNS Write" },
        { "id": "workers-scripts.write", "name": "Workers Scripts Write" },
        { "id": "workers-routes.write", "name": "Workers Routes Write" },
        { "id": "r2.write", "name": "Workers R2 Storage Write" },
        { "id": "zone.write", "name": "Zone Write" }
    ]);
    let (ids, missing) = scope_ids(&listed);
    assert_eq!(ids.len(), PERMISSIONS.len());
    assert!(
        missing.is_empty() && !ids.contains(&"zone.write".to_string()),
        "nothing more"
    );
    let (_, missing) = scope_ids(&json!([]));
    assert_eq!(
        missing.len(),
        PERMISSIONS.len(),
        "a missing scope is named, not skipped"
    );

    // The account a link takes: the one the record names, or the only one granted.
    let granted = vec![
        ("a".repeat(32), "noisypigeon".to_string()),
        ("b".repeat(32), "other".to_string()),
    ];
    let a = "a".repeat(32);
    assert_eq!(choose_account(Some(&a), &granted).unwrap().1, "noisypigeon");
    assert!(
        choose_account(Some(&"c".repeat(32)), &granted).is_err(),
        "not granted"
    );
    assert!(
        choose_account(None, &granted).is_err(),
        "several: the person chooses"
    );
    assert_eq!(choose_account(None, &granted[..1]).unwrap().0, a);
    assert!(choose_account(None, &[]).is_err());

    // What the console shows of a link never holds its sealed tokens.
    let l = Link {
        instance: "noisypigeon".into(),
        account_id: a.clone(),
        account_name: "noisypigeon".into(),
        linked_by: "Willow".into(),
        linked_ms: 1,
        expires_at_ms: 10.0,
        scope: None,
        sealed: "SEALED-TOKENS".into(),
        refreshed_ms: None,
        failed: None,
    };
    let shown = status(Some(&l), 5.0).to_string();
    assert!(
        shown.contains("linked") && !shown.contains("SEALED-TOKENS"),
        "{shown}"
    );
    assert_eq!(status(None, 0.0)["status"], "not linked");
}

/// A linked instance's plan: a first plan creates the bucket and uploads the Worker, then turns
/// on its preview; what pigeon made reads back as unchanged; anything with pigeon's name that
/// pigeon did not make is a conflict that stops the plan; a new Worker module is an update; no
/// workers.dev subdomain blocks only until the person picks one; the digest moves with the steps.
#[test]
fn instance_plans_touch_only_what_pigeon_made() {
    use pigeon_capabilities::instances::plan::{
        Ledger, Observed, SeenDomain, Step, describe, digest, module_digest, plan, record,
    };
    use pigeon_capabilities::instances::record::Instance;

    let inst: Instance = serde_json::from_value(json!({
        "id": "noisypigeon", "title": "noisypigeon",
        "account": { "kind": "linked", "account_id": "a".repeat(32) },
        "zones": ["noisypigeon.com"], "worker": "noisypigeon",
        "sites": [{ "name": "blog", "bucket": "noisypigeon-site", "jurisdiction": "eu",
            "build": ".pigeon/site.toml",
            "targets": [{ "kind": "preview" },
                { "kind": "zone", "hostname": "noisypigeon.com", "route": "noisypigeon.com/*", "enabled": false }] }],
        "version": 2
    }))
    .unwrap();
    let seen = |bucket: bool, worker: bool, dev: Option<bool>, sub: Option<&str>| Observed {
        account_id: "a".repeat(32),
        bucket_exists: bucket,
        worker_exists: worker,
        workers_dev: dev,
        account_subdomain: sub.map(str::to_string),
        domains: vec![],
        subdomains: vec![],
    };
    let tags = |steps: &[Step]| -> Vec<String> {
        steps
            .iter()
            .map(|s| {
                serde_json::to_value(s).unwrap()["step"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    };

    // Nothing there: create, upload, preview, in that order.
    let empty = Ledger::default();
    let first = plan(&inst, &seen(false, false, None, Some("np")), &empty);
    assert_eq!(
        tags(&first),
        ["create_bucket", "upload_worker", "enable_workers_dev"]
    );
    assert!(first.iter().all(|s| !s.stops()));
    assert_eq!(describe(&first).len(), 3);

    // Applied and recorded: everything reads back unchanged.
    let mut l = Ledger::default();
    for s in &first {
        record(&mut l, s);
    }
    assert_eq!(l.buckets, ["noisypigeon-site"]);
    let again = plan(&inst, &seen(true, true, Some(true), Some("np")), &l);
    assert_eq!(tags(&again), ["ok", "ok", "ok"]);
    assert!(again.iter().all(|s| !s.changes()));

    // Same names, not pigeon's: conflicts, never touched.
    let theirs = plan(&inst, &seen(true, true, Some(false), Some("np")), &empty);
    assert_eq!(tags(&theirs)[..2], ["conflict", "conflict"]);
    assert!(theirs.iter().any(Step::stops));

    // pigeon's Worker with an older module: an update, not a conflict.
    let mut old = l.clone();
    old.workers = vec![("noisypigeon".into(), "older".into())];
    let upd = plan(&inst, &seen(true, true, Some(true), Some("np")), &old);
    assert!(matches!(&upd[1], Step::UploadWorker { replaces: true, .. }));
    assert_ne!(module_digest("noisypigeon-site", Some("eu")), "older");

    // No workers.dev subdomain on the account: blocked, for the person.
    let blocked = plan(&inst, &seen(false, false, None, None), &empty);
    assert_eq!(tags(&blocked)[2], "blocked");

    // A Domain target: attached when its zone is there and nothing serves it; ours reads back
    // unchanged; another Worker's is a conflict.
    let mut with_domain = inst.clone();
    with_domain.sites[0].targets.push(
        serde_json::from_value(json!({ "kind": "domain", "hostname": "noisypigeon.com" })).unwrap(),
    );
    let mut o = seen(true, true, Some(true), Some("np"));
    o.domains = vec![SeenDomain {
        hostname: "noisypigeon.com".into(),
        zone_id: Some("z1".into()),
        service: None,
    }];
    let d = plan(&with_domain, &o, &l);
    assert!(matches!(d.last(), Some(Step::AttachDomain { zone_id, .. }) if zone_id == "z1"));
    o.domains[0].service = Some("noisypigeon".into());
    assert!(matches!(
        plan(&with_domain, &o, &l).last(),
        Some(Step::Ok { .. })
    ));
    o.domains[0].service = Some("someone-else".into());
    assert!(plan(&with_domain, &o, &l).last().unwrap().stops());
    o.domains[0] = SeenDomain {
        hostname: "noisypigeon.com".into(),
        zone_id: None,
        service: None,
    };
    assert!(matches!(
        plan(&with_domain, &o, &l).last(),
        Some(Step::Blocked { .. })
    ));

    // A push is accepted only from the repository and installation an instance's record names.
    use pigeon_capabilities::github::instance_for;
    let mut gh = inst.clone();
    gh.repo = Some(
        serde_json::from_value(
            json!({ "kind": "github", "full_name": "noisypigeon/noisypigeon", "installation": 7 }),
        )
        .unwrap(),
    );
    let records = vec![gh];
    assert!(instance_for(&records, "NoisyPigeon/noisypigeon", 7).is_some());
    assert!(instance_for(&records, "noisypigeon/noisypigeon", 8).is_none());
    assert!(instance_for(&records, "someone/else", 7).is_none());

    // A push-started deploy runs when pending, or when its run was cut off (its lease is
    // over), and stops being tried after five attempts or once done.
    use pigeon_capabilities::github::due;
    assert!(due(&json!({ "state": "pending", "attempts": 0 }), 0.0));
    assert!(!due(
        &json!({ "state": "running", "started_ms": 1000.0, "attempts": 1 }),
        60_000.0
    ));
    assert!(due(
        &json!({ "state": "running", "started_ms": 1000.0, "attempts": 1 }),
        300_000.0
    ));
    assert!(!due(&json!({ "state": "pending", "attempts": 5 }), 0.0));
    assert!(!due(&json!({ "state": "done", "attempts": 1 }), 0.0));

    // A Subdomain target: attached in the platform's account, on the platform's Worker, with
    // the instance Worker's workers.dev address as its upstream; unchanged once both are kept.
    use pigeon_capabilities::instances::plan::{SeenSubdomain, upstream_for};
    let mut sub = seen(true, true, Some(true), Some("np"));
    sub.subdomains = vec![SeenSubdomain {
        hostname: "noisypigeon.pigeon.dev".into(),
        zone_id: Some("zp".into()),
        worker: "pigeon-sites".into(),
        service: None,
        upstream: None,
    }];
    let up = upstream_for("noisypigeon", "np");
    assert_eq!(up, "https://noisypigeon.np.workers.dev");
    assert!(matches!(plan(&inst, &sub, &l).last(),
        Some(Step::AttachSubdomain { upstream, worker, .. })
            if *upstream == up && worker == "pigeon-sites"));
    sub.subdomains[0].service = Some("pigeon-sites".into());
    sub.subdomains[0].upstream = Some(up.clone());
    assert!(matches!(
        plan(&inst, &sub, &l).last(),
        Some(Step::Ok { .. })
    ));
    sub.subdomains[0].service = Some("someone-else".into());
    assert!(plan(&inst, &sub, &l).last().unwrap().stops());
    let mut no_dev = sub.clone();
    no_dev.account_subdomain = None;
    no_dev.subdomains[0].service = None;
    assert!(
        plan(&inst, &no_dev, &l)
            .iter()
            .any(|s| matches!(s, Step::Blocked { .. }))
    );

    // The digest binds the steps and the account.
    let d1 = digest(&inst, &"a".repeat(32), &first);
    assert_ne!(d1, digest(&inst, &"a".repeat(32), &again));
    assert_ne!(d1, digest(&inst, &"b".repeat(32), &first));
    assert_eq!(d1, digest(&inst, &"a".repeat(32), &first));
}

// ---- blog: a consumer repository rendered with blog@1 ----

/// The blog renderer (`pigeon_blog`): a consumer repository in, a static site out, with
/// Zola's URLs (posts, pages, alias redirects, tags, feed, sitemap), root-relative links, every
/// problem reported at once, and the same bytes from the same input.
#[test]
fn blog_renders_a_consumer_repo() {
    use pigeon_blog::{Input, render};

    let site_toml = r#"
template = "blog@1"
features = ["math", "no-commit"]

[site]
title = "Example <Blog>"
description = "Notes."
intro = "Hi, I'm **Example**."
avatar = "/assets/a.png"
author_page = "/pages/about"
alternate_names = ["Ex"]

[[site.nav]]
title = "About"
link = "/pages/about"

[[site.nav]]
title = "Elsewhere"
link = "https://example.org/me"

[[site.icons]]
src = "/assets/i.png"
sizes = "192x192"
type = "image/png"
"#;
    let post = |date: &str, extra: &str, body: &str| {
        format!("+++\ntitle = \"Post {date}\"\ndate = {date}T10:00:00+00:00\n{extra}+++\n{body}\n")
    };
    let mut statics: std::collections::BTreeSet<String> = [
        "assets/a.png",
        "assets/i.png",
        "assets/images/site/favicon-32x32.png",
        "assets/images/site/favicon-16x16.png",
        "assets/images/site/apple-touch-icon.png",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let input = Input {
        site_toml: site_toml.into(),
        posts: vec![
            (
                "2024-05-01-older.md".into(),
                post(
                    "2024-05-01",
                    "[taxonomies]\ntags = [\"Travel Notes\"]\n",
                    "Old.",
                ),
            ),
            (
                "2025-02-03-newer.md".into(),
                post(
                    "2025-02-03",
                    "aliases = [\"changelog/newer\"]\n[taxonomies]\ntags = [\"Travel Notes\"]\n",
                    "## Hello\n\n## Hello\n\n[up](#hello)\n\n```rust\nlet a = 1 < 2;\n```\n",
                ),
            ),
        ],
        pages: vec![(
            "about.md".into(),
            "+++\ntitle = \"About\"\naliases = [\"pages/names\"]\n+++\nMe.\n".into(),
        )],
        statics: statics.clone(),
        base_url: "https://example.com/".into(),
        commit: "abc1234".into(),
        commit_year: 2026,
    };
    let out = render(&input).expect("renders");
    assert_eq!((out.template, out.version), ("blog@1", "1.0.0"));
    assert_eq!(out.digest.len(), 64);
    assert_eq!(
        render(&input).unwrap(),
        out,
        "the same input renders the same bytes"
    );

    let file = |p: &str| String::from_utf8(out.files[p].0.clone()).unwrap();
    for p in [
        "index.html",
        "posts/newer/index.html",
        "posts/older/index.html",
        "pages/about/index.html",
        "pages/names/index.html",
        "changelog/newer/index.html",
        "tags/index.html",
        "tags/travel-notes/index.html",
        "posts/posts_feed.xml",
        "sitemap.xml",
        "robots.txt",
        "manifest.json",
        "404.html",
        "assets/css/main.css",
        "assets/css/tokens.css",
        "assets/css/pigeon.css",
    ] {
        assert!(out.files.contains_key(p), "{p}");
    }
    assert_eq!(out.statics.len(), statics.len());

    // The home page: newest first by year, the intro, escaped text, the pigeon look, JSON-LD.
    let home = file("index.html");
    let (newer, older) = (
        home.find("&#x2F;posts&#x2F;newer&#x2F;").unwrap(),
        home.find("&#x2F;posts&#x2F;older&#x2F;").unwrap(),
    );
    assert!(newer < older && home.find(">2025<").unwrap() < home.find(">2024<").unwrap());
    assert!(home.contains("<p>Hi, I'm <strong>Example</strong>.</p>"));
    assert!(home.contains("<title>Example &lt;Blog&gt; — Notes.</title>"));
    assert!(home.contains("color-theme-pigeon") && home.contains("<h1 class=\"m-0\">"));
    assert!(
        home.contains("\"alternateName\": [\"Ex\"]")
            && home.contains("\"sameAs\": [\"https://example.org/me\"]")
    );
    assert!(
        home.contains("\"name\": \"Example \\u003cBlog>\""),
        "JSON-LD escapes <"
    );
    assert!(
        home.contains("katex@0.16.22") && !home.contains("abc1234"),
        "math on, commit off"
    );

    // A post: Zola's heading ids, root-relative anchors, code blocks, absolute canonical URL.
    let p = file("posts/newer/index.html");
    assert!(
        p.contains("<h2 id=\"hello\">Hello</h2>") && p.contains("<h2 id=\"hello-1\">Hello</h2>")
    );
    assert!(p.contains("<a href=\"/posts/newer/#hello\">up</a>"));
    assert!(p.contains("<pre><code data-lang=\"rust\">let a = 1 &lt; 2;\n</code></pre>"));
    assert!(p.contains("<link rel=\"canonical\" href=\"https:&#x2F;&#x2F;example.com&#x2F;posts&#x2F;newer&#x2F;\">"));
    assert!(
        p.contains("href=\"&#x2F;pages&#x2F;about\">Example &lt;Blog&gt;</a>"),
        "byline"
    );
    assert!(p.contains("2025 February 3"));

    // Aliases redirect, root-relative; tags list their posts.
    assert!(file("pages/names/index.html").contains("const target = \"/pages/about/\";"));
    let tag = file("tags/travel-notes/index.html");
    assert!(
        tag.contains("Tag: Travel Notes") && tag.contains("href=\"&#x2F;posts&#x2F;older&#x2F;\"")
    );
    assert!(tag.contains("content=\"Posts tagged &quot;Travel Notes&quot;.\""));

    // Feed, sitemap, robots, manifest.
    let feed = file("posts/posts_feed.xml");
    assert!(feed.contains("<updated>2025-02-03T10:00:00+00:00</updated>"));
    assert!(feed.find("/posts/newer/").unwrap() < feed.find("/posts/older/").unwrap());
    let map = file("sitemap.xml");
    assert!(map.contains("<loc>https://example.com/posts/newer/</loc>\n        <lastmod>2025-02-03T10:00:00Z</lastmod>"));
    assert!(
        map.contains("<loc>https://example.com/tags/travel-notes/</loc>") && !map.contains("names")
    );
    assert_eq!(
        file("robots.txt"),
        "User-agent: *\nAllow: /\n\nSitemap: https://example.com/sitemap.xml\n"
    );
    assert!(file("manifest.json").contains("\"type\": \"image/png\""));

    // Everything wrong is reported at once, by file, and nothing renders.
    let mut bad = input.clone();
    bad.site_toml = site_toml.replace(
        "features = [\"math\", \"no-commit\"]",
        "features = [\"sparkles\"]\nlayout = \"x\"",
    );
    assert!(
        render(&bad).unwrap_err()[0]
            .message
            .contains("unknown field `layout`")
    );
    bad.site_toml = site_toml.replace("\"math\", ", "\"sparkles\", ");
    bad.posts
        .push(("2025-01-01-mismatch.md".into(), post("2025-01-02", "", "x")));
    bad.posts
        .push(("Not Dated.md".into(), post("2025-01-02", "", "x")));
    bad.posts.push((
        "2025-03-03-taken.md".into(),
        post("2025-03-03", "aliases = [\"pages/names\"]\n", "x"),
    ));
    bad.pages.push((
        "extra.md".into(),
        "+++\ntitle = \"X\"\ncolour = \"red\"\n+++\n".into(),
    ));
    statics.remove("assets/a.png");
    statics.insert("robots.txt".into());
    bad.statics = statics;
    let problems: Vec<String> = render(&bad)
        .unwrap_err()
        .iter()
        .map(ToString::to_string)
        .collect();
    let has = |file: &str, words: &str| {
        problems
            .iter()
            .any(|p| p.starts_with(file) && p.contains(words))
    };
    assert!(
        has(".pigeon/site.toml", "`sparkles` is not a feature"),
        "{problems:#?}"
    );
    assert!(has(
        "content/posts/2025-01-01-mismatch.md",
        "is prefixed 2025-01-01, but its date is 2025-01-02"
    ));
    assert!(has("content/posts/Not Dated.md", "YYYY-MM-DD-slug.md"));
    assert!(has("content/pages/extra.md", "unknown field `colour`"));
    assert!(has(
        ".pigeon/site.toml",
        "avatar `/assets/a.png` is not under static/"
    ));
    assert!(has("static/robots.txt", "is generated by blog@1"));
    assert!(has("content/", "/pages/names/ is also"), "{problems:#?}");
}

/// The sites deploy's pure parts: a repository tarball as GitHub serves one (a top folder, gzip)
/// read into its files, sorted into what the renderer reads, with the commit's year.
#[test]
fn sites_deploy_reads_a_repository_tarball() {
    let dir = std::env::temp_dir().join(format!("pigeon-tarball-{}", std::process::id()));
    let top = dir.join("noisypigeon-noisypigeon-2787e52");
    for d in [
        ".pigeon",
        "content/posts",
        "content/pages",
        "static/assets/img",
    ] {
        std::fs::create_dir_all(top.join(d)).unwrap();
    }
    std::fs::write(top.join(".pigeon/site.toml"), "template = \"blog@1\"\n").unwrap();
    std::fs::write(
        top.join("content/posts/2026-01-02-a.md"),
        "+++\ntitle = \"A\"\n+++\n",
    )
    .unwrap();
    std::fs::write(
        top.join("content/pages/about.md"),
        "+++\ntitle = \"About\"\n+++\n",
    )
    .unwrap();
    std::fs::write(top.join("static/assets/img/a.png"), [0u8, 1, 2]).unwrap();
    std::fs::write(top.join("README.md"), "hi").unwrap();
    let gz = dir.join("repo.tar.gz");
    let ok = Command::new("tar")
        .args(["--mtime=@1795315200", "-czf"])
        .arg(&gz)
        .arg("-C")
        .arg(&dir)
        .arg("noisypigeon-noisypigeon-2787e52")
        .status()
        .unwrap();
    assert!(ok.success());
    let archive = pigeon_core::tarball::read(&std::fs::read(&gz).unwrap()).unwrap();
    assert_eq!(pigeon_core::tarball::year_of(archive.mtime), 2026);
    assert_eq!(archive.files.len(), 5);
    let s = pigeon_blog::repo::split(archive.files).unwrap();
    assert_eq!(s.site_toml.as_deref(), Some("template = \"blog@1\"\n"));
    assert_eq!((s.posts.len(), s.pages.len()), (1, 1));
    assert_eq!(
        s.statics,
        vec![("assets/img/a.png".to_string(), vec![0u8, 1, 2])]
    );
    assert_eq!(pigeon_core::tarball::year_of(951_782_400), 2000);
    assert!(pigeon_core::tarball::read(b"not gzip").is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The email parser (`pigeon_email`): raw MIME in, a typed message and its blobs out. It covers
/// headers kept in order, decoded subject and names, addresses with groups flattened, the
/// bodies, attachments stored once by content, signals, a JSON round trip, and bad input
/// returned as an error.
#[test]
fn email_parses_a_multipart_message() {
    use pigeon_email::{ContentRef, Error, Message, Role, Source, parse};

    let pdf = b"%PDF-1.4 test";
    let names = [
        "Received",
        "Received",
        "Authentication-Results",
        "Message-ID",
        "In-Reply-To",
        "References",
        "List-Id",
        "List-Unsubscribe",
        "Date",
        "Subject",
        "From",
        "To",
        "Cc",
        "MIME-Version",
        "Content-Type",
    ];
    let sample = [
        "Received: from mx.example.net (mx.example.net [192.0.2.1])",
        "\tby mail.example.org with ESMTPS; Mon, 23 Nov 2026 10:15:05 +0100",
        "Received: from client.example.net by mx.example.net; Mon, 23 Nov 2026 10:15:02 +0100",
        "Authentication-Results: mail.example.org; spf=pass smtp.mailfrom=example.com;",
        " dkim=pass header.d=example.com; dmarc=pass header.from=example.com",
        "Message-ID: <msg-1@example.com>",
        "In-Reply-To: <prev-1@example.com>",
        "References: <root-1@example.com> <prev-1@example.com>",
        "List-Id: Billing news <news.example.com>",
        "List-Unsubscribe: <mailto:unsub@example.com>",
        "Date: Mon, 23 Nov 2026 10:15:00 +0100",
        "Subject: =?UTF-8?B?UmVjaG51bmcgZsO8ciBOb3ZlbWJlcg==?=",
        "From: =?UTF-8?B?QsO8cm8gQmlsbGluZw==?= <Billing@Example.COM>",
        "To: Ann <ann@example.org>, bob@example.org",
        "Cc: team: cy@example.org, Di <di@example.net>;",
        "MIME-Version: 1.0",
        "Content-Type: multipart/mixed; boundary=\"outer\"",
        "",
        "--outer",
        "Content-Type: multipart/alternative; boundary=\"alt\"",
        "",
        "--alt",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "Your invoice is attached.",
        "--alt",
        "Content-Type: text/html; charset=utf-8",
        "",
        "<p>Your invoice is attached.</p>",
        "--alt--",
        "--outer",
        "Content-Type: application/pdf; name=\"invoice.pdf\"",
        "Content-Disposition: attachment; filename=\"invoice.pdf\"",
        "Content-Transfer-Encoding: base64",
        "",
        "JVBERi0xLjQgdGVzdA==",
        "--outer",
        "Content-Type: image/png; name=\"logo.png\"",
        "Content-Disposition: inline; filename=\"logo.png\"",
        "Content-ID: <logo@x>",
        "Content-Transfer-Encoding: base64",
        "",
        "iVBORw0KGgo=",
        "--outer",
        "Content-Type: application/pdf; name=\"copy.pdf\"",
        "Content-Disposition: attachment; filename=\"copy.pdf\"",
        "Content-Transfer-Encoding: base64",
        "",
        "JVBERi0xLjQgdGVzdA==",
        "--outer--",
        "",
    ]
    .join("\r\n");
    let source = Source::EmailRouting {
        envelope_from: "bounce@example.com".into(),
        envelope_to: "Me@Example.ORG".into(),
    };
    let parsed = parse(sample.as_bytes(), source.clone()).unwrap();
    let m = &parsed.message;

    assert_eq!(m.schema, 1);
    assert_eq!(m.raw, ContentRef::of(sample.as_bytes()));
    let key = m.raw.key();
    assert_eq!(
        key,
        format!(
            "blobs/sha256/{}/{}/{}",
            &m.raw.sha256[0..2],
            &m.raw.sha256[2..4],
            m.raw.sha256
        )
    );
    assert_eq!(m.raw.sha256.len(), 64);

    // Every header is kept, in order; the folded Received is unfolded in `decoded`.
    let got: Vec<&str> = m.headers.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(got, names);
    assert!(m.headers[0].value.contains("\r\n\tby mail.example.org"));
    assert_eq!(
        m.headers[0].decoded.as_deref(),
        Some(
            "from mx.example.net (mx.example.net [192.0.2.1]) \
             by mail.example.org with ESMTPS; Mon, 23 Nov 2026 10:15:05 +0100"
        )
    );
    assert_eq!(m.headers[3].decoded, None);

    assert_eq!(m.subject.as_deref(), Some("Rechnung für November"));
    assert_eq!(m.date.as_deref(), Some("2026-11-23T09:15:00Z"));
    assert_eq!(m.message_id.as_deref(), Some("msg-1@example.com"));
    assert_eq!(m.in_reply_to, ["prev-1@example.com"]);
    assert_eq!(m.references, ["root-1@example.com", "prev-1@example.com"]);

    assert_eq!(m.from.len(), 1);
    assert_eq!(m.from[0].name.as_deref(), Some("Büro Billing"));
    assert_eq!(m.from[0].address, "Billing@Example.COM");
    assert_eq!(m.from[0].normalized, "billing@example.com");
    assert_eq!(m.from[0].domain, "example.com");
    assert_eq!(m.to.len(), 2);
    let cc: Vec<&str> = m.cc.iter().map(|a| a.normalized.as_str()).collect();
    assert_eq!(cc, ["cy@example.org", "di@example.net"]);
    let people = m.participants();
    for role in [Role::From, Role::To, Role::Cc] {
        assert!(people.iter().any(|(r, _)| *r == role));
    }
    assert!(
        people
            .iter()
            .any(|(r, a)| *r == Role::DeliveredTo && a.normalized == "me@example.org")
    );
    assert!(m.delivered_to.iter().any(|a| a.address == "Me@Example.ORG"));

    assert!(
        m.text
            .as_deref()
            .unwrap_or_default()
            .contains("Your invoice is attached.")
    );
    let html = m.html.as_ref().unwrap();
    let html_bytes = b"<p>Your invoice is attached.</p>";
    assert_eq!(html, &ContentRef::of(html_bytes));
    assert!(
        parsed
            .blobs
            .iter()
            .any(|(c, b)| c == html && b == html_bytes)
    );

    assert_eq!(m.attachments.len(), 3);
    let pdf_ref = ContentRef::of(pdf);
    let pdfs: Vec<_> = m
        .attachments
        .iter()
        .filter(|a| a.content_type == "application/pdf")
        .collect();
    assert_eq!(pdfs.len(), 2);
    assert!(pdfs.iter().all(|a| a.content == pdf_ref));
    assert_eq!(pdfs[0].filename.as_deref(), Some("invoice.pdf"));
    assert_eq!(pdfs[1].filename.as_deref(), Some("copy.pdf"));
    assert_eq!(
        parsed.blobs.iter().filter(|(c, _)| *c == pdf_ref).count(),
        1
    );
    // The HTML body, the PDF and the PNG: three distinct blobs.
    assert_eq!(parsed.blobs.len(), 3);
    let logo = m
        .attachments
        .iter()
        .find(|a| a.content_type == "image/png")
        .unwrap();
    assert_eq!(logo.content_id.as_deref(), Some("logo@x"));
    assert_eq!(logo.disposition.as_deref(), Some("inline"));

    let s = &m.taxonomy.signals;
    assert!(s.is_reply);
    assert!(!s.is_forward);
    assert!(s.has_attachments);
    assert_eq!(
        s.list_id.as_deref(),
        Some("Billing news <news.example.com>")
    );
    assert!(s.list_unsubscribe);
    for method in ["spf", "dkim", "dmarc"] {
        assert!(
            s.auth
                .iter()
                .any(|a| a.method == method && a.result == "pass")
        );
    }
    assert_eq!(m.taxonomy.doc_type, None);
    assert!(m.taxonomy.labels.is_empty());

    // The JSON round trip keeps the message, and parsing twice gives the same result.
    let json = serde_json::to_string(m).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, m);
    assert_eq!(parse(sample.as_bytes(), source).unwrap(), parsed);

    // Bad input is returned as an error, never a panic.
    assert_eq!(
        parse(b"", Source::Upload { name: None }).unwrap_err(),
        Error::Empty
    );
    // mail-parser finds no header section in binary junk, so parse returns NotMime.
    let junk = parse(b"\x00\x01 not mail", Source::Upload { name: None });
    assert!(matches!(junk, Err(Error::NotMime(_))));
}

/// The sandbox's prepare, retried after a failed try (overnight 2026-11-22, bug 2): the Worker's
/// setup empties what that try left in /work, files or a `.git` git cannot read, and keeps a
/// checkout that is there.
fn sandbox_prepare_retry() {
    use pigeon_ai_agents::sandbox;
    let base = std::env::temp_dir().join(format!("pigeon-smoke-sbprep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let origin = repo("sbprep-origin");
    std::fs::write(origin.join("a.txt"), "a\n").unwrap();
    git(&origin, &["add", "a.txt"], None);
    commit(&origin, "one");
    let sh = |cmd: &str, dir: &Path| {
        let out = Command::new("bash")
            .args(["-c", cmd])
            .current_dir(dir)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let empty = |what: &str| {
        assert_eq!(
            std::fs::read_dir(&work).unwrap().count(),
            0,
            "setup empties /work: {what}"
        );
    };
    // What a failed clone or checkout leaves: the checkout's files, no .git.
    std::fs::write(work.join("a.txt"), "left over\n").unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    sh(sandbox::PREPARE_SETUP, &work);
    empty("files");
    // A .git that git cannot read, as a repack still writing into a removed one leaves.
    std::fs::create_dir_all(work.join(".git/objects/pack")).unwrap();
    std::fs::write(work.join("a.txt"), "left over\n").unwrap();
    sh(sandbox::PREPARE_SETUP, &work);
    empty("a broken .git");
    // A checkout that is there already is kept.
    let url = format!("file://{}", origin.display());
    sh(
        &format!("git clone -q --no-tags -- {url} {}", work.display()),
        &base,
    );
    sh(sandbox::PREPARE_SETUP, &work);
    assert!(work.join("a.txt").exists() && work.join(".git").exists());
    // The first error is the one reported, with the last.
    assert_eq!(
        sandbox::not_ready(Some("git checkout failed: 403"), "git clone failed: exists"),
        "the sandbox did not get ready: git checkout failed: 403 (and on the last try: git clone failed: exists)"
    );
    assert_eq!(
        sandbox::not_ready(Some("x"), "x"),
        "the sandbox did not get ready: x"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn core_changelog_from_history() {
    use pigeon_capabilities::changelog::wants_event;
    use pigeon_core::changelog::{
        Entry, State, date_utc, landing_of, newest_days, render, subject, walk,
    };
    use pigeon_core::pack;
    use std::collections::BTreeMap;

    let dir = repo("changelog");
    // Commits at fixed times: two on 2026-11-21 UTC (the second at 00:30 +02:00 on the 22nd,
    // still the 21st in UTC), then two on 2026-11-22 UTC.
    let at = |message: &str, date: &str| -> String {
        std::fs::write(dir.join("f"), message).unwrap();
        git(&dir, &["add", "f"], None);
        let ok = Command::new("git")
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(ok.success());
        String::from_utf8(git(&dir, &["rev-parse", "HEAD"], None))
            .unwrap()
            .trim()
            .to_string()
    };
    let a = at("first\n\nbody", "2026-11-21T09:00:00+00:00");
    let b = at("second", "2026-11-22T00:30:00+02:00");
    let c = at("third", "2026-11-22T08:00:00+00:00");
    let d = at("fourth", "2026-11-22T20:00:00+00:00");
    let all = |tip: &str| {
        pack::objects(&git(
            &dir,
            &["pack-objects", "--stdout", "--revs", "-q"],
            Some(&format!("{tip}\n")),
        ))
        .unwrap()
    };

    // Backfill: every commit, newest first, with its subject and committer time.
    let full = walk(&all(&d), &d, None).unwrap();
    let shas: Vec<&str> = full.iter().map(|e| e.sha.as_str()).collect();
    assert_eq!(shas, [d.as_str(), c.as_str(), b.as_str(), a.as_str()]);
    assert_eq!(full[3].subject, "first");
    assert_eq!(date_utc(full[2].unix), "2026-11-21");
    // Incremental: only what is new since the last processed commit, which joins the old
    // entries into the full list; a stop not on the history is an error.
    let old = walk(&all(&b), &b, None).unwrap();
    let since = pack::objects(&pack(&dir, &d, &b)).unwrap();
    let mut new = walk(&since, &d, Some(&b)).unwrap();
    assert_eq!(new.len(), 2);
    new.extend(old);
    assert_eq!(new, full);
    assert!(walk(&all(&d), &d, Some(&"0".repeat(40))).is_err());
    assert!(
        walk(&since, &d, None).is_err(),
        "a missing commit is an error"
    );

    // The render: dates newest first, short ids, a landing id only where a note names it.
    let mut landings = BTreeMap::new();
    landings.insert(d.clone(), "L1".to_string());
    let md = render(&full, &landings, None);
    let day22 = md.find("## 2026-11-22").unwrap();
    let day21 = md.find("## 2026-11-21").unwrap();
    assert!(md.starts_with("# Changelog of main\n") && day22 < day21);
    assert!(md.contains(&format!("- fourth (`{}`, landing `L1`)\n", &d[..7])));
    assert!(md.contains(&format!("- third (`{}`)\n", &c[..7])));
    assert!(md.find("- fourth").unwrap() < md.find("- third").unwrap());
    assert!(md[day21..].contains("- second") && md[day21..].contains("- first"));
    let one = render(&full, &landings, Some(1));
    assert!(one.contains("## 2026-11-22") && !one.contains("## 2026-11-21"));
    assert_eq!(newest_days(&md, 1).trim_end(), one.trim_end());
    assert_eq!(newest_days(&md, 5), md);

    // Notes, dates and subjects.
    assert_eq!(
        landing_of("landing L1 state=landed by=x test=- deploy=- approved_by=- at=t\n"),
        Some("L1".to_string())
    );
    assert_eq!(landing_of("junk"), None);
    assert_eq!(landing_of("landing state=x"), None);
    assert_eq!(date_utc(0), "1970-01-01");
    assert_eq!(date_utc(1_709_164_800), "2024-02-29");
    assert_eq!(date_utc(-1), "1969-12-31");
    let _ = date_utc(i64::MIN);
    assert_eq!(subject("  \n"), "(no subject)");
    assert_eq!(subject(&"x".repeat(150)).chars().count(), 101);
    let state = State {
        version: 1,
        main: d.clone(),
        entries: vec![Entry {
            sha: d.clone(),
            unix: 1,
            subject: "s".into(),
        }],
    };
    let back: State = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    assert_eq!(back, state);

    // It follows pushes to main and the landing notes of its repository, nothing else.
    let push = |r: &str| json!({ "payload": { "ref": r, "repo": "pigeon" } });
    assert!(wants_event("pigeon", "main", &push("refs/heads/main")));
    assert!(wants_event("pigeon", "main", &push("refs/notes/land")));
    assert!(!wants_event("pigeon", "main", &push("refs/heads/claude/x")));
    assert!(!wants_event(
        "pigeon",
        "main",
        &push("refs/notes/changelog")
    ));
    assert!(!wants_event(
        "pigeon",
        "main",
        &json!({ "payload": { "ref": "refs/heads/main", "repo": "other" } })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The architecture model draws one Worker for each entry of ci/workers.toml, each from its own
/// wrangler.toml, with its tools, Durable Object ownership, bindings and externals, and nothing
/// secret. Every Worker of the map is in the model and in the drawing.
#[test]
fn core_architecture_draws_every_worker_in_the_map() {
    use pigeon_capabilities::architecture::{
        Architecture, Source, Wrangler, built, describe, render,
    };
    use pigeon_capabilities::public_host::{PublicHost, Retire};
    use pigeon_capabilities::remote::{Binding, Remote};
    use pigeon_capabilities::{Capability, Hosted};
    use pigeon_contract::CapabilityManifest;

    // The repository's files, as core embeds them: the map, each Worker's wrangler.toml and Rust.
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !root.join("ci/workers.toml").exists() {
        assert!(root.pop(), "ci/workers.toml is above the checkout");
    }
    fn rust(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                rust(&p, root, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let map_text = std::fs::read_to_string(root.join("ci/workers.toml")).unwrap();
    let map = pigeon_core::lanes::parse(&map_text).expect("the map parses");
    let mut files: Vec<(String, String)> = vec![("ci/workers.toml".into(), map_text.clone())];
    for w in &map.workers {
        let toml = std::fs::read_to_string(root.join(&w.dir).join("wrangler.toml"))
            .unwrap_or_else(|_| panic!("{} has a wrangler.toml", w.dir));
        files.push((format!("{}/wrangler.toml", w.dir), toml));
    }
    // Every crate's Rust, as core's build.rs embeds it: a Worker is built from the libraries it
    // links too.
    let mut crates: Vec<PathBuf> = std::fs::read_dir(root.join("src"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    crates.sort();
    for krate in crates {
        rust(&krate.join("src"), &root, &mut files);
    }
    let sources: Vec<Source> = files
        .iter()
        .map(|(path, text)| Source { path, text })
        .collect();
    let paths: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();

    // The capabilities core hosts: its own, and the capability Workers' from their manifests.
    let remote =
        |binding: &'static str, worker: &'static str, manifest: Value| -> Box<dyn Capability> {
            let spec: CapabilityManifest = serde_json::from_value(manifest).unwrap();
            Box::new(Remote::new(&Binding { binding, worker }, spec))
        };
    let tool = |name: &str| json!({ "name": name, "description": "A tool.", "inputSchema": { "type": "object" } });
    let proxy_host = PublicHost {
        zone: "pigeon.dev".into(),
        host: "proxy.pigeon.dev".into(),
        worker: "pigeon-proxy".into(),
        also: vec!["pigeon-platform".into()],
        retire: Retire::default(),
        tool: "setup_proxy_host",
    };
    let capabilities: Vec<Box<dyn Capability>> = vec![
        Box::new(Architecture),
        Box::new(proxy_host),
        remote(
            "MODELS",
            "pigeon-model-gateway",
            json!({
                "name": "models",
                "tools": [tool("models_submit")],
                "footprint": { "gateways": ["pigeon"], "externals": [
                    { "name": "AI Gateway", "kind": "model gateway", "detail": "gateway pigeon" }
                ] }
            }),
        ),
        remote(
            "CI",
            "pigeon-ci-gateway",
            json!({
                "name": "ci",
                "tools": [tool("ci_status")],
                "footprint": { "queues": ["pigeon-ci-events"], "externals": [
                    { "name": "Buildkite", "kind": "CI connector", "detail": "cluster pigeon" }
                ] }
            }),
        ),
        remote(
            "SITES",
            "pigeon-sites",
            json!({ "name": "sites", "tools": [tool("site_render")] }),
        ),
        remote(
            "AI_AGENTS",
            "pigeon-ai-agents",
            json!({ "name": "ai_agents", "tools": [tool("ai_agents_start")], "footprint": null }),
        ),
    ];
    let hosted = Hosted {
        worker: "pigeon-platform",
        capabilities: &capabilities,
        sources: &paths,
    };
    let described = describe(&sources, &hosted);
    let drawing = render(&described);
    let model = serde_json::to_value(&described).unwrap();

    // One Worker per entry of the map, in its order, each in the drawing.
    let all = model["workers"].as_array().unwrap();
    let drawn: Vec<&str> = all.iter().map(|w| w["name"].as_str().unwrap()).collect();
    let scripts: Vec<&str> = map.workers.iter().map(|w| w.script.as_str()).collect();
    assert_eq!(drawn, scripts, "every Worker in ci/workers.toml, once");
    assert!(
        drawn.contains(&"pigeon-proxy"),
        "the proxy Worker has no binding from core"
    );
    for script in &scripts {
        assert!(drawing.contains(script), "{script} is in the drawing");
    }
    let worker = |name: &str| -> Value {
        all.iter()
            .find(|w| w["name"] == name)
            .unwrap_or_else(|| panic!("{name} is drawn"))
            .clone()
    };
    let strs = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };

    // Tools are the owning Worker's; core merges them into /mcp.
    assert_eq!(
        strs(&worker("pigeon-model-gateway")["tools"]),
        ["models_submit"]
    );
    assert_eq!(strs(&worker("pigeon-ci-gateway")["tools"]), ["ci_status"]);
    assert_eq!(strs(&worker("pigeon-sites")["tools"]), ["site_render"]);
    assert_eq!(
        worker("pigeon-model-gateway")["merged_into"],
        "pigeon-platform"
    );
    let core_tools = strs(&worker("pigeon-platform")["tools"]);
    assert!(core_tools.contains(&"architecture_describe".into()));
    assert!(!core_tools.contains(&"models_submit".into()));
    assert_eq!(worker("pigeon-ci-gateway")["lane"], "ci-gateway");
    assert_eq!(
        worker("pigeon-ci-gateway")["deploy_job"],
        "deploy-ci-gateway"
    );

    // Durable Objects: each class under its host; a binding with `script_name` only reaches it.
    let owner = |w: &str, binding: &str| {
        worker(w)["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["binding"] == binding)
            .map(|o| o["owner"].as_str().unwrap().to_string())
    };
    for (w, binding, host) in [
        ("pigeon-platform", "CI_SCHEDULER", "pigeon-ci-gateway"),
        ("pigeon-platform", "MODEL_JOBS", "pigeon-model-gateway"),
        ("pigeon-platform", "CI_RUNNER", "pigeon-platform"),
        ("pigeon-ci-gateway", "CI_RUNNER", "pigeon-platform"),
        ("pigeon-ai-agents", "AI_AGENT_RUNS", "pigeon-ai-agents"),
    ] {
        assert_eq!(owner(w, binding).as_deref(), Some(host), "{w} {binding}");
    }
    // Core's declared bindings are all used somewhere it is built from.
    let unused = strs(&model["resources"]["unused"]);
    for linked in ["CI_RUNNER", "CI_SCHEDULER", "DIRECTORY", "LANDER"] {
        assert!(
            !unused.contains(&linked.to_string()),
            "{linked} is not unused: {unused:?}"
        );
    }
    // Whether a binding is used is read from every crate the Worker is built from, as its map
    // entry lists them: one named only in a library core links is used; one named only in a
    // crate core does not link is not.
    {
        let map = "version = 1\n[docs]\npaths = []\n[shared]\npaths = []\n[[worker]]\nname = \"core\"\nscript = \"pigeon-platform\"\ndir = \"src/platform\"\npaths = [\"src/platform/*\", \"src/lib/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy\"\n";
        let toml = "name = \"pigeon-platform\"\n\n[[secrets_store_secrets]]\nbinding = \"IN_LIB\"\nstore_id = \"s\"\nsecret_name = \"IN_LIB\"\n\n[[secrets_store_secrets]]\nbinding = \"ELSEWHERE\"\nstore_id = \"s\"\nsecret_name = \"ELSEWHERE\"\n";
        let files = [
            ("ci/workers.toml", map),
            ("src/platform/wrangler.toml", toml),
            ("src/platform/src/lib.rs", "pub fn serve() {}\n"),
            ("src/lib/src/lib.rs", "pub const KEY: &str = \"IN_LIB\";\n"),
            (
                "src/other/src/lib.rs",
                "pub const KEY: &str = \"ELSEWHERE\";\n",
            ),
        ];
        let sources: Vec<Source> = files
            .iter()
            .map(|(path, text)| Source { path, text })
            .collect();
        let none: Vec<Box<dyn Capability>> = Vec::new();
        let hosted = Hosted {
            worker: "pigeon-platform",
            capabilities: &none,
            sources: &files,
        };
        let model = serde_json::to_value(describe(&sources, &hosted)).unwrap();
        let unused = strs(&model["resources"]["unused"]);
        assert!(!unused.contains(&"IN_LIB".to_string()), "{unused:?}");
        assert!(unused.contains(&"ELSEWHERE".to_string()), "{unused:?}");
    }
    let objects = strs(&model["resources"]["objects"]);
    for want in [
        "CI_SCHEDULER (CiScheduler)",
        "MODEL_JOBS (ModelJobs)",
        "AI_AGENT_RUNS (AiAgentRuns)",
        "SANDBOXES (Sandbox)",
    ] {
        assert!(objects.iter().any(|o| o == want), "{want} is drawn");
    }
    assert_eq!(
        objects
            .iter()
            .filter(|o| o.starts_with("CI_RUNNER"))
            .count(),
        1,
        "CiRunner is drawn once, under core"
    );
    assert!(
        strs(&model["resources"]["containers"])
            .iter()
            .any(|c| c.starts_with("Sandbox (sandbox: pigeon-ai-agent-sandbox@sha256:"))
    );
    assert!(strs(&worker("pigeon-ai-agents")["containers"]).contains(&"Sandbox".into()));

    // Links: core's bindings, each other Worker's PLATFORM, and the cross-Worker objects.
    let links: Vec<(&str, &str, &str)> = model["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["from"].as_str().unwrap(),
                l["to"].as_str().unwrap(),
                l["via"].as_str().unwrap(),
            )
        })
        .collect();
    let has = |from: &str, to: &str, via: &str| links.contains(&(from, to, via));
    for (binding, to) in [
        ("SITES", "pigeon-sites"),
        ("MODELS", "pigeon-model-gateway"),
        ("CI", "pigeon-ci-gateway"),
        ("AI_AGENTS", "pigeon-ai-agents"),
    ] {
        assert!(has("pigeon-platform", to, binding), "core binds {binding}");
    }
    for from in [
        "pigeon-sites",
        "pigeon-model-gateway",
        "pigeon-ci-gateway",
        "pigeon-proxy",
        "pigeon-ai-agents",
    ] {
        assert!(has(from, "pigeon-platform", "PLATFORM"), "{from}: PLATFORM");
    }
    assert!(has(
        "pigeon-platform",
        "pigeon-ci-gateway",
        "CI_SCHEDULER (CiScheduler)"
    ));
    assert!(has(
        "pigeon-ci-gateway",
        "pigeon-platform",
        "CI_RUNNER (CiRunner)"
    ));

    // Queues, secrets, the proxy host and externals.
    let queues = worker("pigeon-ci-gateway")["queues"].to_string();
    assert!(queues.contains("pigeon-ci-events") && queues.contains("produces"));
    assert!(strs(&worker("pigeon-sites")["secrets"]).contains(&"CALLER_KEY".into()));
    assert!(strs(&worker("pigeon-ci-gateway")["secrets"]).contains(&"BUILDKITE_API_TOKEN".into()));
    assert!(!strs(&worker("pigeon-sites")["secrets"]).contains(&"CF_PARENT_TOKEN".into()));
    assert!(strs(&worker("pigeon-platform")["secrets"]).contains(&"CF_PARENT_TOKEN".into()));
    let host = model["hostnames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "proxy.pigeon.dev")
        .unwrap();
    assert_eq!(host["worker"], "pigeon-proxy");
    let externals: Vec<&str> = model["externals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(externals, ["AI Gateway", "Buildkite"]);
    assert!(drawing.contains("Buildkite"));

    // Nothing secret or personal: not the account, the Secrets Store, or the App's client id.
    let core =
        Wrangler::read(&std::fs::read_to_string(root.join("src/platform/wrangler.toml")).unwrap());
    let text = format!("{model}{drawing}");
    for var in [
        "CF_ACCOUNT_ID",
        "CF_SECRETS_STORE_ID",
        "GITHUB_APP_CLIENT_ID",
        "GITHUB_APP_ID",
    ] {
        assert!(
            !text.contains(core.vars[var].as_str()),
            "{var} is not drawn"
        );
    }

    // Without the map, the hosting Worker is still drawn, alone.
    let alone: Vec<Source> = sources
        .iter()
        .filter(|s| s.path != "ci/workers.toml")
        .map(|s| Source {
            path: s.path,
            text: s.text,
        })
        .collect();
    let fallback = serde_json::to_value(describe(&alone, &hosted)).unwrap();
    assert_eq!(fallback["workers"].as_array().unwrap().len(), 1);

    // The memo is kept while the capabilities stay, and rebuilt when they change.
    let first = built(&hosted);
    assert!(std::ptr::eq(first, built(&hosted)), "kept while unchanged");
    let fewer: Vec<Box<dyn Capability>> = vec![Box::new(Architecture)];
    let smaller = Hosted {
        worker: "pigeon-platform",
        capabilities: &fewer,
        sources: &paths,
    };
    let second = built(&smaller);
    assert!(
        !std::ptr::eq(first, second),
        "rebuilt for other capabilities"
    );
    assert!(first.html.contains("ci_status") || first.model.to_string().contains("ci_status"));
    assert!(!second.model.to_string().contains("ci_status"));
}

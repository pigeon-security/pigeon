//! Landing: checks, deploy lanes, rebases, attribution and the lane map.

use super::*;

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
        pigeon_proxies::git_proxy::moves_forward(&main_at, &tip, &pack_fwd),
        Ok(true)
    );
    assert_eq!(
        pigeon_proxies::git_proxy::moves_forward(&tip, &main_at, &pack(&dir, &main_at, &tip)),
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
fn core_land_first_deploy_incident_text() {
    use pigeon_capabilities::land::lanes::{self, Included, Next};
    let first = "first deploy of sites failed; nothing was running and nothing is rolled back";
    let failing = "sites may still be on a failing build: a person must look";
    assert_eq!(lanes::not_on_good_text("sites", false), first);
    assert_eq!(lanes::not_on_good_text("sites", true), failing);
    assert_eq!(lanes::first_deploy_text("sites"), first);
    let one = [Included {
        id: "L5".into(),
        merged_at: 5,
        commit: "c5".into(),
    }];
    // No good build ever: the verdict carries the first-deploy text.
    let Next::Done(v) = lanes::bisect("sites", None, &one, &[], 4) else {
        panic!("a lone landing is the cause at once");
    };
    assert!(v.summary.contains(first), "{}", v.summary);
    assert!(!v.summary.contains("failing build"));
    // A good build that could not be returned to keeps the old text.
    let g = lanes::Good {
        commit: "c3".into(),
        covers: 3,
    };
    let Next::Done(v) = lanes::bisect("sites", Some(&g), &one, &[lanes::Tried::Error], 4) else {
        panic!("an unrunnable rollback ends the incident");
    };
    assert!(v.summary.contains(failing), "{}", v.summary);
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
        proxy_first: false,
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
fn core_land_resolves_the_deploy_map_from_main() {
    use pigeon_capabilities::land::lanes::{MapUsed, grant_script, resolve_entry};
    let toml = |script: &str| {
        format!(
            "version = 1\n[docs]\npaths = [\"docs/*\"]\n[shared]\npaths = []\n\
             [[worker]]\nname = \"core\"\nscript = \"pigeon-platform\"\ndir = \"src/platform\"\n\
             paths = [\"src/platform/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy\"\n\
             [[worker]]\nname = \"new\"\nscript = \"{script}\"\ndir = \"src/new\"\n\
             paths = [\"*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy\"\n"
        )
    };
    let compiled = pigeon_core::lanes::parse(&toml("old-script")).unwrap();
    // The commit's map wins when it has the Worker.
    let (w, used, why) =
        resolve_entry(Ok(Some(toml("new-script"))), "new", &compiled).expect("an entry");
    assert_eq!((w.script.as_str(), used), ("new-script", MapUsed::Commit));
    assert!(why.contains("commit"));
    // A missing file, a read error, a parse error or no entry falls back, and says so.
    let only_core = toml("x").replace("name = \"new\"", "name = \"other\"");
    for text in [
        Ok(None),
        Err("boom".to_string()),
        Ok(Some("not toml [".to_string())),
        Ok(Some(only_core)),
    ] {
        let (w, used, why) = resolve_entry(text, "new", &compiled).expect("an entry");
        assert_eq!((w.script.as_str(), used), ("old-script", MapUsed::Compiled));
        assert!(why.starts_with("compiled map"));
    }
    // Neither map has the Worker.
    assert!(resolve_entry(Ok(None), "ghost", &compiled).is_none());
    // The grant answers the recorded script, else the compiled one.
    assert_eq!(grant_script(Some("a"), Some("b")).as_deref(), Some("a"));
    assert_eq!(grant_script(None, Some("b")).as_deref(), Some("b"));
    assert_eq!(grant_script(None, None), None);
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
        proxy_first: false,
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
fn core_land_deploys_the_proxy_first_for_allowlist_changes() {
    use pigeon_capabilities::land::lanes::{self, Deploy, DeployState, Merged};
    let l = |ordered: bool, proxy_first: bool| Merged {
        id: "P".into(),
        merged_at: 1,
        commit: "c1".into(),
        workers: vec!["core".into(), "proxy".into(), "sites".into()],
        ordered,
        proxy_first,
    };
    let d = |worker: &str, state: DeployState| Deploy {
        worker: worker.into(),
        covers: 1,
        state,
        incident: false,
    };
    let go = |l: &Merged, deploys: &[Deploy]| {
        lanes::starts("core", std::slice::from_ref(l), deploys, &[])
            .into_iter()
            .map(|s| s.worker)
            .collect::<Vec<_>>()
    };
    assert!(lanes::changes_proxy_first(&[
        "a",
        "src/proxies/src/cf_proxy.rs"
    ]));
    assert!(!lanes::changes_proxy_first(&["src/proxies/src/other.rs"]));

    // Proxy first alone: the proxy deploys, then the rest together.
    let p = l(false, true);
    assert_eq!(go(&p, &[]), ["proxy"]);
    assert_eq!(
        go(&p, &[d("proxy", DeployState::Succeeded)]),
        ["core", "sites"]
    );

    // Core first and proxy first: core, then the proxy, then the rest.
    let both = l(true, true);
    assert_eq!(go(&both, &[]), ["core"]);
    let core_done = d("core", DeployState::Succeeded);
    assert_eq!(go(&both, std::slice::from_ref(&core_done)), ["proxy"]);
    let proxy_done = d("proxy", DeployState::Succeeded);
    assert_eq!(go(&both, &[core_done, proxy_done]), ["sites"]);

    // A failed proxy deploy releases the others.
    assert_eq!(
        go(&p, &[d("proxy", DeployState::Failed)]),
        ["core", "sites"]
    );

    // Old rows (ordered 0 or 1, no proxy bit) are unchanged, but no more than
    // MAX_PARALLEL_DEPLOYS start together: core first, then by name.
    assert_eq!(lanes::MAX_PARALLEL_DEPLOYS, 2);
    assert_eq!(go(&l(false, false), &[]), ["core", "proxy"]);
    // A running deploy takes a slot; a finished one frees it.
    assert_eq!(
        go(&l(false, false), &[d("core", DeployState::Running)]),
        ["proxy"]
    );
    assert_eq!(
        go(
            &l(false, false),
            &[
                d("core", DeployState::Succeeded),
                d("proxy", DeployState::Succeeded)
            ]
        ),
        ["sites"]
    );
    assert_eq!(go(&l(true, false), &[]), ["core"]);
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
        proxy_first: false,
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

#[test]
fn core_land_skips_the_rollback_when_nothing_uploaded() {
    use pigeon_capabilities::land::lanes::{
        self, Good, Included, Next, Purpose, REDEPLOYS, never_uploaded, skip_rollback,
    };
    // Only a failed job with exit code 3 never uploaded.
    assert!(never_uploaded("failed", Some(3)));
    assert!(!never_uploaded("failed", Some(1)));
    assert!(!never_uploaded("timed_out", Some(3)));
    assert!(!never_uploaded("error", Some(3)));
    assert!(!never_uploaded("cancelled", Some(3)));
    assert!(!never_uploaded("failed", None));
    assert_eq!(
        lanes::exit_code_in("w: CI job j: failed (exit code 3)"),
        Some(3)
    );
    assert_eq!(lanes::exit_code_in("w: CI job j: failed"), None);

    // The rollback is skipped only when last_good is the newest earlier deploy.
    assert!(skip_rollback(true, Some(4), Some(4)));
    assert!(!skip_rollback(true, Some(5), Some(4)));
    assert!(!skip_rollback(true, Some(4), None));
    assert!(!skip_rollback(true, None, None));
    assert!(!skip_rollback(false, Some(4), Some(4)));

    let included = |ids: &[i64]| -> Vec<Included> {
        ids.iter()
            .map(|at| Included {
                id: format!("L{at}"),
                commit: format!("c{at}"),
                merged_at: *at,
            })
            .collect()
    };
    let g3 = Good {
        commit: "c3".into(),
        covers: 3,
    };
    // Bisect goes straight to bisecting; one landing is the cause, nothing is redeployed.
    let two = included(&[5, 6]);
    match lanes::bisect_from("sites", Some(&g3), &two, &[], REDEPLOYS, true) {
        Next::Deploy {
            purpose, commit, ..
        } => {
            assert_eq!((purpose, commit.as_str()), (Purpose::Bisect, "c5"));
        }
        other => panic!("not a deploy: {other:?}"),
    }
    match lanes::bisect_from("sites", Some(&g3), &included(&[5]), &[], REDEPLOYS, true) {
        Next::Done(v) => {
            assert_eq!((v.guilty.as_deref(), v.redeploys), (Some("L5"), 0));
            assert_eq!(v.on, Some(g3.clone()));
        }
        other => panic!("not done: {other:?}"),
    }
    // Core ends at once, still on its good build.
    match lanes::core_incident_from(Some(&g3), None, "c7", &two, &[], true) {
        Next::Done(v) => {
            assert_eq!((v.on, v.redeploys), (Some(g3.clone()), 0));
            assert!(v.summary.contains("nothing was uploaded"));
        }
        other => panic!("not done: {other:?}"),
    }
    // Not skipped: the rollback comes first, as before.
    assert!(matches!(
        lanes::bisect_from("sites", Some(&g3), &two, &[], REDEPLOYS, false),
        Next::Deploy {
            purpose: Purpose::Rollback,
            ..
        }
    ));
}

#[test]
fn core_land_history_pack_keeps_only_commits() {
    use pigeon_core::pack;
    let dir = repo("history-pack");
    // Three commits, each with a 3000-byte blob: the pack inflates to over 9000 bytes.
    let mut ids = Vec::new();
    for (n, ch) in ["a", "b", "c"].iter().enumerate() {
        std::fs::write(dir.join("f"), ch.repeat(3000)).unwrap();
        git(&dir, &["add", "f"], None);
        git(&dir, &["commit", "-q", "-m", &format!("c{n}")], None);
        ids.push(String::from_utf8(git(&dir, &["rev-parse", "HEAD"], None)).unwrap());
    }
    let ids: Vec<String> = ids.iter().map(|i| i.trim().to_string()).collect();
    let (a, b, c) = (ids[0].clone(), ids[1].clone(), ids[2].clone());
    let all = String::from_utf8(git(&dir, &["rev-list", "--objects", &c], None)).unwrap();
    assert!(all.lines().count() >= 9, "commits, trees and blobs");
    let packed = git(
        &dir,
        &["pack-objects", "--stdout", "--revs", "-q"],
        Some(&format!("{c}\n")),
    );
    // Whole, it is over a 2000-byte cap; commits alone fit under it.
    let commits = pack::commit_parents_with(&packed, 2000).unwrap();
    assert_eq!(commits.parents.len(), 3);
    assert_eq!(commits.parents[&c], vec![b.clone()]);
    assert_eq!(commits.parents[&b], vec![a.clone()]);
    assert!(commits.parents[&a].is_empty());
    assert_eq!(commits.unresolved, 0);
    // A cap under the commits themselves is still refused.
    assert!(pack::commit_parents_with(&packed, 10).is_err());
    // One commit that declares 64 MiB (over the per-object cap) is refused before inflating.
    let mut big = b"PACK".to_vec();
    big.extend_from_slice(&2u32.to_be_bytes());
    big.extend_from_slice(&1u32.to_be_bytes());
    big.extend_from_slice(&[0x90, 0x80, 0x80, 0x80, 0x02]);
    big.extend_from_slice(&[0u8; 20]);
    assert!(pack::commit_parents(&big).is_err());
}

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
    let map = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
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
    assert!(map.worker("cloud-gateway").is_none());
    assert!(
        map.deploy_order(&map.classify(&["src/cloud-gateway/src/lib.rs"]))
            .is_empty()
    );
    assert!(map.worker("sites").unwrap().queues.is_empty());
    // A Worker's deploy scope: its queues and container classes, from the map.
    let ci = map.worker("ci-gateway").unwrap();
    assert_eq!(ci.queues, ["pigeon-ci-events"]);
    assert!(ci.containers.is_empty());
    let scoped = lanes::parse(&format!(
        "{}\n[[worker]]\nname = \"scoped\"\nscript = \"pigeon-scoped\"\ndir = \"src/scoped\"\npaths = [\"src/scoped/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy-scoped\"\nqueues = [\"pigeon-ci-events\"]\ncontainers = [\"CiRunner\"]\n",
        include_str!("../../ci/workers.toml")
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
            "ai-agents",
            "data-worker"
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
    use pigeon_proxies::cf_proxy::target_script;
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

#[test]
fn core_land_deletes_test_refs() {
    use pigeon_capabilities::land::merge::{test_ref, test_ref_id};

    assert_eq!(test_ref_id("refs/heads/claude/land/abc123"), Some("abc123"));
    assert_eq!(
        test_ref_id(&test_ref("0123456789abcdef")),
        Some("0123456789abcdef")
    );
    for refused in [
        "refs/heads/main",
        "refs/heads/claude/ai-agent-x",
        "refs/heads/claude/land",
        "refs/heads/claude/land/",
        "refs/tags/claude/land/abc",
        "claude/land/abc",
        "refs/heads/claude/land/abc/def",
        "refs/heads/claude/land/../main",
        "refs/heads/claude/lander/abc",
        "refs/notes/land",
    ] {
        assert_eq!(test_ref_id(refused), None, "{refused}");
    }
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
fn core_land_environment_failures_blame_no_landing() {
    use pigeon_capabilities::land::lanes::{
        self, EnvStep, environment_detail, environment_fault, environment_step,
    };
    // The runner lost, or the job cancelled: the environment, whatever the output.
    assert!(environment_fault("error", None, "").is_some());
    assert!(environment_fault("cancelled", None, "").is_some());
    // A failed job naming a known platform fault.
    let why = environment_fault("failed", Some(1), "x\nA request failed [code: 10007]\n");
    assert!(why.unwrap().contains("10007"));
    let why = environment_fault("failed", Some(1), "Cloudflare API returned HTTP 503\n");
    assert!(why.unwrap().contains("503"));
    assert!(environment_fault("failed", Some(1), "status: 502 Bad Gateway").is_some());
    assert!(environment_fault("failed", Some(1), "the proxy could not be reached").is_some());
    // Anything else is a code fault.
    assert_eq!(environment_fault("timed_out", None, "code: 10007"), None);
    assert_eq!(
        environment_fault("failed", Some(1), "error[E0308]: mismatched"),
        None
    );
    assert_eq!(environment_fault("failed", Some(1), ""), None);
    assert_eq!(
        environment_fault("failed", Some(1), "HTTP 5000 lines"),
        None
    );
    assert_eq!(environment_fault("failed", Some(1), "HTTP 404"), None);
    assert_eq!(environment_fault("succeeded", None, "code: 10007"), None);

    // A lane deploy is deployed once more; then, or in an incident, it ends in error.
    assert_eq!(environment_step(false, false, "w"), EnvStep::Redeploy);
    let end = EnvStep::Error(environment_detail("w"));
    assert_eq!(environment_step(true, false, "w"), end);
    assert_eq!(environment_step(false, true, "w"), end);
    assert_eq!(
        environment_detail("w"),
        "environment: w; not this landing's fault; land again"
    );
    assert!(lanes::is_environment_detail(&environment_detail("w")));
    assert!(!lanes::is_environment_detail("sites: CI job j: failed"));

    // Its incident is closed like an unisolated one.
    assert!(lanes::incident_covered(lanes::ENVIRONMENT, 5, 3, 9, 4));
    assert!(!lanes::incident_covered("isolated", 5, 3, 9, 4));
}

/// A landing that adds a Worker's entry classifies its paths to that Worker, never moves an
/// existing entry, is proxy-first, and deploys core, then the proxy, then the new Worker.
#[test]
fn core_land_adds_a_worker_in_one_landing() {
    use pigeon_capabilities::land::lanes::{self, Deploy, DeployState, Merged};
    let map = |extra: &str, core_paths: &str, proxy: bool| {
        let proxy = if proxy {
            "[[worker]]\nname = \"proxy\"\nscript = \"pigeon-proxy\"\ndir = \"src/proxy\"\n\
             paths = [\"src/proxy/*\", \"ci/workers.toml\"]\ntest_job = \"test\"\n\
             deploy_job = \"deploy-proxy\"\n"
        } else {
            ""
        };
        pigeon_core::lanes::parse(&format!(
            "version = 1\n[docs]\npaths = [\"docs/*\"]\n[shared]\npaths = [\"Cargo.toml\"]\n\
             [checks]\npaths = [\".pigeon/ci.toml\"]\n[[worker]]\nname = \"core\"\n\
             script = \"pigeon-platform\"\ndir = \"src/platform\"\npaths = [{core_paths}]\n\
             test_job = \"test\"\ndeploy_job = \"deploy\"\n{proxy}{extra}"
        ))
        .unwrap()
    };
    let new = "[[worker]]\nname = \"fresh\"\nscript = \"pigeon-fresh\"\ndir = \"src/fresh\"\n\
               paths = [\"src/fresh/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy-fresh\"\n";
    let core = "\"src/platform/*\", \"ci/workers.toml\"";
    let main = map("", core, true);
    // Main's map alone counts the new crate as shared.
    assert!(main.classify(&["src/fresh/lib.rs"]).shared);
    // An added entry classifies its paths to the new Worker.
    let (both, added) = lanes::with_added_workers(&main, &map(new, core, true));
    assert_eq!(added, vec!["fresh".to_string()]);
    let a = both.classify(&["src/fresh/lib.rs", "ci/workers.toml"]);
    assert!(!a.shared && a.ordered);
    assert!(a.workers.contains("fresh") && a.workers.contains("core"));
    assert!(a.workers.contains("proxy"));
    // A changed existing entry is ignored: core keeps its paths, and nothing is added.
    let changed = map(
        "",
        "\"src/platform/*\", \"ci/workers.toml\", \"src/fresh/*\"",
        true,
    );
    let (same, added) = lanes::with_added_workers(&main, &changed);
    assert!(added.is_empty());
    assert_eq!(same, main);
    assert!(same.classify(&["src/fresh/lib.rs"]).shared);
    // A removed entry does not drop main's.
    let (kept, added) = lanes::with_added_workers(&main, &map("", core, false));
    assert!(added.is_empty());
    assert!(kept.worker("proxy").is_some());
    // The landing is proxy-first, and the order is core, proxy, then the new Worker.
    let l = Merged {
        id: "L1".into(),
        merged_at: 1,
        commit: "c1".into(),
        workers: a.workers.iter().cloned().collect(),
        ordered: a.ordered,
        proxy_first: !added_for(&both, &main).is_empty(),
    };
    assert!(l.proxy_first);
    let ok = |w: &str| Deploy {
        worker: w.into(),
        covers: 1,
        state: DeployState::Succeeded,
        incident: false,
    };
    let names = |deploys: &[Deploy]| -> Vec<String> {
        lanes::starts("core", std::slice::from_ref(&l), deploys, &[])
            .into_iter()
            .map(|s| s.worker)
            .collect()
    };
    assert_eq!(names(&[]), ["core"]);
    assert_eq!(names(&[ok("core")]), ["proxy"]);
    assert_eq!(names(&[ok("core"), ok("proxy")]), ["fresh"]);
    assert!(names(&[ok("core"), ok("proxy"), ok("fresh")]).is_empty());

    /// The Workers `with` has that `main` lacks.
    fn added_for(with: &pigeon_core::lanes::Map, main: &pigeon_core::lanes::Map) -> Vec<String> {
        with.workers
            .iter()
            .filter(|w| main.worker(&w.name).is_none())
            .map(|w| w.name.clone())
            .collect()
    }
}

#[test]
fn land_portal_oauth_redirects_change_one_uri() {
    use pigeon_capabilities::portal_oauth::{check_uri, differences, plan, summary};

    let app = json!({
        "id": "app-1", "aud": "aud-1", "name": "portal", "type": "mcp_portal",
        "domain": "mcp.example.test",
        "created_at": "2025-01-01T00:00:00Z", "updated_at": "2025-01-02T00:00:00Z",
        "allowed_idps": ["idp-1"],
        "policies": [
            { "id": "pol-a", "name": "people", "precedence": 1, "reusable": true,
              "include": [{ "email": { "email": "owner@example.com" } }] },
            { "id": "pol-b", "name": "bots", "precedence": 2, "reusable": true }
        ],
        "oauth_configuration": {
            "enabled": true,
            "dynamic_client_registration": {
                "enabled": true, "allow_any_on_localhost": true,
                "allow_any_on_loopback": true,
                "allowed_uris": ["https://a.example.test/cb"]
            },
            "grant": { "access_token_lifetime": "1h", "session_duration": "24h" }
        }
    });
    let new = "https://b.example.test/cb";
    let planned = plan(&app, "add", new).unwrap();
    let body = planned.body.unwrap();
    assert_eq!(
        planned.after,
        vec!["https://a.example.test/cb".to_string(), new.to_string()]
    );
    for k in ["id", "aud", "created_at", "updated_at"] {
        assert!(body.get(k).is_none(), "{k} is dropped");
    }
    assert_eq!(
        body["policies"],
        json!([{ "id": "pol-a", "precedence": 1 }, { "id": "pol-b", "precedence": 2 }])
    );
    // Only allowed_uris differs from the read app.
    let mut expect = app.clone();
    for k in ["id", "aud", "created_at", "updated_at"] {
        expect.as_object_mut().unwrap().remove(k);
    }
    expect["policies"] = body["policies"].clone();
    expect["oauth_configuration"]["dynamic_client_registration"]["allowed_uris"] =
        body["oauth_configuration"]["dynamic_client_registration"]["allowed_uris"].clone();
    assert_eq!(body, expect);

    assert!(
        plan(&app, "add", "https://a.example.test/cb")
            .unwrap()
            .body
            .is_none()
    );
    assert!(plan(&app, "remove", new).unwrap().body.is_none());
    let removed = plan(&app, "remove", "https://a.example.test/cb").unwrap();
    assert!(removed.after.is_empty() && removed.body.is_some());

    assert!(check_uri(new).is_ok());
    for bad in [
        "https://x/*",
        "http://localhost/cb",
        "https%3A%2F%2Fx.example.test%2Fcb",
        "https://x.example.test/cb#frag",
    ] {
        assert!(check_uri(bad).is_err(), "{bad}");
    }

    assert!(!summary(&app).to_string().contains("owner@example.com"));

    let mut not_reusable = app.clone();
    not_reusable["policies"][1]["reusable"] = json!(false);
    assert!(plan(&not_reusable, "add", new).is_err());
    let mut dcr_off = app.clone();
    dcr_off["oauth_configuration"]["dynamic_client_registration"]["enabled"] = json!(false);
    assert!(plan(&dcr_off, "add", new).is_err());
    let mut oauth_off = app.clone();
    oauth_off["oauth_configuration"]["enabled"] = json!(false);
    assert!(plan(&oauth_off, "add", new).is_err());
    // Cloudflare reads an omitted `enabled` as true.
    let mut oauth_implicit = app.clone();
    oauth_implicit["oauth_configuration"]
        .as_object_mut()
        .unwrap()
        .remove("enabled");
    assert!(plan(&oauth_implicit, "add", new).unwrap().body.is_some());

    let mut moved = app.clone();
    moved["oauth_configuration"]["dynamic_client_registration"]["allow_any_on_localhost"] =
        json!(false);
    assert_eq!(
        differences(&app, &moved),
        vec!["oauth_configuration.dynamic_client_registration.allow_any_on_localhost"]
    );
    let mut reordered = app.clone();
    reordered["policies"].as_array_mut().unwrap().reverse();
    assert_eq!(differences(&app, &reordered), vec!["policies"]);
    let mut same = app.clone();
    same["updated_at"] = json!("later");
    same["oauth_configuration"]["dynamic_client_registration"]["allowed_uris"] = json!([new]);
    assert!(differences(&app, &same).is_empty());
}

#[test]
fn core_access_apps_permissions_are_exact() {
    use pigeon_core::access::{APPS_READ_PERMISSION, APPS_WRITE_PERMISSION};
    assert_eq!(APPS_READ_PERMISSION, "Access: Apps and Policies Read");
    assert_eq!(APPS_WRITE_PERMISSION, "Access: Apps and Policies Write");
}

#[test]
fn core_land_compact_rows_and_events() {
    use pigeon_capabilities::land::digest::{self, Detail};
    use pigeon_capabilities::land::state::State;
    use pigeon_capabilities::land::watch;

    let row = json!({
        "id": "1a2b3c4d5e6f7a8b", "branch": "refs/heads/claude/ai-agent-x",
        "commit": "abc123def4567890abc123def4567890abc12345", "state": "merged",
        "workers": ["core"], "updated_at": 5,
        "detail": format!("first line\nsecond line {}", "x".repeat(400)),
    });
    let c = digest::compact(&row, true);
    assert_eq!(c["branch"], "claude/ai-agent-x");
    assert_eq!(c["commit"], "abc123def456");
    assert_eq!(c["incident"], true);
    let detail = c["detail"].as_str().unwrap();
    assert_eq!(detail, "first line");
    let long = digest::compact(&json!({ "detail": "y".repeat(500) }), false);
    assert_eq!(long["detail"].as_str().unwrap().chars().count(), 160);

    let full = json!({
        "id": "a", "branch": "refs/heads/b", "state": "landed",
        "steps": [
            { "at": 1, "state": "checking", "detail": "one" },
            { "at": 2, "state": "landed", "detail": "z".repeat(500) },
        ],
        "incidents": [{ "id": 3, "worker": "core", "state": "open", "summary": "long" }],
        "queue": [],
    });
    let s = digest::compact_status(&full);
    assert_eq!(s["last_step"]["state"], "landed");
    assert_eq!(s["last_step"]["detail"].as_str().unwrap().len(), 200);
    assert_eq!(
        s["incidents"][0],
        json!({ "id": 3, "worker": "core", "state": "open" })
    );
    assert_eq!(s["incident"], true);

    let t = watch::event_text(
        "1a2b3c4d5e6f7a8b",
        "refs/heads/claude/ai-agent-x",
        "abc123def456",
        "merged",
        "main moved",
    );
    assert!(t.contains("is now merged: main moved"));
    assert!(t.contains("land_status"));
    let big = watch::event_text("i", "b", "c", "error", &"é".repeat(2000));
    assert!(big.len() <= watch::EVENT_BYTES);
    assert!(watch::notable(State::Landed));
    assert!(watch::notable(State::AwaitingApproval));
    assert!(watch::notable(State::TestFailed));
    assert!(!watch::notable(State::Testing));
    assert!(watch::run_gone("x is not a live gateway run"));
    assert!(!watch::drop_event("timeout", 0));
    assert!(watch::drop_event("timeout", 4));

    assert!(digest::state_filter(None).unwrap().is_none());
    let live = digest::state_filter(Some("live")).unwrap().unwrap();
    assert!(live.contains(&State::Merged) && !live.contains(&State::Landed));
    let ended = digest::state_filter(Some("ended")).unwrap().unwrap();
    assert!(ended.contains(&State::Landed) && !ended.contains(&State::Testing));
    assert_eq!(
        digest::state_filter(Some("testing")).unwrap(),
        Some(vec![State::Testing])
    );
    assert!(digest::state_filter(Some("nope")).is_err());

    assert_eq!(
        digest::parse_detail("run:abc").unwrap(),
        Detail::Run("abc".into())
    );
    assert_eq!(
        digest::parse_detail("landing:9").unwrap(),
        Detail::Landing("9".into())
    );
    assert!(digest::parse_detail("other:1").is_err());
    assert!(digest::parse_detail("run:").is_err());
    assert!(digest::parse_detail("run").is_err());
}

#[test]
fn core_land_work_status_digest() {
    use pigeon_capabilities::land::digest::{self, DIGEST_BYTES, Sources};

    let run = |i: usize, ended: bool| {
        json!({
            "run": format!("run{i:029}"), "role": "planner", "parent_run": "parent00000000000000000000000000",
            "brief": format!("TITLE line {i}\nSECRETBRIEF {}", "b".repeat(400)),
            "report": "SECRETREPORT", "state": "running",
            "step": "s".repeat(300),
            "waiting": { "for": if i == 0 { "approval" } else { "executors" } },
            "spent": { "turns": 3, "cost_usd": 0.123456, "gateway_cost": "$0.1", "cost_source": "gateway", "total_cost": "$0.5" },
            "budget": { "turns": 40, "budget_cost_usd": 5.0 },
            "ended_at": if ended { json!(10) } else { Value::Null },
        })
    };
    let runs: Vec<Value> = (0..30).map(|i| run(i, false)).collect();
    let sources = Sources {
        runs: Ok(json!({ "runs": runs })),
        landings: Ok((json!([{ "id": "l1", "state": "testing" }]), json!([]))),
        approvals: Ok(
            json!({ "calls": [1], "drafts": [], "plans": [], "other": [], "landings": [1, 2] }),
        ),
        inbox: Ok(json!({ "waiting": [
            { "run": "r1", "waiting": { "for": "answer", "question": "q".repeat(500) } },
            { "run": "r2", "waiting": { "for": "approval" } },
        ] })),
        release: Ok(
            json!({ "releases": [{ "job": "j", "build_number": "7", "version": "1.0", "state": "complete", "apple_processing": "VALID", "commit": "c".repeat(40) }] }),
        ),
        tokens: None,
    };
    let d = digest::build(&sources, None);
    let text = d.to_string();
    assert!(text.len() < DIGEST_BYTES, "{} bytes", text.len());
    assert!(!text.contains("SECRETBRIEF") && !text.contains("SECRETREPORT"));
    assert_eq!(d["runs"][0]["waiting_on_person"], true);
    assert_eq!(d["runs"][1]["waiting_on_person"], false);
    assert_eq!(d["runs"][0]["spent_usd"], 0.1235);
    assert_eq!(d["runs"][0]["gateway_usd"], 0.1);
    assert_eq!(d["runs"][0]["total_usd"], 0.5);
    assert_eq!(d["runs"][0]["budget_usd"], 5.0);
    assert_eq!(d["runs"][0]["turns"], "3/40");
    assert_eq!(d["runs"][0]["title"], "TITLE line 0");
    assert_eq!(d["waiting_on_person"]["approvals"]["landings"], 2);
    assert_eq!(d["waiting_on_person"]["passkey_waits"], 1);
    assert_eq!(
        d["waiting_on_person"]["questions"][0]["question"]
            .as_str()
            .unwrap()
            .len(),
        200
    );
    assert_eq!(d["ios_release"]["commit"].as_str().unwrap().len(), 12);
    assert_eq!(d["landings"]["queue"][0]["id"], "l1");
    assert_eq!(d["unavailable"], json!([]));

    // Ended runs show only after `since`, marked ended; a failing source is named.
    let sources = Sources {
        runs: Ok(json!({ "runs": [run(1, true), run(2, false)] })),
        landings: Err("no answer".into()),
        approvals: Err("needs a person".into()),
        inbox: Ok(json!({ "waiting": [] })),
        release: Ok(json!({ "releases": [] })),
        tokens: None,
    };
    let none = digest::build(&sources, None);
    assert_eq!(none["runs"].as_array().unwrap().len(), 1);
    let some = digest::build(&sources, Some(5));
    assert_eq!(some["runs"].as_array().unwrap().len(), 2);
    assert_eq!(some["runs"][1]["ended"], true);
    assert_eq!(some["ios_release"], Value::Null);
    let names: Vec<&str> = some["unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["source"].as_str())
        .collect();
    assert_eq!(names, ["landings", "approvals"]);
}

// ---- ingest: metrics, decisions, zip edges ----

#[test]
fn ingest_metrics_decisions_and_zip_edges() {
    use pigeon_ingest::archive::{Budget, Flag, Limits, Zip, is_zip, safe_path};
    use pigeon_ingest::decide::{Head, Lookup, Reason, Verdict, decide};
    use pigeon_ingest::metrics::{Hasher, Kind, hamming, normalise, simhash, sniff};
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    // Hashes and size, streamed in pieces.
    let mut h = Hasher::new(false);
    h.update(b"a");
    h.update(b"bc");
    let m = h.finish().unwrap();
    assert_eq!(m.size, 3);
    assert_eq!(
        m.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        m.blake3,
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    assert_eq!((m.mime.as_str(), m.kind), ("text/plain", Kind::Text));
    assert_eq!(m.simhash, None);

    // Fingerprint only when asked and text-like.
    let mut h = Hasher::new(true);
    h.update(b"one two three four five");
    assert_eq!(h.finish().unwrap().simhash.map(|s| s.len()), Some(16));
    let mut h = Hasher::new(true);
    h.update(b"%PDF-1.7 one two three");
    assert_eq!(h.finish().unwrap().simhash, None);

    // Sniffing is by content.
    assert_eq!(sniff(b"%PDF-1.4\n").1, Kind::Document);
    assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....").0, "image/png");
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).0, "image/jpeg");
    assert_eq!(
        sniff(b"PK\x03\x04rest"),
        ("application/zip".into(), Kind::Archive)
    );
    assert_eq!(sniff(b"\x1f\x8b\x08").1, Kind::Archive);
    assert_eq!(sniff(b"\0\0\0\x18ftypmp42").1, Kind::Video);
    assert_eq!(sniff(b"From: a@b.c\r\nTo: d@e.f\r\n").1, Kind::Email);
    assert_eq!(sniff("héllo wörld".as_bytes()).1, Kind::Text);
    assert_eq!(
        sniff(&[0x80, 0x81, 0x82, 0x00]),
        ("application/octet-stream".into(), Kind::Other)
    );

    // Names.
    assert_eq!(
        normalise("Photos\\Holiday  Pics\\Copy of IMG_1 (1).JPEG"),
        ("img_1.jpg".into(), "photos/holiday pics/img_1.jpg".into())
    );
    assert_eq!(normalise("a/Report - Copy.HTM").0, "report.html");
    assert_eq!(normalise("scan.tiff").0, "scan.tif");
    assert_eq!(normalise("Cafe\u{301}.txt").0, "caf\u{e9}.txt");

    // SimHash: near texts are near, unrelated texts are far.
    let words = [
        "river", "stone", "paper", "winter", "copper", "garden", "signal", "window", "harbour",
        "lantern", "meadow", "orchard", "pillow", "quartz", "ribbon", "saddle",
    ];
    let base: Vec<&str> = (0..80).map(|i| words[(i * 7 + i / 3) % 16]).collect();
    let mut near = base.clone();
    near[40] = "velvet";
    let far: Vec<&str> = (0..80).map(|i| words[(i * 5 + i * i) % 16]).collect();
    let (a, b, c) = (
        simhash(&base.join(" ")),
        simhash(&near.join(" ")),
        simhash(&far.join(" ").replace("river", "tunnel")),
    );
    assert_eq!(simhash(&base.join(" ").to_uppercase()), a);
    assert!(hamming(a, b) <= 12, "near {}", hamming(a, b));
    assert!(hamming(a, c) > hamming(a, b), "far {}", hamming(a, c));
    assert_eq!(hamming(0b1011, 0b0001), 2);

    // Decisions.
    let head = |size, sha: Option<&str>, b3: Option<&str>| {
        Lookup::Present(Head {
            size,
            sha256: sha.map(str::to_string),
            blake3: b3.map(str::to_string),
        })
    };
    let up = m.sha256.to_uppercase();
    let d = |l: &Lookup, dry| {
        let x = decide(&m, l, dry);
        (x.verdict, x.reason)
    };
    let seen = (Verdict::Duplicate, Reason::SeenInRun);
    assert_eq!(d(&Lookup::SeenInRun, false), seen);
    assert_eq!(d(&Lookup::SeenInRun, true), seen);
    assert_eq!(d(&Lookup::Absent, false), (Verdict::Upload, Reason::New));
    assert_eq!(
        d(&Lookup::Absent, true),
        (Verdict::WouldUpload, Reason::New)
    );
    let same = head(3, Some(&up), Some(&m.blake3));
    assert_eq!(d(&same, false), (Verdict::Duplicate, Reason::InR2));
    assert_eq!(d(&same, true), (Verdict::Duplicate, Reason::InR2));
    let no_b3 = head(3, Some(&m.sha256), None);
    assert_eq!(d(&no_b3, false), (Verdict::Duplicate, Reason::InR2));
    let conflict = (Verdict::Conflict, Reason::Mismatch);
    let bad_b3 = head(3, Some(&m.sha256), Some(&"0".repeat(64)));
    assert_eq!(d(&bad_b3, false), conflict);
    assert_eq!(d(&bad_b3, true), conflict);
    let bad_size = head(4, Some(&m.sha256), Some(&m.blake3));
    assert_eq!(d(&bad_size, false), conflict);
    let bad_sha = head(3, Some(&"0".repeat(64)), None);
    assert_eq!(d(&bad_sha, false), conflict);
    let unattested = head(3, None, Some(&m.blake3));
    assert_eq!(d(&unattested, false), (Verdict::Upload, Reason::Unattested));
    assert_eq!(
        d(&unattested, true),
        (Verdict::WouldUpload, Reason::Unattested)
    );

    // Zips.
    let build = |files: &[(&str, &[u8], bool)]| -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, deflate) in files {
            let method = if *deflate {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            };
            let opts = SimpleFileOptions::default().compression_method(method);
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    let extract = |z: &mut Zip<Cursor<Vec<u8>>>, i: u64, b: &mut Budget| {
        let mut out = Vec::new();
        let r = z.extract(i, b, &mut out).unwrap();
        (r, out)
    };

    // Plain, with a nested zip.
    let inner = build(&[("deep.txt", b"deep bytes", true)]);
    assert!(is_zip(&inner));
    assert!(is_zip(b"PK\x05\x06") && !is_zip(b"PK\x01\x02") && !is_zip(b"%PDF"));
    let outer = build(&[
        ("a.txt", b"alpha", true),
        ("dir\\c.txt", b"beta", false),
        ("inner.zip", &inner, false),
    ]);
    let mut z = Zip::open(Cursor::new(outer)).unwrap();
    let es = z.entries();
    assert_eq!(es.len(), 3);
    assert_eq!(es[1].path, "dir/c.txt");
    assert_eq!(es[1].raw_name, "dir\\c.txt");
    let mut budget = Budget::new(Limits::default());
    assert_eq!(extract(&mut z, 0, &mut budget), (Ok(5), b"alpha".to_vec()));
    assert_eq!(extract(&mut z, 1, &mut budget), (Ok(4), b"beta".to_vec()));
    let (r, bytes) = extract(&mut z, 2, &mut budget);
    assert_eq!(r, Ok(inner.len() as u64));
    assert_eq!(budget.used, 5 + 4 + inner.len() as u64);
    let mut iz = Zip::open(Cursor::new(bytes)).unwrap();
    assert_eq!(extract(&mut iz, 0, &mut budget).1, b"deep bytes");
    assert_eq!(extract(&mut iz, 0, &mut budget).1, b"deep bytes");

    // Path traversal: flagged, normalised, raw name kept.
    assert_eq!(safe_path("/etc/../C:/x"), ("etc/C:/x".into(), true));
    assert_eq!(
        safe_path("C:\\Windows\\a.txt"),
        ("Windows/a.txt".into(), true)
    );
    assert_eq!(safe_path("ok/file.txt"), ("ok/file.txt".into(), false));
    let evil = build(&[("../evil.txt", b"x", false), ("fine.txt", b"y", false)]);
    let z = Zip::open(Cursor::new(evil)).unwrap();
    let es = z.entries();
    assert_eq!(es[0].path, "evil.txt");
    assert_eq!(es[0].raw_name, "../evil.txt");
    assert_eq!(es[0].flags, vec![Flag::UnsafePath]);
    assert!(es[1].flags.is_empty());

    // Encrypted: bit 0 set by hand in the central directory; never opened.
    let mut enc = build(&[
        ("secret.txt", b"classified", false),
        ("open.txt", b"fine", false),
    ]);
    let cd = enc.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    enc[cd + 8] |= 1;
    let mut z = Zip::open(Cursor::new(enc)).unwrap();
    assert_eq!(z.entries()[0].flags, vec![Flag::Encrypted]);
    let mut budget = Budget::new(Limits::default());
    assert_eq!(
        extract(&mut z, 0, &mut budget),
        (Err(Flag::Encrypted), Vec::new())
    );
    assert_eq!(extract(&mut z, 1, &mut budget).0, Ok(4));

    // A bomb: tiny when deflated, large when inflated, with small limits.
    let bomb = build(&[("zeros.bin", &vec![0u8; 200_000], true)]);
    assert!(bomb.len() < 2_000);
    let ratio = Limits {
        ratio_floor: 1024,
        max_ratio: 50,
        ..Limits::default()
    };
    let mut z = Zip::open(Cursor::new(bomb.clone())).unwrap();
    let mut budget = Budget::new(ratio);
    assert_eq!(extract(&mut z, 0, &mut budget).0, Err(Flag::BombSuspect));
    assert!(budget.used < 200_000, "stopped while streaming");
    let mut z = Zip::open(Cursor::new(bomb.clone())).unwrap();
    let mut budget = Budget::new(Limits {
        max_total_bytes: 100_000,
        ..Limits::default()
    });
    assert_eq!(extract(&mut z, 0, &mut budget).0, Err(Flag::BombSuspect));
    let mut z = Zip::open(Cursor::new(bomb)).unwrap();
    let mut budget = Budget::new(Limits {
        max_member_bytes: 1000,
        ..Limits::default()
    });
    assert_eq!(extract(&mut z, 0, &mut budget).0, Err(Flag::MemberTooLarge));

    // A corrupted member is not stored; the next one is.
    let mut bad = build(&[
        ("one.txt", b"first member data", false),
        ("two.txt", b"second", false),
    ]);
    let at = bad
        .windows(17)
        .position(|w| w == b"first member data")
        .unwrap();
    bad[at] ^= 0xFF;
    let mut z = Zip::open(Cursor::new(bad)).unwrap();
    let mut budget = Budget::new(Limits::default());
    assert_eq!(extract(&mut z, 0, &mut budget).0, Err(Flag::CrcMismatch));
    assert_eq!(extract(&mut z, 1, &mut budget), (Ok(6), b"second".to_vec()));

    // Too many entries, with a small limit.
    let many = build(&[("1", b"1", false), ("2", b"2", false), ("3", b"3", false)]);
    let mut z = Zip::open(Cursor::new(many)).unwrap();
    let mut budget = Budget::new(Limits {
        max_entries: 2,
        ..Limits::default()
    });
    assert_eq!(extract(&mut z, 0, &mut budget).0, Ok(1));
    assert_eq!(extract(&mut z, 1, &mut budget).0, Ok(1));
    assert_eq!(extract(&mut z, 2, &mut budget).0, Err(Flag::TooManyEntries));

    // Not a zip, or a broken directory: an error, not a panic.
    assert!(Zip::open(Cursor::new(b"PK\x03\x04 not really".to_vec())).is_err());
    assert!(Zip::open(Cursor::new(Vec::new())).is_err());
}

/// The data Worker's ingest path: the D1 index schema and statements (run for real on an
/// in-memory SQLite), signed starts, R2 event messages and the container's environment.
#[test]
fn ingest_index_applies_manifest_parts_and_verifies_starts() {
    use pigeon_core::sign::{hex, hmac_sha256};
    use pigeon_ingest::index::{MAX_PARAMS, Statement, apply_part, apply_run, schema_statements};
    use pigeon_ingest::manifest::{JobSpec, ManifestLine, Status, part_key};
    use pigeon_ingest::protocol::{
        MAX_SKEW_MS, StartRequest, is_manifest_part, parse_event, signing_message, start_env,
        verify,
    };

    fn run(db: &rusqlite::Connection, s: &Statement) {
        let params: Vec<rusqlite::types::Value> = s
            .params
            .iter()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) if n.is_i64() => {
                    rusqlite::types::Value::Integer(n.as_i64().unwrap())
                }
                Value::Number(n) => rusqlite::types::Value::Real(n.as_f64().unwrap()),
                Value::String(t) => rusqlite::types::Value::Text(t.clone()),
                other => rusqlite::types::Value::Text(other.to_string()),
            })
            .collect();
        assert!(params.len() <= MAX_PARAMS, "{}", s.sql);
        db.execute(&s.sql, rusqlite::params_from_iter(params))
            .unwrap_or_else(|e| panic!("{e}: {}", s.sql));
    }
    fn count(db: &rusqlite::Connection, table: &str) -> i64 {
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn line(id: &str, sha: &str, verdict: &str, member: bool) -> ManifestLine {
        serde_json::from_value(json!({
            "id": id,
            "origin": { "account": "local:laptop", "bucket": "b", "key": format!("k/{id}"),
                        "etag": null, "listed_size": 5, "modified": null },
            "member": if member { json!({ "parent_sha256": "aa", "path": "x.txt",
                "raw_name": "x.txt", "index": 2, "depth": 1, "chain": [2] }) } else { Value::Null },
            "sha256": sha, "blake3": "bb", "size": 5, "mime": "text/plain", "kind": "text",
            "norm_name": "x.txt", "simhash": null, "verdict": verdict, "reason": "new",
            "flags": ["encrypted"],
            "hints": [{ "kind": "same_name_size", "other": "cc", "score": null }],
            "r2_key": null, "error": null, "at_ms": 7
        }))
        .unwrap()
    }

    // The schema runs, twice (IF NOT EXISTS).
    let db = rusqlite::Connection::open_in_memory().unwrap();
    for _ in 0..2 {
        for s in schema_statements() {
            db.execute(&s.sql, []).unwrap();
        }
    }
    assert_eq!(schema_statements().len(), 7);

    // A part: a repeated sighting id, a would-upload (no blob), a skipped member with no hash,
    // and enough lines to need several batches.
    let part = part_key("run1", 0);
    let mut lines = vec![
        line("s1", "sha1", "upload", false),
        line("s1", "sha1", "upload", false),
        line("s2", "sha2", "duplicate", true),
        line("s3", "sha3", "would_upload", false),
        line("s4", "", "skipped", true),
    ];
    for i in 0..60 {
        lines.push(line(&format!("m{i}"), "sha1", "duplicate", false));
    }
    let statements = apply_part(&part, &lines, false, 99);
    assert!(statements[0].sql.starts_with("INSERT INTO index_applied"));
    for s in &statements {
        assert!(s.sql.ends_with("ON CONFLICT DO NOTHING"), "{}", s.sql);
        assert!(s.params.len() <= MAX_PARAMS);
    }
    let sighting_batches = statements
        .iter()
        .filter(|s| s.sql.contains("INTO index_sightings"));
    assert!(sighting_batches.count() > 1);
    for s in &statements {
        run(&db, s);
    }
    let counts = |db: &rusqlite::Connection| {
        [
            "index_applied",
            "index_blobs",
            "index_sightings",
            "index_hints",
        ]
        .map(|t| count(db, t))
    };
    // 64 distinct sighting ids, blobs sha1 and sha2 only, one hint per hash with a blob or not.
    assert_eq!(counts(&db), [1, 2, 64, 3]);
    // Applying the same part again changes nothing.
    for s in apply_part(&part, &lines, false, 100) {
        run(&db, &s);
    }
    assert_eq!(counts(&db), [1, 2, 64, 3]);
    let (decision, flags, run_id): (String, String, String) = db
        .query_row(
            "SELECT decision, flags, run FROM index_sightings WHERE id = 's2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (decision.as_str(), flags.as_str(), run_id.as_str()),
        ("duplicate", "encrypted", "run1")
    );

    // The run row is an upsert.
    let job: JobSpec = serde_json::from_value(json!({
        "run": "run1", "instance": "i",
        "source": { "kind": "s3", "account": "backblaze:b2", "endpoint": "https://s3.example",
                    "region": "r", "bucket": "src", "prefix": "p/" },
        "dest": { "endpoint": "https://d.example", "bucket": "dst" },
        "dry_run": false, "keep_archives": true, "fingerprints": false,
        "limits": pigeon_ingest::archive::Limits::default(), "threads": 4, "commit": "abc"
    }))
    .unwrap();
    let mut status: Status = serde_json::from_value(json!({
        "run": "run1", "state": "running", "dry_run": false,
        "counters": pigeon_ingest::manifest::Counters::default(),
        "started_ms": 1, "updated_ms": 2, "read_rate": 0.0, "eta_s": null, "last_error": null
    }))
    .unwrap();
    run(&db, &apply_run(&job, &status, "cloudflare"));
    status.state = pigeon_ingest::manifest::RunState::Done;
    status.updated_ms = 3;
    run(&db, &apply_run(&job, &status, "cloudflare"));
    assert_eq!(count(&db, "index_runs"), 1);
    let (state, updated): (String, i64) = db
        .query_row("SELECT state, updated_ms FROM index_runs", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((state.as_str(), updated), ("done", 3));

    // Signatures, signed as the capability signs them.
    let key = b"data-key";
    let now = 1_700_000_000_000_i64;
    let body = br#"{"run":"run1"}"#;
    let sign = |k: &[u8], at: i64, body: &[u8]| {
        let m = signing_message("POST", "/ingest/stop", at, body);
        format!("t={at},v1={}", hex(&hmac_sha256(k, m.as_bytes())))
    };
    let good = sign(key, now, body);
    let stop = "/ingest/stop";
    assert!(verify(&good, "POST", stop, body, key, now + 1000).is_ok());
    assert!(verify(&sign(b"other", now, body), "POST", stop, body, key, now).is_err());
    assert!(verify(&good, "POST", stop, body, key, now + MAX_SKEW_MS + 1).is_err());
    assert!(verify(&good, "POST", stop, body, key, now - MAX_SKEW_MS - 1).is_err());
    assert!(verify(&good, "POST", stop, b"{}", key, now).is_err());
    assert!(verify(&good, "POST", "/ingest/start", body, key, now).is_err());
    let odd = format!("t={now},v1=abc");
    let nonhex = format!("t={now},v1=zz");
    for bad in ["", "garbage", "t=x,v1=00", "t=1", "v1=00", &odd, &nonhex] {
        assert!(verify(bad, "POST", stop, body, key, now).is_err(), "{bad}");
    }

    // R2 event messages.
    let event = |action: &str, key: &str| json!({ "action": action, "object": { "key": key } });
    let e = parse_event(&event("PutObject", &part_key("run1", 3))).unwrap();
    assert!(e.is_manifest_part && e.key.ends_with("00000003.jsonl"));
    let status_event = parse_event(&event("PutObject", "ingest/runs/run1/status.json")).unwrap();
    assert!(!status_event.is_manifest_part);
    assert!(parse_event(&event("DeleteObject", &part_key("run1", 3))).is_none());
    assert!(parse_event(&json!("garbage")).is_none());
    assert!(parse_event(&json!({ "object": {} })).is_none());
    for key in [
        "ingest/runs/r/manifest/1.jsonl",
        "ingest/runs/r/manifest/00000001.json",
        "ingest/runs//manifest/00000001.jsonl",
        "blobs/x",
        "ingest/runs/r/manifest/a/00000001.jsonl",
    ] {
        assert!(!is_manifest_part(key), "{key}");
    }

    // The container's environment.
    let req = StartRequest {
        job,
        source_keys: "AK:SK".into(),
        dest_keys: "RK:RS".into(),
    };
    let env = start_env(&req).unwrap();
    let get = |n: &str| {
        env.iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(get("SOURCE_KEYS"), "AK:SK");
    assert_eq!(get("DEST_KEYS"), "RK:RS");
    assert_eq!(get("SPOOL_DIR"), "/spool");
    let back: JobSpec = serde_json::from_str(&get("INGEST_JOB")).unwrap();
    assert_eq!(back, req.job);
}

/// The ingest run engine over in-memory stubs: dedupe against R2, conflicts, dry runs, zips,
/// retries, stop and resume, status and report.
#[test]
fn ingest_engine_dedupes_resumes_and_opens_zips() {
    use pigeon_ingest::archive::Flag;
    use pigeon_ingest::decide::{Head, Reason, Verdict};
    use pigeon_ingest::engine::{
        Clock, Dest, Object, Page, Run, Source, Spool, SpoolFile, Verify, run,
    };
    use pigeon_ingest::manifest::{
        DestSpec, JobSpec, ManifestLine, Report, RunState, SourceSpec, Status, quarantine_key,
    };
    use pigeon_ingest::metrics::{Hasher, Metrics};
    use pigeon_ingest::{Error, archive::Limits, blob_key};
    use std::collections::{BTreeMap, HashMap};
    use std::io::{Cursor, Read, Write};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Bad;
    impl Read for Bad {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk on fire"))
        }
    }
    struct MemSource<'a> {
        objs: BTreeMap<String, Vec<u8>>,
        bad: String,
        stop: &'a AtomicBool,
        trigger: String,
        opens: Mutex<HashMap<String, u32>>,
    }
    impl Source for MemSource<'_> {
        fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
            let start = token.or(after);
            let all: Vec<Object> = self
                .objs
                .iter()
                .filter(|(k, _)| start.is_none_or(|s| k.as_str() > s))
                .map(|(k, v)| Object {
                    key: k.clone(),
                    size: v.len() as u64,
                    etag: None,
                    modified: None,
                })
                .collect();
            let next = if all.len() > 3 {
                Some(all[2].key.clone())
            } else {
                None
            };
            Ok(Page {
                objects: all.into_iter().take(3).collect(),
                next,
            })
        }
        fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
            *self
                .opens
                .lock()
                .unwrap()
                .entry(key.to_string())
                .or_insert(0) += 1;
            if key == self.trigger {
                self.stop.store(true, Ordering::SeqCst);
            }
            if key == self.bad {
                return Ok(Box::new(Bad));
            }
            match self.objs.get(key) {
                Some(v) => Ok(Box::new(Cursor::new(v.clone()))),
                None => Err(Error::Source("no such key".into())),
            }
        }
    }

    type Stored = (Vec<u8>, Option<String>, Option<String>);
    #[derive(Default)]
    struct MemDest {
        objs: Mutex<HashMap<String, Stored>>,
        puts: Mutex<Vec<String>>,
    }
    impl MemDest {
        fn seed(&self, key: &str, bytes: &[u8], sha: Option<String>, b3: Option<String>) {
            self.objs
                .lock()
                .unwrap()
                .insert(key.to_string(), (bytes.to_vec(), sha, b3));
        }
        fn bytes(&self, key: &str) -> Option<Vec<u8>> {
            self.objs.lock().unwrap().get(key).map(|v| v.0.clone())
        }
        fn put_count(&self, key: &str) -> usize {
            self.puts
                .lock()
                .unwrap()
                .iter()
                .filter(|k| *k == key)
                .count()
        }
        fn lines(&self, run_id: &str) -> Vec<ManifestLine> {
            let prefix = format!("ingest/runs/{run_id}/manifest/");
            let mut keys: Vec<String> = self
                .objs
                .lock()
                .unwrap()
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            keys.sort();
            let mut out = Vec::new();
            for k in keys {
                let body = self.bytes(&k).unwrap();
                for l in std::str::from_utf8(&body).unwrap().lines() {
                    out.push(serde_json::from_str(l).unwrap());
                }
            }
            out
        }
    }
    impl Dest for MemDest {
        fn head(&self, key: &str) -> Result<Option<Head>, Error> {
            Ok(self.objs.lock().unwrap().get(key).map(|(b, s, h)| Head {
                size: b.len() as u64,
                sha256: s.clone(),
                blake3: h.clone(),
            }))
        }
        fn put_blob(
            &self,
            key: &str,
            body: &mut dyn Read,
            size: u64,
            m: &Metrics,
        ) -> Result<(), Error> {
            let mut buf = Vec::new();
            body.read_to_end(&mut buf)
                .map_err(|e| Error::Dest(e.to_string()))?;
            if buf.len() as u64 != size {
                return Err(Error::Dest("wrong length".into()));
            }
            self.puts.lock().unwrap().push(key.to_string());
            self.seed(key, &buf, Some(m.sha256.clone()), Some(m.blake3.clone()));
            Ok(())
        }
        fn put_object(&self, key: &str, body: &[u8], _: &str) -> Result<(), Error> {
            self.seed(key, body, None, None);
            Ok(())
        }
        fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
            Ok(self.bytes(key))
        }
    }

    struct MemSpool;
    impl Spool for MemSpool {
        fn create(&self) -> Result<Box<dyn SpoolFile>, Error> {
            Ok(Box::new(Cursor::new(Vec::new())))
        }
        fn free_bytes(&self) -> u64 {
            5000
        }
    }
    struct Fixed;
    impl Clock for Fixed {
        fn now_ms(&self) -> i64 {
            1000
        }
    }

    let metrics = |b: &[u8]| {
        let mut h = Hasher::new(false);
        h.update(b);
        h.finish().unwrap()
    };
    let build_zip = |files: &[(&str, &[u8], bool)]| -> Vec<u8> {
        use zip::unstable::write::FileOptionsExt;
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, enc) in files {
            let o = SimpleFileOptions::default();
            if *enc {
                w.start_file(*name, o.with_deprecated_encryption(b"pw"))
                    .unwrap();
            } else {
                w.start_file(*name, o).unwrap();
            }
            w.write_all(data).unwrap();
        }
        w.add_directory("dir/", SimpleFileOptions::default())
            .unwrap();
        w.finish().unwrap().into_inner()
    };
    let job = |run_id: &str, dry: bool| JobSpec {
        run: run_id.into(),
        instance: "t".into(),
        source: SourceSpec::S3 {
            account: "acct".into(),
            endpoint: "https://s3.test".into(),
            region: "r".into(),
            bucket: "src".into(),
            prefix: String::new(),
        },
        dest: DestSpec {
            endpoint: "https://r2.test".into(),
            bucket: "dst".into(),
        },
        dry_run: dry,
        keep_archives: true,
        fingerprints: false,
        limits: Limits::default(),
        threads: 1,
        commit: "c".into(),
        max_minutes: None,
        resume: None,
        skip_over: None,
        shard: None,
        only_over: None,
    };

    // Verify passes the right bytes and fails on a short, long or changed stream.
    let v = metrics(b"abc");
    let mut out = Vec::new();
    assert!(
        Verify::new(Cursor::new(b"abc".to_vec()), v.clone())
            .read_to_end(&mut out)
            .is_ok()
    );
    for bad in [&b"ab"[..], b"abcd", b"abd"] {
        let mut out = Vec::new();
        assert!(
            Verify::new(Cursor::new(bad.to_vec()), v.clone())
                .read_to_end(&mut out)
                .is_err()
        );
    }

    // The source objects.
    let new_bytes = b"hello new file\n".to_vec();
    let exists = b"already in r2\n".to_vec();
    let conflict_a = b"conflicting by size\n".to_vec();
    let conflict_b = b"conflicting by blake3\n".to_vec();
    let unattested = b"written by another path\n".to_vec();
    let deep = b"deep inside\n".to_vec();
    let inner = build_zip(&[("deep.txt", &deep, false)]);
    let own = b"own member\n".to_vec();
    let outer = build_zip(&[
        ("m1.txt", &new_bytes, false),
        ("inner.zip", &inner, false),
        ("secret.txt", b"top secret\n", true),
        ("own.txt", &own, false),
    ]);
    let known = build_zip(&[("z.txt", b"only in known zip\n", false)]);
    let big: Vec<u8> = (0..6000u32).map(|i| (i % 251) as u8).collect();
    let mut objs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (k, v) in [
        ("a/new.txt", &new_bytes),
        ("b/same.txt", &new_bytes),
        ("c/exists.bin", &exists),
        ("d/conflict.bin", &conflict_a),
        ("e/conflict2.bin", &conflict_b),
        ("f/unattested.bin", &unattested),
        ("g/outer.zip", &outer),
        ("h/known.zip", &known),
        ("i/broken.bin", &new_bytes),
        ("k/big.bin", &big),
        ("l/same.txt", &b"aaaa".to_vec()),
        ("m/same.txt", &b"bbbb".to_vec()),
    ] {
        objs.insert(k.to_string(), v.clone());
    }
    let make_dest = || {
        let d = MemDest::default();
        let m = metrics(&exists);
        d.seed(
            &blob_key(&m.sha256),
            &exists,
            Some(m.sha256),
            Some(m.blake3),
        );
        let m = metrics(&conflict_a);
        d.seed(&blob_key(&m.sha256), b"zz", Some(m.sha256), Some(m.blake3));
        let m = metrics(&conflict_b);
        d.seed(
            &blob_key(&m.sha256),
            &conflict_b,
            Some(m.sha256),
            Some("ff".repeat(32)),
        );
        let m = metrics(&unattested);
        d.seed(&blob_key(&m.sha256), b"x", None, None);
        let m = metrics(&known);
        d.seed(&blob_key(&m.sha256), &known, Some(m.sha256), Some(m.blake3));
        d
    };
    let source = |stop: &'static AtomicBool, trigger: &str| MemSource {
        objs: objs.clone(),
        bad: "i/broken.bin".into(),
        stop,
        trigger: trigger.into(),
        opens: Mutex::new(HashMap::new()),
    };
    let stop_never: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let find = |lines: &[ManifestLine], key: &str| -> ManifestLine {
        lines
            .iter()
            .find(|l| l.origin.key == key && l.member.is_none())
            .cloned()
            .unwrap()
    };
    let member = |lines: &[ManifestLine], path: &str| -> ManifestLine {
        lines
            .iter()
            .find(|l| l.member.as_ref().is_some_and(|m| m.path == path))
            .cloned()
            .unwrap()
    };

    // ---- a full run ----
    let dest = make_dest();
    let src = source(stop_never, "");
    let j = job("r1", false);
    let r1 = run(&Run {
        job: &j,
        source: &src,
        dest: &dest,
        spool: &MemSpool,
        clock: &Fixed,
        stop: stop_never,
    })
    .unwrap();
    assert_eq!(r1.state, RunState::Done);
    let lines = dest.lines("r1");
    let sha = |b: &[u8]| metrics(b).sha256;

    let a = find(&lines, "a/new.txt");
    assert_eq!((a.verdict, a.reason), (Verdict::Upload, Reason::New));
    assert_eq!(a.r2_key, Some(blob_key(&sha(&new_bytes))));
    let stored = dest
        .objs
        .lock()
        .unwrap()
        .get(&blob_key(&sha(&new_bytes)))
        .cloned()
        .unwrap();
    assert_eq!(stored.0, new_bytes);
    assert_eq!(stored.1, Some(sha(&new_bytes)));
    let b = find(&lines, "b/same.txt");
    assert_eq!(
        (b.verdict, b.reason),
        (Verdict::Duplicate, Reason::SeenInRun)
    );
    assert_eq!(dest.put_count(&blob_key(&sha(&new_bytes))), 1);

    let c = find(&lines, "c/exists.bin");
    assert_eq!((c.verdict, c.reason), (Verdict::Duplicate, Reason::InR2));
    assert_eq!(dest.put_count(&blob_key(&sha(&exists))), 0);

    for (key, bytes) in [
        ("d/conflict.bin", &conflict_a),
        ("e/conflict2.bin", &conflict_b),
    ] {
        let l = find(&lines, key);
        assert_eq!((l.verdict, l.reason), (Verdict::Conflict, Reason::Mismatch));
        let q = quarantine_key("r1", &sha(bytes));
        assert_eq!(l.r2_key, Some(q.clone()));
        assert_eq!(dest.bytes(&q).as_ref(), Some(bytes));
        assert_eq!(dest.put_count(&blob_key(&sha(bytes))), 0);
    }
    assert_eq!(dest.bytes(&blob_key(&sha(&conflict_a))).unwrap(), b"zz");
    assert_eq!(r1.counters.conflicts, 2);
    assert_eq!(r1.conflicts.len(), 2);

    let f = find(&lines, "f/unattested.bin");
    assert_eq!((f.verdict, f.reason), (Verdict::Upload, Reason::Unattested));
    assert_eq!(
        dest.bytes(&blob_key(&sha(&unattested))).unwrap(),
        unattested
    );

    // Zips.
    let g = find(&lines, "g/outer.zip");
    assert_eq!(g.verdict, Verdict::Upload);
    let m1 = member(&lines, "m1.txt");
    assert_eq!(
        (m1.verdict, m1.reason),
        (Verdict::Duplicate, Reason::SeenInRun)
    );
    assert_eq!(m1.member.as_ref().unwrap().parent_sha256, g.sha256);
    assert_eq!(m1.member.as_ref().unwrap().depth, 1);
    let iz = member(&lines, "inner.zip");
    assert_eq!(iz.verdict, Verdict::Upload);
    let dp = member(&lines, "deep.txt");
    assert_eq!(dp.verdict, Verdict::Upload);
    let dm = dp.member.as_ref().unwrap();
    assert_eq!((dm.depth, dm.chain.len()), (2, 2));
    assert_eq!(dm.parent_sha256, iz.sha256);
    assert_eq!(dest.bytes(&blob_key(&sha(&deep))).unwrap(), deep);
    let sec = member(&lines, "secret.txt");
    assert_eq!(sec.verdict, Verdict::Skipped);
    assert!(sec.flags.contains(&Flag::Encrypted));
    assert_eq!(member(&lines, "own.txt").verdict, Verdict::Upload);
    assert_eq!(r1.counters.archives_opened, 2);

    // A zip already in R2 is not opened again.
    let h = find(&lines, "h/known.zip");
    assert_eq!((h.verdict, h.reason), (Verdict::Duplicate, Reason::InR2));
    assert!(
        lines
            .iter()
            .all(|l| l.member.as_ref().map(|m| &m.parent_sha256) != Some(&h.sha256))
    );
    assert_eq!(dest.put_count(&blob_key(&sha(b"only in known zip\n"))), 0);

    // A failing read is retried, then Failed; never a duplicate.
    let i = find(&lines, "i/broken.bin");
    assert_eq!((i.verdict, i.reason), (Verdict::Failed, Reason::Error));
    assert!(i.error.is_some() && i.sha256.is_empty());
    assert_eq!(src.opens.lock().unwrap()["i/broken.bin"], 4);
    assert_eq!(r1.counters.failed, 1);
    assert_eq!(r1.failures.len(), 1);

    // Larger than the spool: copied from a verified second read.
    let k = find(&lines, "k/big.bin");
    assert_eq!(k.verdict, Verdict::Upload);
    assert!(k.flags.contains(&Flag::OverSpool));
    assert_eq!(dest.bytes(&blob_key(&sha(&big))).unwrap(), big);
    assert_eq!(src.opens.lock().unwrap()["k/big.bin"], 2);

    // Hints.
    let l = find(&lines, "l/same.txt");
    let mm = find(&lines, "m/same.txt");
    assert!(l.hints.is_empty());
    assert_eq!(mm.hints.len(), 1);
    assert_eq!(mm.hints[0].kind, "same_name_size");
    assert_eq!(mm.hints[0].other, l.sha256);

    // Status and report are written.
    let status: Status =
        serde_json::from_slice(&dest.bytes("ingest/runs/r1/status.json").unwrap()).unwrap();
    assert_eq!(status.state, RunState::Done);
    let written: Report =
        serde_json::from_slice(&dest.bytes("ingest/runs/r1/report.json").unwrap()).unwrap();
    assert_eq!(written.counters, r1.counters);
    assert!(
        written
            .flags
            .iter()
            .any(|(f, n)| *f == Flag::Encrypted && *n == 1)
    );

    // ---- a dry run writes nothing to blobs/ or quarantine/ ----
    let dest_d = make_dest();
    let before: Vec<String> = dest_d.objs.lock().unwrap().keys().cloned().collect();
    let src_d = source(stop_never, "");
    let jd = job("dry", true);
    let rd = run(&Run {
        job: &jd,
        source: &src_d,
        dest: &dest_d,
        spool: &MemSpool,
        clock: &Fixed,
        stop: stop_never,
    })
    .unwrap();
    assert!(dest_d.puts.lock().unwrap().is_empty());
    let after: Vec<String> = dest_d.objs.lock().unwrap().keys().cloned().collect();
    assert!(
        after
            .iter()
            .all(|k| before.contains(k) || k.starts_with("ingest/runs/dry/"))
    );
    assert_eq!(rd.counters.uploaded, 0);
    assert!(rd.counters.would_upload > 0);
    let dl = dest_d.lines("dry");
    assert_eq!(find(&dl, "a/new.txt").verdict, Verdict::WouldUpload);
    assert_eq!(find(&dl, "d/conflict.bin").verdict, Verdict::Conflict);

    // ---- stop, then resume ----
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let dest_s = make_dest();
    let src_s = source(stop, "e/conflict2.bin");
    let js = job("r2", false);
    let go = |s: &MemSource<'_>| {
        run(&Run {
            job: &js,
            source: s,
            dest: &dest_s,
            spool: &MemSpool,
            clock: &Fixed,
            stop,
        })
        .unwrap()
    };
    let first = go(&src_s);
    assert_eq!(first.state, RunState::Stopped);
    let cp: pigeon_ingest::manifest::Checkpoint =
        serde_json::from_slice(&dest_s.bytes("ingest/runs/r2/checkpoint.json").unwrap()).unwrap();
    assert_eq!(cp.after_key.as_deref(), Some("e/conflict2.bin"));
    assert_eq!(cp.seq, 1);
    let st: Status =
        serde_json::from_slice(&dest_s.bytes("ingest/runs/r2/status.json").unwrap()).unwrap();
    assert_eq!(st.state, RunState::Stopped);
    stop.store(false, Ordering::SeqCst);
    let src_s2 = source(stop, "");
    let second = go(&src_s2);
    assert_eq!(second.state, RunState::Done);
    assert!(src_s2.opens.lock().unwrap().get("a/new.txt").is_none());
    assert_eq!(second.counters, r1.counters);
    let sl = dest_s.lines("r2");
    let mut ids: Vec<&String> = sl.iter().map(|l| &l.id).collect();
    let n = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), n, "no line twice across the resume");
    assert_eq!(n, lines.len());
}

/// A file in the smoke suite's area modules runs the tests and deploys nothing, like the root.
#[test]
fn land_smoke_modules_run_tests_and_deploy_nothing() {
    use pigeon_core::lanes::{self, Owner};

    let map = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    assert!(matches!(map.owner("tests/smoke.rs"), Owner::Smoke));
    assert!(matches!(map.owner("tests/smoke/land.rs"), Owner::Smoke));
    assert!(!matches!(map.owner("tests/smoke/notes.md"), Owner::Smoke));
    let affected = map.classify(&["tests/smoke/land.rs"]);
    assert!(affected.smoke);
    assert!(affected.workers.is_empty());
    assert!(!affected.shared);
}

/// land (text merge): a real diff3 against the merge base, with the duplicate guard.
#[test]
fn land_text_merge_is_diff3_and_never_duplicates() {
    use pigeon_core::merge3::{self, Conflict};

    let started = std::time::Instant::now();

    // (a) The duplicate reproduction: a new block P = N + T, where T also ends an old block Q.
    let t = "    assert!(x);\n    assert!(y);\n\n";
    let n = "new one\nnew two\n";
    let h = "h1\nh2\nh3\n";
    let q = format!("q1\nq2\n{t}");
    let u = "u1\nu2\nu3\n";
    let p = format!("{n}{t}");
    let base = format!("{h}{q}{u}");
    let ours = format!("H1\nh2\nh3\n{q}{p}{u}");
    let theirs = format!("{h}{q}{p}u1\nu2\nU3\n");
    let want = format!("H1\nh2\nh3\n{q}{p}u1\nu2\nU3\n");
    let joined = merge3::try_merge(&base, &ours, &theirs).unwrap();
    assert_eq!(joined, want);
    assert_eq!(joined.matches("new one\n").count(), 1);

    // (b) The same five lines added in two functions: compaction cannot align them.
    let block = "    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n";
    let base = "fn one() {\n    first();\n}\n\nfn two() {\n    second();\n}\n";
    let ours = format!("fn one() {{\n    first();\n{block}}}\n\nfn two() {{\n    second();\n}}\n");
    let theirs =
        format!("fn one() {{\n    first();\n}}\n\nfn two() {{\n    second();\n{block}}}\n");
    assert!(matches!(
        merge3::try_merge(base, &ours, &theirs),
        Err(Conflict::Duplicate { .. })
    ));
    assert!(!Conflict::Duplicate { line: 3 }.to_string().is_empty());

    // (c) A 10,000-line file changed near the top by one side and near the bottom by the other.
    let lines = |edit: Option<(usize, &str)>| -> String {
        (1..=10_000)
            .map(|i| match edit {
                Some((at, text)) if at == i => format!("{text}\n"),
                _ => format!("line {i}\n"),
            })
            .collect()
    };
    let big = merge3::try_merge(
        &lines(None),
        &lines(Some((10, "top"))),
        &lines(Some((9_990, "bottom"))),
    )
    .unwrap();
    assert!(big.contains("top\n") && big.contains("bottom\n"));
    assert_eq!(big.lines().count(), 10_000);

    // (d) The same line changed two ways, and two different insertions at one point.
    assert_eq!(
        merge3::try_merge("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n"),
        Err(Conflict::Overlap { line: 2 })
    );
    assert_eq!(
        merge3::try_merge("a\nb\n", "a\nX\nb\n", "a\nY\nb\n"),
        Err(Conflict::Overlap { line: 2 })
    );
    assert_eq!(merge3::merge("a\nb\n", "a\nX\nb\n", "a\nY\nb\n"), None);

    // (e) Identical changes join once; a missing final newline is kept.
    assert_eq!(
        merge3::try_merge("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nB\nc\nD\n").as_deref(),
        Ok("a\nB\nc\nD\n")
    );
    assert_eq!(
        merge3::try_merge("a\nb\nc", "A\nb\nc", "a\nb\nC").as_deref(),
        Ok("A\nb\nC")
    );
    assert_eq!(
        merge3::try_merge("a\nb\nc\n", "a\nb\nc", "A\nb\nc\n").as_deref(),
        Ok("A\nb\nc")
    );

    // (f) The same answer as `git merge-file -p ours base theirs`.
    let dir = std::env::temp_dir().join(format!("pigeon-smoke-merge3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cases = [
        ("a\nb\nc\nd\ne\n", "a\nB\nc\nd\ne\n", "a\nb\nc\nd\nE\n"),
        ("a\nb\nc\n", "a\nX\nc\n", "a\nY\nc\n"),
        ("a\nb\nc\nd\n", "a\nB\nc\nd\n", "a\nb\nC\nd\n"),
        ("a\nb\n", "a\nb\nc\n", "z\na\nb\n"),
        ("a\nb\n", "a\nX\nb\n", "a\nX\nb\nY\n"),
        // A replacement never slides: ours's deletion of the first `l2` must not merge with
        // theirs's deletion of another `l2`, or one deletion is lost.
        (
            "l2\nl2\nl2\nl2\nl1\n",
            "m2\nl2\nl2\nl2\nl1\n",
            "l2\nl2\nl2\nl1\nn2\nn1\nn0\n",
        ),
    ];
    for (i, (base, ours, theirs)) in cases.iter().enumerate() {
        let file = |name: &str, body: &str| {
            let path = dir.join(format!("{i}-{name}"));
            std::fs::write(&path, body).unwrap();
            path
        };
        let out = Command::new("git")
            .arg("merge-file")
            .arg("-p")
            .arg(file("ours", ours))
            .arg(file("base", base))
            .arg(file("theirs", theirs))
            .output()
            .unwrap();
        let ours_result = merge3::try_merge(base, ours, theirs);
        if out.status.success() {
            let text = String::from_utf8(out.stdout).unwrap();
            assert_eq!(ours_result.as_deref(), Ok(text.as_str()), "case {i}");
        } else {
            assert!(ours_result.is_err(), "case {i}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);

    // (g) Beyond git: two insertions at one place where one is the other plus more lines (a
    // branch built on one that has landed since) give the longer, once, either way round and on
    // a file both sides added.
    assert_eq!(
        merge3::try_merge("a\nb\n", "a\nb\nX1\nX2\nY1\n", "a\nb\nX1\nX2\n").as_deref(),
        Ok("a\nb\nX1\nX2\nY1\n")
    );
    assert_eq!(
        merge3::try_merge("a\nb\n", "a\nX\nb\n", "a\nY\nX\nb\n").as_deref(),
        Ok("a\nY\nX\nb\n")
    );
    assert_eq!(
        merge3::try_merge("", "one\ntwo\n", "one\n").as_deref(),
        Ok("one\ntwo\n")
    );

    // (h) With union_insertions (the smoke suite's area modules): two different insertions at
    // one place join, ours then theirs, when each closes its brackets; otherwise, or when they
    // share a run of lines, they are refused.
    let union = merge3::Options {
        union_insertions: true,
    };
    let base = "fn a() {}\n";
    let ours = format!("{base}\nfn b() {{\n    b();\n}}\n");
    let theirs = format!("{base}\nfn c() {{\n    c();\n}}\n");
    assert_eq!(
        merge3::try_merge_with(base, &ours, &theirs, union),
        Ok(format!(
            "{base}\nfn b() {{\n    b();\n}}\n\nfn c() {{\n    c();\n}}\n"
        ))
    );
    assert!(matches!(
        merge3::try_merge(base, &ours, &theirs),
        Err(Conflict::Overlap { .. })
    ));
    assert!(matches!(
        merge3::try_merge_with(base, &format!("{base}fn b() {{\n"), &theirs, union),
        Err(Conflict::Overlap { .. })
    ));
    let body = "    one();\n    two();\n    three();\n    four();\n}\n";
    assert!(matches!(
        merge3::try_merge_with(
            base,
            &format!("{base}\nfn b() {{\n{body}"),
            &format!("{base}\nfn c() {{\n{body}"),
            union
        ),
        Err(Conflict::Duplicate { .. })
    ));

    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
}

#[test]
fn land_reuses_a_green_test_for_the_same_commit() {
    use pigeon_capabilities::land::state::{self, Seen, State};
    // The test dispatch asks CI to reuse; the deploy dispatch does not.
    let src = include_str!("../../src/capabilities/src/land.rs");
    let test_at = src.find("\"job\": s.test_job").unwrap();
    let test_call = &src[test_at..test_at + 200];
    assert!(test_call.contains("\"reuse\": true"), "{test_call}");
    assert_eq!(src.matches("\"reuse\"").count(), 1);
    // A reused job gets its own words in the landing's detail.
    assert!(src.contains("format!(\"reused test job {job}\")"));
    assert!(src.contains("format!(\"test job {job} submitted\")"));
    // A landing goes on from testing to merging as it does off a new job: the job ended green.
    let passed = Seen::JobEnded { passed: true };
    assert_eq!(
        state::next(State::Testing, false, &passed),
        Some(State::Ready)
    );
    assert_eq!(
        state::next(State::Ready, false, &Seen::Start),
        Some(State::Merging)
    );
}

/// land (deploy scope): the map `ci/workers.toml` is checked against Cargo. For each Worker the
/// test derives the crates it builds, the files compiled in from outside their crates, its
/// wrangler.toml, assets and deploy script, and asserts the map's patterns (the Lander's own
/// `matches`) own every one of them. Every crate directory under `src/` must also be named.
#[test]
fn land_scope_map_matches_the_cargo_graph() {
    use pigeon_core::lanes::{self, matches};
    use std::collections::BTreeSet;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let map = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p:?}: {e}"));

    // The path dependencies of one crate's normal and build dependencies, as `src/<dir>`.
    let path_deps = |krate: &str| -> Vec<String> {
        let manifest: toml::Value = read(&root.join(krate).join("Cargo.toml")).parse().unwrap();
        let mut tables: Vec<&toml::Value> = Vec::new();
        for key in ["dependencies", "build-dependencies"] {
            tables.extend(manifest.get(key));
        }
        if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
            for t in targets.values() {
                for key in ["dependencies", "build-dependencies"] {
                    tables.extend(t.get(key));
                }
            }
        }
        let mut out = Vec::new();
        for table in tables {
            for dep in table.as_table().into_iter().flat_map(|t| t.values()) {
                if let Some(rel) = dep.get("path").and_then(|p| p.as_str()) {
                    let name = rel.trim_start_matches("../").trim_end_matches('/');
                    out.push(format!("src/{name}"));
                }
            }
        }
        out
    };
    // The Rust files under a directory.
    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    // Files a crate compiles in from outside its own directory, as paths from the root.
    let outside_includes = |krate: &str| -> Vec<String> {
        let mut files = Vec::new();
        rust_files(&root.join(krate).join("src"), &mut files);
        files.push(root.join(krate).join("build.rs"));
        let mut out = Vec::new();
        for f in files.iter().filter(|f| f.exists()) {
            let canon_root = root.canonicalize().unwrap();
            let rel = f
                .canonicalize()
                .unwrap()
                .strip_prefix(&canon_root)
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf();
            for line in read(f)
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
            {
                for key in ["include_str!(\"", "include_bytes!(\""] {
                    let Some(at) = line.find(key) else { continue };
                    let rest = &line[at + key.len()..];
                    let Some(end) = rest.find('"') else { continue };
                    let mut parts: Vec<String> = Vec::new();
                    for seg in rel.join(&rest[..end]).components() {
                        match seg.as_os_str().to_str().unwrap() {
                            ".." => {
                                parts.pop();
                            }
                            "." => {}
                            s => parts.push(s.to_string()),
                        }
                    }
                    let path = parts.join("/");
                    if !path.starts_with(&format!("{krate}/")) {
                        out.push(path);
                    }
                }
            }
        }
        out
    };

    for w in &map.workers {
        // The crates it builds: its own, and path dependencies, transitively.
        let mut crates: BTreeSet<String> = BTreeSet::new();
        let mut todo = vec![w.dir.clone()];
        while let Some(c) = todo.pop() {
            if crates.insert(c.clone()) {
                todo.extend(path_deps(&c));
            }
        }
        let mut needed: BTreeSet<String> = BTreeSet::new();
        for c in &crates {
            needed.insert(format!("{c}/Cargo.toml"));
            needed.insert(format!("{c}/src/lib.rs"));
            needed.extend(outside_includes(c));
        }
        // Its own config, assets and deploy script; an artifact lane deploys no Worker, so has
        // no config of its own.
        let wrangler = format!("{}/wrangler.toml", w.dir);
        let config: toml::Value = if w.is_worker() {
            needed.insert(wrangler.clone());
            read(&root.join(&wrangler)).parse().unwrap()
        } else {
            toml::Value::Table(Default::default())
        };
        if let Some(dir) = config
            .get("assets")
            .and_then(|a| a.get("directory"))
            .and_then(|d| d.as_str())
        {
            needed.insert(format!("{}/{}/x", w.dir, dir.trim_start_matches("./")));
        }
        let script = format!("ci/deploy/{}.sh", w.deploy_job);
        assert!(
            root.join(&script).exists(),
            "worker {}: no {script}",
            w.name
        );
        needed.insert(script);

        for path in needed {
            // A path matched by [shared] deploys every Worker, so it is owned too.
            let owned = w.paths.iter().chain(&map.shared).any(|p| matches(p, &path));
            assert!(
                owned,
                "worker {:?} builds {path:?}, but none of its paths (or [shared]) match it",
                w.name
            );
        }
    }

    // Every crate directory under src/ is named by some entry, so none silently counts as shared.
    let all: Vec<&String> = map
        .docs
        .iter()
        .chain(&map.shared)
        .chain(&map.checks)
        .chain(map.workers.iter().flat_map(|w| &w.paths))
        .collect();
    for e in std::fs::read_dir(root.join("src")).unwrap().flatten() {
        if !e.path().join("Cargo.toml").exists() {
            continue;
        }
        let probe = format!("src/{}/Cargo.toml", e.file_name().to_string_lossy());
        assert!(
            all.iter().any(|p| matches(p, &probe)),
            "{probe}: no entry of ci/workers.toml names this crate; add it to a Worker or [checks]"
        );
    }
}

/// land (deploy scope): representative paths on the Lander's real scope function.
#[test]
fn land_scope_pins_representative_paths() {
    use pigeon_core::lanes;

    let map = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    let scope = |path: &str| -> Vec<String> {
        let a = map.classify(&[path]);
        let mut names: Vec<String> = map
            .deploy_order(&a)
            .into_iter()
            .map(|w| w.name.clone())
            .collect();
        names.sort_by_key(|n| n != "core");
        names
    };
    // pigeon-ai-agents links pigeon-models too, so the Worker deploys with it (the plan's
    // example named only core and model-gateway; the Cargo graph says three).
    assert_eq!(
        scope("src/models/x.rs"),
        ["core", "model-gateway", "ai-agents"]
    );
    assert_eq!(scope("src/blog/x.rs"), ["core", "sites"]);
    assert_eq!(scope("src/proxies/x.rs"), ["core", "proxy"]);
    assert_eq!(scope("src/ci-gateway/x.rs"), ["ci-gateway"]);
    // The map counts src/ai-agents as core's (core's build script embeds every crate's sources
    // for the architecture page), so it deploys core and the Worker.
    assert_eq!(scope("src/ai-agents/x.rs"), ["core", "ai-agents"]);
    // pigeon-capabilities links pigeon-ingest, so core builds with it; so does the data Worker,
    // whose artifact lane builds and uploads it.
    assert_eq!(scope("src/ingest/x.rs"), ["core", "data-worker"]);
    // The data Worker and the email library it links: its artifact lane alone, no Worker.
    for path in ["src/data-worker/src/lib.rs", "src/email/x.rs"] {
        assert_eq!(scope(path), ["data-worker"], "{path}");
    }
    // Named as deploying nothing: not a platform Worker.
    let runner = map.classify(&["src/ingest-runner/x.rs"]);
    assert!(!runner.shared && runner.workers.is_empty());
    // Docs deploy nothing and need no tests.
    let docs = map.classify(&["docs/plans/landing-speed.md"]);
    assert!(docs.docs_only && !docs.shared && docs.workers.is_empty());
    // A path nothing names is still shared at run time, the safe side.
    assert!(map.classify(&["src/nothing-names-this/x.rs"]).shared);
}

/// The Lander takes Codex's branches as it takes Claude's (docs/plans/agent-git.md): `codex/*`
/// is a branch a landing takes, every agent namespace lands, an app acting for its person may
/// land, and the policy decides once the person adds `codex/*`.
#[test]
fn land_takes_codex_branches() {
    use pigeon_capabilities::land::{may_land, state};
    use pigeon_capabilities::source::push::NAMESPACES;
    use pigeon_core::policy::{Decision, Kind};
    assert_eq!(
        state::branch_ref("codex/fix").as_deref(),
        Ok("refs/heads/codex/fix")
    );
    assert_eq!(
        state::branch_ref("refs/heads/codex/a/b").as_deref(),
        Ok("refs/heads/codex/a/b")
    );
    for bad in ["codex/", "codex/../main", "codexx/y", "refs/heads/codex"] {
        assert!(state::branch_ref(bad).is_err(), "{bad}");
    }
    for (_, namespace) in NAMESPACES {
        assert!(
            state::PREFIXES.contains(&format!("refs/heads/{namespace}").as_str()),
            "{namespace} lands"
        );
    }
    // Codex, acting for its person, may land as Claude may.
    let codex: pigeon_capabilities::users::Caller = serde_json::from_value(json!({
        "user": { "id": "cx", "kind": "service", "name": "Codex", "created_at": 0 },
        "acting_for": { "id": "w", "kind": "standard", "name": "Willow", "created_at": 0 },
    }))
    .unwrap();
    assert!(may_land(&codex));
    // The Lander asks the policy (`land-and-deploy`), and the deploy after it.
    let with = super::policy::policy_with_codex();
    for kind in [Kind::Land, Kind::Deploy] {
        let a = with.evaluate(
            &vars(),
            &pigeon_core::policy_file::request("Codex", kind, "codex/fix"),
        );
        assert_eq!(a.decision, Decision::Allow, "{kind:?}");
    }
}

/// A Cloudflare rate limit is the environment, never the change: classified from the deploy's
/// last attempt only, deployed again after a doubling backoff a bounded number of times, then an
/// error that blames no landing and an incident recorded `rate_limited`.
#[test]
fn core_land_rate_limits_blame_no_landing() {
    use pigeon_capabilities::land::lanes::{
        self, EnvStep, RATE_LIMIT_REDEPLOYS, environment_fault, rate_limit_backoff_ms,
        rate_limit_step, rate_limited,
    };
    // What wrangler and the deploy script print for a rate limit.
    let wrangler = "\n== wrangler deploy through https://proxy (attempt 5)\n\u{2718} [ERROR] A request to the Cloudflare API (/accounts/a/workers/services/pigeon-proxy) failed.\n  Please wait and consider throttling your request speed [code: 10429]\nthe deploy failed; see above\n";
    assert_eq!(
        rate_limited(wrangler).as_deref(),
        Some("Cloudflare rate limit (code 10429)")
    );
    for tail in [
        "R2 REST API answered HTTP 429",
        "status: 429 Too Many Requests",
        "status code 429",
        "429 Too Many Requests",
        "the global API limit: rate limited",
    ] {
        assert!(rate_limited(tail).is_some(), "{tail}");
    }
    // Not a rate limit: other statuses and numbers that only start with 429.
    for tail in [
        "HTTP 4290 bytes",
        "status 404",
        "error[E0308]: mismatched types",
        "",
    ] {
        assert_eq!(rate_limited(tail), None, "{tail}");
    }
    // Only the last attempt counts: limited once, then a code failure, is the code's fault.
    let then_code = format!(
        "{wrangler}\n== wrangler deploy through https://proxy (attempt 2)\nUploaded pigeon-proxy\n\u{2718} [ERROR] Uncaught ReferenceError: x is not defined [code: 10021]\n"
    );
    assert_eq!(rate_limited(&then_code), None);
    assert_eq!(environment_fault("failed", Some(1), &then_code), None);
    // A failed job naming a rate limit is the environment; its reason says so.
    let why = environment_fault("failed", Some(1), wrangler).unwrap();
    assert!(why.starts_with(lanes::RATE_LIMIT), "{why}");
    // Not for a job that timed out.
    assert_eq!(environment_fault("timed_out", None, wrangler), None);

    // Backoff: 2, 4, 8, 16 minutes, then no more than 16.
    assert_eq!(
        (0..5).map(rate_limit_backoff_ms).collect::<Vec<_>>(),
        [120_000, 240_000, 480_000, 960_000, 960_000]
    );
    // Deployed again after the backoff until the redeploys are used up, then an error.
    assert_eq!(RATE_LIMIT_REDEPLOYS, 4);
    for r in 0..RATE_LIMIT_REDEPLOYS {
        assert_eq!(
            rate_limit_step(r, &why),
            EnvStep::RedeployAfter(rate_limit_backoff_ms(r))
        );
    }
    let EnvStep::Error(detail) = rate_limit_step(RATE_LIMIT_REDEPLOYS, &why) else {
        panic!("redeploys used up must end in error");
    };
    // The error is an environment failure (the landing ends `error`, "not this landing's
    // fault"), and a rate limit in particular (its incident is `rate_limited`).
    assert!(lanes::is_environment_detail(&detail));
    assert!(lanes::is_rate_limit_detail(&detail), "{detail}");
    assert!(detail.contains("not this landing's fault"));
    assert!(!lanes::is_rate_limit_detail(&lanes::environment_detail(
        "Cloudflare error code 10007"
    )));
    // A rate-limited incident never blames: a later green deploy closes it like an unisolated one.
    assert!(lanes::incident_covered(lanes::RATE_LIMITED, 5, 3, 9, 4));
    assert!(!lanes::incident_covered(lanes::RATE_LIMITED, 5, 3, 4, 4));
    // The lane deploy that ran out of redeploys ends `error`.
    let lane_failed = [lanes::Deploy {
        worker: "proxy".into(),
        covers: 10,
        state: lanes::DeployState::Error,
        incident: false,
    }];
    let merged = lanes::Merged {
        id: "L".into(),
        merged_at: 10,
        commit: "c".into(),
        workers: vec!["proxy".into()],
        ordered: false,
        proxy_first: false,
    };
    // Its landing ends `error`, never `deploy_failed`.
    assert_eq!(
        lanes::outcome(&merged, &lane_failed),
        Some(pigeon_capabilities::land::state::Seen::Failed)
    );
}

/// An approval is bound to the digest of the landing's change to the guarded paths. The
/// Lander's own rebase keeps it only when that change is the same afterwards; a text merge into
/// a guarded file, a mode change, or an approval with no recorded digest voids it.
#[test]
fn land_approval_holds_across_a_rebase_only_for_the_same_gated_change() {
    use pigeon_capabilities::land::merge::{entry_at, gated_patch};
    use pigeon_capabilities::land::state::{
        TEST_RETRIES, approval_holds, gated_digest, is_guarded, test_rerun,
    };
    use pigeon_core::{merge3, pack, rebase};

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
    let objects_of = |dir: &Path, tips: &[&str]| {
        let packed = git(
            dir,
            &["pack-objects", "--stdout", "--revs", "-q"],
            Some(&(tips.join("\n") + "\n")),
        );
        pack::objects(&packed).unwrap()
    };
    let map = "[docs]\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\n";

    assert!(is_guarded(".pigeon/policy.toml"));
    assert!(is_guarded("ci/workers.toml"));
    assert!(is_guarded("ci/deploy/deploy.sh"));
    assert!(!is_guarded("src/core/src/lib.rs"));
    assert!(!is_guarded("cinema/x"));

    let dir = repo("approval-digest");
    let base = change(
        &dir,
        &[
            (".pigeon/policy.toml", Some("rule = 1\n")),
            ("ci/workers.toml", Some(map)),
            ("src/a.rs", Some("a\n")),
            ("docs/x.md", Some("x\n")),
        ],
        "base",
    );
    git(&dir, &["checkout", "-q", "-b", "claude/policy"], None);
    let tip = change(
        &dir,
        &[
            (".pigeon/policy.toml", Some("rule = 2\n")),
            ("ci/workers.toml", Some(&map.replace("line 2", "branch"))),
            ("src/a.rs", Some("a on the branch\n")),
        ],
        "the branch",
    );
    // Main moves in a path the landing does not change, then in a guarded one it does.
    git(&dir, &["checkout", "-q", "main"], None);
    let main1 = change(
        &dir,
        &[
            ("docs/x.md", Some("x on main\n")),
            ("src/b.rs", Some("b\n")),
        ],
        "main moved elsewhere",
    );
    let main2 = change(
        &dir,
        &[("ci/workers.toml", Some(&map.replace("line 5", "main")))],
        "main moved the map",
    );

    let objects = objects_of(&dir, &[&tip, &main1, &main2]);
    let changed = rebase::changes(&objects, &base, &tip).unwrap();
    // As checked: the digest the person's approval is bound to.
    let patch = gated_patch(&objects, &base, &changed).unwrap();
    assert_eq!(
        patch.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(),
        [".pigeon/policy.toml", "ci/workers.toml"],
        "only guarded files, never src/"
    );
    let approved = gated_digest(&patch);
    assert_eq!(approved.len(), 64);
    assert_ne!(approved, gated_digest(&[]));

    // Rebased onto main1 (main moved in docs and src/b.rs only): the same gated change, so the
    // approval holds.
    let onto1 = gated_digest(&gated_patch(&objects, &main1, &changed).unwrap());
    assert_eq!(onto1, approved);
    assert!(approval_holds(true, Some(&approved), Some(&onto1)));

    // Rebased onto main2, which changed the map too: the Lander merges the map line by line, so
    // the change to a guarded file differs and the approval is void.
    let ours = map.replace("line 2", "branch");
    let theirs = map.replace("line 5", "main");
    let joined = merge3::merge(map, &ours, &theirs).unwrap();
    let mut onto_main2 = changed.clone();
    onto_main2.insert(
        "ci/workers.toml".into(),
        Some((
            "100644".into(),
            pigeon_core::gitobj::Object::blob(joined.as_bytes()).id(),
        )),
    );
    let onto2 = gated_digest(&gated_patch(&objects, &main2, &onto_main2).unwrap());
    assert_ne!(onto2, approved);
    assert!(!approval_holds(true, Some(&approved), Some(&onto2)));

    // The same content with another mode (a script made executable) is another change.
    let mut moded = changed.clone();
    let (_, id) = moded[".pigeon/policy.toml"].clone().unwrap();
    moded.insert(".pigeon/policy.toml".into(), Some(("100755".into(), id)));
    let m = gated_digest(&gated_patch(&objects, &main1, &moded).unwrap());
    assert_ne!(m, approved);
    // Deleting a guarded file is a change too, and differs from changing it.
    let mut deleted = changed.clone();
    deleted.insert(".pigeon/policy.toml".into(), None);
    assert_ne!(
        gated_digest(&gated_patch(&objects, &main1, &deleted).unwrap()),
        approved
    );
    // The order of the patch does not matter; an entry whose before equals its after is no
    // change.
    let mut reversed = patch.clone();
    reversed.reverse();
    assert_eq!(gated_digest(&reversed), approved);
    let mut padded = patch.clone();
    let same = Some(("100644".to_string(), "0".repeat(40)));
    padded.push(("ci/x".into(), same.clone(), same));
    assert_eq!(gated_digest(&padded), approved);

    // Fails closed: no approval, no recorded digest (an approval from before digests), or a
    // rebased digest that could not be read never hold.
    assert!(!approval_holds(false, Some(&approved), Some(&approved)));
    assert!(!approval_holds(true, None, Some(&approved)));
    assert!(!approval_holds(true, Some(&approved), None));

    // entry_at reads modes and ids, and nothing where nothing is.
    let (mode, _) = entry_at(&objects, &tip, ".pigeon/policy.toml")
        .unwrap()
        .unwrap();
    assert_eq!(mode, "100644");
    assert_eq!(entry_at(&objects, &tip, "nope/x").unwrap(), None);
    assert_eq!(entry_at(&objects, &tip, "src/a.rs/x").unwrap(), None);
    assert!(entry_at(&objects, &tip, "../x").is_err());

    // A test whose runner was lost is run once more at the same commit; a failed test is not.
    assert_eq!(TEST_RETRIES, 1);
    assert!(test_rerun("error", 0));
    assert!(!test_rerun("error", TEST_RETRIES));
    assert!(!test_rerun("failed", 0));
    assert!(!test_rerun("timed_out", 0));
    assert!(!test_rerun("cancelled", 0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Worker whose entry and source a commit removes is skipped there, never deployed; one whose
/// source is gone but whose entry stays is still deployed, so its failure is the change's.
#[test]
fn land_skips_workers_removed_at_the_commit() {
    use pigeon_capabilities::land::lanes::{is_skipped_detail, removed_at, skipped_detail};
    use pigeon_core::lanes;
    let main = lanes::parse(include_str!("../../ci/workers.toml")).unwrap();
    let extra = "\n[[worker]]\nname = \"dotest-a\"\nscript = \"pigeon-dotest-a\"\ndir = \"src/dotest-a\"\npaths = [\"src/dotest-a/*\"]\ntest_job = \"test\"\ndeploy_job = \"deploy-dotest-a\"\n";
    let with = lanes::parse(&format!("{}{extra}", include_str!("../../ci/workers.toml"))).unwrap();
    assert!(with.worker("dotest-a").is_some());
    // A shared change classified with the old map deploys the throwaway Worker too.
    assert!(with.classify(&["Cargo.toml"]).workers.contains("dotest-a"));

    // Removed: no entry at the commit and no directory.
    let why = removed_at(Some(&main), "dotest-a", Some("src/dotest-a"), Some(false)).unwrap();
    assert!(
        why.contains("dotest-a") && why.contains("src/dotest-a/"),
        "{why}"
    );
    // Removed: no map the Lander knows names it.
    assert!(removed_at(Some(&main), "dotest-a", None, None).is_some());
    // Not removed: its entry stays (its failure is the change's), or its source is still there,
    // or the commit's map could not be read.
    assert_eq!(
        removed_at(Some(&with), "dotest-a", Some("src/dotest-a"), Some(false)),
        None
    );
    assert_eq!(
        removed_at(Some(&main), "dotest-a", Some("src/dotest-a"), Some(true)),
        None
    );
    assert_eq!(
        removed_at(None, "dotest-a", Some("src/dotest-a"), Some(false)),
        None
    );
    // Every Worker in the map is not removed.
    for w in &main.workers {
        assert_eq!(
            removed_at(Some(&main), &w.name, Some(&w.dir), Some(false)),
            None
        );
    }
    // A skipped deploy says so, and is told apart from any other detail.
    let detail = skipped_detail(&why);
    assert!(is_skipped_detail(&detail));
    assert!(!is_skipped_detail("proxy: CI job j: succeeded"));

    // A skipped deploy ends `succeeded`: it covers the landings it carries, so they land.
    use pigeon_capabilities::land::lanes::{Deploy, DeployState, Merged, outcome};
    let landing = Merged {
        id: "L".into(),
        merged_at: 10,
        commit: "c".into(),
        workers: vec!["core".into(), "dotest-a".into()],
        ordered: false,
        proxy_first: false,
    };
    let deploys = [
        Deploy {
            worker: "core".into(),
            covers: 10,
            state: DeployState::Succeeded,
            incident: false,
        },
        Deploy {
            worker: "dotest-a".into(),
            covers: 10,
            state: DeployState::Succeeded,
            incident: false,
        },
    ];
    assert_eq!(
        outcome(&landing, &deploys),
        Some(pigeon_capabilities::land::state::Seen::JobEnded { passed: true })
    );
}

#[test]
fn managed_ingest_grants_limit_keys_authentication_and_writes() {
    use pigeon_ingest::{blob_key, datastore, managed_destination as managed, manifest};

    let now = 1_000_000;
    let grant = managed::Grant {
        instance: "account/example".into(),
        run: "00112233445566778899aabbccddeeff".into(),
        issued_ms: now,
        expires_ms: now + 1000,
        dry_run: true,
        max_bytes: 10,
        max_objects: 2,
    };
    let secret = b"D1-only test credential";
    let token = managed::issue(&grant, secret, now).unwrap();
    assert_eq!(
        managed::verify(&token, secret, &grant.instance, now),
        Some(grant.clone())
    );
    assert!(managed::verify(&token, secret, "other", now).is_none());
    assert!(managed::verify(&token, b"wrong", &grant.instance, now).is_none());
    assert!(managed::verify(&token, secret, &grant.instance, grant.expires_ms).is_none());
    let replaced = format!("00{}", &token[2..]);
    assert!(managed::verify(&replaced, secret, &grant.instance, now).is_none());

    let sha = managed::hashes(b"same bytes").0;
    assert!(managed::payload_key(&grant.run, &blob_key(&sha), &sha));
    assert!(!managed::payload_key(
        &grant.run,
        &blob_key(&sha),
        &"0".repeat(64)
    ));
    assert!(!managed::payload_read_key(
        &grant.run,
        &manifest::quarantine_key("other", &sha)
    ));
    assert!(managed::control_key(&grant.run, &blob_key(&sha)).is_none());
    assert!(managed::control_key(&grant.run, &manifest::job_key(&grant.run)).is_none());
    assert_eq!(
        managed::control_key(&grant.run, &manifest::checkpoint_key(&grant.run)),
        Some(false)
    );
    assert_eq!(
        managed::control_key(&grant.run, &manifest::part_key(&grant.run, 14)),
        Some(true)
    );
    assert!(managed::control_key(&grant.run, &manifest::report_key("other")).is_none());
    assert!(managed::valid_origin("https://data.example.workers.dev"));
    assert!(!managed::valid_origin(
        "https://data.example.workers.dev:443"
    ));
    assert!(!managed::valid_origin("https://example.test@evil.test"));

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let migration = datastore::MIGRATIONS
        .iter()
        .find(|m| m.id == "0005_ingest_grants")
        .unwrap();
    conn.execute_batch(migration.sql).unwrap();
    let charge = |bytes: u64, expires: i64| {
        conn.query_row(
            managed::CHARGE_SQL,
            rusqlite::params![
                grant.run,
                expires,
                bytes,
                grant.max_bytes,
                grant.max_objects
            ],
            |row| row.get::<_, u64>(0),
        )
    };
    assert_eq!(charge(6, grant.expires_ms).unwrap(), 1);
    assert!(matches!(
        charge(5, grant.expires_ms),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert!(matches!(
        charge(4, grant.expires_ms + 1),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
    assert_eq!(charge(4, grant.expires_ms).unwrap(), 2);
    assert!(matches!(
        charge(0, grant.expires_ms),
        Err(rusqlite::Error::QueryReturnedNoRows)
    ));
}

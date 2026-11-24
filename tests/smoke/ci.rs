//! CI routing, connectors and the Buildkite pipelines and setup.

use super::*;

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
fn ci_route_and_run_on_a_fake_connector() {
    use pigeon_ci::Dispatched;
    use pigeon_ci::class::{Class, Offered, Size};
    use pigeon_ci::config::{self, Arch, Os};
    use pigeon_ci::route::{Ask, Availability, Candidate, choose, fallback_due, route};
    use pigeon_ci::scheduler::{self, Outcome};
    use pigeon_ci::state::{self, Next, Report, State};
    // The repository's own jobs parse, and `test` asks for a medium machine.
    let cfg = config::parse(include_str!("../../.pigeon/ci.toml")).unwrap();
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
fn ci_scripts_print_timing_lines() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read =
        |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
    let test = read("ci/runner/test.sh");
    for (name, text) in [
        ("test.sh", &test),
        ("deploy.sh", &read("ci/deploy/deploy.sh")),
        ("allowlist.sh", &read("ci/deploy/allowlist.sh")),
    ] {
        assert!(text.contains("[timing] "), "{name} prints no timing line");
    }
    for stage in [
        "guard",
        "fmt",
        "clippy",
        "smoke-build",
        "smoke-run",
        "settings",
        "check-platform",
    ] {
        assert!(
            test.contains(&format!("step {stage} ")),
            "test.sh lacks stage {stage}"
        );
    }
    assert!(test.contains("export CARGO_INCREMENTAL=0"));
}

#[test]
fn ci_reads_the_pinned_runner_image() {
    use pigeon_ci::reuse::{base_image, runner_image};
    let digest = "a".repeat(64);
    let good = format!(
        "[[containers]]\nclass_name = \"X\"\n\n[containers.images.base]\nimage = \"reg/base@sha256:{digest}\"\n"
    );
    assert_eq!(
        base_image(&good).as_deref(),
        Some(format!("reg/base@sha256:{digest}").as_str())
    );
    // No digest, a short digest, another key and a missing section all give nothing.
    for text in [
        "[[containers]]\n[containers.images.base]\nimage = \"reg/base:latest\"\n".to_string(),
        "[[containers]]\n[containers.images.base]\nimage = \"reg/base@sha256:abc\"\n".to_string(),
        format!("[[containers]]\n[containers.images.other]\nimage = \"r@sha256:{digest}\"\n"),
        "[[containers]]\nclass_name = \"X\"\n".to_string(),
        "name = \"x\"\n".to_string(),
        "not toml [".to_string(),
    ] {
        assert_eq!(base_image(&text), None, "{text}");
    }
    // The repository's own wrangler.toml pins one.
    assert!(runner_image().is_some_and(|i| i.contains("@sha256:")));
}

#[test]
fn ci_reuses_only_the_same_green_job() {
    use pigeon_ci::reuse::{Candidate, Want, reusable};
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let image = "reg/base@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    let job = |id: &str| Candidate {
        id: id.into(),
        repo: "pigeon".into(),
        commit: sha.into(),
        name: "test".into(),
        state: "succeeded".into(),
        image: Some(image.into()),
    };
    let want = Want {
        repo: "pigeon".into(),
        commit: sha.into(),
        name: "test".into(),
        image: image.into(),
    };
    // The first match (the newest) is the one reused.
    let list = [job("new"), job("old")];
    assert_eq!(reusable(&list, &want).map(|c| c.id.as_str()), Some("new"));
    let none = |c: Candidate| assert!(reusable(&[c], &want).is_none());
    none(Candidate {
        state: "failed".into(),
        ..job("x")
    });
    none(Candidate {
        state: "running".into(),
        ..job("x")
    });
    none(Candidate {
        state: "cancelled".into(),
        ..job("x")
    });
    none(Candidate {
        commit: "f".repeat(40),
        ..job("x")
    });
    none(Candidate {
        commit: sha[..12].into(),
        ..job("x")
    });
    none(Candidate {
        commit: sha.to_uppercase(),
        ..job("x")
    });
    none(Candidate {
        image: None,
        ..job("x")
    });
    none(Candidate {
        image: Some("reg/base@sha256:2222".into()),
        ..job("x")
    });
    none(Candidate {
        name: "deploy".into(),
        ..job("x")
    });
    none(Candidate {
        repo: "other".into(),
        ..job("x")
    });
    // A want that is not a full lowercase sha, or has no image, reuses nothing.
    for bad in [sha.to_uppercase(), sha[..12].to_string()] {
        let w = Want {
            commit: bad,
            ..want.clone()
        };
        assert!(reusable(&[job("x")], &w).is_none());
    }
    let w = Want {
        image: String::new(),
        ..want.clone()
    };
    assert!(
        reusable(
            &[Candidate {
                image: Some(String::new()),
                ..job("x")
            }],
            &w
        )
        .is_none()
    );
    // The tool takes `reuse` as a boolean.
    let ci = pigeon_ci::Ci {
        settings: pigeon_ci::pigeon::settings(pigeon_ci::Accounts::Parent("P".into())),
    };
    let tools = ci.tools();
    let submit = tools.iter().find(|t| t.name == "ci_submit").unwrap();
    assert_eq!(
        submit.input_schema["properties"]["reuse"]["type"],
        "boolean"
    );
}

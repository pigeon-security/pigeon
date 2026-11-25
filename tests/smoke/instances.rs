//! Instances and versions: pins, plans and upgrades.

use super::*;

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

/// Instance pins: a record carries `pins` (absent when empty, so old records digest as before),
/// the diff shows each pin, unknown names and bad requirements are problems, and
/// `capabilities_versions` and `capabilities_upgrade_plan` answer from fake components.
#[test]
fn core_instances_pins_checked_and_planned() {
    use pigeon_capabilities::instances::record::{self, Instance, diff};
    use pigeon_capabilities::versioning::{upgrade_view, versions_view};
    use pigeon_core::versions::{Component, Kind, pin_problems};
    use std::collections::BTreeMap;

    let base = json!({
        "id": "example-a", "title": "Example A",
        "account": { "kind": "home" }, "zones": ["example.test"]
    });

    // A record with pins parses; other unknown keys are still refused.
    let mut with_pins = base.clone();
    with_pins["pins"] = json!({ "blog": "^1" });
    let pinned: Instance = serde_json::from_value(with_pins).unwrap();
    assert_eq!(pinned.pins.get("blog").map(String::as_str), Some("^1"));
    let mut odd = base.clone();
    odd["surprise"] = json!(1);
    assert!(serde_json::from_value::<Instance>(odd).is_err());

    // Without pins the JSON has no `pins` key, and the digest is that of the same JSON text.
    let plain: Instance = serde_json::from_value(base).unwrap();
    let text = serde_json::to_string(&plain).unwrap();
    assert!(!text.contains("pins"), "{text}");
    assert_eq!(record::digest(&plain), pigeon_core::oauth::hash(&text));

    // The diff shows the pin by itself.
    let changes = diff(Some(&plain), &pinned);
    let pin = changes.iter().find(|c| c.path == "pins.blog").unwrap();
    assert_eq!(pin.from, Value::Null);
    assert_eq!(pin.to, json!("^1"));

    // Fake components: `sites` serves 1.0.0 and 2.0.0 (2.0.0 is breaking), `blog` serves only
    // 2.0.0 (breaking), `ci` is declared twice and the first wins.
    let sites = Component::began("sites", Kind::Capability)
        .breaking("2.0.0", "new shape", "rename the field")
        .serving(&["2.0.0", "1.0.0"]);
    let blog =
        Component::began("blog", Kind::Template).breaking("2.0.0", "new layout", "move the config");
    let ci = Component::began("ci", Kind::Capability);
    let components = vec![
        (sites, "example-core".to_string()),
        (blog, "example-core".to_string()),
        (ci.clone(), "example-core".to_string()),
        (ci, "example-ci".to_string()),
    ];
    let carried: Vec<Component> = components.iter().map(|(c, _)| c.clone()).collect();

    let pins = |p: &[(&str, &str)]| -> BTreeMap<String, String> {
        p.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    assert!(!pin_problems(&pins(&[("nope", "^1")]), &carried).is_empty());
    assert!(!pin_problems(&pins(&[("blog", "not a version")]), &carried).is_empty());
    assert!(pin_problems(&pins(&[("blog", "^1")]), &carried).is_empty());

    let mut a = plain.clone();
    a.pins = pins(&[("blog", "^1"), ("sites", "^1")]);
    let mut b = plain.clone();
    b.id = "example-b".into();
    b.pins = pins(&[("blog", "^2")]);
    let c = {
        let mut c = plain.clone();
        c.id = "example-c".into();
        c
    };
    let instances = vec![a, b, c];

    let undeclared = vec!["legacy".to_string()];
    let v = versions_view(&components, &undeclared, &instances, false);
    let names: Vec<&str> = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["sites", "blog", "ci"]);
    assert_eq!(v["components"][2]["worker"], "example-core");
    assert_eq!(v["components"][1]["kind"], "template");
    assert!(v["components"][0].get("changelog").is_none());
    assert_eq!(v["undeclared"], json!(["legacy"]));
    assert_eq!(v["instances"].as_array().unwrap().len(), 3);
    assert_eq!(v["instances"][2]["pins"], json!([]));
    assert_eq!(v["instances"][0]["pins"][0]["component"], "blog");
    assert_eq!(v["instances"][0]["pins"][0]["state"], "held");
    assert_eq!(v["instances"][1]["pins"][0]["state"], "ok");
    let with_log = versions_view(&components, &undeclared, &instances, true);
    assert!(with_log["components"][0]["changelog"].is_array());

    let plan = upgrade_view(&components, &instances);
    assert_eq!(plan["instances"][0]["id"], "example-a");
    let steps = plan["instances"][0]["steps"].as_array().unwrap();
    let step_names: Vec<&str> = steps
        .iter()
        .map(|s| s["component"].as_str().unwrap())
        .collect();
    assert_eq!(step_names, ["blog", "sites"]);
    assert_eq!(steps[1]["from"], "1.0.0");
    assert_eq!(steps[1]["to"], "2.0.0");
    assert_eq!(steps[1]["suggested"], "^2.0.0");
    assert_eq!(steps[1]["work"][0]["migration"], "rename the field");
    assert_eq!(plan["instances"][0]["held"], json!(["blog"]));
    assert_eq!(plan["instances"][1]["steps"], json!([]));
    assert_eq!(plan["instances"][1]["held"], json!([]));
    assert!(plan["note"].as_str().unwrap().contains("instance_propose"));
}

/// `instances_tabs` gives the home instance the full tabs and a linked one fewer.
#[test]
fn instances_tabs_for_the_app() {
    use pigeon_capabilities::instances::record::Instance;
    use pigeon_capabilities::instances::tabs;

    let home: Instance = serde_json::from_value(json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"],
        "repo": { "kind": "artifacts", "namespace": "example", "repo": "platform" }
    }))
    .unwrap();
    let linked: Instance = serde_json::from_value(json!({
        "id": "sample", "title": "Sample", "account": { "kind": "linked" },
        "repo": { "kind": "github", "full_name": "sample/site" },
        "sites": [{ "name": "blog", "bucket": "sample-site",
            "targets": [{ "kind": "domain", "hostname": "blog.example.com" }] }]
    }))
    .unwrap();
    let out = tabs(&[linked.clone(), home]);
    assert_eq!(out["current"], "example/platform");
    assert_eq!(out["scoping"], "app");
    assert_eq!(out["instances"][0]["account_kind"], "linked");
    assert_eq!(out["instances"][0]["tabs"], json!(["home", "instances"]));
    assert_eq!(out["instances"][0]["settings"], false);
    assert_eq!(
        out["instances"][0]["facts"]["website"],
        "https://blog.example.com"
    );
    assert_eq!(out["instances"][0]["facts"]["site"], "blog.example.com");
    assert_eq!(out["instances"][0]["facts"]["repository"], "sample/site");
    assert_eq!(
        out["instances"][1]["tabs"],
        json!(["approvals", "status", "cost", "instances"])
    );
    assert_eq!(out["instances"][1]["settings"], true);
    assert_eq!(
        out["instances"][1]["facts"]["website"],
        "https://example.com"
    );
    assert!(out["instances"][1]["facts"]["site"].is_null());
    assert_eq!(
        out["instances"][1]["facts"]["repository"],
        "example/platform"
    );
    assert!(tabs(&[linked])["current"].is_null());
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

/// `pigeon_core::versions`: semver versions and Cargo's requirements, a declaration's rules
/// (changelog newest first, breaking entries move MAJOR and name their migration), pins checked
/// against what is served (ok, held, unknown, invalid), and the upgrade plan's work items.
#[test]
fn core_versions_semver_pins_and_upgrades() {
    use pigeon_core::versions::{
        Component, Kind, PinState, Requirement, Version, pin_problems, pin_status, upgrade,
    };
    use std::collections::BTreeMap;

    // Versions: three numbers, ordered, shown as written.
    let v = |s: &str| Version::parse(s).unwrap();
    assert_eq!(v("1.2.3"), Version::new(1, 2, 3));
    assert!(v("1.10.0") > v("1.9.9"));
    assert_eq!(v("2.0.1").to_string(), "2.0.1");
    for bad in [
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.x.3",
        "",
        "v1.2.3",
        "1.2.3-beta",
    ] {
        assert!(Version::parse(bad).is_err(), "{bad} is not a version");
    }
    assert_eq!(serde_json::to_value(v("1.2.3")).unwrap(), json!("1.2.3"));

    // Requirements: Cargo's grammar; bare means ^.
    let m = |r: &str, ver: &str| Requirement::parse(r).unwrap().matches(&v(ver));
    assert!(m("=1.2.3", "1.2.3") && !m("=1.2.3", "1.2.4"));
    assert!(m("^1.2", "1.9.0") && !m("^1.2", "1.1.9") && !m("^1.2", "2.0.0"));
    assert!(m("1.2.3", "1.4.0") && !m("1.2.3", "2.0.0"), "bare is ^");
    assert!(m("2.0", "2.5.1") && !m("2.0", "3.0.0"));
    assert!(m("3", "3.0.0") && !m("3", "4.0.0") && !m("3", "2.9.9"));
    assert!(m("~1.2", "1.2.9") && !m("~1.2", "1.3.0"));
    assert!(m("~1", "1.9.0") && !m("~1", "2.0.0"));
    assert!(
        m("^0.2", "0.2.5") && !m("^0.2", "0.3.0"),
        "below 1.0.0, MINOR breaks"
    );
    assert!(m("^0.0.3", "0.0.3") && !m("^0.0.3", "0.0.4"));
    assert!(m("*", "9.9.9"));
    for bad in ["", "^", "=1.2", "1.2.3.4", ">=1.0", "latest", "^1.x"] {
        assert!(
            Requirement::parse(bad).is_err(),
            "{bad} is not a requirement"
        );
    }
    assert_eq!(Requirement::parse(" ^1.2 ").unwrap().to_string(), "^1.2");

    // Declarations: began at 1.0.0, then entries newest last; the rules hold.
    let blog = Component::began("blog", Kind::Template)
        .entry("1.1.0", "Dark look.")
        .breaking(
            "2.0.0",
            "[site.look] is [site.theme].",
            "Rename [site.look] to [site.theme] in .pigeon/site.toml; template = \"blog@2\".",
        )
        .entry("2.0.1", "A fix.")
        .breaking(
            "3.0.0",
            "Posts move to /p/.",
            "Add aliases for every post's old URL.",
        )
        .serving(&["3.0.0", "2.0.1", "1.1.0"]);
    assert_eq!(blog.version, "3.0.0");
    assert_eq!(blog.changelog[0].version, "3.0.0", "newest first");
    assert!(blog.problems().is_empty(), "{:?}", blog.problems());
    assert_eq!(blog.served(), vec![v("3.0.0"), v("2.0.1"), v("1.1.0")]);
    let began = Component::began("instances", Kind::Capability);
    assert_eq!(began.version, "1.0.0");
    assert!(began.problems().is_empty());

    let broken = Component::began("x", Kind::Connector).entry("2.0.0", "Silently breaking.");
    assert!(
        broken
            .problems()
            .iter()
            .any(|p| p.contains("marked breaking")),
        "{:?}",
        broken.problems()
    );
    let mut no_migration = Component::began("x", Kind::Connector).breaking("2.0.0", "B.", "m");
    no_migration.changelog[0].migration = None;
    assert!(
        no_migration
            .problems()
            .iter()
            .any(|p| p.contains("migration"))
    );
    let fake_break = Component::began("x", Kind::Connector).breaking("1.1.0", "B.", "m");
    assert!(
        fake_break
            .problems()
            .iter()
            .any(|p| p.contains("moves MAJOR"))
    );
    let mut out_of_order = Component::began("x", Kind::Connector).entry("1.1.0", "A.");
    out_of_order.changelog.swap(0, 1);
    out_of_order.version = "1.0.0".into();
    assert!(
        out_of_order
            .problems()
            .iter()
            .any(|p| p.contains("newest first"))
    );
    let mut mismatch = Component::began("x", Kind::Connector);
    mismatch.version = "1.0.1".into();
    assert!(
        mismatch
            .problems()
            .iter()
            .any(|p| p.contains("newest changelog entry"))
    );
    assert!(
        Component::began("Bad Name", Kind::Capability)
            .problems()
            .iter()
            .any(|p| p.contains("name"))
    );
    let unserved = Component::began("x", Kind::Template)
        .entry("1.1.0", "A.")
        .serving(&["1.0.0"]);
    assert!(unserved.problems().iter().any(|p| p.contains("serves")));

    // Wire form: what a capability Worker's manifest carries, read back the same.
    let wire = serde_json::to_value(&blog).unwrap();
    assert_eq!(wire["kind"], "template");
    assert_eq!(wire["changelog"][0]["breaking"], true);
    assert!(
        wire["changelog"][1].get("breaking").is_none(),
        "false is left out"
    );
    let back: Component = serde_json::from_value(wire).unwrap();
    assert_eq!(back, blog);

    // Pins: the person's example, as noisypigeon and willowgraysen would pin.
    let sites = Component::began("sites", Kind::Capability)
        .entry("1.2.3", "Deploy records.")
        .breaking(
            "2.0.0",
            "site_deploy takes a site name.",
            "Name the site in each call.",
        );
    let carried = vec![sites.clone(), blog.clone(), began.clone()];
    let pins = |p: &[(&str, &str)]| -> BTreeMap<String, String> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    let noisy = pins(&[("sites", "=1.2.3"), ("blog", "=2.0.1")]);
    let willow = pins(&[("sites", "2.0"), ("blog", "3")]);
    assert!(pin_problems(&noisy, &carried).is_empty());
    assert!(pin_problems(&willow, &carried).is_empty());

    let st = pin_status(&noisy, &carried);
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].component, "blog", "in name order");
    assert_eq!(st[0].state, PinState::Ok);
    assert_eq!(
        st[0].selected.as_deref(),
        Some("2.0.1"),
        "an older served template"
    );
    assert_eq!(st[1].component, "sites");
    assert_eq!(st[1].state, PinState::Held, "one live sites version, 2.0.0");
    assert_eq!(st[1].work.len(), 1);
    assert_eq!(st[1].work[0].version, "2.0.0");
    assert!(st[1].note.contains("refuse"));
    let st = pin_status(&willow, &carried);
    assert!(st.iter().all(|s| s.state == PinState::Ok));
    assert_eq!(st[0].selected.as_deref(), Some("3.0.0"));

    let odd = pins(&[("nope", "^1"), ("blog", "latest")]);
    let problems = pin_problems(&odd, &carried);
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("pins.nope") && p.contains("blog, instances, sites"))
    );
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("pins.blog") && p.contains("not a requirement"))
    );
    let st = pin_status(&odd, &carried);
    assert_eq!(st[0].state, PinState::Invalid);
    assert_eq!(st[1].state, PinState::Unknown);

    // Upgrades: what each pin needs to reach the newest version, breaking work oldest first.
    let up = upgrade(&noisy, &carried);
    assert_eq!(up.len(), 2);
    assert_eq!(up[0].component, "blog");
    assert_eq!(up[0].from.as_deref(), Some("2.0.1"));
    assert_eq!(up[0].to, "3.0.0");
    assert_eq!(up[0].suggested, "^3.0.0");
    assert_eq!(up[0].work.len(), 1);
    assert_eq!(up[0].work[0].version, "3.0.0");
    assert!(
        up[0].work[0]
            .migration
            .as_deref()
            .unwrap()
            .contains("aliases")
    );
    assert_eq!(up[1].component, "sites");
    assert_eq!(up[1].from, None, "held: nothing served allows =1.2.3");
    assert_eq!(up[1].work[0].version, "2.0.0");
    assert!(
        upgrade(&willow, &carried).is_empty(),
        "already on the newest"
    );
    let old = pins(&[("blog", "^1")]);
    let up = upgrade(&old, &carried);
    let work: Vec<&str> = up[0].work.iter().map(|e| e.version.as_str()).collect();
    assert_eq!(
        work,
        ["2.0.0", "3.0.0"],
        "both breaking entries, oldest first"
    );
    assert_eq!(up[0].from.as_deref(), Some("1.1.0"));
    let safe = pins(&[("sites", "=2.0.0")]);
    assert!(upgrade(&safe, &carried).is_empty());
    let patch = pins(&[("blog", "=2.0.1")]);
    let one = vec![Component::began("blog", Kind::Template).entry("1.0.1", "Fix.")];
    let up = upgrade(&pins(&[("blog", "=1.0.0")]), &one);
    assert!(up[0].work.is_empty(), "a fix needs no work");
    assert!(
        upgrade(&patch, &one).is_empty(),
        "held above what is served: nothing to move up to"
    );
}

/// Every capability in `src/capabilities` declares its version (a source check), and the
/// declarations the suite can build natively (CI, models, AI agents, storage, linked,
/// instances) keep the rules: own component first, unique names, parts named `owner.part`.
#[test]
fn core_every_capability_declares_its_version() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::linked::Linked;
    use pigeon_capabilities::passkeys::Passkeys;
    use pigeon_capabilities::storage::Storage;
    use pigeon_core::versions::{Component, Kind};

    // Source coverage: every `impl Capability for` outside remote.rs has a `fn versions(`.
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !root.join("ci/workers.toml").exists() {
        assert!(root.pop(), "ci/workers.toml is above the checkout");
    }
    fn rust(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rust(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                out.push((p.display().to_string(), text));
            }
        }
    }
    let mut files = Vec::new();
    rust(&root.join("src/capabilities/src"), &mut files);
    assert!(!files.is_empty(), "no capability sources were found");
    let offenders: Vec<&str> = files
        .iter()
        .filter(|(p, _)| !p.ends_with("remote.rs"))
        .filter(|(_, t)| t.contains("impl Capability for") && !t.contains("fn versions("))
        .map(|(p, _)| p.as_str())
        .collect();
    assert!(
        offenders.is_empty(),
        "capabilities without fn versions: {offenders:?}"
    );

    // The declarations that build natively keep the rules.
    let storage = Storage { accounts: vec![] };
    let linked = Linked {
        host: "example.test".into(),
        passkeys: Passkeys {
            host: "example.test".into(),
            site_name: "pigeon".into(),
        },
    };
    let instances = pigeon_capabilities::instances::Instances {
        zone: "example.test".into(),
        passkeys: Passkeys {
            host: "example.test".into(),
            site_name: "pigeon".into(),
        },
    };
    let sets: Vec<(&str, Vec<Component>)> = vec![
        ("ci", pigeon_ci::versions()),
        ("models", pigeon_models::versions()),
        ("ai-agents", pigeon_ai_agents::versions()),
        ("storage", storage.versions()),
        ("linked", linked.versions()),
        ("instances", instances.versions()),
    ];
    for (own, list) in sets {
        assert_eq!(list[0].name, own, "own component first");
        assert_eq!(list[0].kind, Kind::Capability);
        let mut names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        for c in &list {
            assert!(c.problems().is_empty(), "{}: {:?}", c.name, c.problems());
        }
        names.sort();
        names.dedup();
        assert_eq!(names.len(), list.len(), "{own}: names are unique");
        for c in &list[1..] {
            assert!(
                matches!(c.kind, Kind::Connector | Kind::Installed),
                "{}",
                c.name
            );
            assert!(c.name.starts_with(&format!("{own}.")), "{}", c.name);
        }
    }
    assert_eq!(storage.versions().len(), 4);
}

/// The ingest capability's pure parts: its tools, secret slots, cloud-init and host request
/// bodies, the job defaults, the estimates, and the approval digest.
#[test]
fn core_ingest_plans_hosts_and_estimates() {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::ingest::{Ingest, Target, hosts};
    use pigeon_capabilities::passkeys::Passkeys;
    use pigeon_capabilities::storage::{Account, Provider};
    use pigeon_ingest::estimate::{Host, estimate};
    use pigeon_ingest::manifest::SourceSpec;

    let do_acct = Account::digitalocean("legacy", "fra1");
    let scw_acct = Account::scaleway("archive", "fr-par");
    let b2_acct = Account::backblaze("old", "us-west-004");
    let cap = Ingest {
        targets: vec![Target::new("willowgraysen")],
        accounts: vec![do_acct.clone(), scw_acct.clone(), b2_acct.clone()],
        passkeys: Passkeys {
            host: "example.test".into(),
            site_name: "pigeon".into(),
        },
    };

    // The five tools, and the slots.
    let tools: Vec<&str> = cap.tools().iter().map(|t| t.name).collect();
    assert_eq!(
        tools,
        [
            "ingest_plan",
            "ingest_start",
            "ingest_status",
            "ingest_report",
            "ingest_stop"
        ]
    );
    let slots: Vec<String> = cap.secrets().into_iter().map(|s| s.name).collect();
    assert_eq!(
        slots,
        [
            "INGEST_R2_WILLOWGRAYSEN",
            "INGEST_HOST_DIGITALOCEAN_LEGACY",
            "INGEST_HOST_SCALEWAY_ARCHIVE"
        ]
    );
    assert_eq!(Target::new("willowgraysen").bucket, "willowgraysen-data");

    // Host choice.
    assert_eq!(hosts::host_for(Provider::DigitalOcean), Host::DigitalOcean);
    assert_eq!(hosts::host_for(Provider::Scaleway), Host::Scaleway);
    assert_eq!(hosts::host_for(Provider::Backblaze), Host::Cloudflare);
    assert_eq!(hosts::zone("fr-par"), "fr-par-1");

    // The job and its defaults.
    let commit = "0123456789abcdef0123456789abcdef01234567";
    assert!(hosts::is_commit(commit));
    assert!(!hosts::is_commit("main"));
    let inputs = |account: &Account, dry_run: bool, commit: &str| {
        hosts::job_spec(&hosts::JobInputs {
            instance: "willowgraysen",
            account,
            bucket: "photos",
            prefix: "",
            dest_bucket: "willowgraysen-data",
            account_id: &"a".repeat(32),
            dry_run,
            keep_archives: true,
            fingerprints: false,
            commit,
            day: "20261123",
            nonce: 0xabcdef,
        })
    };
    let job = inputs(&do_acct, true, commit);
    assert!(job.dry_run && job.keep_archives && !job.fingerprints);
    assert_eq!(job.threads, 8);
    assert_eq!(job.run, "20261123-digitalocean-legacy-photos-abcdef");
    assert_eq!(
        job.dest.endpoint,
        format!("https://{}.r2.cloudflarestorage.com", "a".repeat(32))
    );
    assert!(matches!(&job.source, SourceSpec::S3 { region, .. } if region == "fra1"));

    // Cloud-init: the pinned commit and URL; the redacted form holds no key.
    let (src, dst) = ("FAKESRCID:fakesrcsecret", "FAKEDSTID:fakedstsecret");
    let full = hosts::cloud_init(&job, src, dst);
    assert!(full.contains(&format!("PIGEON_COMMIT={commit}")));
    assert!(full.contains(&format!(
        "https://github.com/pigeon-security/pigeon.git#{commit}"
    )));
    assert!(full.contains("src/ingest-runner/Dockerfile"));
    assert!(full.contains(src) && full.contains(dst));
    assert!(full.trim_end().ends_with("poweroff"));
    let redacted = hosts::cloud_init_redacted(&job);
    assert!(!redacted.contains("fakesrcsecret") && !redacted.contains("fakedstsecret"));
    assert!(!redacted.contains("FAKESRCID") && redacted.contains("<redacted>"));
    assert!(redacted.contains(commit));

    // DigitalOcean's body.
    let body = hosts::droplet_body(&job, "fra1", &redacted);
    assert_eq!(body["region"], "fra1");
    assert_eq!(body["size"], "s-4vcpu-8gb");
    assert_eq!(body["image"], "ubuntu-24-04-x64");
    assert_eq!(body["tags"], json!(["pigeon-ingest"]));
    let name = body["name"].as_str().unwrap();
    assert!(name.starts_with("pigeon-ingest-") && name.len() <= 63);
    let long = hosts::vm_name(&"x".repeat(200));
    assert!(long.len() <= 63 && !long.ends_with('-'));
    assert!(long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));

    // Scaleway's bodies.
    let scw_job = inputs(&scw_acct, true, commit);
    let server = hosts::scaleway_server_body(&scw_job, "proj-1");
    assert_eq!(server["commercial_type"], "POP2-4C-16G");
    assert_eq!(server["image"], "ubuntu_noble");
    assert_eq!(server["project"], "proj-1");
    assert_eq!(server["tags"], json!(["pigeon-ingest"]));
    assert_eq!(server["volumes"]["0"]["size"], 100_000_000_000u64);
    assert_eq!(hosts::scaleway_poweron(), json!({ "action": "poweron" }));
    assert_eq!(
        hosts::scaleway_terminate(),
        json!({ "action": "terminate" })
    );
    assert_eq!(
        hosts::scaleway_user_data_path("fr-par-1", "srv"),
        "/instance/v1/zones/fr-par-1/servers/srv/user_data/cloud-init"
    );
    assert_eq!(
        hosts::scaleway_parts("proj:key").unwrap(),
        ("proj".to_string(), "key".to_string())
    );
    assert!(hosts::scaleway_parts("nocolon").is_err());

    // The redacted request for each host holds no key.
    for (h, region) in [
        (Host::DigitalOcean, "fra1"),
        (Host::Scaleway, "fr-par"),
        (Host::Cloudflare, ""),
    ] {
        let text = hosts::host_request(h, &job, region).to_string();
        assert!(!text.contains("fakesrcsecret") && text.contains("redacted"));
    }

    // Estimates, from decision 13.7.
    let tb = 1_000_000_000_000u64;
    let d = estimate(Host::DigitalOcean, 3_400_000_000_000, 4_000_000, 0.3);
    assert!(d.hours_low >= 6.0 && d.hours_high <= 10.0, "{d:?}");
    assert!(d.total_usd > 5.0 && d.total_usd < 30.0, "{d:?}");
    assert!(!d.assumptions.is_empty());
    let s = estimate(Host::Scaleway, 8 * tb / 10, 1_000_000, 0.3);
    assert!(s.hours_low > 1.0 && s.hours_high < 3.0, "{s:?}");
    assert!(s.total_usd > 1.0 && s.total_usd < 10.0, "{s:?}");
    let c = estimate(Host::Cloudflare, 3 * tb / 2, 2_500_000, 0.3);
    assert!(c.hours_low > 2.0 && c.hours_high < 7.0, "{c:?}");
    assert!(c.total_usd > 1.0 && c.total_usd < 30.0, "{c:?}");
    let l = estimate(Host::Local, tb, 1_000_000, 0.3);
    assert_eq!(l.host_usd, 0.0);

    // The approval digest moves with the commit and with dry_run.
    use pigeon_core::approval::{Kind, payload_digest};
    let kind = Kind::Other("ingest_run".into());
    let digest_of = |j: &pigeon_ingest::manifest::JobSpec| {
        let req = hosts::host_request(Host::DigitalOcean, j, "fra1");
        payload_digest(&kind, &j.run, &hosts::approval_payload(j, &req))
    };
    let base = digest_of(&job);
    assert_eq!(base, digest_of(&inputs(&do_acct, true, commit)));
    let other_commit = "fedcba9876543210fedcba9876543210fedcba98";
    assert_ne!(base, digest_of(&inputs(&do_acct, true, other_commit)));
    assert_ne!(base, digest_of(&inputs(&do_acct, false, commit)));
}

#[test]
fn protected_origins_require_dedicated_resources_and_bind_to_the_site_bucket() {
    use pigeon_capabilities::instances::{
        plan::{Ledger, Observed, Step, plan, record},
        record::{Instance, Rules, validate},
    };
    use pigeon_core::{r2::private_domains, site_auth::origin_key};

    let mut instance: Instance = serde_json::from_value(json!({
        "id": "fixture", "title": "Fixture", "worker": "fixture",
        "account": {"kind":"linked","account_id":"00000000000000000000000000000000"}, "version": 1,
        "sites": [{
            "name": "preview", "bucket": "fixture-site", "auth": "passkey", "allow": ["site"],
            "targets": [{"kind":"subdomain","label":"fixture"}]
        }]
    }))
    .unwrap();
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &[],
        reserved_workers: &[],
    };
    assert!(validate(&instance, &[], &rules).is_empty());
    let mut shared = instance.clone();
    let mut second = shared.sites[0].clone();
    second.name = "second".into();
    second.bucket = "fixture-second".into();
    second.auth = None;
    second.allow.clear();
    second.targets.clear();
    shared.sites.push(second);
    assert!(
        validate(&shared, &[], &rules)
            .iter()
            .any(|problem| problem.contains("dedicated instance with one site"))
    );

    let mut observed = Observed {
        account_id: "fixture".into(),
        bucket_exists: false,
        worker_exists: false,
        workers_dev: None,
        account_subdomain: Some("fixture".into()),
        domains: vec![],
        subdomains: vec![],
    };
    let mut ledger = Ledger::default();
    let steps = plan(&instance, &observed, &ledger);
    assert!(matches!(
        steps.as_slice(),
        [
            Step::CreateBucket {
                protected: true,
                ..
            },
            Step::UploadWorker {
                origin: Some(_),
                ..
            },
            Step::EnableWorkersDev { .. }
        ]
    ));
    observed.bucket_exists = true;
    assert!(plan(&instance, &observed, &ledger).iter().any(Step::stops));
    record(&mut ledger, &steps[0]);
    assert!(!plan(&instance, &observed, &ledger).iter().any(Step::stops));
    observed.worker_exists = true;
    assert!(plan(&instance, &observed, &ledger).iter().any(Step::stops));
    record(&mut ledger, &steps[1]);
    observed.workers_dev = Some(true);
    assert!(
        !plan(&instance, &observed, &ledger)
            .iter()
            .any(Step::changes)
    );

    instance.sites[0].auth = None;
    assert!(plan(&instance, &observed, &ledger).iter().any(Step::stops));
    let upstream = "https://fixture.sandbox.workers.dev";
    let key = origin_key(
        b"fixture-only",
        "fixture",
        "preview",
        "fixture-site",
        upstream,
    );
    assert_ne!(
        key,
        origin_key(
            b"fixture-only",
            "other",
            "preview",
            "fixture-site",
            upstream
        )
    );
    assert_ne!(
        key,
        origin_key(
            b"fixture-only",
            "fixture",
            "other",
            "fixture-site",
            upstream
        )
    );
    assert_ne!(
        key,
        origin_key(
            b"fixture-only",
            "fixture",
            "preview",
            "other-site",
            upstream
        )
    );

    assert_ne!(
        key,
        origin_key(
            b"fixture-only",
            "fixture",
            "preview",
            "fixture-site",
            "https://fixture.other.workers.dev"
        )
    );

    assert!(private_domains(
        &json!({"enabled":false}),
        &json!({"domains":[]})
    ));
    assert!(!private_domains(
        &json!({"enabled":true}),
        &json!({"domains":[]})
    ));
    assert!(!private_domains(
        &json!({"enabled":false}),
        &json!({"domains":[{}]})
    ));
    assert!(!private_domains(&json!({}), &json!({"domains":[]})));
}

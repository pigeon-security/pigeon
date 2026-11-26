//! Instances and versions: pins, plans and upgrades.

use super::*;

#[test]
fn instances_are_data_a_person_approves() {
    use pigeon_capabilities::instances::record::{
        self, AccountRef, Instance, Rules, Target, diff, reserved_labels, site_for_host, validate,
    };
    use pigeon_capabilities::instances::template;

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
        platform_worker: "pigeon-platform",
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

    // `source` and `observability` share one opt-in type whose form record digests cover.
    let mut toggled = serde_json::to_value(&plain).unwrap();
    toggled["source"] = json!({"state": "enabled"});
    toggled["observability"] = json!({"state": "disabled"});
    let parsed: Instance = serde_json::from_value(toggled.clone()).unwrap();
    assert_eq!(parsed.source, Some(record::Toggle::Enabled));
    assert_eq!(parsed.observability, Some(record::Toggle::Disabled));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), toggled);

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

/// The platform capabilities' view sections, as `console_manifest` collects them.
fn manifest_platform_views() -> Vec<(
    &'static str,
    pigeon_capabilities::console::sections::Section,
)> {
    use pigeon_capabilities::Capability;
    use pigeon_capabilities::architecture::Architecture;
    let mut out = Vec::new();
    for (name, sections) in [
        ("architecture", Architecture.sections()),
        (
            "multi-instance",
            pigeon_capabilities::instances::multi_instance_sections(),
        ),
    ] {
        out.extend(sections.into_iter().map(|s| (name, s)));
    }
    out
}

fn manifest_of(record: &Value, inbox: Option<usize>) -> Value {
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};
    let record: Instance = serde_json::from_value(record.clone()).unwrap();
    let scope = Scope::new(&record).unwrap();
    manifest(&record, &scope, &manifest_platform_views(), &[], inbox)
}

fn manifest_areas(m: &Value) -> Vec<&str> {
    m["areas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect()
}

fn manifest_views<'a>(m: &'a Value, area: &str) -> Vec<&'a str> {
    let area = m["areas"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == area)
        .unwrap();
    area["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect()
}

/// `console_manifest` for the home instance: Inbox, its instance areas, then the platform
/// areas the platform capabilities declare views in, each in the stable order with its key.
#[test]
fn console_manifest_for_the_home_instance() {
    let m = manifest_of(
        &json!({
            "id": "example/platform", "title": "Example", "account": { "kind": "home" },
            "zones": ["example.com"],
            "observability": { "state": "enabled" }, "worker": "example-platform"
        }),
        Some(4),
    );
    assert_eq!(
        m["instance"],
        json!({ "id": "example/platform", "title": "Example", "home": true, "host": "example.com" })
    );
    assert_eq!(
        manifest_areas(&m),
        [
            "inbox",
            "overview",
            "metrics",
            "capabilities",
            "cost",
            "instances",
            "platform"
        ]
    );
    assert_eq!(m["inbox"], json!({ "count": 4 }));
    let inbox = &m["areas"][0];
    assert_eq!(inbox["group"], "top");
    assert_eq!(inbox["key"], "i");
    assert_eq!(inbox["views"], json!([]));
    assert_eq!(m["areas"][2]["group"], "instance");
    assert_eq!(m["areas"][2]["key"], "m");
    assert_eq!(
        m["areas"][2]["views"][0],
        json!({ "id": "workers", "title": "Workers", "order": 10,
                "capability": "observability", "upgrade": null })
    );
    assert_eq!(m["areas"][4]["group"], "platform");
    assert_eq!(manifest_views(&m, "cost"), ["7d", "today", "all"]);
    assert_eq!(manifest_views(&m, "instances"), ["list"]);
    assert_eq!(
        manifest_views(&m, "platform"),
        ["architecture", "endpoints"]
    );
}

/// A linked instance that publishes a passkey-gated site: Site with Sites, Deployments and
/// Visitors, and no platform areas.
#[test]
fn console_manifest_for_a_linked_publishing_instance() {
    let mut record = json!({
        "id": "sample", "title": "Sample", "account": { "kind": "linked", "account_id": null },
        "sites": [{ "name": "blog", "bucket": "sample-site", "auth": "passkey",
            "targets": [{ "kind": "domain", "hostname": "blog.example.com" }] }],
        "publishing": { "state": "enabled", "site": "blog" }
    });
    let m = manifest_of(&record, Some(1));
    assert_eq!(m["instance"]["home"], false);
    assert_eq!(m["instance"]["host"], "blog.example.com");
    assert_eq!(
        manifest_areas(&m),
        ["inbox", "overview", "site", "capabilities"]
    );
    assert_eq!(
        manifest_views(&m, "site"),
        ["sites", "deployments", "visitors"]
    );
    assert_eq!(m["areas"][2]["views"][1]["capability"], "publishing");
    assert_eq!(m["areas"][2]["views"][2]["capability"], "site_passkeys");

    // Without passkeys the site has no Visitors; without publishing, no Site at all, even
    // with a gated site.
    record["sites"][0]["auth"] = Value::Null;
    assert_eq!(
        manifest_views(&manifest_of(&record, None), "site"),
        ["sites", "deployments"]
    );
    record["sites"][0]["auth"] = json!("passkey");
    record["publishing"] = json!({ "state": "disabled" });
    let m = manifest_of(&record, None);
    assert_eq!(manifest_areas(&m), ["inbox", "overview", "capabilities"]);
    assert_eq!(m["inbox"], json!({ "count": null }));
}

/// A record with nothing enabled shows only Inbox, Overview and Capabilities; a disabled
/// profile hides its area as an absent one does.
#[test]
fn console_manifest_with_nothing_enabled() {
    let mut record = json!({ "id": "bare", "title": "Bare", "account": { "kind": "linked" } });
    assert_eq!(
        manifest_areas(&manifest_of(&record, Some(0))),
        ["inbox", "overview", "capabilities"]
    );
    record["storage"] = json!({ "state": "enabled", "store": "archive-store", "bucket": "archive", "prefix": "p/" });
    record["ingest"] = json!({ "state": "disabled" });
    assert_eq!(
        manifest_areas(&manifest_of(&record, Some(0))),
        ["inbox", "overview", "storage", "capabilities"]
    );
    record["storage"] = json!({ "state": "disabled" });
    assert_eq!(
        manifest_areas(&manifest_of(&record, Some(0))),
        ["inbox", "overview", "capabilities"]
    );
}

/// Each area of `m` with its views, as `view@capability`.
fn manifest_by_capability(m: &Value) -> Vec<(String, Vec<String>)> {
    m["areas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let views = a["views"].as_array().unwrap().iter().map(|v| {
                format!(
                    "{}@{}",
                    v["id"].as_str().unwrap(),
                    v["capability"].as_str().unwrap()
                )
            });
            (a["id"].as_str().unwrap().to_string(), views.collect())
        })
        .collect()
}

/// Capability scoping, M1 (docs/plans/capability-scoping.md): given every hosted capability's
/// views as the platform Worker has them, each kind of instance today shows exactly the areas
/// and views it showed when a `group` decided them. Home: its own areas, then every hosted
/// capability's (home runs the platform), and Cost only while its record enables observability.
/// A linked instance, publishing or with source alone: only what its record enables, and no
/// Cost even with observability, since `cost_daily` reads the platform's spend. Each area still carries its group for the iOS app.
#[test]
fn console_manifest_by_capability_per_instance_kind() {
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};
    let hosted = crate::console::hosted_views();
    let of = |record: Value| {
        let record: Instance = serde_json::from_value(record).unwrap();
        let scope = Scope::new(&record).unwrap();
        manifest(&record, &scope, &hosted, &[], Some(0))
    };
    let expect = |rows: &[(&str, &[&str])]| -> Vec<(String, Vec<String>)> {
        rows.iter()
            .map(|(a, v)| (a.to_string(), v.iter().map(|s| s.to_string()).collect()))
            .collect()
    };

    let home = of(json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"], "observability": { "state": "enabled" },
        "worker": "example-platform"
    }));
    assert_eq!(
        manifest_by_capability(&home),
        expect(&[
            ("inbox", &[]),
            ("overview", &[]),
            ("metrics", &["workers@observability"]),
            ("capabilities", &[]),
            ("sessions", &["live@ai-agents", "compare@ai-agents"]),
            (
                "cost",
                &[
                    "7d@observability",
                    "today@observability",
                    "all@observability"
                ]
            ),
            (
                "access",
                &["secrets@secrets", "tokens@proxy", "people@users"]
            ),
            ("instances", &["list@multi-instance"]),
            (
                "platform",
                &[
                    "architecture@architecture",
                    "models@models",
                    "endpoints@multi-instance"
                ]
            ),
        ])
    );
    let groups: Vec<&str> = home["areas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["group"].as_str().unwrap())
        .collect();
    assert_eq!(
        groups,
        [
            "top", "instance", "instance", "instance", "platform", "platform", "platform",
            "platform", "platform"
        ]
    );

    // Cost follows home's own `observability` toggle, as Metrics does: without it, neither.
    let home_off = of(json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"], "observability": { "state": "disabled" },
        "worker": "example-platform"
    }));
    let areas: Vec<String> = manifest_by_capability(&home_off)
        .into_iter()
        .map(|(a, _)| a)
        .collect();
    assert!(
        !areas.iter().any(|a| a == "cost" || a == "metrics"),
        "{areas:?}"
    );
    assert!(areas.iter().any(|a| a == "sessions"), "{areas:?}");

    let publishing = of(json!({
        "id": "docs", "title": "Docs",
        "account": { "kind": "linked", "account_id": "0123456789abcdef0123456789abcdef" },
        "zones": ["example.org"], "worker": "example",
        "sites": [{ "name": "docs", "bucket": "example-site", "auth": "passkey",
            "targets": [{ "kind": "domain", "hostname": "docs.example.org" }] }],
        "publishing": { "state": "enabled", "site": "docs" },
        "storage": { "state": "enabled", "store": "s", "bucket": "b", "prefix": "p/" },
        "observability": { "state": "enabled" }
    }));
    assert_eq!(
        manifest_by_capability(&publishing),
        expect(&[
            ("inbox", &[]),
            ("overview", &[]),
            (
                "site",
                &[
                    "sites@publishing",
                    "deployments@publishing",
                    "visitors@site_passkeys"
                ]
            ),
            ("storage", &["buckets@storage"]),
            ("metrics", &["workers@observability"]),
            ("capabilities", &[]),
        ])
    );

    let source = of(json!({
        "id": "notes", "title": "Notes", "account": { "kind": "linked" },
        "repo": { "kind": "github", "full_name": "example/notes" },
        "source": { "state": "enabled" }
    }));
    assert_eq!(
        manifest_by_capability(&source),
        expect(&[
            ("inbox", &[]),
            ("overview", &[]),
            ("source", &["repository@source"]),
            ("capabilities", &[]),
        ])
    );
}

/// The catalogue capabilities a linked instance's record may enable so far (capability scoping,
/// M4, one at a time), besides `instances`, which every instance has.
const LINKED_SO_FAR: &[&str] = &[
    "redirects",
    "architecture",
    "secrets",
    "cloud",
    "digitalocean",
    "jobs",
];

/// The capability catalogue (docs/plans/capability-scoping.md): each capability's placement,
/// scope and availability as decided, and every hosted capability that fills a console view
/// is in it.
#[test]
fn capability_catalogue_as_decided() {
    use pigeon_capabilities::catalogue::{Availability, CATALOGUE, Placement, Scope, find};
    for (name, choice, home, worker, scope, availability, requires) in [
        (
            "ai-agents",
            true,
            Placement::OwnWorker,
            Some("pigeon-ai-agents"),
            Scope::Instance,
            Availability::Any,
            &[][..],
        ),
        (
            "observability",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "secrets",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "proxy",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &["secrets"],
        ),
        (
            "cloud",
            true,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "digitalocean",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &["cloud"],
        ),
        (
            "jobs",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "models",
            true,
            Placement::OwnWorker,
            Some("pigeon-model-gateway"),
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "multi-instance",
            false,
            Placement::Inline,
            None,
            Scope::Platform,
            Availability::HomeOnly,
            &[],
        ),
        (
            "instances",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "users",
            false,
            Placement::Inline,
            None,
            Scope::Platform,
            Availability::HomeOnly,
            &[],
        ),
        (
            "architecture",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "redirects",
            false,
            Placement::Inline,
            None,
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        // The unified data store and email (docs/plans/email.md): in the data Worker alone.
        (
            "data",
            false,
            Placement::OwnWorker,
            Some("pigeon-data"),
            Scope::Instance,
            Availability::Any,
            &[],
        ),
        (
            "email",
            false,
            Placement::OwnWorker,
            Some("pigeon-data"),
            Scope::Instance,
            Availability::Any,
            &["data"],
        ),
    ] {
        let e = find(name).unwrap_or_else(|| panic!("{name} is catalogued"));
        assert_eq!(e.offers_choice(), choice, "{name}");
        assert!(e.placements.contains(&e.home), "{name}");
        assert_eq!(e.home, home, "{name}");
        assert_eq!(e.home_worker, worker, "{name}");
        assert_eq!(e.scope, scope, "{name}");
        assert_eq!(e.availability, availability, "{name}");
        assert_eq!(e.requires, requires, "{name}");
    }
    // The catalogue capabilities a linked instance's record may enable: `redirects` (set by the
    // record's own `redirects` list) and those M4 turned on, one at a time; every instance has
    // `instances`.
    for e in CATALOGUE {
        assert_eq!(
            e.linked,
            e.always || LINKED_SO_FAR.contains(&e.name),
            "{}",
            e.name
        );
        assert_eq!(e.always, e.name == "instances", "{}", e.name);
    }
    for e in CATALOGUE {
        assert_eq!(
            e.home == Placement::OwnWorker,
            e.home_worker.is_some(),
            "{}",
            e.name
        );
        for r in e.requires {
            assert!(find(r).is_some(), "{} requires an uncatalogued {r}", e.name);
        }
    }
    for (name, _) in crate::console::hosted_views() {
        assert!(
            find(name).is_some(),
            "{name} fills a view but is not catalogued"
        );
    }
    // `capabilities_versions` answers it beside the components.
    let v = pigeon_capabilities::versioning::versions_view(&[], &[], &[], false);
    assert_eq!(v["linked_placement_choices"]["cloud"], json!(["inline"]));
    assert_eq!(v["catalogue"].as_array().unwrap().len(), CATALOGUE.len());
    assert_eq!(
        v["catalogue"][0]["placements"],
        json!(["inline", "own_worker"])
    );
    assert_eq!(v["catalogue"][0]["availability"], "any");
    let recorded: Value = serde_json::from_str(include_str!(
        "../../src/capabilities/src/console/web/fixtures/capabilities_versions.json"
    ))
    .unwrap();
    assert_eq!(
        recorded["catalogue"], v["catalogue"],
        "the recorded catalogue"
    );
    // Internal plumbing is marked on each component row, so the console can fold it away; the
    // recorded components mark it as the code does.
    use pigeon_capabilities::catalogue::{PLUMBING, is_plumbing};
    use pigeon_core::versions::{Component, Kind};
    let carried: Vec<(Component, String)> = ["dispatch", "secrets"]
        .iter()
        .map(|n| {
            (
                Component::began(n, Kind::Capability),
                "pigeon-platform".to_string(),
            )
        })
        .collect();
    let v = pigeon_capabilities::versioning::versions_view(&carried, &[], &[], false);
    assert_eq!(v["components"][0]["plumbing"], true);
    assert_eq!(v["components"][1]["plumbing"], false);
    for name in PLUMBING {
        assert!(
            find(name).is_none(),
            "{name} is plumbing, not a catalogued capability"
        );
    }
    for c in recorded["components"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        assert_eq!(c["plumbing"], is_plumbing(name), "recorded {name}");
    }
}

/// Capability scoping, M2: a record lists the catalogue capabilities it enables beyond its
/// profile fields. Validation refuses an unknown one, `instances` (every instance has it), a
/// home-only one (`multi-instance`, `users`) off home, any on a linked instance until M4, and
/// one without what it requires. The manifest never shows a home-only capability's views off
/// home, whatever the record says, and a record that lists none keeps its digest.
#[test]
fn capability_scoping_enforces_home_only() {
    use pigeon_capabilities::catalogue::{MULTI_INSTANCE, hosted_enabled, problems};
    use pigeon_capabilities::instances::record::{self, Instance, Rules};
    use pigeon_capabilities::instances::{manifest::manifest, tools::Scope};

    let home_json = json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"]
    });
    let linked_json = json!({ "id": "docs", "title": "Docs", "account": { "kind": "linked" } });
    let with = |base: &Value, caps: Value| -> Instance {
        let mut v = base.clone();
        v["capabilities"] = caps;
        serde_json::from_value(v).unwrap()
    };

    // Absent when empty: the stored records' digests do not change.
    let plain: Instance = serde_json::from_value(home_json.clone()).unwrap();
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("capabilities")
            .is_none()
    );
    let empty = with(&home_json, json!({}));
    assert_eq!(record::digest(&empty), record::digest(&plain));
    // An unknown setting does not parse (`placement` is the one setting, M3).
    let mut bad = home_json.clone();
    bad["capabilities"] = json!({ "users": { "worker": "pigeon-users" } });
    assert!(serde_json::from_value::<Instance>(bad).is_err());

    // Home may list the home-only ones, and the others with what they require.
    let home = with(
        &home_json,
        json!({ "multi-instance": {}, "users": {}, "secrets": {}, "proxy": {} }),
    );
    assert_eq!(problems(&home), Vec::<String>::new());
    let home_problems = |caps: Value| problems(&with(&home_json, caps));
    assert_eq!(
        home_problems(json!({ "proxy": {} })),
        ["The capability \"proxy\" needs \"secrets\" enabled beside it."]
    );
    assert_eq!(
        home_problems(json!({ "instances": {} })),
        ["The capability \"instances\" is on every instance; leave it out of capabilities."]
    );
    assert_eq!(
        home_problems(json!({ "teleport": {} })),
        ["The capability \"teleport\" is not one pigeon knows."]
    );

    // A linked record may not enable a home-only one, nor any other yet.
    let linked = |caps: Value| problems(&with(&linked_json, caps));
    for name in ["multi-instance", "users"] {
        assert_eq!(
            linked(json!({ name: {} })),
            [format!(
                "The capability \"{name}\" is home-only: only the home instance may enable it."
            )]
        );
    }
    for name in ["ai-agents", "observability", "secrets", "models"]
        .into_iter()
        .filter(|n| !LINKED_SO_FAR.contains(n))
    {
        assert_eq!(
            linked(json!({ name: {} })),
            [format!(
                "The capability \"{name}\" cannot be enabled on a linked instance yet."
            )]
        );
    }
    assert_eq!(
        linked(json!({ "proxy": {}, "secrets": {} })),
        ["The capability \"proxy\" cannot be enabled on a linked instance yet."]
    );
    assert!(linked(json!({})).is_empty());

    // Record validation carries them.
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &[],
        reserved_workers: &[],
        platform_worker: "pigeon-platform",
    };
    let refused = with(&linked_json, json!({ MULTI_INSTANCE: {} }));
    assert!(
        record::validate(&refused, &[], &rules)
            .iter()
            .any(|p| p.contains("home-only"))
    );
    let plain_linked: Instance = serde_json::from_value(linked_json.clone()).unwrap();
    assert!(
        !record::validate(&plain_linked, &[], &rules)
            .iter()
            .any(|p| p.contains("capability"))
    );

    // Which hosted capabilities are enabled where: every one on home; off home, `instances`
    // and those listed that M4 turned on for linked instances, never a home-only one, whatever
    // the record lists.
    let listed = with(
        &linked_json,
        json!({ "multi-instance": {}, "users": {}, "ai-agents": {}, "models": {} }),
    );
    for name in [
        "multi-instance",
        "users",
        "secrets",
        "architecture",
        "ai-agents",
        "models",
        "approvals",
    ] {
        assert!(hosted_enabled(&plain, name), "{name} on home");
        assert!(!hosted_enabled(&listed, name), "{name} off home");
    }
    assert!(hosted_enabled(&listed, "instances"));

    // The manifest of that record: no hosted capability's views, home-only least of all.
    let hosted = crate::console::hosted_views();
    let scope = Scope::new(&listed).unwrap();
    let m = manifest(&listed, &scope, &hosted, &[], None);
    assert_eq!(manifest_areas(&m), ["inbox", "overview", "capabilities"]);
    let scope = Scope::new(&plain).unwrap();
    let m = manifest(&plain, &scope, &hosted, &[], None);
    assert_eq!(manifest_views(&m, "instances"), ["list"]);
    assert_eq!(
        m["areas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "instances")
            .unwrap()["views"][0]["capability"],
        MULTI_INSTANCE
    );
}

/// Capability scoping, M4 step 1: `architecture` on a linked instance. It needs no linked-account
/// scope (it reads the record and what the platform serves for it), so a linked record may list
/// it, and then, and only then, its console shows Platform › Architecture; every other area and
/// view stays as it was.
#[test]
fn architecture_on_a_linked_instance() {
    use pigeon_capabilities::catalogue::{hosted_enabled, problems};
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};
    let hosted = crate::console::hosted_views();
    let base = json!({
        "id": "docs", "title": "Docs",
        "account": { "kind": "linked", "account_id": "0123456789abcdef0123456789abcdef" },
        "zones": ["example.org"], "worker": "example",
        "storage": { "state": "enabled", "store": "s", "bucket": "b", "prefix": "p/" }
    });
    let of = |v: &Value| {
        let record: Instance = serde_json::from_value(v.clone()).unwrap();
        let scope = Scope::new(&record).unwrap();
        (manifest(&record, &scope, &hosted, &[], None), record)
    };
    let (before, plain) = of(&base);
    assert!(!hosted_enabled(&plain, "architecture"));
    let mut with = base.clone();
    with["capabilities"] = json!({ "architecture": {} });
    let (after, record) = of(&with);
    assert!(problems(&record).is_empty(), "{:?}", problems(&record));
    assert!(hosted_enabled(&record, "architecture"));
    assert_eq!(
        manifest_by_capability(&after),
        [
            manifest_by_capability(&before),
            vec![(
                "platform".to_string(),
                vec!["architecture@architecture".to_string()]
            )]
        ]
        .concat()
    );
    // Inline only: it offers nothing else.
    with["capabilities"] = json!({ "architecture": { "placement": "inline" } });
    assert!(problems(&of(&with).1).is_empty());
}

/// Capability scoping, M4 step 2: `secrets` on a linked instance. Its record lists
/// `capabilities.secrets`, and then Access › Secrets shows, and nothing else changes. The view
/// reads that instance's own account's Secrets Store (`secrets_list {instance}`), never the
/// platform's. The link needs `secrets-store.write`: a link without it is refused with "re-link
/// to enable secrets", in the plan and the read alike. The plan creates the store when the
/// account has none and records it in the ledger, and a record that does not enable secrets
/// plans as before.
#[test]
fn secrets_on_a_linked_instance() {
    use pigeon_capabilities::catalogue::{hosted_enabled, problems};
    use pigeon_capabilities::instances::plan::{
        Ledger, SeenCapabilities, Step, capability_steps, describe, record as ledger_record,
    };
    use pigeon_capabilities::instances::{manifest::manifest, record::Instance, tools::Scope};
    use pigeon_capabilities::linked::{capability_refusal, explain, grants, relink_for};

    let hosted = crate::console::hosted_views();
    let base = json!({
        "id": "docs", "title": "Docs",
        "account": { "kind": "linked", "account_id": "0123456789abcdef0123456789abcdef" },
        "zones": ["example.org"], "worker": "example"
    });
    let of = |v: &Value| {
        let record: Instance = serde_json::from_value(v.clone()).unwrap();
        let scope = Scope::new(&record).unwrap();
        (manifest(&record, &scope, &hosted, &[], None), record)
    };
    let (before, plain) = of(&base);
    let mut with = base.clone();
    with["capabilities"] = json!({ "secrets": {} });
    let (after, record) = of(&with);
    assert!(problems(&record).is_empty(), "{:?}", problems(&record));
    assert!(hosted_enabled(&record, "secrets") && !hosted_enabled(&plain, "secrets"));
    // Only Access › Secrets is added; Tokens (proxy) and the home-only People and Apps are not.
    assert_eq!(
        manifest_by_capability(&after),
        [
            manifest_by_capability(&before),
            vec![("access".to_string(), vec!["secrets@secrets".to_string()])]
        ]
        .concat()
    );
    // The view reads the selected instance's own secrets.
    assert_eq!(
        pigeon_capabilities::console::ops::read_keys("secrets_list"),
        Some(&["instance"][..])
    );
    let js = include_str!("../../src/capabilities/src/console/web/views/access.js");
    assert!(js.contains("ctx.op(\"secrets_list\", { instance: ctx.instance.id })"));

    // The scope: granted, not granted, or not reported.
    let full = "account-settings.read zone.read secrets-store.write offline_access";
    let old = "account-settings.read zone.read dns.write offline_access";
    assert_eq!(grants(Some(full), "secrets-store.write"), Some(true));
    assert_eq!(grants(Some(old), "secrets-store.write"), Some(false));
    assert_eq!(grants(None, "secrets-store.write"), None);
    assert_eq!(capability_refusal("docs", "secrets", Some(full)), None);
    assert_eq!(capability_refusal("docs", "secrets", None), None);
    assert_eq!(capability_refusal("docs", "architecture", Some(old)), None);
    let why = capability_refusal("docs", "secrets", Some(old)).unwrap();
    assert_eq!(
        why,
        "The link to docs does not grant Secrets Store Write (secrets-store.write), which \
         secrets needs: re-link docs (Instances › docs › Overview, Link) and allow it, to enable \
         secrets."
    );
    assert_eq!(relink_for(Some(old)), ["secrets"]);
    assert!(relink_for(Some(full)).is_empty() && relink_for(None).is_empty());
    // Cloudflare's refusal of an older link reads as "re-link", never as a bare 403.
    let refused = explain("docs", "secrets", "Cloudflare API: 403: Unauthorized");
    assert!(refused.contains("re-link docs") && refused.contains("Secrets Store Write"));
    assert_eq!(
        explain("docs", "secrets", "Cloudflare API: 500: oops"),
        "Cloudflare API: 500: oops"
    );

    // The plan: nothing for a record that does not enable it, nothing on home.
    let none = SeenCapabilities::default();
    assert!(capability_steps(&plain, &none).is_empty());
    let home: Instance = serde_json::from_value(json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"], "capabilities": { "secrets": {} }
    }))
    .unwrap();
    let missing = SeenCapabilities {
        secrets_store: Some(false),
        refused: vec![],
        ..Default::default()
    };
    assert!(capability_steps(&home, &missing).is_empty());
    // A store is made when there is none, once, and kept in the ledger.
    let steps = capability_steps(&record, &missing);
    assert_eq!(
        steps,
        [Step::CreateSecretsStore {
            name: "pigeon".into()
        }]
    );
    assert!(steps[0].changes() && !steps[0].stops());
    assert!(
        describe(&steps)[0]
            .as_str()
            .unwrap()
            .contains("Secrets Store, pigeon")
    );
    let mut ledger = Ledger::default();
    ledger_record(&mut ledger, &steps[0]);
    ledger_record(&mut ledger, &steps[0]);
    assert_eq!(ledger.secrets_stores, ["pigeon"]);
    assert!(
        serde_json::to_value(Ledger::default())
            .unwrap()
            .get("secrets_stores")
            .is_none(),
        "absent when empty"
    );
    let present = SeenCapabilities {
        secrets_store: Some(true),
        refused: vec![],
        ..Default::default()
    };
    assert!(
        !capability_steps(&record, &present)
            .iter()
            .any(Step::changes)
    );
    // A link without the scope: left alone with why, and the site's steps still apply.
    let refused = SeenCapabilities {
        secrets_store: None,
        refused: vec![("secrets".into(), why.clone())],
        ..Default::default()
    };
    let steps = capability_steps(&record, &refused);
    assert_eq!(
        steps,
        [Step::Keep {
            what: "capability".into(),
            name: "secrets".into(),
            why
        }]
    );
    assert!(!steps[0].stops() && !steps[0].changes());

    // The store's id from Cloudflare's list.
    use pigeon_capabilities::secrets::store_of;
    assert_eq!(
        store_of(&json!([{ "id": "s1", "name": "default_secrets_store" }])).as_deref(),
        Some("s1")
    );
    assert_eq!(store_of(&json!([])), None);
}

/// Capability scoping, M5: a capability placed `own_worker` on a linked instance is planned as
/// its own Worker, `<instance>-<capability>`, uploaded before the instance Worker that binds it
/// by Service Binding and kept in the ledger, so retire removes it. No capability has a build for
/// a linked account yet (`LINKED_WORKERS` is empty), so every catalogue entry stays inline there
/// and every plan is as before; the mechanism is pinned here with a stand-in build.
#[test]
fn own_worker_placement_on_a_linked_instance() {
    use pigeon_capabilities::catalogue::{CATALOGUE, Placement, linked_placements, problems};
    use pigeon_capabilities::instances::plan::{
        LINKED_WORKERS, Ledger, LinkedWorker, SeenCapabilities, Step, capability_worker_digest,
        capability_workers, capability_workers_with, describe, own_workers,
        record as ledger_record, services, services_with, with_services,
    };
    use pigeon_capabilities::instances::record::Instance;

    // Nothing has a linked build, so nothing may be placed in its own Worker off home.
    assert!(LINKED_WORKERS.is_empty());
    for e in CATALOGUE {
        assert_eq!(linked_placements(e), [Placement::Inline], "{}", e.name);
    }

    // A record as validation would refuse today, to drive the mechanism.
    let record: Instance = serde_json::from_value(json!({
        "id": "docs", "title": "Docs",
        "account": { "kind": "linked", "account_id": "0123456789abcdef0123456789abcdef" },
        "worker": "docs", "capabilities": { "models": { "placement": "own_worker" } }
    }))
    .unwrap();
    assert!(
        problems(&record)
            .iter()
            .any(|p| p.contains("cannot run own_worker on a linked instance yet"))
    );
    let build = LinkedWorker {
        capability: "models",
        binding: "MODELS",
        module: "export default { fetch() { return new Response('ok'); } };",
    };
    let builds = [build];
    let none = SeenCapabilities::default();
    let mut ledger = Ledger::default();
    assert_eq!(own_workers(&record), ["docs-models"]);

    // Missing: uploaded, before anything else, and recorded as pigeon's.
    let steps = capability_workers_with(&record, &none, &ledger, &builds);
    let digest = capability_worker_digest(&build);
    assert_eq!(
        steps,
        [Step::UploadCapabilityWorker {
            capability: "models".into(),
            name: "docs-models".into(),
            binding: "MODELS".into(),
            module: digest.clone(),
            replaces: false,
        }]
    );
    assert!(steps[0].changes() && !steps[0].stops());
    assert_eq!(
        describe(&steps)[0],
        "upload models's own Worker docs-models; the instance's Worker reaches it as MODELS, \
         and retire removes it"
    );
    ledger_record(&mut ledger, &steps[0]);
    assert_eq!(
        ledger.workers,
        [("docs-models".to_string(), digest.clone())]
    );
    // There and as uploaded: unchanged. Changed since: updated. Not pigeon's: left alone.
    let seen = SeenCapabilities {
        workers: vec!["docs-models".into()],
        ..Default::default()
    };
    assert_eq!(
        capability_workers_with(&record, &seen, &ledger, &builds),
        [Step::Ok {
            what: "worker".into(),
            name: "docs-models".into()
        }]
    );
    let older = Ledger {
        workers: vec![("docs-models".into(), "old".into())],
        ..Ledger::default()
    };
    assert!(matches!(
        &capability_workers_with(&record, &seen, &older, &builds)[0],
        Step::UploadCapabilityWorker { replaces: true, .. }
    ));
    let theirs = capability_workers_with(&record, &seen, &Ledger::default(), &builds);
    assert!(matches!(&theirs[0], Step::Conflict { .. }) && theirs[0].stops());
    // No build: blocked, never guessed at.
    let unbuilt = capability_workers(&record, &none, &Ledger::default());
    assert_eq!(
        unbuilt,
        [Step::Blocked {
            why: "models has no Worker build for a linked account yet".into()
        }]
    );

    // The instance Worker binds it, and its digest changes only when it has one to bind.
    assert_eq!(
        services_with(&record, &builds),
        [("MODELS".to_string(), "docs-models".to_string())]
    );
    assert!(services(&record).is_empty());
    assert_eq!(with_services("m", &[]), "m");
    assert_ne!(with_services("m", &services_with(&record, &builds)), "m");

    // Home and inline records get none of it.
    let home: Instance = serde_json::from_value(json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"], "capabilities": { "models": { "placement": "own_worker" } }
    }))
    .unwrap();
    assert!(capability_workers_with(&home, &none, &Ledger::default(), &builds).is_empty());
    assert!(services_with(&home, &builds).is_empty());
    let inline: Instance = serde_json::from_value(json!({
        "id": "docs", "title": "Docs", "account": { "kind": "linked" },
        "capabilities": { "secrets": {} }
    }))
    .unwrap();
    assert!(own_workers(&inline).is_empty());
    assert!(capability_workers_with(&inline, &none, &Ledger::default(), &builds).is_empty());

    // The Service Binding as the upload metadata carries it.
    use pigeon_core::workers::{Binding, metadata};
    let m = metadata(
        "2026-11-01",
        &[Binding::Service {
            name: "MODELS".into(),
            service: "docs-models".into(),
        }],
    );
    assert_eq!(
        m["bindings"][0],
        json!({ "type": "service", "name": "MODELS", "service": "docs-models" })
    );
}

/// Capability scoping, M3: a record's `capabilities.<name>.placement`, `inline` or
/// `own_worker`, checked against the catalogue. Home's is read-only and says what is deployed
/// (ai-agents and models in their own Workers), so a home record may restate it but never move
/// it; a linked record may give only `inline`; a placement the capability does not offer is
/// refused everywhere. Records without one keep their digest, and `capabilities_versions`
/// answers where each runs.
#[test]
fn capability_placement_in_the_record() {
    use pigeon_capabilities::catalogue::{Placement, placement, placements, problems};
    use pigeon_capabilities::instances::record::{self, Instance};

    let home_json = json!({
        "id": "example/platform", "title": "Example", "account": { "kind": "home" },
        "zones": ["example.com"]
    });
    let linked_json = json!({ "id": "docs", "title": "Docs", "account": { "kind": "linked" },
        "worker": "docs" });
    let with = |base: &Value, caps: Value| -> Instance {
        let mut v = base.clone();
        v["capabilities"] = caps;
        serde_json::from_value(v).unwrap()
    };

    // It parses as inline or own_worker, and nothing else.
    let mut bad = home_json.clone();
    bad["capabilities"] = json!({ "models": { "placement": "elsewhere" } });
    assert!(serde_json::from_value::<Instance>(bad).is_err());
    // Absent, it is absent: `{}` reads back as `{}`, and the digest is the one without it.
    let bare = with(&home_json, json!({ "users": {} }));
    assert_eq!(
        serde_json::to_value(&bare).unwrap()["capabilities"],
        json!({ "users": {} })
    );
    let placed = with(&home_json, json!({ "users": { "placement": "inline" } }));
    assert_ne!(record::digest(&placed), record::digest(&bare));
    assert_eq!(
        serde_json::to_value(&placed).unwrap()["capabilities"]["users"],
        json!({ "placement": "inline" })
    );

    // Home: restating what is deployed is fine; moving it is refused.
    let home_ok = with(
        &home_json,
        json!({
            "ai-agents": { "placement": "own_worker" },
            "models": { "placement": "own_worker" },
            "secrets": { "placement": "inline" },
            "users": { "placement": "inline" }
        }),
    );
    assert_eq!(problems(&home_ok), Vec::<String>::new());
    assert_eq!(
        problems(&with(
            &home_json,
            json!({ "ai-agents": { "placement": "inline" } })
        )),
        [
            "The capability \"ai-agents\" runs in its own Worker, pigeon-ai-agents on the home \
          instance; its placement there is read-only, and moving it is a code landing, not a \
          record change."
        ]
    );
    assert_eq!(
        problems(&with(
            &home_json,
            json!({ "models": { "placement": "inline" } })
        ))[0],
        "The capability \"models\" runs in its own Worker, pigeon-model-gateway on the home \
         instance; its placement there is read-only, and moving it is a code landing, not a \
         record change."
    );
    // A placement the capability does not offer, anywhere.
    assert_eq!(
        problems(&with(
            &home_json,
            json!({ "secrets": { "placement": "own_worker" } })
        )),
        ["The capability \"secrets\" cannot run own_worker: it offers inline."]
    );

    // A linked record may give only inline, even where the catalogue offers more.
    assert_eq!(
        problems(&with(
            &linked_json,
            json!({ "ai-agents": { "placement": "own_worker" } })
        )),
        [
            "The capability \"ai-agents\" cannot be enabled on a linked instance yet.",
            "The capability \"ai-agents\" cannot run own_worker on a linked instance yet: only \
             inline is accepted."
        ]
    );

    // Where each runs: home's from the catalogue whatever the record says; a linked instance's
    // inline, in its own Worker, unless its record says otherwise.
    let plain_home: Instance = serde_json::from_value(home_json.clone()).unwrap();
    for (name, p, w) in [
        ("ai-agents", Placement::OwnWorker, Some("pigeon-ai-agents")),
        ("models", Placement::OwnWorker, Some("pigeon-model-gateway")),
        ("secrets", Placement::Inline, None),
        ("proxy", Placement::Inline, None),
    ] {
        assert_eq!(
            placement(&plain_home, name),
            Some((p, w.map(str::to_string))),
            "{name}"
        );
    }
    let plain_linked: Instance = serde_json::from_value(linked_json.clone()).unwrap();
    assert_eq!(
        placement(&plain_linked, "secrets"),
        Some((Placement::Inline, Some("docs".to_string())))
    );
    assert_eq!(placement(&plain_linked, "teleport"), None);
    let shown = placements(&plain_home);
    assert_eq!(
        shown["ai-agents"],
        json!({ "placement": "own_worker", "worker": "pigeon-ai-agents",
                "read_only": true, "choices": [] })
    );
    assert_eq!(shown["secrets"]["placement"], "inline");
    // A linked instance: only what it has, inline being the one choice.
    assert_eq!(
        placements(&plain_linked),
        json!({ "instances": { "placement": "inline", "worker": "docs",
                               "read_only": false, "choices": ["inline"] } })
    );
    let v = pigeon_capabilities::versioning::versions_view(&[], &[], &[plain_home], false);
    assert_eq!(
        v["instances"][0]["placements"]["models"]["worker"],
        "pigeon-model-gateway"
    );
}

/// Platform › Architecture is the selected instance's (capability scoping, M1): a linked
/// instance's own Worker, hostnames, bucket, enabled capabilities and scoped tools, and none
/// of the platform's. What `/architecture.json` serves gains nothing.
#[test]
fn architecture_of_a_linked_instance_is_its_own() {
    use pigeon_capabilities::architecture::{diagram, instance_model, public};
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    let record: Instance = serde_json::from_value(json!({
        "id": "docs", "title": "Docs",
        "account": { "kind": "linked", "account_id": "0123456789abcdef0123456789abcdef" },
        "zones": ["example.org"], "worker": "example",
        "repo": { "kind": "github", "full_name": "example/docs" },
        "sites": [{ "name": "docs", "bucket": "example-site", "targets": [
            { "kind": "preview" },
            { "kind": "zone", "hostname": "www.example.org", "route": "www.example.org/*", "enabled": true },
            { "kind": "domain", "hostname": "docs.example.org" },
            { "kind": "subdomain", "label": "example" }
        ] }],
        "source": { "state": "enabled" },
        "observability": { "state": "enabled" }
    }))
    .unwrap();
    let scope = Scope::new(&record).unwrap();
    assert_eq!(scope.capabilities(), ["source", "observability"]);
    let platform = json!({
        "commit": "0123456789ab", "digest": "d1",
        "resources": { "zone": "pigeon.example" },
        "parts": [{ "name": "source", "summary": "Read-only source." }],
        "workers": [{ "name": "pigeon-platform" }]
    });
    let tools = vec![
        "source_read_file".to_string(),
        "observability_worker_metrics".into(),
    ];
    let own = instance_model(&record, scope.capabilities(), &tools, &platform);
    let v = serde_json::to_value(&own).unwrap();
    assert_eq!(
        v["instance"],
        json!({ "id": "docs", "title": "Docs", "home": false })
    );
    assert_eq!(v["workers"].as_array().unwrap().len(), 1);
    let w = &v["workers"][0];
    assert_eq!(w["name"], "example");
    assert_eq!(w["workers_dev"], true);
    assert_eq!(w["routes"], json!(["www.example.org/*"]));
    assert_eq!(
        w["tools"],
        json!(["observability_worker_metrics", "source_read_file"])
    );
    assert_eq!(w["uses"], json!(["source", "observability"]));
    let hosts: Vec<&str> = v["hostnames"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["host"].as_str().unwrap())
        .collect();
    assert_eq!(
        hosts,
        [
            "docs.example.org",
            "example.pigeon.example",
            "www.example.org"
        ]
    );
    assert!(
        v["hostnames"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["worker"] == "example")
    );
    assert_eq!(v["resources"]["buckets"], json!(["example-site"]));
    assert_eq!(v["resources"]["zone"], "example.org");
    assert_eq!(v["resources"]["repos"], json!(["example/docs"]));
    assert_eq!(v["parts"][0]["name"], "source");
    assert_eq!(v["parts"][0]["summary"], "Read-only source.");
    assert_eq!(v["parts"][1]["capability"], true);
    assert_eq!(v["commit"], "0123456789ab");
    assert!(!v.to_string().contains("pigeon-platform"));
    assert!(diagram(&own).starts_with("<svg "));
    // The public view, as `/architecture.json` serves it, never carries an instance or buckets.
    let shown = serde_json::to_value(public(&own)).unwrap();
    assert!(shown.get("instance").is_none());
    assert!(shown["resources"].get("buckets").is_none());
}

/// A view newer than the version an instance's pin selects stays, flagged for an upgrade;
/// with no pin the instance runs the newest version and needs none.
#[test]
fn console_manifest_flags_views_that_need_an_upgrade() {
    use pigeon_capabilities::instances::manifest::{manifest, selected};
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_core::versions::{Component, Kind, Version};

    let sites = Component::began("sites", Kind::Capability)
        .entry("1.1.0", "Deployments")
        .serving(&["1.1.0", "1.0.0"]);
    let mut record: Instance = serde_json::from_value(json!({
        "id": "sample", "title": "Sample", "account": { "kind": "linked" },
        "sites": [{ "name": "blog", "bucket": "sample-site", "targets": [] }],
        "publishing": { "state": "enabled", "site": "blog" },
        "pins": { "sites": "~1.0" }
    }))
    .unwrap();
    let carried = [sites];
    assert_eq!(
        selected(&record, "sites", &carried),
        Some(Version::new(1, 0, 0))
    );
    // Declared views carry no `since` today, so none needs an upgrade.
    let scope = Scope::new(&record).unwrap();
    let m = manifest(&record, &scope, &[], &carried, None);
    assert!(m["areas"][2]["views"][1]["upgrade"].is_null());
    // A view first served at 1.1.0 is flagged on an instance pinned to 1.0.
    let view = pigeon_capabilities::instances::tools::View {
        area: "site",
        id: "deployments",
        title: "Deployments",
        order: 20,
        since: Some("1.1.0"),
    };
    assert_eq!(
        pigeon_capabilities::instances::manifest::upgrade(
            view.since,
            selected(&record, "sites", &carried)
        ),
        json!({ "selected": "1.0.0", "since": "1.1.0" })
    );
    record.pins.clear();
    assert_eq!(
        selected(&record, "sites", &carried),
        Some(Version::new(1, 1, 0))
    );
    assert!(
        pigeon_capabilities::instances::manifest::upgrade(
            view.since,
            selected(&record, "sites", &carried)
        )
        .is_null()
    );
    assert!(
        pigeon_capabilities::instances::manifest::upgrade(
            view.since,
            selected(&record, "nothing", &carried)
        )
        .is_null()
    );
}

/// Area ids and keys are stable (P12), every view id is an anchor-shaped id unique within its
/// area, and the manifest is data only: no HTML anywhere in it (P16).
#[test]
fn console_manifest_ids_are_stable_and_data_only() {
    use pigeon_capabilities::console::sections::{AREAS, view_id};
    let ids: Vec<&str> = AREAS.iter().map(|a| a.id).collect();
    assert_eq!(
        ids,
        [
            "inbox",
            "overview",
            "site",
            "storage",
            "source",
            "ci",
            "metrics",
            "jobs",
            "ingest",
            "email",
            "capabilities",
            "sessions",
            "cost",
            "access",
            "instances",
            "platform"
        ]
    );
    let keys: Vec<&str> = AREAS.iter().map(|a| a.key).collect();
    assert_eq!(
        keys,
        [
            "i", "v", "w", "b", "r", "k", "m", "j", "e", "l", "x", "s", "c", "a", "n", "p"
        ]
    );
    let every = json!({
        "id": "all", "title": "All <b>", "account": { "kind": "home" },
        "zones": ["example.com"], "worker": "all-worker",
        "repo": { "kind": "github", "full_name": "example/all" },
        "sites": [{ "name": "blog", "bucket": "all-site", "auth": "passkey", "targets": [] }],
        "publishing": { "state": "enabled", "site": "blog" },
        "storage": { "state": "enabled", "store": "s", "bucket": "b", "prefix": "p/" },
        "source": { "state": "enabled" },
        "observability": { "state": "enabled" },
        "ingest": { "state": "disabled" }
    });
    let m = manifest_of(&every, Some(0));
    assert_eq!(
        manifest_areas(&m),
        [
            "inbox",
            "overview",
            "site",
            "storage",
            "source",
            "metrics",
            "capabilities",
            "cost",
            "instances",
            "platform"
        ]
    );
    for area in m["areas"].as_array().unwrap() {
        let views = area["views"].as_array().unwrap();
        for (i, v) in views.iter().enumerate() {
            assert!(view_id(v["id"].as_str().unwrap()), "{v}");
            assert!(views[..i].iter().all(|w| w["id"] != v["id"]));
        }
    }
    // The record's own title is data, passed through as it is; nothing else holds markup.
    let mut text = m.clone();
    text["instance"]["title"] = json!("All");
    let text = text.to_string();
    assert!(!text.contains('<') && !text.contains("html"), "{text}");
}

/// The Inbox counts every pending request on the home instance, landings included, and only
/// its own requests on another.
#[test]
fn console_manifest_inbox_is_filtered_to_the_instance() {
    use pigeon_capabilities::instances::manifest::inbox_count;
    use pigeon_capabilities::instances::record::Instance;
    use pigeon_core::approval::Record;

    let approval = |kind: &str, subject: &str, summary: Value, state: &str| -> Record {
        serde_json::from_value(json!({
            "id": "a", "kind": kind, "subject": subject, "digest": "d", "summary": summary,
            "caller_id": "c", "approver_id": null, "state": state,
            "created_ms": 0, "expires_ms": 1, "decided_by": null, "decided_ms": null,
            "used_ms": null
        }))
        .unwrap()
    };
    let open = [
        approval("instance_draft", "sample", json!({}), "pending"),
        approval("linked_plan", "other", json!({}), "pending"),
        approval(
            "tool_call",
            "site_deploy",
            json!({ "args": { "instance": "sample" } }),
            "pending",
        ),
        approval(
            "tool_call",
            "source_token",
            json!({ "args": {} }),
            "pending",
        ),
        approval("instance_draft", "sample", json!({}), "denied"),
    ];
    let home: Instance = serde_json::from_value(
        json!({ "id": "example/platform", "title": "Example", "account": { "kind": "home" } }),
    )
    .unwrap();
    let sample: Instance = serde_json::from_value(
        json!({ "id": "sample", "title": "Sample", "account": { "kind": "linked" } }),
    )
    .unwrap();
    assert_eq!(inbox_count(&home, &open, 2), 6);
    assert_eq!(inbox_count(&sample, &open, 2), 2);
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
    let first = plan(&inst, &seen(false, false, None, Some("np")), &empty, None);
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
    let again = plan(&inst, &seen(true, true, Some(true), Some("np")), &l, None);
    assert_eq!(tags(&again), ["ok", "ok", "ok"]);
    assert!(again.iter().all(|s| !s.changes()));

    // Same names, not pigeon's: conflicts, never touched.
    let theirs = plan(
        &inst,
        &seen(true, true, Some(false), Some("np")),
        &empty,
        None,
    );
    assert_eq!(tags(&theirs)[..2], ["conflict", "conflict"]);
    assert!(theirs.iter().any(Step::stops));

    // pigeon's Worker with an older module: an update, not a conflict.
    let mut old = l.clone();
    old.workers = vec![("noisypigeon".into(), "older".into())];
    let upd = plan(&inst, &seen(true, true, Some(true), Some("np")), &old, None);
    assert!(matches!(&upd[1], Step::UploadWorker { replaces: true, .. }));
    assert_ne!(module_digest("noisypigeon-site", Some("eu")), "older");

    // No workers.dev subdomain on the account: blocked, for the person.
    let blocked = plan(&inst, &seen(false, false, None, None), &empty, None);
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
    let d = plan(&with_domain, &o, &l, None);
    assert!(matches!(d.last(), Some(Step::AttachDomain { zone_id, .. }) if zone_id == "z1"));
    o.domains[0].service = Some("noisypigeon".into());
    assert!(matches!(
        plan(&with_domain, &o, &l, None).last(),
        Some(Step::Ok { .. })
    ));
    o.domains[0].service = Some("someone-else".into());
    assert!(plan(&with_domain, &o, &l, None).last().unwrap().stops());
    o.domains[0] = SeenDomain {
        hostname: "noisypigeon.com".into(),
        zone_id: None,
        service: None,
    };
    assert!(matches!(
        plan(&with_domain, &o, &l, None).last(),
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
    assert!(matches!(plan(&inst, &sub, &l, None).last(),
        Some(Step::AttachSubdomain { upstream, worker, .. })
            if *upstream == up && worker == "pigeon-sites"));
    sub.subdomains[0].service = Some("pigeon-sites".into());
    sub.subdomains[0].upstream = Some(up.clone());
    assert!(matches!(
        plan(&inst, &sub, &l, None).last(),
        Some(Step::Ok { .. })
    ));
    sub.subdomains[0].service = Some("someone-else".into());
    assert!(plan(&inst, &sub, &l, None).last().unwrap().stops());
    let mut no_dev = sub.clone();
    no_dev.account_subdomain = None;
    no_dev.subdomains[0].service = None;
    assert!(
        plan(&inst, &no_dev, &l, None)
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
        platform_worker: "pigeon-platform",
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
    let steps = plan(&instance, &observed, &ledger, None);
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
    assert!(
        plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::stops)
    );
    record(&mut ledger, &steps[0]);
    assert!(
        !plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::stops)
    );
    observed.worker_exists = true;
    assert!(
        plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::stops)
    );
    record(&mut ledger, &steps[1]);
    observed.workers_dev = Some(true);
    assert!(
        !plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::changes)
    );

    instance.sites[0].auth = None;
    assert!(
        plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::stops)
    );
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

#[test]
fn scoped_storage_and_metrics_calls_refuse_raw_targets() {
    use pigeon_capabilities::observability::instance::metrics_arguments;
    use pigeon_capabilities::storage::scoped_instance;

    // Storage: the instance and relative object arguments only.
    assert_eq!(
        scoped_instance(
            "storage_get_object",
            &json!({"instance":"archive","key":"a"})
        )
        .unwrap(),
        "archive"
    );
    assert_eq!(
        scoped_instance(
            "storage_list_objects",
            &json!({"instance":"archive","prefix":"p/","delimiter":"/","max_keys":5,"token":"t"})
        )
        .unwrap(),
        "archive"
    );
    for tool in ["storage_get_object", "storage_list_objects"] {
        for raw in ["account", "bucket"] {
            let mut args = json!({"instance":"archive","key":"a"});
            if tool == "storage_list_objects" {
                args = json!({"instance":"archive"});
            }
            args[raw] = json!("elsewhere");
            assert!(scoped_instance(tool, &args).is_err(), "{tool} {raw}");
        }
    }
    assert!(scoped_instance("storage_get_object", &json!({"key":"a"})).is_err());
    assert!(scoped_instance("storage_head_object", &json!({"instance":"archive"})).is_err());

    // Metrics: the instance and the window only.
    assert_eq!(
        metrics_arguments(&json!({"instance":"blog"})).unwrap(),
        ("blog", 24)
    );
    assert_eq!(
        metrics_arguments(&json!({"instance":"blog","hours":168})).unwrap(),
        ("blog", 168)
    );
    for extra in ["worker", "account", "account_id", "script"] {
        let mut args = json!({"instance":"blog"});
        args[extra] = json!("other");
        assert!(metrics_arguments(&args).is_err(), "{extra}");
    }
    for hours in [json!(0), json!(169), json!("24")] {
        assert!(metrics_arguments(&json!({"instance":"blog","hours":hours})).is_err());
    }
    assert!(metrics_arguments(&json!({"hours":1})).is_err());
}

#[test]
fn disabled_publishing_hides_deploy_and_rollback_from_scoped_discovery() {
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_core::mcp::Tool;

    let mut record = json!({
        "id": "blog", "title": "Blog", "account": {"kind":"home"},
        "sites": [{"name":"blog", "bucket":"site", "targets":[]}],
        "publishing": {"state":"enabled", "site":"blog"}
    });
    let listed = |record: &Value| {
        let instance: Instance = serde_json::from_value(record.clone()).unwrap();
        let scope = Scope::new(&instance).unwrap();
        let mut tools: Vec<Tool> = ["site_deploy", "site_rollback", "call"]
            .into_iter()
            .map(|name| Tool {
                name,
                description: "",
                input_schema: json!({"properties": {"instance": {}}, "required": ["instance"]}),
            })
            .collect();
        scope.restrict_tools(&mut tools);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name).collect();
        (
            names,
            scope.bind("site_deploy", &json!({"rev":"main"})).is_ok(),
        )
    };
    assert_eq!(
        listed(&record),
        (vec!["site_deploy", "site_rollback", "call"], true)
    );
    record["publishing"] = json!({"state":"disabled"});
    assert_eq!(listed(&record), (Vec::<&str>::new(), false));
}

/// `instance_retire_plan` and `instance_retire_apply`: only what pigeon's ledger says pigeon
/// made is removed, in order (stop serving, Worker, bucket, pigeon's state, record last); what
/// pigeon did not make is kept; the home instance and a depended-on instance are refused; the
/// digest binds the steps; and a run that stopped resumes at the failed step.
#[test]
fn instance_retire_removes_only_what_pigeon_made() {
    use pigeon_capabilities::instances::plan::{
        Ledger, SeenDomain, SeenSubdomain, Step as ApplyStep, record,
    };
    use pigeon_capabilities::instances::record::Instance;
    use pigeon_capabilities::instances::retire::{
        Journal, Seen, SeenBucket, SeenWorker, SiteUser, Step, describe, digest, hosts, plan,
        refusal, state_keys, unrecord,
    };

    let account = "a".repeat(32);
    let inst: Instance = serde_json::from_value(json!({
        "id": "canary", "title": "canary",
        "account": { "kind": "linked", "account_id": account },
        "zones": ["canary.example"], "worker": "canary",
        "sites": [{ "name": "blog", "bucket": "canary-site", "jurisdiction": "eu",
            "targets": [{ "kind": "preview" },
                { "kind": "domain", "hostname": "canary.example" },
                { "kind": "subdomain", "label": "canary" },
                { "kind": "zone", "hostname": "www.canary.example", "route": "www.canary.example/*" }] }],
        "version": 3
    }))
    .unwrap();
    assert_eq!(
        hosts(&inst, "pigeon.dev"),
        ["canary.example", "canary.pigeon.dev", "www.canary.example"]
    );

    // The ledger as instance_apply leaves it: the bucket, the Worker, the domain and subdomain.
    let mut l = Ledger::default();
    for s in [
        ApplyStep::CreateBucket {
            name: "canary-site".into(),
            jurisdiction: Some("eu".into()),
            protected: false,
        },
        ApplyStep::UploadWorker {
            name: "canary".into(),
            bucket: Some("canary-site".into()),
            jurisdiction: Some("eu".into()),
            module: "m".into(),
            replaces: false,
            origin: None,
            redirects: None,
        },
        ApplyStep::AttachDomain {
            hostname: "canary.example".into(),
            zone_id: "z".into(),
            worker: "canary".into(),
        },
        ApplyStep::AttachSubdomain {
            hostname: "canary.pigeon.dev".into(),
            zone_id: "zp".into(),
            worker: "pigeon-sites".into(),
            upstream: "https://canary.np.workers.dev".into(),
        },
    ] {
        record(&mut l, &s);
    }
    assert_eq!(l.domains, ["canary.example"]);
    assert_eq!(l.subdomains, ["canary.pigeon.dev"]);

    let seen = Seen {
        account_id: Some(account.clone()),
        workers: vec![SeenWorker {
            name: "canary".into(),
            exists: true,
            workers_dev: Some(true),
        }],
        buckets: vec![SeenBucket {
            name: "canary-site".into(),
            jurisdiction: Some("eu".into()),
            exists: true,
            objects: 12,
        }],
        domains: vec![SeenDomain {
            hostname: "canary.example".into(),
            zone_id: None,
            service: Some("canary".into()),
        }],
        subdomains: vec![SeenSubdomain {
            hostname: "canary.pigeon.dev".into(),
            zone_id: Some("zp".into()),
            worker: "pigeon-sites".into(),
            service: Some("pigeon-sites".into()),
            upstream: Some("https://canary.np.workers.dev".into()),
        }],
        site_users: vec![SiteUser {
            id: "u1".into(),
            host: "canary.pigeon.dev".into(),
        }],
        settings: state_keys("canary"),
        linked: true,
    };
    let tag = |s: &Step| {
        serde_json::to_value(s).unwrap()["step"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let removing =
        |steps: &[Step]| -> Vec<String> { steps.iter().filter(|s| s.removes()).map(tag).collect() };

    // Everything pigeon made goes, in order; the route it never made is kept.
    let steps = plan(&inst, &seen, &l, &[], false);
    assert!(!steps.iter().any(Step::stops));
    assert_eq!(
        removing(&steps),
        [
            "detach_subdomain",
            "detach_domain",
            "disable_workers_dev",
            "delete_worker",
            "empty_bucket",
            "delete_bucket",
            "remove_site_user",
            "forget",
            "forget",
            "forget",
            "forget",
            "forget",
            "forget",
            "forget",
            "forget",
            "forget",
            "unlink",
            "remove_record",
        ]
    );
    assert!(matches!(steps.last(), Some(Step::RemoveRecord { id }) if id == "canary"));
    assert!(steps.iter().any(|s| matches!(s,
        Step::Keep { what, name, .. } if what == "route" && name == "www.canary.example/*")));
    assert!(steps.iter().any(|s| matches!(s,
        Step::RemoveSiteUser { id, host } if id == "u1" && host == "canary.pigeon.dev")));
    // The publishing queue row is forgotten, and the ledger after the rest.
    let forgets: Vec<&str> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Forget { key } => Some(key.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(forgets[0], "github:deploy:canary");
    assert!(forgets.contains(&"digitalocean-token:canary"));
    assert!(forgets.contains(&"digitalocean-spaces-key:canary"));
    assert!(forgets.contains(&"digitalocean-droplet-token:canary"));
    assert!(!forgets.contains(&"digitalocean-droplet-job:canary"));
    assert_eq!(forgets.last(), Some(&"ledger:canary"));
    assert_eq!(describe(&steps).len(), steps.len());
    // Keeping the link, for a reset.
    let kept = plan(&inst, &seen, &l, &[], true);
    assert!(!kept.iter().any(|s| matches!(s, Step::Unlink)));
    // Another instance linked to the same account: forget this link, keep the grant.
    let mut sibling = inst.clone();
    sibling.id = "prod".into();
    sibling.worker = None;
    sibling.sites.clear();
    let shared = plan(&inst, &seen, &l, std::slice::from_ref(&sibling), false);
    assert!(!shared.iter().any(|s| matches!(s, Step::Unlink)));
    assert!(shared.iter().any(|s| matches!(s,
        Step::ForgetLink { shared_with } if shared_with == &vec!["prod".to_string()])));

    // Same names, not in the ledger: kept, and nothing in the account is removed.
    let theirs = plan(&inst, &seen, &Ledger::default(), &[], false);
    assert!(!theirs.iter().any(Step::in_linked_account));
    let keeps: Vec<String> = theirs
        .iter()
        .filter_map(|s| match s {
            Step::Keep { what, .. } => Some(what.clone()),
            _ => None,
        })
        .collect();
    for what in ["Worker", "bucket", "custom domain"] {
        assert!(keeps.iter().any(|k| k == what), "{what} kept");
    }
    // A subdomain served by another Worker is kept; only pigeon's upstream for it is forgotten.
    let mut other_sub = seen.clone();
    other_sub.subdomains[0].service = Some("someone-else".into());
    let o = plan(&inst, &other_sub, &l, &[], false);
    assert!(
        o.iter()
            .any(|s| matches!(s, Step::Keep { what, .. } if what == "subdomain"))
    );
    assert!(
        o.iter()
            .any(|s| matches!(s, Step::DetachSubdomain { worker: None, .. }))
    );
    // A custom domain served by a Worker pigeon did not upload is kept.
    let mut foreign = seen.clone();
    foreign.domains[0].service = Some("someone-else".into());
    assert!(
        !plan(&inst, &foreign, &l, &[], false)
            .iter()
            .any(|s| matches!(s, Step::DetachDomain { .. }))
    );
    // An empty bucket is deleted with no emptying step; a Worker already gone is not touched.
    let mut bare = seen.clone();
    bare.buckets[0].objects = 0;
    bare.workers[0] = SeenWorker {
        name: "canary".into(),
        exists: false,
        workers_dev: None,
    };
    let b = removing(&plan(&inst, &bare, &l, &[], false));
    assert!(
        !b.iter()
            .any(|s| s == "empty_bucket" || s == "delete_worker")
    );
    assert!(b.iter().any(|s| s == "delete_bucket"));

    // The home instance is refused.
    let mut home = inst.clone();
    home.account = serde_json::from_value(json!({ "kind": "home" })).unwrap();
    assert!(refusal(&home).is_some());
    assert!(plan(&home, &seen, &l, &[], false).iter().all(Step::stops));
    assert!(refusal(&inst).is_none());

    // Another instance reading pigeon's bucket, or ingesting into it, stops the plan.
    let mut reader = inst.clone();
    reader.id = "reader".into();
    reader.worker = Some("reader".into());
    reader.sites[0].targets.clear();
    let stops = |others: &[Instance]| {
        plan(&inst, &seen, &l, others, false)
            .iter()
            .any(Step::stops)
    };
    assert!(stops(std::slice::from_ref(&reader)));
    reader.sites[0].bucket = "reader-site".into();
    assert!(!stops(std::slice::from_ref(&reader)));
    reader.ingest = Some(
        serde_json::from_value(json!({ "state": "enabled", "source": "r2:fixture",
            "bucket": "src", "destination_bucket": "canary-site" }))
        .unwrap(),
    );
    assert!(stops(std::slice::from_ref(&reader)));

    // Account resources in the ledger but no link to reach them: stopped, not forgotten.
    let mut unlinked = seen.clone();
    unlinked.account_id = None;
    assert!(
        plan(&inst, &unlinked, &l, &[], false)
            .iter()
            .any(Step::stops)
    );

    // The digest binds the steps, the account and the version: a changed plan is refused.
    let d = digest(&inst, Some(&account), &steps);
    assert_eq!(d, digest(&inst, Some(&account), &steps));
    let mut more = seen.clone();
    more.buckets[0].objects = 13;
    assert_ne!(
        d,
        digest(&inst, Some(&account), &plan(&inst, &more, &l, &[], false))
    );
    assert_ne!(d, digest(&inst, Some(&"b".repeat(32)), &steps));
    let mut newer = inst.clone();
    newer.version += 1;
    assert_ne!(d, digest(&newer, Some(&account), &steps));
    assert_ne!(
        d,
        pigeon_capabilities::instances::plan::digest(&inst, &account, &[])
    );

    // The journal holds only removing steps, runs them in order and resumes at a failed step.
    let mut ledger = l.clone();
    let mut ran: Vec<String> = Vec::new();
    let mut fail_once = true;
    let mut run = |j: &mut Journal| {
        while let Some(i) = j.next() {
            let step = j.steps[i].clone();
            if matches!(step, Step::DeleteBucket { .. }) && fail_once {
                fail_once = false;
                j.fail(i, "bucket not empty");
                return false;
            }
            ran.push(tag(&step));
            unrecord(&mut ledger, &step);
            j.finish(i);
        }
        true
    };
    let mut j = Journal::new("canary", Some(&account), "caller", &steps);
    assert_eq!(j.steps.len(), removing(&steps).len());
    assert!(j.needs_account());
    assert!(!run(&mut j));
    assert!(j.failed.as_deref().unwrap().contains("bucket not empty"));
    assert!(!j.complete());
    // Kept and read back as the Directory would, then resumed: the failed step runs again.
    let mut j: Journal = serde_json::from_str(&serde_json::to_string(&j).unwrap()).unwrap();
    assert!(run(&mut j));
    assert!(j.complete() && j.failed.is_none() && !j.needs_account());
    // Once done, nothing runs again.
    assert!(run(&mut j));
    let expected: Vec<String> = removing(&steps);
    assert_eq!(ran, expected);
    assert!(ledger.workers.is_empty() && ledger.buckets.is_empty());
    assert!(ledger.domains.is_empty() && ledger.subdomains.is_empty());
}

/// The home instance's guard rails, before any home plan exists: one home record, an account
/// kind no draft changes, the platform's own Worker named only by home, and a home plan that
/// keeps or blocks every platform-owned Worker and hostname instead of acting on it. Linked
/// instances plan as before.
#[test]
fn home_instance_guard_rails() {
    use pigeon_capabilities::instances::plan::{
        Ledger, Observed, PLATFORM_OWNED, SeenDomain, SeenSubdomain, Step, describe, plan,
        platform_owned,
    };
    use pigeon_capabilities::instances::record::{
        AccountRef, Instance, Rules, Target, account_change, approval_problems, reserved_labels,
        validate,
    };
    use pigeon_capabilities::instances::retire::refusal;

    let reserved = reserved_labels(
        &[
            "platform.pigeon.dev".into(),
            "proxy.pigeon.dev".into(),
            "pigeon.dev".into(),
        ],
        "pigeon.dev",
    );
    let workers: Vec<String> = ["ai-agents", "pigeon-platform", "pigeon-sites", "proxy"]
        .map(String::from)
        .to_vec();
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
        platform_worker: "pigeon-platform",
    };
    let home: Instance = serde_json::from_value(json!({
        "id": "pigeon-security/platform", "title": "pigeon",
        "account": { "kind": "home" }, "zones": ["pigeon.dev"]
    }))
    .unwrap();
    let linked: Instance = serde_json::from_value(json!({
        "id": "noisypigeon", "title": "noisypigeon",
        "account": { "kind": "linked", "account_id": "a".repeat(32) },
        "zones": ["noisypigeon.com"], "worker": "noisypigeon",
        "sites": [{ "name": "blog", "bucket": "noisypigeon-site",
            "targets": [{ "kind": "preview" }, { "kind": "domain", "hostname": "noisypigeon.com" }] }],
        "version": 1
    }))
    .unwrap();
    let has = |problems: &[String], why: &str| {
        assert!(
            problems.iter().any(|p| p.contains(why)),
            "{why}: {problems:?}"
        );
    };
    assert_eq!(
        validate(&home, std::slice::from_ref(&linked), &rules),
        Vec::<String>::new()
    );
    assert_eq!(
        validate(&linked, std::slice::from_ref(&home), &rules),
        Vec::<String>::new()
    );

    // One home instance: a second is refused on edit, and again on approval.
    let mut second = home.clone();
    second.id = "other/platform".into();
    has(
        &validate(&second, std::slice::from_ref(&home), &rules),
        "only one home instance",
    );
    has(
        &approval_problems(&second, &[home.clone(), linked.clone()]),
        "only one home instance",
    );
    assert!(approval_problems(&home, &[home.clone(), linked.clone()]).is_empty());

    // A draft never changes a stored record's account kind, either way.
    let mut to_home = linked.clone();
    to_home.account = AccountRef::Home;
    has(
        &account_change(Some(&linked), &to_home)
            .into_iter()
            .collect::<Vec<_>>(),
        "may not change it to home",
    );
    let problems = approval_problems(&to_home, &[home.clone(), linked.clone()]);
    has(&problems, "may not change it to home");
    has(&problems, "only one home instance");
    let mut to_linked = home.clone();
    to_linked.account = AccountRef::Linked {
        account_id: Some("b".repeat(32)),
    };
    has(
        &approval_problems(&to_linked, &[home.clone(), linked.clone()]),
        "may not change it to linked",
    );
    let mut relinked = linked.clone();
    relinked.account = AccountRef::Linked {
        account_id: Some("c".repeat(32)),
    };
    assert_eq!(account_change(Some(&linked), &relinked), None);
    assert_eq!(account_change(None, &to_home), None, "a new record");

    // Home may name the platform's own Worker as its worker; linked never may, and home may
    // not take any other reserved Worker or serve a platform hostname.
    let mut managed = home.clone();
    managed.worker = Some("pigeon-platform".into());
    assert_eq!(
        validate(&managed, std::slice::from_ref(&linked), &rules),
        Vec::<String>::new()
    );
    let mut taken = linked.clone();
    taken.worker = Some("pigeon-platform".into());
    has(&validate(&taken, &[], &rules), "platform's own");
    taken.worker_override = true;
    has(&validate(&taken, &[], &rules), "platform's own");
    let bare = Rules {
        reserved_workers: &[],
        ..rules
    };
    has(&validate(&taken, &[], &bare), "platform's own");
    let mut sites_worker = home.clone();
    sites_worker.worker = Some("pigeon-sites".into());
    sites_worker.worker_override = true;
    has(&validate(&sites_worker, &[], &rules), "platform's own");
    let mut serving = managed.clone();
    serving.sites = serde_json::from_value(json!([{ "name": "landing", "bucket": "landing-site",
        "bucket_override": true,
        "targets": [{ "kind": "domain", "hostname": "platform.pigeon.dev" },
            { "kind": "domain", "hostname": "pigeon.dev" },
            { "kind": "domain", "hostname": "blog.pigeon.dev" }] }]))
    .unwrap();
    let problems = validate(&serving, &[], &rules);
    has(&problems, "\"platform.pigeon.dev\" is the platform's own");
    has(&problems, "\"pigeon.dev\" is the platform's own");
    assert!(!problems.iter().any(|p| p.contains("blog.pigeon.dev")));

    // The platform-owned set comes from the reserved names: the footprint's Workers and the
    // apex, platform. and proxy. hosts. Nothing is platform-owned for a linked instance.
    let owned = platform_owned(&serving, &rules).unwrap();
    for w in ["pigeon-platform", "pigeon-sites", "proxy", "ai-agents"] {
        assert!(owned.worker(w), "{w}");
    }
    for h in ["pigeon.dev", "platform.pigeon.dev", "PROXY.pigeon.dev"] {
        assert!(owned.host(h), "{h}");
    }
    assert!(!owned.host("blog.pigeon.dev") && !owned.worker("landing"));
    assert!(platform_owned(&linked, &rules).is_none());

    // A home plan naming the platform's Worker and hosts: only Keep or Blocked for them.
    serving.sites[0].targets.insert(0, Target::Preview);
    serving.sites[0].targets.push(Target::Subdomain {
        label: "landing".into(),
    });
    let domain = |h: &str| SeenDomain {
        hostname: h.into(),
        zone_id: Some("z".into()),
        service: None,
    };
    let seen = Observed {
        account_id: "h".repeat(32),
        bucket_exists: false,
        worker_exists: true,
        workers_dev: Some(false),
        account_subdomain: Some("home".into()),
        domains: vec![
            domain("platform.pigeon.dev"),
            domain("pigeon.dev"),
            domain("blog.pigeon.dev"),
        ],
        subdomains: vec![SeenSubdomain {
            hostname: "landing.pigeon.dev".into(),
            zone_id: Some("z".into()),
            worker: "pigeon-sites".into(),
            service: None,
            upstream: None,
        }],
    };
    let l = Ledger::default();
    let steps = plan(&serving, &seen, &l, Some(&owned));
    let actions: Vec<&Step> = steps.iter().filter(|s| s.changes()).collect();
    assert!(
        matches!(actions.as_slice(), [Step::CreateBucket { name, .. }] if name == "landing-site"),
        "{steps:?}"
    );
    let keep = |what: &str| Step::Keep {
        what: what.into(),
        name: "pigeon-platform".into(),
        why: PLATFORM_OWNED.into(),
    };
    assert_eq!(steps[1], keep("worker"), "never a conflict or an upload");
    assert_eq!(steps[2], keep("workers.dev"));
    let blocked: Vec<&str> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Blocked { why } => Some(why.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(blocked.len(), 4, "{blocked:?}");
    assert!(blocked.iter().all(|why| why.contains("platform-owned")));
    assert!(
        describe(&steps)[1]
            .as_str()
            .unwrap()
            .contains("platform-owned")
    );
    let mut fresh = seen.clone();
    fresh.worker_exists = false;
    assert!(
        !plan(&serving, &fresh, &l, Some(&owned))
            .iter()
            .any(|s| matches!(s, Step::UploadWorker { .. }))
    );
    // Without the platform-owned set, the home plan is blocked outright.
    assert!(matches!(
        plan(&serving, &seen, &l, None).as_slice(),
        [Step::Blocked { .. }]
    ));
    // A home Worker that is not the platform's still plans its own hosts; never the apex.
    let mut own = serving.clone();
    own.worker = Some("landing".into());
    own.worker_override = true;
    own.sites[0]
        .targets
        .retain(|t| !matches!(t, Target::Subdomain { .. }));
    let mut theirs = seen.clone();
    theirs.subdomains.clear();
    let steps = plan(&own, &theirs, &l, Some(&owned));
    assert!(steps.iter().any(
        |s| matches!(s, Step::AttachDomain { hostname, .. } if hostname == "blog.pigeon.dev")
    ));
    assert!(!steps.iter().any(
        |s| matches!(s, Step::AttachDomain { hostname, .. } if hostname != "blog.pigeon.dev")
    ));

    // Linked instances plan exactly as before, whatever set is passed.
    let linked_seen = Observed {
        account_id: "a".repeat(32),
        domains: vec![domain("noisypigeon.com")],
        subdomains: vec![],
        ..seen.clone()
    };
    assert_eq!(
        plan(&linked, &linked_seen, &l, None),
        plan(&linked, &linked_seen, &l, Some(&owned))
    );

    // Retire still refuses the home instance.
    assert!(refusal(&home).is_some() && refusal(&managed).is_some());
    assert!(refusal(&linked).is_none());
}

/// One mint as [`HomeMinter`] records it: the purpose, then each policy's scope and permissions.
type HomeMint = (String, Vec<(String, Vec<String>)>);

/// The `instance_home` token's minter in the suite: the core suite's [`MockTokens`], recording
/// each mint's purpose and policies as (scope, permission names).
struct HomeMinter<'a> {
    tokens: &'a super::core::MockTokens,
    minted: std::cell::RefCell<Vec<HomeMint>>,
}

impl pigeon_capabilities::instances::home::Minter for HomeMinter<'_> {
    async fn mint(
        &self,
        purpose: &str,
        policies: Vec<pigeon_core::tokens::Policy>,
        ttl_secs: u64,
    ) -> worker::Result<(String, String)> {
        use pigeon_core::tokens::{Scope, TokenApi, name};
        assert!(ttl_secs <= 900, "short-lived: {ttl_secs}");
        let seen = policies
            .iter()
            .map(|p| {
                let scope = match &p.scope {
                    Scope::Account => "account".to_string(),
                    Scope::Zones(z) => format!("zones:{}", z.join(",")),
                    Scope::AllZones => "all zones".to_string(),
                };
                (scope, p.permissions.clone())
            })
            .collect();
        self.minted.borrow_mut().push((purpose.into(), seen));
        let body = json!({
            "name": name(purpose, "2026-11-26T00:00:00Z"),
            "expires_on": "2026-11-26T00:10:00Z",
        });
        let made = self.tokens.create(&body).await?;
        Ok((made.id, made.value))
    }

    async fn revoke(&self, id: &str) -> worker::Result<()> {
        use pigeon_core::tokens::TokenApi;
        self.tokens.delete(id).await
    }
}

/// Area: instances. H2: the home instance is planned and applied like a linked one, with the
/// same passkey approval, but only its managed resources (the landing bucket) are ever steps;
/// everything platform-owned is kept or blocks the plan, never reaches the ledger, and the
/// work runs with a minted `instance_home` token that is revoked whatever happens.
#[test]
fn home_instance_plans_and_applies_with_a_minted_token() {
    use super::core::{MockTokens, token_block_on};
    use pigeon_capabilities::instances::home::{self, PERSON_STEPS, PURPOSE, with_token};
    use pigeon_capabilities::instances::plan::{
        Ledger, NO_SCRIPTS, Observed, PLATFORM_OWNED, SeenDomain, Step, digest, home_refusal, plan,
        platform_owned, record,
    };
    use pigeon_capabilities::instances::record::{
        Instance, LANDING_TEMPLATE, Rules, Target, reserved_labels, validate,
    };
    use pigeon_capabilities::split::{CORE_ONLY_PURPOSES, purpose_permissions};

    let reserved = reserved_labels(
        &[
            "platform.pigeon.dev".into(),
            "proxy.pigeon.dev".into(),
            "pigeon.dev".into(),
        ],
        "pigeon.dev",
    );
    let workers: Vec<String> = ["ai-agents", "pigeon-platform", "pigeon-sites", "proxy"]
        .map(String::from)
        .to_vec();
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
        platform_worker: "pigeon-platform",
    };
    let has = |problems: &[String], why: &str| {
        assert!(
            problems.iter().any(|p| p.contains(why)),
            "{why}: {problems:?}"
        );
    };

    // The record the plan describes: the platform's Worker, its host, and a landing site on the
    // apex. The id is renamed by R1's migration (`home_id_rename_r1`); a dotted id equal to a
    // zone is allowed for home.
    assert_eq!(LANDING_TEMPLATE, pigeon_landing::TEMPLATE);
    let home_json = json!({
        "id": "pigeon-security/platform", "title": "pigeon",
        "account": { "kind": "home" }, "zones": ["pigeon.dev"],
        "worker": "pigeon-platform",
        "platform": { "host": "platform.pigeon.dev" },
        "sites": [{ "name": "landing", "bucket": "pigeon-platform-site", "template": "landing@1",
            "targets": [{ "kind": "domain", "hostname": "pigeon.dev" }] }],
        "version": 3
    });
    let home: Instance = serde_json::from_value(home_json).unwrap();
    let stored = serde_json::to_value(&home).unwrap();
    assert_eq!(stored["platform"]["host"], "platform.pigeon.dev");
    assert_eq!(stored["sites"][0]["template"], "landing@1");
    let linked_json = json!({
        "id": "noisypigeon", "title": "noisypigeon",
        "account": { "kind": "linked", "account_id": "a".repeat(32) },
        "zones": ["noisypigeon.com"], "worker": "noisypigeon",
        "sites": [{ "name": "blog", "bucket": "noisypigeon-site",
            "targets": [{ "kind": "preview" }, { "kind": "domain", "hostname": "noisypigeon.com" }] }],
        "version": 1
    });
    let linked: Instance = serde_json::from_value(linked_json).unwrap();
    // A record without the new fields serializes as before, so its digest does not move.
    let text = serde_json::to_string(&linked).unwrap();
    assert!(
        !text.contains("platform") && !text.contains("template"),
        "{text}"
    );
    assert_eq!(
        validate(&home, std::slice::from_ref(&linked), &rules),
        Vec::<String>::new()
    );
    assert_eq!(
        validate(&linked, std::slice::from_ref(&home), &rules),
        Vec::<String>::new()
    );
    let mut dotted = home.clone();
    dotted.id = "pigeon.dev".into();
    assert_eq!(validate(&dotted, &[], &rules), Vec::<String>::new());
    let mut second = home.clone();
    second.id = "other/platform".into();
    has(
        &validate(&second, std::slice::from_ref(&home), &rules),
        "only one home instance",
    );
    // Only home names the platform's host, and only a platform host that is not the apex.
    let mut platform_linked = linked.clone();
    platform_linked.platform = home.platform.clone();
    has(
        &validate(&platform_linked, &[], &rules),
        "Only the home instance",
    );
    for bad_host in ["pigeon.dev", "blog.pigeon.dev", "platform.example.com"] {
        let mut h = home.clone();
        h.platform.as_mut().unwrap().host = bad_host.into();
        has(&validate(&h, &[], &rules), "The platform.host");
    }
    // The apex is the landing template's alone, and only as a custom domain.
    let mut not_landing = home.clone();
    not_landing.sites[0].template = None;
    has(
        &validate(&not_landing, &[], &rules),
        "\"pigeon.dev\" is the platform's own",
    );
    let mut other_template = home.clone();
    other_template.sites[0].template = Some("blog@1".into());
    let problems = validate(&other_template, &[], &rules);
    has(&problems, "template \"blog@1\" must be \"landing@1\"");
    has(&problems, "\"pigeon.dev\" is the platform's own");
    let mut routed = home.clone();
    routed.sites[0].targets = vec![Target::Zone {
        hostname: "pigeon.dev".into(),
        route: "pigeon.dev/*".into(),
        enabled: true,
    }];
    has(
        &validate(&routed, &[], &rules),
        "\"pigeon.dev\" is the platform's own",
    );
    let mut reserved_host = home.clone();
    reserved_host.sites[0].targets.push(Target::Domain {
        hostname: "platform.pigeon.dev".into(),
    });
    has(
        &validate(&reserved_host, &[], &rules),
        "\"platform.pigeon.dev\" is the platform's own",
    );

    // A home plan has only managed steps: the landing bucket is created; the platform's Worker
    // and the apex are kept as they are, never uploaded or attached.
    let owned = platform_owned(&home, &rules).unwrap();
    assert_eq!(owned.apex, "pigeon.dev");
    let seen = Observed {
        account_id: "h".repeat(32),
        bucket_exists: false,
        worker_exists: true,
        workers_dev: None,
        account_subdomain: None,
        domains: vec![],
        subdomains: vec![],
    };
    let l = Ledger::default();
    let steps = plan(&home, &seen, &l, Some(&owned));
    let keep = |what: &str, name: &str| Step::Keep {
        what: what.into(),
        name: name.into(),
        why: PLATFORM_OWNED.into(),
    };
    assert_eq!(
        steps,
        vec![
            Step::CreateBucket {
                name: "pigeon-platform-site".into(),
                jurisdiction: None,
                protected: false,
            },
            keep("worker", "pigeon-platform"),
            keep("custom domain", "pigeon.dev"),
        ]
    );
    assert_eq!(home_refusal(&steps, Some(&owned)), None);
    assert!(
        home_refusal(&steps, None).is_some(),
        "fails closed without the set"
    );
    // Even when the account says the apex is served (by either platform Worker), it is kept.
    for service in ["pigeon-platform", "pigeon-sites"] {
        let served = Observed {
            domains: vec![SeenDomain {
                hostname: "pigeon.dev".into(),
                zone_id: Some("z".into()),
                service: Some(service.into()),
            }],
            ..seen.clone()
        };
        assert_eq!(plan(&home, &served, &l, Some(&owned)), steps, "{service}");
    }
    // Nothing platform-owned is ever a change, or in the ledger.
    let mut after = l.clone();
    for step in &steps {
        record(&mut after, step);
    }
    assert_eq!(after.buckets, vec!["pigeon-platform-site".to_string()]);
    assert!(after.workers.is_empty() && after.domains.is_empty() && after.subdomains.is_empty());
    assert!(after.protected_workers.is_empty() && after.protected_buckets.is_empty());
    let applied = plan(
        &home,
        &Observed {
            bucket_exists: true,
            ..seen.clone()
        },
        &after,
        Some(&owned),
    );
    assert!(!applied.iter().any(Step::changes), "{applied:?}");
    assert!(!applied.iter().any(Step::stops), "{applied:?}");
    // The approval binds the same subject and digest as a linked plan's: the instance, its
    // version, the platform's own account and the exact steps.
    let d = digest(&home, &seen.account_id, &steps);
    assert_eq!(d, digest(&home, &seen.account_id, &steps));
    assert_ne!(d, digest(&home, &"i".repeat(32), &steps));
    assert_ne!(d, digest(&home, &seen.account_id, &applied));

    // A reserved Worker or host blocks the plan, and apply refuses it.
    let blocked = |i: &Instance| {
        let steps = plan(i, &seen, &l, Some(&owned));
        assert!(steps.iter().any(Step::stops), "{steps:?}");
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, Step::UploadWorker { .. } | Step::AttachDomain { .. })),
            "{steps:?}"
        );
        let why = home_refusal(&steps, Some(&owned)).unwrap();
        assert!(why.contains("in the way"), "{why}");
        steps
    };
    let steps = blocked(&reserved_host);
    assert!(steps.contains(&Step::Blocked {
        why: format!("platform.pigeon.dev is {PLATFORM_OWNED}")
    }));
    let mut blog = home.clone();
    blog.sites[0].targets.push(Target::Domain {
        hostname: "blog.pigeon.dev".into(),
    });
    let steps = blocked(&blog);
    assert!(steps.contains(&Step::Blocked {
        why: format!("blog.pigeon.dev's Worker pigeon-platform is {PLATFORM_OWNED}")
    }));
    let mut sub = home.clone();
    sub.sites[0].targets.push(Target::Subdomain {
        label: "landing".into(),
    });
    let with_sub = Observed {
        subdomains: vec![pigeon_capabilities::instances::plan::SeenSubdomain {
            hostname: "landing.pigeon.dev".into(),
            zone_id: None,
            worker: "pigeon-sites".into(),
            service: None,
            upstream: None,
        }],
        ..seen.clone()
    };
    let steps = plan(&sub, &with_sub, &l, Some(&owned));
    assert!(home_refusal(&steps, Some(&owned)).is_some(), "{steps:?}");
    // A home Worker that is not the platform's is never uploaded: the token holds no
    // Workers-scripts write.
    let mut own = home.clone();
    own.worker = Some("landing".into());
    own.worker_override = true;
    let steps = plan(
        &own,
        &Observed {
            worker_exists: false,
            ..seen.clone()
        },
        &l,
        Some(&owned),
    );
    assert!(steps.contains(&Step::Blocked {
        why: format!("Worker landing: {NO_SCRIPTS}")
    }));
    // The second line: a change naming anything platform-owned, or a Worker, is refused even
    // if a plan somehow held it.
    let attach = |hostname: &str, worker: &str| Step::AttachDomain {
        hostname: hostname.into(),
        zone_id: "z".into(),
        worker: worker.into(),
    };
    for bad in [
        attach("pigeon.dev", "landing"),
        attach("blog.pigeon.dev", "pigeon-platform"),
        Step::UploadWorker {
            name: "pigeon-platform".into(),
            bucket: Some("b".into()),
            jurisdiction: None,
            module: "m".into(),
            replaces: true,
            origin: None,
            redirects: None,
        },
        Step::EnableWorkersDev {
            name: "landing".into(),
        },
    ] {
        assert!(
            home_refusal(std::slice::from_ref(&bad), Some(&owned)).is_some(),
            "{bad:?}"
        );
    }
    assert_eq!(
        home_refusal(&[attach("blog.pigeon.dev", "landing")], Some(&owned)),
        None
    );

    // The credential: exactly R2 write and the zone's custom domains, never Workers scripts,
    // and core alone mints it.
    let permissions = purpose_permissions(PURPOSE).unwrap();
    assert_eq!(permissions, &["Workers R2 Storage Write", "Zone Read",][..]);
    assert!(!permissions.iter().any(|p| p.starts_with("Workers Scripts")));
    assert!(CORE_ONLY_PURPOSES.contains(&PURPOSE));
    let exact = vec![
        (
            "account".to_string(),
            vec!["Workers R2 Storage Write".to_string()],
        ),
        (
            "zones:zone-1".to_string(),
            permissions[1..].iter().map(|p| p.to_string()).collect(),
        ),
    ];
    let missing = home::policies(None, "zone-1").err().unwrap();
    assert!(missing.contains("not a credential purpose") && missing.contains(PERSON_STEPS));
    assert!(
        home::policies(Some(&["Workers Scripts Write"]), "zone-1")
            .err()
            .unwrap()
            .contains("never holds")
    );
    assert!(home::policies(Some(permissions), "").is_err());

    // Minted for the work, used as the account's bearer, and revoked after it succeeds...
    let tokens = MockTokens::new(10, 50);
    let minter = HomeMinter {
        tokens: &tokens,
        minted: Default::default(),
    };
    let ran = token_block_on(with_token(
        &minter,
        Some(permissions),
        "zone-1",
        &"h".repeat(32),
        "store",
        |account| async move {
            assert_eq!(account.bearer.as_deref(), Some("v"));
            assert!(account.parent.is_empty(), "nothing mints from the parent");
            assert_eq!(account.id, "h".repeat(32));
            Ok(json!({ "done": ["created bucket pigeon-platform-site"] }))
        },
    ))
    .unwrap();
    assert!(
        !ran.to_string().contains("\"v\""),
        "the token is never returned"
    );
    assert_eq!(
        *minter.minted.borrow(),
        vec![("instance-home".to_string(), exact.clone())]
    );
    assert_eq!(*tokens.deleted.borrow(), vec!["t100".to_string()]);
    assert!(tokens.tokens.borrow().is_empty());

    // ... and after it fails, the work's own error first.
    let failed = token_block_on(with_token(
        &minter,
        Some(permissions),
        "zone-1",
        &"h".repeat(32),
        "store",
        |_account| async move { Err::<(), _>(worker::Error::RustError("bucket refused".into())) },
    ))
    .unwrap_err()
    .to_string();
    assert!(failed.contains("bucket refused"), "{failed}");
    assert_eq!(minter.minted.borrow().len(), 2);
    assert_eq!(minter.minted.borrow()[1].1, exact);
    assert_eq!(
        *tokens.deleted.borrow(),
        vec!["t100".to_string(), "t101".to_string()]
    );
    assert!(tokens.tokens.borrow().is_empty());

    // A token that could not be revoked is said so, after the work's own result.
    let mut stuck = MockTokens::new(10, 50);
    stuck.fail_delete = vec!["t100"];
    let stuck_minter = HomeMinter {
        tokens: &stuck,
        minted: Default::default(),
    };
    let unrevoked = token_block_on(with_token(
        &stuck_minter,
        Some(permissions),
        "zone-1",
        "h",
        "store",
        |_account| async move { Ok(()) },
    ))
    .unwrap_err()
    .to_string();
    assert!(unrevoked.contains("not revoked"), "{unrevoked}");

    // Refused before any work when the purpose is missing (nothing minted) or the parent token
    // cannot mint it; the message names what the person does.
    let worked = std::cell::Cell::new(false);
    let w = &worked;
    let none = MockTokens::new(10, 50);
    let none_minter = HomeMinter {
        tokens: &none,
        minted: Default::default(),
    };
    let refused = token_block_on(with_token(
        &none_minter,
        None,
        "zone-1",
        "h",
        "store",
        move |_account| async move {
            w.set(true);
            Ok(())
        },
    ))
    .unwrap_err()
    .to_string();
    assert!(refused.contains(PERSON_STEPS), "{refused}");
    assert_eq!(none.attempts.get(), 0);
    let full = MockTokens::new(0, 50);
    let full_minter = HomeMinter {
        tokens: &full,
        minted: Default::default(),
    };
    let refused = token_block_on(with_token(
        &full_minter,
        Some(permissions),
        "zone-1",
        "h",
        "store",
        move |_account| async move {
            w.set(true);
            Ok(())
        },
    ))
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("parent token could not mint") && refused.contains(PERSON_STEPS),
        "{refused}"
    );
    assert!(!worked.get(), "nothing is half-applied");
    assert!(full.deleted.borrow().is_empty());

    // Linked instances plan exactly as before.
    let linked_seen = Observed {
        account_id: "a".repeat(32),
        worker_exists: false,
        workers_dev: Some(false),
        account_subdomain: Some("np".into()),
        domains: vec![SeenDomain {
            hostname: "noisypigeon.com".into(),
            zone_id: Some("z".into()),
            service: None,
        }],
        ..seen.clone()
    };
    let linked_steps = plan(&linked, &linked_seen, &l, None);
    assert_eq!(linked_steps, plan(&linked, &linked_seen, &l, Some(&owned)));
    assert!(platform_owned(&linked, &rules).is_none());
    assert!(linked_steps.iter().any(
        |s| matches!(s, Step::AttachDomain { hostname, .. } if hostname == "noisypigeon.com")
    ));
    assert!(
        linked_steps
            .iter()
            .any(|s| matches!(s, Step::EnableWorkersDev { .. }))
    );
}

/// A site bucket in the suite: every object by (bucket, key), and the bearer each write and
/// read used. A put or the blobs' write can be made to fail.
#[derive(Default)]
struct BucketState {
    objects: std::collections::BTreeMap<(String, String), Vec<u8>>,
    bearers: Vec<Option<String>>,
    fail_blobs: bool,
}

/// One bucket of [`BucketState`], opened with an account's credential.
struct MockBucket<'a> {
    bucket: String,
    bearer: Option<String>,
    state: &'a std::cell::RefCell<BucketState>,
}

impl pigeon_core::site_publish::Store for MockBucket<'_> {
    async fn list(&self, prefix: &str) -> worker::Result<Vec<String>> {
        let mut s = self.state.borrow_mut();
        s.bearers.push(self.bearer.clone());
        Ok(s.objects
            .keys()
            .filter(|(b, k)| *b == self.bucket && k.starts_with(prefix))
            .map(|(_, k)| k.clone())
            .collect())
    }
    async fn get(&self, key: &str) -> worker::Result<Option<Vec<u8>>> {
        let mut s = self.state.borrow_mut();
        s.bearers.push(self.bearer.clone());
        Ok(s.objects
            .get(&(self.bucket.clone(), key.to_string()))
            .cloned())
    }
    async fn put(&self, key: &str, _ctype: &str, body: Vec<u8>) -> worker::Result<()> {
        let mut s = self.state.borrow_mut();
        s.bearers.push(self.bearer.clone());
        s.objects
            .insert((self.bucket.clone(), key.to_string()), body);
        Ok(())
    }
    async fn put_objects(&self, objects: Vec<pigeon_core::r2::ObjectWrite>) -> worker::Result<()> {
        if self.state.borrow().fail_blobs {
            return Err(worker::Error::RustError("blob write refused".into()));
        }
        for o in objects {
            self.put(&o.key, &o.content_type, o.body).await?;
        }
        Ok(())
    }
}

/// A repository whose `site/` holds the home landing's content (the repository's own `site/`),
/// a static file and a page, beside files outside it: its directory and the commit.
fn home_site_repo(name: &str, extra: &str) -> (PathBuf, String) {
    let dir = repo(name);
    let put = |path: &str, body: &str| {
        let p = dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    put(
        "site/.pigeon/site.toml",
        include_str!("../../site/.pigeon/site.toml"),
    );
    put(
        "site/content/index.toml",
        include_str!("../../site/content/index.toml"),
    );
    put(
        "site/content/pages/about.md",
        &format!("+++\ntitle = \"About\"\n+++\n\nAbout pigeon. {extra}\n"),
    );
    put("site/static/robots.txt", "User-agent: *\n");
    put("src/lib.rs", "// not part of the site\n");
    put("README.md", "outside the content root\n");
    git(&dir, &["add", "-A"], None);
    git(&dir, &["commit", "-q", "-m", "site"], None);
    let head = String::from_utf8(git(&dir, &["rev-parse", "HEAD"], None))
        .unwrap()
        .trim()
        .to_string();
    (dir, head)
}

/// What Artifacts answers a one-commit-deep fetch of `commit`: the commit and its whole tree.
fn commit_pack(dir: &Path, commit: &str) -> pigeon_core::pack::Objects {
    let pack = git(
        dir,
        &["pack-objects", "--stdout", "--revs", "-q"],
        Some(&format!("{commit}\n")),
    );
    pigeon_core::pack::objects(&pack).unwrap()
}

/// Area: instances. L2–L3: the home instance publishes its landing site from its Artifacts
/// repository's content root, with no GitHub App and no linked account, as immutable releases in
/// its own bucket, written with a minted `instance_home` token revoked whatever happens; the
/// release, rollback and listing code is the one linked instances use, unchanged for them.
#[test]
fn home_site_publishes_from_artifacts_with_a_minted_token() {
    use super::core::{MockTokens, token_block_on};
    use pigeon_capabilities::instances::home::PURPOSE;
    use pigeon_capabilities::instances::home_site::{
        BUILD_TOOL, Build, LIMITS, Token, build_request, built_from, provenance, release, releases,
        rollback, routed, target,
    };
    use pigeon_capabilities::instances::record::{Instance, Rules, reserved_labels, validate};
    use pigeon_capabilities::split::purpose_permissions;
    use pigeon_core::pack::{TreeLimits, read_tree};
    use pigeon_core::site_publish::{self, Owner};

    // The record the person approves to publish: the content root and the publishing profile.
    let home_json = json!({
        "id": "pigeon-security/platform", "title": "pigeon",
        "account": { "kind": "home" }, "zones": ["pigeon.dev"],
        "repo": { "kind": "artifacts", "namespace": "pigeon-security", "repo": "pigeon" },
        "worker": "pigeon-platform",
        "platform": { "host": "platform.pigeon.dev" },
        "sites": [{ "name": "landing", "bucket": "pigeon-platform-site", "template": "landing@1",
            "build": "site", "targets": [{ "kind": "domain", "hostname": "pigeon.dev" }] }],
        "publishing": { "state": "enabled", "site": "landing" },
        "version": 4
    });
    let home: Instance = serde_json::from_value(home_json).unwrap();
    let reserved = reserved_labels(
        &["platform.pigeon.dev".into(), "proxy.pigeon.dev".into()],
        "pigeon.dev",
    );
    let workers: Vec<String> = ["pigeon-platform", "pigeon-sites"]
        .map(String::from)
        .to_vec();
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
        platform_worker: "pigeon-platform",
    };
    assert_eq!(validate(&home, &[], &rules), Vec::<String>::new());
    // Home publishes from Artifacts and only the landing template; a linked instance still
    // needs GitHub and a linked account, word for word.
    let mut on_github = home.clone();
    on_github.repo = Some(pigeon_capabilities::instances::record::RepoRef::Github {
        full_name: "pigeon-security/pigeon".into(),
        installation: None,
    });
    assert!(
        validate(&on_github, &[], &rules)
            .iter()
            .any(|p| p.contains("publishes from its Artifacts repository")),
    );
    let mut other_site = home.clone();
    other_site.sites.push(other_site.sites[0].clone());
    other_site.sites[1].name = "notes".into();
    other_site.sites[1].template = None;
    other_site.sites[1].targets = vec![];
    other_site.publishing = Some(site_publish::Profile::Enabled {
        site: "notes".into(),
    });
    let problems = validate(&other_site, &[], &rules);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("publishes only the landing@1 template, and site notes")),
        "{problems:?}"
    );
    let mut unknown = home.clone();
    unknown.publishing = Some(site_publish::Profile::Enabled {
        site: "nope".into(),
    });
    assert!(
        validate(&unknown, &[], &rules)
            .iter()
            .any(|p| p == "Publishing names an unknown site: nope.")
    );
    let linked: Instance = serde_json::from_value(json!({
        "id": "noisypigeon", "title": "noisypigeon",
        "account": { "kind": "linked", "account_id": "a".repeat(32) },
        "zones": ["noisypigeon.com"], "worker": "noisypigeon",
        "repo": { "kind": "artifacts", "namespace": "np", "repo": "site" },
        "sites": [{ "name": "blog", "bucket": "noisypigeon-site",
            "targets": [{ "kind": "domain", "hostname": "noisypigeon.com" }] }],
        "publishing": { "state": "enabled", "site": "blog" },
        "version": 1
    }))
    .unwrap();
    assert!(
        validate(&linked, &[], &rules)
            .contains(&"Publishing needs a GitHub repository and a linked account.".to_string())
    );
    let mut linked_github = linked.clone();
    linked_github.repo = on_github.repo.clone();
    assert_eq!(validate(&linked_github, &[], &rules), Vec::<String>::new());

    // pigeon-sites never acts for home: its callback still refuses the record, so only core,
    // which holds the token, publishes it. Linked publishing tools still go to pigeon-sites.
    let refused = pigeon_capabilities::sites_scope::approved_record(
        &home,
        &pigeon_contract::SitesScopeRequest::Record {
            instance: home.id.clone(),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("GitHub repository and a linked account"),
        "{refused}"
    );
    for tool in ["site_deploy", "site_releases", "site_rollback"] {
        assert!(routed("sites", tool, true), "{tool}");
        assert!(!routed("sites", tool, false), "{tool}");
    }
    assert!(!routed("sites", "site_render", true));
    assert!(!routed("ci", "site_deploy", true));
    // The tools and the Deployments view are on for home, bound to it as for a linked record.
    let scope = pigeon_capabilities::instances::tools::Scope::new(&home).unwrap();
    assert!(scope.home());
    assert!(scope.views().iter().any(|v| v.view.id == "deployments"));
    let bound = pigeon_capabilities::instances::tools::bind_global(
        &home.id,
        Some(&home),
        "site_deploy",
        &json!({ "instance": home.id, "rev": "main" }),
    )
    .unwrap();
    assert_eq!(bound["instance"], "pigeon-security/platform");
    assert!(
        !pigeon_capabilities::instances::tools::Scope::new(&linked_github)
            .unwrap()
            .home()
    );

    // L2: the content root at the resolved commit, and nothing outside it.
    let t = target(&home, "pigeon.dev").unwrap();
    assert_eq!(t.bucket.name, "pigeon-platform-site");
    assert_eq!(t.base_url, "https://pigeon.dev");
    assert_eq!(
        (t.namespace.as_str(), t.repo.as_str()),
        ("pigeon-security", "pigeon")
    );
    assert_eq!(t.root.as_deref(), Some("site"));
    for (bad, why) in [
        (&linked_github, "not the home instance"),
        (&on_github, "publishes from Artifacts"),
    ] {
        assert!(
            target(bad, "pigeon.dev").unwrap_err().contains(why),
            "{why}"
        );
    }
    let mut off = home.clone();
    off.publishing = None;
    assert!(target(&off, "pigeon.dev").is_err());
    let (dir, commit) = home_site_repo("home-site", "One.");
    let objects = commit_pack(&dir, &commit);
    let files = read_tree(&objects, &commit, t.root.as_deref(), &LIMITS).unwrap();
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            ".pigeon/site.toml",
            "content/index.toml",
            "content/pages/about.md",
            "static/robots.txt"
        ]
    );
    assert_eq!(
        (LIMITS.max_files, LIMITS.max_bytes, LIMITS.max_depth),
        (256, 8 * 1024 * 1024, 8)
    );
    // The whole tree when there is no root; refused roots and trees past the limits.
    let all = read_tree(&objects, &commit, None, &LIMITS).unwrap();
    assert!(all.iter().any(|(p, _)| p == "src/lib.rs"));
    for root in [
        "../src",
        "site/../src",
        "/site",
        "site/",
        "",
        ".",
        "si\\te",
        "README.md",
        "nope",
    ] {
        assert!(
            read_tree(&objects, &commit, Some(root), &LIMITS).is_err(),
            "{root:?}"
        );
    }
    let tight = |f: &dyn Fn(&mut TreeLimits)| {
        let mut l = LIMITS;
        f(&mut l);
        read_tree(&objects, &commit, Some("site"), &l).unwrap_err()
    };
    assert!(tight(&|l| l.max_files = 3).contains("more than 3 files"));
    assert!(tight(&|l| l.max_bytes = 1024).contains("more than 1024 bytes"));
    assert!(tight(&|l| l.max_depth = 1).contains("directories deep"));
    assert!(tight(&|l| l.max_path = 10).contains("longer than 10 bytes"));
    // A tree naming `..` (written past git's own checks), a symbolic link and a submodule are
    // refused, never followed.
    let blob = String::from_utf8(git(&dir, &["hash-object", "-w", "--stdin"], Some("x")))
        .unwrap()
        .trim()
        .to_string();
    let raw = |name: &str, mode: &str, id: &str| {
        let mut tree = format!("{mode} {name}\0").into_bytes();
        for i in 0..20 {
            tree.push(u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).unwrap());
        }
        tree
    };
    let evil = |tree: Vec<u8>| {
        let file = dir.join("tree.bin");
        std::fs::write(&file, tree).unwrap();
        let sub = String::from_utf8(git(
            &dir,
            &[
                "hash-object",
                "-t",
                "tree",
                "--literally",
                "-w",
                file.to_str().unwrap(),
            ],
            None,
        ))
        .unwrap()
        .trim()
        .to_string();
        let top = raw("site", "40000", &sub);
        std::fs::write(&file, top).unwrap();
        let top = String::from_utf8(git(
            &dir,
            &[
                "hash-object",
                "-t",
                "tree",
                "--literally",
                "-w",
                file.to_str().unwrap(),
            ],
            None,
        ))
        .unwrap()
        .trim()
        .to_string();
        let c = String::from_utf8(git(&dir, &["commit-tree", &top, "-m", "evil"], None))
            .unwrap()
            .trim()
            .to_string();
        let objects = commit_pack(&dir, &c);
        read_tree(&objects, &c, Some("site"), &LIMITS).unwrap_err()
    };
    assert!(evil(raw("..", "100644", &blob)).contains("bad name"));
    assert!(evil(raw("a\\b", "100644", &blob)).contains("backslash"));
    assert!(evil(raw("link", "120000", &blob)).contains("symbolic link"));
    assert!(evil(raw("sub", "160000", &commit)).contains("submodule"));

    // The landing@1 build, rendered by pigeon-sites (which carries the templates) in a private
    // call holding no credential: the template's digest, no scripts, statics as they are.
    assert_eq!(BUILD_TOOL, super::sites_templates::BUILD_TOOL);
    let build = |files: Vec<(String, Vec<u8>)>, commit: &str, pin: Option<&str>| {
        let request = build_request(&home.id, &files, commit, pin);
        assert_eq!(request["instance"], json!(home.id));
        built_from(&super::sites_templates::site_build(&request)?)
    };
    let Build::Ready(built) = build(files.clone(), &commit, None).unwrap() else {
        panic!("the home content builds")
    };
    assert_eq!(built.template, "landing@1");
    assert_eq!(built.version, pigeon_landing::VERSION);
    assert_eq!(
        built.template_digest,
        pigeon_landing::template_digest(&pigeon_landing::TEMPLATES[0])
    );
    assert_eq!(built.renderer_digest, pigeon_landing::renderer_digest());
    let html: Vec<&(String, Vec<u8>, String)> = built
        .files
        .iter()
        .filter(|(_, _, ctype)| ctype.starts_with("text/"))
        .collect();
    assert!(html.iter().any(|(p, _, _)| p == "index.html"));
    assert!(html.iter().any(|(p, _, _)| p == "about/index.html"));
    for (path, bytes, _) in &html {
        let text = String::from_utf8_lossy(bytes).to_lowercase();
        assert!(!text.contains("<script"), "{path}");
    }
    assert!(built.files.iter().any(|(p, b, c)| p == "robots.txt"
        && b == b"User-agent: *\n"
        && c.starts_with("text/plain")));
    // A pin that allows nothing carried, a blog site.toml and a missing site.toml are the
    // content's problems, never a release; a bad pin is refused.
    let problems = |b: Build| match b {
        Build::Problems(p) => p,
        Build::Ready(_) => panic!("built"),
    };
    assert!(
        problems(build(files.clone(), &commit, Some("=0.9.0")).unwrap())
            .iter()
            .any(|p| p.contains("pin allows none"))
    );
    let mut blog = files.clone();
    blog[0].1 = b"template = \"blog@1\"\n".to_vec();
    assert!(problems(build(blog, &commit, None).unwrap())[0].contains("only landing@1"));
    assert!(
        problems(build(files[1..].to_vec(), &commit, None).unwrap())[0]
            .contains(".pigeon/site.toml")
    );
    assert!(build(files.clone(), &commit, Some("not a pin")).is_err());
    assert!(build(files.clone(), "not a commit", None).is_err());
    // Core takes nothing from pigeon-sites but a landing@1 release of site paths.
    let answer =
        super::sites_templates::site_build(&build_request(&home.id, &files, &commit, None))
            .unwrap();
    for (field, value, why) in [
        ("template", json!("landing@2"), "Problems"),
        ("files", json!([["../x", "", "text/plain"]]), "bad path"),
        ("files", json!([["/x", "", "text/plain"]]), "bad path"),
        ("files", json!([["a\\b", "", "text/plain"]]), "bad path"),
        ("files", json!([["x"]]), "malformed"),
        ("built", json!("yes"), "did not answer"),
    ] {
        let mut a = answer.clone();
        a[field] = value;
        let got = format!("{:?}", built_from(&a));
        assert!(got.contains(why), "{field}: {got}");
    }

    // L3: staged and made live in the home bucket alone, every read and write with the minted
    // `instance_home` token, revoked after.
    let permissions = purpose_permissions(PURPOSE).unwrap();
    let tokens = MockTokens::new(10, 50);
    let minter = HomeMinter {
        tokens: &tokens,
        minted: Default::default(),
    };
    let token = Token {
        minter: &minter,
        permissions: Some(permissions),
        zone_id: "zone-1",
        account_id: "h",
        secrets_store: "store",
    };
    let state = std::cell::RefCell::new(BucketState::default());
    let open = |account: pigeon_core::Account| {
        assert!(account.parent.is_empty(), "nothing mints from the parent");
        MockBucket {
            bucket: t.bucket.name.clone(),
            bearer: account.bearer,
            state: &state,
        }
    };
    let current = || async { Ok(()) };
    let first = provenance(&home.id, &t, &commit, &built);
    assert_eq!(first["template"], "landing@1");
    assert_eq!(first["template_digest"], json!(built.template_digest));
    let staged =
        token_block_on(release(&token, open, &first, built.files.clone(), current)).unwrap();
    {
        let s = state.borrow();
        assert!(s.objects.keys().all(|(b, _)| b == "pigeon-platform-site"));
        assert!(
            s.bearers.iter().all(|b| b.as_deref() == Some("v")),
            "{:?}",
            s.bearers
        );
        let key = |k: &str| ("pigeon-platform-site".to_string(), k.to_string());
        assert_eq!(s.objects[&key("live")], staged.release.as_bytes());
        let meta: Value =
            serde_json::from_slice(&s.objects[&key(&site_publish::release_key(&staged.release))])
                .unwrap();
        assert_eq!(meta, first);
        assert!(
            s.objects
                .contains_key(&key(&pigeon_core::r2::manifest_key(&staged.release)))
        );
    }
    assert_eq!(staged.files, built.files.len());
    assert_eq!(staged.uploaded, built.files.len());
    let exact = vec![
        (
            "account".to_string(),
            vec!["Workers R2 Storage Write".to_string()],
        ),
        ("zones:zone-1".to_string(), vec!["Zone Read".to_string()]),
    ];
    assert_eq!(
        *minter.minted.borrow(),
        vec![("instance-home".to_string(), exact)]
    );
    assert_eq!(*tokens.deleted.borrow(), vec!["t100".to_string()]);
    // The same deploy again is the same release, and uploads nothing new.
    let again =
        token_block_on(release(&token, open, &first, built.files.clone(), current)).unwrap();
    assert_eq!(
        (again.release.as_str(), again.uploaded),
        (staged.release.as_str(), 0)
    );
    // The release id is what linked releases have always been: the provenance and manifest.
    let manifest: pigeon_core::r2::Manifest = built
        .files
        .iter()
        .map(|(p, b, c)| {
            let (path, mut e) = pigeon_core::r2::manifest_of(&[(p.clone(), b.clone())])
                .into_iter()
                .next()
                .unwrap();
            e.content_type = c.clone();
            (path, e)
        })
        .collect();
    assert_eq!(staged.release, site_publish::release_id(&first, &manifest));

    // A failed write, or a record that changed before `live` moves, leaves `live` where it was,
    // and the token is revoked all the same.
    let (dir2, commit2) = home_site_repo("home-site-2", "Two.");
    let files2 = read_tree(
        &commit_pack(&dir2, &commit2),
        &commit2,
        Some("site"),
        &LIMITS,
    )
    .unwrap();
    let Build::Ready(built2) = build(files2, &commit2, Some("^1")).unwrap() else {
        panic!("builds")
    };
    let second = provenance(&home.id, &t, &commit2, &built2);
    state.borrow_mut().fail_blobs = true;
    let failed = token_block_on(release(
        &token,
        open,
        &second,
        built2.files.clone(),
        current,
    ))
    .unwrap_err()
    .to_string();
    assert!(failed.contains("blob write refused"), "{failed}");
    state.borrow_mut().fail_blobs = false;
    let changed = || async {
        Err::<(), _>(worker::Error::RustError(
            "the approved publishing record changed".into(),
        ))
    };
    let failed = token_block_on(release(
        &token,
        open,
        &second,
        built2.files.clone(),
        changed,
    ))
    .unwrap_err()
    .to_string();
    assert!(failed.contains("record changed"), "{failed}");
    let live = |s: &std::cell::RefCell<BucketState>| {
        String::from_utf8(
            s.borrow().objects[&("pigeon-platform-site".to_string(), "live".to_string())].clone(),
        )
        .unwrap()
    };
    assert_eq!(live(&state), staged.release);
    assert_eq!(
        *tokens.deleted.borrow(),
        ["t100", "t101", "t102", "t103"].map(String::from)
    );
    assert!(tokens.tokens.borrow().is_empty());

    // A second release goes live; releases list live first; rollback moves `live` back.
    let newer = token_block_on(release(
        &token,
        open,
        &second,
        built2.files.clone(),
        current,
    ))
    .unwrap();
    assert_ne!(newer.release, staged.release);
    assert_eq!(live(&state), newer.release);
    let listed = token_block_on(releases(&token, open, &home.id, &t.site, 20)).unwrap();
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["live"], json!(newer.release));
    assert_eq!(listed["releases"][0]["release"], json!(newer.release));
    assert_eq!(listed["releases"][0]["live"], true);
    assert_eq!(listed["releases"][1]["commit"], json!(commit));
    assert_eq!(listed["releases"][1]["template"], "landing@1");
    let owner = Owner {
        instance: &home.id,
        base_url: &t.base_url,
        site: &t.site,
    };
    let back = token_block_on(rollback(&token, open, &staged.release, &owner, current)).unwrap();
    assert_eq!(back["commit"], json!(commit));
    assert_eq!(live(&state), staged.release);
    // A release of another target, an unknown one or no release id is refused; `live` stays.
    let elsewhere = Owner {
        instance: "noisypigeon",
        ..owner
    };
    for (release_id, who, why) in [
        (
            newer.release.as_str(),
            &elsewhere,
            "different publishing target",
        ),
        (
            "release-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            &owner,
            "unknown site release",
        ),
        ("main", &owner, "expected a site release ID"),
    ] {
        let why_not = token_block_on(rollback(&token, open, release_id, who, current))
            .unwrap_err()
            .to_string();
        assert!(why_not.contains(why), "{why_not}");
    }
    assert_eq!(live(&state), staged.release);
    // Every mint was revoked; nothing outside the home bucket was ever written.
    assert!(tokens.tokens.borrow().is_empty());
    assert!(
        minter
            .minted
            .borrow()
            .iter()
            .all(|(p, _)| p == "instance-home")
    );
    assert!(
        state
            .borrow()
            .objects
            .keys()
            .all(|(b, _)| b == "pigeon-platform-site")
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

/// One approval in the R1 double: id, subject, state, whether it is a rename plan, its note.
type DoubleApproval = (String, String, String, bool, Option<String>);

/// The R1 migration's double: the Directory's settings and approvals and the CI scheduler's
/// jobs, in memory, with a count of reads and a write that can be made to fail.
#[derive(Default)]
struct RenameDouble {
    settings: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
    approvals: std::cell::RefCell<Vec<DoubleApproval>>,
    /// Each job's instance.
    jobs: std::cell::RefCell<Vec<Option<String>>>,
    reads: std::cell::Cell<u32>,
    fail_jobs: std::cell::Cell<bool>,
}

impl RenameDouble {
    fn approval(&self, id: &str, subject: &str, state: &str, rename: bool) {
        self.approvals
            .borrow_mut()
            .push((id.into(), subject.into(), state.into(), rename, None));
    }
    fn state_of(&self, id: &str) -> (String, Option<String>) {
        let a = self.approvals.borrow();
        let r = a.iter().find(|r| r.0 == id).unwrap();
        (r.2.clone(), r.4.clone())
    }
    fn jobs_of(&self, id: &str) -> usize {
        self.jobs
            .borrow()
            .iter()
            .filter(|j| j.as_deref() == Some(id))
            .count()
    }
}

impl pigeon_capabilities::instances::rename::Store for RenameDouble {
    async fn get(&self, key: &str) -> worker::Result<Option<String>> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.settings.borrow().get(key).cloned())
    }
    async fn put(&self, key: &str, value: &str) -> worker::Result<()> {
        self.settings.borrow_mut().insert(key.into(), value.into());
        Ok(())
    }
    async fn forget(&self, key: &str) -> worker::Result<()> {
        self.settings.borrow_mut().remove(key);
        Ok(())
    }
    async fn open_approvals(&self, subject: &str) -> worker::Result<Vec<String>> {
        Ok(self
            .approvals
            .borrow()
            .iter()
            .filter(|a| a.1 == subject && !a.3 && (a.2 == "pending" || a.2 == "approved"))
            .map(|a| a.0.clone())
            .collect())
    }
    async fn expire_approvals(&self, ids: &[String], note: &str) -> worker::Result<()> {
        for a in self.approvals.borrow_mut().iter_mut() {
            if ids.contains(&a.0) && (a.2 == "pending" || a.2 == "approved") {
                a.2 = "expired".into();
                a.4 = Some(note.into());
            }
        }
        Ok(())
    }
    async fn jobs(&self, from: &str, to: &str, apply: bool) -> worker::Result<u64> {
        let n = self.jobs_of(from) as u64;
        if apply {
            if self.fail_jobs.get() {
                return Err(worker::Error::RustError("the CI scheduler is away".into()));
            }
            for j in self.jobs.borrow_mut().iter_mut() {
                if j.as_deref() == Some(from) {
                    *j = Some(to.into());
                }
            }
        }
        Ok(n)
    }
}

/// R1 (docs/plans/platform-self-hosting.md): the home instance's id becomes `pigeon.dev`. Before
/// the migration nothing reads differently; a dotted id is allowed only for home and only as
/// one of its zones; the dry run lists exactly what the run then does; the run moves every key,
/// re-attributes CI jobs, expires open approvals naming the old id and leaves a tombstone that
/// answers the old id (records, CI reads, MCP arguments, console URLs) for the new one; a second
/// run does nothing; it refuses a conflicting `pigeon.dev`; a run that stopped resumes.
#[test]
fn home_id_rename_r1() {
    use super::core::token_block_on as block;
    use pigeon_capabilities::ci::{ScopeRequest, resolve_renamed, resolve_scope};
    use pigeon_capabilities::console::Get;
    use pigeon_capabilities::console::page;
    use pigeon_capabilities::instances::record::{AccountRef, Instance, Rules, reserved_labels};
    use pigeon_capabilities::instances::rename::{
        self, COMPAT_MS, FROM, Step, Stored, TO, Tombstone, current_args, describe, observe, plan,
        record, resolve, run,
    };
    use pigeon_capabilities::instances::{INDEX, key, record::validate};

    assert_eq!((FROM, TO), ("pigeon-security/platform", "pigeon.dev"));
    assert_eq!(rename::alias_of(FROM), Some(TO));
    assert_eq!(rename::renamed_from(TO), Some(FROM));
    assert_eq!(rename::alias_of("noisypigeon"), None);

    // The home record as it is: version 4, its zone, the landing site and publishing.
    let home_json = json!({
        "id": FROM, "title": "pigeon",
        "account": { "kind": "home" }, "zones": ["pigeon.dev"],
        "repo": { "kind": "artifacts", "namespace": "pigeon-security", "repo": "pigeon" },
        "worker": "pigeon-platform",
        "platform": { "host": "platform.pigeon.dev" },
        "sites": [{ "name": "landing", "bucket": "pigeon-platform-site", "template": "landing@1",
            "build": "site",
            "targets": [{ "kind": "domain", "hostname": "pigeon.dev" }] }],
        "publishing": { "state": "enabled", "site": "landing" },
        "ci": { "state": "enabled", "jobs": ["test"] },
        "version": 4
    });
    let home: Instance = serde_json::from_value(home_json).unwrap();
    let linked: Instance = serde_json::from_value(json!({
        "id": "noisypigeon", "title": "noisypigeon",
        "account": { "kind": "linked", "account_id": "a".repeat(32) },
        "zones": ["noisypigeon.com"], "version": 1
    }))
    .unwrap();

    // ---- The dotted id: home only, and only one of its zones.
    let reserved = reserved_labels(
        &["platform.pigeon.dev".into(), "pigeon.dev".into()],
        "pigeon.dev",
    );
    let workers: Vec<String> = ["pigeon-platform", "pigeon-sites"]
        .map(String::from)
        .to_vec();
    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &reserved,
        reserved_workers: &workers,
        platform_worker: "pigeon-platform",
    };
    let id_problems = |r: &Instance| -> Vec<String> {
        validate(r, &[], &rules)
            .into_iter()
            .filter(|p| p.contains("The id"))
            .collect()
    };
    let renamed = rename::renamed(&home, TO);
    assert_eq!((renamed.id.as_str(), renamed.version), (TO, 5));
    assert_eq!(id_problems(&renamed), Vec::<String>::new());
    assert_eq!(id_problems(&home), Vec::<String>::new());
    let mut not_a_zone = renamed.clone();
    not_a_zone.id = "example.com".into();
    assert!(id_problems(&not_a_zone)[0].contains("one of the home instance's zones"));
    let mut dotted_linked = linked.clone();
    dotted_linked.id = "noisypigeon.com".into();
    assert!(id_problems(&dotted_linked)[0].contains("only the home instance"));
    let mut linked_slash = linked.clone();
    linked_slash.id = "noisy/pigeon".into();
    assert_eq!(id_problems(&linked_slash), Vec::<String>::new());
    for bad in ["pigeon..dev", "pigeon.dev/x", ".pigeon.dev", "Pigeon.dev"] {
        let mut r = renamed.clone();
        r.id = bad.into();
        r.zones = vec![bad.into()];
        assert!(!id_problems(&r).is_empty(), "{bad}");
    }
    // Signed instance context carries the dotted id; a malformed one is still refused.
    {
        use pigeon_contract::{Refused, WorkerContext, sign_with_context, verify_for};
        let key = b"fixture worker key";
        let caller = json!({"user":{"id":"fixture"}});
        let body = br#"{"args":{"instance":"pigeon.dev"}}"#;
        for (id, ok) in [
            (TO, true),
            (FROM, true),
            ("pigeon..dev", false),
            ("pigeon.dev/x", false),
        ] {
            let context = WorkerContext::new("pigeon-ci-gateway", Some(id));
            let header = sign_with_context(key, &caller, "call", body, 10, "n", &context);
            let got = verify_for(key, Some(&header), "call", body, 10, "pigeon-ci-gateway");
            match ok {
                true => assert!(got.unwrap().require_instance(id).is_ok(), "{id}"),
                false => assert_eq!(got, Err(Refused::BadContext), "{id}"),
            }
        }
    }

    // ---- The Directory and CI as they are.
    let store = RenameDouble::default();
    let put = |k: &str, v: String| {
        store.settings.borrow_mut().insert(k.into(), v);
    };
    put(&key(FROM), serde_json::to_string(&home).unwrap());
    put(&key("noisypigeon"), serde_json::to_string(&linked).unwrap());
    put(INDEX, json!([FROM, "noisypigeon"]).to_string());
    put(
        &format!("ledger:{FROM}"),
        r#"{"buckets":["pigeon-platform-site"]}"#.into(),
    );
    put(
        "ledger:noisypigeon",
        r#"{"buckets":["noisypigeon-site"]}"#.into(),
    );
    store.approval("draft-old", FROM, "pending", false);
    store.approval("plan-old", FROM, "approved", false);
    store.approval("used-old", FROM, "used", false);
    store.approval("noisy", "noisypigeon", "pending", false);
    store.approval("rename-1", FROM, "pending", true);
    *store.jobs.borrow_mut() = vec![
        Some(FROM.into()),
        Some(FROM.into()),
        Some("noisypigeon".into()),
        None,
        Some(FROM.into()),
    ];
    let before = store.settings.borrow().clone();

    // ---- Before the migration nothing reads differently.
    assert_eq!(block(resolve(&store, FROM)).unwrap(), None);
    let reads = store.reads.get();
    assert_eq!(block(resolve(&store, "noisypigeon")).unwrap(), None);
    assert_eq!(
        store.reads.get(),
        reads,
        "an id with no alias costs no read"
    );
    let args = json!({ "instance": FROM, "hours": 24 });
    assert_eq!(block(current_args(&store, &args)).unwrap(), None);
    assert_eq!(block(record(&store, FROM)).unwrap().unwrap(), home);
    let records = [home.clone(), linked.clone()];
    let read = ScopeRequest::Read {
        instance: FROM.into(),
    };
    assert_eq!(
        resolve_renamed(&records, &read, None),
        resolve_scope(&records, &read)
    );
    assert_eq!(resolve_scope(&records, &read)["instance"], FROM);

    // ---- The dry run: exactly what will be done, and the digest the passkey binds.
    let seen = block(observe(&store, FROM, TO)).unwrap();
    let steps = plan(&seen, FROM, TO);
    let lines: Vec<String> = describe(&steps)
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        lines,
        [
            "write instance:pigeon.dev: the home record as pigeon.dev, version 5",
            "move ledger:pigeon-security/platform to ledger:pigeon.dev",
            "expire 2 open approval(s) for pigeon-security/platform: draft-old, plan-old",
            "list pigeon.dev in instances:index in place of pigeon-security/platform",
            "re-attribute 3 CI job(s) from pigeon-security/platform to pigeon.dev (UPDATE jobs SET instance)",
            "replace instance:pigeon-security/platform with a tombstone naming pigeon.dev: old \
             console URLs, CI reads and MCP calls naming the old id are answered for pigeon.dev \
             for 30 days",
        ],
        "{lines:#?}"
    );
    assert_eq!(
        *store.settings.borrow(),
        before,
        "the dry run changes nothing"
    );
    let kind = pigeon_core::approval::Kind::LinkedPlan;
    let digest_of = |seen: &rename::Seen| {
        let steps = plan(seen, FROM, TO);
        let payload = rename::payload(FROM, TO, &rename::record_digest(seen), &steps);
        assert_eq!(payload["rename"], true);
        pigeon_core::approval::payload_digest(&kind, FROM, &payload)
    };
    let approved = digest_of(&seen);
    assert_eq!(
        digest_of(&block(observe(&store, FROM, TO)).unwrap()),
        approved,
        "the same state plans the same digest"
    );
    // The record changing since the plan changes the digest, so the approval no longer runs.
    let mut edited = seen.clone();
    let mut h = home.clone();
    h.title = "pigeon!".into();
    edited.old = Some(Stored::Record(Box::new(h)));
    assert_ne!(rename::record_digest(&edited), rename::record_digest(&seen));
    assert_ne!(digest_of(&edited), approved);

    // ---- The run.
    let now = 1_800_000_000_000;
    let done = block(run(&store, &steps, "rename-1", now)).unwrap();
    assert_eq!(done.len(), 6, "{done:?}");
    let settings = store.settings.borrow().clone();
    let moved: Instance = serde_json::from_str(&settings[&key(TO)]).unwrap();
    assert_eq!(moved, renamed);
    assert_eq!(settings[INDEX], json!(["noisypigeon", TO]).to_string());
    assert_eq!(
        settings[&format!("ledger:{TO}")],
        r#"{"buckets":["pigeon-platform-site"]}"#
    );
    assert!(!settings.contains_key(&format!("ledger:{FROM}")));
    assert!(settings.contains_key("ledger:noisypigeon"));
    let tomb: Tombstone = serde_json::from_str(&settings[&key(FROM)]).unwrap();
    assert_eq!(
        tomb,
        Tombstone {
            renamed_to: TO.into(),
            renamed_from: FROM.into(),
            renamed_ms: now,
            compat_until_ms: now + COMPAT_MS,
            approval: "rename-1".into(),
        }
    );
    assert_eq!(COMPAT_MS, 30 * 24 * 3_600_000);
    // No key names the old id but the tombstone.
    let old_keys: Vec<&String> = settings.keys().filter(|k| k.contains(FROM)).collect();
    assert_eq!(old_keys, [&key(FROM)]);
    assert!(!settings[INDEX].contains(FROM));
    // CI jobs are re-attributed; others' are not.
    assert_eq!((store.jobs_of(FROM), store.jobs_of(TO)), (0, 3));
    assert_eq!(store.jobs_of("noisypigeon"), 1);
    // Open approvals naming the old id expired; the used one, others' and the rename's own kept.
    let (state, note) = store.state_of("draft-old");
    assert_eq!(state, "expired");
    assert!(note.unwrap().contains("renamed"));
    assert_eq!(store.state_of("plan-old").0, "expired");
    assert_eq!(store.state_of("used-old").0, "used");
    assert_eq!(store.state_of("noisy").0, "pending");
    assert_eq!(store.state_of("rename-1").0, "pending");

    // ---- After: the tombstone resolves; the old id reads as the new one.
    assert_eq!(block(resolve(&store, FROM)).unwrap().as_deref(), Some(TO));
    assert_eq!(block(resolve(&store, TO)).unwrap(), None);
    assert_eq!(block(record(&store, FROM)).unwrap().unwrap().id, TO);
    assert_eq!(block(record(&store, TO)).unwrap().unwrap(), renamed);
    // MCP and console calls naming the old id (`instance` arguments, observability's included).
    assert_eq!(
        block(current_args(&store, &args)).unwrap(),
        Some(json!({ "instance": TO, "hours": 24 }))
    );
    assert_eq!(
        block(current_args(&store, &json!({ "instance": "noisypigeon" }))).unwrap(),
        None
    );
    // instances_list: one home record, pigeon.dev.
    let index: Vec<String> =
        serde_json::from_str(store.settings.borrow().get(INDEX).unwrap()).unwrap();
    let listed: Vec<Instance> = index
        .iter()
        .map(|id| block(record(&store, id)).unwrap().unwrap())
        .collect();
    let homes: Vec<&Instance> = listed
        .iter()
        .filter(|i| i.account == AccountRef::Home)
        .collect();
    assert_eq!(homes.len(), 1);
    assert_eq!(homes[0].id, TO);
    // CI reads naming the old id resolve for pigeon.dev and CI accepts them; jobs attributed
    // after the rename resolve by repository to pigeon.dev.
    let records = [renamed.clone(), linked.clone()];
    let scope = resolve_renamed(&records, &read, Some(TO));
    assert_eq!(scope["instance"], TO);
    assert_eq!(scope["renamed_from"], FROM);
    let scope: pigeon_ci::scope::Scope = serde_json::from_value(scope).unwrap();
    assert!(scope.answers(FROM) && scope.answers(TO) && !scope.answers("noisypigeon"));
    assert!(scope.allows(Some(TO), "pigeon-security/pigeon", "test"));
    assert!(!scope.allows(Some(FROM), "pigeon-security/pigeon", "test"));
    let fresh = resolve_scope(
        &records,
        &ScopeRequest::Attribute {
            repo: "pigeon-security/pigeon".into(),
            job: "test".into(),
        },
    );
    assert_eq!(fresh["instance"], TO);
    assert!(fresh.get("renamed_from").is_none());
    let unaliased: pigeon_ci::scope::Scope = serde_json::from_value(fresh).unwrap();
    assert!(!unaliased.answers(FROM));
    // Console URLs: the old id's pages answer 301 to the same page of the new id.
    let (id, rest) = page::page_instance("/app/pigeon-security%2Fplatform/site/visitors").unwrap();
    assert_eq!((id.as_str(), rest), (FROM, "/site/visitors"));
    let to = format!("{}/{}{rest}", page::BASE, page::segment(TO));
    assert_eq!(to, "/app/pigeon.dev/site/visitors");
    assert_eq!(page::url(TO, "inbox", None), "/app/pigeon.dev/inbox");
    for p in [
        "/app/pigeon.dev/inbox",
        "/app/pigeon.dev/site/visitors",
        &to,
    ] {
        assert_eq!(Get::route(p, None), Some(Get::App), "{p}");
    }
    for p in [
        "/app/shell.css",
        "/app/pigeon.dev",
        "/app/pigeon.dev/site/x.js",
        "/app/views/nope.js",
    ] {
        assert_ne!(Get::route(p, None), Some(Get::App), "{p}");
    }
    assert_eq!(
        page::page_instance("/app/pigeon.dev/inbox"),
        Some((TO.to_string(), "/inbox"))
    );
    assert_eq!(page::page_instance("/app"), None);
    assert_eq!(
        Get::route("/app", Some("instance=pigeon.dev&tab=cost")),
        Some(Get::Bookmark(page::Bookmark::Area {
            instance: Some(TO.into()),
            area: "cost",
            view: None,
        }))
    );

    // ---- A second run is a no-op.
    let again = plan(&block(observe(&store, FROM, TO)).unwrap(), FROM, TO);
    assert_eq!(again, Vec::<Step>::new());
    let settled = store.settings.borrow().clone();
    assert!(
        block(run(&store, &again, "rename-2", now + 1))
            .unwrap()
            .is_empty()
    );
    assert_eq!(*store.settings.borrow(), settled);

    // ---- It refuses a conflicting pigeon.dev, and changes nothing.
    let conflict = RenameDouble::default();
    for (k, v) in &before {
        conflict.settings.borrow_mut().insert(k.clone(), v.clone());
    }
    let mut other = renamed.clone();
    other.title = "someone else's".into();
    conflict
        .settings
        .borrow_mut()
        .insert(key(TO), serde_json::to_string(&other).unwrap());
    let snapshot = conflict.settings.borrow().clone();
    let blocked = plan(&block(observe(&conflict, FROM, TO)).unwrap(), FROM, TO);
    assert!(
        matches!(&blocked[..], [Step::Blocked { why }] if why.contains("already exists as a different record")),
        "{blocked:?}"
    );
    assert!(block(run(&conflict, &blocked, "x", now)).is_err());
    assert_eq!(*conflict.settings.borrow(), snapshot);
    // A ledger already under the new id with another value also stops it.
    conflict.settings.borrow_mut().remove(&key(TO));
    conflict
        .settings
        .borrow_mut()
        .insert(format!("ledger:{TO}"), "{}".into());
    let blocked = plan(&block(observe(&conflict, FROM, TO)).unwrap(), FROM, TO);
    assert!(
        matches!(&blocked[..], [Step::Blocked { .. }]),
        "{blocked:?}"
    );
    // Only the home instance, and only to one of its zones.
    let mut seen = block(observe(&store, FROM, TO)).unwrap();
    let mut not_home = linked.clone();
    not_home.id = FROM.into();
    seen.old = Some(Stored::Record(Box::new(not_home)));
    seen.new = None;
    assert!(
        matches!(&plan(&seen, FROM, TO)[..], [Step::Blocked { why }] if why.contains("not the home instance"))
    );
    seen.old = Some(Stored::Record(Box::new(home.clone())));
    assert!(
        matches!(&plan(&seen, FROM, "example.com")[..], [Step::Blocked { why }] if why.contains("zones"))
    );

    // ---- A run that stopped part way plans only what remains, then completes.
    let partial = RenameDouble::default();
    for (k, v) in &before {
        partial.settings.borrow_mut().insert(k.clone(), v.clone());
    }
    *partial.jobs.borrow_mut() = vec![Some(FROM.into())];
    partial.fail_jobs.set(true);
    let steps = plan(&block(observe(&partial, FROM, TO)).unwrap(), FROM, TO);
    assert!(block(run(&partial, &steps, "rename-1", now)).is_err());
    assert_eq!(
        block(resolve(&partial, FROM)).unwrap(),
        None,
        "no tombstone until the last step"
    );
    partial.fail_jobs.set(false);
    let rest = plan(&block(observe(&partial, FROM, TO)).unwrap(), FROM, TO);
    assert!(
        matches!(
            &rest[..],
            [
                Step::ReattributeJobs { jobs: 1, .. },
                Step::Tombstone { .. }
            ]
        ),
        "{rest:?}"
    );
    block(run(&partial, &rest, "rename-3", now)).unwrap();
    assert_eq!(partial.jobs_of(TO), 1);
    assert_eq!(
        plan(&block(observe(&partial, FROM, TO)).unwrap(), FROM, TO),
        Vec::<Step>::new()
    );
}

/// Redirects (docs/plans/redirects.md): a record's `redirects` are checked (in a zone, listed
/// once, not a site's host, not another instance's, not platform-owned, an `https://` target
/// that does not loop, a redirect status, at most 20); the plan attaches each host to the
/// instance's own Worker and uploads it with the redirect map, for an instance with no site and
/// for one whose only site is a passkey-protected platform subdomain (willowgraysen's shape);
/// the Worker's answer is decided as `redirects::answer` decides it; the drawing labels each
/// redirect host with its target.
#[test]
fn redirects_answered_by_the_instance_worker() {
    use pigeon_capabilities::architecture::{Mode, Theme, Width, check, draw, instance_model};
    use pigeon_capabilities::instances::plan::{
        HOME_REDIRECTS, Ledger, Observed, PlatformOwned, SITE_WORKER, SeenDomain, SeenSubdomain,
        Step, describe, plan, protected_site_worker, record, with_redirects,
    };
    use pigeon_capabilities::instances::record::{Instance, Rules, digest, validate};
    use pigeon_capabilities::instances::redirects::{self, answer, config, wrap};

    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &["platform".to_string()],
        reserved_workers: &[],
        platform_worker: "pigeon-platform",
    };
    let willow = json!({
        "id": "willowgraysen", "title": "Willow Graysen",
        "account": { "kind": "linked", "account_id": "b".repeat(32) },
        "zones": ["willowgraysen.com"], "worker": "willowgraysen",
        "sites": [{ "name": "site", "bucket": "willowgraysen-site", "auth": "passkey",
            "allow": ["site"], "targets": [{ "kind": "subdomain", "label": "willowgraysen" }] }],
        "redirects": [{ "host": "willowgraysen.com", "to": "https://noisypigeon.com" }],
        "version": 3
    });
    let inst: Instance = serde_json::from_value(willow.clone()).unwrap();
    assert_eq!(validate(&inst, &[], &rules), Vec::<String>::new());
    // Defaults stay absent, and a record without redirects keeps its digest.
    let v = serde_json::to_value(&inst).unwrap();
    assert_eq!(
        v["redirects"],
        json!([{ "host": "willowgraysen.com", "to": "https://noisypigeon.com" }])
    );
    let mut bare = inst.clone();
    bare.redirects.clear();
    assert!(
        serde_json::to_value(&bare)
            .unwrap()
            .get("redirects")
            .is_none()
    );
    let mut without = willow.clone();
    without.as_object_mut().unwrap().remove("redirects");
    assert_eq!(
        digest(&bare),
        digest(&serde_json::from_value(without).unwrap())
    );

    // Refused, one problem each.
    let refused = |patch: Value, what: &str| {
        let mut v = willow.clone();
        for (k, x) in patch.as_object().unwrap() {
            v[k] = x.clone();
        }
        let i: Instance = serde_json::from_value(v).unwrap();
        let problems = validate(&i, &[], &rules);
        assert!(
            problems.iter().any(|p| p.contains(what)),
            "{what}: {problems:?}"
        );
    };
    let one = |r: Value| json!({ "redirects": [r] });
    refused(
        one(json!({ "host": "example.com", "to": "https://noisypigeon.com" })),
        "not in this instance's zones",
    );
    refused(
        one(json!({ "host": "Willowgraysen.com", "to": "https://noisypigeon.com" })),
        "not a valid lowercase DNS name",
    );
    refused(
        one(json!({ "host": "willowgraysen.com", "to": "http://noisypigeon.com" })),
        "must start with https://",
    );
    refused(
        one(json!({ "host": "willowgraysen.com", "to": "https://noisypigeon.com/?a=1" })),
        "query or a fragment",
    );
    refused(
        one(json!({ "host": "willowgraysen.com", "to": "https://u@noisypigeon.com" })),
        "must not name a user",
    );
    refused(
        one(json!({ "host": "willowgraysen.com", "to": "https://WillowGraysen.com/x" })),
        "would loop",
    );
    refused(
        one(json!({ "host": "willowgraysen.com", "to": "https://noisypigeon.com", "status": 303 })),
        "must be 301, 302, 307 or 308",
    );
    refused(
        json!({ "redirects": [
            { "host": "willowgraysen.com", "to": "https://noisypigeon.com" },
            { "host": "willowgraysen.com", "to": "https://example.com" }] }),
        "listed more than once",
    );
    refused(
        json!({ "redirects": [
            { "host": "a.willowgraysen.com", "to": "https://b.willowgraysen.com" },
            { "host": "b.willowgraysen.com", "to": "https://a.willowgraysen.com" }] }),
        "would loop",
    );
    let many: Vec<Value> = (0..21)
        .map(|n| json!({ "host": format!("h{n}.willowgraysen.com"), "to": "https://noisypigeon.com" }))
        .collect();
    refused(json!({ "redirects": many }), "at most 20");
    refused(
        json!({ "sites": [{ "name": "site", "bucket": "willowgraysen-site",
            "targets": [{ "kind": "domain", "hostname": "willowgraysen.com" }] }] }),
        "also a site target",
    );
    refused(
        json!({ "capabilities": { "redirects": {} } }),
        "leave it out of capabilities",
    );
    refused(json!({ "worker": null }), "The worker is missing");
    // Another instance's host is refused.
    let mut other = inst.clone();
    other.id = "other".into();
    other.worker = Some("other".into());
    other.sites.clear();
    let mut mine = inst.clone();
    mine.id = "mine".into();
    assert!(
        validate(&mine, &[other], &rules)
            .iter()
            .any(|p| p.contains("already used by instance other"))
    );
    // The home instance may not redirect a platform-owned host.
    let home: Instance = serde_json::from_value(json!({
        "id": "home", "title": "Home", "account": { "kind": "home" },
        "zones": ["pigeon.dev"], "worker": "pigeon-platform",
        "redirects": [{ "host": "platform.pigeon.dev", "to": "https://noisypigeon.com" }]
    }))
    .unwrap();
    assert!(
        validate(&home, &[], &rules)
            .iter()
            .any(|p| p.contains("the platform's own"))
    );

    // The map the Worker reads, and its answers: the path and query kept unless asked not to.
    let map = config(&inst).unwrap();
    assert_eq!(
        map,
        r#"{"willowgraysen.com":{"cache":"public, max-age=3600","keep_path":true,"status":301,"to":"https://noisypigeon.com"}}"#
    );
    assert_eq!(config(&bare), None);
    assert_eq!(
        answer(&map, "WillowGraysen.com", "/a/b", "?c=1"),
        Some((
            301,
            "https://noisypigeon.com/a/b?c=1".into(),
            "public, max-age=3600".into()
        ))
    );
    assert_eq!(
        answer(&map, "willowgraysen.com", "/", "").unwrap().1,
        "https://noisypigeon.com/"
    );
    assert_eq!(answer(&map, "willowgraysen.pigeon.dev", "/", ""), None);
    let mut temp = inst.clone();
    temp.redirects[0].to = "https://noisypigeon.com/landing/".into();
    temp.redirects[0].status = Some(302);
    temp.redirects[0].keep_path = Some(false);
    assert_eq!(
        answer(&config(&temp).unwrap(), "willowgraysen.com", "/x", "?y"),
        Some((
            302,
            "https://noisypigeon.com/landing/".into(),
            "no-store".into()
        ))
    );
    temp.redirects[0].keep_path = None;
    assert_eq!(
        answer(&config(&temp).unwrap(), "willowgraysen.com", "/x", "?y")
            .unwrap()
            .1,
        "https://noisypigeon.com/landing/x?y"
    );
    // The Worker decides as `answer` does, before the site and a protected origin's key check.
    let js = redirects::MODULE;
    assert!(js.contains(
        "const location = rule.keep_path ? rule.to + url.pathname + url.search : rule.to;"
    ));
    assert!(js.contains("\"cache-control\": rule.cache"));
    assert!(js.contains("return served.fetch(request, env, ctx);"));
    for module in [wrap(SITE_WORKER), wrap(&protected_site_worker())] {
        assert_eq!(module.matches("export default {").count(), 1);
        assert!(module.contains("const served = {"));
        let export = module.find("export default {").unwrap();
        assert!(module[export..].contains("redirectMap[url.hostname]"));
        assert!(!module[export..].contains("authorized("));
    }

    // The plan: willowgraysen's shape, nothing there yet.
    let seen = |worker: bool, served: Option<&str>| Observed {
        account_id: "b".repeat(32),
        bucket_exists: worker,
        worker_exists: worker,
        workers_dev: worker.then_some(true),
        account_subdomain: Some("willow".into()),
        domains: vec![SeenDomain {
            hostname: "willowgraysen.com".into(),
            zone_id: Some("zw".into()),
            service: served.map(str::to_string),
        }],
        subdomains: vec![SeenSubdomain {
            hostname: "willowgraysen.pigeon.dev".into(),
            zone_id: Some("zp".into()),
            worker: "pigeon-sites".into(),
            service: worker.then(|| "pigeon-sites".into()),
            upstream: worker.then(|| "https://willowgraysen.willow.workers.dev".into()),
        }],
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
    let first = plan(&inst, &seen(false, None), &Ledger::default(), None);
    assert_eq!(
        tags(&first),
        [
            "create_bucket",
            "upload_worker",
            "enable_workers_dev",
            "attach_domain",
            "attach_subdomain"
        ]
    );
    assert!(
        matches!(&first[1], Step::UploadWorker { origin: Some(_), redirects: Some(m), bucket: Some(b), .. }
        if *m == map && b == "willowgraysen-site")
    );
    assert!(
        matches!(&first[3], Step::AttachDomain { hostname, zone_id, worker }
        if hostname == "willowgraysen.com" && zone_id == "zw" && worker == "willowgraysen")
    );
    let lines = describe(&first);
    assert!(
        lines[1]
            .as_str()
            .unwrap()
            .contains("authenticated origin Worker willowgraysen")
    );
    assert!(
        lines[1]
            .as_str()
            .unwrap()
            .contains("it answers willowgraysen.com with its redirect first")
    );

    // Already deployed without redirects (sandbox's shape today): the Worker is updated with
    // the map and the host attached; nothing else changes.
    let mut l = Ledger::default();
    for s in plan(&bare, &seen(false, None), &Ledger::default(), None) {
        record(&mut l, &s);
    }
    let before = plan(&bare, &seen(true, None), &l, None);
    assert!(!before.iter().any(Step::changes), "{before:?}");
    let added = plan(&inst, &seen(true, None), &l, None);
    assert_eq!(
        tags(&added),
        ["ok", "upload_worker", "ok", "attach_domain", "ok"]
    );
    assert!(matches!(
        &added[1],
        Step::UploadWorker {
            replaces: true,
            origin: Some(_),
            ..
        }
    ));
    for s in &added {
        record(&mut l, s);
    }
    assert_eq!(l.domains, ["willowgraysen.com"]);
    let applied = plan(&inst, &seen(true, Some("willowgraysen")), &l, None);
    assert!(!applied.iter().any(Step::changes), "{applied:?}");
    // Another Worker's custom domain is a conflict, never moved.
    assert!(
        plan(&inst, &seen(true, Some("someone-else")), &l, None)
            .iter()
            .any(Step::stops)
    );
    // Removing the redirect detaches nothing: the Worker goes back to the site module alone.
    let removed = plan(&bare, &seen(true, Some("willowgraysen")), &l, None);
    assert_eq!(tags(&removed), ["ok", "upload_worker", "ok", "ok"]);
    assert!(
        !removed
            .iter()
            .any(|s| matches!(s, Step::AttachDomain { .. }))
    );

    // An instance with redirects and no site: its Worker alone, with no bucket.
    let mut only = inst.clone();
    only.sites.clear();
    assert_eq!(validate(&only, &[], &rules), Vec::<String>::new());
    let mut o = seen(false, None);
    o.subdomains.clear();
    let steps = plan(&only, &o, &Ledger::default(), None);
    assert_eq!(tags(&steps), ["upload_worker", "attach_domain"]);
    assert!(
        matches!(&steps[0], Step::UploadWorker { bucket: None, origin: None, redirects: Some(_), replaces: false, module, .. }
        if *module == with_redirects(None, Some(&map)))
    );
    assert!(
        describe(&steps)[0]
            .as_str()
            .unwrap()
            .contains("answers only its redirects (willowgraysen.com)")
    );
    let mut lo = Ledger::default();
    for s in &steps {
        record(&mut lo, s);
    }
    o.worker_exists = true;
    o.domains[0].service = Some("willowgraysen".into());
    assert_eq!(tags(&plan(&only, &o, &lo, None)), ["ok", "ok"]);
    let mut moved = only.clone();
    moved.redirects[0].to = "https://example.com".into();
    assert!(matches!(
        &plan(&moved, &o, &lo, None)[0],
        Step::UploadWorker { replaces: true, .. }
    ));
    // A Worker with that name pigeon did not upload: a conflict.
    assert!(
        plan(&only, &o, &Ledger::default(), None)
            .iter()
            .any(Step::stops)
    );

    // The home instance's plan uploads no Worker, so its redirects are blocked, with the reason.
    let mut home_ok = home.clone();
    home_ok.redirects[0].host = "go.pigeon.dev".into();
    let owned = PlatformOwned {
        workers: vec!["pigeon-platform".into()],
        hosts: vec!["pigeon.dev".into()],
        apex: "pigeon.dev".into(),
    };
    let h = plan(&home_ok, &o, &Ledger::default(), Some(&owned));
    assert!(matches!(h.as_slice(), [Step::Blocked { why }] if why == HOME_REDIRECTS));

    // The drawing: the redirect host beside the subdomain, on the instance's Worker, labelled.
    let platform = json!({ "commit": "c", "digest": "d", "resources": { "zone": "pigeon.dev" } });
    let m = instance_model(&inst, &[], &[], &platform);
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(
        v["hostnames"],
        json!([
            { "host": "willowgraysen.com", "worker": "willowgraysen", "by": "redirects",
              "access": [], "public_paths": [], "redirect": "https://noisypigeon.com" },
            { "host": "willowgraysen.pigeon.dev", "worker": "willowgraysen", "by": "instances",
              "access": [], "public_paths": [] }
        ])
    );
    assert!(check(&m).is_empty(), "{:?}", check(&m));
    for mode in [Mode::Compact, Mode::Full] {
        for width in [Width::Wide, Width::Narrow] {
            let d = draw(&m, mode, width, Theme::Light);
            assert!(
                d.svg.contains("redirects to noisypigeon.com"),
                "{mode:?} {width:?}"
            );
            assert!(d.text.contains("willowgraysen.com"));
        }
    }
    let m = instance_model(&only, &[], &[], &platform);
    assert_eq!(m.workers[0].handlers, ["fetch"]);
    assert!(check(&m).is_empty());
}

/// Step 4 of docs/plans/caller-keys.md: protected origin keys are derived from `SITE_KEY`, not
/// `CALLER_KEY`. A protected origin Worker pigeon uploaded before then holds the old key, so its
/// recorded digest no longer matches: the plan shows the Worker's upload again (replacing
/// pigeon's own), with its origin, and once applied it reads back unchanged. Public sites'
/// Workers are untouched.
#[test]
fn protected_origins_replan_once_their_key_moves_to_site_key() {
    use pigeon_capabilities::instances::{
        plan::{
            Ledger, ORIGIN_KEY_BINDING, Observed, Step, module_digest, plan, protected_site_worker,
            record,
        },
        record::Instance,
    };

    let mut instance: Instance = serde_json::from_value(json!({
        "id": "fixture", "title": "Fixture", "worker": "fixture",
        "account": {"kind":"linked","account_id":"00000000000000000000000000000000"}, "version": 1,
        "sites": [{
            "name": "preview", "bucket": "fixture-site", "auth": "passkey", "allow": ["site"],
            "targets": [{"kind":"subdomain","label":"fixture"}]
        }]
    }))
    .unwrap();
    let observed = Observed {
        account_id: "fixture".into(),
        bucket_exists: true,
        worker_exists: true,
        workers_dev: Some(true),
        account_subdomain: Some("fixture".into()),
        domains: vec![],
        subdomains: vec![],
    };
    // The digest pigeon recorded for the Worker before step 4: no key source in it.
    let before = pigeon_core::oauth::hash(
        &json!({
            "public_module": module_digest("fixture-site", None),
            "protected_module": protected_site_worker(),
            "origin_key": ORIGIN_KEY_BINDING,
            "instance": "fixture",
            "site": "preview",
            "upstream": "https://fixture.fixture.workers.dev",
        })
        .to_string(),
    );
    let mut ledger = Ledger {
        buckets: vec!["fixture-site".into()],
        workers: vec![("fixture".into(), before.clone())],
        protected_workers: vec!["fixture".into()],
        protected_buckets: vec!["fixture-site".into()],
        ..Ledger::default()
    };
    let steps = plan(&instance, &observed, &ledger, None);
    assert!(!steps.iter().any(Step::stops), "{steps:?}");
    let changes: Vec<&Step> = steps.iter().filter(|s| s.changes()).collect();
    let [
        Step::UploadWorker {
            name,
            replaces: true,
            origin: Some(origin),
            module,
            ..
        },
    ] = changes.as_slice()
    else {
        panic!("only the origin Worker's upload: {steps:?}");
    };
    assert_eq!(name, "fixture");
    assert_eq!(origin, "https://fixture.fixture.workers.dev");
    assert_ne!(module, &before);
    // Applied, it reads back unchanged.
    let upload = changes[0].clone();
    record(&mut ledger, &upload);
    assert!(
        !plan(&instance, &observed, &ledger, None)
            .iter()
            .any(Step::changes)
    );

    // A public site's Worker keeps its digest: nothing to re-apply.
    instance.sites[0].auth = None;
    instance.sites[0].allow.clear();
    let public = Ledger {
        buckets: vec!["fixture-site".into()],
        workers: vec![("fixture".into(), module_digest("fixture-site", None))],
        ..Ledger::default()
    };
    assert!(
        !plan(&instance, &observed, &public, None)
            .iter()
            .any(Step::changes)
    );
}

#[test]
fn cloud_gateway_off_by_default_and_scoped_status_opt_in() {
    use pigeon_capabilities::catalogue::{hosted_enabled, placements, problems};
    use pigeon_capabilities::cloud_scope::{
        NAME, STATUS, discovered_globally, home_enabled, route,
    };
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_core::mcp::Tool;

    let make = |id, kind, capabilities: Value| -> Instance {
        serde_json::from_value(json!({
            "id": id,
            "title": "Cloud Gateway fixture",
            "account": { "kind": kind },
            "capabilities": capabilities,
        }))
        .unwrap()
    };
    let home = make("pigeon.dev", "home", json!({}));
    let home_on = make("pigeon.dev", "home", json!({ "cloud": {} }));
    let linked = make("willowgraysen", "linked", json!({ "cloud": {} }));
    let linked_off = make("elsewhere", "linked", json!({}));
    let linked_inline = make(
        "willowgraysen",
        "linked",
        json!({ "cloud": { "placement": "inline" } }),
    );
    let linked_dedicated = make(
        "willowgraysen",
        "linked",
        json!({ "cloud": { "placement": "own_worker" } }),
    );
    assert!(problems(&linked_inline).is_empty());
    assert_eq!(
        placements(&linked_inline)[NAME]["worker"],
        "pigeon-platform"
    );
    assert!(
        problems(&linked_dedicated)
            .iter()
            .any(|why| why.contains("only inline"))
    );
    assert!(problems(&home_on).is_empty());
    assert!(problems(&linked).is_empty());
    assert!(!hosted_enabled(&home, NAME));
    assert!(placements(&home).get(NAME).is_none());
    assert_eq!(placements(&linked)[NAME]["worker"], "pigeon-platform");
    assert!(!home_enabled(None));
    assert!(!home_enabled(Some(&linked)));
    assert!(!home_enabled(Some(&home)));
    assert!(home_enabled(Some(&home_on)));
    assert!(!discovered_globally(NAME, home_enabled(Some(&home))));
    assert!(discovered_globally(NAME, home_enabled(Some(&home_on))));
    assert!(discovered_globally("source", false));

    let names = |scope: &Scope| {
        let mut tools = vec![
            Tool::bare("call", "Call by name"),
            Tool::bare(STATUS, "Shared provider status"),
            Tool::bare("cloud_tokens_list", "Shared provider tokens"),
        ];
        scope.restrict_tools(&mut tools);
        tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>()
    };
    let off = Scope::new(&linked_off).unwrap();
    let on = Scope::new(&linked).unwrap();
    assert!(names(&Scope::new(&home).unwrap()).is_empty());
    assert!(names(&off).is_empty());
    assert_eq!(names(&on), ["call", STATUS]);
    assert!(route(Some(&off), true, STATUS, &json!({})).is_err());
    assert!(route(Some(&on), false, "cloud_tokens_list", &json!({})).is_err());
    assert!(
        route(
            Some(&on),
            false,
            STATUS,
            &json!({ "instance": "elsewhere" })
        )
        .is_err()
    );
    assert!(
        on.bind(
            "call",
            &json!({ "tool": "cloud_apply", "args": { "plan": "x" } })
        )
        .is_err()
    );
    let bound = on.bind("call", &json!({ "tool": STATUS })).unwrap();
    assert_eq!(bound["args"]["instance"], "willowgraysen");
    let status = route(Some(&on), false, STATUS, &bound["args"])
        .unwrap()
        .unwrap();
    assert_eq!(status["instance"], "willowgraysen");
    assert_eq!(status["providers_configured"], false);

    assert!(route(None, false, "cloud_tokens_list", &json!({})).is_err());
    assert!(
        route(
            None,
            true,
            "cloud_tokens_list",
            &json!({ "instance": "willowgraysen" })
        )
        .is_err()
    );
    assert_eq!(route(None, true, "cloud_tokens_list", &json!({})), Ok(None));
}

#[test]
fn digitalocean_connection_uses_approved_instance_team_and_version() {
    use pigeon_capabilities::catalogue::problems;
    use pigeon_capabilities::console::write::{Step, action, digest};
    use pigeon_capabilities::digitalocean::{
        SET_TOKEN, STATUS, VERIFY, connection_status, matches_target, token_context, token_key,
    };
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_capabilities::membership::{Need, Role, tool_need, write_need};
    use pigeon_core::{mcp::Tool, oauth::hash};

    let record = |capabilities: Value| -> Instance {
        serde_json::from_value(json!({
            "id": "willowgraysen",
            "title": "Fixture",
            "account": { "kind": "linked" },
            "capabilities": capabilities,
            "version": 4,
        }))
        .unwrap()
    };
    let off = record(json!({ "cloud": {} }));
    let missing_cloud = record(json!({ "digitalocean": { "team_id": "team_A" } }));
    let bad_team = record(json!({ "cloud": {}, "digitalocean": { "team_id": "https://other" } }));
    let wrong_setting = record(json!({ "cloud": { "team_id": "team_A" } }));
    let on = record(json!({ "cloud": {}, "digitalocean": { "team_id": "team_A" } }));
    assert!(problems(&on).is_empty());
    assert!(problems(&missing_cloud).iter().any(|p| p.contains("needs")));
    assert!(problems(&bad_team).iter().any(|p| p.contains("team_id")));
    assert!(
        problems(&wrong_setting)
            .iter()
            .any(|p| p.contains("team_id"))
    );

    let names = |record: &Instance| {
        let scope = Scope::new(record).unwrap();
        let mut tools = vec![Tool::bare(STATUS, "Status"), Tool::bare(VERIFY, "Verify")];
        scope.restrict_tools(&mut tools);
        tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>()
    };
    assert!(names(&off).is_empty());
    assert!(names(&missing_cloud).is_empty());
    assert_eq!(names(&on), [STATUS, VERIFY]);
    let scoped = Scope::new(&on).unwrap();
    for args in [
        json!({"instance":"other"}),
        json!({"team_id":"team_B"}),
        json!({"token":"example"}),
        json!({"url":"https://example.invalid"}),
    ] {
        assert!(scoped.bind(VERIFY, &args).is_err());
        assert!(
            scoped
                .bind("call", &json!({"tool":VERIFY,"args":args}))
                .is_err()
        );
    }
    assert_eq!(connection_status(&on, None).unwrap()["state"], "missing");
    assert_eq!(
        connection_status(&on, Some("team_B")).unwrap()["state"],
        "unverified"
    );
    assert_eq!(
        connection_status(&on, Some("team_A")).unwrap(),
        json!({"instance":"willowgraysen", "enabled":true, "state":"connected","team_id":"team_A"})
    );
    assert_ne!(token_key("willowgraysen"), token_key("other"));
    assert_ne!(
        token_context("willowgraysen", "team_A"),
        token_context("other", "team_A")
    );
    assert_ne!(
        token_context("willowgraysen", "team_A"),
        token_context("willowgraysen", "team_B")
    );

    let action = action(SET_TOKEN).unwrap();
    let target = action
        .target(&json!({"instance":"willowgraysen","team_id":"team_A","version":4}))
        .unwrap();
    let value = "dummy-token";
    let start = action
        .change(&json!({"digest":hash(value)}), Step::Begin)
        .unwrap();
    let finish = action
        .change(&json!({"value":value}), Step::Finish)
        .unwrap();
    assert!(matches_target(&on, &target));
    assert_eq!(
        write_need(action, &target),
        Need::On(Some("willowgraysen".into()), Role::Operator)
    );
    assert_eq!(
        tool_need("digitalocean", STATUS, &json!({})),
        Some(Role::Viewer)
    );
    assert_eq!(
        tool_need("digitalocean", VERIFY, &json!({})),
        Some(Role::Operator)
    );
    assert_eq!(action.bound(&start), action.bound(&finish));
    assert!(!action.bound(&finish).to_string().contains(value));
    let challenge = |target: &Value| {
        digest(
            action,
            "fixture-person",
            target,
            &action.bound(&finish),
            None,
        )
    };
    let mut stale_version = target.clone();
    stale_version["version"] = json!(3);
    assert!(!matches_target(&on, &stale_version));
    assert_ne!(challenge(&target), challenge(&stale_version));
}

#[test]
fn digitalocean_spaces_has_one_approved_instance_scope() {
    use pigeon_capabilities::catalogue::problems;
    use pigeon_capabilities::console::write::{Step, action};
    use pigeon_capabilities::digitalocean::{self, spaces};
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};
    use pigeon_capabilities::membership::{Role, tool_need};
    use pigeon_core::oauth::hash;

    let mut record: Instance = serde_json::from_value(json!({
        "id":"willowgraysen",
        "title":"Fixture",
        "account":{"kind":"linked"},
        "capabilities":{"cloud":{}, "digitalocean":{"team_id":"approved-team"}},
        "version":7
    }))
    .unwrap();
    assert!(
        Scope::new(&record)
            .unwrap()
            .bind(spaces::VERIFY, &json!({}))
            .is_err()
    );
    record.capabilities.get_mut("digitalocean").unwrap().spaces = Some(
        serde_json::from_value(
            json!({"region":"nyc3","bucket":"fixture-bucket","prefix":"inside/"}),
        )
        .unwrap(),
    );
    assert!(problems(&record).is_empty());
    let scope = Scope::new(&record).unwrap();
    assert_eq!(
        scope.bind(spaces::VERIFY, &json!({})).unwrap()["instance"],
        record.id
    );
    for override_args in [
        json!({"instance":"another"}),
        json!({"bucket":"outside"}),
        json!({"region":"sfo3"}),
        json!({"prefix":"outside/"}),
        json!({"token":"example"}),
    ] {
        assert!(scope.bind(spaces::VERIFY, &override_args).is_err());
        assert!(
            scope
                .bind("call", &json!({"tool":spaces::VERIFY,"args":override_args}))
                .is_err()
        );
    }
    assert_eq!(
        tool_need(digitalocean::NAME, spaces::VERIFY, &json!({})),
        Some(Role::Operator)
    );
    let set = action(spaces::SET_KEY).unwrap();
    assert!(!set.by_bearer);
    let target = set
        .target(&json!({"instance":"willowgraysen","team_id":"approved-team","version":7}))
        .unwrap();
    assert!(digitalocean::matches_target(&record, &target));
    let pair = "dummy-id:dummy-secret";
    let start = set
        .change(&json!({"digest":hash(pair)}), Step::Begin)
        .unwrap();
    let finish = set.change(&json!({"value":pair}), Step::Finish).unwrap();
    assert_eq!(set.bound(&start), set.bound(&finish));
    assert!(!set.bound(&finish).to_string().contains(pair));

    record
        .capabilities
        .get_mut("digitalocean")
        .unwrap()
        .spaces
        .as_mut()
        .unwrap()
        .region = "fake".into();
    assert!(!problems(&record).is_empty());
    assert!(spaces::binding(&record).is_none());
}

#[test]
fn digitalocean_droplet_plan_is_opted_in_and_passkey_bound() {
    use pigeon_capabilities::console::write::{Step, action, digest};
    use pigeon_capabilities::digitalocean::health;
    use pigeon_capabilities::instances::{record::Instance, tools::Scope};

    let mut record: Instance = serde_json::from_value(json!({
        "id":"willowgraysen", "title":"Fixture", "account":{"kind":"linked"},
        "capabilities":{"cloud":{}, "digitalocean":{"team_id":"approved-team"}},
    }))
    .unwrap();
    assert!(
        Scope::new(&record)
            .unwrap()
            .bind(health::STATUS, &json!({}))
            .is_err()
    );
    record
        .capabilities
        .get_mut("digitalocean")
        .unwrap()
        .droplet_health = true;
    let scope = Scope::new(&record).unwrap();
    assert!(
        scope
            .bind(health::STATUS, &json!({"instance":"someone-else"}))
            .is_err()
    );
    assert_eq!(
        scope.bind(health::STATUS, &json!({})).unwrap()["instance"],
        "willowgraysen"
    );
    let plan = health::plan();
    assert_eq!(plan["region"], "tor1");
    assert_eq!(plan["max_minutes"], 30);
    assert_eq!(plan["commit"].as_str().unwrap().len(), 40);
    let start = action(health::START).unwrap();
    assert!(!start.by_bearer);
    assert!(start.change(&json!({}), Step::Begin).is_err());
    let target = start
        .target(&json!({"instance":"willowgraysen","team_id":"approved-team","version":0}))
        .unwrap();
    let current = start
        .change(&json!({"plan":plan["fingerprint"]}), Step::Finish)
        .unwrap();
    let stale = start
        .change(&json!({"plan":"a".repeat(43)}), Step::Finish)
        .unwrap();
    assert_ne!(
        digest(start, "operator", &target, &current, None),
        digest(start, "operator", &target, &stale, None)
    );
    let cancel = action(health::CANCEL).unwrap();
    assert!(cancel.destructive && !cancel.by_bearer);
    let one = cancel
        .change(&json!({"run":"1".repeat(32)}), Step::Finish)
        .unwrap();
    let two = cancel
        .change(&json!({"run":"2".repeat(32)}), Step::Finish)
        .unwrap();
    assert_ne!(
        digest(cancel, "operator", &target, &one, None),
        digest(cancel, "operator", &target, &two, None)
    );
    assert!(
        cancel
            .change(&json!({"run":"../../random"}), Step::Finish)
            .is_err()
    );
}

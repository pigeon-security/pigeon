use pigeon_capabilities::jobs_audit::diagnostic_object;
use serde_json::{Value, json};

fn terminal() -> Value {
    json!({
        "id": "0123456789abcdef0123456789abcdef",
        "state": "gone",
        "health": "passed",
        "started_ms": 100,
        "deadline_ms": 500,
        "finished_ms": 501,
        "compute_estimate": {
            "estimated_usd_micros": 4, "usd_micros_per_hour": 35710,
            "duration_ms": 401, "deletion_confirmed": true, "ignored": "private details"
        },
        "events": [
            {"at_ms": 110, "stage": "image_build", "outcome": "succeeded", "output": "private log"},
            {"at_ms": 501, "stage": "cleanup", "outcome": "confirmed"}
        ],
        "error": "provider details",
        "credential": "private credential data"
    })
}

#[test]
fn jobs_audit_contains_only_bounded_structured_diagnostics() {
    let mut job = terminal();
    let first = diagnostic_object("owner/demo", &job).unwrap();
    let document: Value = serde_json::from_slice(&first.json).unwrap();
    assert_eq!(
        document,
        json!({
            "schema_version": 1,
            "instance_id": "owner/demo",
            "job_type": "digitalocean-runner-check",
            "run_id": "0123456789abcdef0123456789abcdef",
            "provider": "digitalocean",
            "job": {
                "id": "0123456789abcdef0123456789abcdef",
                "state": "gone",
                "health": "passed",
                "started_ms": 100,
                "deadline_ms": 500,
                "finished_ms": 501,
                "compute_estimate": {
                    "estimated_usd_micros": 4, "usd_micros_per_hour": 35710,
                    "duration_ms": 401, "deletion_confirmed": true
                },
                "events": [
                    {"at_ms": 110, "stage": "image_build", "outcome": "succeeded"},
                    {"at_ms": 501, "stage": "cleanup", "outcome": "confirmed"}
                ]
            }
        })
    );
    assert!(
        first
            .key
            .starts_with("audit/v1/instances/owner%2Fdemo/jobs/digitalocean-runner-check/0123456789abcdef0123456789abcdef/terminal/")
    );
    job["error"] = json!("changed human error");
    assert_eq!(
        diagnostic_object("owner/demo", &job).unwrap().key,
        first.key
    );
    job["health"] = json!("failed");
    assert_ne!(
        diagnostic_object("owner/demo", &job).unwrap().key,
        first.key
    );
}

#[test]
fn jobs_audit_rejects_unbounded_unstructured_or_nonterminal_records() {
    let mut job = terminal();
    assert!(diagnostic_object("../demo", &job).is_none());
    job["state"] = json!("waiting");
    assert!(diagnostic_object("demo", &job).is_none());
    job["state"] = json!("gone");
    job["events"][0]["stage"] = json!("untrusted input");
    assert!(diagnostic_object("demo", &job).is_none());
    job["events"] = json!(vec![job["events"][1].clone(); 33]);
    assert!(diagnostic_object("demo", &job).is_none());
}

#[test]
fn jobs_audit_destination_requires_a_linked_jobs_bucket() {
    use pigeon_capabilities::instances::record::{Instance, Rules, validate};

    let rules = Rules {
        platform_zone: "pigeon.dev",
        reserved_labels: &[],
        reserved_workers: &[],
        platform_worker: "pigeon-platform",
    };
    let problems = |account: Value, capabilities: Value| {
        let record: Instance = serde_json::from_value(json!({
            "id": "demo", "title": "Demo", "account": account, "capabilities": capabilities
        }))
        .unwrap();
        validate(&record, &[], &rules)
            .into_iter()
            .any(|problem| problem.contains("The audit bucket"))
    };
    let linked = json!({"kind":"linked","account_id":"00000000000000000000000000000000"});
    let jobs = json!({"jobs":{"audit_bucket":"private-audit"}});
    assert!(!problems(linked.clone(), jobs.clone()));
    assert!(problems(json!({"kind":"home"}), jobs.clone()));
    assert!(problems(json!({"kind":"linked"}), jobs));
    assert!(problems(
        linked.clone(),
        json!({"digitalocean":{"audit_bucket":"private-audit"}})
    ));
    assert!(problems(
        linked,
        json!({"jobs":{"audit_bucket":"../public"}})
    ));
}

#[test]
fn jobs_audit_default_names_do_not_mix_namespaced_and_flat_instances() {
    use pigeon_capabilities::jobs_audit::default_bucket;

    assert_eq!(default_bucket("willowgraysen"), "willowgraysen-jobs-audit");
    assert_ne!(default_bucket("owner/demo"), default_bucket("owner-demo"));
    let long = default_bucket(&"a".repeat(64));
    assert!(long.len() <= 63);
    assert!(long.ends_with("-jobs-audit"));
    for name in [long, default_bucket("owner/demo")] {
        assert!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
    }
}

#[test]
fn jobs_audit_profile_requires_console_setup_before_draft_apply() {
    use pigeon_capabilities::instances::record::{Instance, approval_problems, jobs_audit_change};

    let prior: Instance =
        serde_json::from_value(json!({"id":"demo","title":"Demo","account":{"kind":"linked"}}))
            .unwrap();
    let mut next = prior.clone();
    next.capabilities.insert("jobs".into(), Default::default());
    assert!(jobs_audit_change(Some(&prior), &next));
    assert!(
        approval_problems(&next, std::slice::from_ref(&prior))
            .iter()
            .any(|problem| problem.contains("required Jobs audit bucket"))
    );

    let legacy = next.clone();
    next.capabilities.get_mut("jobs").unwrap().audit_bucket = Some("demo-jobs-audit".into());
    assert!(jobs_audit_change(Some(&legacy), &next));
    assert!(!approval_problems(&next, std::slice::from_ref(&legacy)).is_empty());

    let mut no_setup = legacy.clone();
    no_setup.title = "Renamed title".into();
    assert!(!jobs_audit_change(Some(&legacy), &no_setup));
    assert!(approval_problems(&no_setup, &[legacy]).is_empty());

    let mut disabled = next.clone();
    disabled.capabilities.clear();
    assert!(!jobs_audit_change(Some(&next), &disabled));
}

#[test]
fn pipeline_terminal_audit_is_stable_and_scoped() {
    use pigeon_ingest::audit::{
        Cleanup, PipelineCounts, PipelineKind, PipelineOutcome, PipelineTerminal, terminal_object,
    };

    let mut terminal = PipelineTerminal {
        run_id: "run-123".into(),
        started_ms: 100,
        finished_ms: 200,
        outcome: PipelineOutcome::Succeeded,
        cleanup: Some(Cleanup::Confirmed),
        counts: PipelineCounts {
            stored: 2,
            ..PipelineCounts::default()
        },
        estimated_usd_micros: Some(5),
    };
    let object = terminal_object("owner/demo", PipelineKind::ScalewayIngest, &terminal).unwrap();
    assert!(
        object
            .key
            .starts_with("audit/v1/instances/owner%2Fdemo/jobs/scaleway-ingest/run-123/terminal/")
    );
    assert_eq!(
        object.key,
        terminal_object("owner/demo", PipelineKind::ScalewayIngest, &terminal)
            .unwrap()
            .key
    );
    let body: Value = serde_json::from_slice(&object.json).unwrap();
    assert_eq!(body["job"]["cleanup"], "confirmed");
    assert_eq!(body["job"]["counts"]["stored"], 2);
    assert!(terminal_object("../demo", PipelineKind::ScalewayIngest, &terminal).is_none());
    assert!(terminal_object("pigeon.dev", PipelineKind::EmailIngest, &terminal).is_some());
    assert!(terminal_object("pigeon..dev", PipelineKind::EmailIngest, &terminal).is_none());
    terminal.run_id = "run/123".into();
    assert!(terminal_object("owner/demo", PipelineKind::ScalewayIngest, &terminal).is_none());
}

use pigeon_capabilities::jobs_audit::diagnostic_object;
use serde_json::{Value, json};

fn terminal() -> Value {
    json!({
        "id": "0123456789abcdef0123456789abcdef",
        "state": "gone",
        "health": "passed",
        "started_ms": 100,
        "deadline_ms": 500,
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
            "version": 1,
            "instance": "owner/demo",
            "job": {
                "id": "0123456789abcdef0123456789abcdef",
                "state": "gone",
                "health": "passed",
                "started_ms": 100,
                "deadline_ms": 500,
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
            .starts_with("jobs/owner/demo/digitalocean-droplet/0123456789abcdef0123456789abcdef/")
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

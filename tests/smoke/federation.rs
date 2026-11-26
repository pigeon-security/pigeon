use pigeon_capabilities::digitalocean::health::{JOB, JOBS, STATUS};
use pigeon_capabilities::federation::{NAME, permitted};
use pigeon_capabilities::instances::record::{Instance, approval_problems};
use pigeon_capabilities::membership::Access;
use serde_json::json;

fn record(id: &str, home: bool, peer: &str, tools: &[&str]) -> Instance {
    serde_json::from_value(json!({
        "id": id,
        "title": id,
        "account": {"kind": if home { "home" } else { "linked" }},
        "version": 1,
        "capabilities": {
            NAME: {"federation": [{"peer": peer, "person": "willow", "app": "chatgpt", "tools": tools}]}
        }
    })).unwrap()
}

fn access() -> Access {
    serde_json::from_value(json!({
        "person": "willow",
        "home": "pigeon.dev",
        "instances": ["pigeon.dev", "willowgraysen"],
        "roles": {"pigeon.dev": "owner", "willowgraysen": "viewer"},
    }))
    .unwrap()
}

#[test]
fn platform_federation_requires_current_bilateral_trust_for_one_person_app_and_tool() {
    let mut home = record("pigeon.dev", true, "willowgraysen", &[JOB, JOBS]);
    let mut linked = record("willowgraysen", false, "pigeon.dev", &[JOBS, STATUS]);
    let access = access();
    let allowed = |home: &Instance, linked: &Instance, person, app, tool| {
        permitted(home, linked, &access, person, app, tool)
    };

    assert!(allowed(&home, &linked, "willow", "chatgpt", JOBS));
    assert!(!allowed(&home, &linked, "willow", "chatgpt", JOB));
    assert!(!allowed(&home, &linked, "willow", "chatgpt", STATUS));
    assert!(!allowed(
        &home,
        &linked,
        "willow",
        "chatgpt",
        "digitalocean_verify"
    ));
    assert!(!allowed(&home, &linked, "external", "chatgpt", JOBS));
    assert!(!allowed(&home, &linked, "willow", "another-app", JOBS));
    assert!(!allowed(&linked, &home, "willow", "chatgpt", JOBS));

    let remote_grant = linked.capabilities.remove(NAME).unwrap();
    assert!(!allowed(&home, &linked, "willow", "chatgpt", JOBS));
    linked.capabilities.insert(NAME.into(), remote_grant);
    home.capabilities.remove(NAME);
    assert!(!allowed(&home, &linked, "willow", "chatgpt", JOBS));
}

#[test]
fn platform_federation_uses_exact_membership_even_for_an_admin_and_blocks_draft_changes() {
    let home = record("pigeon.dev", true, "willowgraysen", &[JOBS]);
    let linked = record("willowgraysen", false, "pigeon.dev", &[JOBS]);
    let mut access = access();
    access.admin = true;
    access.roles.remove("willowgraysen");
    assert!(!permitted(
        &home, &linked, &access, "willow", "chatgpt", JOBS
    ));

    let mut disabled = linked.clone();
    disabled.capabilities.remove(NAME);
    assert!(
        approval_problems(&linked, std::slice::from_ref(&disabled))
            .iter()
            .any(|problem| problem.contains("MCP Federation"))
    );
    assert!(
        approval_problems(&disabled, &[linked])
            .iter()
            .any(|problem| problem.contains("MCP Federation"))
    );
}

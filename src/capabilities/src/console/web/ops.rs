//! The app's one read path, `/app/op`: the browser-facing tool subset. Do not accept tool
//! schemas as an allowlist: some tools that list data also change settings when given optional
//! arguments (`ai_agents_config`, `ai_agents_models`), or clean up as they read
//! (`ingest_status`), so each read lists the arguments it may take. Every op runs through
//! `dispatch` as the signed-in person, under the same scope and policy as MCP; the `app_*` ops
//! are the console's own reads. Writes go through the console's write path
//! (`console::write`); the three run writes below are the old way, kept until phase 5.

use serde_json::{Value, json};
use worker::{Response, Result};

use super::{SECTION_MS, answer, bad, failed, timed};
use crate::site_passkeys::console as site_access;
use crate::users::{Caller, User};
use crate::{Context, approvals, dispatch};

/// The reads the console may make, each with the only arguments it may be given. The called
/// tools still check their own types and ranges.
pub const READS: &[(&str, &[&str])] = &[
    // Inbox (P5): what waits, filtered by instance, and landings.
    ("app_inbox", &["instance"]),
    ("approvals_list", &[]),
    ("land_list", &["limit", "state"]),
    ("ai_agents_inbox", &["since", "brief", "report", "fields"]),
    // Instances: the list, a record's overview, its link, its capabilities.
    ("instances_list", &[]),
    ("console_manifest", &["instance"]),
    ("app_linked_details", &["instance"]),
    ("capabilities_versions", &["instance", "changelog"]),
    ("capabilities_upgrade_plan", &["instance"]),
    // Site: deployments (immutable releases and the live pointer) and visitors.
    ("site_releases", &["instance", "limit"]),
    ("app_site_hosts", &["instance"]),
    ("app_site_visitors", &["instance", "site", "host"]),
    // Storage, source, CI, metrics and ingest, scoped by their own arguments.
    ("storage_accounts", &[]),
    ("storage_list_buckets", &["account"]),
    (
        "storage_list_objects",
        &[
            "account",
            "bucket",
            "delimiter",
            "max_keys",
            "prefix",
            "token",
        ],
    ),
    ("storage_head_object", &["account", "bucket", "key"]),
    ("source_resolve_revision", &["instance", "revision"]),
    ("source_read_file", &["instance", "path", "revision"]),
    ("source_tokens", &[]),
    (
        "ci_jobs",
        &["caller", "commit", "limit", "repo", "state", "task"],
    ),
    ("ci_status", &["id"]),
    ("ci_pool", &[]),
    ("observability_summary", &["hours"]),
    ("observability_errors", &["hours", "limit", "worker"]),
    ("observability_worker_metrics", &["instance", "hours"]),
    ("ingest_report", &["instance", "run"]),
    // Sessions, cost and compare.
    (
        "ai_agents_status",
        &["run", "limit", "live", "turns", "brief", "report", "fields"],
    ),
    ("ai_agents_compare", &["days", "role"]),
    ("cost_daily", &["range"]),
    // Access: secrets (names and state only), tokens, people, passkeys, apps.
    ("secrets_list", &[]),
    ("tokens_status", &[]),
    ("users_people", &[]),
    ("users_list", &[]),
    ("passkeys_list", &["user_id"]),
    ("oauth_clients", &[]),
    ("users_whoami", &[]),
    // Platform: architecture, models (read with no `set`), limits (read with no arguments).
    ("architecture_describe", &["public", "svg"]),
    ("ai_agents_models", &[]),
    ("ai_agents_config", &[]),
];

/// The arguments read `name` may take, when it is one.
pub fn read_keys(name: &str) -> Option<&'static [&'static str]> {
    READS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, keys)| *keys)
}

/// The pending items of an `approvals_list` answer that concern `instance`; landings only when
/// `home` (the instance the platform runs in), since only it lands.
pub fn inbox_for(mut listing: Value, instance: &str, home: bool) -> Value {
    let of = |v: &Value| {
        [
            &v["args"]["instance"],
            &v["instance"],
            &v["summary"]["instance"],
            &v["summary"]["args"]["instance"],
        ]
        .into_iter()
        .any(|i| i.as_str() == Some(instance))
    };
    for list in ["calls", "drafts", "plans", "other"] {
        if let Some(items) = listing[list].as_array_mut() {
            items.retain(of);
        }
    }
    if !home && let Some(items) = listing["landings"].as_array_mut() {
        items.clear();
    }
    listing
}

/// The op allowlist: whether `/app/op` runs `name` with `args`.
pub fn allowed(name: &str, args: &Value) -> bool {
    let Some(object) = args.as_object() else {
        return false;
    };
    if object.get(approvals::ARG).is_some_and(|v| {
        !v.as_str()
            .is_some_and(|id| !id.is_empty() && id.len() <= 128)
    }) {
        return false;
    }
    let only = |keys: &[&str]| {
        object
            .keys()
            .all(|key| key == approvals::ARG || keys.contains(&key.as_str()))
    };
    if let Some(keys) = read_keys(name) {
        return only(keys);
    }
    // Only one run per write. No bulk stop, event impersonation, caller override or alias keys.
    if !args["run"].as_str().is_some_and(|run| {
        !run.is_empty()
            && run.len() <= 64
            && run
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return false;
    }
    match name {
        "ai_agents_stop" => only(&["run"]),
        "ai_agents_note" => {
            only(&["run", "text"])
                && args["text"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty() && text.len() <= 4000)
        }
        "ai_agents_extend" => {
            const AMOUNTS: &[&str] = &["cost_usd", "turns", "wall_min", "sandbox_min", "executors"];
            only(&[
                "run",
                "cost_usd",
                "turns",
                "wall_min",
                "sandbox_min",
                "executors",
            ]) && AMOUNTS.iter().any(|key| object.contains_key(*key))
                && AMOUNTS.iter().all(|key| {
                    object.get(*key).is_none_or(|v| {
                        if *key == "cost_usd" {
                            v.as_f64().is_some_and(|n| n > 0.0)
                        } else {
                            v.as_u64().is_some_and(|n| n > 0)
                        }
                    })
                })
        }
        _ => false,
    }
}

// Read only this instance. Never fall back to a person's other link or the platform's stores.
async fn linked_details(cx: &Context<'_>, args: &Value) -> Result<Value> {
    use crate::instances::record::{AccountRef, worker_for};
    let id = args["instance"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| bad("expected a linked instance ID"))?;
    let Some(record) = crate::instances::one(cx.env, id).await? else {
        return Err(bad("no such linked instance"));
    };
    let AccountRef::Linked { account_id } = &record.account else {
        return Err(bad("expected a linked instance"));
    };
    let link = crate::linked::link(cx.env, id).await?;
    let status = match link.as_ref() {
        Some(link)
            if link.instance != id
                || account_id.as_ref().is_some_and(|id| id != &link.account_id) =>
        {
            json!({ "status": "account mismatch" })
        }
        Some(link) => json!({
            "status": if link.failed.is_some() { "refresh failed" } else { "linked" },
            "account_name": link.account_name,
            "account_id": link.account_id,
            "linked_ms": link.linked_ms,
            "scope": link.scope,
            "access_expires_ms": link.expires_at_ms,
            "fresh": link.expires_at_ms > worker::js_sys::Date::now(),
        }),
        None => json!({ "status": "not linked" }),
    };
    Ok(json!({
        "id": id,
        "link": status,
        "worker": record.worker.clone().or_else(|| worker_for(&record).ok().flatten()),
    }))
}

/// The inbox for `args.instance`, or all of it.
async fn inbox(cx: &Context<'_>, args: &Value) -> Result<Value> {
    let listing = dispatch(cx, "approvals_list", &serde_json::json!({})).await?;
    let Some(id) = args["instance"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(listing);
    };
    let home = crate::instances::one(cx.env, id)
        .await?
        .is_some_and(|r| r.account == crate::instances::record::AccountRef::Home);
    Ok(inbox_for(listing, id, home))
}

/// Called after the console's same-origin JSON and standard-person session checks.
pub(super) async fn serve(
    cx: &Context<'_>,
    host: &str,
    person: User,
    body: &Value,
) -> Result<Response> {
    let Some(name) = body["name"].as_str().filter(|_| {
        body.as_object()
            .is_some_and(|o| o.len() == 2 && o.contains_key("args"))
    }) else {
        return failed(&bad("expected { name, args }"));
    };
    let args = &body["args"];
    if !allowed(name, args) {
        return answer(
            &json!({ "error": "operation or arguments not allowed in the app" }),
            403,
        );
    }
    let caller = Caller {
        user: person,
        acting_for: None,
        run: None,
    };
    let person_cx = Context {
        account: cx.account,
        env: cx.env,
        hosted: cx.hosted,
        caller: Some(&caller),
        caller_note: None,
        instance: None,
        worker: cx.worker,
    };
    let call = async {
        match name {
            "app_linked_details" => linked_details(&person_cx, args).await,
            "app_inbox" => inbox(&person_cx, args).await,
            "app_site_hosts" => {
                site_access::serve(host, cx.env, site_access::Action::Hosts, args).await
            }
            "app_site_visitors" => site_access::visitors(host, cx.env, args).await,
            _ => dispatch(&person_cx, name, args).await,
        }
    };
    // A read can time out. Do not put a local timeout around a write that might still commit.
    let result = if read_keys(name).is_some() {
        timed(SECTION_MS, call).await
    } else {
        call.await
    };
    match result {
        Ok(data) => answer(&json!({ "data": data }), 200),
        Err(e) => failed(&e),
    }
}

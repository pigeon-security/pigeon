//! The app's browser-facing tool subset. Do not accept tool schemas as an allowlist: some
//! tools that list data also change settings when given optional arguments.

use serde_json::{Value, json};
use worker::{Response, Result};

use super::{SECTION_MS, answer, bad, failed, timed};
use crate::users::{Caller, User};
use crate::{Context, approvals, dispatch};

// Keys for read operations only. The called tools still check their own types and ranges.
fn read_keys(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "ai_agents_status" => &["run", "limit", "live", "turns", "brief", "report", "fields"],
        "land_list" => &["limit", "state"],
        "cost_daily" => &["range"],
        "capabilities_versions" => &["instance", "changelog"],
        "capabilities_upgrade_plan" | "app_linked_details" => &["instance"],
        "architecture_describe" => &["public", "svg"],
        "ai_agents_compare" => &["days", "role"],
        "tokens_status" | "oauth_clients" | "users_people" | "secrets_list" | "instances_list"
        | "ai_agents_models" | "users_whoami" => &[],
        _ => return None,
    })
}

fn allowed(name: &str, args: &Value) -> bool {
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

/// Called after the console's same-origin JSON and standard-person session checks.
pub(super) async fn serve(cx: &Context<'_>, person: User, body: &Value) -> Result<Response> {
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

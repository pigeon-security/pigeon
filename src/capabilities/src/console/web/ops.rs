//! The app's one read path, `/app/op`: the browser-facing tool subset. Do not accept tool
//! schemas as an allowlist: some tools that list data also change settings when given optional
//! arguments (`ai_agents_config`, `ai_agents_models`), or clean up as they read
//! (`ingest_status`), so each read lists the arguments it may take. Every op runs through
//! `dispatch` as the signed-in person (or, for the iOS app by bearer, the person its token acts
//! for, run exactly as that person: [`super::app_person`]), under the same scope and policy as
//! MCP; the `app_*` ops are the console's own reads. Nothing here writes: every write, a run's stop, note and
//! extension included, goes through the console's write path (`console::write`).

use serde_json::{Value, json};
use worker::{Response, Result};

use super::{READ_MS, answer, bad, failed, timed};
use crate::membership::{self, Role};
use crate::site_passkeys::console as site_access;
use crate::users::{Caller, User, directory};
use crate::{Context, approvals, dispatch, dispatch_checked};

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
    (crate::digitalocean::STATUS, &["instance"]),
    (crate::digitalocean::VERIFY, &["instance"]),
    (crate::digitalocean::spaces::STATUS, &["instance"]),
    (crate::digitalocean::spaces::VERIFY, &["instance"]),
    ("capabilities_versions", &["instance", "changelog"]),
    ("capabilities_upgrade_plan", &["instance"]),
    // Site: deployments (immutable releases and the live pointer) and visitors.
    ("site_releases", &["instance", "limit"]),
    ("app_site_hosts", &["instance"]),
    ("app_site_visitors", &["instance", "site", "host"]),
    // Storage, source, CI, metrics and ingest, scoped by their own arguments. With `instance`,
    // an op in `INSTANCE_OPS` is bound to that instance's approved record first.
    ("storage_accounts", &[]),
    ("storage_list_buckets", &["account"]),
    (
        "storage_list_objects",
        &[
            "account",
            "bucket",
            "delimiter",
            "instance",
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
        &[
            "caller", "commit", "instance", "limit", "repo", "state", "task",
        ],
    ),
    ("ci_status", &["id", "instance"]),
    ("ci_pool", &[]),
    ("observability_summary", &["hours"]),
    ("observability_errors", &["hours", "limit", "worker"]),
    ("observability_worker_metrics", &["instance", "hours"]),
    ("ingest_report", &["instance", "run"]),
    // Ingest › Runs lists an instance's runs here: `ingest_status` would also delete the VM of
    // a run that has ended, so the console never calls it.
    ("app_ingest_runs", &["instance"]),
    // Sessions, cost and compare.
    (
        "ai_agents_status",
        &["run", "limit", "live", "turns", "brief", "report", "fields"],
    ),
    ("ai_agents_compare", &["days", "role"]),
    ("cost_daily", &["range"]),
    // Access: secrets (names and state only), tokens, people, passkeys, apps.
    ("secrets_list", &["instance"]),
    ("credentials_expiry", &[]),
    ("app_proxy_tokens", &[]),
    ("app_people", &[]),
    ("tokens_status", &[]),
    ("users_people", &[]),
    ("users_list", &[]),
    ("passkeys_list", &["user_id"]),
    ("oauth_clients", &[]),
    ("users_whoami", &[]),
    // Platform: architecture (of the selected instance), models (read with no `set`), limits
    // (read with no arguments).
    ("architecture_describe", &["public", "svg", "instance"]),
    // The map itself, drawn for the selected instance in each theme and width.
    ("architecture_draw", &["instance", "mode", "width", "theme"]),
    ("ai_agents_models", &[]),
    ("ai_agents_config", &[]),
];

/// The reads that are an instance tool's registration (`instances::tools`). Called with an
/// `instance`, each is bound to that instance's approved record as the instance MCP endpoint
/// binds it ([`bind_global`](crate::instances::tools::bind_global)): refused when the record
/// does not enable it or an argument falls outside its registration, such as a raw storage
/// `account` or a CI `repo`.
pub const INSTANCE_OPS: &[&str] = &[
    "storage_list_objects",
    "source_resolve_revision",
    "source_read_file",
    "ci_jobs",
    "ci_status",
    "observability_worker_metrics",
    "ingest_report",
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

/// For one of the console's own `app_*` reads, the instance it is about (`None`: the home
/// instance), whose viewer role it needs; `None` for a tool read, which `dispatch_checked`
/// checks, and for the whole inbox, which lists only what the person may decide.
pub fn app_read_instance<'a>(name: &str, args: &'a Value) -> Option<Option<&'a str>> {
    let instance = args["instance"].as_str().filter(|id| !id.is_empty());
    match name {
        "app_inbox" if instance.is_none() => None,
        n if n.starts_with("app_") => Some(instance),
        _ => None,
    }
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
    read_keys(name).is_some_and(only)
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

/// `args` bound to the approved record of the instance they name, for an op in
/// [`INSTANCE_OPS`]; any other call's arguments unchanged.
async fn bound(cx: &Context<'_>, name: &str, args: &Value) -> Result<Value> {
    let Some(id) = args
        .get("instance")
        .filter(|_| INSTANCE_OPS.contains(&name))
    else {
        return Ok(args.clone());
    };
    let id = id
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| bad("`instance` must be an instance id"))?;
    let record = crate::instances::one(cx.env, id).await?;
    crate::instances::tools::bind_global(id, record.as_ref(), name, args).map_err(|why| bad(&why))
}

/// Ingest › Runs: the runs of one instance whose record enables ingest. Reads only.
async fn ingest_runs(cx: &Context<'_>, args: &Value) -> Result<Value> {
    let id = args["instance"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| bad("expected an instance ID"))?;
    let record = crate::instances::one(cx.env, id).await?;
    // The instance's own ingest_report must be available to it, as on its MCP endpoint.
    crate::instances::tools::bind_global(id, record.as_ref(), "ingest_report", &json!({}))
        .map_err(|why| bad(&why))?;
    Ok(json!({ "instance": id, "runs": crate::ingest::runs(cx.env, id).await? }))
}

/// Access › Tokens: the live proxy credentials and those that ended in the last two weeks, by
/// id, label, who issued each and when each ends. Never a value or a hash.
async fn proxy_tokens(cx: &Context<'_>) -> Result<Value> {
    let tokens = directory(
        cx.env,
        "proxy.tokens",
        json!({ "expired_within_ms": crate::expiry::EXPIRED_SHOWN_MS }),
    )
    .await?;
    Ok(json!({ "tokens": tokens }))
}

/// Access › People and Apps: each person with their passkeys (no key material) and whether
/// they are an admin, each app or service user with the person it acts for, its live token
/// count and its kind, and who is asking and whether they are an admin.
async fn people(cx: &Context<'_>, me: &str) -> Result<Value> {
    let all = directory(cx.env, "people.list", json!({})).await?;
    let clients = directory(cx.env, "oauth.clients", json!({})).await?;
    let list = |v: &Value, k: &str| v[k].as_array().cloned().unwrap_or_default();
    let oauth: Vec<&str> = clients
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["service_user_id"].as_str())
        .collect();
    let passkeys = list(&all, "passkeys");
    let people: Vec<Value> = list(&all, "people")
        .iter()
        .map(|p| {
            let name = p["name"].as_str().unwrap_or_default();
            json!({
                "id": p["id"],
                "name": name,
                "initials": crate::users::initials(name),
                "created_at": p["created_at"],
                "admin": p["role"] == crate::users::ADMIN,
                "passkeys": passkeys.iter().filter(|k| k["user_id"] == p["id"]).collect::<Vec<_>>(),
            })
        })
        .collect();
    let apps: Vec<Value> = list(&all, "apps")
        .into_iter()
        .map(|mut a| {
            let app = a["id"].as_str().is_some_and(|id| oauth.contains(&id));
            a["kind"] = json!(if app { "app" } else { "service" });
            a
        })
        .collect();
    // Whether the one asking is an admin, so the view offers only what they may do; every
    // write checks again.
    let me_admin = people.iter().any(|p| p["id"] == me && p["admin"] == true);
    // Per-instance roles (docs/plans/membership.md), on the instances the one asking may see;
    // `manage` lists those where they may change them (an admin's: all; an owner's: theirs).
    let access = membership::access(cx.env, me).await?;
    let members = directory(cx.env, "members.all", json!({})).await?;
    let (instances, people) = with_roles(&access, people, members.as_array().map_or(&[], |v| v));
    let manage: Vec<&String> = instances
        .iter()
        .filter(|i| access.may_manage(i).is_ok())
        .collect();
    Ok(json!({
        "me": me, "me_admin": me_admin, "people": people, "apps": apps,
        "instances": instances, "manage": manage,
    }))
}

/// The instances `access` may see, and `people` each with `roles`: their role on each of those
/// instances, from the `members` rows (`instance`, `person_id`, `role`); an admin is `owner`
/// everywhere.
pub fn with_roles(
    access: &membership::Access,
    mut people: Vec<Value>,
    members: &[Value],
) -> (Vec<String>, Vec<Value>) {
    let instances: Vec<String> = access
        .visible(access.instances.iter().map(String::as_str))
        .into_iter()
        .map(str::to_string)
        .collect();
    for p in &mut people {
        let admin = p["admin"] == true;
        let roles: serde_json::Map<String, Value> = instances
            .iter()
            .filter_map(|i| {
                let role = if admin {
                    Some(Role::Owner.name())
                } else {
                    members
                        .iter()
                        .find(|m| m["instance"] == i.as_str() && m["person_id"] == p["id"])
                        .and_then(|m| m["role"].as_str())
                };
                role.map(|r| (i.clone(), json!(r)))
            })
            .collect();
        p["roles"] = Value::Object(roles);
    }
    (instances, people)
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

/// Called after the console's JSON check and its person check: a same-origin request with a
/// standard person's session, or an app's bearer acting for a standard person.
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
    // An old id during R1's compat window reads as the id it was renamed to.
    let current = match crate::instances::current_args(cx.env, args).await {
        Ok(c) => c,
        Err(e) => return failed(&e),
    };
    let args = current.as_ref().unwrap_or(args);
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
        // The console's own reads need viewer on their instance, or on home for a platform-wide
        // one (docs/plans/membership.md); a tool read is checked by `dispatch_checked`.
        if let Some(instance) = app_read_instance(name, args) {
            membership::require(cx.env, &caller.user, instance, Role::Viewer).await?;
        }
        match name {
            "app_linked_details" => linked_details(&person_cx, args).await,
            "app_inbox" => inbox(&person_cx, args).await,
            "app_proxy_tokens" => proxy_tokens(&person_cx).await,
            "app_people" => people(&person_cx, &caller.user.id).await,
            "app_site_hosts" => site_access::site_hosts(host, cx.env, args).await,
            "app_site_visitors" => site_access::visitors(host, cx.env, args).await,
            "app_ingest_runs" => ingest_runs(&person_cx, args).await,
            crate::digitalocean::STATUS
            | crate::digitalocean::VERIFY
            | crate::digitalocean::spaces::STATUS
            | crate::digitalocean::spaces::VERIFY => {
                let id = args["instance"]
                    .as_str()
                    .ok_or_else(|| bad("expected an instance ID"))?;
                let record = crate::instances::one(cx.env, id)
                    .await?
                    .ok_or_else(|| bad("there is no approved instance"))?;
                let scope = crate::instances::tools::Scope::new(&record)?;
                let scoped = Context {
                    instance: Some(&scope),
                    ..person_cx
                };
                dispatch_checked(&scoped, name, args).await
            }
            _ => {
                let args = bound(&person_cx, name, args).await?;
                dispatch_checked(&person_cx, name, &args).await
            }
        }
    };
    match timed(READ_MS, call).await {
        Ok(data) => answer(&json!({ "data": data }), 200),
        Err(e) => failed(&e),
    }
}

//! Site › Visitors: the console's reads and writes for the visitors of one approved and applied
//! site hostname. The reads are `/app/op` ops and the writes `/console/write` actions
//! (`visitor-invite`, `visitor-revoke`); both run after the console's session checks.

use pigeon_core::site_auth;
use serde_json::{Value, json};
use worker::{Env, Result};

use super::{admit_new_visitors, allows_new_visitors, applied, approved, bad};
use crate::instances::{self, record};
use crate::users::{User, Users, directory, invite_name};

/// Resolve the exact selected instance, site, and hostname.
pub fn matches_selection(scope: &str, instance: &str, site: &str, host: &str) -> bool {
    scope == site_auth::binding(instance, site, host)
}

/// Site › Visitors' hostnames: those of `args.instance`'s passkey sites that are approved and
/// applied, each with its site. Called by `/app/op` (`app_site_hosts`) after the console's
/// session checks.
pub(crate) async fn site_hosts(platform_host: &str, env: &Env, args: &Value) -> Result<Value> {
    let instance = crate::arg(args, "instance")?;
    Ok(json!({ "hosts": hosts(env, zone(platform_host)?, instance).await? }))
}

fn zone(platform_host: &str) -> Result<&str> {
    platform_host
        .split_once('.')
        .map(|(_, zone)| zone)
        .ok_or_else(|| bad("platform host has no zone"))
}

/// The approved scope and config of a selection `{instance, site, host}`, which must name the
/// site its hostname is approved for.
pub(crate) async fn selection(
    platform_host: &str,
    env: &Env,
    target: &Value,
) -> Result<(String, site_auth::Config)> {
    let field = |k: &str| target[k].as_str().unwrap_or_default();
    let (scope, config) = approved(env, zone(platform_host)?, field("host")).await?;
    if !matches_selection(&scope, field("instance"), field("site"), field("host")) {
        return Err(bad("the hostname does not belong to the selected site"));
    }
    Ok((scope, config))
}

/// The visitors of a selection, and whether new ones may be invited.
pub(crate) async fn visitors(platform_host: &str, env: &Env, target: &Value) -> Result<Value> {
    let (scope, config) = selection(platform_host, env, target).await?;
    let host = target["host"].as_str().unwrap_or_default();
    let visitors = directory(
        env,
        "users.site_visitors",
        json!({ "kind": format!("site:{host}"), "site_scope": scope }),
    )
    .await?;
    Ok(json!({ "visitors": visitors, "can_invite": allows_new_visitors(&config) }))
}

/// Checks a visitor invitation can be made, before the passkey.
pub(crate) async fn invite_check(platform_host: &str, env: &Env, target: &Value) -> Result<()> {
    let (_, config) = selection(platform_host, env, target).await?;
    admit_new_visitors(&config)
}

/// Invites visitor `name` to a selection, approved by a passkey bound to both: the selection
/// is checked again, in case it changed since the write began.
pub(crate) async fn invite(
    platform_host: &str,
    env: &Env,
    target: &Value,
    name: &str,
) -> Result<Value> {
    let (scope, config) = selection(platform_host, env, target).await?;
    if !allows_new_visitors(&config) {
        return Err(bad("site access changed; start the invitation again"));
    }
    let name = invite_name(name).map_err(|why| bad(&why))?;
    let host = target["host"].as_str().unwrap_or_default();
    let invite = Users {
        host: platform_host.to_owned(),
    }
    .enrol_new_user(env, &name, Some(host), Some(&scope))
    .await?;
    // `reveal` is what the console's confirm dialog shows once (P3).
    Ok(json!({
        "message": format!("{name} is invited. Send them the link: it works once."),
        "reveal": {
            "value": invite["enrol"],
            "kind": "invite",
            "note": format!(
                "Send this link to {name}. It works once, within {} minutes.",
                invite["expires_in_minutes"]
            ),
        },
    }))
}

/// Removes visitor `user` of a selection, with their passkeys: only a visitor of exactly this
/// hostname and approved scope.
pub(crate) async fn revoke(
    platform_host: &str,
    env: &Env,
    target: &Value,
    user: &str,
) -> Result<Value> {
    let (scope, _) = selection(platform_host, env, target).await?;
    let host = target["host"].as_str().unwrap_or_default();
    let found: Option<User> =
        serde_json::from_value(directory(env, "users.get", json!({ "id": user })).await?)?;
    let found = found.ok_or_else(|| bad("no such visitor"))?;
    let visitor_scope = directory(env, "users.site_scope", json!({ "id": user })).await?;
    if found.kind != format!("site:{host}") || visitor_scope.as_str() != Some(scope.as_str()) {
        return Err(bad("this user is not a visitor of the selected site"));
    }
    directory(env, "users.remove", json!({ "id": user })).await?;
    Ok(json!({ "message": format!("{} can no longer sign in to {host}.", found.name) }))
}

async fn hosts(env: &Env, zone: &str, instance: &str) -> Result<Vec<Value>> {
    let instances = instances::all(env).await?;
    let selected = instances
        .iter()
        .find(|record| record.id == instance)
        .ok_or_else(|| bad("no such instance"))?;
    let mut out = Vec::new();
    for site in &selected.sites {
        if site.auth.as_deref() != Some("passkey") {
            continue;
        }
        for target in &site.targets {
            let record::Target::Subdomain { label } = target else {
                continue;
            };
            let host = record::subdomain_host(label, zone);
            if record::site_for_host(&instances, &host, zone)
                .is_some_and(|(owner, matched)| owner.id == instance && matched.name == site.name)
                && applied(env, &host).await?
            {
                out.push(json!({ "host": host, "site": site.name }));
            }
        }
    }
    Ok(out)
}

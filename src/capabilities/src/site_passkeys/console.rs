//! Cookie-authenticated visitor management for one approved and applied site hostname.

use pigeon_core::{oauth::hash, site_auth};
use serde_json::{Value, json};
use worker::{Env, Result};

use super::{applied, approved, bad};
use crate::instances::{self, record};
use crate::passkeys::Passkeys;
use crate::users::{User, Users, directory, invite_name};

/// The visitor UI asset mounted after the console shell.
pub(crate) const ASSET: (&str, &str, &str) = (
    "/app/site_access.js",
    "text/javascript; charset=utf-8",
    include_str!("visitor.js"),
);

/// Visitor management actions exposed to the signed console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Hosts,
    List,
    InviteBegin,
    InviteFinish,
}

impl Action {
    pub fn from_path(path: &str) -> Option<Self> {
        match path.strip_prefix("/console/site-access/")? {
            "hosts" => Some(Self::Hosts),
            "list" => Some(Self::List),
            "invite/begin" => Some(Self::InviteBegin),
            "invite/finish" => Some(Self::InviteFinish),
            _ => None,
        }
    }
}

/// Resolve the exact selected instance, site, and hostname.
pub fn matches_selection(scope: &str, instance: &str, site: &str, host: &str) -> bool {
    scope == site_auth::binding(instance, site, host)
}

/// Bind a visitor invitation approval to the approved site scope and normalized name.
pub fn invite_purpose(scope: &str, name: &str, person: &str) -> String {
    format!("console:site-invite:{scope}:{person}:{}", hash(name))
}

/// Runs after the console authenticates a same-origin request by a signed-in person.
pub(crate) async fn serve(
    platform_host: &str,
    passkeys: &Passkeys,
    env: &Env,
    action: Action,
    person: &User,
    body: &Value,
) -> Result<Value> {
    let instance = crate::arg(body, "instance")?;
    let (_, zone) = platform_host
        .split_once('.')
        .ok_or_else(|| bad("platform host has no zone"))?;
    if action == Action::Hosts {
        return Ok(json!({ "hosts": hosts(env, zone, instance).await? }));
    }
    let site = crate::arg(body, "site")?;
    let host = crate::arg(body, "host")?;
    let (scope, config) = approved(env, zone, host).await?;
    if !matches_selection(&scope, instance, site, host) {
        return Err(bad("the hostname does not belong to the selected site"));
    }

    let can_invite = allows_new_visitors(&config);
    if action == Action::List {
        let visitors = directory(
            env,
            "users.site_visitors",
            json!({ "kind": format!("site:{host}"), "site_scope": scope }),
        )
        .await?;
        return Ok(json!({ "visitors": visitors, "can_invite": can_invite }));
    }
    if !can_invite {
        return Err(bad("the site's allowlist does not admit new site visitors"));
    }

    let name = invite_name(crate::arg(body, "name")?).map_err(|why| bad(&why))?;
    let purpose = invite_purpose(&scope, &name, &person.id);
    match action {
        Action::InviteBegin => {
            passkeys
                .sign_in_begin(env, &purpose, Some(&person.id))
                .await
        }
        Action::InviteFinish => {
            passkeys
                .sign_in_finish(env, &purpose, body, Some(&person.id))
                .await?;
            let (current_scope, current_config) = approved(env, zone, host).await?;
            if current_scope != scope || !allows_new_visitors(&current_config) {
                return Err(bad("site access changed; start the invitation again"));
            }
            let invite = Users {
                host: platform_host.to_owned(),
            }
            .enrol_new_user(env, &name, Some(host), Some(&scope))
            .await?;
            Ok(json!({
                "enrol": invite["enrol"],
                "expires_in_minutes": invite["expires_in_minutes"],
            }))
        }
        _ => Err(bad("not found")),
    }
}

fn allows_new_visitors(config: &site_auth::Config) -> bool {
    config.allow.iter().any(|entry| entry == "site")
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

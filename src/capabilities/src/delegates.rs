//! Delegates: an app acting for a person. One model for both ways an app reaches pigeon.
//!
//! - An **OAuth app** registers itself (`/oauth/register`, RFC 7591). Each registration is an
//!   OAuth client with its own service user, so one real app can have many: Pigeon for iOS
//!   registers again after every sign-out, and the Cloudflare MCP Portal registers once from
//!   the dashboard and once for its own server connection.
//! - A **service user** is made from a service id (`service_ids`): an Access service token, or a
//!   key pigeon names itself, such as the AI agents' `ai-agent:planner` and `ai-agent:executor`.
//!
//! Either way the service user is the unit that holds tokens and is linked to a person
//! (`links`). This module calls each such service user a **grant**, and rolls grants up into
//! **apps**: one entry per real app and person. Nothing is stored for the roll-up; it is
//! computed from the rows the Directory already keeps, in this order, first match wins:
//!
//! 1. an admin's override (`app_assignments`), naming one of [`KNOWN`] or `own`;
//! 2. the grant's service id, for apps pigeon makes itself (the AI agents);
//! 3. the client's redirect URIs: a host the app owns (`chatgpt.com` is OpenAI's);
//! 4. the client's name, only when its redirects name no host of their own: loopback (a CLI,
//!    such as Codex), the platform's own page for its iOS app (`/app/oauth`) or the portal's
//!    callback (`/servers-callback`), or no client left at all;
//! 5. otherwise the app is its redirect host, or its name, on its own.
//!
//! A grant acting for no one is grouped the same way, under no person.
//!
//! The functions here are pure; the Directory reads the rows ([`GRANTS_SQL`]) and applies the
//! changes ([`DISCONNECT_GRANT`], `users::REMOVE_USER`).

use serde_json::{Value, json};

/// One role a grant can have within an app, and what it may do, in plain words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
}

/// A real app pigeon knows, and how its grants are recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownApp {
    /// A stable key: what an override names.
    pub key: &'static str,
    pub label: &'static str,
    pub vendor: &'static str,
    /// Redirect hosts the app owns: a client redirecting to one is this app, whatever its name.
    pub hosts: &'static [&'static str],
    /// Redirect paths, on any host, that this app uses with one of `names`.
    pub paths: &'static [&'static str],
    /// The names its clients register with, compared without case.
    pub names: &'static [&'static str],
    /// Service ids, each with the role it gives.
    pub service_keys: &'static [(&'static str, Role)],
}

const PLANNER: Role = Role {
    id: "planner",
    label: "Planner",
    note: "Plans a run, starts executor runs and may land.",
};

const EXECUTOR: Role = Role {
    id: "executor",
    label: "Executor",
    note: "Edits and tests on its run's branch; never lands.",
};

/// The apps pigeon knows. A client none of these matches still gets one entry per redirect host
/// or name, and an admin can put it under one of these with an override.
pub const KNOWN: &[KnownApp] = &[
    KnownApp {
        key: "pigeon-ios",
        label: "Pigeon for iOS",
        vendor: "Pigeon",
        hosts: &[],
        paths: &["/app/oauth"],
        names: &["Pigeon for iOS"],
        service_keys: &[],
    },
    KnownApp {
        key: "ai-gateway",
        label: "AI Gateway",
        vendor: "Pigeon",
        hosts: &[],
        paths: &[],
        names: &[],
        service_keys: &[
            ("ai-agent:planner", PLANNER),
            ("ai-agent:executor", EXECUTOR),
        ],
    },
    KnownApp {
        key: "cloudflare-portal",
        label: "Cloudflare MCP Portal",
        vendor: "Cloudflare",
        hosts: &["dash.cloudflare.com", "one.dash.cloudflare.com"],
        paths: &["/servers-callback"],
        names: &["Cloudflare MCP Portal"],
        service_keys: &[],
    },
    KnownApp {
        key: "claude",
        label: "Claude",
        vendor: "Anthropic",
        hosts: &["claude.ai", "claude.com"],
        paths: &[],
        names: &["Claude", "Claude Code", "Claude Desktop"],
        service_keys: &[],
    },
    KnownApp {
        key: "openai",
        label: "OpenAI",
        vendor: "OpenAI",
        hosts: &[
            "chatgpt.com",
            "chat.openai.com",
            "openai.com",
            "platform.openai.com",
        ],
        paths: &[],
        names: &["ChatGPT", "Codex", "OpenAI Codex"],
        service_keys: &[],
    },
    KnownApp {
        key: "grokbot",
        label: "Grokbot",
        vendor: "xAI",
        hosts: &["grok.com", "x.ai", "accounts.x.ai"],
        paths: &[],
        names: &["Grok", "Grokbot", "Grok Bot", "Grok CLI"],
        service_keys: &[],
    },
];

/// The override that keeps a grant on its own, whatever it matches.
pub const OWN: &str = "own";

/// The known app `key`.
pub fn known(key: &str) -> Option<&'static KnownApp> {
    KNOWN.iter().find(|a| a.key == key)
}

/// Whether `key` is an override an admin may set: a known app, or [`OWN`].
pub fn override_allowed(key: &str) -> bool {
    key == OWN || known(key).is_some()
}

/// One grant's row from the Directory ([`GRANTS_SQL`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grant {
    /// The service user's id.
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub client_id: Option<String>,
    pub redirect_uris: Vec<String>,
    pub service_key: Option<String>,
    /// The person it acts for.
    pub person: Option<String>,
    pub linked_at: Option<i64>,
    /// Live tokens it holds, proxy credentials aside.
    pub tokens: i64,
    /// When it was last issued a token: an access token comes with each sign-in and refresh, so
    /// this is about when it was last used.
    pub last_used_at: Option<i64>,
    /// An admin's override (`app_assignments`).
    pub app_override: Option<String>,
}

/// SQLite's integers may arrive as floats.
fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

impl Grant {
    /// A grant from a [`GRANTS_SQL`] row.
    pub fn from_row(r: &Value) -> Grant {
        let text = |k: &str| r[k].as_str().map(str::to_string);
        Grant {
            id: text("id").unwrap_or_default(),
            name: text("name").unwrap_or_default(),
            created_at: int(&r["created_at"]).unwrap_or(0),
            client_id: text("client_id"),
            redirect_uris: r["redirect_uris"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            service_key: text("service_key"),
            person: text("person"),
            linked_at: int(&r["linked_at"]),
            tokens: int(&r["tokens"]).unwrap_or(0),
            last_used_at: int(&r["last_used_at"]),
            app_override: text("app_override"),
        }
    }

    /// `oauth` (it has an OAuth client), `service` (a service id) or `leftover` (neither: what
    /// stays of a client that was revoked).
    pub fn kind(&self) -> &'static str {
        if self.client_id.is_some() {
            "oauth"
        } else if self.service_key.is_some() {
            "service"
        } else {
            "leftover"
        }
    }
}

/// The host of `uri` and its path, lowercased host, when it is an http(s) URI.
fn host_path(uri: &str) -> Option<(String, String)> {
    let rest = uri
        .strip_prefix("https://")
        .or_else(|| uri.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, h)| h)
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path = rest[end..]
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    Some((host, path))
}

/// Whether `host` is the loopback or localhost, as a CLI's redirect is.
fn loopback(host: &str) -> bool {
    host == "localhost" || host == "[::1]" || host.starts_with("127.")
}

/// Whether `host` is `owned` or under it.
fn under(host: &str, owned: &str) -> bool {
    host == owned || host.ends_with(&format!(".{owned}"))
}

/// Which app a grant belongs to, and how it was told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// The app's key: a [`KNOWN`] key, or `host:{host}`, `name:{name}` or `own:{id}`.
    pub key: String,
    pub label: String,
    pub vendor: Option<&'static str>,
    pub role: Option<Role>,
    /// `override`, `service id`, `redirect`, `name` or `own`.
    pub by: &'static str,
}

impl Match {
    fn known(app: &KnownApp, role: Option<Role>, by: &'static str) -> Match {
        Match {
            key: app.key.into(),
            label: app.label.into(),
            vendor: Some(app.vendor),
            role,
            by,
        }
    }
}

/// The app grant `g` belongs to ([module docs](self)).
pub fn app_of(g: &Grant) -> Match {
    let role = g.service_key.as_deref().and_then(|k| {
        KNOWN
            .iter()
            .flat_map(|a| a.service_keys.iter())
            .find(|(key, _)| *key == k)
            .map(|(_, r)| *r)
    });
    // 1. An admin's override.
    match g.app_override.as_deref() {
        Some(OWN) => {
            return Match {
                key: format!("own:{}", g.id),
                label: g.name.clone(),
                vendor: None,
                role,
                by: "override",
            };
        }
        Some(key) => {
            if let Some(app) = known(key) {
                return Match::known(app, role, "override");
            }
        }
        None => {}
    }
    // 2. A service id pigeon names itself.
    if let Some(k) = g.service_key.as_deref()
        && let Some(app) = KNOWN
            .iter()
            .find(|a| a.service_keys.iter().any(|(key, _)| *key == k))
    {
        return Match::known(app, role, "service id");
    }
    // 3. A redirect host an app owns.
    let parsed: Vec<(String, String)> = g
        .redirect_uris
        .iter()
        .filter_map(|u| host_path(u))
        .collect();
    for (host, _) in &parsed {
        if let Some(app) = KNOWN
            .iter()
            .find(|a| a.hosts.iter().any(|h| under(host, h)))
        {
            return Match::known(app, role, "redirect");
        }
    }
    // 4. Its name, when its redirects name no host of their own.
    let named = |a: &KnownApp| {
        a.names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(g.name.trim()))
    };
    let no_host = parsed.iter().all(|(h, _)| loopback(h));
    if let Some(app) = KNOWN.iter().find(|a| {
        named(a)
            && (no_host
                || parsed
                    .iter()
                    .any(|(_, p)| a.paths.iter().any(|path| p.trim_end_matches('/') == *path)))
    }) {
        return Match::known(app, role, "name");
    }
    // 5. Its own: by redirect host, else by name.
    match parsed.iter().find(|(h, _)| !loopback(h)) {
        Some((host, _)) => Match {
            key: format!("host:{host}"),
            label: g.name.clone(),
            vendor: None,
            role,
            by: "own",
        },
        None => Match {
            key: format!("name:{}", g.name.trim().to_lowercase()),
            label: g.name.clone(),
            vendor: None,
            role,
            by: "own",
        },
    }
}

/// The first redirect host of a grant's client, for display.
fn redirect_host(g: &Grant) -> Option<String> {
    g.redirect_uris
        .iter()
        .find_map(|u| host_path(u))
        .map(|(h, _)| h)
}

/// The grants rolled up into apps: one per app and person (`person` null for those acting for
/// no one), ordered by person, then label. Each app has its grants as `members`, newest first.
pub fn group(grants: &[Grant]) -> Vec<Value> {
    let mut apps: Vec<(Option<String>, Match, Vec<&Grant>)> = Vec::new();
    for g in grants {
        let m = app_of(g);
        match apps
            .iter_mut()
            .find(|(p, a, _)| *p == g.person && a.key == m.key)
        {
            Some((_, _, members)) => members.push(g),
            None => apps.push((g.person.clone(), m, vec![g])),
        }
    }
    apps.sort_by(|a, b| {
        (a.0.is_none(), &a.0, a.1.label.to_lowercase(), &a.1.key).cmp(&(
            b.0.is_none(),
            &b.0,
            b.1.label.to_lowercase(),
            &b.1.key,
        ))
    });
    apps.into_iter()
        .map(|(person, m, mut members)| {
            members.sort_by_key(|g| std::cmp::Reverse((g.last_used_at, g.created_at)));
            let mut kinds: Vec<&str> = members.iter().map(|g| g.kind()).collect();
            kinds.sort_unstable();
            kinds.dedup();
            // Roles in the order the app declares them.
            let declared: Vec<Role> = KNOWN
                .iter()
                .flat_map(|a| a.service_keys.iter().map(|(_, r)| *r))
                .collect();
            let roles: Vec<Value> = declared
                .iter()
                .filter(|r| members.iter().any(|g| app_of(g).role == Some(**r)))
                .map(|r| json!({ "id": r.id, "label": r.label, "note": r.note }))
                .collect();
            // A group's own label follows its newest member, so `name:` and `host:` apps read
            // as their clients call themselves now.
            let label = if m.vendor.is_some() {
                m.label.clone()
            } else {
                members.first().map_or(m.label.clone(), |g| g.name.clone())
            };
            let list: Vec<Value> = members
                .iter()
                .map(|g| {
                    let gm = app_of(g);
                    json!({
                        "id": g.id, "name": g.name, "kind": g.kind(),
                        "client_id": g.client_id, "redirect_host": redirect_host(g),
                        "role": gm.role.map(|r| r.id), "matched_by": gm.by,
                        "tokens": g.tokens, "linked_at": g.linked_at,
                        "last_used_at": g.last_used_at, "created_at": g.created_at,
                    })
                })
                .collect();
            json!({
                "app": m.key, "label": label, "vendor": m.vendor, "person": person,
                "kinds": kinds, "roles": roles,
                "clients": members.len(),
                "tokens": members.iter().map(|g| g.tokens).sum::<i64>(),
                "linked_at": members.iter().filter_map(|g| g.linked_at).max(),
                "last_used_at": members.iter().filter_map(|g| g.last_used_at).max(),
                "added_ms": members.iter().map(|g| g.created_at).min(),
                "members": list,
            })
        })
        .collect()
}

/// The app that has grant `id` among its members, from [`group`]'s answer.
pub fn app_with<'a>(apps: &'a [Value], id: &str) -> Option<&'a Value> {
    apps.iter().find(|a| {
        a["members"]
            .as_array()
            .is_some_and(|m| m.iter().any(|g| g["id"] == id))
    })
}

/// The members of `app` that are also in `asked`, in the app's order: what disconnecting the
/// app as the dialog showed it disconnects now. A grant that joined since is left alone.
pub fn still_members(app: &Value, asked: &[String]) -> Vec<String> {
    app["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| g["id"].as_str())
        .filter(|id| asked.iter().any(|a| a == id))
        .map(str::to_string)
        .collect()
}

/// Every service user, as a grant row: its OAuth client, service id, link, live token count
/// (proxy credentials aside: those a person issued, never an app's), when it was last issued a
/// token and any override. One `?`: now, in milliseconds.
#[doc(hidden)]
pub const GRANTS_SQL: &str = "SELECT u.id AS id, u.name AS name, u.created_at AS created_at,
    c.id AS client_id, c.redirect_uris AS redirect_uris,
    (SELECT s.access_client_id FROM service_ids s WHERE s.user_id = u.id ORDER BY s.access_client_id LIMIT 1) AS service_key,
    l.standard_user_id AS person, l.approved_at AS linked_at,
    (SELECT COUNT(*) FROM oauth_tokens o WHERE o.service_user_id = u.id AND o.kind != 'proxy' AND o.expires_at > ?) AS tokens,
    (SELECT MAX(o.created_at) FROM oauth_tokens o WHERE o.service_user_id = u.id AND o.kind != 'proxy') AS last_used_at,
    a.app AS app_override
    FROM users u
    LEFT JOIN oauth_clients c ON c.service_user_id = u.id
    LEFT JOIN links l ON l.service_user_id = u.id
    LEFT JOIN app_assignments a ON a.service_user_id = u.id
    WHERE u.kind = 'service' ORDER BY u.created_at, u.id";

/// Admins' overrides of which app a grant belongs to. Added, never filled on deploy: with none,
/// the roll-up is the computed one.
#[doc(hidden)]
pub const ASSIGNMENTS_TABLE: &str = "CREATE TABLE app_assignments (service_user_id TEXT PRIMARY KEY, app TEXT NOT NULL, set_at INTEGER NOT NULL, set_by TEXT NOT NULL)";

/// Disconnecting one grant, each statement bound to its service user's id: its own tokens
/// (proxy credentials a person issued stay), its link and its push registrations. The grant,
/// its OAuth client and any override stay, acting for no one, so the app can be allowed again
/// with a passkey.
#[doc(hidden)]
pub const DISCONNECT_GRANT: [&str; 3] = [
    "DELETE FROM oauth_tokens WHERE service_user_id = ? AND kind != 'proxy'",
    "DELETE FROM links WHERE service_user_id = ?",
    "DELETE FROM apns_devices WHERE service_user_id = ?",
];

// ---- the clean-up plan ----

/// One change of a clean-up plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Remove a grant: its service user, OAuth client, tokens, link and push registrations.
    Remove(String),
    /// Put a grant under app `key` (a [`KNOWN`] key or [`OWN`]).
    Assign(String, String),
}

/// The plan's steps from a tool's arguments: `remove` (grant ids) and `assign` (`[{id, app}]`).
/// Each id must be a grant in `grants`, each app a [`KNOWN`] key or [`OWN`], and no grant may
/// be both removed and assigned.
pub fn steps(args: &Value, grants: &[Grant]) -> Result<Vec<Step>, String> {
    let has = |id: &str| grants.iter().any(|g| g.id == id);
    let mut out = Vec::new();
    for v in args["remove"].as_array().into_iter().flatten() {
        let id = v.as_str().ok_or("`remove` lists grant ids")?;
        if !has(id) {
            return Err(format!("no app acting through `{id}`"));
        }
        if !out.contains(&Step::Remove(id.into())) {
            out.push(Step::Remove(id.into()));
        }
    }
    for v in args["assign"].as_array().into_iter().flatten() {
        let (Some(id), Some(app)) = (v["id"].as_str(), v["app"].as_str()) else {
            return Err("`assign` lists {id, app}".into());
        };
        if !has(id) {
            return Err(format!("no app acting through `{id}`"));
        }
        if !override_allowed(app) {
            let keys: Vec<&str> = KNOWN.iter().map(|a| a.key).collect();
            return Err(format!(
                "`{app}` is not an app; these are: {}, or {OWN}",
                keys.join(", ")
            ));
        }
        if out.contains(&Step::Remove(id.into())) {
            return Err(format!("`{id}` is both removed and assigned"));
        }
        out.retain(|s| !matches!(s, Step::Assign(i, _) if i == id));
        out.push(Step::Assign(id.into(), app.into()));
    }
    if out.is_empty() {
        return Err("the plan changes nothing: give `remove` or `assign`".into());
    }
    Ok(out)
}

/// What a plan covers, for its approval's digest: each step with the grant as it is now (name,
/// kind, person, client), so a grant that changed since makes the approval not match.
pub fn payload(steps: &[Step], grants: &[Grant]) -> Value {
    let of = |id: &str| grants.iter().find(|g| g.id == id);
    let grant = |id: &str| {
        of(id).map_or(Value::Null, |g| {
            json!({ "id": g.id, "name": g.name, "kind": g.kind(), "person": g.person, "client_id": g.client_id })
        })
    };
    let list: Vec<Value> = steps
        .iter()
        .map(|s| match s {
            Step::Remove(id) => json!({ "remove": grant(id) }),
            Step::Assign(id, app) => json!({ "assign": grant(id), "app": app }),
        })
        .collect();
    json!({ "steps": list })
}

/// The plan in plain lines, for the approval card. `names` maps a person's id to their name.
pub fn plan_lines(steps: &[Step], grants: &[Grant], names: &dyn Fn(&str) -> String) -> Vec<String> {
    let of = |id: &str| grants.iter().find(|g| g.id == id);
    steps
        .iter()
        .map(|s| match s {
            Step::Remove(id) => {
                let g = of(id).cloned().unwrap_or_default();
                let who = g.person.as_deref().map_or("no one".to_string(), names);
                format!(
                    "Remove {} ({}, {}), acting for {who}, with its {}.",
                    g.name,
                    kind_words(g.kind()),
                    g.id,
                    counted(g.tokens, "live token")
                )
            }
            Step::Assign(id, app) => {
                let g = of(id).cloned().unwrap_or_default();
                let to = known(app).map_or("its own entry", |a| a.label);
                format!("Show {} ({}) under {to}.", g.name, g.id)
            }
        })
        .collect()
}

fn kind_words(kind: &str) -> &'static str {
    match kind {
        "oauth" => "OAuth app",
        "service" => "service user",
        _ => "no client",
    }
}

fn counted(n: i64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// What the clean-up tool proposes when asked with no steps, with a reason for each: grants
/// acting for no one (`remove`), and earlier sign-ins of an app that registers again on each
/// sign-in, beside the newest one used (`maybe_remove`: the person keeps one per device).
pub fn suggest(grants: &[Grant]) -> Value {
    let mut remove = Vec::new();
    for g in grants.iter().filter(|g| g.person.is_none()) {
        let why = match g.kind() {
            "leftover" => "acts for no one and has no client left: what stays of a revoked client",
            "oauth" if g.linked_at.is_none() => "registered but never allowed by a person",
            _ => "acts for no one",
        };
        remove.push(json!({ "id": g.id, "name": g.name, "app": app_of(g).label, "why": why, "tokens": g.tokens }));
    }
    let mut maybe = Vec::new();
    for app in group(grants) {
        if app["person"].is_null() || app["app"] != "pigeon-ios" {
            continue;
        }
        // Members are newest-used first; the first is kept.
        for m in app["members"].as_array().into_iter().flatten().skip(1) {
            maybe.push(json!({
                "id": m["id"], "name": m["name"], "app": app["label"], "tokens": m["tokens"],
                "last_used_at": m["last_used_at"],
                "why": "an earlier sign-in: the app registers again after each sign-out; keep it if it is another device you use",
            }));
        }
    }
    json!({ "remove": remove, "maybe_remove": maybe })
}

/// Sets one override: `service_user_id`, `app`, when, and the admin who set it.
#[doc(hidden)]
pub const ASSIGN: &str = "INSERT OR REPLACE INTO app_assignments (service_user_id, app, set_at, set_by) VALUES (?, ?, ?, ?)";

// ---- the clean-up tools: plan, then apply once the person approves ----

/// The approval kind of a clean-up plan.
pub const KIND: &str = "apps_cleanup";

/// How long a plan waits for its approval: 30 minutes.
const PENDING_MS: i64 = 30 * 60 * 1000;

fn bad(why: impl Into<String>) -> worker::Error {
    worker::Error::RustError(why.into())
}

/// Every grant, read now.
async fn read_grants(cx: &crate::Context<'_>) -> worker::Result<Vec<Grant>> {
    let rows = crate::users::directory(cx.env, "apps.grants", json!({})).await?;
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .map(Grant::from_row)
        .collect())
}

/// People's names by id, for the plan's lines.
async fn names(cx: &crate::Context<'_>) -> worker::Result<Vec<(String, String)>> {
    let all = crate::users::directory(cx.env, "users.list", json!({})).await?;
    Ok(all["users"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|u| u["kind"] == "standard")
        .map(|u| {
            (
                u["id"].as_str().unwrap_or_default().to_string(),
                u["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect())
}

/// `apps_cleanup`: with no steps, the apps and a suggestion; with steps, the plan in words and
/// an approval for the person to decide. Changes nothing.
pub async fn cleanup_plan(cx: &crate::Context<'_>, args: &Value) -> worker::Result<Value> {
    let grants = read_grants(cx).await?;
    if args["remove"].is_null() && args["assign"].is_null() {
        return Ok(json!({
            "apps": group(&grants),
            "suggested": suggest(&grants),
            "note": "Nothing changes. To plan, call apps_cleanup with `remove` and/or `assign`.",
        }));
    }
    let steps = steps(args, &grants).map_err(bad)?;
    let caller = cx
        .caller
        .ok_or_else(|| bad("planning needs a known caller"))?;
    let person = crate::users::person_of(caller)
        .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
    let people = names(cx).await?;
    let name = |id: &str| {
        people
            .iter()
            .find(|(i, _)| i == id)
            .map_or(id.to_string(), |(_, n)| n.clone())
    };
    let lines = plan_lines(&steps, &grants, &name);
    let payload = payload(&steps, &grants);
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest = pigeon_core::approval::payload_digest(&kind, KIND, &payload);
    let made = crate::users::directory(
        cx.env,
        "approval.create",
        json!({
            "kind": kind.name(),
            "subject": KIND,
            "digest": digest,
            "summary": { "plan": lines },
            "payload": payload,
            "caller_id": caller.user.id,
            "approver_id": person.id,
            "pending_ms": PENDING_MS,
        }),
    )
    .await?;
    Ok(json!({
        "plan": lines,
        "approval": made["record"]["id"],
        "waits_until_ms": made["record"]["expires_ms"],
        "note": "Nothing changes until the person approves this plan with their passkey on the \
                 console's Approvals tab; then the platform makes it once, as you \
                 (apps_cleanup_apply). approvals_status {id: <this approval>} says how it went.",
    }))
}

/// `apps_cleanup_apply`: re-reads the grants, checks they still give the approved plan, spends
/// the approval and makes the plan's changes.
pub async fn cleanup_apply(cx: &crate::Context<'_>, args: &Value) -> worker::Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| bad("applying needs a known caller"))?;
    let id = crate::arg(args, "plan")?;
    let got = crate::users::directory(cx.env, "approval.get", json!({ "id": id })).await?;
    if got["kind"] != KIND {
        return Err(bad(format!("there is no apps clean-up approval {id}")));
    }
    // The steps as approved, checked against the grants as they are now.
    let mut asked = json!({ "remove": [], "assign": [] });
    for s in got["payload"]["steps"].as_array().into_iter().flatten() {
        if let Some(g) = s["remove"]["id"].as_str() {
            asked["remove"].as_array_mut().unwrap().push(json!(g));
        } else if let Some(g) = s["assign"]["id"].as_str() {
            asked["assign"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "id": g, "app": s["app"] }));
        }
    }
    let grants = read_grants(cx).await?;
    let steps = steps(&asked, &grants).map_err(|e| {
        bad(format!(
            "the plan does not run: {e}. The apps changed since it was approved; plan again."
        ))
    })?;
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest = pigeon_core::approval::payload_digest(&kind, KIND, &payload(&steps, &grants));
    let taken = crate::users::directory(
        cx.env,
        "approval.take",
        json!({ "id": id, "digest": digest, "caller_id": caller.user.id }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(bad(format!(
            "the plan does not run: {why}. If an app changed since it was approved, plan again."
        )));
    }
    let set_by = crate::users::person_of(caller).map_or(caller.user.id.clone(), |p| p.id.clone());
    crate::users::directory(
        cx.env,
        "apps.cleanup",
        json!({ "remove": asked["remove"], "assign": asked["assign"], "set_by": set_by }),
    )
    .await
}

//! The console: one page at the platform's root, with a tab for each part of the platform
//! (Overview, Architecture, Gateways, Settings) and a section for each capability that declares
//! one. A person signs in once with a passkey and gets a session cookie: HttpOnly, SameSite
//! Strict, kept only as a hash in the Directory, ending after 30 minutes idle or 8 hours. The
//! session lets them read; every write that changes a secret, a credential, a passkey or a link
//! still takes a passkey of its own, bound to what it changes. Every POST must come from this
//! host's own pages, in JSON.

pub mod page;
pub mod sections;
pub mod session;
pub mod writes;

use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::passkeys::Passkeys;
use crate::users::{Passkey, User, directory};
use crate::{Call, Capability, Context, Serve, arg, unknown};
use sections::{Section, Tab};
use writes::Write;

/// The console page and its API.
pub struct Console {
    /// The hostname the console is on; POSTs must name it as their Origin.
    pub host: String,
    pub passkeys: Passkeys,
}

/// The challenge purpose for signing in.
const SIGN_IN: &str = "console:sign-in";

const APP: &str = include_str!("console/app.js");
const CHART: &str = include_str!("console/chart.js");
const OVERVIEW: &str = include_str!("console/overview.js");
const KEYS: &str = include_str!("console/keys.js");
const USERS: &str = include_str!("console/users.js");
const STYLES: &str = include_str!("console/console.css");
const ICON: &[u8] = include_bytes!("console/icon.png");

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// A signed-in person and their session's times, in ms since the epoch.
#[derive(Deserialize)]
struct Signed {
    user: User,
    created_at: i64,
    last_seen_at: i64,
}

/// `value` as a JSON response that is never cached, with `status`.
fn answer(value: &Value, status: u16) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::from_json(value)?
        .with_status(status)
        .with_headers(headers))
}

/// The number of days a section's data covers: `days` from the query, 1 to 30, 7 by default.
pub fn days(query: Option<&str>) -> i64 {
    query
        .and_then(|q| q.parse::<i64>().ok())
        .filter(|d| (1..=30).contains(d))
        .unwrap_or(7)
}

/// A person's initials for their picture until they set one: the first letter of their first
/// and last words, upper-cased.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string());
    match words.as_slice() {
        [] => "?".into(),
        [one] => first(one).unwrap_or_default(),
        [a, .., z] => [first(a), first(z)].into_iter().flatten().collect(),
    }
}

impl Console {
    /// The person whose session cookie `req` carries, while it is fresh.
    async fn signed(&self, env: &Env, req: &Request) -> Result<Option<Signed>> {
        let header = req.headers().get("Cookie")?.unwrap_or_default();
        let Some(token) = session::token_from(&header) else {
            return Ok(None);
        };
        let found = directory(env, "session.get", json!({ "hash": hash(token) })).await?;
        Ok(serde_json::from_value(found).ok())
    }

    /// Every hosted capability's sections, with the capability's name.
    fn declared(cx: &Context<'_>) -> Vec<(String, Section)> {
        cx.hosted
            .capabilities
            .iter()
            .flat_map(|c| c.sections().into_iter().map(|s| (c.name().to_string(), s)))
            .collect()
    }

    /// Section `id`'s data from each capability that fills it: one owner's data as it is,
    /// several owners' by name.
    async fn section(cx: &Context<'_>, id: &str, person: &User, query: &Value) -> Result<Value> {
        let owners: Vec<&dyn Capability> = cx
            .hosted
            .capabilities
            .iter()
            .filter(|c| c.sections().iter().any(|s| s.id == id))
            .map(|c| c.as_ref())
            .collect();
        match owners.as_slice() {
            [] => Err(bad("no such section")),
            [one] => one.section_data(cx, id, person, query).await,
            many => {
                let mut all = serde_json::Map::new();
                for c in many {
                    all.insert(
                        c.name().into(),
                        c.section_data(cx, id, person, query).await?,
                    );
                }
                Ok(Value::Object(all))
            }
        }
    }

    /// What the overview shows: secrets the platform needs and lacks, and each gateway's cost
    /// over the last week with their total.
    async fn overview(cx: &Context<'_>, person: &User) -> Result<Value> {
        let listing = sections::tabs(&Self::declared(cx));
        let week = json!({ "days": 7 });
        let mut answers = Vec::new();
        let mut missing = Vec::new();
        for tab in &listing {
            for l in &tab.sections {
                let wanted = l.section.cost.is_some() || l.section.id == "keys";
                if !wanted {
                    continue;
                }
                // One failing section leaves its figure out rather than the whole overview.
                let Ok(data) = Self::section(cx, l.section.id, person, &week).await else {
                    continue;
                };
                if let Some(cost) = data["cost_7d"].as_f64() {
                    answers.push((l.section.id.to_string(), cost));
                }
                for found in [&data["missing"], &data["secrets"]["missing"]] {
                    if let Some(list) = found.as_array() {
                        missing.extend(list.iter().cloned());
                    }
                }
            }
        }
        let (costs, total) = sections::costs(&listing, &answers);
        let costs: Vec<Value> = listing
            .iter()
            .flat_map(|t| t.sections.iter().map(move |l| (t, l)))
            .filter_map(|(t, l)| {
                let name = l.section.cost?;
                let cost = costs.iter().find(|(n, _)| *n == name).map(|(_, c)| *c);
                Some(json!({ "name": name, "cost": cost, "path": format!("{}#{}", t.path, l.section.id) }))
            })
            .collect();
        Ok(json!({ "missing": missing, "costs": costs, "total": total }))
    }

    /// The signed-in person, their picture and passkeys, how many apps act for them, and when
    /// their session ends: after idling, and at the latest.
    async fn me(env: &Env, signed: &Signed) -> Result<Value> {
        let user = &signed.user;
        let avatar = directory(env, "user.avatar", json!({ "id": user.id })).await?;
        let keys: Vec<Passkey> = serde_json::from_value(
            directory(env, "passkey.list", json!({ "user_id": user.id })).await?,
        )?;
        let listed = directory(env, "users.list", json!({})).await?;
        let apps = listed["links"].as_array().map_or(0, |l| {
            l.iter()
                .filter(|x| x["standard_user_id"] == user.id.as_str())
                .count()
        });
        let ends_at = signed.created_at + session::MAX_MS;
        Ok(json!({
            "id": user.id,
            "name": user.name,
            "initials": initials(&user.name),
            "kind": user.kind,
            "avatar": avatar,
            "passkeys": keys.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
            "apps": apps,
            "expires_at": (signed.last_seen_at + session::IDLE_MS).min(ends_at),
            "ends_at": ends_at,
        }))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        let method = req.method();
        if method == Method::Get {
            return match path.as_str() {
                p if Tab::from_path(p).is_some() => {
                    page::html(&page::shell(&self.passkeys.site_name))
                }
                "/console/app.js" => page::asset(APP, "text/javascript; charset=utf-8"),
                "/console/chart.js" => page::asset(CHART, "text/javascript; charset=utf-8"),
                "/console/overview.js" => page::asset(OVERVIEW, "text/javascript; charset=utf-8"),
                "/console/keys.js" => page::asset(KEYS, "text/javascript; charset=utf-8"),
                "/console/users.js" => page::asset(USERS, "text/javascript; charset=utf-8"),
                "/console/console.css" => page::asset(STYLES, "text/css; charset=utf-8"),
                "/console/icon.png" => page::image(ICON, "image/png"),
                p if p.starts_with("/console/api/") => {
                    let Some(signed) = self.signed(cx.env, &req).await? else {
                        return answer(&json!({ "error": "sign in first" }), 401);
                    };
                    let query = url
                        .query_pairs()
                        .find(|(k, _)| k == "days")
                        .map(|(_, v)| v.into_owned());
                    let data = match p {
                        "/console/api/me" => Self::me(cx.env, &signed).await,
                        "/console/api/tabs" => {
                            Ok(serde_json::to_value(sections::tabs(&Self::declared(cx)))?)
                        }
                        "/console/api/overview" => Self::overview(cx, &signed.user).await,
                        p => match p.strip_prefix("/console/api/section/") {
                            Some(id) if sections::anchor(id) => {
                                let q = json!({ "days": days(query.as_deref()) });
                                Self::section(cx, id, &signed.user, &q).await
                            }
                            _ => return answer(&json!({ "error": "not found" }), 404),
                        },
                    };
                    match data {
                        Ok(v) => answer(&v, 200),
                        Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                    }
                }
                _ => Response::error("not found", 404),
            };
        }
        if method != Method::Post {
            return Response::error("not found", 404);
        }
        let origin = req.headers().get("Origin")?;
        let content = req.headers().get("Content-Type")?;
        if !session::same_origin(origin.as_deref(), &self.host)
            || !session::json_content(content.as_deref())
        {
            return answer(
                &json!({ "error": "refused: not from this site's own pages" }),
                403,
            );
        }
        let body: Value = req.json().await.unwrap_or(Value::Null);
        match path.as_str() {
            "/console/sign-in/begin" => {
                match self.passkeys.sign_in_begin(cx.env, SIGN_IN, None).await {
                    Ok(v) => answer(&v, 200),
                    Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                }
            }
            "/console/sign-in/finish" => match self.sign_in(cx.env, &body).await {
                Ok(token) => {
                    let res = answer(&json!({ "ok": true }), 200)?;
                    res.headers().set("Set-Cookie", &session::cookie(&token))?;
                    Ok(res)
                }
                Err(e) => answer(&json!({ "error": e.to_string() }), 400),
            },
            "/console/sign-out" => {
                let header = req.headers().get("Cookie")?.unwrap_or_default();
                if let Some(token) = session::token_from(&header) {
                    directory(cx.env, "session.remove", json!({ "hash": hash(token) })).await?;
                }
                let res = answer(&json!({ "ok": true }), 200)?;
                res.headers().set("Set-Cookie", &session::clear())?;
                Ok(res)
            }
            p if p.starts_with("/console/write/") => {
                let Some(signed) = self.signed(cx.env, &req).await? else {
                    return answer(&json!({ "error": "sign in first" }), 401);
                };
                match self.write(cx.env, p, &signed.user, &body).await {
                    Ok(v) => answer(&v, 200),
                    Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                }
            }
            "/console/api/avatar" => {
                let Some(signed) = self.signed(cx.env, &req).await? else {
                    return answer(&json!({ "error": "sign in first" }), 401);
                };
                let avatar = body["avatar"].as_str().unwrap_or_default();
                let set = directory(
                    cx.env,
                    "user.avatar_set",
                    json!({ "id": signed.user.id, "avatar": avatar }),
                )
                .await;
                match set {
                    Ok(v) => answer(&v, 200),
                    Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                }
            }
            _ => Response::error("not found", 404),
        }
    }

    /// One of the console's writes, at `/console/write/{action}/{begin|finish}`: `begin` starts a
    /// passkey approval by `person`, bound to the write and its target; `finish` checks it and
    /// makes the change.
    async fn write(&self, env: &Env, path: &str, person: &User, body: &Value) -> Result<Value> {
        let rest = path.trim_start_matches("/console/write/");
        let (action, step) = rest.split_once('/').ok_or_else(|| bad("not found"))?;
        let write = Write::named(action).ok_or_else(|| bad("no such write"))?;
        let target = write.target(arg(body, "target")?).map_err(|e| bad(&e))?;
        let purpose = write.purpose(&target);
        match step {
            "begin" => {
                self.passkeys
                    .sign_in_begin(env, &purpose, Some(&person.id))
                    .await
            }
            "finish" => {
                self.passkeys
                    .sign_in_finish(env, &purpose, body, Some(&person.id))
                    .await?;
                self.act(env, write, &target).await
            }
            _ => Err(bad("not found")),
        }
    }

    /// Makes an approved write.
    async fn act(&self, env: &Env, write: Write, target: &str) -> Result<Value> {
        Ok(match write {
            Write::RemovePasskey => {
                directory(env, "passkey.remove", json!({ "id": target })).await?;
                json!({ "message": "The passkey is removed." })
            }
            Write::DisconnectApp => {
                let gone = directory(env, "app.disconnect", json!({ "id": target })).await?;
                json!({ "message": format!(
                    "{} is disconnected: its tokens are revoked and it acts for no one.",
                    gone["name"].as_str().unwrap_or("The app")
                ) })
            }
            Write::Invite => {
                let user = directory(
                    env,
                    "users.create",
                    json!({ "kind": "standard", "name": target }),
                )
                .await?;
                let id = user["id"].as_str().unwrap_or_default();
                let code = directory(env, "enrol.create", json!({ "user_id": id })).await?;
                json!({
                    "message": format!("{target} is added. Send them this link to register a passkey: it works once, within 15 minutes."),
                    "link": format!("https://{}/auth/enrol?code={}", self.host, code.as_str().unwrap_or_default()),
                })
            }
        })
    }

    /// Checks the passkey answer, which must be a person's, and starts their session: the new
    /// session's token, for the cookie. Only its hash is kept.
    async fn sign_in(&self, env: &Env, body: &Value) -> Result<String> {
        arg(body, "challenge_id")?;
        let key = self
            .passkeys
            .sign_in_finish(env, SIGN_IN, body, None)
            .await?;
        let (token, token_hash) = token(session::PREFIX)?;
        directory(
            env,
            "session.create",
            json!({ "hash": token_hash, "user_id": key.user_id }),
        )
        .await?;
        Ok(token)
    }
}

impl Capability for Console {
    fn name(&self) -> &'static str {
        "console"
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    /// The tabs' paths and the console's own; `/` itself is handed over by the platform, since
    /// a route is a prefix.
    fn routes(&self) -> Vec<&'static str> {
        vec!["/console/", "/architecture", "/gateways", "/settings"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_default_to_a_week_and_stay_within_a_month() {
        assert_eq!(days(None), 7);
        assert_eq!(days(Some("30")), 30);
        assert_eq!(days(Some("31")), 7);
        assert_eq!(days(Some("0")), 7);
        assert_eq!(days(Some("x")), 7);
    }

    #[test]
    fn initials_take_the_first_and_last_words() {
        assert_eq!(initials("Willow"), "W");
        assert_eq!(initials("ada  king lovelace"), "AL");
        assert_eq!(initials(""), "?");
    }
}

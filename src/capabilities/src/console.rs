//! The console: one page at the platform's root, with a tab for each part of the platform the
//! person looks after (Approvals while something waits for them, Architecture, Access, Cost) and
//! a section for each capability that declares one. Each section loads on its own, within
//! [`SECTION_MS`]; the slow ones answer a snapshot at once ([`snapshot`]). A person signs in once with a passkey and gets a session cookie: HttpOnly, SameSite
//! Strict, kept only as a hash in the Directory, ending after 30 minutes idle or 8 hours. The
//! session lets them read; every write that changes a secret, a credential, a passkey or a link
//! still takes a passkey of its own, bound to what it changes. Every POST must come from this
//! host's own pages, in JSON.

#[path = "console/web/ops.rs"]
mod ops;
pub mod page;
pub mod sections;
pub mod session;
pub mod snapshot;
pub mod writes;

use futures_util::future::{Either, join_all, select};
use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::time::Duration;
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

const KEYS: &str = include_str!("console/keys.js");
const USERS: &str = include_str!("console/users.js");
const APPROVALS: &str = include_str!("console/approvals.js");
const INSTANCES: &str = include_str!("console/instances.js");
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

/// How long a section may take before the console answers that it timed out, in ms. Below the
/// runtime's own limits, so the page gets an answer it can offer to retry, not a dropped
/// connection.
pub const SECTION_MS: u64 = 10_000;

/// What a section that ran out of time answers.
const TIMED_OUT: &str = "timed out: its source did not answer within 10 seconds";

/// `work`, or the [`TIMED_OUT`] error once `ms` have passed.
async fn timed<T>(ms: u64, work: impl Future<Output = Result<T>>) -> Result<T> {
    let wait = worker::Delay::from(Duration::from_millis(ms));
    futures_util::pin_mut!(work);
    futures_util::pin_mut!(wait);
    match select(work, wait).await {
        Either::Left((done, _)) => done,
        Either::Right(_) => Err(bad(TIMED_OUT)),
    }
}

/// An error as the page gets it: a timeout as 504 that says it can be retried, anything else
/// as 400.
fn failed(e: &Error) -> Result<Response> {
    let why = e.to_string();
    if why.contains(TIMED_OUT) {
        answer(&json!({ "error": why, "retry": true }), 504)
    } else {
        answer(&json!({ "error": why }), 400)
    }
}

fn now_ms() -> i64 {
    worker::js_sys::Date::now() as i64
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

/// The person signed in to the console on `req`, if any: the same check as the console's own
/// pages, for other capabilities' console writes.
pub async fn signed_person(env: &Env, req: &Request) -> Result<Option<User>> {
    let header = req.headers().get("Cookie")?.unwrap_or_default();
    let Some(token) = session::token_from(&header) else {
        return Ok(None);
    };
    let found = directory(env, "session.get", json!({ "hash": hash(token) })).await?;
    let signed: Option<Signed> = serde_json::from_value(found).ok();
    Ok(signed.map(|s| s.user).filter(|u| u.kind == "standard"))
}

impl Console {
    /// The person whose session cookie `req` carries, while it is fresh. The Directory answers a
    /// session only while its user exists and is a person, and removes it otherwise; the kind is
    /// checked again here, so nothing but a person is ever signed in.
    async fn signed(&self, env: &Env, req: &Request) -> Result<Option<Signed>> {
        let header = req.headers().get("Cookie")?.unwrap_or_default();
        let Some(token) = session::token_from(&header) else {
            return Ok(None);
        };
        let found = directory(env, "session.get", json!({ "hash": hash(token) })).await?;
        let signed: Option<Signed> = serde_json::from_value(found).ok();
        Ok(signed.filter(|s| s.user.kind == "standard"))
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
                // Every owner at once: the section takes as long as its slowest, not their sum.
                let answers =
                    join_all(many.iter().map(|c| c.section_data(cx, id, person, query))).await;
                let mut all = serde_json::Map::new();
                for (c, data) in many.iter().zip(answers) {
                    all.insert(c.name().into(), data?);
                }
                Ok(Value::Object(all))
            }
        }
    }

    /// The tabs and their sections, with how many landings and tool calls wait for the person on
    /// Approvals. One that does not answer in time counts 0; both so, the tab is hidden.
    async fn tabs(cx: &Context<'_>) -> Result<Value> {
        let mut listing = sections::tabs(&Self::declared(cx));
        let (landings, calls) = futures_util::future::join(
            timed(SECTION_MS / 2, crate::land::awaiting(cx.env)),
            timed(SECTION_MS / 2, crate::approvals::waiting(cx.env)),
        )
        .await;
        let waiting = landings.map_or(0, |w| w.len()) + calls.unwrap_or(0);
        for t in &mut listing {
            if t.tab == Tab::Approvals {
                t.waiting = Some(waiting);
            }
        }
        Ok(serde_json::to_value(listing)?)
    }

    /// The signed-in person, their picture and passkeys, how many apps act for them, and when
    /// their session ends: after idling, and at the latest.
    async fn me(env: &Env, signed: &Signed) -> Result<Value> {
        let user = &signed.user;
        let (avatar, keys, listed) = futures_util::future::join3(
            directory(env, "user.avatar", json!({ "id": user.id })),
            directory(env, "passkey.list", json!({ "user_id": user.id })),
            directory(env, "users.list", json!({})),
        )
        .await;
        let (avatar, listed) = (avatar?, listed?);
        let keys: Vec<Passkey> = serde_json::from_value(keys?)?;
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

    /// Section `id`'s data over `days` days, within [`SECTION_MS`]. A section that keeps a
    /// snapshot answers it at once unless `fresh` asks otherwise, and keeps each fresh answer.
    /// `X-Snapshot-At` says when the answer was made; `X-Snapshot-Stale: 1` that it is past its
    /// freshness, so the page shows it and asks again with `fresh=1`.
    async fn snapshot(
        cx: &Context<'_>,
        id: &str,
        person: &User,
        days: i64,
        fresh: bool,
    ) -> Result<Response> {
        let kept_for = snapshot::fresh_ms(id);
        let key = snapshot::key(id, days);
        if let (Some(fresh_ms), false) = (kept_for, fresh) {
            // A snapshot that cannot be read only means computing the answer.
            if let Ok(Value::String(kept)) =
                directory(cx.env, "setting.get", json!({ "key": key })).await
                && let Ok(snap) = serde_json::from_str::<snapshot::Snapshot>(&kept)
            {
                let stale = snapshot::stale(snap.at_ms, fresh_ms, now_ms());
                return Self::stamped(&snap.data, snap.at_ms, stale);
            }
        }
        let query = json!({ "days": days });
        let data = match timed(SECTION_MS, Self::section(cx, id, person, &query)).await {
            Ok(data) => data,
            Err(e) => return failed(&e),
        };
        let at_ms = now_ms();
        if kept_for.is_some() {
            let snap = snapshot::Snapshot {
                at_ms,
                data: data.clone(),
            };
            if let Ok(value) = serde_json::to_string(&snap) {
                // A snapshot that cannot be written only means computing it next time.
                let _ = directory(
                    cx.env,
                    "setting.put",
                    json!({ "key": key, "value": value, "ttl_ms": snapshot::KEPT_MS }),
                )
                .await;
            }
        }
        Self::stamped(&data, at_ms, false)
    }

    /// `data` with the time it was made and whether it is stale, in headers.
    fn stamped(data: &Value, at_ms: i64, stale: bool) -> Result<Response> {
        let res = answer(data, 200)?;
        res.headers().set("X-Snapshot-At", &at_ms.to_string())?;
        if stale {
            res.headers().set("X-Snapshot-Stale", "1")?;
        }
        Ok(res)
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        let method = req.method();
        if method == Method::Get {
            return match path.as_str() {
                p if page::app_redirect(p).is_some() => {
                    let mut target = url
                        .join(page::app_redirect(p).unwrap())
                        .map_err(|e| worker::Error::RustError(e.to_string()))?;
                    if p == "/instances"
                        && let Some((_, value)) = url.query_pairs().find(|(key, _)| key == "i")
                    {
                        target.query_pairs_mut().append_pair("i", &value);
                    }
                    page::redirect(
                        &format!("{}?{}", target.path(), target.query().unwrap_or_default())
                            .trim_end_matches('?'),
                    )
                }
                "/app" | "/app/" => {
                    let signed = self.signed(cx.env, &req).await?;
                    page::html(&page::app_shell(
                        &self.passkeys.site_name,
                        signed.as_ref().map(|s| s.user.name.as_str()),
                    ))
                }
                p if p.starts_with("/app/") => page::app_asset(p),
                "/console/keys.js" => page::asset(KEYS, "text/javascript; charset=utf-8"),
                "/console/users.js" => page::asset(USERS, "text/javascript; charset=utf-8"),
                "/console/approvals.js" => page::asset(APPROVALS, "text/javascript; charset=utf-8"),
                "/console/instances.js" => page::asset(INSTANCES, "text/javascript; charset=utf-8"),
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
                    let fresh = url.query_pairs().any(|(k, v)| k == "fresh" && v == "1");
                    let data = match p {
                        "/console/api/me" => Self::me(cx.env, &signed).await,
                        "/console/api/tabs" => Self::tabs(cx).await,
                        p => match p.strip_prefix("/console/api/section/") {
                            Some(id) if sections::anchor(id) => {
                                let days = days(query.as_deref());
                                return Self::snapshot(cx, id, &signed.user, days, fresh).await;
                            }
                            _ => return answer(&json!({ "error": "not found" }), 404),
                        },
                    };
                    match data {
                        Ok(v) => answer(&v, 200),
                        Err(e) => failed(&e),
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
        let bearer = req
            .headers()
            .get("Authorization")?
            .and_then(|h| h.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
            .filter(|t| !t.is_empty());
        let is_write = path.starts_with("/console/write/");
        // A bearer is not ambient, so the Origin check that guards the cookie is skipped for it.
        let by_bearer = is_write && bearer.is_some();
        if (!by_bearer && !session::same_origin(origin.as_deref(), &self.host))
            || !session::json_content(content.as_deref())
        {
            return answer(
                &json!({ "error": "refused: not from this site's own pages" }),
                403,
            );
        }
        let body: Value = req.json().await.unwrap_or(Value::Null);
        match path.as_str() {
            "/app/op" => {
                let Some(signed) = self.signed(cx.env, &req).await? else {
                    return answer(&json!({ "error": "sign in first" }), 401);
                };
                ops::serve(cx, signed.user, &body).await
            }
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
                if let Some(token) = bearer {
                    let allowed = p
                        .trim_start_matches("/console/write/")
                        .split_once('/')
                        .and_then(|(a, _)| Write::named(a))
                        .is_some_and(Write::by_bearer);
                    if !allowed {
                        return answer(
                            &json!({ "error": "this write needs the console, not an app" }),
                            403,
                        );
                    }
                    let caller = match crate::oauth::caller(cx.env, &token).await {
                        Ok(c) => c,
                        Err(e) => return answer(&json!({ "error": e.to_string() }), 401),
                    };
                    let Some(person) = caller.acting_for.filter(|u| u.kind == "standard") else {
                        return answer(&json!({ "error": "the app acts for no person" }), 403);
                    };
                    return match self.write(cx.env, p, &person, &body).await {
                        Ok(v) => answer(&v, 200),
                        Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                    };
                }
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
        let purpose = if write.bound_to_digest() {
            crate::approvals::purpose(env, &target).await?
        } else {
            write.purpose(&target)
        };
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
                self.act(env, write, &target, person).await
            }
            _ => Err(bad("not found")),
        }
    }

    /// Makes an approved write.
    async fn act(&self, env: &Env, write: Write, target: &str, person: &User) -> Result<Value> {
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
            Write::LandApprove => crate::land::approve(env, target, &person.name).await?,
            Write::LandApproval => {
                crate::land::set_approval(env, target == "on", &person.name).await?
            }
            Write::ApprovalApprove => crate::approvals::decide(env, target, true, person).await?,
            Write::ApprovalDeny => crate::approvals::decide(env, target, false, person).await?,
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
        // Only a person signs in. The Directory refuses a session to anyone else too.
        let user: Option<User> = serde_json::from_value(
            directory(env, "users.get", json!({ "id": key.user_id })).await?,
        )?;
        user.filter(|u| u.kind == "standard")
            .ok_or_else(|| bad("only a person can sign in to the console"))?;
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

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "console",
            pigeon_core::versions::Kind::Capability,
        )]
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
        vec![
            "/console/",
            "/app",
            "/approvals",
            "/instances",
            "/architecture",
            "/access",
            "/cost",
            "/gateways",
            "/settings",
        ]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

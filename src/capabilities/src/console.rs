//! The console at `/app/<instance>/<area>/<view>` (docs/plans/console-redesign.md): its shell
//! and assets, its one read path (`/app/op`, [`ops`]), its one write path
//! (`/console/write/<action>/{begin,finish}`, [`write`]), and one-hop redirects from the old
//! console's bookmarks (`/`, `/approvals`, `/secrets`, `/app?tab=…`, ...) into it
//! ([`page::bookmark`]). A person signs in once with a passkey and gets a session cookie:
//! HttpOnly, SameSite Strict, kept only as a hash in the Directory, ending after 30 minutes idle
//! or 8 hours. The session lets them read; every write still takes a passkey of its own, bound
//! to what it changes. Every POST is JSON and must come from this host's own pages, except a
//! read or write an app (the iOS app) makes by bearer for the person it acts for: that one is
//! made exactly as that person's session would make it, with the same allowlist, binding and
//! passkey ([`app_person`], [`Post::takes_bearer`], [`session::admitted`]).

#[path = "console/web/ops.rs"]
pub mod ops;
pub mod page;
pub mod sections;
pub mod session;
pub mod write;

use futures_util::future::{Either, select};
use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::time::Duration;
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::passkeys::Passkeys;
use crate::site_passkeys::console as site_access;
use crate::users::{Caller, User, directory};
use crate::{Call, Capability, Context, Serve, arg, unknown};
use write::Step;

/// The console page and its API.
pub struct Console {
    /// The hostname the console is on; POSTs must name it as their Origin.
    pub host: String,
    pub passkeys: Passkeys,
}

/// The challenge purpose for signing in.
const SIGN_IN: &str = "console:sign-in";

const ICON: &[u8] = include_bytes!("console/icon.png");

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// A signed-in person, as the Directory answers their session.
#[derive(Deserialize)]
struct Signed {
    user: User,
}

/// `value` as a JSON response that is never cached, with `status`.
fn answer(value: &Value, status: u16) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::from_json(value)?
        .with_status(status)
        .with_headers(headers))
}

/// How long a read may take before the console answers that it timed out, in ms. Below the
/// runtime's own limits, so the page gets an answer it can offer to retry, not a dropped
/// connection.
pub const READ_MS: u64 = 10_000;

/// What a read that ran out of time answers.
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

/// What a GET to one of the console's paths reaches; any other path answers 404.
#[derive(Debug, PartialEq)]
pub enum Get {
    /// An old console bookmark, redirected in one hop to its place in the console.
    Bookmark(page::Bookmark),
    /// The console's shell, at `/app` and every page below it.
    App,
    /// A script or stylesheet: its body and Content-Type.
    Asset(&'static str, &'static str),
    /// The console's icon.
    Icon,
    /// The pigeon.dev mark's artwork, for the sign-in card (also answered to HEAD).
    Mark,
}

impl Get {
    /// The route for a GET to `path` with `query`.
    pub fn route(path: &str, query: Option<&str>) -> Option<Self> {
        if let Some(to) = page::bookmark(path, query) {
            return Some(Self::Bookmark(to));
        }
        Some(match path {
            "/console/icon.png" => Self::Icon, // route: open
            page::MARK => Self::Mark,
            p if page::is_page(p) => Self::App,
            p => {
                let (body, kind) = page::asset_at(p)?;
                Self::Asset(body, kind)
            }
        })
    }
}

/// What a POST to one of the console's paths reaches; any other path answers 404.
#[derive(Debug, PartialEq)]
pub enum Post {
    /// One of the console's reads.
    AppOp,
    SignInBegin,
    SignInFinish,
    SignOut,
    /// One of the console's writes, `/console/write/{action}/{step}`.
    Write,
}

impl Post {
    pub fn route(path: &str) -> Option<Self> {
        Some(match path {
            "/app/op" => Self::AppOp,                        // route: session
            "/console/sign-in/begin" => Self::SignInBegin,   // route: sign-in
            "/console/sign-in/finish" => Self::SignInFinish, // route: sign-in
            "/console/sign-out" => Self::SignOut,            // route: session
            p if write::route(p).is_some() => Self::Write,
            _ => return None,
        })
    }

    /// Whether an app may make this POST by bearer for the person it acts for, instead of the
    /// session cookie: the reads, and the writes whose action allows it
    /// ([`write::Action::by_bearer`]). Signing in and out are the browser's own.
    pub fn takes_bearer(&self) -> bool {
        matches!(self, Self::AppOp | Self::Write)
    }
}

/// The person an app's bearer acts for, as `caller` (its OAuth token's caller) names them: only
/// a person (a standard user), never the app's own service user. Reads and writes by bearer
/// both find the person this way, and then run exactly as for that person's console session.
pub fn app_person(caller: Caller) -> Option<User> {
    caller.acting_for.filter(|u| u.kind == "standard")
}

/// The home instance's id, when it can be read.
async fn home(env: &Env) -> Option<String> {
    let all = crate::instances::all(env).await.ok()?;
    all.into_iter()
        .find(|i| i.account == crate::instances::record::AccountRef::Home)
        .map(|i| i.id)
}

/// The console's URL for `area` (and `view`) of the home instance; the console's root when the
/// home instance cannot be read.
pub async fn home_url(env: &Env, area: &str, view: Option<&str>) -> String {
    match home(env).await {
        Some(id) => page::url(&id, area, view),
        None => page::BASE.to_string(),
    }
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

    /// Who a POST is made for: with `bearer`, the person the app's OAuth token acts for
    /// ([`app_person`]), and the cookie is not read; else the person signed in by cookie. `Err`
    /// is the refusal to answer.
    async fn person(
        &self,
        env: &Env,
        req: &Request,
        bearer: Option<&str>,
    ) -> Result<std::result::Result<User, Response>> {
        let Some(token) = bearer else {
            return Ok(match self.signed(env, req).await? {
                Some(signed) => Ok(signed.user),
                None => Err(answer(&json!({ "error": "sign in first" }), 401)?),
            });
        };
        let caller = match crate::oauth::caller(env, token).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(answer(&json!({ "error": e.to_string() }), 401)?)),
        };
        Ok(match app_person(caller) {
            Some(person) => Ok(person),
            None => Err(answer(
                &json!({ "error": "the app acts for no person" }),
                403,
            )?),
        })
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        let method = req.method();
        if method == Method::Head && path == page::MARK {
            return page::mark();
        }
        if method == Method::Get {
            let Some(route) = Get::route(&path, url.query()) else {
                return Response::error("not found", 404);
            };
            return match route {
                Get::Bookmark(page::Bookmark::Root) => page::redirect(page::BASE),
                Get::Bookmark(page::Bookmark::Area {
                    instance,
                    area,
                    view,
                }) => {
                    let to = match instance {
                        Some(id) => {
                            let id = crate::instances::renamed(cx.env, &id).await?.unwrap_or(id);
                            page::url(&id, area, view)
                        }
                        None => home_url(cx.env, area, view).await,
                    };
                    page::redirect(&to)
                }
                Get::App => {
                    // An old instance id (R1's compat window): the same page of its new id.
                    if let Some((id, rest)) = page::page_instance(&path)
                        && let Some(to) = crate::instances::renamed(cx.env, &id).await?
                    {
                        let query = url.query().map_or(String::new(), |q| format!("?{q}"));
                        let to = format!("{}/{}{rest}{query}", page::BASE, page::segment(&to));
                        return page::moved(&to);
                    }
                    let signed = self.signed(cx.env, &req).await?;
                    page::html(&page::shell(
                        &self.passkeys.site_name,
                        signed.as_ref().map(|s| s.user.name.as_str()),
                    ))
                }
                Get::Asset(body, kind) => page::asset(body, kind),
                Get::Icon => page::image(ICON, "image/png"),
                Get::Mark => page::mark(),
            };
        }
        let route = match method {
            Method::Post => Post::route(&path),
            _ => None,
        };
        let Some(route) = route else {
            return Response::error("not found", 404);
        };
        let origin = req.headers().get("Origin")?;
        let content = req.headers().get("Content-Type")?;
        // A bearer counts only on a route an app may use; there, the cookie is never read.
        let bearer = session::bearer(req.headers().get("Authorization")?.as_deref())
            .filter(|_| route.takes_bearer())
            .map(str::to_string);
        if !session::admitted(
            origin.as_deref(),
            &self.host,
            content.as_deref(),
            bearer.is_some(),
        ) {
            return answer(
                &json!({ "error": "refused: not from this site's own pages" }),
                403,
            );
        }
        let body: Value = req.json().await.unwrap_or(Value::Null);
        match route {
            Post::AppOp => match self.person(cx.env, &req, bearer.as_deref()).await? {
                Ok(person) => ops::serve(cx, &self.host, person, &body).await,
                Err(refused) => Ok(refused),
            },
            Post::SignInBegin => match self.passkeys.sign_in_begin(cx.env, SIGN_IN, None).await {
                Ok(v) => answer(&v, 200),
                Err(e) => answer(&json!({ "error": e.to_string() }), 400),
            },
            Post::SignInFinish => match self.sign_in(cx.env, &body).await {
                Ok(token) => {
                    let res = answer(&json!({ "ok": true }), 200)?;
                    res.headers().set("Set-Cookie", &session::cookie(&token))?;
                    Ok(res)
                }
                Err(e) => answer(&json!({ "error": e.to_string() }), 400),
            },
            Post::SignOut => {
                // It needs the session: a live one is ended; an expired or unknown one only has
                // its cookie cleared; with no session cookie at all it is refused.
                let header = req.headers().get("Cookie")?.unwrap_or_default();
                let token = session::token_from(&header);
                let live = match token {
                    Some(t) => {
                        let found =
                            directory(cx.env, "session.get", json!({ "hash": hash(t) })).await?;
                        serde_json::from_value::<Signed>(found).is_ok()
                    }
                    None => false,
                };
                match session::sign_out(token, live) {
                    session::SignOut::Refuse => answer(&json!({ "error": "sign in first" }), 401),
                    step => {
                        if let (session::SignOut::End, Some(token)) = (step, token) {
                            directory(cx.env, "session.remove", json!({ "hash": hash(token) }))
                                .await?;
                        }
                        let res = answer(&json!({ "ok": true }), 200)?;
                        res.headers().set("Set-Cookie", &session::clear())?;
                        Ok(res)
                    }
                }
            }
            Post::Write => {
                let Some((action, step)) = write::route(&path) else {
                    return Response::error("not found", 404);
                };
                if bearer.is_some() && !action.by_bearer {
                    return answer(
                        &json!({ "error": "this write needs the console, not an app" }),
                        403,
                    );
                }
                let person = match self.person(cx.env, &req, bearer.as_deref()).await? {
                    Ok(person) => person,
                    Err(refused) => return Ok(refused),
                };
                let who = &person;
                let (target, change) = (&body["target"], &body["change"]);
                let done = match step {
                    Step::Begin => {
                        write::begin(cx, &self.passkeys, action, who, target, change).await
                    }
                    Step::Finish => {
                        write::finish(cx, &self.passkeys, action, who, target, change, &body).await
                    }
                };
                match done {
                    Ok(v) => answer(&v, 200),
                    Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                }
            }
        }
    }

    /// Makes one of the console's own approved writes ([`write::ACTIONS`] handled by
    /// `console`).
    async fn make(&self, cx: &Context<'_>, w: &write::Write<'_>) -> Result<Value> {
        let person = w.person;
        let env = cx.env;
        let target = w.text();
        Ok(match w.action.id {
            "passkey-remove" => {
                directory(env, "passkey.remove", json!({ "id": target })).await?;
                json!({ "message": "The passkey is removed." })
            }
            "app-disconnect" => {
                let gone = directory(env, "app.disconnect", json!({ "id": target })).await?;
                json!({ "message": format!(
                    "{} is disconnected: its tokens are revoked and it acts for no one.",
                    gone["name"].as_str().unwrap_or("The app")
                ) })
            }
            "land-approve" => crate::land::approve(env, target, &person.name).await?,
            "land-approval" => crate::land::set_approval(env, target == "on", &person.name).await?,
            "approval-approve" => crate::approvals::decide(env, target, true, person).await?,
            "approval-deny" => crate::approvals::decide(env, target, false, person).await?,
            "invite" => {
                let user = directory(
                    env,
                    "users.create",
                    json!({ "kind": "standard", "name": target }),
                )
                .await?;
                let id = user["id"].as_str().unwrap_or_default();
                let code = directory(env, "enrol.create", json!({ "user_id": id })).await?;
                let link = format!(
                    "https://{}/auth/enrol?code={}",
                    self.host,
                    code.as_str().unwrap_or_default()
                );
                json!({
                    "message": format!("{target} is added. Send them this link to register a passkey: it works once, within 15 minutes."),
                    "link": link,
                    // The new console's ShowOnce (P3).
                    "reveal": {
                        "value": link,
                        "note": format!("Send this link to {target}. They register their own passkey from it, within 15 minutes."),
                        "kind": "invite",
                    },
                })
            }
            "visitor-invite" => {
                let name = w.change["name"].as_str().unwrap_or_default();
                site_access::invite(&self.host, env, &w.target, name).await?
            }
            "visitor-revoke" => {
                let user = w.change["user"].as_str().unwrap_or_default();
                site_access::revoke(&self.host, env, &w.target, user).await?
            }
            other => return Err(bad(&format!("the console makes no write `{other}`"))),
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

    /// The console's own paths and its old bookmarks' ([`page::BOOKMARKS`]); `/` itself is
    /// handed over by the platform, since a route is a prefix.
    fn routes(&self) -> Vec<&'static str> {
        let mut routes = vec!["/console/", page::BASE];
        routes.extend(page::BOOKMARKS.iter().map(|(path, ..)| *path));
        routes
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }

    fn write_check<'a>(&'a self, cx: &'a Context<'a>, w: &'a write::Write<'a>) -> Call<'a> {
        Box::pin(async move {
            match w.action.id {
                "visitor-invite" => {
                    site_access::invite_check(&self.host, cx.env, &w.target).await?;
                }
                "visitor-revoke" => {
                    site_access::selection(&self.host, cx.env, &w.target).await?;
                }
                _ => {}
            }
            Ok(Value::Null)
        })
    }

    fn write_make<'a>(&'a self, cx: &'a Context<'a>, w: &'a write::Write<'a>) -> Call<'a> {
        Box::pin(self.make(cx, w))
    }
}

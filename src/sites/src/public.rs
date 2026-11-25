//! The platform subdomains pigeon-sites serves by host, such as `noisypigeon.pigeon.dev`: each
//! one an approved instance plan attached here as a Workers Custom Domain. A request for one is
//! passed through, GET and HEAD only, to the instance Worker's workers.dev address, and its answer
//! kept in the edge cache under the instance Worker's own Cache-Control. Which hosts exist, their
//! upstreams and their sign-in config come from core (`instance_subdomains`), asked as this Worker
//! itself and kept for five minutes per isolate. No credential is on this path. Any other public
//! host is 404.
//!
//! A site that sets `auth: "passkey"` on its record is gated (docs/plans/sites.md, part A): see
//! [`pigeon_core::site_auth`] for the rules and `gated` below for the pages. A host with no
//! config is served exactly as above.
//!
//! A session's cookie holds the user's kind and id ([`site_auth::subject`]), so every request
//! checks [`site_auth::allowed`] again against the config read from core; no call to core is
//! needed per request.

use std::cell::RefCell;
use std::collections::BTreeMap;

use pigeon_contract::Answer;
use pigeon_core::design::{Page, escape};
use pigeon_core::site_auth::{self, Config, Gate, Reserved};
use serde_json::{Value, json};
use worker::{Cache, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result};

/// How long the host map is kept.
const MAP_MS: f64 = 5.0 * 60_000.0;

/// The gated pages' policy: their own script and calls, inline styles, nothing else.
const CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// A host the map knows.
#[derive(Clone)]
pub struct Host {
    /// The instance Worker's https workers.dev address.
    pub upstream: String,
    /// Set when the site opts in to passkey sign-in; `None` leaves the host public.
    pub config: Option<Config>,
    pub scope: Option<String>,
}

thread_local! {
    static MAP: RefCell<Option<(f64, BTreeMap<String, Host>)>> = const { RefCell::new(None) };
}

/// The caller this Worker signs as when it asks core: itself, a service, acting for no one.
pub fn itself(worker: &str) -> Value {
    json!({
        "user": { "id": format!("worker:{worker}"), "kind": "service", "name": worker, "created_at": 0 },
        "acting_for": null,
    })
}

/// The host map from core's `instance_subdomains` answer. Pure: only https workers.dev
/// upstreams are kept, and hosts are lowercased.
pub fn map_of(listed: &Value) -> BTreeMap<String, Host> {
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let host = e["host"].as_str()?.to_ascii_lowercase();
            let up = e["upstream"].as_str()?;
            (up.starts_with("https://") && up.ends_with(".workers.dev")).then(|| {
                let scope = e["instance"]
                    .as_str()
                    .zip(e["site"].as_str())
                    .map(|(instance, site)| site_auth::binding(instance, site, &host));
                (
                    host,
                    Host {
                        upstream: up.to_string(),
                        config: site_auth::config_of(e),
                        scope,
                    },
                )
            })
        })
        .collect()
}

/// Asks core for `tool` as this Worker itself and answers its value, or an error with core's
/// message.
async fn ask(env: &Env, key: &[u8], worker: &str, tool: &str, args: &Value) -> Result<Value> {
    let nonce = pigeon_core::webauthn::random()?;
    let answer = pigeon_contract::ask(
        &env.service("PLATFORM")?,
        "https://core",
        "dispatch",
        &json!({ "tool": tool, "args": args }),
        Some((key, &itself(worker))),
        &nonce,
    )
    .await?;
    match answer {
        Answer::Ok(v) => Ok(v),
        Answer::Error(f) => Err(worker::Error::RustError(f.message)),
    }
}

async fn host_of(env: &Env, key: &[u8], worker: &str, host: &str) -> Result<Option<Host>> {
    let now = worker::Date::now().as_millis() as f64;
    let kept = MAP.with(|m| {
        m.borrow()
            .as_ref()
            .filter(|(at, _)| now - at < MAP_MS)
            .map(|(_, map)| map.get(host).cloned())
    });
    if let Some(found) = kept {
        return Ok(found);
    }
    let listed = ask(env, key, worker, "instance_subdomains", &json!({}))
        .await
        .map_err(|e| worker::Error::RustError(format!("core's instance_subdomains: {e}")))?;
    let map = map_of(&listed);
    let found = map.get(host).cloned();
    MAP.with(|m| *m.borrow_mut() = Some((now, map)));
    Ok(found)
}

/// Serves `req` for public host `host`.
pub async fn serve(
    mut req: Request,
    env: &Env,
    key: &[u8],
    worker: &str,
    host: &str,
) -> Result<Response> {
    let host = host.to_ascii_lowercase();
    let Some(entry) = host_of(env, key, worker, &host).await? else {
        return Response::error("not found", 404);
    };
    let Some(config) = entry.config.as_ref() else {
        return pass(req, &entry.upstream, false).await;
    };
    let url = req.url()?;
    let method = match req.method() {
        Method::Get => "GET",
        Method::Head => "HEAD",
        Method::Post => "POST",
        _ => "OTHER",
    };
    let Some(scope) = entry.scope.as_deref() else {
        return no_store(Response::error(
            "site access configuration unavailable",
            503,
        )?);
    };
    let skey = site_auth::session_key(key, scope);
    let now = i64::try_from(worker::Date::now().as_millis()).unwrap_or(0);
    let signed_in = signed_in(&req, &skey, &host, config, now);
    match site_auth::gate(Some(config), method, url.path(), signed_in) {
        Gate::Pass => pass(req, &entry.upstream, true).await,
        Gate::SignIn => {
            let next = match url.query() {
                Some(q) => format!("{}?{q}", url.path()),
                None => url.path().to_string(),
            };
            page(&sign_in_page(&site_auth::safe_next(&next)), 401)
        }
        Gate::Unauthorized => no_store(Response::error("sign in required", 401)?),
        Gate::Reserved(r) => {
            let body: Value = if method == "POST" {
                req.json().await.unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            reserved(
                r, env, key, worker, &host, scope, &skey, config, &url, &body, now,
            )
            .await
        }
    }
}

/// Whether the request carries a valid session of a user the config still allows.
fn signed_in(req: &Request, skey: &[u8; 32], host: &str, config: &Config, now: i64) -> bool {
    let cookie = req.headers().get("Cookie").ok().flatten();
    let Some(session) = site_auth::cookie_value(cookie.as_deref())
        .and_then(|v| site_auth::verify(skey, host, v, now))
    else {
        return false;
    };
    site_auth::split_subject(&session.user_id)
        .is_some_and(|(kind, id)| site_auth::allowed(config, host, id, kind))
}

/// The value of query parameter `name`, or empty.
fn query(url: &worker::Url, name: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// `answer` with `Cache-Control: private, no-store`, so no shared cache keeps it.
fn no_store(answer: Response) -> Result<Response> {
    let headers = answer.headers().clone();
    headers.set("Cache-Control", "private, no-store")?;
    Ok(answer.with_headers(headers))
}

/// Passes a GET or HEAD to the upstream, through the edge cache; anything else is 405. A gated
/// host's answers are marked private and no-store.
async fn pass(req: Request, upstream: &str, gated: bool) -> Result<Response> {
    let method = req.method();
    if method != Method::Get && method != Method::Head {
        return Response::error("method not allowed", 405);
    }
    let url = req.url()?;
    let cache = Cache::default();
    let cache_key = url.to_string();
    if method == Method::Get
        && let Some(hit) = cache.get(&cache_key, false).await?
    {
        return if gated { no_store(hit) } else { Ok(hit) };
    }
    let target = match url.query() {
        Some(q) => format!("{upstream}{}?{q}", url.path()),
        None => format!("{upstream}{}", url.path()),
    };
    let headers = Headers::new();
    for name in ["Accept", "Accept-Encoding"] {
        if let Some(v) = req.headers().get(name)? {
            headers.set(name, &v)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(method.clone()).with_headers(headers);
    let mut answer = Fetch::Request(Request::new_with_init(&target, &init)?)
        .send()
        .await?;
    if method == Method::Get && answer.status_code() == 200 {
        cache.put(&cache_key, answer.cloned()?).await?;
    }
    if gated { no_store(answer) } else { Ok(answer) }
}

/// A gated host's page as a response, under the pages' security headers.
fn page(html: &str, status: u16) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Content-Security-Policy", CSP)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("Cache-Control", "private, no-store")?;
    Ok(Response::ok(html)?
        .with_headers(headers)
        .with_status(status))
}

/// A sign-in or enrolment page: a message and, optionally, one button that runs a ceremony.
fn ceremony_page(
    title: &str,
    message: &str,
    button: Option<(&str, &str, &str, &str, &str)>,
) -> String {
    let button = button.map_or(String::new(), |(base, ceremony, key, value, label)| {
        format!(
            r#"<button class="button" data-base="{}" data-ceremony="{}" data-key="{}" data-value="{}">{}</button>"#,
            escape(base),
            escape(ceremony),
            escape(key),
            escape(value),
            escape(label)
        )
    });
    let body = format!(
        r#"<main style="max-width:34rem"><span class="eyebrow">passkeys</span><h1>{}</h1><article class="card"><p>{message}</p>{button}<p data-status role="status"></p></article></main><script src="/_pigeon/passkey.js" defer></script>"#,
        escape(title)
    );
    Page {
        title,
        description: "Sign in with a passkey",
        body: &body,
    }
    .render()
}

/// The sign-in page, whose button returns to `next` (already made safe).
fn sign_in_page(next: &str) -> String {
    ceremony_page(
        "Sign in",
        "This site is private. Sign in with your passkey.",
        Some((
            "/_pigeon/sign-in",
            "get",
            "next",
            next,
            "Sign in with passkey",
        )),
    )
}

/// A JSON answer, with status 400 and `{"error"}` when `answer` is an error.
fn json_answer(answer: Result<Value>) -> Result<Response> {
    match answer {
        Ok(v) => no_store(Response::from_json(&v)?),
        Err(e) => {
            no_store(Response::from_json(&json!({ "error": e.to_string() }))?.with_status(400))
        }
    }
}

/// Answers a reserved path: the script, the ceremonies (checked by core) and sign-out.
#[allow(clippy::too_many_arguments)]
async fn reserved(
    r: Reserved,
    env: &Env,
    key: &[u8],
    worker: &str,
    host: &str,
    scope: &str,
    skey: &[u8; 32],
    config: &Config,
    url: &worker::Url,
    body: &Value,
    now: i64,
) -> Result<Response> {
    // What core is asked: the step, the host and whatever the browser posted.
    let step = |name: &str| {
        let mut args = body.clone();
        if !args.is_object() {
            args = json!({});
        }
        args["step"] = json!(name);
        args["host"] = json!(host);
        args["site_scope"] = json!(scope);
        args
    };
    let core = async |name: &'static str| ask(env, key, worker, "site_passkey", &step(name)).await;
    match r {
        Reserved::Script => {
            let headers = Headers::new();
            headers.set("Content-Type", "text/javascript; charset=utf-8")?;
            headers.set("X-Content-Type-Options", "nosniff")?;
            headers.set("Cache-Control", "private, no-store")?;
            Ok(Response::ok(pigeon_core::webauthn::BROWSER_SCRIPT)?.with_headers(headers))
        }
        Reserved::SignInPage => page(
            &sign_in_page(&site_auth::safe_next(&query(url, "next"))),
            200,
        ),
        Reserved::SignInBegin => json_answer(core("sign_in_begin").await),
        Reserved::SignInFinish => {
            let user = match core("sign_in_finish").await {
                Ok(v) if v["site_scope"] == scope => v["user"].clone(),
                Ok(_) => {
                    return no_store(Response::error("site access configuration changed", 503)?);
                }
                Err(e) => return json_answer(Err(e)),
            };
            let (id, kind) = (
                user["id"].as_str().unwrap_or_default(),
                user["kind"].as_str().unwrap_or_default(),
            );
            if !site_auth::allowed(config, host, id, kind) {
                return no_store(
                    Response::from_json(&json!({ "error": "this passkey is not allowed here" }))?
                        .with_status(403),
                );
            }
            let value = site_auth::issue(skey, host, &site_auth::subject(kind, id), now);
            let next = site_auth::safe_next(body["next"].as_str().unwrap_or_default());
            let answered = json_answer(Ok(json!({ "redirect": next })))?;
            answered
                .headers()
                .append("Set-Cookie", &site_auth::set_cookie(&value))?;
            Ok(answered)
        }
        Reserved::EnrolPage => {
            let code = query(url, "code");
            let page_html = match ask(
                env,
                key,
                worker,
                "site_passkey",
                &json!({ "step": "enrol_peek", "host": host, "site_scope": scope, "code": code }),
            )
            .await
            {
                Ok(user) => ceremony_page(
                    "Register a passkey",
                    &format!(
                        "Register a passkey for <strong>{}</strong> on this device. It is how you will sign in to {}.",
                        escape(user["name"].as_str().unwrap_or_default()),
                        escape(host)
                    ),
                    Some((
                        "/_pigeon/enrol",
                        "create",
                        "code",
                        &code,
                        "Register passkey",
                    )),
                ),
                Err(e) => ceremony_page("Link not usable", &escape(&e.to_string()), None),
            };
            page(&page_html, 200)
        }
        Reserved::EnrolBegin => json_answer(core("enrol_begin").await),
        Reserved::EnrolFinish => json_answer(core("enrol_finish").await),
        Reserved::SignOut => {
            let answered = json_answer(Ok(json!({ "redirect": "/" })))?;
            answered
                .headers()
                .append("Set-Cookie", &site_auth::clear_cookie())?;
            Ok(answered)
        }
    }
}

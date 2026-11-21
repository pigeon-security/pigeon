//! The Cloudflare API proxy: `/cf/client/v4/…` on the proxy's host, in front of
//! `api.cloudflare.com/client/v4/…`, for callers holding a proxy credential. It passes on only
//! the calls `wrangler deploy` of one Worker makes, each on that Worker of this account, and
//! refuses everything else in Cloudflare's own error shape, so wrangler prints why. Each call
//! gets a Cloudflare token of its own, holding only that call's permissions, minted when it
//! arrives and revoked once the answer is back; the caller never sees it. Wrangler reaches it
//! with `CLOUDFLARE_API_BASE_URL`. Every request is logged as one audit line.

use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{Policy, Scope, Scoped};
use serde_json::{Value, json};
use worker::js_sys::Uint8Array;
use worker::{Fetch, Headers, Method, Request, RequestInit, Response, Result, console_error};

use crate::proxy::{self, Audit, Outcome, ProxyCaller};
use crate::{Call, Capability, Context, Serve, unknown};

/// Where the proxy is mounted, standing for `https://api.cloudflare.com/client/v4`.
pub const PREFIX: &str = "/cf/client/v4";

/// The API the proxy passes calls on to.
const API: &str = "https://api.cloudflare.com/client/v4";

/// How long a token minted for one call lives: the ceiling if its revoke is lost.
const TOKEN_TTL: u64 = 300;

/// How large a request body may be: a Worker upload, with room to spare.
pub const BODY_MAX: usize = 16 * 1024 * 1024;

/// How long to wait before each retry of a call refused with 401: a token minted moments
/// before may not have reached the API yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// One call the proxy passes on.
#[derive(Debug, PartialEq, Eq)]
pub struct Allowed {
    /// Its HTTP method, matched exactly.
    pub method: &'static str,
    /// Its path after [`PREFIX`]: literal segments, `{account}` and `{script}`.
    pub path: &'static str,
    /// The `key=value` pairs its query may carry; `{queue}` stands for a queue the Worker
    /// binds.
    pub query: &'static [&'static str],
    /// The pairs of `query` it must carry.
    pub needs: &'static [&'static str],
    /// The account permissions the token minted for it holds.
    pub permissions: &'static [&'static str],
}

/// Reading the Worker.
pub const READ: &[&str] = &["Workers Scripts Read"];
/// Changing the Worker.
pub const WRITE: &[&str] = &["Workers Scripts Write"];
/// Looking up a queue the Worker produces to.
pub const QUEUES: &[&str] = &["Queues Read"];
/// Uploading the Worker, which binds Secrets Store secrets.
pub const UPLOAD: &[&str] = &["Workers Scripts Write", "Secrets Store Write"];

/// The calls `wrangler deploy` makes (wrangler 4.166.0, recorded against a stand-in API and
/// read in its source).
pub const ALLOWED: [Allowed; 11] = [
    // Whether the Worker exists, and its migration tag.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/services/{script}",
        query: &[],
        needs: &[],
        permissions: READ,
    },
    // The Worker's secrets, so a deploy can't drop one bound in the config.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/secrets",
        query: &[],
        needs: &[],
        permissions: READ,
    },
    // Its deployments, to warn about gradual rollouts.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/deployments",
        query: &[],
        needs: &[],
        permissions: READ,
    },
    // Its settings, such as tags.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/settings",
        query: &[],
        needs: &[],
        permissions: READ,
    },
    // Uploads a version, when no Durable Object migration is pending.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/versions",
        query: &["bindings_inherit=strict"],
        needs: &[],
        permissions: UPLOAD,
    },
    // Deploys that version.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/deployments",
        query: &[],
        needs: &[],
        permissions: WRITE,
    },
    // Settings outside versions: logpush, tail consumers, observability, tags.
    Allowed {
        method: "PATCH",
        path: "/accounts/{account}/workers/scripts/{script}/script-settings",
        query: &[],
        needs: &[],
        permissions: WRITE,
    },
    // Uploads and deploys at once, when a migration is pending.
    Allowed {
        method: "PUT",
        path: "/accounts/{account}/workers/scripts/{script}",
        query: &["excludeScript=true", "bindings_inherit=strict"],
        needs: &[],
        permissions: UPLOAD,
    },
    // Its workers.dev settings.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/workers/{script}",
        query: &[],
        needs: &[],
        permissions: READ,
    },
    // Whether each queue the Worker produces to exists, looked up by name (wrangler's
    // `ensureQueuesExistByConfig`). Only with the name of a queue the Worker binds.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/queues",
        query: &["name={queue}", "page=1"],
        needs: &["name={queue}"],
        permissions: QUEUES,
    },
    // Keeps workers.dev and preview URLs off, as the config says.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/subdomain",
        query: &[],
        needs: &[],
        permissions: WRITE,
    },
];

/// Whether a path segment is plain: not empty, not a dot segment, only `[A-Za-z0-9_-]`.
fn plain(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Whether `segments` match `pattern` one to one.
fn matches(pattern: &str, segments: &[&str], account: &str, script: &str) -> bool {
    let parts: Vec<&str> = pattern.split('/').skip(1).collect();
    parts.len() == segments.len()
        && parts.iter().zip(segments).all(|(p, s)| match *p {
            "{account}" => *s == account,
            "{script}" => *s == script,
            literal => *s == literal,
        })
}

/// Whether every pair of `query` is one `entry` allows, none repeated.
fn query_ok(entry: &Allowed, query: Option<&str>, queues: &[&str]) -> bool {
    // Whether `piece` is the pair `pattern` stands for.
    let is = |pattern: &str, piece: &str| {
        if pattern.contains("{queue}") {
            queues
                .iter()
                .any(|q| pattern.replace("{queue}", q) == piece)
        } else {
            pattern == piece
        }
    };
    let pieces: Vec<&str> = match query.filter(|q| !q.is_empty()) {
        Some(q) => q.split('&').collect(),
        None => Vec::new(),
    };
    let mut seen: Vec<&str> = Vec::new();
    for piece in &pieces {
        if !entry.query.iter().any(|p| is(p, piece)) || seen.contains(piece) {
            return false;
        }
        seen.push(piece);
    }
    entry
        .needs
        .iter()
        .all(|need| pieces.iter().any(|piece| is(need, piece)))
}

/// The allowed call `method path?query` is, on `account`'s Worker `script`, which produces to
/// `queues`; or why it isn't one. `path` is the raw request path, [`PREFIX`] included.
pub fn allowed(
    method: &str,
    path: &str,
    query: Option<&str>,
    account: &str,
    script: &str,
    queues: &[&str],
) -> std::result::Result<&'static Allowed, String> {
    let refused = |rest: &str| format!("pigeon's proxy does not allow {method} {rest}");
    let rest = path.strip_prefix(PREFIX).ok_or_else(|| refused(path))?;
    let segments: Vec<&str> = rest
        .strip_prefix('/')
        .ok_or_else(|| refused(rest))?
        .split('/')
        .collect();
    if !segments.iter().all(|s| plain(s)) {
        return Err(refused(rest));
    }
    let entry = ALLOWED
        .iter()
        .find(|e| e.method == method && matches(e.path, &segments, account, script))
        .ok_or_else(|| refused(rest))?;
    if !query_ok(entry, query, queues) {
        return Err("pigeon's proxy does not allow this query".into());
    }
    Ok(entry)
}

/// A refusal in the API's own shape, which wrangler prints.
fn api_error(status: u16, message: &str) -> Result<Response> {
    let body = json!({
        "success": false,
        "errors": [{ "code": 10000, "message": message }],
        "messages": [],
        "result": null,
    });
    Ok(Response::from_json(&body)?.with_status(status))
}

/// The Cloudflare API proxy for one Worker.
pub struct CfProxy {
    /// The hostname it answers on; requests to any other are not found.
    pub host: String,
    /// The Worker it deploys, such as `pigeon-platform`.
    pub script: String,
    /// The queues the Worker produces to, which a deploy looks up by name.
    pub queues: Vec<String>,
}

impl CfProxy {
    async fn serve(&self, cx: &Context<'_>, req: Request) -> Result<Response> {
        let method = req.method();
        let path = req.path();
        let mut caller = None;
        let answer = self.answer(cx, req, &mut caller).await;
        let (outcome, status, why) = match &answer {
            Ok((res, why)) => {
                let status = res.status_code();
                let outcome = match why {
                    Some(_) => Outcome::Refused,
                    None if (200..400).contains(&status) => Outcome::Forwarded,
                    None => Outcome::Failed,
                };
                (outcome, Some(status), why.clone())
            }
            Err(e) => (Outcome::Failed, Some(502), Some(e.to_string())),
        };
        proxy::audit(
            caller.as_ref(),
            &Audit {
                route: "cf",
                method: method.as_ref(),
                path: &path,
                outcome,
                status,
                why: why.as_deref(),
            },
        );
        match answer {
            Ok((res, _)) => Ok(res),
            Err(e) => {
                console_error!("cf proxy: {e}");
                api_error(502, "pigeon's proxy failed; see the platform's logs")
            }
        }
    }

    /// The answer, and why when it is a refusal.
    async fn answer(
        &self,
        cx: &Context<'_>,
        mut req: Request,
        caller: &mut Option<ProxyCaller>,
    ) -> Result<(Response, Option<String>)> {
        let refuse = |status: u16, why: &str| -> Result<(Response, Option<String>)> {
            Ok((api_error(status, why)?, Some(why.to_string())))
        };
        let url = req.url()?;
        if url.host_str() != Some(self.host.as_str()) {
            return refuse(404, "not found");
        }
        *caller = match proxy::caller(cx.env, &req).await {
            Ok(c) => Some(c),
            Err(e) => return refuse(403, &e.to_string()),
        };
        let method = req.method();
        let entry = match allowed(
            method.as_ref(),
            url.path(),
            url.query(),
            &cx.account.id,
            &self.script,
            &self.queues.iter().map(String::as_str).collect::<Vec<_>>(),
        ) {
            Ok(e) => e,
            Err(why) => return refuse(403, &why),
        };
        let too_large = req
            .headers()
            .get("Content-Length")?
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n > BODY_MAX);
        if too_large {
            return refuse(413, "the request body is too large for pigeon's proxy");
        }
        let body = match method {
            Method::Get => Vec::new(),
            _ => req.bytes().await?,
        };
        if body.len() > BODY_MAX {
            return refuse(413, "the request body is too large for pigeon's proxy");
        }
        let content_type = req.headers().get("Content-Type")?;
        let mut upstream_url = format!("{API}{}", &url.path()[PREFIX.len()..]);
        if let Some(q) = url.query().filter(|q| !q.is_empty()) {
            upstream_url.push('?');
            upstream_url.push_str(q);
        }
        let token = Scoped::open_for(
            cx.account,
            "cf-proxy",
            vec![Policy::new(Scope::Account, entry.permissions)],
            TOKEN_TTL,
        )
        .await?;
        let sent = send(
            token.value(),
            method,
            &upstream_url,
            content_type.as_deref(),
            &body,
        )
        .await;
        let (status, content_type, bytes) = token.finish(sent).await?;
        let headers = Headers::new();
        headers.set(
            "Content-Type",
            content_type.as_deref().unwrap_or("application/json"),
        )?;
        Ok((
            Response::from_bytes(bytes)?
                .with_status(status)
                .with_headers(headers),
            None,
        ))
    }
}

/// Sends one call to the API as `token`, retried while it answers 401; its status, content
/// type and body.
async fn send(
    token: &str,
    method: Method,
    url: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Result<(u16, Option<String>, Vec<u8>)> {
    let mut waits = NOT_YET_ACTIVE_WAITS_MS.iter();
    loop {
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bearer {token}"))?;
        if let Some(ct) = content_type {
            headers.set("Content-Type", ct)?;
        }
        let mut init = RequestInit::new();
        init.with_method(method.clone()).with_headers(headers);
        if !body.is_empty() {
            init.with_body(Some(Uint8Array::from(body).into()));
        }
        let mut response = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        let status = response.status_code();
        if status == 401
            && let Some(ms) = waits.next()
        {
            worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
            continue;
        }
        let content_type = response.headers().get("Content-Type")?;
        let bytes = response.bytes().await?;
        return Ok((status, content_type, bytes));
    }
}

impl Capability for CfProxy {
    fn name(&self) -> &'static str {
        "cf_proxy"
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/cf/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "4f94676cb2cbe46fd985b089ca97ea27";
    const S: &str = "pigeon-platform";
    const Q: &[&str] = &["pigeon-ci-events"];

    fn build(pattern: &str) -> String {
        format!(
            "{PREFIX}{}",
            pattern.replace("{account}", A).replace("{script}", S)
        )
    }

    fn full_query(e: &Allowed) -> Option<String> {
        (!e.query.is_empty()).then(|| e.query.join("&").replace("{queue}", Q[0]))
    }

    #[test]
    fn every_allowed_call_matches() {
        for e in &ALLOWED {
            let p = build(e.path);
            let q = full_query(e);
            assert_eq!(allowed(e.method, &p, q.as_deref(), A, S, Q), Ok(e));
            for bare in [None, Some("")] {
                let got = allowed(e.method, &p, bare, A, S, Q);
                if e.needs.is_empty() {
                    assert_eq!(got, Ok(e));
                } else {
                    assert!(got.is_err(), "{} needs {:?}", e.path, e.needs);
                }
            }
        }
    }

    #[test]
    fn variations_are_refused() {
        for e in &ALLOWED {
            let p = build(e.path);
            let q = full_query(e);
            let q = q.as_deref();
            let short = p.rsplit_once('/').map_or("", |(a, _)| a).to_string();
            for (method, path, query, account, script) in [
                (e.method, p.clone(), q, "0000", S),
                (e.method, p.clone(), q, A, "other"),
                ("DELETE", p.clone(), q, A, S),
                (e.method, format!("{p}/extra"), q, A, S),
                (e.method, format!("{p}/"), q, A, S),
                (e.method, format!("{p}/.."), q, A, S),
                (e.method, p.replace("/workers/", "/Workers/"), q, A, S),
                (e.method, p.replace(PREFIX, ""), q, A, S),
                (e.method, p.replace(PREFIX, "/cf/client/v4x"), q, A, S),
                (e.method, p.replace(PREFIX, "/cf/client/v4/"), q, A, S),
                (e.method, p.clone(), Some("foo=bar"), A, S),
            ] {
                // A variation that changes nothing this call has (a script, a `workers`
                // segment) is no variation.
                let same = (script != S && !e.path.contains("{script}"))
                    || (path == p
                        && method == e.method
                        && query == q
                        && account == A
                        && script == S);
                if same {
                    continue;
                }
                assert!(
                    allowed(method, &path, query, account, script, Q).is_err(),
                    "{method} {path} {query:?}"
                );
            }
            assert!(allowed(e.method, &short, q, A, S, Q).is_err(), "{short}");
        }
    }

    #[test]
    fn queries_are_checked_pair_by_pair() {
        let put = build("/accounts/{account}/workers/scripts/{script}");
        let ver = build("/accounts/{account}/workers/scripts/{script}/versions");
        assert!(
            allowed(
                "PUT",
                &put,
                Some("bindings_inherit=strict&excludeScript=true"),
                A,
                S,
                Q
            )
            .is_ok()
        );
        for q in [
            "bindings_inherit=loose",
            "bindings_inherit=strict&bindings_inherit=strict",
            "excludeScript=false",
            "bindings_inherit=strict&x=1",
        ] {
            assert!(allowed("PUT", &put, Some(q), A, S, Q).is_err(), "{q}");
        }
        assert!(allowed("POST", &ver, Some("excludeScript=true"), A, S, Q).is_err());
    }

    #[test]
    fn a_queue_is_looked_up_only_by_a_name_the_worker_binds() {
        let p = format!("{PREFIX}/accounts/{A}/queues");
        for q in [
            "name=pigeon-ci-events&page=1",
            "name=pigeon-ci-events",
            "page=1&name=pigeon-ci-events",
        ] {
            assert!(allowed("GET", &p, Some(q), A, S, Q).is_ok(), "{q}");
        }
        for q in [
            "page=1",
            "name=other",
            "name=pigeon-ci-events&name=other",
            "name=pigeon-ci-events&page=2",
            "name=pigeon-ci-events&per_page=1000",
            "name=pigeon-ci-events&name=pigeon-ci-events",
        ] {
            assert!(allowed("GET", &p, Some(q), A, S, Q).is_err(), "{q}");
        }
        assert!(allowed("GET", &p, Some("name=pigeon-ci-events"), A, S, &[]).is_err());
        assert!(allowed("POST", &p, Some("name=pigeon-ci-events"), A, S, Q).is_err());
        let consumers = format!("{PREFIX}/accounts/{A}/queues/q1/consumers");
        assert!(allowed("POST", &consumers, None, A, S, Q).is_err());
    }

    #[test]
    fn nothing_else_on_the_account_is_reachable() {
        for (m, p) in [
            ("GET", format!("{PREFIX}/accounts/{A}/tokens")),
            ("POST", format!("{PREFIX}/accounts/{A}/tokens")),
            (
                "DELETE",
                build("/accounts/{account}/workers/scripts/{script}"),
            ),
            ("GET", format!("{PREFIX}/user")),
            ("GET", format!("{PREFIX}/user/tokens/verify")),
            ("GET", format!("{PREFIX}/memberships")),
            ("GET", format!("{PREFIX}/accounts")),
            (
                "PUT",
                format!("{PREFIX}/accounts/{A}/workers/scripts/pigeon"),
            ),
            ("GET", format!("{PREFIX}/accounts/{A}/workers/scripts")),
            ("GET", format!("{PREFIX}/accounts/{A}/secrets_store/stores")),
        ] {
            assert!(allowed(m, &p, None, A, S, Q).is_err(), "{m} {p}");
        }
    }

    /// The calls recorded from `wrangler deploy` of `src/platform` against a stand-in API, both
    /// ways it deploys, all pass.
    #[test]
    fn a_recorded_deploy_passes() {
        let recorded = [
            "GET /accounts/{A}/workers/services/pigeon-platform",
            "GET /accounts/{A}/workers/scripts/pigeon-platform/secrets",
            "GET /accounts/{A}/workers/scripts/pigeon-platform/deployments",
            "GET /accounts/{A}/workers/scripts/pigeon-platform/settings",
            "POST /accounts/{A}/workers/scripts/pigeon-platform/versions?bindings_inherit=strict",
            "POST /accounts/{A}/workers/scripts/pigeon-platform/deployments",
            "PATCH /accounts/{A}/workers/scripts/pigeon-platform/script-settings",
            "GET /accounts/{A}/workers/workers/pigeon-platform",
            "POST /accounts/{A}/workers/scripts/pigeon-platform/subdomain",
            "PUT /accounts/{A}/workers/scripts/pigeon-platform?excludeScript=true&bindings_inherit=strict",
            "GET /accounts/{A}/queues?name=pigeon-ci-events&page=1",
        ];
        for call in recorded {
            let call = call.replace("{A}", A);
            let (method, target) = call.split_once(' ').unwrap();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p, Some(q)),
                None => (target, None),
            };
            let path = format!("{PREFIX}{path}");
            assert!(allowed(method, &path, query, A, S, Q).is_ok(), "{call}");
        }
    }
}

//! The Cloudflare API proxy: `/cf/client/v4/…` on the proxy's host, in front of
//! `api.cloudflare.com/client/v4/…`, for callers holding a proxy credential. It passes on only
//! the calls `wrangler deploy` of one Worker makes, each on that Worker of this account (and,
//! for its containers, only its own container namespaces and images in the account's
//! registry, checked in the call's body too), and
//! refuses everything else in Cloudflare's own error shape, so wrangler prints why. Each call
//! gets a Cloudflare token of its own, holding only that call's permissions, minted when it
//! arrives and revoked once the answer is back; the caller never sees it. Wrangler reaches it
//! with `CLOUDFLARE_API_BASE_URL`. Every request is logged as one audit line.
//!
//! Besides a proxy credential, it takes one other bearer: the CI job token of the deploy job of
//! the live landing, while that landing is `deploying` and for its commit only
//! (`land::deploy_caller`). Its calls pass the same allowlist. `GET /cf/grant` asks only that:
//! 200 for the live deploying landing's job token, 403 otherwise, so a deploy job not started by
//! a landing stops before its build.

use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{self, Policy, Scoped};
use serde_json::{Value, json};
use worker::js_sys::Uint8Array;
use worker::{Fetch, Headers, Method, Request, RequestInit, Response, Result, console_error};

use crate::proxy::{self, Audit, Outcome, ProxyCaller};
use crate::{Call, Capability, Context, Serve, unknown};

/// Where the proxy is mounted, standing for `https://api.cloudflare.com/client/v4`.
pub const PREFIX: &str = "/cf/client/v4";

/// The pre-flight route: `GET /cf/grant` tells a CI job whether its job token is the deploy grant
/// of the live landing. It sits outside [`PREFIX`], never reaches the allowlist and never calls
/// the API.
pub const GRANT_PATH: &str = "/cf/grant";

/// Why a job token is turned away from this proxy.
const JOB_TOKEN_REFUSED: &str = "a job token reaches this proxy only as the deploy job of a landing that is deploying its commit";

/// The answer of `GET /cf/grant`, as a status and a message: `method` is the request method,
/// `job_token` whether the bearer is a CI job token, `granted` whether that token is the deploy
/// grant of the live landing deploying its commit.
pub fn grant_answer(method: &str, job_token: bool, granted: bool) -> (u16, &'static str) {
    if method != "GET" {
        (405, "only GET answers /cf/grant")
    } else if !job_token {
        (403, "/cf/grant answers only a CI job token")
    } else if !granted {
        (403, JOB_TOKEN_REFUSED)
    } else {
        (
            200,
            "this job token is the deploy grant of the landing deploying its commit",
        )
    }
}

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
    /// What its body must be.
    pub body: BodyRule,
}

/// Reading the Worker.
pub const READ: &[&str] = &["Workers Scripts Read"];
/// Changing the Worker.
pub const WRITE: &[&str] = &["Workers Scripts Write"];
/// Reading the Worker's container applications.
pub const CONTAINERS_READ: &[&str] = &["Workers Containers Read"];
/// Preparing its container image, and creating or changing its container applications.
pub const CONTAINERS_WRITE: &[&str] = &["Workers Containers Write"];
/// Looking up a queue the Worker produces to.
pub const QUEUES: &[&str] = &["Queues Read"];
/// Uploading the Worker, which binds Secrets Store secrets.
pub const UPLOAD: &[&str] = &["Workers Scripts Write", "Secrets Store Write"];

/// The calls `wrangler deploy` makes (wrangler 4.166.0, recorded against a stand-in API and
/// read in its source).
pub const ALLOWED: [Allowed; 16] = [
    // Whether the Worker exists, and its migration tag.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/services/{script}",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // The Worker's secrets, so a deploy can't drop one bound in the config.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/secrets",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // Its deployments, to warn about gradual rollouts.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/deployments",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // Its settings, such as tags.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/settings",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // Uploads a version, when no Durable Object migration is pending.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/versions",
        query: &["bindings_inherit=strict"],
        needs: &[],
        permissions: UPLOAD,
        body: BodyRule::Any,
    },
    // Deploys that version.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/deployments",
        query: &[],
        needs: &[],
        permissions: WRITE,
        body: BodyRule::Any,
    },
    // Settings outside versions: logpush, tail consumers, observability, tags.
    Allowed {
        method: "PATCH",
        path: "/accounts/{account}/workers/scripts/{script}/script-settings",
        query: &[],
        needs: &[],
        permissions: WRITE,
        body: BodyRule::Any,
    },
    // Uploads and deploys at once, when a migration is pending.
    Allowed {
        method: "PUT",
        path: "/accounts/{account}/workers/scripts/{script}",
        query: &["excludeScript=true", "bindings_inherit=strict"],
        needs: &[],
        permissions: UPLOAD,
        body: BodyRule::Any,
    },
    // Its workers.dev settings.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/workers/{script}",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // Whether each queue the Worker produces to exists, looked up by name (wrangler's
    // `ensureQueuesExistByConfig`). Only with the name of a queue the Worker binds.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/queues",
        query: &["name={queue}", "page=1"],
        needs: &["name={queue}"],
        permissions: QUEUES,
        body: BodyRule::Any,
    },
    // Containers (a `[[containers]]` block; wrangler's `deployDurableObjectContainerApplications`
    // after the upload, which a Worker with containers always makes by the `PUT` above).
    // Readies the container image, polled until ready; only an image under this account's
    // registry.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/containers/image-preparations",
        query: &[],
        needs: &[],
        permissions: CONTAINERS_WRITE,
        body: BodyRule::Image,
    },
    // The uploaded version, for the namespace ids of its Durable Object bindings.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/scripts/{script}/versions/{version}",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
    },
    // The container application of one of this Worker's container classes, by namespace id.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/containers/applications/{namespace}",
        query: &[],
        needs: &[],
        permissions: CONTAINERS_READ,
        body: BodyRule::Any,
    },
    // Creates it, on the first deploy: only for one of those namespaces.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/containers/applications",
        query: &[],
        needs: &[],
        permissions: CONTAINERS_WRITE,
        body: BodyRule::ApplicationCreate,
    },
    // Changes its settings, when the config's differ.
    Allowed {
        method: "PATCH",
        path: "/accounts/{account}/containers/applications/{namespace}",
        query: &[],
        needs: &[],
        permissions: CONTAINERS_WRITE,
        body: BodyRule::ApplicationPatch,
    },
    // Keeps workers.dev and preview URLs off, as the config says.
    Allowed {
        method: "POST",
        path: "/accounts/{account}/workers/scripts/{script}/subdomain",
        query: &[],
        needs: &[],
        permissions: WRITE,
        body: BodyRule::Any,
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

/// What the proxy's calls may name: the account, the Worker, the queues it produces to and
/// the Durable Object namespaces of its container classes.
pub struct Scope<'a> {
    pub account: &'a str,
    pub script: &'a str,
    pub queues: &'a [&'a str],
    /// Found by the proxy itself, and only for container application calls; empty otherwise.
    pub namespaces: &'a [&'a str],
}

/// Whether `s` is a Worker version id: a UUID, with or without its hyphens.
fn is_version(s: &str) -> bool {
    let hex = s.bytes().filter(|b| *b != b'-').count();
    (s.len() == 36 || s.len() == 32)
        && hex == 32
        && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        && (s.len() == 32 || [8, 13, 18, 23].iter().all(|i| s.as_bytes()[*i] == b'-'))
}

/// Whether `segments` match `pattern` one to one.
fn matches(pattern: &str, segments: &[&str], scope: &Scope<'_>) -> bool {
    let parts: Vec<&str> = pattern.split('/').skip(1).collect();
    parts.len() == segments.len()
        && parts.iter().zip(segments).all(|(p, s)| match *p {
            "{account}" => *s == scope.account,
            "{script}" => *s == scope.script,
            "{version}" => is_version(s),
            "{namespace}" => scope.namespaces.contains(s),
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

/// The allowed call `method path?query` is, within `scope`; or why it isn't one. `path` is the raw request path, [`PREFIX`] included.
pub fn allowed(
    method: &str,
    path: &str,
    query: Option<&str>,
    scope: &Scope<'_>,
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
        .find(|e| e.method == method && matches(e.path, &segments, scope))
        .ok_or_else(|| refused(rest))?;
    if !query_ok(entry, query, scope.queues) {
        return Err("pigeon's proxy does not allow this query".into());
    }
    Ok(entry)
}

/// The calls in `recorded` the proxy would refuse within `scope`, each with why. A recorded call
/// is one line, `METHOD /path?query`, the path after the API's `/client/v4`, and optionally
/// ` body=<base64>` holding its JSON body, which is then checked against the call's body rule:
/// what `ci/deploy/standin.mjs` writes while a real `wrangler deploy` runs against it.
pub fn unlisted(recorded: &[&str], scope: &Scope<'_>) -> Vec<String> {
    let mut out = Vec::new();
    for line in recorded.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let (call, body) = match line.split_once(" body=") {
            Some((c, b)) => (c, Some(b)),
            None => (line, None),
        };
        let Some((method, target)) = call.split_once(' ') else {
            out.push(format!("{line}: not a recorded call"));
            continue;
        };
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (target, None),
        };
        let entry = match allowed(method, &format!("{PREFIX}{path}"), query, scope) {
            Ok(e) => e,
            Err(why) => {
                out.push(format!("{call}: {why}"));
                continue;
            }
        };
        let Some(body) = body else { continue };
        match unbase64(body) {
            Some(bytes) => {
                if let Err(why) = body_ok(entry.body, &bytes, scope) {
                    out.push(format!("{call}: {why}"));
                }
            }
            None => out.push(format!("{call}: its recorded body is not base64")),
        }
    }
    out
}

/// Standard base64, padded or not; `None` when not base64.
fn unbase64(s: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        Some(u32::from(v))
    };
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// What a call's JSON body must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyRule {
    /// Anything: the body is not JSON, or needs no check.
    Any,
    /// `{"image": "registry.cloudflare.com/<account>/<name>[:<tag>|@sha256:<64 hex>]"}`.
    Image,
    /// A container application created for one of the scope's namespaces.
    ApplicationCreate,
    /// Changes to an existing application's settings.
    ApplicationPatch,
}

const BODY_REFUSED: &str = "pigeon's proxy does not allow this body: ";

/// The settings an application may be created with or changed to.
const SETTINGS_KEYS: [&str; 6] = [
    "configuration",
    "observability",
    "instances",
    "max_instances",
    "constraints",
    "rollout_active_grace_period",
];

fn body_refused(why: &str) -> std::result::Result<(), String> {
    Err(format!("{BODY_REFUSED}{why}"))
}

/// Whether `image` is in `account`'s Cloudflare registry: a name, then a tag or a digest.
fn image_ok(image: &str, account: &str) -> bool {
    if account.is_empty() || account.contains('/') {
        return false;
    }
    let prefix = format!("registry.cloudflare.com/{account}/");
    let Some(rest) = image.strip_prefix(prefix.as_str()) else {
        return false;
    };
    let (name, suffix_ok) = if let Some((name, digest)) = rest.split_once('@') {
        let ok = digest.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        (name, ok)
    } else if let Some((name, tag)) = rest.split_once(':') {
        let ok = !tag.is_empty()
            && tag.len() <= 128
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        (name, ok)
    } else {
        (rest, true)
    };
    suffix_ok
        && !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'/' | b'-')
        })
        && !name.contains("..")
        && !name.contains("//")
        && !name.starts_with('/')
        && !name.ends_with('/')
}

fn json_object(body: &[u8]) -> std::result::Result<serde_json::Map<String, Value>, String> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(format!("{BODY_REFUSED}not a JSON object")),
    }
}

/// A `configuration.image`, when there is one, must be in the account's registry.
fn configuration_ok(
    obj: &serde_json::Map<String, Value>,
    account: &str,
) -> std::result::Result<(), String> {
    match obj.get("configuration") {
        None => Ok(()),
        Some(Value::Object(c)) => match c.get("image") {
            None => Ok(()),
            Some(Value::String(s)) if image_ok(s, account) => Ok(()),
            Some(_) => body_refused("an image outside the account's registry"),
        },
        Some(_) => body_refused("configuration is not an object"),
    }
}

/// Whether `body` is what `rule` allows, within `scope`.
pub fn body_ok(rule: BodyRule, body: &[u8], scope: &Scope<'_>) -> std::result::Result<(), String> {
    match rule {
        BodyRule::Any => Ok(()),
        BodyRule::Image => {
            let obj = json_object(body)?;
            if obj.len() != 1 {
                return body_refused("only an image is expected");
            }
            match obj.get("image") {
                Some(Value::String(s)) if image_ok(s, scope.account) => Ok(()),
                _ => body_refused("an image outside the account's registry"),
            }
        }
        BodyRule::ApplicationCreate => {
            let obj = json_object(body)?;
            let known = |k: &str| {
                matches!(k, "name" | "scheduling_policy" | "durable_objects")
                    || SETTINGS_KEYS.contains(&k)
            };
            if !obj.keys().all(|k| known(k)) {
                return body_refused("an unexpected field");
            }
            if obj.get("scheduling_policy").and_then(Value::as_str) != Some("durable_object") {
                return body_refused("scheduling_policy must be durable_object");
            }
            match obj.get("durable_objects") {
                Some(Value::Object(d))
                    if d.len() == 1
                        && d.get("namespace_id")
                            .and_then(Value::as_str)
                            .is_some_and(|ns| scope.namespaces.contains(&ns)) => {}
                _ => return body_refused("not one of this Worker's container namespaces"),
            }
            let name_ok = obj.get("name").and_then(Value::as_str).is_some_and(|n| {
                !n.is_empty()
                    && n.len() <= 128
                    && n.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            });
            if !name_ok {
                return body_refused("a bad application name");
            }
            configuration_ok(&obj, scope.account)
        }
        BodyRule::ApplicationPatch => {
            let obj = json_object(body)?;
            if obj.keys().any(|k| !SETTINGS_KEYS.contains(&k.as_str())) {
                return body_refused("an unexpected field");
            }
            configuration_ok(&obj, scope.account)
        }
    }
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
    /// The Worker's container classes, such as `CiRunner`: their Durable Object namespaces
    /// are the container applications a deploy may read, create and change.
    pub containers: Vec<String>,
    /// The repository landings deploy from, as `namespace/name`: a deploy job's token is taken
    /// only when its grant is for this repository.
    pub land_repo: String,
}

/// The bearer token `req` carries, if any.
fn bearer(req: &Request) -> Result<Option<String>> {
    Ok(req.headers().get("Authorization")?.and_then(|h| {
        let (scheme, rest) = h.trim().split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| rest.trim().to_string())
    }))
}

/// The Durable Object namespace ids of `script`'s `classes`, looked up with a token minted for
/// it and revoked after.
async fn container_namespaces(
    account: &pigeon_core::Account,
    script: &str,
    classes: &[String],
) -> Result<Vec<String>> {
    if classes.is_empty() {
        return Ok(Vec::new());
    }
    let token = Scoped::open_for(
        account,
        "cf-proxy-lookup",
        vec![Policy::new(tokens::Scope::Account, READ)],
        TOKEN_TTL,
    )
    .await?;
    let path = format!(
        "/accounts/{}/workers/durable_objects/namespaces?per_page=1000",
        account.id
    );
    let listed = pigeon_core::cloudflare::call_fresh(token.value(), Method::Get, &path, None)
        .await
        .map(|v| namespaces_of(&v, script, classes));
    token.finish(listed).await
}

/// The ids of the namespaces in `listed` (the API's list) that belong to `script`'s `classes`.
pub fn namespaces_of(listed: &Value, script: &str, classes: &[String]) -> Vec<String> {
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| {
            n["script"].as_str() == Some(script)
                && n["class"]
                    .as_str()
                    .is_some_and(|c| classes.iter().any(|k| k == c))
        })
        .filter_map(|n| n["id"].as_str().map(String::from))
        .collect()
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
        if url.path() == GRANT_PATH {
            let token = bearer(&req)?.filter(|t| t.starts_with(crate::ci::token::PREFIX));
            let job_token = token.is_some();
            let method = req.method();
            let name: &str = method.as_ref();
            let found = match &token {
                Some(t) if name == "GET" => {
                    crate::land::deploy_caller(cx.env, t, &self.land_repo).await?
                }
                _ => None,
            };
            let granted = found.is_some();
            *caller = found;
            let (status, msg) = grant_answer(name, job_token, granted);
            if status == 200 {
                return Ok((
                    Response::from_json(&json!({
                        "success": true,
                        "errors": [],
                        "messages": [],
                        "result": {"granted": true, "message": msg}
                    }))?
                    .with_status(200),
                    None,
                ));
            }
            return refuse(status, msg);
        }
        *caller = match bearer(&req)? {
            // A landing's deploy job, with its own job token.
            Some(t) if t.starts_with(crate::ci::token::PREFIX) => {
                match crate::land::deploy_caller(cx.env, &t, &self.land_repo).await? {
                    Some(c) => Some(c),
                    None => {
                        return refuse(403, JOB_TOKEN_REFUSED);
                    }
                }
            }
            _ => match proxy::caller(cx.env, &req).await {
                Ok(c) => Some(c),
                Err(e) => return refuse(403, &e.to_string()),
            },
        };
        let method = req.method();
        // Container application calls name this Worker's container namespaces, so they are
        // looked up first; no other call needs them.
        let namespaces = if url.path().contains("/containers/applications") {
            container_namespaces(cx.account, &self.script, &self.containers).await?
        } else {
            Vec::new()
        };
        let queues: Vec<&str> = self.queues.iter().map(String::as_str).collect();
        let namespaces: Vec<&str> = namespaces.iter().map(String::as_str).collect();
        let scope = Scope {
            account: &cx.account.id,
            script: &self.script,
            queues: &queues,
            namespaces: &namespaces,
        };
        let entry = match allowed(method.as_ref(), url.path(), url.query(), &scope) {
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
        if let Err(why) = body_ok(entry.body, &body, &scope) {
            return refuse(403, &why);
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
            vec![Policy::new(tokens::Scope::Account, entry.permissions)],
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
    const N: &[&str] = &["ns1"];

    fn scope<'a>(account: &'a str, script: &'a str) -> Scope<'a> {
        Scope {
            account,
            script,
            queues: Q,
            namespaces: N,
        }
    }

    fn build(pattern: &str) -> String {
        format!(
            "{PREFIX}{}",
            pattern
                .replace("{account}", A)
                .replace("{script}", S)
                .replace("{version}", "01234567-89ab-cdef-0123-456789abcdef")
                .replace("{namespace}", N[0])
        )
    }

    fn full_query(e: &Allowed) -> Option<String> {
        (!e.query.is_empty()).then(|| e.query.join("&").replace("{queue}", Q[0]))
    }

    #[test]
    fn the_grant_route_answers_only_a_deploying_landings_job_token() {
        assert_eq!(grant_answer("POST", true, true).0, 405);
        assert_eq!(grant_answer("GET", false, false).0, 403);
        assert_eq!(grant_answer("GET", false, true).0, 403);
        assert_eq!(grant_answer("GET", true, false), (403, JOB_TOKEN_REFUSED));
        assert_eq!(grant_answer("GET", true, true).0, 200);
        assert!(!GRANT_PATH.starts_with(PREFIX));
        assert!(allowed("GET", GRANT_PATH, None, &scope(A, S)).is_err());
    }

    #[test]
    fn every_allowed_call_matches() {
        for e in &ALLOWED {
            let p = build(e.path);
            let q = full_query(e);
            assert_eq!(allowed(e.method, &p, q.as_deref(), &scope(A, S)), Ok(e));
            for bare in [None, Some("")] {
                let got = allowed(e.method, &p, bare, &scope(A, S));
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
                    allowed(method, &path, query, &scope(account, script)).is_err(),
                    "{method} {path} {query:?}"
                );
            }
            assert!(
                allowed(e.method, &short, q, &scope(A, S)).is_err(),
                "{short}"
            );
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
                &scope(A, S)
            )
            .is_ok()
        );
        for q in [
            "bindings_inherit=loose",
            "bindings_inherit=strict&bindings_inherit=strict",
            "excludeScript=false",
            "bindings_inherit=strict&x=1",
        ] {
            assert!(allowed("PUT", &put, Some(q), &scope(A, S)).is_err(), "{q}");
        }
        assert!(allowed("POST", &ver, Some("excludeScript=true"), &scope(A, S)).is_err());
    }

    #[test]
    fn a_queue_is_looked_up_only_by_a_name_the_worker_binds() {
        let p = format!("{PREFIX}/accounts/{A}/queues");
        for q in [
            "name=pigeon-ci-events&page=1",
            "name=pigeon-ci-events",
            "page=1&name=pigeon-ci-events",
        ] {
            assert!(allowed("GET", &p, Some(q), &scope(A, S)).is_ok(), "{q}");
        }
        for q in [
            "page=1",
            "name=other",
            "name=pigeon-ci-events&name=other",
            "name=pigeon-ci-events&page=2",
            "name=pigeon-ci-events&per_page=1000",
            "name=pigeon-ci-events&name=pigeon-ci-events",
        ] {
            assert!(allowed("GET", &p, Some(q), &scope(A, S)).is_err(), "{q}");
        }
        assert!(
            allowed(
                "GET",
                &p,
                Some("name=pigeon-ci-events"),
                &Scope {
                    queues: &[],
                    ..scope(A, S)
                },
            )
            .is_err()
        );
        assert!(allowed("POST", &p, Some("name=pigeon-ci-events"), &scope(A, S)).is_err());
        let consumers = format!("{PREFIX}/accounts/{A}/queues/q1/consumers");
        assert!(allowed("POST", &consumers, None, &scope(A, S)).is_err());
    }

    #[test]
    fn container_calls_name_only_this_workers_namespaces_and_images() {
        let app = |ns: &str| format!("{PREFIX}/accounts/{A}/containers/applications/{ns}");
        assert!(allowed("GET", &app("ns1"), None, &scope(A, S)).is_ok());
        assert!(allowed("PATCH", &app("ns1"), None, &scope(A, S)).is_ok());
        for (m, p) in [
            ("GET", app("ns2")),
            ("PATCH", app("ns2")),
            ("DELETE", app("ns1")),
            ("GET", app("ns1/status")),
            ("POST", app("ns1/jobs")),
            (
                "GET",
                format!("{PREFIX}/accounts/{A}/containers/applications"),
            ),
            ("GET", format!("{PREFIX}/accounts/{A}/containers/me")),
            (
                "GET",
                format!("{PREFIX}/accounts/{A}/workers/durable_objects/namespaces"),
            ),
            (
                "GET",
                format!("{PREFIX}/accounts/{A}/workers/scripts/{S}/versions/latest"),
            ),
            (
                "GET",
                format!("{PREFIX}/accounts/{A}/workers/scripts/{S}/versions/0123"),
            ),
            (
                "GET",
                format!(
                    "{PREFIX}/accounts/{A}/workers/scripts/other/versions/0123456789abcdef0123456789abcdef"
                ),
            ),
        ] {
            assert!(allowed(m, &p, None, &scope(A, S)).is_err(), "{m} {p}");
        }
        // No namespaces found: no application call passes.
        let none = Scope {
            namespaces: &[],
            ..scope(A, S)
        };
        assert!(allowed("GET", &app("ns1"), None, &none).is_err());
        let v = format!(
            "{PREFIX}/accounts/{A}/workers/scripts/{S}/versions/0123456789abcdef0123456789abcdef"
        );
        assert!(allowed("GET", &v, None, &scope(A, S)).is_ok());
    }

    #[test]
    fn container_bodies_are_checked() {
        let sc = scope(A, S);
        let digest = "a".repeat(64);
        let img = |i: &str| format!(r#"{{"image":"{i}"}}"#);
        let ours = format!("registry.cloudflare.com/{A}/pigeon-ci-base@sha256:{digest}");
        assert!(body_ok(BodyRule::Image, img(&ours).as_bytes(), &sc).is_ok());
        let tagged = format!("registry.cloudflare.com/{A}/pigeon-ci-base:v1");
        assert!(body_ok(BodyRule::Image, img(&tagged).as_bytes(), &sc).is_ok());
        for bad in [
            img("registry.cloudflare.com/other/x:v1"),
            img("docker.io/library/alpine"),
            img(&format!("registry.cloudflare.com/{A}/../x:v1")),
            img(&format!("registry.cloudflare.com/{A}/X:v1")),
            img(&format!("registry.cloudflare.com/{A}/x/")),
            img(&format!("registry.cloudflare.com/{A}/x@sha256:abc")),
            img(&format!("registry.cloudflare.com/{A}/x@a@sha256:{digest}")),
            format!(r#"{{"image":"{ours}","x":1}}"#),
            r#"{"image":1}"#.to_string(),
            "{}".to_string(),
            "not json".to_string(),
        ] {
            assert!(
                body_ok(BodyRule::Image, bad.as_bytes(), &sc).is_err(),
                "{bad}"
            );
        }
        let create = |ns: &str, extra: &str| {
            format!(
                r#"{{"name":"pigeon-platform-cirunner","scheduling_policy":"durable_object","durable_objects":{{"namespace_id":"{ns}"}}{extra}}}"#
            )
        };
        assert!(
            body_ok(
                BodyRule::ApplicationCreate,
                create("ns1", "").as_bytes(),
                &sc
            )
            .is_ok()
        );
        let with_image = create("ns1", &format!(r#","configuration":{{"image":"{ours}"}}"#));
        assert!(body_ok(BodyRule::ApplicationCreate, with_image.as_bytes(), &sc).is_ok());
        for bad in [
            create("ns2", ""),
            create("ns1", r#","account_id":"x""#),
            create("ns1", r#","configuration":{"image":"docker.io/x"}"#),
            r#"{"name":"a","durable_objects":{"namespace_id":"ns1"}}"#.to_string(),
            r#"{"name":"a","scheduling_policy":"default","durable_objects":{"namespace_id":"ns1"}}"#.to_string(),
            r#"{"name":"a","scheduling_policy":"durable_object","durable_objects":{"namespace_id":"ns1","x":1}}"#.to_string(),
        ] {
            assert!(body_ok(BodyRule::ApplicationCreate, bad.as_bytes(), &sc).is_err(), "{bad}");
        }
        let none = Scope {
            namespaces: &[],
            ..scope(A, S)
        };
        assert!(
            body_ok(
                BodyRule::ApplicationCreate,
                create("ns1", "").as_bytes(),
                &none
            )
            .is_err()
        );
        let patch = r#"{"configuration":{"instance_type":"basic"},"max_instances":2}"#;
        assert!(body_ok(BodyRule::ApplicationPatch, patch.as_bytes(), &sc).is_ok());
        for bad in [
            r#"{"name":"x"}"#,
            r#"{"durable_objects":{"namespace_id":"ns1"}}"#,
            r#"{"configuration":{"image":"docker.io/x"}}"#,
        ] {
            assert!(
                body_ok(BodyRule::ApplicationPatch, bad.as_bytes(), &sc).is_err(),
                "{bad}"
            );
        }
        assert!(body_ok(BodyRule::Any, b"\xff\x00anything", &sc).is_ok());
    }

    /// The bodies wrangler 4.166.0 sent for `src/platform`'s container, recorded against the
    /// stand-in.
    #[test]
    fn recorded_container_bodies_pass() {
        let sc = scope(A, S);
        let image = format!(
            r#"{{"image":"registry.cloudflare.com/{A}/pigeon-ci-base@sha256:195f9463688ba1b5c4973fcd3c3c83463ec428a9d88522c3ae71425f676d262b"}}"#
        );
        assert!(body_ok(BodyRule::Image, image.as_bytes(), &sc).is_ok());
        let create = r#"{"name":"pigeon-platform-cirunner","scheduling_policy":"durable_object","durable_objects":{"namespace_id":"ns1"}}"#;
        assert!(body_ok(BodyRule::ApplicationCreate, create.as_bytes(), &sc).is_ok());
    }

    #[test]
    fn namespaces_are_this_workers_container_classes() {
        let listed = json!([
            { "id": "ns1", "class": "CiRunner", "script": "pigeon-platform" },
            { "id": "ns2", "class": "Directory", "script": "pigeon-platform" },
            { "id": "ns3", "class": "CiRunner", "script": "other" },
            { "id": "ns4", "class": "CiRunner" },
        ]);
        let classes = vec!["CiRunner".to_string()];
        assert_eq!(namespaces_of(&listed, "pigeon-platform", &classes), ["ns1"]);
        assert!(namespaces_of(&json!({}), "pigeon-platform", &classes).is_empty());
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
            assert!(allowed(m, &p, None, &scope(A, S)).is_err(), "{m} {p}");
        }
    }

    #[test]
    fn unlisted_names_each_refused_call_and_body() {
        let image = format!(r#"{{"image":"registry.cloudflare.com/{A}/pigeon-ci-base:v3"}}"#);
        let other = r#"{"image":"docker.io/library/alpine"}"#;
        let b64 = |s: &str| pigeon_core::git::base64(s.as_bytes());
        let recorded = [
            format!("GET /accounts/{A}/workers/services/{S}"),
            String::new(),
            format!("GET /accounts/{A}/queues?name=pigeon-ci-events&page=1"),
            format!(
                "POST /accounts/{A}/containers/image-preparations body={}",
                b64(&image)
            ),
            format!(
                "POST /accounts/{A}/containers/image-preparations body={}",
                b64(other)
            ),
            format!("GET /accounts/{A}/workers/durable_objects/namespaces?per_page=1000"),
            format!("DELETE /accounts/{A}/workers/scripts/{S}"),
            format!("POST /accounts/{A}/containers/image-preparations body=***"),
            "nonsense".to_string(),
        ];
        let lines: Vec<&str> = recorded.iter().map(String::as_str).collect();
        let found = unlisted(&lines, &scope(A, S));
        assert_eq!(found.len(), 5, "{found:#?}");
        assert!(found[0].contains("an image outside the account's registry"));
        assert!(found[1].starts_with("GET /accounts/") && found[1].contains("namespaces"));
        assert!(found[2].starts_with("DELETE"));
        assert!(found[3].contains("not base64"));
        assert!(found[4].contains("not a recorded call"));
        assert!(unlisted(&lines[..4], &scope(A, S)).is_empty());
    }

    #[test]
    fn unbase64_reads_what_base64_writes() {
        for s in ["", "a", "ab", "abc", "{\"image\":\"x\"}"] {
            assert_eq!(
                unbase64(&pigeon_core::git::base64(s.as_bytes())).as_deref(),
                Some(s.as_bytes())
            );
        }
        assert_eq!(unbase64("a*b"), None);
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
            // With a [[containers]] block (recorded against the stand-in, first and later
            // deploys).
            "POST /accounts/{A}/containers/image-preparations",
            "GET /accounts/{A}/workers/scripts/pigeon-platform/versions/01234567-89ab-cdef-0123-456789abcdef",
            "GET /accounts/{A}/containers/applications/ns1",
            "POST /accounts/{A}/containers/applications",
        ];
        for call in recorded {
            let call = call.replace("{A}", A);
            let (method, target) = call.split_once(' ').unwrap();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p, Some(q)),
                None => (target, None),
            };
            let path = format!("{PREFIX}{path}");
            assert!(
                allowed(method, &path, query, &scope(A, S)).is_ok(),
                "{call}"
            );
        }
    }
}

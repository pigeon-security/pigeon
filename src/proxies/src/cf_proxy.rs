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
//! ([`Host::deploy_grant`]). Its calls pass the same allowlist. `GET /cf/grant` asks only that:
//! 200 for the live deploying landing's job token, 403 otherwise, so a deploy job not started by
//! a landing stops before its build.
//!
//! Served by core, each call's token is minted from the parent token with only that call's
//! permissions. Served by the proxy Worker, which never holds the parent token, each call uses
//! the scoped `cf_deploy` token core minted for it ([`Purpose::Deploy`]): the union of the
//! permissions [`ALLOWED`] names, living 15 minutes. The allowlist and the body rules are the
//! same either way.

use pigeon_core::job_token as job;
use pigeon_core::tokens::{self, Policy, Scoped};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::js_sys::Uint8Array;
use worker::{Fetch, Headers, Method, Request, RequestInit, Response, Result, console_error};

use crate::lookup;
use crate::proxy::{self, Audit, Outcome, ProxyCaller, bearer};
use crate::{Host, Purpose};

/// Where the proxy is mounted, standing for `https://api.cloudflare.com/client/v4`.
pub const PREFIX: &str = "/cf/client/v4";

/// The pre-flight route: `GET /cf/grant` tells a CI job whether its job token is the deploy grant
/// of the live landing. It sits outside [`PREFIX`], never reaches the allowlist and never calls
/// the API.
pub const GRANT_PATH: &str = "/cf/grant";

/// Why a git-only credential is refused here: an AI agent run's git grant, or an app's session
/// credential (`session_credential`), reaches `/git/` and nothing else.
pub const GIT_ONLY: &str = "a git-only credential (an AI agent run's grant, an app's session credential) reaches /git/ only";
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

/// How large a request body may be: a Worker upload (uncompressed multipart,
/// the Worker's wasm included). 48 MiB is still well under the proxy Worker's
/// 128 MB memory, since the body is buffered once.
pub const BODY_MAX: usize = 48 * 1024 * 1024;

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
/// Reading an R2 bucket's own record (not its objects).
pub const R2_READ: &[&str] = &["Workers R2 Storage Read"];
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
pub const ALLOWED: [Allowed; 19] = [
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
    // An R2 bucket a Worker binds: `wrangler deploy` reads the bucket's record ([[r2_buckets]])
    // before the upload. The bucket's own record only, never its objects, and any bucket name:
    // the allowed name cannot come from the config being deployed, which the deploying proxy
    // has not built yet.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/r2/buckets/{bucket}",
        query: &[],
        needs: &[],
        permissions: R2_READ,
        body: BodyRule::Any,
    },
    // The account's workers.dev subdomain, read when a Worker's config turns workers.dev on
    // (the Durable Object rehearsal's throwaway Workers do). Read only: it names the account's
    // subdomain, nothing more.
    Allowed {
        method: "GET",
        path: "/accounts/{account}/workers/subdomain",
        query: &[],
        needs: &[],
        permissions: READ,
        body: BodyRule::Any,
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
    // Sets the Worker's cron triggers, as its config's [triggers] says.
    Allowed {
        method: "PUT",
        path: "/accounts/{account}/workers/scripts/{script}/schedules",
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
    /// The account id.
    pub account: &'a str,
    /// The Worker script a call may name.
    pub script: &'a str,
    /// The queues it produces to.
    pub queues: &'a [&'a str],
    /// Found by the proxy itself, and only for container application calls; empty otherwise.
    pub namespaces: &'a [&'a str],
}

/// Whether `s` is a Worker version id: a UUID, with or without its hyphens.
/// Whether `s` is an R2 bucket name: 3 to 63 of `[a-z0-9-]`, starting and ending alphanumeric.
fn is_bucket(s: &str) -> bool {
    let b = s.as_bytes();
    (3..=63).contains(&b.len())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

fn is_version(s: &str) -> bool {
    let hex = s.bytes().filter(|b| *b != b'-').count();
    (s.len() == 36 || s.len() == 32)
        && hex == 32
        && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
        && (s.len() == 32 || [8, 13, 18, 23].iter().all(|i| s.as_bytes()[*i] == b'-'))
}

/// The Worker script a call names in its path (`.../workers/scripts/{s}`, `.../services/{s}`
/// or `.../workers/workers/{s}`), if it names one.
pub fn target_script(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(PREFIX)?;
    let parts: Vec<&str> = rest.split('/').skip(1).collect();
    match parts.as_slice() {
        [
            "accounts",
            _,
            "workers",
            "scripts" | "services" | "workers",
            s,
            ..,
        ] => Some(s),
        _ => None,
    }
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
            "{bucket}" => is_bucket(s),
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
    /// The other platform Workers it deploys (docs/plans/workers-split.md), such as
    /// `pigeon-sites`, each with its queues and container classes (from ci/workers.toml).
    pub others: Vec<Other>,
    /// The queues the Worker produces to, which a deploy looks up by name.
    pub queues: Vec<String>,
    /// The Worker's container classes, such as `CiRunner`: their Durable Object namespaces
    /// are the container applications a deploy may read, create and change.
    pub containers: Vec<String>,
    /// The repository landings deploy from, as `namespace/name`: a deploy job's token is taken
    /// only when its grant is for this repository.
    pub land_repo: String,
}

/// Another platform Worker the proxy deploys, and its deploy's scope.
#[derive(Clone, Debug, Default)]
pub struct Other {
    /// Its script name, such as `pigeon-sites`.
    pub script: String,
    /// The queues it produces to.
    pub queues: Vec<String>,
    /// Its container classes.
    pub containers: Vec<String>,
}

/// What a Worker's `wrangler.toml` says its deploy touches outside its own script: the queues it
/// produces to and its container classes, in file order. Empty when the text does not parse.
pub fn deploy_scope(wrangler: &str) -> (Vec<String>, Vec<String>) {
    #[derive(Deserialize, Default)]
    struct Raw {
        #[serde(default)]
        queues: Queues,
        #[serde(default)]
        containers: Vec<Container>,
    }
    #[derive(Deserialize, Default)]
    struct Queues {
        #[serde(default)]
        producers: Vec<Producer>,
    }
    #[derive(Deserialize)]
    struct Producer {
        queue: Option<String>,
    }
    #[derive(Deserialize)]
    struct Container {
        class_name: String,
    }
    let raw: Raw = toml::from_str(wrangler).unwrap_or_default();
    (
        raw.queues
            .producers
            .into_iter()
            .filter_map(|p| p.queue)
            .collect(),
        raw.containers.into_iter().map(|c| c.class_name).collect(),
    )
}

impl CfProxy {
    /// The proxy for the platform Workers: core's `script`, with the queues and container
    /// classes its `wrangler` text declares, and every other Worker in the path `map`
    /// (ci/workers.toml) with its own. A map that does not parse leaves core alone, as core's
    /// fallback map does.
    pub fn platform(host: &str, script: &str, wrangler: &str, map: &str, land_repo: &str) -> Self {
        let (queues, containers) = deploy_scope(wrangler);
        let others = pigeon_core::lanes::parse(map)
            .map(|m| m.workers)
            .unwrap_or_default()
            .into_iter()
            .filter(|w| w.script != script)
            .map(|w| Other {
                script: w.script,
                queues: w.queues,
                containers: w.containers,
            })
            .collect();
        CfProxy {
            host: host.into(),
            script: script.into(),
            others,
            queues,
            containers,
            land_repo: land_repo.into(),
        }
    }
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
    /// Serves one request under `/cf/` for `host`'s callers, and writes its audit line.
    pub async fn serve(&self, host: &dyn Host, req: Request) -> Result<Response> {
        let method = req.method();
        let path = req.path();
        let mut caller = None;
        let answer = self.answer(host, req, &mut caller).await;
        let (outcome, status, why) = match &answer {
            Ok((res, why)) => {
                let status = res.status_code();
                let replayed = res
                    .headers()
                    .get(lookup::SERVED_FROM_CACHE)
                    .ok()
                    .flatten()
                    .is_some();
                let outcome = match why {
                    Some(_) => Outcome::Refused,
                    None if replayed => Outcome::Replayed,
                    None if (200..400).contains(&status) => Outcome::Forwarded,
                    None => Outcome::Failed,
                };
                let why = match outcome {
                    Outcome::Replayed => Some(lookup::REPLAYED_WHY.to_string()),
                    _ => why.clone(),
                };
                (outcome, Some(status), why)
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
        host: &dyn Host,
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
            let token = bearer(&req)?.filter(|t| t.starts_with(job::PREFIX));
            let job_token = token.is_some();
            let method = req.method();
            let name: &str = method.as_ref();
            let found = match &token {
                Some(t) if name == "GET" => host.deploy_grant(t, &self.land_repo).await?,
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
            Some(t) if t.starts_with(job::PREFIX) => {
                match host.deploy_grant(&t, &self.land_repo).await? {
                    Some(c) => Some(c),
                    None => {
                        return refuse(403, JOB_TOKEN_REFUSED);
                    }
                }
            }
            _ => match proxy::caller(host, &req).await {
                // A git-only credential (an AI agent run's grant, an app's session
                // credential) reaches /git/ only.
                Ok(c) if c.branch_prefix.is_some() => {
                    return refuse(403, GIT_ONLY);
                }
                Ok(c) => Some(c),
                Err(e) => return refuse(403, &e.to_string()),
            },
        };
        let method = req.method();
        // The Worker the call is for: the one its path names, or this one. A landing's deploy
        // job deploys only the Worker its grant names; a proxy credential, any platform Worker.
        let target = target_script(url.path()).unwrap_or(&self.script);
        let other = self.others.iter().find(|o| o.script == target);
        let known = target == self.script || other.is_some();
        let granted = caller
            .as_ref()
            .and_then(|c| c.script.as_deref())
            .is_none_or(|s| s == target || target_script(url.path()).is_none());
        if !known || !granted {
            return refuse(
                403,
                &format!("pigeon's proxy does not deploy Worker {target} for this caller"),
            );
        }
        // An account-wide call (queues, container applications) is scoped to the Worker being
        // deployed: the one a deploy job's grant names, or the one the path names.
        let deploying = caller
            .as_ref()
            .and_then(|c| c.script.clone())
            .unwrap_or_else(|| target.to_string());
        let (scope_script, queues_owned, classes): (&str, Vec<String>, Vec<String>) =
            if deploying == self.script {
                (&self.script, self.queues.clone(), self.containers.clone())
            } else {
                match self.others.iter().find(|o| o.script == deploying) {
                    Some(o) => (&o.script, o.queues.clone(), o.containers.clone()),
                    None => (&self.script, Vec::new(), Vec::new()),
                }
            };
        // Container application calls name the Worker's container namespaces, so they are
        // looked up first; no other call needs them.
        let namespaces = if !classes.is_empty() && url.path().contains("/containers/applications") {
            let account = host.account(Purpose::Deploy).await?;
            container_namespaces(&account, scope_script, &classes).await?
        } else {
            Vec::new()
        };
        let account_id = host.account_id();
        let queues: Vec<&str> = queues_owned.iter().map(String::as_str).collect();
        let namespaces: Vec<&str> = namespaces.iter().map(String::as_str).collect();
        let scope = Scope {
            account: &account_id,
            script: target,
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
        let account = host.account(Purpose::Deploy).await?;
        let token = Scoped::open_for(
            &account,
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
        let (status, answered_type, bytes) = token.finish(sent).await?;
        if lookup::is_lookup(entry) {
            let store = lookup::EdgeStore::new(&self.host, &account_id, target);
            if let Some(kept) = remember(
                &store,
                &account_id,
                target,
                status,
                answered_type.as_deref(),
                &bytes,
            )
            .await
            {
                return Ok((lookup::replay(&kept)?, None));
            }
        } else if lookup::forgets(entry, status, content_type.as_deref(), &body) {
            let store = lookup::EdgeStore::new(&self.host, &account_id, target);
            if let Err(e) = store.forget().await {
                console_error!("cf proxy: could not forget the lookup of {target}: {e}");
            }
        }
        let headers = Headers::new();
        headers.set(
            "Content-Type",
            answered_type.as_deref().unwrap_or("application/json"),
        )?;
        Ok((
            Response::from_bytes(bytes)?
                .with_status(status)
                .with_headers(headers),
            None,
        ))
    }
}

/// The time in milliseconds since the epoch.
fn now_ms() -> i64 {
    i64::try_from(worker::Date::now().as_millis()).unwrap_or(i64::MAX)
}

/// Remembers a successful lookup of `script`, or finds the one to answer a rate-limited lookup
/// with ([`lookup::decide`]). The store failing never fails the call: it is logged, and
/// Cloudflare's answer passes on as it is.
async fn remember(
    store: &lookup::EdgeStore,
    account: &str,
    script: &str,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) -> Option<lookup::Remembered> {
    let now = now_ms();
    let kept = if lookup::rate_limited(status, body) {
        match store.get().await {
            Ok(k) => k,
            Err(e) => {
                console_error!("cf proxy: could not read the lookup of {script}: {e}");
                None
            }
        }
    } else {
        None
    };
    match lookup::decide(account, script, status, content_type, body, kept, now) {
        lookup::Lookup::Fill(r) => {
            if let Err(e) = store.put(&r, now).await {
                console_error!("cf proxy: could not keep the lookup of {script}: {e}");
            }
            None
        }
        lookup::Lookup::Replay(r) => Some(r),
        lookup::Lookup::Pass => None,
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

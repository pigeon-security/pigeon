//! The deploy proxy's remembered Worker lookups. `wrangler deploy` starts with
//! `GET /accounts/{account}/workers/services/{script}`: whether the Worker exists, its tag and
//! its Durable Object migration tag. For a Worker deployed or rolled back many times in a day,
//! Cloudflare can refuse that one lookup with `10429` (rate limited) for a quarter of an hour
//! while every other call, and every other Worker, goes through; deploy.sh's retries only add to
//! the count. So the deploy proxy remembers each successful lookup, per account and script, for
//! [`KEEP_FOR_MS`], and answers a lookup Cloudflare refuses with `10429` with the remembered
//! answer, marked [`SERVED_FROM_CACHE`]. Nothing else is ever remembered or replayed.
//!
//! - Only the answer to that one `GET`, after the caller was authenticated and the call passed
//!   the allowlist, and only a `2xx` answer: every successful lookup replaces the one kept, so
//!   the migration tag kept is the one Cloudflare last reported.
//! - Only the fields wrangler reads ([`kept_body`]), so nothing sensitive is kept even if the
//!   API adds fields.
//! - A successful upload that carries Durable Object migrations (the `PUT` of the script, or a
//!   new version) moves the migration tag, so it forgets the script's lookup
//!   ([`forgets`]); the next deploy asks Cloudflare again. An upload without migrations leaves
//!   the tag as it was, and the lookup stays.
//!
//! The answers live in the Workers Cache API (`caches.default`) of the Worker serving the proxy,
//! under a key on the proxy's host that no request is ever routed to ([`key`]): it needs no
//! binding, and both deployments of the proxy (the `pigeon-proxy` Worker, and core's backup)
//! have it. It outlives the isolate, but it is kept per Cloudflare data center and may be evicted
//! early; a lookup it no longer holds is simply passed through as Cloudflare answered it.
//!
//! The decisions are pure ([`decide`], [`forgets`]), so the smoke suite checks them without a
//! Workers runtime; [`EdgeStore`] is the Workers glue.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use worker::{Cache, Headers, Response, Result};

use crate::cf_proxy::Allowed;

/// The allowlist path of the lookup.
pub const LOOKUP_PATH: &str = "/accounts/{account}/workers/services/{script}";

/// Cloudflare's error code for a rate-limited call.
pub const RATE_LIMITED: u64 = 10429;

/// How long a remembered lookup may be replayed, in milliseconds: a day.
pub const KEEP_FOR_MS: i64 = 86_400_000;

/// The header a replayed lookup carries, naming the code Cloudflare refused it with.
pub const SERVED_FROM_CACHE: &str = "x-pigeon-served-from-cache";

/// The audit line's `why` for a replayed lookup.
pub const REPLAYED_WHY: &str =
    "Cloudflare answered 10429 (rate limited); answered with the lookup the proxy remembered";

/// Where on the proxy's host the lookups are kept: a path the proxy never serves.
const KEY_PATH: &str = "/_pigeon/cf-lookup/v1";

/// The fields of the lookup's `result.default_environment.script` that are kept: what wrangler
/// 4.166.0 reads (`tag`, `tags`, `migration_tag`, `last_deployed_from`) and plain metadata.
const SCRIPT_FIELDS: [&str; 10] = [
    "id",
    "tag",
    "tags",
    "etag",
    "migration_tag",
    "last_deployed_from",
    "compatibility_date",
    "usage_model",
    "created_on",
    "modified_on",
];

/// The fields of `result.default_environment` that are kept besides its script.
const ENVIRONMENT_FIELDS: [&str; 3] = ["environment", "created_on", "modified_on"];

/// The fields of `result` that are kept besides its default environment.
const RESULT_FIELDS: [&str; 3] = ["id", "created_on", "modified_on"];

/// Whether `entry` is the lookup: the only call whose answer is ever remembered.
pub fn is_lookup(entry: &Allowed) -> bool {
    entry.method == "GET" && entry.path == LOOKUP_PATH
}

/// The key `account`'s lookup of `script` is kept under, on the proxy's `host`.
pub fn key(host: &str, account: &str, script: &str) -> String {
    format!("https://{host}{KEY_PATH}/{account}/{script}")
}

/// Whether an answer of `status` with `body` is Cloudflare refusing the call as rate limited:
/// not a success, and `10429` among its errors.
pub fn rate_limited(status: u16, body: &[u8]) -> bool {
    if (200..300).contains(&status) {
        return false;
    }
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("errors").and_then(Value::as_array).cloned())
        .is_some_and(|errors| {
            errors
                .iter()
                .any(|e| e.get("code").and_then(Value::as_u64) == Some(RATE_LIMITED))
        })
}

/// The fields of `from` named in `fields`, as they are.
fn pick(from: &Map<String, Value>, fields: &[&str]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|f| from.get(*f).map(|v| ((*f).to_string(), v.clone())))
        .collect()
}

/// The part of a successful lookup's `body` that is kept: the API's envelope and only the
/// fields wrangler reads, so nothing else the API answers is ever stored. `None` when `body` is
/// not a successful lookup with a default environment's script.
pub fn kept_body(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let result = v.get("result")?.as_object()?;
    let env = result.get("default_environment")?.as_object()?;
    let script = env.get("script")?.as_object()?;
    let mut kept_env = pick(env, &ENVIRONMENT_FIELDS);
    kept_env.insert("script".into(), Value::Object(pick(script, &SCRIPT_FIELDS)));
    let mut kept_result = pick(result, &RESULT_FIELDS);
    kept_result.insert("default_environment".into(), Value::Object(kept_env));
    Some(
        json!({
            "success": true,
            "errors": [],
            "messages": [],
            "result": kept_result,
        })
        .to_string(),
    )
}

/// A remembered lookup.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Remembered {
    /// The account it was looked up in.
    pub account: String,
    /// The Worker script it is of.
    pub script: String,
    /// Cloudflare's status.
    pub status: u16,
    /// Cloudflare's content type.
    pub content_type: String,
    /// The kept body ([`kept_body`]).
    pub body: String,
    /// When it was kept, in milliseconds since the epoch.
    pub kept_at_ms: i64,
    /// When it stops being replayed.
    pub until_ms: i64,
}

impl Remembered {
    /// What to remember of a lookup of `script` in `account` answered `status`, `content_type`
    /// and `body` at `now_ms`: `None` unless it is a `2xx` lookup [`kept_body`] keeps.
    pub fn of(
        account: &str,
        script: &str,
        status: u16,
        content_type: Option<&str>,
        body: &[u8],
        now_ms: i64,
    ) -> Option<Self> {
        if !(200..300).contains(&status) {
            return None;
        }
        Some(Remembered {
            account: account.into(),
            script: script.into(),
            status,
            content_type: content_type.unwrap_or("application/json").into(),
            body: kept_body(body)?,
            kept_at_ms: now_ms,
            until_ms: now_ms.saturating_add(KEEP_FOR_MS),
        })
    }

    /// Whether it may answer a lookup of `script` in `account` at `now_ms`.
    pub fn answers(&self, account: &str, script: &str, now_ms: i64) -> bool {
        self.account == account && self.script == script && now_ms < self.until_ms
    }
}

/// What to do with Cloudflare's answer to a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// A success: pass it on, and remember this.
    Fill(Remembered),
    /// Rate limited, with a lookup to answer instead: answer with this, marked.
    Replay(Remembered),
    /// Pass it on as it is, remembering nothing.
    Pass,
}

/// What to do with Cloudflare's answer (`status`, `content_type`, `body`) to the lookup of
/// `script` in `account` at `now_ms`, given the lookup `kept` (if any).
pub fn decide(
    account: &str,
    script: &str,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
    kept: Option<Remembered>,
    now_ms: i64,
) -> Lookup {
    if let Some(r) = Remembered::of(account, script, status, content_type, body, now_ms) {
        return Lookup::Fill(r);
    }
    match kept {
        Some(k) if rate_limited(status, body) && k.answers(account, script, now_ms) => {
            Lookup::Replay(k)
        }
        _ => Lookup::Pass,
    }
}

/// The search for `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The `metadata` part of a `multipart/form-data` upload, parsed; `None` when there is none or
/// it is not JSON.
fn metadata(content_type: Option<&str>, body: &[u8]) -> Option<Value> {
    let ct = content_type?;
    if !ct
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return None;
    }
    let boundary = ct.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        k.eq_ignore_ascii_case("boundary")
            .then(|| v.trim().trim_matches('"').to_string())
    })?;
    let delimiter = format!("--{boundary}");
    let mut rest = body;
    while let Some(at) = find(rest, delimiter.as_bytes()) {
        rest = &rest[at + delimiter.len()..];
        let end = find(rest, delimiter.as_bytes()).unwrap_or(rest.len());
        let part = &rest[..end];
        if let Some(split) = find(part, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&part[..split]).to_ascii_lowercase();
            if headers.contains("name=\"metadata\"") {
                let content = &part[split + 4..];
                let content = content.strip_suffix(b"\r\n").unwrap_or(content);
                return serde_json::from_slice(content).ok();
            }
        }
    }
    None
}

/// Whether a call to `entry` answered `status`, sent with `content_type` and `body`, makes the
/// remembered lookup of its script wrong: a successful upload (`PUT` of the script, or a new
/// version) carrying Durable Object migrations, which moves the migration tag. An upload whose
/// metadata cannot be read counts as carrying them.
pub fn forgets(entry: &Allowed, status: u16, content_type: Option<&str>, body: &[u8]) -> bool {
    let upload = matches!(
        (entry.method, entry.path),
        ("PUT", "/accounts/{account}/workers/scripts/{script}")
            | (
                "POST",
                "/accounts/{account}/workers/scripts/{script}/versions"
            )
    );
    if !upload || !(200..300).contains(&status) {
        return false;
    }
    match metadata(content_type, body) {
        Some(m) => !m.get("migrations").is_none_or(Value::is_null),
        None => true,
    }
}

/// The answer a replayed lookup gives the client.
pub fn replay(kept: &Remembered) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", &kept.content_type)?;
    headers.set(SERVED_FROM_CACHE, &RATE_LIMITED.to_string())?;
    Ok(Response::from_bytes(kept.body.clone().into_bytes())?
        .with_status(kept.status)
        .with_headers(headers))
}

/// The remembered lookups in the Workers Cache API of the Worker serving the proxy.
pub struct EdgeStore {
    key: String,
}

impl EdgeStore {
    /// The store of `account`'s lookup of `script`, on the proxy's `host`.
    pub fn new(host: &str, account: &str, script: &str) -> Self {
        EdgeStore {
            key: key(host, account, script),
        }
    }

    /// The lookup kept, if any.
    pub async fn get(&self) -> Result<Option<Remembered>> {
        match Cache::default().get(self.key.as_str(), false).await? {
            Some(mut res) => Ok(res.json::<Remembered>().await.ok()),
            None => Ok(None),
        }
    }

    /// Keeps `r` until it expires.
    pub async fn put(&self, r: &Remembered, now_ms: i64) -> Result<()> {
        let secs = (r.until_ms.saturating_sub(now_ms) / 1000).max(1);
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        headers.set("Cache-Control", &format!("max-age={secs}"))?;
        let res = Response::from_json(r)?.with_headers(headers);
        Cache::default().put(self.key.as_str(), res).await
    }

    /// Forgets the lookup kept.
    pub async fn forget(&self) -> Result<()> {
        Cache::default().delete(self.key.as_str(), false).await?;
        Ok(())
    }
}

//! Cloudflare AI Gateway: one gateway in front of many model providers. This block has a gateway
//! exist, keeps its providers' keys (bring your own keys, stored in the Secrets Store and named
//! `{gateway}_{provider}_{alias}`), sends requests through it and reads its logs. Requests carry
//! `cf-aig-authorization`, a token minted for the call and revoked after; a provider's key never
//! leaves Cloudflare.
//!
//! Each request also carries `cf-aig-metadata`, up to [`MAX_TAGS`] tags naming who sent it and
//! why, and `cf-aig-collect-log-payload: false`, so the gateway logs keep tokens, cost, model,
//! status and the tags but never the prompt or the answer. A gateway has no setting for that:
//! it is per request (<https://developers.cloudflare.com/ai-gateway/observability/logging/>).
//! The logs name a model as `anthropic/claude-haiku-5.5` where a request named
//! `claude-haiku-5-5`; [`canonical_model`] reads both as one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::{Date, Uint8Array};
use worker::{Fetch, Headers, Method, Request, RequestInit, Result};

use crate::Account;
use crate::cloudflare::{call, call_fresh};
use crate::tokens::{Policy, Scope, Scoped};

/// What managing a gateway and its providers' keys takes.
pub const WRITE_PERMISSIONS: [&str; 2] = ["AI Gateway Write", "Secrets Store Write"];

/// What reading a gateway's logs takes.
pub const READ_PERMISSIONS: [&str; 1] = ["AI Gateway Read"];

/// What sending a request through a gateway takes.
pub const RUN_PERMISSIONS: [&str; 1] = ["AI Gateway Run"];

/// The Secrets Store scope a provider's key needs for a gateway to read it.
pub const SECRET_SCOPE: &str = "ai_gateway";

const GATEWAY: &str = "https://gateway.ai.cloudflare.com/v1";

/// The Cloudflare REST API, which is the only way to Workers AI models through a gateway.
const REST_API: &str = "https://api.cloudflare.com/client/v4/accounts";

/// The provider name and slug of Workers AI.
pub const WORKERS_AI: &str = "workers-ai";

/// The header a request's tags go in, as a JSON object
/// (<https://developers.cloudflare.com/ai-gateway/observability/custom-metadata/>).
pub const METADATA_HEADER: &str = "cf-aig-metadata";

/// The header that has a request's log kept without its request and response bodies.
pub const PAYLOAD_HEADER: &str = "cf-aig-collect-log-payload";

/// How many tags a request may carry: the gateway keeps the first five and drops the rest.
pub const MAX_TAGS: usize = 5;

/// The tags pigeon sends with a request, and reads back from its log.
pub mod tag {
    /// The ID of the user who sent it.
    pub const CALLER: &str = "caller";
    /// Their name.
    pub const CALLER_NAME: &str = "caller_name";
    /// The ID of the standard user they act for, when linked.
    pub const FOR: &str = "for";
    /// That user's name.
    pub const FOR_NAME: &str = "for_name";
    /// What the request was for, as its sender labelled it.
    pub const TASK: &str = "task";
}

/// How long to wait before each retry of a request refused with 401: a token minted moments
/// before may not have reached the gateway yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// The error code the gateway answers, with status 401, when it does not take a request's
/// `cf-aig-authorization` token: `{"code":2009,"message":"Unauthorized","name":"AiGatewayError"}`.
pub const UNAUTHORIZED_CODE: u64 = 2009;

/// Whether a refusal with `status` and `body` is the gateway refusing the token, not the provider
/// refusing the request. A token minted moments before can be refused this way for a while, so
/// the cure is to wait and send again with a token minted afresh. The body may be the error
/// object itself, a list of them, or an object holding them in `errors` or `error`.
pub fn refused_token(status: u16, body: &Value) -> bool {
    if status != 401 {
        return false;
    }
    let is_it = |v: &Value| {
        v["code"].as_u64() == Some(UNAUTHORIZED_CODE)
            || v["name"].as_str() == Some("AiGatewayError")
    };
    let listed = |v: &Value| v.as_array().is_some_and(|a| a.iter().any(is_it));
    is_it(body) || listed(body) || listed(&body["errors"]) || is_it(&body["error"])
}

/// The Secrets Store name a gateway looks a provider's key up by.
pub fn secret_name(gateway: &str, provider: &str, alias: &str) -> String {
    format!("{gateway}_{provider}_{alias}")
}

/// Which credentials a gateway's requests bill.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayKind {
    /// Provider keys stored on the gateway (bring your own keys); nothing else is accepted.
    Byok,
    /// Cloudflare AI Gateway Unified Billing: Cloudflare credits pay for the requests.
    Credits,
    /// A gateway no provider routes to any more. Its logs are still read, and always count as
    /// BYOK. It is never created, and should hold no stored key.
    Retired,
}

/// A gateway as pigeon wants it, by `kind`. All are authenticated and keep logs.
pub fn spec(id: &str, store_id: &str, kind: GatewayKind) -> Value {
    match kind {
        GatewayKind::Byok | GatewayKind::Retired => json!({
            "id": id,
            "cache_ttl": 0,
            "cache_invalidate_on_update": false,
            "collect_logs": true,
            "rate_limiting_interval": 0,
            "rate_limiting_limit": 0,
            "authentication": true,
            "byok_only": true,
            "store_id": store_id,
        }),
        GatewayKind::Credits => json!({
            "id": id,
            "cache_ttl": 0,
            "cache_invalidate_on_update": false,
            "collect_logs": true,
            "rate_limiting_interval": 0,
            "rate_limiting_limit": 0,
            "authentication": true,
            "byok_only": false,
            "zdr": true,
            "workers_ai_billing_mode": "unified",
        }),
    }
}

/// What [`ensure`] found or did.
#[derive(Serialize)]
pub struct Ensured {
    pub id: String,
    pub created: bool,
    /// Settings that differ from [`spec`]'s on a gateway that already existed; left as they are.
    pub drift: Vec<String>,
}

/// Has gateway `id` exist, using the account's Secrets Store; idempotent. One that exists is
/// left as it is, and where it differs from [`spec`] is reported.
pub async fn ensure(account: &Account, id: &str, kind: GatewayKind) -> Result<Ensured> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &WRITE_PERMISSIONS)],
    )
    .await?;
    let ensured = ensure_with(token.value(), account, id, kind).await;
    token.finish(ensured).await
}

async fn ensure_with(
    token: &str,
    account: &Account,
    id: &str,
    kind: GatewayKind,
) -> Result<Ensured> {
    let base = format!("/accounts/{}/ai-gateway/gateways", account.id);
    let listed = call(token, Method::Get, &format!("{base}?per_page=50"), None).await?;
    let want = spec(id, &account.secrets_store, kind);
    match rows(&listed)
        .into_iter()
        .find(|g| g["id"].as_str() == Some(id))
    {
        Some(found) => Ok(Ensured {
            id: id.into(),
            created: false,
            drift: drift(&want, &found, kind),
        }),
        None if kind == GatewayKind::Retired => Ok(Ensured {
            id: id.into(),
            created: false,
            drift: vec!["does not exist (retired; nothing to do)".into()],
        }),
        None => {
            call(token, Method::Post, &base, Some(&want)).await?;
            Ok(Ensured {
                id: id.into(),
                created: true,
                drift: Vec::new(),
            })
        }
    }
}

/// The settings in `want` that `found` holds otherwise, for a gateway of `kind`.
pub fn drift(want: &Value, found: &Value, kind: GatewayKind) -> Vec<String> {
    let mut keys = vec!["authentication", "byok_only", "collect_logs"];
    match kind {
        GatewayKind::Byok => keys.push("store_id"),
        GatewayKind::Credits => {
            keys.push("zdr");
            keys.push("workers_ai_billing_mode");
        }
        GatewayKind::Retired => {}
    }
    keys.iter()
        .filter(|k| want[**k] != found[**k])
        .map(|k| format!("{k} is {}, not {}", found[*k], want[*k]))
        .collect()
}

/// Drift in the keys a gateway of `kind` holds. A credits gateway must hold none: one stored for
/// a provider would be billed instead of credits. A BYOK gateway must hold one for each of
/// `declared_slugs`. Pure.
pub fn stored_key_drift(
    kind: GatewayKind,
    configs: &[ProviderConfig],
    declared_slugs: &[&str],
) -> Vec<String> {
    match kind {
        GatewayKind::Retired => configs
            .iter()
            .map(|c| {
                format!(
                    "holds a stored key for {}: delete it to retire this gateway",
                    c.provider_slug
                )
            })
            .collect(),
        GatewayKind::Credits => configs
            .iter()
            .map(|c| {
                format!(
                    "holds a stored key for {}: credits requests for it would bill that key instead",
                    c.provider_slug
                )
            })
            .collect(),
        GatewayKind::Byok => declared_slugs
            .iter()
            .filter(|s| !configs.iter().any(|c| c.provider_slug == **s))
            .map(|s| format!("holds no stored key for {s}"))
            .collect(),
    }
}

/// Whether a credits request through gateway `gateway_id` may go ahead, given the gateway as the
/// API reads it and its stored keys. It refuses while the gateway needs provider keys, or holds
/// any stored key: a key would be billed instead of credits. Pure.
pub fn credits_guard(
    gateway_id: &str,
    gateway: &Value,
    configs: &[ProviderConfig],
) -> std::result::Result<(), String> {
    if gateway["byok_only"].as_bool() == Some(true) {
        return Err(format!(
            "the credits gateway `{gateway_id}` still requires provider keys (byok_only is on): turn it off in the dashboard"
        ));
    }
    if let Some(c) = configs.first() {
        return Err(format!(
            "the credits gateway `{gateway_id}` holds a stored {} key, which would be billed instead of credits: delete it in the dashboard",
            c.provider_slug
        ));
    }
    Ok(())
}

/// Whether a BYOK request for `slug` may go ahead through `gateway_id`: the gateway must exist,
/// accept only stored keys, and hold one for `slug`. `gateway` is `None` when it does not exist.
/// Pure.
pub fn byok_ready(
    gateway: Option<&Value>,
    configs: &[ProviderConfig],
    gateway_id: &str,
    slug: &str,
) -> std::result::Result<(), String> {
    let Some(gateway) = gateway else {
        return Err(format!(
            "the BYOK gateway `{gateway_id}` does not exist: run models_setup"
        ));
    };
    if gateway["byok_only"].as_bool() != Some(true) {
        return Err(format!(
            "the BYOK gateway `{gateway_id}` does not require stored keys (byok_only is not on): run models_setup"
        ));
    }
    if !configs.iter().any(|c| c.provider_slug == slug) {
        return Err(format!(
            "the BYOK gateway `{gateway_id}` holds no key for {slug}: set {} on the Access tab, then run models_setup",
            secret_name(gateway_id, slug, "default")
        ));
    }
    Ok(())
}

/// Whether a Workers AI request may go ahead through credits gateway `gateway_id`: the gateway
/// must bill Workers AI as Unified Billing. Called only for the `workers-ai` provider, beside
/// [`credits_guard`]. Pure.
pub fn workers_ai_guard(gateway_id: &str, gateway: &Value) -> std::result::Result<(), String> {
    if gateway["workers_ai_billing_mode"].as_str() != Some("unified") {
        return Err(format!(
            "the credits gateway `{gateway_id}` does not bill Workers AI as Unified Billing (workers_ai_billing_mode is not \"unified\")"
        ));
    }
    Ok(())
}

/// The URL of a request to `provider`'s `path` through `gateway`. Workers AI goes to the REST
/// API; the gateway is then named by a header ([`workers_ai_headers`]), not by the URL.
pub fn endpoint(account_id: &str, gateway: &str, provider: &str, path: &str) -> String {
    if provider == WORKERS_AI {
        format!("{REST_API}/{account_id}/ai/v1/{path}")
    } else {
        format!("{GATEWAY}/{account_id}/{gateway}/{provider}/{path}")
    }
}

/// The headers a Workers AI request needs beside the gateway's own: the gateway, always named so
/// the account's `default` gateway never serves it, and `token`, the short-lived token core
/// minted for this one call. Pure.
pub fn workers_ai_headers(gateway: &str, token: &str) -> Vec<(String, String)> {
    vec![
        ("cf-aig-gateway-id".into(), gateway.into()),
        ("Authorization".into(), format!("Bearer {token}")),
    ]
}

/// A plain message for a credits error `status` and `code`, or `None` for any other.
pub fn credits_error(status: u16, code: u64) -> Option<String> {
    Some(
        match (status, code) {
            (403, 2049) => "Cloudflare credits cannot be used (403, code 2049): Unified Billing is not possible on this account or gateway",
            (402, 2052) => "Cloudflare credits are too low (402, code 2052): buy credits on the AI Gateway page",
            (429, 2018) => "the credits gateway is rate limited (429, code 2018): try again shortly",
            (503, 7005) => "Cloudflare credits are unavailable for now (503, code 7005): try again shortly",
            _ => return None,
        }
        .into(),
    )
}

/// Reads gateway `gateway`'s settings, as the API answers them.
pub async fn gateway_settings(account: &Account, gateway: &str) -> Result<Value> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let path = format!("/accounts/{}/ai-gateway/gateways/{gateway}", account.id);
    let got = call(token.value(), Method::Get, &path, None).await;
    token.finish(got).await
}

/// A provider's key on a gateway, as listed: never the key, only its preview.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub provider_slug: String,
    pub alias: String,
    #[serde(default)]
    pub default_config: bool,
    #[serde(default)]
    pub secret_id: Option<String>,
    #[serde(default)]
    pub secret_preview: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
}

/// The providers' keys gateway `gateway` holds.
pub async fn provider_configs(account: &Account, gateway: &str) -> Result<Vec<ProviderConfig>> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let path = format!(
        "/accounts/{}/ai-gateway/gateways/{gateway}/provider_configs?per_page=50",
        account.id
    );
    let listed = call(token.value(), Method::Get, &path, None).await;
    let listed = token.finish(listed).await?;
    Ok(serde_json::from_value(listed)?)
}

/// Has gateway `gateway` use Secrets Store secret `secret_id` as `provider`'s key under `alias`;
/// idempotent by provider and alias. Answers whether it was added.
pub async fn ensure_provider(
    account: &Account,
    gateway: &str,
    provider: &str,
    alias: &str,
    secret_id: &str,
) -> Result<bool> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &WRITE_PERMISSIONS)],
    )
    .await?;
    let path = format!(
        "/accounts/{}/ai-gateway/gateways/{gateway}/provider_configs",
        account.id
    );
    let ensured = async {
        let listed = call(
            token.value(),
            Method::Get,
            &format!("{path}?per_page=50"),
            None,
        )
        .await?;
        let exists = rows(&listed).iter().any(|c| {
            c["provider_slug"].as_str() == Some(provider) && c["alias"].as_str() == Some(alias)
        });
        if exists {
            return Ok(false);
        }
        let body = json!({
            "provider_slug": provider,
            "alias": alias,
            "default_config": alias == "default",
            "secret_id": secret_id,
        });
        call(token.value(), Method::Post, &path, Some(&body)).await?;
        Ok(true)
    }
    .await;
    token.finish(ensured).await
}

/// A provider's answer through the gateway.
pub struct Ran {
    pub status: u16,
    pub body: Value,
}

/// `tags` as the [`METADATA_HEADER`] value: a JSON object of the tags with a value, or `None`
/// when none has one. More than [`MAX_TAGS`], or a key the gateway reserves (`cf.`), is an error
/// rather than a tag the gateway would silently drop.
pub fn metadata_header(tags: &[(&str, &str)]) -> Result<Option<String>> {
    let kept: BTreeMap<&str, &str> = tags
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .copied()
        .collect();
    if kept.len() > MAX_TAGS {
        return Err(worker::Error::RustError(format!(
            "{} tags; the gateway keeps {MAX_TAGS}",
            kept.len()
        )));
    }
    if let Some(k) = kept.keys().find(|k| k.starts_with("cf.")) {
        return Err(worker::Error::RustError(format!(
            "`{k}`: the gateway reserves `cf.` tags"
        )));
    }
    Ok((!kept.is_empty()).then(|| json!(kept).to_string()))
}

/// Sends `body` to `provider`'s `path` (such as `v1/messages`) through gateway `gateway`, with
/// `headers` beside the gateway's own and `tags` in [`METADATA_HEADER`]. The provider's key is
/// the gateway's, so none is sent; the log keeps no payload ([`PAYLOAD_HEADER`]).
pub async fn run(
    account: &Account,
    gateway: &str,
    provider: &str,
    path: &str,
    headers: &[(&str, &str)],
    tags: &[(&str, &str)],
    body: &Value,
) -> Result<Ran> {
    let metadata = metadata_header(tags)?;
    let mut headers = headers.to_vec();
    headers.push((PAYLOAD_HEADER, "false"));
    if let Some(m) = &metadata {
        headers.push((METADATA_HEADER, m));
    }
    let token = Scoped::open(
        account,
        "ai-gateway-run",
        vec![Policy::new(Scope::Account, &RUN_PERMISSIONS)],
    )
    .await?;
    let url = endpoint(&account.id, gateway, provider, path);
    let ran = send(token.value(), &url, &headers, body).await;
    token.finish(ran).await
}

/// The response header that names a request's log in the gateway. The gateway sets it on its
/// responses. Source: Cloudflare's AI Gateway docs, "Add human feedback using API", step
/// "Retrieve the `cf-aig-log-id`", method "Locate the `cf-aig-log-id` in the request response"
/// (cloudflare-docs, `ai-gateway/evaluations/add-human-feedback-api.mdx`).
pub const LOG_ID_HEADER: &str = "cf-aig-log-id";

/// What [`stream`] got: the provider's refusal, read whole, or what `read` made of its answer.
/// Each also holds the gateway's log id, when it sent one.
pub enum Streamed<T> {
    Refused(Ran, Option<String>),
    Read(T, Option<String>),
}

/// How long a token minted for a streamed request lives: the ceiling if its revoke is lost. The
/// gateway checks it when the request starts, not while it streams.
const STREAM_TOKEN_TTL: u64 = 1_200;

/// [`run`], with the provider's answer handed to `read` as it arrives rather than read whole:
/// for a body that asks the provider to stream. `read` decides when to stop; once it returns,
/// the token is revoked. `read` is given the log id ([`LOG_ID_HEADER`]) with the body, so it
/// has it even if reading fails. The same tags and the same payload-free log as [`run`].
#[allow(clippy::too_many_arguments)]
pub async fn stream<T>(
    account: &Account,
    gateway: &str,
    provider: &str,
    path: &str,
    headers: &[(&str, &str)],
    tags: &[(&str, &str)],
    body: &Value,
    read: impl AsyncFnOnce(worker::ByteStream, Option<String>) -> Result<T>,
) -> Result<Streamed<T>> {
    let metadata = metadata_header(tags)?;
    let mut headers = headers.to_vec();
    headers.push((PAYLOAD_HEADER, "false"));
    if let Some(m) = &metadata {
        headers.push((METADATA_HEADER, m));
    }
    let token = Scoped::open_for(
        account,
        "ai-gateway-run",
        vec![Policy::new(Scope::Account, &RUN_PERMISSIONS)],
        STREAM_TOKEN_TTL,
    )
    .await?;
    let url = endpoint(&account.id, gateway, provider, path);
    let streamed = async {
        let mut response = open(token.value(), &url, &headers, body).await?;
        let status = response.status_code();
        // Read before the body is streamed.
        let log_id = response.headers().get(LOG_ID_HEADER).ok().flatten();
        if !(200..300).contains(&status) {
            let text = response.text().await?;
            let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
            return Ok(Streamed::Refused(Ran { status, body }, log_id));
        }
        let made = read(response.stream()?, log_id.clone()).await?;
        Ok(Streamed::Read(made, log_id))
    }
    .await;
    token.finish(streamed).await
}

/// Sends a request through the gateway, retried while it answers 401; the response, unread.
async fn open(
    token: &str,
    url: &str,
    extra: &[(&str, &str)],
    body: &Value,
) -> Result<worker::Response> {
    let bytes = body.to_string().into_bytes();
    let mut waits = NOT_YET_ACTIVE_WAITS_MS.iter();
    loop {
        let headers = Headers::new();
        headers.set("cf-aig-authorization", &format!("Bearer {token}"))?;
        headers.set("Content-Type", "application/json")?;
        for (k, v) in extra {
            headers.set(k, v)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(Uint8Array::from(bytes.as_slice()).into()));
        let response = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        if response.status_code() == 401
            && let Some(ms) = waits.next()
        {
            worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
            continue;
        }
        return Ok(response);
    }
}

async fn send(token: &str, url: &str, extra: &[(&str, &str)], body: &Value) -> Result<Ran> {
    let mut response = open(token, url, extra, body).await?;
    let status = response.status_code();
    let text = response.text().await?;
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Ok(Ran { status, body })
}

/// One request through a gateway, as its logs keep it.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Log {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub tokens_in: Option<u64>,
    #[serde(default)]
    pub tokens_out: Option<u64>,
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub duration: Option<f64>,
    /// The request's tags; the gateway lists them as a string holding a JSON object.
    #[serde(default, deserialize_with = "tags_of")]
    pub metadata: BTreeMap<String, String>,
    /// Whether the gateway could keep the request and response bodies: `false` once
    /// [`PAYLOAD_HEADER`] is sent, `None` for logs from before the gateway reported it.
    #[serde(default)]
    pub collect_log_payload: Option<bool>,
}

impl Log {
    /// Tag `key`, when the request carried it.
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(String::as_str)
    }
}

/// A log's `metadata`, whether listed as a string holding a JSON object or as the object; a
/// number or boolean reads as its text, and anything else as no tags.
fn tags_of<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    let v = Value::deserialize(d)?;
    let v = match v {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        v => v,
    };
    Ok(v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(_) | Value::Bool(_) => v.to_string(),
                        _ => return None,
                    };
                    Some((k.clone(), v))
                })
                .collect()
        })
        .unwrap_or_default())
}

/// One name for a model whichever form names it: without a provider prefix (`anthropic/`),
/// lower case, with `-` for `.`. So a request's `claude-haiku-5-5` and a log's
/// `anthropic/claude-haiku-5.5` are both `claude-haiku-5-5`. Only Claude names change their dots:
/// another provider's names, such as OpenAI's `gpt-6.2-sol`, keep theirs.
pub fn canonical_model(model: &str) -> String {
    let bare = model.rsplit('/').next().unwrap_or(model);
    let lower = bare.trim().to_lowercase();
    if lower.starts_with("claude-") {
        lower.replace('.', "-")
    } else {
        lower
    }
}

/// Which logs to count; an unset field keeps all.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub provider: Option<String>,
    /// In either form [`canonical_model`] reads.
    pub model: Option<String>,
    /// A user's ID or name, as the sender or as the person the sender acted for.
    pub caller: Option<String>,
    pub task: Option<String>,
    /// Only requests that failed.
    pub failed: bool,
}

impl Filter {
    /// Whether `log` is one to count.
    pub fn keeps(&self, log: &Log) -> bool {
        let caller = |c: &str| {
            [tag::CALLER, tag::CALLER_NAME, tag::FOR, tag::FOR_NAME]
                .iter()
                .any(|k| log.tag(k) == Some(c))
        };
        (!self.failed || !log.success)
            && self.provider.as_deref().is_none_or(|p| log.provider == p)
            && self
                .model
                .as_deref()
                .is_none_or(|m| canonical_model(&log.model) == canonical_model(m))
            && self.caller.as_deref().is_none_or(caller)
            && self
                .task
                .as_deref()
                .is_none_or(|t| log.tag(tag::TASK) == Some(t))
    }
}

/// Gateway `gateway`'s logs since `since_ms` (milliseconds since the epoch), newest first, those
/// `filter` keeps only: at most `max`, so a busy gateway cannot run a call long.
pub async fn logs(
    account: &Account,
    gateway: &str,
    since_ms: f64,
    filter: &Filter,
    max: usize,
) -> Result<Vec<Log>> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let read = logs_with(token.value(), account, gateway, since_ms, filter, max).await;
    token.finish(read).await
}

async fn logs_with(
    token: &str,
    account: &Account,
    gateway: &str,
    since_ms: f64,
    filter: &Filter,
    max: usize,
) -> Result<Vec<Log>> {
    const PER_PAGE: usize = 50;
    // Fetched together: 10 rounds for 5,000 logs, not 100 calls in a row.
    const BATCH: usize = 10;
    let base = format!(
        "/accounts/{}/ai-gateway/gateways/{gateway}/logs",
        account.id
    );
    let pages = max.div_ceil(PER_PAGE);
    let fetch = |page: usize| {
        let path = format!(
            "{base}?per_page={PER_PAGE}&page={page}&order_by=created_at&order_by_direction=desc"
        );
        async move {
            let answer = call(token, Method::Get, &path, None).await?;
            serde_json::from_value::<Vec<Log>>(answer).map_err(worker::Error::from)
        }
    };
    let mut out = Vec::new();
    let mut next = 1;
    while next <= pages {
        // The first page goes alone and fresh: it is the minted token's first call, which may come
        // before it is active.
        let batch: Vec<Result<Vec<Log>>> = if next == 1 {
            let path = format!(
                "{base}?per_page={PER_PAGE}&page=1&order_by=created_at&order_by_direction=desc"
            );
            let answer = call_fresh(token, Method::Get, &path, None).await?;
            vec![serde_json::from_value(answer).map_err(worker::Error::from)]
        } else {
            let end = (next + BATCH - 1).min(pages);
            futures_util::future::join_all((next..=end).map(&fetch)).await
        };
        next += batch.len();
        for got in batch {
            let got = got?;
            let last_page = got.len() < PER_PAGE;
            for log in got {
                if Date::parse(&log.created_at) < since_ms {
                    return Ok(out);
                }
                if filter.keeps(&log) {
                    out.push(log);
                }
                if out.len() >= max {
                    return Ok(out);
                }
            }
            if last_page {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// How many log ids one [`log_costs`] call takes.
pub const MAX_LOG_IDS: usize = 50;

/// Whether `id` may be put in a log URL: 1 to 100 characters of `A-Za-z0-9_-`. The error says
/// what is wrong.
pub fn check_log_id(id: &str) -> std::result::Result<(), String> {
    if id.is_empty() || id.len() > 100 {
        return Err("a log id is 1 to 100 characters".into());
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("a log id has only letters, digits, `_` and `-`".into());
    }
    Ok(())
}

/// What looking one log up came to.
#[derive(Debug, Clone)]
pub enum LogLookup {
    /// The log, as the gateway keeps it.
    Found(Log),
    /// The gateway has not written it yet (or has dropped it).
    NotFound,
    /// Any other failure; the text holds no account id.
    Failed(String),
}

/// The answer of `models_cost`: `{"logs": [...]}`, one entry per id in `ids`, in order. A log
/// not found, or one that failed to read, has `found: false` (and an `error` when it failed).
/// A found log with no cost has `cost_usd: null`. Pure.
pub fn cost_answer(ids: &[String], lookups: &[LogLookup]) -> Value {
    let logs: Vec<Value> = ids
        .iter()
        .zip(lookups)
        .map(|(id, lookup)| match lookup {
            LogLookup::Found(log) => json!({
                "log_id": id,
                "found": true,
                "cost_usd": log.cost,
                "model": log.model,
                "tokens_in": log.tokens_in,
                "tokens_out": log.tokens_out,
            }),
            LogLookup::NotFound => json!({
                "log_id": id,
                "found": false,
                "cost_usd": null,
                "model": null,
                "tokens_in": null,
                "tokens_out": null,
            }),
            LogLookup::Failed(error) => json!({
                "log_id": id,
                "found": false,
                "cost_usd": null,
                "model": null,
                "tokens_in": null,
                "tokens_out": null,
                "error": error,
            }),
        })
        .collect();
    json!({ "logs": logs })
}

/// Reads each log in `ids` from gateway `gateway` with ONE scoped read token; the lookups in
/// order. One id's failure does not fail the others. The endpoint is
/// `GET /accounts/{account_id}/ai-gateway/gateways/{gateway_id}/logs/{id}` in Cloudflare's API
/// schema (github.com/cloudflare/api-schemas, `openapi.json`).
pub async fn log_costs(account: &Account, gateway: &str, ids: &[String]) -> Result<Vec<LogLookup>> {
    for id in ids {
        check_log_id(id).map_err(worker::Error::RustError)?;
    }
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let read = log_costs_with(token.value(), account, gateway, ids).await;
    token.finish(read).await
}

async fn log_costs_with(
    token: &str,
    account: &Account,
    gateway: &str,
    ids: &[String],
) -> Result<Vec<LogLookup>> {
    let mut out = Vec::new();
    for (n, id) in ids.iter().enumerate() {
        let path = format!(
            "/accounts/{}/ai-gateway/gateways/{gateway}/logs/{id}",
            account.id
        );
        // The first call is the minted token's first, which may come before it is active.
        let answer = if n == 0 {
            call_fresh(token, Method::Get, &path, None).await
        } else {
            call(token, Method::Get, &path, None).await
        };
        out.push(match answer {
            Ok(v) => match serde_json::from_value::<Log>(v) {
                Ok(log) => LogLookup::Found(log),
                Err(e) => LogLookup::Failed(format!("the log is unreadable: {e}")),
            },
            Err(e) => {
                let text = e.to_string();
                if text.contains(": 404:") {
                    LogLookup::NotFound
                } else {
                    LogLookup::Failed(text.replace(account.id.as_str(), "<account>"))
                }
            }
        });
    }
    Ok(out)
}

/// What [`summarize`] can group logs by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    /// The provider and the model, in [`canonical_model`]'s form.
    Model,
    /// Who sent the request, and whom they acted for.
    Caller,
    /// The request's task label.
    Task,
}

impl Dimension {
    /// Every dimension, by its name.
    pub const ALL: [(&'static str, Dimension); 3] = [
        ("model", Dimension::Model),
        ("caller", Dimension::Caller),
        ("task", Dimension::Task),
    ];

    /// The dimension named `name`.
    pub fn named(name: &str) -> Option<Dimension> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
    }

    /// `log`'s fields along this dimension; a tag the request lacked is `None`.
    fn of(self, log: &Log) -> Vec<(String, Option<String>)> {
        let tags = |keys: &[&str]| {
            keys.iter()
                .map(|k| (k.to_string(), log.tag(k).map(String::from)))
                .collect()
        };
        match self {
            Dimension::Model => vec![
                ("provider".into(), Some(log.provider.clone())),
                ("model".into(), Some(canonical_model(&log.model))),
            ],
            Dimension::Caller => tags(&[tag::CALLER, tag::CALLER_NAME, tag::FOR, tag::FOR_NAME]),
            Dimension::Task => tags(&[tag::TASK]),
        }
    }
}

/// Requests, tokens and cost for one group of logs.
#[derive(Serialize, Debug, Default, PartialEq)]
pub struct Usage {
    /// What the group shares, such as its `provider` and `model`, or its `caller`.
    #[serde(flatten)]
    pub group: BTreeMap<String, Option<String>>,
    pub requests: u64,
    pub failures: u64,
    pub cached: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// As the gateway reports it; `None` when no request had a cost.
    pub cost: Option<f64>,
    /// Requests whose log may hold their bodies: those sent without [`PAYLOAD_HEADER`].
    pub payloads_kept: u64,
}

/// `logs` added up by the dimensions in `by`, the costliest first.
pub fn summarize(logs: &[Log], by: &[Dimension]) -> Vec<Usage> {
    let mut groups: BTreeMap<Vec<(String, Option<String>)>, Usage> = BTreeMap::new();
    for log in logs {
        let key: Vec<_> = by.iter().flat_map(|d| d.of(log)).collect();
        let u = groups.entry(key.clone()).or_insert_with(|| Usage {
            group: key.into_iter().collect(),
            ..Usage::default()
        });
        u.requests += 1;
        u.failures += u64::from(!log.success);
        u.cached += u64::from(log.cached);
        u.tokens_in += log.tokens_in.unwrap_or(0);
        u.tokens_out += log.tokens_out.unwrap_or(0);
        u.payloads_kept += u64::from(log.collect_log_payload == Some(true));
        if let Some(c) = log.cost {
            u.cost = Some(u.cost.unwrap_or(0.0) + c);
        }
    }
    let mut out: Vec<Usage> = groups.into_values().collect();
    out.sort_by(|a, b| {
        b.cost
            .unwrap_or(0.0)
            .total_cmp(&a.cost.unwrap_or(0.0))
            .then(b.requests.cmp(&a.requests))
    });
    out
}

fn rows(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// The gateway error code in a refusal's `body`: the object itself, or the first of a list in the
/// body, in `errors` or in `error`.
pub fn error_code(body: &Value) -> Option<u64> {
    let first = |v: &Value| match v {
        Value::Array(a) => a.iter().find_map(|e| e["code"].as_u64()),
        other => other["code"].as_u64(),
    };
    first(body)
        .or_else(|| first(&body["errors"]))
        .or_else(|| first(&body["error"]))
}

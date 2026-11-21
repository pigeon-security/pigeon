//! Cloudflare AI Gateway: one gateway in front of many model providers. This block has a gateway
//! exist, keeps its providers' keys (bring your own keys, stored in the Secrets Store and named
//! `{gateway}_{provider}_{alias}`), sends requests through it and reads its logs. Requests carry
//! `cf-aig-authorization`, a token minted for the call and revoked after; a provider's key never
//! leaves Cloudflare.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::{Date, Uint8Array};
use worker::{Fetch, Headers, Method, Request, RequestInit, Result};

use crate::Account;
use crate::cloudflare::call;
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

/// How long to wait before each retry of a request refused with 401: a token minted moments
/// before may not have reached the gateway yet.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// The Secrets Store name a gateway looks a provider's key up by.
pub fn secret_name(gateway: &str, provider: &str, alias: &str) -> String {
    format!("{gateway}_{provider}_{alias}")
}

/// A gateway as pigeon wants it: authenticated, keys only from its own store, logs kept.
pub fn spec(id: &str, store_id: &str) -> Value {
    json!({
        "id": id,
        "cache_ttl": 0,
        "cache_invalidate_on_update": false,
        "collect_logs": true,
        "rate_limiting_interval": 0,
        "rate_limiting_limit": 0,
        "authentication": true,
        "byok_only": true,
        "store_id": store_id,
    })
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
pub async fn ensure(account: &Account, id: &str) -> Result<Ensured> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &WRITE_PERMISSIONS)],
    )
    .await?;
    let ensured = ensure_with(token.value(), account, id).await;
    token.finish(ensured).await
}

async fn ensure_with(token: &str, account: &Account, id: &str) -> Result<Ensured> {
    let base = format!("/accounts/{}/ai-gateway/gateways", account.id);
    let listed = call(token, Method::Get, &format!("{base}?per_page=50"), None).await?;
    let want = spec(id, &account.secrets_store);
    match rows(&listed)
        .into_iter()
        .find(|g| g["id"].as_str() == Some(id))
    {
        Some(found) => Ok(Ensured {
            id: id.into(),
            created: false,
            drift: drift(&want, &found),
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

/// The settings in `want` that `found` holds otherwise.
fn drift(want: &Value, found: &Value) -> Vec<String> {
    ["authentication", "byok_only", "collect_logs", "store_id"]
        .iter()
        .filter(|k| want[**k] != found[**k])
        .map(|k| format!("{k} is {}, not {}", found[*k], want[*k]))
        .collect()
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

/// Sends `body` to `provider`'s `path` (such as `v1/messages`) through gateway `gateway`, with
/// `headers` beside the gateway's own. The provider's key is the gateway's, so none is sent.
pub async fn run(
    account: &Account,
    gateway: &str,
    provider: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<Ran> {
    let token = Scoped::open(
        account,
        "ai-gateway-run",
        vec![Policy::new(Scope::Account, &RUN_PERMISSIONS)],
    )
    .await?;
    let url = format!("{GATEWAY}/{}/{gateway}/{provider}/{path}", account.id);
    let ran = send(token.value(), &url, headers, body).await;
    token.finish(ran).await
}

async fn send(token: &str, url: &str, extra: &[(&str, &str)], body: &Value) -> Result<Ran> {
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
        let mut response = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        let status = response.status_code();
        let text = response.text().await?;
        if status == 401
            && let Some(ms) = waits.next()
        {
            worker::Delay::from(std::time::Duration::from_millis(*ms)).await;
            continue;
        }
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        return Ok(Ran { status, body });
    }
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
}

/// Gateway `gateway`'s logs since `since_ms` (milliseconds since the epoch), newest first, of
/// `provider` only when named: at most `max`, so a busy gateway cannot run a call long.
pub async fn logs(
    account: &Account,
    gateway: &str,
    since_ms: f64,
    provider: Option<&str>,
    max: usize,
) -> Result<Vec<Log>> {
    let token = Scoped::open(
        account,
        "ai-gateway",
        vec![Policy::new(Scope::Account, &READ_PERMISSIONS)],
    )
    .await?;
    let read = logs_with(token.value(), account, gateway, since_ms, provider, max).await;
    token.finish(read).await
}

async fn logs_with(
    token: &str,
    account: &Account,
    gateway: &str,
    since_ms: f64,
    provider: Option<&str>,
    max: usize,
) -> Result<Vec<Log>> {
    const PER_PAGE: usize = 50;
    let base = format!(
        "/accounts/{}/ai-gateway/gateways/{gateway}/logs",
        account.id
    );
    let mut out = Vec::new();
    for page in 1..=max.div_ceil(PER_PAGE) {
        let path = format!(
            "{base}?per_page={PER_PAGE}&page={page}&order_by=created_at&order_by_direction=desc"
        );
        let got: Vec<Log> = serde_json::from_value(call(token, Method::Get, &path, None).await?)?;
        let last_page = got.len() < PER_PAGE;
        for log in got {
            if Date::parse(&log.created_at) < since_ms {
                return Ok(out);
            }
            if provider.is_none_or(|p| log.provider == p) {
                out.push(log);
            }
            if out.len() >= max {
                return Ok(out);
            }
        }
        if last_page {
            break;
        }
    }
    Ok(out)
}

/// Requests, tokens and cost for one provider's model.
#[derive(Serialize, Debug, Default, PartialEq)]
pub struct Usage {
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub failures: u64,
    pub cached: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// As the gateway reports it; `None` when no request had a cost.
    pub cost: Option<f64>,
}

/// `logs` added up by provider and model, the costliest first.
pub fn summarize(logs: &[Log]) -> Vec<Usage> {
    let mut by: BTreeMap<(String, String), Usage> = BTreeMap::new();
    for log in logs {
        let u = by
            .entry((log.provider.clone(), log.model.clone()))
            .or_insert_with(|| Usage {
                provider: log.provider.clone(),
                model: log.model.clone(),
                ..Usage::default()
            });
        u.requests += 1;
        u.failures += u64::from(!log.success);
        u.cached += u64::from(log.cached);
        u.tokens_in += log.tokens_in.unwrap_or(0);
        u.tokens_out += log.tokens_out.unwrap_or(0);
        if let Some(c) = log.cost {
            u.cost = Some(u.cost.unwrap_or(0.0) + c);
        }
    }
    let mut out: Vec<Usage> = by.into_values().collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn log(provider: &str, model: &str, ok: bool, tin: u64, tout: u64, cost: Option<f64>) -> Log {
        Log {
            provider: provider.into(),
            model: model.into(),
            success: ok,
            tokens_in: Some(tin),
            tokens_out: Some(tout),
            cost,
            ..Log::default()
        }
    }

    #[test]
    fn usage_adds_up_by_provider_and_model_costliest_first() {
        let logs = [
            log("anthropic", "haiku", true, 10, 5, Some(0.001)),
            log("anthropic", "opus", true, 100, 50, Some(0.5)),
            log("anthropic", "haiku", false, 3, 0, None),
            log("openai", "gpt", true, 1, 1, None),
        ];
        let u = summarize(&logs);
        assert_eq!(u.len(), 3);
        assert_eq!(u[0].model, "opus");
        let haiku = u.iter().find(|x| x.model == "haiku").unwrap();
        assert_eq!((haiku.requests, haiku.failures), (2, 1));
        assert_eq!((haiku.tokens_in, haiku.tokens_out), (13, 5));
        assert_eq!(haiku.cost, Some(0.001));
        assert_eq!(
            u.iter().find(|x| x.provider == "openai").unwrap().cost,
            None
        );
    }

    #[test]
    fn drift_names_only_what_differs() {
        let want = spec("pigeon", "store");
        let mut found = want.clone();
        assert!(drift(&want, &found).is_empty());
        found["byok_only"] = json!(false);
        assert_eq!(drift(&want, &found), ["byok_only is false, not true"]);
    }

    #[test]
    fn secret_name_follows_the_gateways_convention() {
        assert_eq!(
            secret_name("pigeon", "anthropic", "default"),
            "pigeon_anthropic_default"
        );
    }
}

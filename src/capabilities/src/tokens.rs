//! Mints and revokes short-lived Cloudflare account API tokens from a parent token.
//! A minted token's value is returned to the caller only; it is never stored or logged.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use worker::js_sys::Date;
use worker::wasm_bindgen::JsValue;
use worker::{Error, Method, Result};

use crate::cloudflare::call;

/// How long a minted token lives unless asked otherwise.
pub const DEFAULT_TTL_SECS: u64 = 3600;

const ACCOUNT: &str = "com.cloudflare.api.account";
const ZONE: &str = "com.cloudflare.api.account.zone";

/// What a minted token's permissions apply to.
pub enum Scope {
    Account,
    Zones(Vec<String>),
}

/// A token to mint.
pub struct MintRequest {
    /// Names the token `pigeon:<purpose>:<timestamp>`.
    pub purpose: String,
    /// Cloudflare permission group names, such as `Workers Scripts Write`.
    pub permissions: Vec<String>,
    pub scope: Scope,
    pub ttl_secs: u64,
}

/// A minted token. `value` is the secret.
pub struct Minted {
    pub id: String,
    pub value: String,
    pub expires_on: String,
}

#[derive(Deserialize)]
struct Group {
    id: String,
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Mints `request` on `account`, as `parent`.
pub async fn mint(parent: &str, account: &str, request: &MintRequest) -> Result<Minted> {
    let groups: Vec<Group> = serde_json::from_value(
        call(parent, Method::Get, &format!("/accounts/{account}/tokens/permission_groups"), None)
            .await?,
    )?;
    let (scope, resources) = match &request.scope {
        Scope::Account => (ACCOUNT, json!({ format!("{ACCOUNT}.{account}"): "*" })),
        Scope::Zones(zones) => {
            let mut r = Map::new();
            for zone in zones {
                r.insert(format!("{ZONE}.{zone}"), json!("*"));
            }
            (ZONE, Value::Object(r))
        }
    };
    let mut ids = Vec::new();
    for name in &request.permissions {
        // Names repeat across scopes, so match the scope too.
        let group = groups
            .iter()
            .find(|g| g.name == *name && g.scopes.iter().any(|s| s == scope))
            .ok_or_else(|| Error::RustError(format!("no `{name}` permission group in {scope}")))?;
        ids.push(json!({ "id": group.id }));
    }
    let now = Date::now();
    let expires_on = iso(now + (request.ttl_secs as f64) * 1000.0);
    let body = json!({
        "name": format!("pigeon:{}:{}", request.purpose, iso(now)),
        "policies": [{ "effect": "allow", "permission_groups": ids, "resources": resources }],
        "not_before": iso(now),
        "expires_on": expires_on,
    });
    let v = call(parent, Method::Post, &format!("/accounts/{account}/tokens"), Some(&body)).await?;
    match (v["id"].as_str(), v["value"].as_str()) {
        (Some(id), Some(value)) => Ok(Minted {
            id: id.to_string(),
            value: value.to_string(),
            expires_on,
        }),
        _ => Err(Error::RustError("the minted token came back without its id or value".into())),
    }
}

/// Revokes token `id` on `account`, as `parent`.
pub async fn revoke(parent: &str, account: &str, id: &str) -> Result<()> {
    call(parent, Method::Delete, &format!("/accounts/{account}/tokens/{id}"), None).await?;
    Ok(())
}

/// `ms` since the epoch as RFC 3339 to the second, as the tokens API takes it.
fn iso(ms: f64) -> String {
    let s = String::from(Date::new(&JsValue::from_f64(ms)).to_iso_string());
    format!("{}Z", &s[..19])
}

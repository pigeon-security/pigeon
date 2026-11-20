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

/// What a policy's permissions apply to.
pub enum Scope {
    /// The account itself.
    Account,
    /// These zones, by ID.
    Zones(Vec<String>),
    /// Every zone in the account.
    AllZones,
}

/// Permission group names, such as `Workers Scripts Write`, on one scope.
pub struct Policy {
    pub permissions: Vec<String>,
    pub scope: Scope,
}

impl Policy {
    /// `permissions` on `scope`.
    pub fn new(scope: Scope, permissions: &[&str]) -> Self {
        Self {
            permissions: permissions.iter().map(|p| (*p).to_string()).collect(),
            scope,
        }
    }
}

/// A token to mint.
pub struct MintRequest {
    /// Names the token `pigeon:<purpose>:<timestamp>`.
    pub purpose: String,
    pub policies: Vec<Policy>,
    pub ttl_secs: u64,
}

impl MintRequest {
    /// A token for `purpose` holding `policies`, living [`DEFAULT_TTL_SECS`].
    pub fn new(purpose: &str, policies: Vec<Policy>) -> Self {
        Self {
            purpose: purpose.into(),
            policies,
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }
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
        call(
            parent,
            Method::Get,
            &format!("/accounts/{account}/tokens/permission_groups"),
            None,
        )
        .await?,
    )?;
    let mut policies = Vec::new();
    for policy in &request.policies {
        let (scope, resources) = match &policy.scope {
            Scope::Account => (ACCOUNT, json!({ format!("{ACCOUNT}.{account}"): "*" })),
            Scope::Zones(zones) => {
                let mut r = Map::new();
                for zone in zones {
                    r.insert(format!("{ZONE}.{zone}"), json!("*"));
                }
                (ZONE, Value::Object(r))
            }
            Scope::AllZones => (
                ZONE,
                json!({ format!("{ACCOUNT}.{account}"): { format!("{ZONE}.*"): "*" } }),
            ),
        };
        let mut ids = Vec::new();
        for name in &policy.permissions {
            // Names repeat across scopes, so match the scope too.
            let group = groups
                .iter()
                .find(|g| g.name == *name && g.scopes.iter().any(|s| s == scope))
                .ok_or_else(|| {
                    Error::RustError(format!("no `{name}` permission group in {scope}"))
                })?;
            ids.push(json!({ "id": group.id }));
        }
        policies
            .push(json!({ "effect": "allow", "permission_groups": ids, "resources": resources }));
    }
    let now = Date::now();
    let expires_on = iso(now + (request.ttl_secs as f64) * 1000.0);
    let body = json!({
        "name": format!("pigeon:{}:{}", request.purpose, iso(now)),
        "policies": policies,
        "not_before": iso(now),
        "expires_on": expires_on,
    });
    let v = call(
        parent,
        Method::Post,
        &format!("/accounts/{account}/tokens"),
        Some(&body),
    )
    .await?;
    match (v["id"].as_str(), v["value"].as_str()) {
        (Some(id), Some(value)) => Ok(Minted {
            id: id.to_string(),
            value: value.to_string(),
            expires_on,
        }),
        _ => Err(Error::RustError(
            "the minted token came back without its id or value".into(),
        )),
    }
}

/// Revokes token `id` on `account`, as `parent`.
pub async fn revoke(parent: &str, account: &str, id: &str) -> Result<()> {
    call(
        parent,
        Method::Delete,
        &format!("/accounts/{account}/tokens/{id}"),
        None,
    )
    .await?;
    Ok(())
}

/// Whether `token` is active on `account`: its status, as Cloudflare reports it.
pub async fn verify(token: &str, account: &str) -> Result<String> {
    let v = call(
        token,
        Method::Get,
        &format!("/accounts/{account}/tokens/verify"),
        None,
    )
    .await?;
    Ok(v["status"].as_str().unwrap_or("unknown").to_string())
}

/// `ms` since the epoch as RFC 3339 to the second, as the tokens API takes it.
fn iso(ms: f64) -> String {
    let s = String::from(Date::new(&JsValue::from_f64(ms)).to_iso_string());
    format!("{}Z", &s[..19])
}

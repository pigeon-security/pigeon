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

#[derive(Deserialize, Clone)]
struct Group {
    id: String,
    name: String,
    #[serde(default)]
    scopes: Vec<String>,
}

/// How long this isolate keeps the account's permission groups: they change only when
/// Cloudflare adds a product, and a name not found asks again.
const GROUPS_KEPT_MS: f64 = 3_600_000.0;

std::thread_local! {
    /// The account's permission groups and when they were read, for this isolate.
    static GROUPS: std::cell::RefCell<Option<(String, f64, Vec<Group>)>> =
        const { std::cell::RefCell::new(None) };
}

/// `account`'s permission groups, read at most hourly per isolate: every mint needs them, and
/// asking each time doubled a mint's round trips.
async fn permission_groups(parent: &str, account: &str) -> Result<Vec<Group>> {
    let now = Date::now();
    let kept = GROUPS.with(|g| {
        g.borrow()
            .as_ref()
            .filter(|(a, at, _)| a == account && now - at < GROUPS_KEPT_MS)
            .map(|(_, _, groups)| groups.clone())
    });
    if let Some(groups) = kept {
        return Ok(groups);
    }
    let groups: Vec<Group> = serde_json::from_value(
        call(
            parent,
            Method::Get,
            &format!("/accounts/{account}/tokens/permission_groups"),
            None,
        )
        .await?,
    )?;
    GROUPS.with(|g| *g.borrow_mut() = Some((account.to_string(), now, groups.clone())));
    Ok(groups)
}

/// Mints `request` on `account`, as `parent`.
pub async fn mint(parent: &str, account: &str, request: &MintRequest) -> Result<Minted> {
    let groups = permission_groups(parent, account).await?;
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
                    // A kept list may predate the group: the next mint reads it afresh.
                    GROUPS.with(|g| *g.borrow_mut() = None);
                    Error::RustError(format!(
                        "no `{name}` permission group in {scope}; similar: {}",
                        similar(name, &groups)
                    ))
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

/// What Cloudflare says of `token` on `account`: its `status` and, when it has one, its
/// `expires_on`. Never the token.
pub async fn verify_detail(token: &str, account: &str) -> Result<Value> {
    let v = call(
        token,
        Method::Get,
        &format!("/accounts/{account}/tokens/verify"),
        None,
    )
    .await?;
    Ok(json!({ "status": v["status"], "expires_on": v["expires_on"] }))
}

/// `ms` since the epoch as RFC 3339 to the second, as the tokens API takes it.
fn iso(ms: f64) -> String {
    let s = String::from(Date::new(&JsValue::from_f64(ms)).to_iso_string());
    format!("{}Z", &s[..19])
}

/// A token minted for one capability's calls. Call [`Scoped::finish`] to revoke it.
pub struct Scoped<'a> {
    account: &'a crate::Account,
    minted: Minted,
}

impl<'a> Scoped<'a> {
    /// Mints a token for `purpose` holding `policies`, on `account`.
    pub async fn open(
        account: &'a crate::Account,
        purpose: &str,
        policies: Vec<Policy>,
    ) -> Result<Self> {
        let minted = mint(
            &account.parent,
            &account.id,
            &MintRequest::new(purpose, policies),
        )
        .await?;
        Ok(Self { account, minted })
    }

    /// [`Scoped::open`], with the token living `ttl_secs` instead of the default.
    pub async fn open_for(
        account: &'a crate::Account,
        purpose: &str,
        policies: Vec<Policy>,
        ttl_secs: u64,
    ) -> Result<Self> {
        let request = MintRequest {
            ttl_secs,
            ..MintRequest::new(purpose, policies)
        };
        let minted = mint(&account.parent, &account.id, &request).await?;
        Ok(Self { account, minted })
    }

    /// The token's value, for API calls.
    pub fn value(&self) -> &str {
        &self.minted.value
    }

    /// Revokes the token, then answers `result`: the work's own error comes first.
    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        let revoked = revoke(&self.account.parent, &self.account.id, &self.minted.id).await;
        let value = result?;
        revoked?;
        Ok(value)
    }
}

/// The group names sharing `name`'s first word, to show what a wrong name may have meant.
fn similar(name: &str, groups: &[Group]) -> String {
    let first = name
        .split_whitespace()
        .next()
        .unwrap_or(name)
        .to_lowercase();
    let mut names: Vec<&str> = groups
        .iter()
        .filter(|g| g.name.to_lowercase().contains(&first))
        .map(|g| g.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

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

/// The JSON body that creates `request` on `account`: its policies with their permission group
/// ids, a name from [`name`], and the validity window. Reads the permission groups as `parent`.
pub async fn mint_body(parent: &str, account: &str, request: &MintRequest) -> Result<Value> {
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
    Ok(json!({
        "name": name(&request.purpose, &iso(now)),
        "policies": policies,
        "not_before": iso(now),
        "expires_on": expires_on,
    }))
}

/// Mints `request` on `account`, as `parent`.
pub async fn mint(parent: &str, account: &str, request: &MintRequest) -> Result<Minted> {
    let body = mint_body(parent, account, request).await?;
    let expires_on = body["expires_on"].as_str().unwrap_or_default().to_string();
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

/// The start of every name pigeon gives a token it mints.
pub const NAME_PREFIX: &str = "pigeon:";

/// The start of the name of every token core mints for a capability Worker. Workers may revoke
/// only tokens whose name starts with it (see `scoped_token_revocable` in the capabilities).
pub const WORKER_NAME_PREFIX: &str = "pigeon:worker-";

/// The length of a name's time part, `YYYY-MM-DDTHH:MM:SSZ`.
const TIME_LEN: usize = 20;

/// The name of a token minted for `purpose` at `time` (RFC 3339 to the second, ending in `Z`):
/// `pigeon:<purpose>:<time>`. [`minted`] is its inverse. Pure.
pub fn name(purpose: &str, time: &str) -> String {
    format!("{NAME_PREFIX}{purpose}:{time}")
}

/// Whether `t` has the shape `YYYY-MM-DDTHH:MM:SSZ`.
fn is_time(t: &str) -> bool {
    t.len() == TIME_LEN
        && t.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            19 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

/// The purpose of a token name of the exact shape `pigeon:<purpose>:<YYYY-MM-DDTHH:MM:SSZ>`,
/// with a non-empty purpose holding no `:`. Any other name (the parent token, a person's or a
/// job's token, a name with extra parts) answers `None`. Pure.
pub fn minted(name: &str) -> Option<&str> {
    let rest = name.strip_prefix(NAME_PREFIX)?;
    let split = rest.len().checked_sub(TIME_LEN + 1)?;
    let (purpose, time) = (rest.get(..split)?, rest.get(split + 1..)?);
    let colon = rest.as_bytes()[split] == b':';
    (colon && !purpose.is_empty() && !purpose.contains(':') && is_time(time)).then_some(purpose)
}

/// One token as the account's token list shows it. Never holds the token's value.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Listed {
    /// The token's id.
    pub id: String,
    /// The token's name.
    #[serde(default)]
    pub name: String,
    /// When it expires, RFC 3339; absent for a token that never expires.
    #[serde(default)]
    pub expires_on: Option<String>,
    /// Cloudflare's status, such as `active` or `expired`.
    #[serde(default)]
    pub status: String,
}

/// The account's tokens, as far as the sweeper and its tests need them.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait TokenApi {
    /// One page of the account's tokens, numbered from 1. An empty page ends the list.
    async fn list(&self, page: u32) -> Result<Vec<Listed>>;
    /// Creates the token `body` describes (the JSON the tokens API takes).
    async fn create(&self, body: &Value) -> Result<Minted>;
    /// Deletes token `id`.
    async fn delete(&self, id: &str) -> Result<()>;
}

/// [`TokenApi`] over Cloudflare, as the parent token, on one account.
pub struct CloudflareTokens<'a> {
    /// The parent token.
    pub parent: &'a str,
    /// The account's id.
    pub account: &'a str,
}

impl TokenApi for CloudflareTokens<'_> {
    async fn list(&self, page: u32) -> Result<Vec<Listed>> {
        let path = format!("/accounts/{}/tokens?page={page}&per_page=50", self.account);
        let v = call(self.parent, Method::Get, &path, None).await?;
        Ok(serde_json::from_value(v)?)
    }

    async fn create(&self, body: &Value) -> Result<Minted> {
        let v = call(
            self.parent,
            Method::Post,
            &format!("/accounts/{}/tokens", self.account),
            Some(body),
        )
        .await?;
        match (v["id"].as_str(), v["value"].as_str()) {
            (Some(id), Some(value)) => Ok(Minted {
                id: id.to_string(),
                value: value.to_string(),
                expires_on: body["expires_on"].as_str().unwrap_or_default().to_string(),
            }),
            _ => Err(Error::RustError(
                "the minted token came back without its id or value".into(),
            )),
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        revoke(self.parent, self.account, id).await
    }
}

/// Milliseconds since the epoch of an RFC 3339 time such as `2026-11-24T05:04:00Z`, with an
/// optional fraction, ending in `Z`. `None` when it does not parse. Pure.
pub fn parse_ms(t: &str) -> Option<i64> {
    let t = t.strip_suffix('Z')?;
    let (t, frac) = t.split_once('.').unwrap_or((t, ""));
    if t.len() != TIME_LEN - 1 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, s) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let millis: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000 + millis)
}

/// Whether the sweeper may delete `token`: pigeon minted it and it has expired.
fn sweepable(token: &Listed, now_ms: i64) -> bool {
    minted(&token.name).is_some()
        && (token.status == "expired"
            || token
                .expires_on
                .as_deref()
                .and_then(parse_ms)
                .is_some_and(|at| at <= now_ms))
}

/// The ids to delete: tokens named by [`minted`] that have expired (an `expires_on` in the past,
/// or status `expired`). A token of any other name, an unexpired one, and one with no
/// `expires_on` are never in it. Pure.
pub fn sweep_plan(tokens: &[Listed], now_ms: i64) -> Vec<String> {
    tokens
        .iter()
        .filter(|t| sweepable(t, now_ms))
        .map(|t| t.id.clone())
        .collect()
}

/// What one sweep did: tokens `deleted`, pigeon's live tokens `kept_pigeon` (the live-token
/// gauge), tokens `skipped_other` (not pigeon's), and the deletes that `failed` as (id, error).
/// The errors name the account, so log their count, not their text.
#[derive(Debug, Default, PartialEq)]
pub struct SweepReport {
    /// Expired pigeon tokens deleted.
    pub deleted: usize,
    /// Pigeon tokens left alone because they have not expired.
    pub kept_pigeon: usize,
    /// Tokens whose name is not pigeon's, never touched.
    pub skipped_other: usize,
    /// Deletes that failed, as (token id, error). They do not stop the sweep.
    pub failed: Vec<(String, String)>,
}

/// The most pages a sweep reads, so an API that ignores `page` cannot loop it forever.
const MAX_PAGES: u32 = 200;

/// Lists the account's tokens (page by page until an empty page), deletes the expired ones
/// named by [`minted`], and counts. A failed delete is recorded and the rest go on.
pub async fn sweep(api: &impl TokenApi, now_ms: i64) -> Result<SweepReport> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let got = api.list(page).await?;
        if got.is_empty() {
            break;
        }
        all.extend(got);
    }
    let mut report = SweepReport::default();
    for token in &all {
        if minted(&token.name).is_none() {
            report.skipped_other += 1;
        } else if sweepable(token, now_ms) {
            match api.delete(&token.id).await {
                Ok(()) => report.deleted += 1,
                Err(e) => report.failed.push((token.id.clone(), e.to_string())),
            }
        } else {
            report.kept_pigeon += 1;
        }
    }
    Ok(report)
}

/// Whether `token` is named by [`minted`] and has not expired at `now_ms`. A token with no
/// `expires_on` counts as live. Pure.
fn live_pigeon(token: &Listed, now_ms: i64) -> bool {
    minted(&token.name).is_some() && !sweepable(token, now_ms) && token.status != "expired"
}

/// Counts the account's live pigeon-minted tokens: lists every page and counts the names
/// [`minted`] accepts that have not expired. Read-only: it deletes nothing.
pub async fn count_live(api: &impl TokenApi, now_ms: i64) -> Result<usize> {
    let mut live = 0;
    for page in 1..=MAX_PAGES {
        let got = api.list(page).await?;
        if got.is_empty() {
            break;
        }
        live += got.iter().filter(|t| live_pigeon(t, now_ms)).count();
    }
    Ok(live)
}

/// `ms` since the epoch as RFC 3339 to the second, as the tokens API takes it.
fn iso(ms: f64) -> String {
    let s = String::from(Date::new(&JsValue::from_f64(ms)).to_iso_string());
    format!("{}Z", &s[..19])
}

/// A token minted for one capability's calls. Call [`Scoped::finish`] to revoke it.
pub struct Scoped<'a> {
    account: &'a crate::Account,
    /// `None` for a linked account's bearer, which is used as it is and never revoked.
    minted: Option<Minted>,
    bearer: Option<&'a str>,
}

impl<'a> Scoped<'a> {
    /// Mints a token for `purpose` holding `policies`, on `account`.
    pub async fn open(
        account: &'a crate::Account,
        purpose: &str,
        policies: Vec<Policy>,
    ) -> Result<Self> {
        if let Some(bearer) = account.bearer.as_deref() {
            return Ok(Self::linked(account, bearer));
        }
        let minted = mint(
            &account.parent,
            &account.id,
            &MintRequest::new(purpose, policies),
        )
        .await?;
        Ok(Self {
            account,
            minted: Some(minted),
            bearer: None,
        })
    }

    /// A linked account's granted token, standing in for a minted one: what it may do is what
    /// the account's owner granted, not `policies`.
    fn linked(account: &'a crate::Account, bearer: &'a str) -> Self {
        Self {
            account,
            minted: None,
            bearer: Some(bearer),
        }
    }

    /// [`Scoped::open`], with the token living `ttl_secs` instead of the default.
    pub async fn open_for(
        account: &'a crate::Account,
        purpose: &str,
        policies: Vec<Policy>,
        ttl_secs: u64,
    ) -> Result<Self> {
        if let Some(bearer) = account.bearer.as_deref() {
            return Ok(Self::linked(account, bearer));
        }
        let request = MintRequest {
            ttl_secs,
            ..MintRequest::new(purpose, policies)
        };
        let minted = mint(&account.parent, &account.id, &request).await?;
        Ok(Self {
            account,
            minted: Some(minted),
            bearer: None,
        })
    }

    /// The token's value, for API calls.
    pub fn value(&self) -> &str {
        match (&self.minted, self.bearer) {
            (Some(m), _) => &m.value,
            (None, Some(b)) => b,
            (None, None) => "",
        }
    }

    /// Revokes the token, then answers `result`: the work's own error comes first. A linked
    /// account's granted token is not revoked.
    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        let Some(minted) = self.minted else {
            return result;
        };
        let revoked = revoke(&self.account.parent, &self.account.id, &minted.id).await;
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

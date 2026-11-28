//! The minted multipart publisher (docs/plans/multipart-enable.md, "Automatic").
//!
//! With `data.multipart_mint` and the link's optional Account API Tokens Write scope, an
//! approved instance plan mints one account-owned API token whose only policy is R2 object
//! read and write on exactly the instance's Data bucket. Pigeon mints no other shape
//! ([`allowed_policy`] checks every body before it is sent), never from a bare tool call, and
//! records every mint and revoke in the instance's ledger. The token's value and the S3 secret
//! derived from it exist only inside [`mint`]: they are written to the publisher slot and
//! dropped.

use pigeon_core::{Account, cloudflare};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use super::data_publisher::{credential, slot_for, write_slot};
use super::plan::Ledger;
use super::record::Instance;

fn bad(message: impl Into<String>) -> Error {
    Error::RustError(message.into())
}

/// The capability whose optional link scope lets pigeon mint the publisher token.
pub const SCOPE: &str = "data multipart mint";
/// The two permission groups the minted token holds, by Cloudflare's names, on one bucket.
pub const GROUPS: [&str; 2] = [
    "Workers R2 Storage Bucket Item Write",
    "Workers R2 Storage Bucket Item Read",
];
/// The resource scope those groups must apply to.
const BUCKET_SCOPE: &str = "com.cloudflare.edge.r2.bucket";
const DAY_MS: i64 = 24 * 3_600_000;
/// How long a minted token lives.
pub const TTL_DAYS: i64 = 365;
/// How long before its expiry a plan mints the next one.
pub const RENEW_DAYS: i64 = 30;
/// The most audit lines the ledger keeps.
pub const AUDIT_KEPT: usize = 50;

/// What the ledger keeps of a minted token: never its value.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Minted {
    pub token: String,
    pub name: String,
    pub bucket: String,
    pub slot: String,
    pub fingerprint: String,
    pub minted_ms: i64,
    pub expires_ms: i64,
}

/// One mint or revoke, for the instance's audit.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuditLine {
    pub at_ms: i64,
    /// `minted` or `revoked`.
    pub action: String,
    pub token: String,
    pub bucket: String,
}

/// Whether `instance` asks pigeon to mint its publisher.
pub fn mints(instance: &Instance) -> bool {
    crate::data_store::settings(instance).is_some_and(|s| s.multipart_mint)
}

/// Whether `instance` wants a multipart publisher at all, either way.
pub fn wanted(instance: &Instance) -> bool {
    super::data_publisher::fingerprint(instance).is_some() || mints(instance)
}

/// The fingerprint the bound publisher must have: the record's, or the minted token's.
pub fn expected<'a>(instance: &'a Instance, ledger: &'a Ledger) -> Option<&'a str> {
    if mints(instance) {
        ledger.multipart.as_ref().map(|m| m.fingerprint.as_str())
    } else {
        super::data_publisher::fingerprint(instance)
    }
}

/// Whether a plan must mint now: nothing minted for this bucket, its slot gone, or within
/// [`RENEW_DAYS`] of expiry at `now_ms`.
pub fn due(minted: Option<&Minted>, bucket: &str, slot_present: bool, now_ms: i64) -> bool {
    match minted {
        None => true,
        Some(m) => {
            m.bucket != bucket
                || !slot_present
                || m.expires_ms.saturating_sub(now_ms) < RENEW_DAYS * DAY_MS
        }
    }
}

/// The resource of exactly `bucket` in `account` (no jurisdiction), as Cloudflare names it.
pub fn bucket_resource(account: &str, bucket: &str) -> String {
    format!("{BUCKET_SCOPE}.{account}_default_{bucket}")
}

/// The ids of [`GROUPS`] in an account's permission-group listing, in that order: each matched
/// by its exact name and bucket scope, once. `None` unless both are found.
pub fn groups(listed: &Value) -> Option<[String; 2]> {
    let find = |name: &str| {
        let mut found = listed
            .as_array()?
            .iter()
            .filter(|g| {
                g["name"].as_str() == Some(name)
                    && g["scopes"]
                        .as_array()
                        .is_some_and(|s| s.iter().any(|s| s.as_str() == Some(BUCKET_SCOPE)))
            })
            .filter_map(|g| g["id"].as_str());
        let id = found.next()?;
        (found.next().is_none() && !id.is_empty()).then(|| id.to_string())
    };
    let ids = [find(GROUPS[0])?, find(GROUPS[1])?];
    (ids[0] != ids[1]).then_some(ids)
}

/// The token's name: recognisable, per instance and day.
pub fn token_name(instance: &str, now_ms: i64) -> String {
    format!(
        "pigeon-data-multipart-{instance}-{}",
        crate::expiry::date(now_ms)
    )
}

/// The one token body pigeon mints: allow, the two bucket-item groups, on exactly `bucket`,
/// expiring in [`TTL_DAYS`].
pub fn body(account: &str, bucket: &str, instance: &str, ids: &[String; 2], now_ms: i64) -> Value {
    json!({
        "name": token_name(instance, now_ms),
        "expires_on": format!("{}T00:00:00Z", crate::expiry::date(now_ms + TTL_DAYS * DAY_MS)),
        "policies": [{
            "effect": "allow",
            "resources": { bucket_resource(account, bucket): "*" },
            "permission_groups": [{ "id": ids[0] }, { "id": ids[1] }],
        }],
    })
}

/// Whether `body` is exactly the shape [`body`] makes for `account`'s `bucket` with `ids`: one
/// allow policy holding those two groups and nothing else, on that one bucket, with a name and
/// an expiry and no other field. Every mint is checked with this before it is sent.
pub fn allowed_policy(body: &Value, account: &str, bucket: &str, ids: &[String; 2]) -> bool {
    let Some(top) = body.as_object() else {
        return false;
    };
    if top.len() != 3
        || !top
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n.starts_with("pigeon-data-multipart-"))
        || !top.get("expires_on").is_some_and(Value::is_string)
    {
        return false;
    }
    let Some([policy]) = top
        .get("policies")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    else {
        return false;
    };
    let Some(policy) = policy.as_object() else {
        return false;
    };
    let resource = bucket_resource(account, bucket);
    let resources_ok = policy
        .get("resources")
        .and_then(Value::as_object)
        .is_some_and(|r| r.len() == 1 && r.get(&resource).and_then(Value::as_str) == Some("*"));
    let mut got: Vec<&str> = policy
        .get("permission_groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|g| {
            g.as_object()
                .filter(|o| o.len() == 1)
                .and_then(|o| o.get("id")?.as_str())
                .unwrap_or("")
        })
        .collect();
    got.sort_unstable();
    let mut want = vec![ids[0].as_str(), ids[1].as_str()];
    want.sort_unstable();
    policy.len() == 3
        && policy.get("effect").and_then(Value::as_str) == Some("allow")
        && resources_ok
        && got == want
        && ids[0] != ids[1]
        && account.len() == 32
        && account.bytes().all(|b| b.is_ascii_hexdigit())
        && !bucket.is_empty()
        && !bucket.contains('*')
}

/// The R2 S3 keys of an API token, as Cloudflare documents them: the Access Key ID is the
/// token's id, the Secret Access Key the SHA-256 hex of its value.
pub fn token_keys(id: &str, value: &str) -> pigeon_core::s3::Keys {
    pigeon_core::s3::Keys {
        access_key_id: id.into(),
        secret_access_key: pigeon_ingest::datastore::sha256_hex(value.as_bytes()),
    }
}

/// Mints the publisher token for `instance`'s `bucket` with the link's `token`, writes its
/// credential to the publisher slot in `store`, and answers what the ledger keeps. The token's
/// value and secret never leave this call; a token whose slot could not be written is revoked.
pub async fn mint(
    token: &str,
    account: &str,
    instance: &str,
    bucket: &str,
    store: &str,
    now_ms: i64,
) -> Result<Minted> {
    let listed = cloudflare::call(
        token,
        Method::Get,
        &format!("/accounts/{account}/tokens/permission_groups"),
        None,
    )
    .await
    .map_err(|e| {
        bad(crate::linked::explain(
            instance,
            SCOPE,
            &format!("reading the account's permission groups: {e}"),
        ))
    })?;
    let ids = groups(&listed)
        .ok_or_else(|| bad("the account lists no bucket-scoped R2 object permission groups"))?;
    let body = body(account, bucket, instance, &ids, now_ms);
    if !allowed_policy(&body, account, bucket, &ids) {
        return Err(bad(
            "refused: pigeon mints only a token for one Data bucket's objects",
        ));
    }
    let made = cloudflare::call(
        token,
        Method::Post,
        &format!("/accounts/{account}/tokens"),
        Some(&body),
    )
    .await
    .map_err(|_| bad("Cloudflare did not mint the bucket-scoped R2 token"))?;
    let (Some(id), Some(value)) = (made["id"].as_str(), made["value"].as_str()) else {
        return Err(bad("Cloudflare answered no token id or value"));
    };
    let id = id.to_string();
    let keys = token_keys(&id, value);
    let (text, fingerprint) = credential(account, bucket, &keys);
    let slot = slot_for(instance, &fingerprint);
    let linked = Account {
        id: account.into(),
        parent: String::new(),
        secrets_store: String::new(),
        bearer: Some(token.into()),
        token_ledger: None,
    };
    if let Err(e) = write_slot(&linked, store, &slot, &text).await {
        let _ = revoke(token, account, &id).await;
        return Err(e);
    }
    Ok(Minted {
        token: id,
        name: body["name"].as_str().unwrap_or_default().into(),
        bucket: bucket.into(),
        slot,
        fingerprint,
        minted_ms: now_ms,
        expires_ms: now_ms + TTL_DAYS * DAY_MS,
    })
}

/// Revokes minted token `id`; one already gone counts as revoked.
pub async fn revoke(token: &str, account: &str, id: &str) -> Result<()> {
    match cloudflare::call(
        token,
        Method::Delete,
        &format!("/accounts/{account}/tokens/{id}"),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains(": 404:") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Records `minted` in `ledger`: the earlier token retired for revoke, the audit appended.
pub fn record_mint(ledger: &mut Ledger, minted: Minted) {
    if let Some(old) = ledger.multipart.take()
        && !ledger.multipart_retired.contains(&old.token)
    {
        ledger.multipart_retired.push(old.token);
    }
    audit(
        ledger,
        "minted",
        &minted.token,
        &minted.bucket,
        minted.minted_ms,
    );
    ledger.multipart = Some(minted);
}

/// Records the revoke of `token` in `ledger`.
pub fn record_revoke(ledger: &mut Ledger, token: &str, bucket: &str, at_ms: i64) {
    ledger.multipart_retired.retain(|t| t != token);
    audit(ledger, "revoked", token, bucket, at_ms);
}

fn audit(ledger: &mut Ledger, action: &str, token: &str, bucket: &str, at_ms: i64) {
    ledger.multipart_audit.push(AuditLine {
        at_ms,
        action: action.into(),
        token: token.into(),
        bucket: bucket.into(),
    });
    let over = ledger.multipart_audit.len().saturating_sub(AUDIT_KEPT);
    ledger.multipart_audit.drain(..over);
}

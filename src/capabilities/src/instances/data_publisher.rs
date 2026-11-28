//! An opt-in R2 publisher for a linked instance's private Data bucket.
use pigeon_core::{Account, cloudflare, oauth, r2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use super::record::{AccountRef, Instance};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Publisher {
    pub account: String,
    pub bucket: String,
    pub store: String,
    pub slot: String,
    generation: String,
    fingerprint: String,
}

impl Publisher {
    pub fn applies_to(&self, instance: &Instance, bucket: &str) -> bool {
        matches!(&instance.account, AccountRef::Linked { account_id }
            if account_id.as_deref() == Some(self.account.as_str()))
            && self.bucket == bucket
            && fingerprint(instance) == Some(self.fingerprint.as_str())
    }
}

pub fn fingerprint(instance: &Instance) -> Option<&str> {
    crate::data_store::settings(instance)?
        .multipart_publisher
        .as_deref()
}

fn slot(instance: &Instance, fingerprint: &str) -> String {
    slot_for(&instance.id, fingerprint)
}

/// The start of every publisher slot of instance `id`, whatever its credential.
pub fn slot_prefix(id: &str) -> String {
    // Distinct even when two instances share a linked account.
    let instance_hash = pigeon_ingest::datastore::sha256_hex(id.as_bytes());
    format!("DATA_R2_PUBLISH_{}_", &instance_hash[..16])
}

/// The Secrets Store slot of instance `id`'s publisher credential with `fingerprint`.
pub fn slot_for(id: &str, fingerprint: &str) -> String {
    format!("{}{}", slot_prefix(id), &fingerprint[..24])
}

/// The publisher credential for `keys` on `account`'s `bucket`: the exact JSON the Data Worker
/// reads, and its fingerprint (the lowercase SHA-256 of those bytes). The only way either is
/// built, for the manual and the minted path alike (docs/plans/multipart-enable.md).
pub fn credential(account: &str, bucket: &str, keys: &pigeon_core::s3::Keys) -> (String, String) {
    let text = json!({
        "account": account,
        "bucket": bucket,
        "keys": format!("{}:{}", keys.access_key_id, keys.secret_access_key),
    })
    .to_string();
    let fingerprint = pigeon_ingest::datastore::sha256_hex(text.as_bytes());
    (text, fingerprint)
}

/// The scratch prefix the manual key check writes one object under, then deletes.
pub const KEY_CHECK_PREFIX: &str = "ingest/canary/key-check/";

/// Whether a ListBuckets answer (`status`, its bucket names) shows a key limited to `bucket`:
/// refused outright, or listing nothing but it.
pub fn listing_scoped(status: u16, names: &[String], bucket: &str) -> bool {
    match status {
        200 => names.iter().all(|n| n == bucket),
        401 | 403 => true,
        _ => false,
    }
}

async fn s3_call(
    endpoint: &pigeon_core::s3::Endpoint,
    keys: &pigeon_core::s3::Keys,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> Result<(u16, String)> {
    use pigeon_core::s3;
    let payload = body.map_or(s3::EMPTY_SHA256.to_string(), |b| {
        pigeon_ingest::datastore::sha256_hex(b.as_bytes())
    });
    let date = s3::amz_date(worker::js_sys::Date::now());
    let signed = s3::sign(
        method.as_ref(),
        endpoint,
        path,
        &[],
        &[],
        &payload,
        keys,
        &date,
    );
    let headers: Vec<_> = signed
        .iter()
        .filter(|(name, _)| name.as_str() != "host")
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    let url = format!("{}{path}", endpoint.base);
    pigeon_cloud::scaleway_ingest::http(
        method,
        &url,
        &headers,
        body.map(|b| ("application/octet-stream", b.to_string())),
    )
    .await
    .map_err(|_| bad("R2 could not be reached to check the key"))
}

/// Checks that `keys` write `bucket` in `account` and nothing else it can see: a PUT and a
/// DELETE of one scratch object under [`KEY_CHECK_PREFIX`], and a ListBuckets that is refused or
/// lists only `bucket`. Errors never hold the key.
pub async fn verify_key(account: &str, bucket: &str, keys: &pigeon_core::s3::Keys) -> Result<()> {
    use pigeon_core::s3;
    let endpoint = s3::Endpoint {
        base: format!("https://{account}.r2.cloudflarestorage.com"),
        region: "auto".into(),
    };
    let nonce = pigeon_ingest::datastore::sha256_hex(pigeon_core::webauthn::random()?.as_bytes());
    let path = s3::object_path(bucket, &format!("{KEY_CHECK_PREFIX}{}", &nonce[..32]));
    let (put, _) = s3_call(
        &endpoint,
        keys,
        Method::Put,
        &path,
        Some("pigeon key check"),
    )
    .await?;
    if put != 200 {
        return Err(bad(format!(
            "the key could not write the Data bucket {bucket} (R2 answered {put}): give it \
             Object Read & Write on that bucket"
        )));
    }
    let (deleted, _) = s3_call(&endpoint, keys, Method::Delete, &path, None).await?;
    if !matches!(deleted, 200 | 204) {
        return Err(bad(format!(
            "the key wrote but could not delete its check object in {bucket} (R2 answered \
             {deleted})"
        )));
    }
    let (status, body) = s3_call(&endpoint, keys, Method::Get, "/", None).await?;
    let names: Vec<String> = if status == 200 {
        s3::parse_list_buckets(&body)
            .map_err(|_| bad("R2 answered an unreadable bucket listing"))?
            .into_iter()
            .map(|b| b.name)
            .collect()
    } else {
        Vec::new()
    };
    if !listing_scoped(status, &names, bucket) {
        return Err(bad(format!(
            "the key reaches more than the Data bucket {bucket}: make one limited to that \
             bucket only"
        )));
    }
    Ok(())
}

/// Writes `text` (a [`credential`]) to `slot` in the linked account's Secrets Store, Workers
/// scope, through the link. Answers nothing of the value.
pub async fn write_slot(account: &Account, store: &str, slot: &str, text: &str) -> Result<()> {
    let scoped = Account {
        id: account.id.clone(),
        parent: String::new(),
        secrets_store: store.into(),
        bearer: account.bearer.clone(),
        token_ledger: None,
    };
    let writer = pigeon_core::secrets_store::Writer::open(&scoped).await?;
    let written = writer
        .put_scoped(
            slot,
            text,
            "pigeon: Data multipart publisher credential",
            &[pigeon_core::secrets_store::WORKERS],
        )
        .await;
    writer.finish(written).await.map(|_| ())
}

/// Every publisher slot of instance `id` in `listed` (a Secrets Store listing) but `current`:
/// the earlier credentials a bound plan removes. Each as `(id, name)`.
pub fn stale_slots(listed: &Value, id: &str, current: &str) -> Vec<(String, String)> {
    let prefix = slot_prefix(id);
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s["id"].as_str()?, s["name"].as_str()?)))
        .filter(|(sid, name)| !sid.is_empty() && name.starts_with(&prefix) && *name != current)
        .map(|(sid, name)| (sid.to_string(), name.to_string()))
        .collect()
}

/// The lifecycle rule the data plan keeps on the Data bucket while a publisher applies.
pub const LIFECYCLE_RULE: &str = "pigeon-abort-incomplete-multipart";
/// Its age: 7 days, in seconds.
pub const LIFECYCLE_MAX_AGE: u64 = 7 * 24 * 3600;

/// Whether `rules` (a bucket's lifecycle rules) already abort every incomplete multipart upload
/// within [`LIFECYCLE_MAX_AGE`].
pub fn lifecycle_ok(rules: &Value) -> bool {
    rules.as_array().into_iter().flatten().any(|r| {
        r["enabled"].as_bool() == Some(true)
            && r["conditions"]["prefix"].as_str().unwrap_or("").is_empty()
            && r["abortMultipartUploadsTransition"]["condition"]["type"] == "Age"
            && r["abortMultipartUploadsTransition"]["condition"]["maxAge"]
                .as_u64()
                .is_some_and(|age| age <= LIFECYCLE_MAX_AGE)
    })
}

/// `rules` with pigeon's rule in place: every other rule kept as it is.
pub fn lifecycle_with_ours(rules: &Value) -> Value {
    let mut out: Vec<Value> = rules
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["id"].as_str() != Some(LIFECYCLE_RULE))
        .cloned()
        .collect();
    out.push(json!({
        "id": LIFECYCLE_RULE,
        "enabled": true,
        "conditions": { "prefix": "" },
        "abortMultipartUploadsTransition": {
            "condition": { "type": "Age", "maxAge": LIFECYCLE_MAX_AGE },
        },
    }));
    json!({ "rules": out })
}

fn bad(message: impl Into<String>) -> Error {
    Error::RustError(message.into())
}

pub async fn observe(account: &Account, instance: &Instance, bucket: &str) -> Result<Publisher> {
    let fingerprint = fingerprint(instance)
        .filter(|hash| pigeon_ingest::managed_destination::digest(hash))
        .ok_or_else(|| {
            bad("set the lowercase SHA-256 fingerprint of the multipart publisher credential")
        })?;
    if crate::data_store::inline(instance)
        || !matches!(&instance.account, AccountRef::Linked { account_id }
            if account_id.as_deref() == Some(account.id.as_str()))
        || bucket != crate::data_store::store_name(instance)
    {
        return Err(bad(
            "multipart publishing requires the instance's linked Data bucket",
        ));
    }
    let token = account
        .bearer
        .as_deref()
        .ok_or_else(|| bad("the instance's account must be linked"))?;
    let target = r2::Bucket {
        name: bucket.into(),
        jurisdiction: None,
    };
    r2::require_private_bucket(account, &target)
        .await
        .map_err(|_| bad("could not verify the instance's Data bucket is private"))?;
    let store = crate::secrets::linked_store(account)
        .await?
        .ok_or_else(|| {
            bad("set up the linked instance's Secrets Store for multipart publishing")
        })?;
    let slot = slot(instance, fingerprint);
    let listed = cloudflare::call(
        token,
        Method::Get,
        &format!(
            "/accounts/{}/secrets_store/stores/{store}/secrets?search={slot}",
            account.id
        ),
        None,
    )
    .await?;
    let mut matching = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter(|secret| secret["name"].as_str() == Some(&slot));
    let entry = matching.next().filter(|secret| {
        matching.next().is_none()
            && secret["status"].as_str() == Some("active")
            && matches!(secret["scopes"].as_array().map(Vec::as_slice),
                Some([scope]) if scope.as_str() == Some(pigeon_core::secrets_store::WORKERS))
    })
        .and_then(|secret| Some((secret["id"].as_str()?, secret["modified"].as_str()?)))
        .filter(|(id, modified)| !id.is_empty() && !modified.is_empty())
        .ok_or_else(|| bad(format!("store the bucket-scoped R2 publisher credential in linked Secrets Store slot {slot}")))?;
    Ok(Publisher {
        account: account.id.clone(),
        bucket: bucket.into(),
        store,
        slot,
        generation: oauth::hash(&json!(entry).to_string()),
        fingerprint: fingerprint.into(),
    })
}

/// What the plan reads beside the publisher: whether the Data bucket's lifecycle already aborts
/// incomplete multipart uploads ([`lifecycle_ok`]), and the earlier publisher slots
/// ([`stale_slots`]).
pub async fn observe_upkeep(
    account: &Account,
    instance: &Instance,
    publisher: &Publisher,
) -> Result<(bool, Vec<(String, String)>)> {
    let token = account
        .bearer
        .as_deref()
        .ok_or_else(|| bad("the instance's account must be linked"))?;
    let lifecycle = cloudflare::call(
        token,
        Method::Get,
        &format!(
            "/accounts/{}/r2/buckets/{}/lifecycle",
            account.id, publisher.bucket
        ),
        None,
    )
    .await?;
    let listed = cloudflare::call(
        token,
        Method::Get,
        &format!(
            "/accounts/{}/secrets_store/stores/{}/secrets?search={}&per_page=100",
            account.id,
            publisher.store,
            slot_prefix(&instance.id)
        ),
        None,
    )
    .await?;
    Ok((
        lifecycle_ok(&lifecycle["rules"]),
        stale_slots(&listed, &instance.id, &publisher.slot),
    ))
}

pub async fn verify(
    account: &Account,
    instance: &Instance,
    bucket: &str,
    approved: Option<&Publisher>,
) -> Result<()> {
    match approved {
        Some(expected) if observe(account, instance, bucket).await? == *expected => Ok(()),
        None if fingerprint(instance).is_none() => Ok(()),
        _ => Err(bad("the approved multipart publisher changed: plan again")),
    }
}

pub fn bind(metadata: &mut Value, publisher: &Publisher) -> Result<()> {
    let bindings = metadata["bindings"]
        .as_array_mut()
        .ok_or_else(|| bad("missing Data Worker bindings"))?;
    if bindings.iter().any(|binding| {
        matches!(
            binding["name"].as_str(),
            Some(
                "INGEST_R2_ACCOUNT"
                    | "INGEST_R2_BUCKET"
                    | "INGEST_R2_PUBLISH"
                    | "INGEST_R2_PUBLISH_SHA256"
            )
        )
    }) {
        return Err(bad("duplicate multipart publisher binding"));
    }
    bindings.extend([
        json!({ "type": "plain_text", "name": "INGEST_R2_ACCOUNT", "text": publisher.account }),
        json!({ "type": "plain_text", "name": "INGEST_R2_BUCKET", "text": publisher.bucket }),
        json!({ "type": "plain_text", "name": "INGEST_R2_PUBLISH_SHA256", "text": publisher.fingerprint }),
        json!({ "type": "secrets_store_secret", "name": "INGEST_R2_PUBLISH",
            "store_id": publisher.store, "secret_name": publisher.slot }),
    ]);
    Ok(())
}

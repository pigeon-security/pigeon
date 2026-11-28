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
    // Distinct even when two instances share a linked account.
    let instance_hash = pigeon_ingest::datastore::sha256_hex(instance.id.as_bytes());
    format!(
        "DATA_R2_PUBLISH_{}_{}",
        &instance_hash[..16],
        &fingerprint[..24]
    )
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

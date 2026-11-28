//! Enabling the managed destination's multipart publisher (docs/plans/multipart-enable.md):
//! the manual key's console write, and the canary check.
//!
//! The key a person pastes is checked against the instance's own Data bucket, written to its
//! linked Secrets Store slot and dropped: only its fingerprint is answered, for the record
//! change the console proposes next. Nothing here binds it; the approved Data plan does.

use pigeon_core::{r2, s3};
use pigeon_ingest::managed_destination as wire;
use serde_json::{Value, json};
use worker::{Env, Result};

use super::{bad, net, now_ms};
use crate::console::write::Write;
use crate::instances::{self, data_publisher, record::AccountRef, record::Instance};
use crate::{Context, data_store};

/// The canary check tool.
pub const CHECK: &str = "data_ingest_multipart_check";
/// The console write that checks and stores a manual R2 key.
pub const SET_KEY: &str = "data-multipart-key-set";

/// The approved instance a multipart write or check is for: linked, with a standalone Data
/// Worker.
async fn target(env: &Env, id: &str) -> Result<Instance> {
    let record = instances::one(env, id)
        .await?
        .ok_or_else(|| bad("There is no approved instance."))?;
    if !data_store::enabled(&record)
        || data_store::inline(&record)
        || !matches!(record.account, AccountRef::Linked { .. })
    {
        return Err(bad(
            "Large-file uploads need a linked instance whose Data store runs in its own Worker.",
        ));
    }
    Ok(record)
}

/// `data_ingest_multipart_check {instance}`: the Data Worker's canary, answered as it ran.
pub(super) async fn check(env: &Env, instance: &str) -> Result<Value> {
    let record = target(env, instance).await?;
    let (origin, key) = data_store::reach(env, &record).await?;
    if !wire::valid_origin(&origin) {
        return Err(bad("The Data Worker has no approved HTTPS origin."));
    }
    let reply = net::data_post(&origin, wire::CANARY_PATH, &key, now_ms(), &json!({}))
        .await
        .map_err(|_| {
            bad(
                "The Data Worker could not run the canary: bind the multipart publisher with \
                 an approved Data plan first (the Data Worker must be the current build).",
            )
        })?;
    let canary: Value = serde_json::from_str(&reply)?;
    let passed = canary["passed"].as_bool() == Some(true);
    Ok(json!({
        "instance": record.id,
        "passed": passed,
        "canary": canary,
        "note": if passed {
            "Large-file uploads are ready: ingest plans now write objects over 16 MiB."
        } else {
            "The canary failed: ingest keeps skipping objects over 16 MiB. See `checks`."
        },
    }))
}

pub(super) async fn check_write(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = target(cx.env, w.text()).await?;
    Ok(json!({ "instance": record.id, "bucket": data_store::store_name(&record) }))
}

/// Checks the pasted key (`ACCESS_KEY_ID:SECRET`) against the Data bucket only, writes its
/// credential to the instance's publisher slot, and answers the fingerprint to propose.
pub(super) async fn save(cx: &Context<'_>, host: &str, w: &Write<'_>) -> Result<Value> {
    let record = target(cx.env, w.text()).await?;
    let pair = super::arg(&w.change, "value")?;
    if pair.len() > 512 || pair.chars().any(char::is_control) {
        return Err(bad("Supply the key as ACCESS_KEY_ID:SECRET_ACCESS_KEY."));
    }
    let keys = s3::parse_keys(pair)?;
    let account =
        crate::linked::account_for_capability(cx.env, host, &record.id, "secrets").await?;
    if !matches!(&record.account, AccountRef::Linked { account_id }
        if account_id.as_deref() == Some(account.id.as_str()))
    {
        return Err(bad(
            "The linked account does not match the approved instance.",
        ));
    }
    let bucket = data_store::store_name(&record);
    r2::require_private_bucket(
        &account,
        &r2::Bucket {
            name: bucket.clone(),
            jurisdiction: None,
        },
    )
    .await
    .map_err(|_| bad("Could not verify the instance's Data bucket is private."))?;
    let store = crate::secrets::linked_store(&account)
        .await?
        .ok_or_else(|| {
            bad("Set up the linked instance's Secrets Store first (its plan creates one).")
        })?;
    data_publisher::verify_key(&account.id, &bucket, &keys).await?;
    let (text, fingerprint) = data_publisher::credential(&account.id, &bucket, &keys);
    let slot = data_publisher::slot_for(&record.id, &fingerprint);
    data_publisher::write_slot(&account, &store, &slot, &text).await?;
    Ok(json!({
        "instance": record.id,
        "bucket": bucket,
        "slot": slot,
        "fingerprint": fingerprint,
        "verified": true,
        "next": "Propose data.multipart_publisher set to this fingerprint; once approved, the \
                 Data plan binds it, then run data_ingest_multipart_check.",
    }))
}

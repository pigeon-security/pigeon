//! The `data` capability: an instance's unified data store, lite tier (docs/plans/email.md,
//! "Tiers"; docs/plans/data-worker.md). The record enables it with
//! `[capabilities.data] tier = "lite"`; an instance plan makes its bucket, D1 and data Worker
//! (`instances::data_plan`). Core reaches the data Worker only through signed contract ops at
//! `/data/call`, with the instance's `DATA_KEY`, after the checks of data-worker.md decision 3.
//! The home instance uses the same path, so there is one code path.

use pigeon_ingest::datastore::{self, CallRequest, DataCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Env, Error, Result};

use crate::instances::record::{AccountRef, CapabilitySettings, Instance};
use crate::users::directory;

/// The capability's name in the catalogue and a record's `capabilities`.
pub const NAME: &str = "data";

/// Where the store is backed up: a Backblaze B2 bucket of its own (ingest.md decision 10), by
/// its S3 endpoint and region. The key is a Secrets Store secret the person sets
/// ([`b2_slot`]), never in the record.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    /// Such as `pigeon-backup-pigeon-data`.
    pub bucket: String,
    /// Such as `https://s3.eu-central-003.backblazeb2.com`.
    pub endpoint: String,
    /// Such as `eu-central-003`.
    pub region: String,
}

/// The record's `data` settings, when it enables the store.
pub fn settings(record: &Instance) -> Option<&CapabilitySettings> {
    record.capabilities.get(NAME)
}

/// Whether the record enables the data store.
pub fn enabled(record: &Instance) -> bool {
    settings(record).is_some()
}

/// The tier the record names, `lite` unless set.
pub fn tier(record: &Instance) -> Option<&str> {
    settings(record).map(|s| s.tier.as_deref().unwrap_or(datastore::TIER_LITE))
}

/// The store's name: bucket, D1 database and data Worker (`{instance}-data`; `pigeon-data` on
/// home).
pub fn store_name(record: &Instance) -> String {
    datastore::store_name(&record.id, record.account == AccountRef::Home)
}

/// The Secrets Store secret holding the B2 key of the store's backup, `KEY_ID:APPLICATION_KEY`:
/// `DATA_B2_{STORE}`, such as `DATA_B2_PIGEON_DATA`.
pub fn b2_slot(record: &Instance) -> String {
    format!(
        "DATA_B2_{}",
        store_name(record).to_ascii_uppercase().replace('-', "_")
    )
}

/// The Directory keys of the store's signing key and Worker URL, as the ingest capability reads
/// them (`data_key:{instance}`, `data_url:{instance}`).
pub fn key_setting(instance: &str) -> String {
    format!("data_key:{instance}")
}
pub fn url_setting(instance: &str) -> String {
    format!("data_url:{instance}")
}

fn valid_bucket(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// What is wrong with capability `name`'s data-store fields on `record`: a tier anywhere but
/// `data`, or one not built; a bad backup.
pub fn field_problems(name: &str, s: &CapabilitySettings) -> Vec<String> {
    let mut out = Vec::new();
    if name != NAME {
        if s.tier.is_some() {
            out.push(format!("The capability \"{name}\" does not accept a tier."));
        }
        if s.backup.is_some() {
            out.push(format!(
                "The capability \"{name}\" does not accept a backup."
            ));
        }
        return out;
    }
    if let Some(t) = &s.tier
        && !datastore::TIERS.contains(&t.as_str())
    {
        out.push(format!(
            "The data store's tier \"{t}\" is not built yet: only \"lite\" is."
        ));
    }
    if let Some(b) = &s.backup {
        if !valid_bucket(&b.bucket) {
            out.push("The data store's backup bucket is not a B2 bucket name.".into());
        }
        let host = b.endpoint.strip_prefix("https://").unwrap_or_default();
        if host.is_empty()
            || host.contains('/')
            || !host.ends_with(".backblazeb2.com")
            || !host.contains(&format!(".{}.", b.region))
        {
            out.push(
                "The data store's backup endpoint is `https://s3.<region>.backblazeb2.com`, \
                 with the backup's region."
                    .into(),
            );
        }
    }
    out
}

/// The setup steps an instance without a ready store is told.
pub const SETUP: &str = "To set it up: add `[capabilities.data] tier = \"lite\"` (and \
`[capabilities.email]` for mail) to the instance's record with `instance_propose`, approve it \
with your passkey, then `instance_plan` and `instance_apply` (docs/plans/email.md).";

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The data Worker's URL and key for `record`, after data-worker.md decision 3's checks in
/// order: the record enables the store; the account is reachable (home always; a linked
/// account is not built for the lite tier); the Worker has been deployed (its key and URL are
/// in the Directory).
pub async fn reach(env: &Env, record: &Instance) -> Result<(String, String)> {
    let id = &record.id;
    if !enabled(record) {
        return Err(bad(format!(
            "the data store is not set up for {id}. {SETUP}"
        )));
    }
    if record.account != AccountRef::Home {
        return Err(bad(format!(
            "the data store of linked instance {id} is not built yet: the lite tier is the home \
             instance's"
        )));
    }
    let setting = |k: String| async move {
        directory(env, "setting.get", serde_json::json!({ "key": k }))
            .await
            .map(|v| v.as_str().map(str::to_string))
    };
    let key = setting(key_setting(id)).await?;
    let url = setting(url_setting(id)).await?;
    match (url, key) {
        (Some(url), Some(key)) => Ok((url, key)),
        _ => Err(bad(format!(
            "the data Worker of {id} has not been deployed: run instance_plan and \
             instance_apply for it"
        ))),
    }
}

/// Sends read op `call` to `record`'s data Worker, signed with its `DATA_KEY`, and answers its
/// JSON. Refuses before sending what the Worker would refuse.
pub async fn call(env: &Env, record: &Instance, call: DataCall) -> Result<Value> {
    if let Some(why) = datastore::call_problem(&call) {
        return Err(bad(why));
    }
    let (url, key) = reach(env, record).await?;
    let body = serde_json::to_value(CallRequest {
        instance: record.id.clone(),
        call,
    })?;
    let now = worker::js_sys::Date::now() as i64;
    let text = crate::ingest::net::data_post(&url, datastore::CALL_PATH, &key, now, &body).await?;
    serde_json::from_str(&text).map_err(|_| bad("the data Worker's answer was not JSON"))
}

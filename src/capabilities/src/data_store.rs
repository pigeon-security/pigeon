//! The `data` capability: an instance's unified data store, lite tier (docs/plans/email.md,
//! "Tiers"; docs/plans/data-worker.md). The record enables it with
//! `data = {tier = "lite"}`; an instance plan makes its bucket, D1 and data Worker
//! (`instances::data_plan`). Core reaches the data Worker only through signed contract ops at
//! `/data/call`, with the instance's `DATA_KEY`, after the checks of data-worker.md decision 3.
//! The home instance uses the same path, so there is one code path.

use pigeon_ingest::datastore::{self, CallRequest, DataCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Env, Error, Result};

use crate::catalogue::Placement;
use crate::instances::record::{AccountRef, CapabilitySettings, Instance};
use crate::users::directory;

/// The capability's name in the catalogue and a record's `capabilities`.
pub const NAME: &str = "data";
/// Historical, signed IMAP backfill runs in the instance's Data Worker.
pub const MAIL_IMPORT: &str = "data-ingest-email";

pub fn mail_import_enabled(record: &Instance) -> bool {
    enabled(record) && record.capabilities.contains_key(MAIL_IMPORT)
}

/// Optional linked OAuth grants used only when the Data backup is configured.
pub const BACKUP_SCOPES: &str = "data backup";

pub fn backup(record: &Instance) -> Option<&Backup> {
    settings(record)?.backup.as_ref()
}

/// The approved Jobs audit bucket, distinct from this Data store and its site buckets.
pub fn data_jobs_audit_bucket(record: &Instance) -> Option<&str> {
    let bucket = crate::jobs_audit::configured_bucket(record)?;
    (crate::jobs_audit::bucket_ok(bucket)
        && bucket != store_name(record)
        && !record.sites.iter().any(|site| site.bucket == bucket))
    .then_some(bucket)
}

/// Verify that a Data Worker may bind its approved same-account Jobs bucket. The caller supplies
/// the account that will run the Worker, using the instance's existing plan permissions.
pub async fn verify_data_jobs_audit<'a>(
    account: &pigeon_core::Account,
    record: &'a Instance,
) -> Result<&'a str> {
    let bucket = data_jobs_audit_bucket(record).ok_or_else(|| {
        Error::RustError("Configure a separate approved Jobs audit bucket first.".into())
    })?;
    if let AccountRef::Linked { account_id } = &record.account
        && account_id.as_deref() != Some(account.id.as_str())
    {
        return Err(Error::RustError(
            "The linked Jobs audit account does not match the instance.".into(),
        ));
    }
    let target = pigeon_core::r2::Bucket {
        name: bucket.to_string(),
        jurisdiction: None,
    };
    if !pigeon_core::r2::bucket_exists(account, &target).await?
        || pigeon_core::r2::require_private_bucket(account, &target)
            .await
            .is_err()
    {
        return Err(Error::RustError(
            "Could not verify the approved Jobs audit bucket is private.".into(),
        ));
    }
    Ok(bucket)
}

/// The Jobs audit bucket used by a linked backup. Existing home backups keep their behavior.
pub fn backup_audit_bucket(record: &Instance) -> Option<&str> {
    if record.account == AccountRef::Home || backup(record).is_none() {
        return None;
    }
    data_jobs_audit_bucket(record)
}

/// Mail audits use the same approved Jobs destination as other linked Data work.
pub fn mail_import_audit_bucket(record: &Instance) -> Option<&str> {
    if record.account == AccountRef::Home || !mail_import_enabled(record) {
        return None;
    }
    crate::jobs_audit::configured_bucket(record)
}

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

/// The approved external storage location the data store's read tools also read
/// (`data.read`), when the record enables the store and names one.
pub fn read(record: &Instance) -> Option<&crate::data_read::scope::Profile> {
    settings(record).and_then(|s| s.read.as_ref())
}

/// Whether the record enables the data store.
pub fn enabled(record: &Instance) -> bool {
    settings(record).is_some()
}

/// Whether capability `name` is planned by the data plan (`instances::data_plan`), never by
/// the capability Workers' (M5) steps: Data, Data Email and historical IMAP backfill.
pub fn planned_apart(name: &str) -> bool {
    matches!(name, NAME | MAIL_IMPORT) || name == crate::email::NAME
}

/// Where the data store runs for `record` (docs/plans/linked-data.md): on home in its own
/// Worker, `pigeon-data`, always; on a linked instance what its record says, `own_worker`
/// unless set.
pub fn placement(record: &Instance) -> Placement {
    if record.account == AccountRef::Home {
        return Placement::OwnWorker;
    }
    settings(record)
        .and_then(|s| s.placement)
        .unwrap_or(Placement::OwnWorker)
}

/// Whether the store runs inline, in the instance's own Worker (a linked instance only).
pub fn inline(record: &Instance) -> bool {
    placement(record) == Placement::Inline
}

/// The Worker that runs the store, receives the zone's mail and answers `/data/call`: the
/// store's own (`{instance}-data`, `pigeon-data` on home), or inline the instance's Worker
/// (`None` when the record names none).
pub fn worker(record: &Instance) -> Option<String> {
    match placement(record) {
        Placement::OwnWorker => Some(store_name(record)),
        Placement::Inline => record.worker.clone(),
    }
}

/// What is wrong with where `record` places the store and its mail: inline without an instance
/// Worker; `email` placed anywhere but where `data` runs; linked backups without Jobs audit.
pub fn placement_problems(record: &Instance) -> Vec<String> {
    let mut out = Vec::new();
    let Some(s) = settings(record) else {
        return out;
    };
    if inline(record) && record.worker.is_none() {
        out.push(
            "The data store runs inline in the instance's own Worker, and the record names \
             none: choose own_worker, or give the instance a worker."
                .into(),
        );
    }
    if let Some(fingerprint) = s.multipart_publisher.as_deref() {
        if !pigeon_ingest::managed_destination::digest(fingerprint) {
            out.push("Data multipart publishing requires the credential's lowercase SHA-256 fingerprint.".into());
        }
        if record.account == AccountRef::Home || inline(record) {
            out.push(
                "Multipart publishing requires a linked Data Worker on its own Worker.".into(),
            );
        }
    }
    if let Some(e) = crate::email::settings(record)
        && let Some(p) = e.placement
        && p != placement(record)
    {
        out.push(format!(
            "Email runs where the data store runs ({}): leave its placement out, or make it \
             the same.",
            placement(record).as_str()
        ));
    }
    if let Some(import) = record.capabilities.get(MAIL_IMPORT)
        && let Some(p) = import.placement
        && p != placement(record)
    {
        out.push(
            "Historical mail import runs where Data runs. Leave its placement unset or match Data."
                .into(),
        );
    }
    if record.account != AccountRef::Home
        && s.backup.is_some()
        && backup_audit_bucket(record).is_none()
    {
        out.push("A linked Data backup requires a separate approved Jobs audit bucket.".into());
    }
    out
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
        if s.read.is_some() {
            out.push(format!(
                "The capability \"{name}\" does not accept read: it is the data store's (data.read)."
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
pub const SETUP: &str = "To set it up: add `\"data\": {\"tier\": \"lite\"}` (and \
`\"email\": {}` inside it for mail) to the instance's record with `instance_propose`, approve it \
with your passkey, then `instance_plan` and `instance_apply` (docs/plans/email.md).";

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The data Worker's URL and key for `record`, after data-worker.md decision 3's checks in
/// order: the record enables the store; the Worker has been deployed (its key and URL are in
/// the Directory). Either placement (docs/plans/linked-data.md) keeps the same two settings:
/// the URL is the workers.dev address of whichever Worker runs the store.
pub async fn reach(env: &Env, record: &Instance) -> Result<(String, String)> {
    let id = &record.id;
    if !enabled(record) {
        return Err(bad(format!(
            "the data store is not set up for {id}. {SETUP}"
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

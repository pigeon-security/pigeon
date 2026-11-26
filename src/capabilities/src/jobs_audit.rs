//! Redacted, terminal runner diagnostics for an approved instance's private R2 bucket.

use pigeon_core::{r2, sign::hex};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use worker::{Env, Error, Result};

use crate::instances::{
    self,
    record::{AccountRef, Instance},
};

const MAX_EVENTS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    #[default]
    NotConfigured,
    Pending,
    Retrying,
    Stored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    account: String,
    bucket: String,
}

pub(crate) fn bucket_ok(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// A stable per-instance default; path-style and long IDs include a collision-resistant suffix.
pub fn default_bucket(instance: &str) -> String {
    const SUFFIX: &str = "-jobs-audit";
    let simple = format!("{instance}{SUFFIX}");
    if bucket_ok(&simple) {
        return simple;
    }
    let slug: String = instance
        .bytes()
        .take(30)
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c as char
            } else {
                '-'
            }
        })
        .collect();
    let digest = hex(&Sha256::digest(instance.as_bytes()));
    format!("jobs-{}-{}{SUFFIX}", slug.trim_matches('-'), &digest[..16])
}

pub(crate) fn configured_bucket(record: &Instance) -> Option<&str> {
    record.capabilities.get("jobs")?.audit_bucket.as_deref()
}

fn setup_target(record: &Instance) -> Result<(Target, r2::Bucket)> {
    let AccountRef::Linked {
        account_id: Some(account),
    } = &record.account
    else {
        return Err(setup_error(
            "Link this instance to its Cloudflare account before configuring Jobs.",
        ));
    };
    let name = configured_bucket(record)
        .map(str::to_owned)
        .unwrap_or_else(|| default_bucket(&record.id));
    if !bucket_ok(&name) || record.sites.iter().any(|site| site.bucket == name) {
        return Err(setup_error(
            "Jobs needs a valid private audit bucket dedicated to this instance’s job records.",
        ));
    }
    Ok((
        Target {
            account: account.clone(),
            bucket: name.clone(),
        },
        r2::Bucket {
            name,
            jurisdiction: None,
        },
    ))
}

fn setup_error(message: &str) -> Error {
    Error::RustError(message.into())
}

async fn open_target(
    env: &Env,
    host: &str,
    record: &Instance,
) -> Result<(pigeon_core::Account, r2::Bucket)> {
    let (target, bucket) = setup_target(record)?;
    let account = crate::linked::account_for_bound(env, host, &record.id, &target.account)
        .await
        .map_err(|_| {
            setup_error(
                "Could not open this instance’s Cloudflare link. Check the link in Overview.",
            )
        })?;
    Ok((account, bucket))
}

/// Read-only inspection behind the console's instance operator check.
pub(crate) async fn setup_plan(env: &Env, host: &str, record: &Instance) -> Result<Value> {
    let (account, bucket) = open_target(env, host, record).await?;
    let exists = r2::bucket_exists(&account, &bucket)
        .await
        .map_err(|_| setup_error("Could not check the Jobs audit bucket. Check that the Cloudflare link grants R2 access."))?;
    if exists {
        require_private(&account, &bucket).await?;
    }
    let link = crate::linked::link(env, &record.id)
        .await
        .ok()
        .flatten()
        .filter(|link| link.account_id == account.id)
        .ok_or_else(|| {
            setup_error("The instance’s Cloudflare link changed. Review setup again.")
        })?;
    Ok(json!({
        "bucket": bucket.name,
        "account_name": link.account_name,
        "action": if exists { "reuse" } else { "create" },
    }))
}

async fn require_private(account: &pigeon_core::Account, bucket: &r2::Bucket) -> Result<()> {
    r2::require_private_bucket(account, bucket).await.map_err(|_| {
        setup_error("Could not verify that the Jobs audit bucket is private. Check the managed public URL, custom domains, and R2 access.")
    })
}

/// Make only the reviewed action. A name taken between preview and creation needs a fresh review.
pub(crate) async fn setup_apply(
    env: &Env,
    host: &str,
    record: &Instance,
    action: &str,
) -> Result<()> {
    let (account, bucket) = open_target(env, host, record).await?;
    if action == "create" {
        let made = r2::ensure_bucket(&account, &bucket)
            .await
            .map_err(|_| setup_error("Could not create the private Jobs audit bucket. Review setup again to check whether the bucket now exists."))?;
        if !made.created {
            return Err(setup_error(
                "The audit bucket now exists. Return to setup and review reusing it.",
            ));
        }
    }
    let verified = verify_writable(&account, &bucket).await;
    if action == "create" {
        return verified.map_err(|_| setup_error(
            "The audit bucket was created, but its privacy or write access could not be verified. Check its public access and R2 permissions, then review setup again.",
        ));
    }
    verified
}

async fn verify_writable(account: &pigeon_core::Account, bucket: &r2::Bucket) -> Result<()> {
    require_private(account, bucket).await?;
    let probe = format!("audit/v1/_setup/{}", pigeon_core::webauthn::random()?);
    r2::put(account, bucket, &probe, "application/json", b"{}".to_vec())
        .await
        .map_err(|_| setup_error("Could not verify write access to the Jobs audit bucket. Check that the Cloudflare link grants R2 write access, then review setup again."))?;
    r2::delete(account, bucket, &probe)
        .await
        .map_err(|_| setup_error("Could not remove the empty write probe in the Jobs audit bucket. Check R2 permissions and review setup again."))?;
    Ok(())
}

fn target(record: &Instance) -> Option<Target> {
    let AccountRef::Linked {
        account_id: Some(account),
    } = &record.account
    else {
        return None;
    };
    let bucket = configured_bucket(record)?;
    if !bucket_ok(bucket) {
        return None;
    }
    Some(Target {
        account: account.clone(),
        bucket: bucket.to_owned(),
    })
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    DropletCreate,
    CloudInit,
    PackageInstall,
    ImageBuild,
    VersionCheck,
    Cleanup,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Requested,
    Started,
    Succeeded,
    Failed,
    Uncertain,
    Retrying,
    Cancelled,
    TimedOut,
    Confirmed,
}

#[derive(Deserialize, Serialize)]
struct Event {
    at_ms: u64,
    stage: Stage,
    outcome: Outcome,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TerminalState {
    Gone,
    Rejected,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Health {
    Pending,
    Started,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Deserialize, Serialize)]
struct Job {
    id: String,
    state: TerminalState,
    health: Health,
    started_ms: u64,
    deadline_ms: u64,
    finished_ms: Option<u64>,
    compute_estimate: Option<ComputeEstimate>,
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Deserialize, Serialize)]
struct ComputeEstimate {
    estimated_usd_micros: u64,
    usd_micros_per_hour: u64,
    duration_ms: u64,
    deletion_confirmed: bool,
}

#[derive(Serialize)]
struct Audit<'a> {
    schema_version: u8,
    instance_id: &'a str,
    job_type: &'static str,
    run_id: &'a str,
    provider: &'static str,
    job: &'a Job,
}

pub struct Object {
    pub key: String,
    pub json: Vec<u8>,
}

/// Build a content-addressed diagnostic object from a terminal public snapshot.
/// Only fields declared in the schema above survive serialization.
pub fn diagnostic_object(instance: &str, job: &Value) -> Option<Object> {
    if !(1..=64).contains(&instance.len())
        || instance.matches('/').count() > 1
        || !instance.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
        || job.get("events").is_some_and(|events| {
            events
                .as_array()
                .is_none_or(|events| events.len() > MAX_EVENTS)
        })
    {
        return None;
    }
    let mut job: Job = serde_json::from_value(job.clone()).ok()?;
    if job.id.len() != 32
        || !job
            .id
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
        || job.deadline_ms <= job.started_ms
        || job
            .finished_ms
            .is_some_and(|finished| finished < job.started_ms)
        || job.events.iter().any(|event| event.at_ms < job.started_ms)
    {
        return None;
    }
    const JOB_TYPE: &str = "digitalocean-runner-check";
    let instance_segment = instance.replace('/', "%2F");
    let prefix = format!(
        "audit/v1/instances/{instance_segment}/jobs/{JOB_TYPE}/{}/terminal",
        job.id
    );
    job.events.sort_by_key(|event| event.at_ms);
    let body = serde_json::to_string(&Audit {
        schema_version: 1,
        instance_id: instance,
        job_type: JOB_TYPE,
        run_id: &job.id,
        provider: "digitalocean",
        job: &job,
    })
    .ok()?;
    let digest = hex(&Sha256::digest(body.as_bytes()));
    Some(Object {
        key: format!("{prefix}/{digest}.json"),
        json: body.into_bytes(),
    })
}

/// Attempt a copy to the approved linked-instance bucket. Failures remain pending for retry;
/// callers persist the health outcome independently. Never routes through a home credential.
pub async fn export(env: &Env, host: &str, instance: &str, public_job: &Value) -> ExportStatus {
    let approved = match current_target(env, instance).await {
        Ok(Some(target)) => target,
        Ok(None) => return ExportStatus::NotConfigured,
        Err(_) => return ExportStatus::Retrying,
    };
    let Some(object) = diagnostic_object(instance, public_job) else {
        return ExportStatus::Retrying;
    };
    let result = async {
        let linked =
            crate::linked::account_for_bound(env, host, instance, &approved.account).await?;
        let bucket = r2::Bucket {
            name: approved.bucket.clone(),
            jurisdiction: None,
        };
        r2::require_private_bucket(&linked, &bucket).await?;
        if current_target(env, instance).await?.as_ref() != Some(&approved) {
            return Err(worker::Error::RustError(
                "approved audit destination changed".into(),
            ));
        }
        r2::put(
            &linked,
            &bucket,
            &object.key,
            "application/json",
            object.json,
        )
        .await
    }
    .await;
    if result.is_ok() {
        ExportStatus::Stored
    } else {
        ExportStatus::Retrying
    }
}

async fn current_target(env: &Env, instance: &str) -> Result<Option<Target>> {
    Ok(instances::one(env, instance)
        .await?
        .as_ref()
        .and_then(target))
}

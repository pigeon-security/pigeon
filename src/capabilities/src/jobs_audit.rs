//! Redacted, terminal runner diagnostics for an approved instance's private R2 bucket.

use pigeon_core::{oauth, r2};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Env, Result};

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

fn target(record: &Instance) -> Option<Target> {
    let AccountRef::Linked {
        account_id: Some(account),
    } = &record.account
    else {
        return None;
    };
    let bucket = record.capabilities.get("jobs")?.audit_bucket.as_ref()?;
    if !bucket_ok(bucket) {
        return None;
    }
    Some(Target {
        account: account.clone(),
        bucket: bucket.clone(),
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
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Serialize)]
struct Audit<'a> {
    version: u8,
    instance: &'a str,
    job: Job,
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
    let job: Job = serde_json::from_value(job.clone()).ok()?;
    if job.id.len() != 32
        || !job
            .id
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
        || job.deadline_ms <= job.started_ms
        || job.events.iter().any(|event| event.at_ms < job.started_ms)
    {
        return None;
    }
    let prefix = format!("jobs/{instance}/digitalocean-droplet/{}", job.id);
    let body = serde_json::to_string(&Audit {
        version: 1,
        instance,
        job,
    })
    .ok()?;
    let digest = oauth::hash(&body);
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
        Err(_) => return ExportStatus::Pending,
    };
    let Some(object) = diagnostic_object(instance, public_job) else {
        return ExportStatus::Pending;
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
        ExportStatus::Pending
    }
}

async fn current_target(env: &Env, instance: &str) -> Result<Option<Target>> {
    Ok(instances::one(env, instance)
        .await?
        .as_ref()
        .and_then(target))
}

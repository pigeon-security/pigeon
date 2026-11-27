//! Portable, redacted terminal records for Data pipeline Jobs.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PipelineKind {
    DigitaloceanIngest,
    ScalewayIngest,
    BackblazeIngest,
    BackblazeBackup,
    EmailIngest,
}

impl PipelineKind {
    fn name(self) -> &'static str {
        match self {
            Self::DigitaloceanIngest => "digitalocean-ingest",
            Self::ScalewayIngest => "scaleway-ingest",
            Self::BackblazeIngest => "backblaze-ingest",
            Self::BackblazeBackup => "backblaze-backup",
            Self::EmailIngest => "email-ingest",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineOutcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    Confirmed,
    Unconfirmed,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineCounts {
    pub examined: u64,
    pub stored: u64,
    pub duplicates: u64,
    pub failed: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

/// Contains no provider credentials, object keys, mailbox names, or arbitrary error text.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PipelineTerminal {
    pub run_id: String,
    pub started_ms: u64,
    pub finished_ms: u64,
    pub outcome: PipelineOutcome,
    pub cleanup: Option<Cleanup>,
    pub counts: PipelineCounts,
    pub estimated_usd_micros: Option<u64>,
}

pub struct AuditObject {
    pub key: String,
    pub json: Vec<u8>,
}

fn path_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

pub fn terminal_object(
    instance: &str,
    kind: PipelineKind,
    terminal: &PipelineTerminal,
) -> Option<AuditObject> {
    if instance.len() > 64
        || instance.matches('/').count() > 1
        || !instance.split('/').all(path_component)
        || terminal.run_id.len() > 96
        || !path_component(&terminal.run_id)
        || terminal.finished_ms < terminal.started_ms
    {
        return None;
    }

    #[derive(Serialize)]
    struct Record<'a> {
        schema_version: u8,
        instance_id: &'a str,
        job_type: &'static str,
        run_id: &'a str,
        job: &'a PipelineTerminal,
    }

    let job_type = kind.name();
    let json = serde_json::to_vec(&Record {
        schema_version: 1,
        instance_id: instance,
        job_type,
        run_id: &terminal.run_id,
        job: terminal,
    })
    .ok()?;
    let digest = Sha256::digest(&json);
    let key = format!(
        "audit/v1/instances/{}/jobs/{job_type}/{}/terminal/{digest:x}.json",
        instance.replace('/', "%2F"),
        terminal.run_id
    );
    Some(AuditObject { key, json })
}

//! Signed Data Worker operations and the credential-free status of a Cloudflare ingest runner.

use serde::{Deserialize, Serialize};

use crate::manifest::Counters;
use crate::protocol::StartRequest;

pub const TERMINAL_PATH: &str = "/internal/data-ingest/backblaze-terminal";
pub const READY_PATH: &str = "/ingest/ready";
pub const STATUS_PATH: &str = "/ingest/status";
pub const MAX_RUNTIME_MS: i64 = 24 * 3_600_000;
/// Less than the standard-4 container's disk so the binary, filesystem and runtime have room.
pub const SPOOL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Reviewed runner image and the Git revision used to build that image.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub image: String,
    pub revision: String,
}

impl Runtime {
    pub fn valid_for(&self, account: &str) -> bool {
        valid_image(&self.image, account) && valid_revision(&self.revision)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Start {
    #[serde(flatten)]
    pub request: StartRequest,
    /// The exact revision approved by the operator and configured on the Worker deployment.
    /// Absent on historical requests to a manually deployed Data Worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub report_to_jobs: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Terminal {
    pub run: String,
    pub started_ms: i64,
    pub finished_ms: i64,
    pub outcome: Outcome,
    pub counters: Counters,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalNotice {
    pub instance: String,
    pub terminal: Terminal,
}

pub fn valid_run(run: &str) -> bool {
    (1..=96).contains(&run.len())
        && run
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn valid_revision(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}

pub fn valid_deadline(started_ms: i64, deadline_ms: i64) -> bool {
    deadline_ms > started_ms && deadline_ms.saturating_sub(started_ms) <= MAX_RUNTIME_MS
}

/// The runner image must be content-addressed in the approved account's registry.
pub fn valid_image(image: &str, account: &str) -> bool {
    if account.len() != 32 || !account.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    let prefix = format!("registry.cloudflare.com/{account}/pigeon-ingest-runner@sha256:");
    image.strip_prefix(&prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    })
}

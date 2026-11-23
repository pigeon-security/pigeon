//! What a run is told and what it writes back (decision 13.3): the job, one manifest line per
//! file or member, the checkpoint, the status and the report, and their R2 keys under
//! `ingest/runs/{run}/`.

use serde::{Deserialize, Serialize};

use crate::archive::{Flag, Limits};
use crate::decide::{Reason, Verdict};
use crate::metrics::Kind;

/// Lines per manifest part at most.
pub const PART_LINES: usize = 5_000;
/// Bytes per manifest part at most.
pub const PART_BYTES: usize = 8 * 1024 * 1024;

/// Where a run reads from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceSpec {
    /// An S3-compatible bucket.
    S3 {
        /// The storage account id, such as `digitalocean:legacy`.
        account: String,
        /// Such as `https://fra1.digitaloceanspaces.com`, no trailing `/`.
        endpoint: String,
        region: String,
        bucket: String,
        /// Only keys under this prefix; empty for all.
        prefix: String,
    },
    /// A folder on the machine the runner runs on.
    Local {
        /// The account id to record, such as `local:laptop`.
        account: String,
        /// An absolute path.
        root: String,
    },
}

/// Where a run writes: the instance's R2 bucket, through R2's S3 API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DestSpec {
    /// `https://{account_id}.r2.cloudflarestorage.com` (or the jurisdiction's endpoint).
    pub endpoint: String,
    pub bucket: String,
}

/// Everything a runner needs except its keys. Written to `job.json`; never holds a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub run: String,
    pub instance: String,
    pub source: SourceSpec,
    pub dest: DestSpec,
    /// Decide and record only; write nothing under `blobs/` or `quarantine/`.
    pub dry_run: bool,
    /// Keep a zip's own bytes as well as its members (default true).
    pub keep_archives: bool,
    /// Compute SimHash for text-like files.
    pub fingerprints: bool,
    pub limits: Limits,
    /// Worker threads.
    pub threads: u32,
    /// The repository commit the runner was built from.
    pub commit: String,
}

/// Where a file or member came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub account: String,
    pub bucket: String,
    pub key: String,
    pub etag: Option<String>,
    /// The listing's size of the top-level object.
    pub listed_size: u64,
    pub modified: Option<String>,
}

/// Where a member sits in its archives, outermost first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The SHA-256 of the archive directly holding it.
    pub parent_sha256: String,
    /// Its normalised path in that archive.
    pub path: String,
    /// Its raw name, escaped.
    pub raw_name: String,
    /// Its index in that archive's directory.
    pub index: u64,
    /// 1 for a member of a top-level archive.
    pub depth: u32,
    /// The indexes from the top-level archive down to this one, for the line's id.
    pub chain: Vec<u64>,
}

/// A weaker metric's match, recorded and never decisive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hint {
    /// `same_name_size` or `near_text`.
    pub kind: String,
    /// The other file's SHA-256.
    pub other: String,
    /// Bits apart for `near_text`; none for `same_name_size`.
    pub score: Option<f64>,
}

/// One manifest line: one file or member and what was decided.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestLine {
    /// Stable: see [`line_id`].
    pub id: String,
    pub origin: Origin,
    pub member: Option<Member>,
    /// Empty when the bytes were not read (a skipped member).
    pub sha256: String,
    pub blake3: String,
    pub size: u64,
    pub mime: String,
    pub kind: Kind,
    pub norm_name: String,
    pub simhash: Option<String>,
    pub verdict: Verdict,
    pub reason: Reason,
    pub flags: Vec<Flag>,
    pub hints: Vec<Hint>,
    /// The key written or found, if any.
    pub r2_key: Option<String>,
    pub error: Option<String>,
    pub at_ms: i64,
}

/// Counters of a run, in the status, checkpoint and report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub listed: u64,
    pub listed_bytes: u64,
    pub files: u64,
    pub members: u64,
    pub bytes_read: u64,
    pub uploaded: u64,
    pub uploaded_bytes: u64,
    pub would_upload: u64,
    pub would_upload_bytes: u64,
    pub duplicates: u64,
    pub duplicate_bytes: u64,
    pub conflicts: u64,
    pub failed: u64,
    pub skipped: u64,
    pub archives_opened: u64,
    pub archives_not_opened: u64,
}

/// Where a run can resume from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The next manifest part's number.
    pub seq: u64,
    /// Every listing key at or below this one is finished.
    pub after_key: Option<String>,
    pub counters: Counters,
}

/// A run's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Starting,
    Listing,
    Running,
    Done,
    Failed,
    Stopped,
}

/// `status.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub run: String,
    pub state: RunState,
    pub dry_run: bool,
    pub counters: Counters,
    pub started_ms: i64,
    pub updated_ms: i64,
    /// Bytes read per second over the run.
    pub read_rate: f64,
    /// Seconds left, when the listing is complete.
    pub eta_s: Option<u64>,
    pub last_error: Option<String>,
}

/// `report.json`, written at the end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub run: String,
    pub state: RunState,
    pub dry_run: bool,
    pub counters: Counters,
    /// Count per flag, by its snake-case name.
    pub flags: Vec<(Flag, u64)>,
    /// At most 200 each.
    pub conflicts: Vec<ManifestLine>,
    pub failures: Vec<ManifestLine>,
    pub not_opened: Vec<ManifestLine>,
    /// The SHA-256 values seen most often in this run, with their count and size, at most 50.
    pub top_duplicates: Vec<(String, u64, u64)>,
    pub started_ms: i64,
    pub finished_ms: i64,
}

/// `ingest/runs/{run}/`.
pub fn run_prefix(run: &str) -> String {
    format!("ingest/runs/{run}/")
}

/// The key of `job.json`.
pub fn job_key(run: &str) -> String {
    format!("{}job.json", run_prefix(run))
}

/// The key of manifest part `seq`.
pub fn part_key(run: &str, seq: u64) -> String {
    format!("{}manifest/{seq:08}.jsonl", run_prefix(run))
}

/// The key of `checkpoint.json`.
pub fn checkpoint_key(run: &str) -> String {
    format!("{}checkpoint.json", run_prefix(run))
}

/// The key of `status.json`.
pub fn status_key(run: &str) -> String {
    format!("{}status.json", run_prefix(run))
}

/// The key of `report.json`.
pub fn report_key(run: &str) -> String {
    format!("{}report.json", run_prefix(run))
}

/// The key a conflicting file is copied aside to.
pub fn quarantine_key(run: &str, sha256: &str) -> String {
    format!("quarantine/{run}/{sha256}")
}

/// A line's stable id: the SHA-256 hex of account, bucket, key and the member chain, so a
/// line written twice is one sighting.
pub fn line_id(origin: &Origin, chain: &[u64]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in [&origin.account, &origin.bucket, &origin.key] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    for i in chain {
        h.update(i.to_be_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A run id: `{yyyymmdd}-{provider}-{name}-{bucket}-{6 hex}`, lowercased, at most 96
/// characters of `a-z0-9-`. `account` is such as `digitalocean:legacy`; `day` is `yyyymmdd`;
/// `nonce` gives the hex.
pub fn run_id(day: &str, account: &str, bucket: &str, nonce: u32) -> String {
    let raw = format!(
        "{day}-{}-{bucket}-{:06x}",
        account.replace(':', "-"),
        nonce & 0xff_ffff
    );
    let mut s: String = raw
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.truncate(96);
    s
}

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
    /// The approved run time: the runner stops itself, writing its checkpoint, shortly before
    /// it. None for older jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_minutes: Option<u64>,
    /// The stopped run this one continues, and its checkpoint, read and approved at plan time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<Resume>,
    /// Objects larger than this many bytes are not read: skipped with `too_large` and listed in
    /// the report (a live run whose destination has no multipart publisher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_over: Option<u64>,
    /// This run is one shard of a sharded run: it takes only the keys of its shard
    /// (docs/plans/ingest-bytes.md). None for an unsharded run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shard: Option<Shard>,
    /// Only objects larger than this many bytes are taken; the rest are passed over unlisted
    /// (docs/plans/multipart-enable.md, "Picking up skipped files"). A job with it carries a
    /// [`fenced`] `resume`, so a runner that predates this field resumes after every key and
    /// does nothing, rather than copy the whole prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_over: Option<u64>,
}

/// One shard of a sharded run: the keys whose [`crate::shard::shard_of`] is `index` of `count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shard {
    pub index: u32,
    pub count: u32,
    /// The approved run this shard belongs to.
    pub parent: String,
}

/// The `after_key` above every real key (byte order): a runner resuming after it lists nothing.
pub const FENCE: &str = "\u{10FFFF}\u{10FFFF}\u{10FFFF}\u{10FFFF}";

/// The resume of an [`JobSpec::only_over`] run picking up `run`'s skipped objects: fenced, so
/// only a runner that reads `only_over` (and so starts from the beginning) does any work.
pub fn fenced(run: &str) -> Resume {
    Resume {
        run: run.to_string(),
        checkpoint: Checkpoint {
            after_key: Some(FENCE.into()),
            ..Checkpoint::default()
        },
    }
}

/// Whether `resume` is a [`fenced`] one.
pub fn is_fenced(resume: &Resume) -> bool {
    resume.checkpoint.after_key.as_deref() == Some(FENCE)
}

/// Where a continuation starts: another run's checkpoint (docs/plans/ingest-throughput.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    /// The run continued.
    pub run: String,
    /// Its checkpoint when the continuation was planned.
    pub checkpoint: Checkpoint,
}

impl JobSpec {
    /// When the runner started at `started_ms` should stop itself: five minutes or a tenth of
    /// the approved time before its end, whichever is less. None with no approved time.
    pub fn stop_at_ms(&self, started_ms: i64) -> Option<i64> {
        let total = i64::try_from(self.max_minutes?)
            .ok()?
            .saturating_mul(60_000);
        let margin = (total / 10).min(5 * 60_000);
        Some(started_ms.saturating_add(total - margin))
    }
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
    /// Where this session's time went (docs/plans/ingest-bytes.md); absent in older runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<crate::timing::Timings>,
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
    /// Objects listed and bytes read a second in this session, for later estimates.
    #[serde(default)]
    pub files_per_s: f64,
    #[serde(default)]
    pub bytes_per_s: f64,
    /// The run this one continues, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    /// Objects skipped as larger than the run takes, at most 200.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub too_large: Vec<ManifestLine>,
    /// The run stopped because its write budget was spent; continue it with a new grant.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub budget_spent: bool,
    /// Where this session's time went (docs/plans/ingest-bytes.md); absent in older runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<crate::timing::Timings>,
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
    if s.len() > 96 {
        // Retain the nonce, since long source names can otherwise collide on the same day.
        s.truncate(89);
        s.push_str(&format!("-{:06x}", nonce & 0xff_ffff));
    }
    s
}

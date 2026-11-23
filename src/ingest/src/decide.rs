//! The decision for one file: left behind only when R2 certainly holds the same bytes
//! (decision 13.1), otherwise copied.

use serde::{Deserialize, Serialize};

use crate::metrics::Metrics;

/// What a `HEAD` on the blob's key answered, when the object exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    /// Its `content-length`.
    pub size: u64,
    /// Its `x-amz-meta-sha256`, if any.
    pub sha256: Option<String>,
    /// Its `x-amz-meta-blake3`, if any.
    pub blake3: Option<String>,
}

/// What is known about the blob's key before deciding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// This run already stored or confirmed this SHA-256.
    SeenInRun,
    /// R2 has no object at the key.
    Absent,
    /// R2 has an object at the key.
    Present(Head),
}

/// What happens to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Copied to its blob key.
    Upload,
    /// Would be copied, in a dry run.
    WouldUpload,
    /// Left behind: R2 certainly holds the same bytes.
    Duplicate,
    /// R2 holds something else at the key: copied aside to `quarantine/`, reported.
    Conflict,
    /// Reading or hashing failed after retries.
    Failed,
    /// Not read, with a flag saying why (an encrypted member, a limit).
    Skipped,
}

/// Why, in one word, for the manifest and the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Rule 1: seen earlier in this run.
    SeenInRun,
    /// Rule 2: equal size and SHA-256 (and BLAKE3 when both have it) on the R2 object.
    InR2,
    /// Rule 3: the R2 object differs in size or a hash.
    Mismatch,
    /// Rule 4: the R2 object carries no SHA-256 to compare.
    Unattested,
    /// Rule 5: nothing at the key.
    New,
    /// The read failed.
    Error,
    /// Not opened, for a flag's reason.
    Flagged,
}

/// One decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub verdict: Verdict,
    pub reason: Reason,
}

/// Decides for a file with `metrics` given `lookup` (rules 1 to 5 of decision 13.1):
///
/// 1. `SeenInRun` is `Duplicate` (`SeenInRun`).
/// 2. `Present` with equal size, equal SHA-256 metadata (hex, any case) and, when both sides
///    have a BLAKE3, equal BLAKE3 is `Duplicate` (`InR2`).
/// 3. `Present` with any other difference is `Conflict` (`Mismatch`).
/// 4. `Present` with no SHA-256 metadata is `Upload` (`Unattested`).
/// 5. `Absent` is `Upload` (`New`).
///
/// Nothing else ever produces `Duplicate`. In a dry run every `Upload` is `WouldUpload` (the
/// reason is kept) and a `Conflict` stays `Conflict` (nothing is written).
pub fn decide(metrics: &Metrics, lookup: &Lookup, dry_run: bool) -> Decision {
    let (verdict, reason) = match lookup {
        Lookup::SeenInRun => (Verdict::Duplicate, Reason::SeenInRun),
        Lookup::Absent => (Verdict::Upload, Reason::New),
        Lookup::Present(head) => match &head.sha256 {
            None => (Verdict::Upload, Reason::Unattested),
            Some(sha) => {
                let blake_ok = match &head.blake3 {
                    Some(b) => b.eq_ignore_ascii_case(&metrics.blake3),
                    None => true,
                };
                if head.size == metrics.size
                    && sha.eq_ignore_ascii_case(&metrics.sha256)
                    && blake_ok
                {
                    (Verdict::Duplicate, Reason::InR2)
                } else {
                    (Verdict::Conflict, Reason::Mismatch)
                }
            }
        },
    };
    let verdict = if dry_run && verdict == Verdict::Upload {
        Verdict::WouldUpload
    } else {
        verdict
    };
    Decision { verdict, reason }
}

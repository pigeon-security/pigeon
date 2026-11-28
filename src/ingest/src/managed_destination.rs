//! Run-scoped authority for the Data Worker's managed ingest destination.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decide::Head;
use crate::{blob_key, container_job::valid_run, manifest};

pub const PATH: &str = "/ingest/destination";
pub const READY_PATH: &str = "/ingest/destination/ready";
/// Core's signed request for the Data Worker's multipart canary (docs/plans/multipart-enable.md).
pub const CANARY_PATH: &str = "/ingest/destination/multipart-canary";
pub const ENV: &str = "INGEST_DESTINATION";
pub const OPERATION: &str = "x-pigeon-ingest-operation";
pub const OBJECT: &str = "x-pigeon-ingest-object";
pub const SHA256: &str = "x-pigeon-ingest-sha256";
pub const BLAKE3: &str = "x-pigeon-ingest-blake3";
pub const MAX_BLOB: u64 = 16 * 1024 * 1024;
/// The default most source bytes a live run may declare: 10 GB. The person raises it for a run
/// with the plan's `max_live_bytes`, shown in the approval they sign.
pub const LIVE_CAP_BYTES: u64 = 10_000_000_000;
/// The highest `max_live_bytes`: 100 TB.
pub const LIVE_CAP_MAX: u64 = 100_000_000_000_000;

/// What a live managed run may do, from [`live_guard`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Live {
    /// Objects above this many bytes are skipped and listed (`job.skip_over`).
    pub skip_over: Option<u64>,
    /// Plan lines for the person.
    pub notes: Vec<String>,
}

/// The one guard for a managed run on every host (Backblaze, Scaleway, DigitalOcean): a dry run
/// is always allowed; a live run may cover many listing pages when the whole source is declared
/// (`size_bytes`, so the grant is sized from it) and stays within `cap` bytes. Objects over
/// [`MAX_BLOB`] go through the multipart publisher when the Data Worker reports it configured,
/// else they are skipped and listed in the report rather than refusing the run.
pub fn live_guard(
    dry_run: bool,
    truncated: bool,
    declared: Option<u64>,
    sampled_bytes: u64,
    largest: u64,
    multipart: bool,
    cap: u64,
) -> Result<Live, String> {
    if dry_run {
        return Ok(Live {
            skip_over: None,
            notes: Vec::new(),
        });
    }
    if truncated && declared.is_none() {
        return Err("A live run beyond one listing page needs size_bytes (the whole prefix's size, from the provider's console), so its write grant covers it.".into());
    }
    let total = declared.unwrap_or(sampled_bytes).max(sampled_bytes);
    if total > cap {
        return Err(format!(
            "This live run declares {total} bytes, over its live cap of {cap} bytes. Choose a smaller prefix, or pass max_live_bytes (at most {LIVE_CAP_MAX}) to raise the cap; the person approves it with the plan."
        ));
    }
    let mut notes = vec![format!(
        "Live run of {total} declared bytes within a cap of {cap} bytes; a spent write grant stops the run cleanly for a continuation."
    )];
    let skip_over = if multipart {
        notes.push("Objects over 16 MiB go through the Data Worker's multipart publisher.".into());
        Some(crate::immutable_multipart::MAX_BYTES)
    } else {
        notes.push(format!(
            "Objects over 16 MiB are skipped and listed in the report (too_large) until the Data Worker's multipart publisher is configured{}.",
            if largest > MAX_BLOB {
                "; the sample already has some"
            } else {
                ""
            }
        ));
        Some(MAX_BLOB)
    };
    Ok(Live { skip_over, notes })
}

/// Most payload keys in one `head-many` request.
pub const HEAD_MANY: usize = 500;
/// The largest `head-many` request and reply bodies.
pub const HEAD_MANY_BODY: u64 = 64 * 1024;
pub const HEAD_MANY_REPLY: u64 = 256 * 1024;

/// A `head-many` request: payload keys of the run's grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadMany {
    pub keys: Vec<String>,
}

/// A `head-many` reply: for each key in order, the blob's identity or none. A hint only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HeadManyReply {
    pub heads: Vec<Option<Head>>,
}

/// The SHA-256 a `head-many` key may be answered for from the D1 index: a blob key's own hash.
/// A quarantine key never is (the index says nothing about `quarantine/`).
pub fn indexable_sha(key: &str) -> Option<&str> {
    let sha = key.rsplit('/').next()?;
    (digest(sha) && key == blob_key(sha)).then_some(sha)
}

/// The identities the index rows `rows` (`sha256, size, blake3`, from
/// [`crate::index::known_blobs`]) give, by SHA-256: what R2's HEAD of the blob answers, since a
/// row is made only from a live run's proof and the blob's metadata carries the same hashes.
pub fn known_heads(rows: &[serde_json::Value]) -> std::collections::HashMap<String, Head> {
    rows.iter()
        .filter_map(|row| {
            let sha = row["sha256"].as_str()?;
            let size = row["size"]
                .as_u64()
                .or_else(|| row["size"].as_f64().map(|f| f as u64))?;
            let blake3 = row["blake3"].as_str().filter(|b| !b.is_empty());
            Some((
                sha.to_string(),
                Head {
                    size,
                    sha256: Some(sha.to_string()),
                    blake3: blake3.map(str::to_string),
                },
            ))
        })
        .collect()
}

/// The keys of `keys` the index does not know (in order): the ones R2 must still HEAD.
pub fn index_misses(
    keys: &[String],
    known: &std::collections::HashMap<String, Head>,
) -> Vec<String> {
    keys.iter()
        .filter(|key| !indexable_sha(key).is_some_and(|sha| known.contains_key(sha)))
        .cloned()
        .collect()
}

/// The `head-many` answers, in `keys` order: the index's for the keys it knows, R2's (`misses`,
/// the HEADs of [`index_misses`] in order) for the rest. None when `misses` is the wrong length.
pub fn merge_heads(
    keys: &[String],
    known: &std::collections::HashMap<String, Head>,
    misses: Vec<Option<Head>>,
) -> Option<Vec<Option<Head>>> {
    let mut misses = misses.into_iter();
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        match indexable_sha(key).and_then(|sha| known.get(sha)) {
            Some(head) => out.push(Some(head.clone())),
            None => out.push(misses.next()?),
        }
    }
    misses.next().is_none().then_some(out)
}

/// Whether a `head-many` request is within a run's grant: 1 to [`HEAD_MANY`] distinct payload
/// keys.
pub fn head_many_keys(run: &str, request: &HeadMany) -> bool {
    let mut seen = std::collections::HashSet::new();
    (1..=HEAD_MANY).contains(&request.keys.len())
        && request
            .keys
            .iter()
            .all(|key| payload_read_key(run, key) && seen.insert(key.as_str()))
}
/// A manifest is flushed at 8 MiB and can contain one final record past that threshold.
pub const MAX_CONTROL: u64 = 10 * 1024 * 1024;
pub const MAX_GRANT_MS: i64 = 25 * 3_600_000;
pub const CHARGE_SQL: &str =
    "INSERT INTO ingest_grants(run,expires,bytes,writes) VALUES (?1,?2,?3,1)
 ON CONFLICT(run) DO UPDATE SET bytes=bytes+?3,writes=writes+1
 WHERE expires=?2 AND bytes<=?4-?3 AND writes<?5 RETURNING writes";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub origin: String,
    pub grant: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub instance: String,
    pub run: String,
    pub issued_ms: i64,
    pub expires_ms: i64,
    pub dry_run: bool,
    pub max_bytes: u64,
    pub max_objects: u64,
}

impl Grant {
    fn valid(&self, now_ms: i64) -> bool {
        !self.instance.is_empty()
            && self.instance.len() <= 160
            && valid_run(&self.run)
            && self.issued_ms <= now_ms
            && now_ms < self.expires_ms
            && self.expires_ms.saturating_sub(self.issued_ms) <= MAX_GRANT_MS
            && (1..=9_007_199_254_740_991).contains(&self.max_bytes)
            && (1..=9_007_199_254_740_991).contains(&self.max_objects)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unhex(value: &str) -> Option<Vec<u8>> {
    if !value.is_ascii() || !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect()
}

fn mac(key: &[u8], body: &str) -> Option<HmacSha256> {
    if key.is_empty() {
        return None;
    }
    let mut mac = HmacSha256::new_from_slice(key).ok()?;
    mac.update(b"pigeon-data-ingest-destination-v1\n");
    mac.update(body.as_bytes());
    Some(mac)
}

pub fn issue(grant: &Grant, key: &[u8], now_ms: i64) -> Option<String> {
    if !grant.valid(now_ms) {
        return None;
    }
    let body = hex(&serde_json::to_vec(grant).ok()?);
    let signature = hex(&mac(key, &body)?.finalize().into_bytes());
    Some(format!("{body}.{signature}"))
}

pub fn verify(token: &str, key: &[u8], instance: &str, now_ms: i64) -> Option<Grant> {
    if token.len() > 2048 {
        return None;
    }
    let (body, signature) = token.split_once('.')?;
    mac(key, body)?.verify_slice(&unhex(signature)?).ok()?;
    let grant: Grant = serde_json::from_slice(&unhex(body)?).ok()?;
    (grant.instance == instance && grant.valid(now_ms)).then_some(grant)
}

pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn payload_key(run: &str, key: &str, sha: &str) -> bool {
    digest(sha) && (key == blob_key(sha) || key == manifest::quarantine_key(run, sha))
}

pub fn payload_read_key(run: &str, key: &str) -> bool {
    key.rsplit('/')
        .next()
        .is_some_and(|sha| payload_key(run, key, sha))
}

/// Returns whether an authorized control object is immutable. The approved job description
/// is written by Core and is outside the runner's authority.
pub fn control_key(run: &str, key: &str) -> Option<bool> {
    if key == manifest::checkpoint_key(run) || key == manifest::status_key(run) {
        return Some(false);
    }
    if key == manifest::report_key(run) {
        return Some(true);
    }
    let seq = key
        .strip_prefix(&format!("{}manifest/", manifest::run_prefix(run)))?
        .strip_suffix(".jsonl")?
        .parse::<u64>()
        .ok()?;
    (key == manifest::part_key(run, seq)).then_some(true)
}

/// Long enough for the reviewed wall clock bound and a terminal write. Reserve payload bytes
/// for archive expansion and a separate bounded allowance for control records and retries.
pub fn run_budget(source_bytes: u64, source_objects: u64, dry_run: bool) -> (u64, u64) {
    let writes = source_objects.saturating_mul(64).clamp(4_096, 20_000_000);
    let control = source_bytes / 10 + MAX_CONTROL * 8;
    let bytes = if dry_run {
        control
    } else {
        source_bytes.saturating_mul(8).saturating_add(control)
    };
    (bytes.min(9_007_199_254_740_991), writes)
}

pub fn hashes(body: &[u8]) -> (String, String) {
    (
        format!("{:x}", Sha256::digest(body)),
        blake3::hash(body).to_hex().to_string(),
    )
}

/// A Worker origin without a port, path, credentials or query.
pub fn valid_origin(origin: &str) -> bool {
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    host.len() <= 253
        && host.split('.').count() >= 2
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

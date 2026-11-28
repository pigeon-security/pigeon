//! Run-scoped authority for the Data Worker's managed ingest destination.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{blob_key, container_job::valid_run, manifest};

pub const PATH: &str = "/ingest/destination";
pub const READY_PATH: &str = "/ingest/destination/ready";
pub const ENV: &str = "INGEST_DESTINATION";
pub const OPERATION: &str = "x-pigeon-ingest-operation";
pub const OBJECT: &str = "x-pigeon-ingest-object";
pub const SHA256: &str = "x-pigeon-ingest-sha256";
pub const BLAKE3: &str = "x-pigeon-ingest-blake3";
pub const MAX_BLOB: u64 = 16 * 1024 * 1024;
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

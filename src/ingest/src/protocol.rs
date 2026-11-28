//! How core asks an instance's data Worker to start an ingest run as a Cloudflare Container
//! (decision 13.6): the request body and the message both sides sign with the instance's
//! `DATA_KEY` (HMAC-SHA256, computed by the callers with `pigeon_core::sign::hmac_sha256`).

use serde::{Deserialize, Serialize};

use hmac::{Hmac, Mac};
use serde_json::Value;

use crate::Error;
use crate::manifest::JobSpec;

/// The path of the start request on the data Worker.
pub const START_PATH: &str = "/ingest/start";
/// The path of the stop request on the data Worker.
pub const STOP_PATH: &str = "/ingest/stop";
/// The header carrying `t=<unix ms>,v1=<hex HMAC>`.
pub const SIGNATURE_HEADER: &str = "x-pigeon-signature";
/// The most a signature's time may differ from the receiver's clock, in milliseconds.
pub const MAX_SKEW_MS: i64 = 60_000;

/// The body of a start request. The keys are passed to the container's environment and kept
/// nowhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub job: JobSpec,
    /// The source account's read key, `ACCESS_KEY_ID:SECRET`.
    pub source_keys: String,
    /// The instance bucket's R2 key, `ACCESS_KEY_ID:SECRET`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dest_keys: String,
}

/// The body of a stop request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopRequest {
    pub run: String,
}

/// The message signed for a request: method, path, time and the body's SHA-256 hex, one per
/// line.
pub fn signing_message(method: &str, path: &str, at_ms: i64, body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    signing_digest(method, path, at_ms, &digest)
}

fn signing_digest(method: &str, path: &str, at_ms: i64, digest: &str) -> String {
    format!("{method}\n{path}\n{at_ms}\n{digest}")
}

/// Decodes lowercase or uppercase hex; none if it is odd or has another character.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Checks a request's `t=<unix ms>,v1=<hex HMAC-SHA256>` header against `key`: the time must be
/// within [`MAX_SKEW_MS`] of `now_ms` and the MAC must match [`signing_message`] (compared in
/// constant time). Answers why not, in words that name no secret.
pub fn verify(
    header: &str,
    method: &str,
    path: &str,
    body: &[u8],
    key: &[u8],
    now_ms: i64,
) -> Result<(), Error> {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(body));
    verify_digest(header, method, path, &digest, key, now_ms)
}

/// Verifies a signed, claimed SHA-256 before reading a potentially large body. The caller must
/// still compare the streamed body's hash with this digest before acting on it.
pub fn verify_digest(
    header: &str,
    method: &str,
    path: &str,
    digest: &str,
    key: &[u8],
    now_ms: i64,
) -> Result<(), Error> {
    let bad = |why: &str| Error::Config(format!("signature: {why}"));
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad("invalid body digest"));
    }
    let (mut at, mut mac_hex) = (None, None);
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => at = v.parse::<i64>().ok(),
            Some(("v1", v)) => mac_hex = Some(v),
            _ => {}
        }
    }
    let at = at.ok_or_else(|| bad("malformed header"))?;
    let tag = mac_hex
        .and_then(unhex)
        .ok_or_else(|| bad("malformed header"))?;
    if at.abs_diff(now_ms) > MAX_SKEW_MS.unsigned_abs() {
        return Err(bad("stale"));
    }
    let message = signing_digest(method, path, at, digest);
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).map_err(|_| bad("bad key"))?;
    mac.update(message.as_bytes());
    mac.verify_slice(&tag).map_err(|_| bad("does not match"))
}

/// What an R2 event notification says about one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectEvent {
    /// The object's key.
    pub key: String,
    /// Whether the key is a manifest part, `ingest/runs/{run}/manifest/{seq:08}.jsonl`.
    pub is_manifest_part: bool,
}

/// Whether `key` is `ingest/runs/{run}/manifest/{seq:08}.jsonl` (eight digits).
pub fn is_manifest_part(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("ingest/runs/") else {
        return false;
    };
    let mut it = rest.split('/');
    let (Some(run), Some("manifest"), Some(file), None) =
        (it.next(), it.next(), it.next(), it.next())
    else {
        return false;
    };
    let Some(seq) = file.strip_suffix(".jsonl") else {
        return false;
    };
    !run.is_empty() && seq.len() == 8 && seq.bytes().all(|b| b.is_ascii_digit())
}

/// Reads an R2 event-notification message body (`{"action":…,"object":{"key":…}}`). None when
/// it is not one, or the action deletes the object.
pub fn parse_event(body: &Value) -> Option<ObjectEvent> {
    let key = body.get("object")?.get("key")?.as_str()?;
    if body
        .get("action")
        .and_then(Value::as_str)
        .is_some_and(|a| a.starts_with("Delete") || a.starts_with("LifecycleDeletion"))
    {
        return None;
    }
    Some(ObjectEvent {
        key: key.to_string(),
        is_manifest_part: is_manifest_part(key),
    })
}

/// The container's environment for a start request: `INGEST_JOB` (the job as JSON),
/// `SOURCE_KEYS`, `DEST_KEYS` and `SPOOL_DIR`, the names the ingest runner reads.
pub fn start_env(req: &StartRequest) -> Result<Vec<(String, String)>, Error> {
    let job = serde_json::to_string(&req.job).map_err(|e| Error::Config(e.to_string()))?;
    Ok(vec![
        ("INGEST_JOB".to_string(), job),
        ("SOURCE_KEYS".to_string(), req.source_keys.clone()),
        ("DEST_KEYS".to_string(), req.dest_keys.clone()),
        ("SPOOL_DIR".to_string(), "/spool".to_string()),
    ])
}

//! The signed caller context: who core says is calling, tied to one op and one body, good for
//! a minute. Service bindings are already private; the signature is the second wall, so a route
//! added to a capability Worker by mistake still serves nobody.

use std::fmt;

use pigeon_core::sign::{equal, hex, hmac_sha256};
use pigeon_core::webauthn::{b64url, unb64url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The header that carries the signed caller.
pub const HEADER: &str = "X-Pigeon-Caller";

/// How long a signed caller stays good, in milliseconds.
pub const MAX_AGE_MS: i64 = 60_000;

/// How far ahead of this clock a signature may be dated, in milliseconds.
const SKEW_MS: i64 = 5_000;

/// The caller context core signs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signed {
    /// Who is calling, as core serializes `users::Caller`; null for none.
    #[serde(default)]
    pub caller: Value,
    /// When it was signed, in milliseconds since the epoch.
    pub iat: i64,
    /// When it stops being good, in milliseconds since the epoch.
    pub exp: i64,
    /// A value that differs for every signature.
    pub nonce: String,
}

/// Why a caller was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// There was no caller header.
    Missing,
    /// The header could not be read.
    Malformed,
    /// The signature does not match the op and body.
    BadSignature,
    /// The signature is older than [`MAX_AGE_MS`].
    Expired,
    /// The signature is dated later than this clock allows.
    FromTheFuture,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::Missing => "the caller is missing",
            Refused::Malformed => "the caller is malformed",
            Refused::BadSignature => "the caller's signature is bad",
            Refused::Expired => "the caller's signature has expired",
            Refused::FromTheFuture => "the caller's signature is dated in the future",
        })
    }
}

impl std::error::Error for Refused {}

/// The header value for `caller` calling `op` with `body`: base64url(JSON of [`Signed`]), a
/// dot, and base64url(HMAC-SHA256 over op, newline, hex(sha256(body)), newline, the first
/// part). It expires [`MAX_AGE_MS`] after `now_ms`.
pub fn sign(key: &[u8], caller: &Value, op: &str, body: &[u8], now_ms: i64, nonce: &str) -> String {
    let signed = Signed {
        caller: caller.clone(),
        iat: now_ms,
        exp: now_ms.saturating_add(MAX_AGE_MS),
        nonce: nonce.to_string(),
    };
    let json = serde_json::to_vec(&signed).unwrap_or_default();
    let first = b64url(&json);
    let tag = mac(key, op, body, &first);
    format!("{first}.{}", b64url(&tag))
}

/// The caller in `header`, when its signature over `op` and `body` is good and `now_ms` is
/// within five seconds before its `iat` and its `exp`.
pub fn verify(
    key: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
) -> Result<Signed, Refused> {
    let header = header.ok_or(Refused::Missing)?;
    let (first, tag) = header.split_once('.').ok_or(Refused::Malformed)?;
    let tag = unb64url(tag).map_err(|_| Refused::Malformed)?;
    if !equal(&tag, &mac(key, op, body, first)) {
        return Err(Refused::BadSignature);
    }
    let json = unb64url(first).map_err(|_| Refused::Malformed)?;
    let signed: Signed = serde_json::from_slice(&json).map_err(|_| Refused::Malformed)?;
    if now_ms < signed.iat.saturating_sub(SKEW_MS) {
        return Err(Refused::FromTheFuture);
    }
    if now_ms > signed.exp || signed.exp.saturating_sub(signed.iat) > MAX_AGE_MS {
        return Err(Refused::Expired);
    }
    Ok(signed)
}

/// The HMAC over the op, the body's digest and the encoded first part.
fn mac(key: &[u8], op: &str, body: &[u8], first: &str) -> [u8; 32] {
    let digest = Sha256::digest(body);
    let message = format!("{op}\n{}\n{first}", hex(&digest));
    hmac_sha256(key, message.as_bytes())
}

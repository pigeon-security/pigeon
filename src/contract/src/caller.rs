//! The signed caller context: who core says is calling, tied to one op and one body, good for
//! a minute. Service bindings are already private; the signature is the second wall, so a route
//! added to a capability Worker by mistake still serves nobody.
//!
//! The optional Worker context binds recipient and instance inside the signed payload.
//! Older envelopes remain valid during the receiver-first rollout.

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

/// The version of the optional signed Worker context.
pub const WORKER_CONTEXT_VERSION: u32 = 1;

/// A distinct header format makes a contextual request fail closed on an older receiver.
pub(crate) const CONTEXT_PREFIX: &str = "wc1.";

/// An authenticated destination and optional approved instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerContext {
    /// The context format, independent of the enclosing wire contract.
    pub version: u32,
    /// The receiver's deployed Worker name.
    pub recipient: String,
    /// The instance this call is bound to, when core selected an instance profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
}

impl WorkerContext {
    pub fn new(recipient: &str, instance: Option<&str>) -> Self {
        Self {
            version: WORKER_CONTEXT_VERSION,
            recipient: recipient.into(),
            instance: instance.map(str::to_owned),
        }
    }
}

fn valid_instance(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.split('/').count() <= 2
        && id.split('/').all(|part| {
            (1..=63).contains(&part.len())
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

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
    /// The Worker context, absent in legacy envelopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<WorkerContext>,
}

impl Signed {
    /// The signed scope's instance, when present.
    pub fn instance(&self) -> Option<&str> {
        self.context.as_ref().and_then(|c| c.instance.as_deref())
    }

    /// Compare arguments with a supplied signed instance. Legacy calls remain compatible.
    pub fn check_instance(&self, instance: Option<&str>) -> Result<(), Refused> {
        if let Some(expected) = self.instance()
            && instance != Some(expected)
        {
            return Err(Refused::InstanceMismatch);
        }
        Ok(())
    }

    /// For a receiver that requires an authenticated instance before doing privileged work.
    pub fn require_instance(&self, instance: &str) -> Result<(), Refused> {
        if self.instance() == Some(instance) {
            Ok(())
        } else {
            Err(Refused::InstanceMismatch)
        }
    }
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
    /// The signed context is invalid or uses an unknown version.
    BadContext,
    /// The signed recipient differs from the receiver.
    WrongRecipient,
    /// The request arguments differ from the signed instance or omit it.
    InstanceMismatch,
}

impl Refused {
    /// A short machine-readable reason, safe to log and to send back: it names the failure and
    /// never carries the header or any part of it.
    pub fn reason(&self) -> &'static str {
        match self {
            Refused::Missing => "missing_authorization",
            Refused::Malformed => "malformed_authorization",
            Refused::BadSignature => "token_invalid",
            Refused::Expired => "token_expired",
            Refused::FromTheFuture => "token_not_yet_valid",
            Refused::BadContext => "worker_context_invalid",
            Refused::WrongRecipient => "worker_recipient_mismatch",
            Refused::InstanceMismatch => "worker_instance_mismatch",
        }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::Missing => "the caller is missing",
            Refused::Malformed => "the caller is malformed",
            Refused::BadSignature => "the caller's signature is bad",
            Refused::Expired => "the caller's signature has expired",
            Refused::FromTheFuture => "the caller's signature is dated in the future",
            Refused::BadContext => "the signed Worker context is invalid",
            Refused::WrongRecipient => "the signed recipient does not match this Worker",
            Refused::InstanceMismatch => "the request does not match the signed instance",
        })
    }
}

impl std::error::Error for Refused {}

/// The header value for `caller` calling `op` with `body`: base64url(JSON of [`Signed`]), a
/// dot, and base64url(HMAC-SHA256 over op, newline, hex(sha256(body)), newline, the first
/// part). It expires [`MAX_AGE_MS`] after `now_ms`.
pub fn sign(key: &[u8], caller: &Value, op: &str, body: &[u8], now_ms: i64, nonce: &str) -> String {
    sign_inner(key, caller, op, body, now_ms, nonce, None)
}

/// Signs a caller together with the destination and optional approved instance.
pub fn sign_with_context(
    key: &[u8],
    caller: &Value,
    op: &str,
    body: &[u8],
    now_ms: i64,
    nonce: &str,
    context: &WorkerContext,
) -> String {
    sign_inner(key, caller, op, body, now_ms, nonce, Some(context))
}

fn sign_inner(
    key: &[u8],
    caller: &Value,
    op: &str,
    body: &[u8],
    now_ms: i64,
    nonce: &str,
    context: Option<&WorkerContext>,
) -> String {
    let signed = Signed {
        caller: caller.clone(),
        iat: now_ms,
        exp: now_ms.saturating_add(MAX_AGE_MS),
        nonce: nonce.to_string(),
        context: context.cloned(),
    };
    let json = serde_json::to_vec(&signed).unwrap_or_default();
    let first = b64url(&json);
    let tag = mac(key, op, body, &first, context.is_some());
    let prefix = if context.is_some() {
        CONTEXT_PREFIX
    } else {
        ""
    };
    format!("{prefix}{first}.{}", b64url(&tag))
}

/// Verify a legacy caller envelope. Use [`verify_for`] when the receiver knows its name.
/// Both verify the signature over `op` and `body` and enforce the envelope time window.
pub fn verify(
    key: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
) -> Result<Signed, Refused> {
    let signed = verify_inner(key, header, op, body, now_ms)?;
    if signed.context.is_some() {
        // A receiver that cannot check the audience must not accept contextual calls.
        return Err(Refused::WrongRecipient);
    }
    Ok(signed)
}

/// Verify a legacy or contextual envelope for the receiver's own deployed Worker name.
pub fn verify_for(
    key: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
    recipient: &str,
) -> Result<Signed, Refused> {
    let signed = verify_inner(key, header, op, body, now_ms)?;
    if let Some(context) = &signed.context {
        if context.version != WORKER_CONTEXT_VERSION
            || context.recipient.is_empty()
            || context
                .instance
                .as_deref()
                .is_some_and(|id| !valid_instance(id))
            || (context.instance.is_some() && signed.caller.is_null())
        {
            return Err(Refused::BadContext);
        }
        if recipient.is_empty() || context.recipient != recipient {
            return Err(Refused::WrongRecipient);
        }
    }
    Ok(signed)
}

fn verify_inner(
    key: &[u8],
    header: Option<&str>,
    op: &str,
    body: &[u8],
    now_ms: i64,
) -> Result<Signed, Refused> {
    let header = header.ok_or(Refused::Missing)?;
    let contextual = header.starts_with(CONTEXT_PREFIX);
    let header = header.strip_prefix(CONTEXT_PREFIX).unwrap_or(header);
    let (first, tag) = header.split_once('.').ok_or(Refused::Malformed)?;
    let tag = unb64url(tag).map_err(|_| Refused::Malformed)?;
    if !equal(&tag, &mac(key, op, body, first, contextual)) {
        return Err(Refused::BadSignature);
    }
    let json = unb64url(first).map_err(|_| Refused::Malformed)?;
    let signed: Signed = serde_json::from_slice(&json).map_err(|_| Refused::Malformed)?;
    if contextual != signed.context.is_some() {
        return Err(Refused::BadContext);
    }
    if now_ms < signed.iat.saturating_sub(SKEW_MS) {
        return Err(Refused::FromTheFuture);
    }
    if now_ms > signed.exp || signed.exp.saturating_sub(signed.iat) > MAX_AGE_MS {
        return Err(Refused::Expired);
    }
    Ok(signed)
}

/// The HMAC over the op, the body's digest and the encoded first part.
fn mac(key: &[u8], op: &str, body: &[u8], first: &str, contextual: bool) -> [u8; 32] {
    let domain = if contextual {
        "pigeon-worker-context-v1\n"
    } else {
        ""
    };
    let digest = Sha256::digest(body);
    let message = format!("{domain}{op}\n{}\n{first}", hex(&digest));
    hmac_sha256(key, message.as_bytes())
}

//! What the proxy Worker keeps between requests, per isolate (docs/plans/workers-split.md,
//! phase 6): the scoped credentials core minted for it, reused until a minute before they
//! expire, and the proxy credentials core verified, for a minute each, keyed by a hash of the
//! credential, never the credential itself. A revoked proxy credential is therefore refused
//! within a minute. CI job grants and deploy grants are never kept: each request asks core.
//!
//! Pure: every method takes the time, in milliseconds since the epoch, so the smoke suite can
//! check expiry and keying without a Workers runtime.

use std::collections::HashMap;

use pigeon_contract::Credential;
use pigeon_core::oauth::hash;

use crate::{ProxyCaller, Purpose};

/// How long a verified proxy credential is kept, in milliseconds: how long a revocation can
/// take to reach the proxy Worker.
pub const VERIFIED_FOR_MS: i64 = 60_000;

/// How long before its expiry a scoped credential stops being reused, in milliseconds, so a
/// request never starts with a token about to expire.
pub const RENEW_BEFORE_MS: i64 = 60_000;

/// How many verified credentials are kept at most; the oldest goes first.
pub const VERIFIED_MAX: usize = 256;

/// Proxy credentials core verified, each kept [`VERIFIED_FOR_MS`]. Only a live credential is
/// kept: an unknown one is asked again each time, so a new credential works at once.
#[derive(Default)]
pub struct Verified {
    /// By the credential's hash: the holder and when it stops being kept.
    kept: HashMap<String, (ProxyCaller, i64)>,
}

impl Verified {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The key `presented` is kept under: its hash, as the Directory keeps it.
    pub fn key(presented: &str) -> String {
        hash(presented)
    }

    /// The holder of `presented`, when it was verified less than [`VERIFIED_FOR_MS`] before
    /// `now_ms`.
    pub fn get(&self, presented: &str, now_ms: i64) -> Option<ProxyCaller> {
        self.kept
            .get(&Self::key(presented))
            .filter(|(_, until)| now_ms < *until)
            .map(|(caller, _)| caller.clone())
    }

    /// Keeps `caller` as the holder of `presented`, verified at `now_ms`. Expired entries go
    /// first; when it is still full, the one that expires soonest.
    pub fn put(&mut self, presented: &str, caller: ProxyCaller, now_ms: i64) {
        self.kept.retain(|_, (_, until)| now_ms < *until);
        if self.kept.len() >= VERIFIED_MAX
            && let Some(oldest) = self
                .kept
                .iter()
                .min_by_key(|(_, (_, until))| *until)
                .map(|(k, _)| k.clone())
        {
            self.kept.remove(&oldest);
        }
        self.kept.insert(
            Self::key(presented),
            (caller, now_ms.saturating_add(VERIFIED_FOR_MS)),
        );
    }

    /// How many credentials are kept, expired ones included until the next [`Verified::put`].
    pub fn len(&self) -> usize {
        self.kept.len()
    }

    /// Whether none is kept.
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }
}

/// The scoped credentials core minted for this Worker, one per [`Purpose`].
#[derive(Default)]
pub struct Credentials {
    kept: HashMap<Purpose, Credential>,
}

impl Credentials {
    /// None kept.
    pub fn new() -> Self {
        Self::default()
    }

    /// The credential for `purpose`, when it has more than [`RENEW_BEFORE_MS`] left at
    /// `now_ms`.
    pub fn get(&self, purpose: Purpose, now_ms: i64) -> Option<&Credential> {
        self.kept
            .get(&purpose)
            .filter(|c| now_ms < c.expires_at.saturating_sub(RENEW_BEFORE_MS))
    }

    /// Keeps `credential` for `purpose`, replacing the one before.
    pub fn put(&mut self, purpose: Purpose, credential: Credential) {
        self.kept.insert(purpose, credential);
    }

    /// Forgets the credential for `purpose` when it is still `token`: Cloudflare refused it
    /// (core revoked it before it expired, as its token ledger does with the leases it held
    /// when it restarted), so the next request asks core for another. One another request
    /// already replaced is left alone.
    pub fn forget(&mut self, purpose: Purpose, token: &str) {
        if self.kept.get(&purpose).is_some_and(|c| c.token == token) {
            self.kept.remove(&purpose);
        }
    }
}

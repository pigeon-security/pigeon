//! A capability Worker's caller keys (docs/plans/caller-keys.md, step 3): its own key, bound as
//! [`OWN_KEY`], and the shared one, bound as [`SHARED_KEY`] until step 4 drops it.
//!
//! A Worker checks core's signature with its own key first, then the shared key, so core can
//! switch to signing with the Worker's own key without a refused call in between. It signs its
//! own requests to core with [`own_key`]. A Worker whose own key is missing serves and signs
//! nothing: there is no fall-back to the shared key alone, nor to unsigned.

use pigeon_core::secrets_store;
use worker::{Env, Error, Result};

use crate::caller::{CONTEXT_PREFIX, Refused, Signed, verify, verify_for};

/// The Secrets Store binding of a capability Worker's own caller key. The same binding name in
/// every Worker; each binds its own secret (`CALLER_KEY_<WORKER>`) under it.
pub const OWN_KEY: &str = "CALLER_KEY_OWN";

/// The Secrets Store binding of the shared caller key, accepted until step 4.
pub const SHARED_KEY: &str = "CALLER_KEY";

/// The keys a capability Worker checks core's signatures with. No `Debug`: it holds secrets.
#[derive(Clone)]
pub struct Keys {
    /// This Worker's own key, tried first.
    pub own: Vec<u8>,
    /// The shared key, tried only when the own key gives a bad signature; `None` once it is
    /// no longer bound.
    pub shared: Option<Vec<u8>>,
}

impl Keys {
    /// The caller in `header`, for this Worker (`recipient`, its deployed name): checked with
    /// the own key, then the shared key. Only a bad signature with the own key falls back; a
    /// good one that fails the age, skew or context checks is refused for that reason. A
    /// contextual envelope is checked with [`verify_for`], a legacy one with [`verify`]. Pure.
    pub fn verify(
        &self,
        header: Option<&str>,
        op: &str,
        body: &[u8],
        now_ms: i64,
        recipient: &str,
    ) -> std::result::Result<Signed, Refused> {
        let check = |key: &[u8]| {
            if header.is_some_and(|h| h.starts_with(CONTEXT_PREFIX)) {
                verify_for(key, header, op, body, now_ms, recipient)
            } else {
                verify(key, header, op, body, now_ms)
            }
        };
        match check(&self.own) {
            Err(Refused::BadSignature) => match &self.shared {
                Some(shared) => check(shared),
                None => Err(Refused::BadSignature),
            },
            done => done,
        }
    }
}

fn missing(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// This Worker's own caller key, from [`OWN_KEY`]; an error when it is not bound or not set.
pub async fn own_key(env: &Env) -> Result<Vec<u8>> {
    let binding = env
        .secret_store(OWN_KEY)
        .map_err(|_| missing("this Worker has no own caller key: CALLER_KEY_OWN is not bound"))?;
    let key = secrets_store::read(&binding)
        .await?
        .ok_or_else(|| missing("this Worker has no own caller key: CALLER_KEY_OWN is not set"))?;
    Ok(key.into_bytes())
}

/// The shared caller key, from [`SHARED_KEY`]; `None` when it is not bound.
pub async fn shared_key(env: &Env) -> Result<Option<Vec<u8>>> {
    match env.secret_store(SHARED_KEY) {
        Ok(binding) => Ok(secrets_store::read(&binding).await?.map(String::into_bytes)),
        Err(_) => Ok(None),
    }
}

/// The keys this Worker checks core's signatures with. Fails closed when the own key is
/// missing.
pub async fn read(env: &Env) -> Result<Keys> {
    Ok(Keys {
        own: own_key(env).await?,
        shared: shared_key(env).await?,
    })
}

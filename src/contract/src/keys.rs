//! A capability Worker's caller key (docs/plans/caller-keys.md): its own, bound as [`OWN_KEY`].
//! Since step 4 there is no shared key: core signs its calls to a Worker with that Worker's key
//! alone, and the Worker checks them with it alone.
//!
//! A Worker signs its own requests to core with [`own_key`]. A Worker whose own key is missing
//! serves and signs nothing: there is no fall-back to another key, nor to unsigned.

use pigeon_core::secrets_store;
use worker::{Env, Error, Result};

use crate::caller::{CONTEXT_PREFIX, Refused, Signed, verify, verify_for};

/// The Secrets Store binding of a capability Worker's own caller key. The same binding name in
/// every Worker; each binds its own secret (`CALLER_KEY_<WORKER>`) under it.
pub const OWN_KEY: &str = "CALLER_KEY_OWN";

/// The key a capability Worker checks core's signatures with. No `Debug`: it holds a secret.
#[derive(Clone)]
pub struct Keys {
    /// This Worker's own key, the only one it accepts.
    pub own: Vec<u8>,
}

impl Keys {
    /// The caller in `header`, for this Worker (`recipient`, its deployed name), checked with
    /// the own key. A contextual envelope is checked with [`verify_for`], a legacy one with
    /// [`verify`]. Pure.
    pub fn verify(
        &self,
        header: Option<&str>,
        op: &str,
        body: &[u8],
        now_ms: i64,
        recipient: &str,
    ) -> std::result::Result<Signed, Refused> {
        if header.is_some_and(|h| h.starts_with(CONTEXT_PREFIX)) {
            verify_for(&self.own, header, op, body, now_ms, recipient)
        } else {
            verify(&self.own, header, op, body, now_ms)
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

/// The key this Worker checks core's signatures with. Fails closed when the own key is missing.
pub async fn read(env: &Env) -> Result<Keys> {
    Ok(Keys {
        own: own_key(env).await?,
    })
}

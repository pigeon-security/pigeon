//! A zone in the account, such as `pigeon.dev`. The zone is added and its nameservers moved by
//! hand; this capability finds it and reports whether it is active.

use serde::Deserialize;
use worker::{Error, Method, Result};

use crate::cloudflare::call;
use crate::tokens::{Policy, Scope};

/// What a token for this capability holds.
pub fn policies() -> Vec<Policy> {
    vec![Policy::new(Scope::AllZones, &["Zone Read"])]
}

/// A zone, as the API lists it.
#[derive(Debug, Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
    /// `active` once its nameservers point at Cloudflare; `pending` before.
    pub status: String,
}

impl Zone {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

/// Zone `name` in `account`.
pub async fn zone(token: &str, account: &str, name: &str) -> Result<Zone> {
    let v = call(
        token,
        Method::Get,
        &format!("/zones?name={name}&account.id={account}"),
        None,
    )
    .await?;
    let zones: Vec<Zone> = serde_json::from_value(v)?;
    zones
        .into_iter()
        .next()
        .ok_or_else(|| Error::RustError(format!("zone `{name}` is not in the account")))
}

//! Zones in the account, such as `pigeon.dev`. A zone is added and its nameservers moved by
//! hand; this finds it and checks it is active.

use serde::{Deserialize, Serialize};
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// A zone, as the API lists it.
#[derive(Debug, Deserialize, Serialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
    /// `active` once its nameservers point at Cloudflare; `pending` before.
    pub status: String,
}

/// Zone `name` in `account`, which must be active.
pub async fn active(account: &Account, name: &str) -> Result<Zone> {
    let policies = vec![Policy::new(Scope::AllZones, &["Zone Read"])];
    let token = Scoped::open(account, "zones", policies).await?;
    let zone = find(token.value(), &account.id, name).await;
    let zone = token.finish(zone).await?;
    if zone.status != "active" {
        return Err(Error::RustError(format!(
            "zone `{name}` is {}, not active",
            zone.status
        )));
    }
    Ok(zone)
}

async fn find(token: &str, account: &str, name: &str) -> Result<Zone> {
    let path = format!("/zones?name={name}&account.id={account}");
    let zones: Vec<Zone> = serde_json::from_value(call(token, Method::Get, &path, None).await?)?;
    zones
        .into_iter()
        .next()
        .ok_or_else(|| Error::RustError(format!("zone `{name}` is not in the account")))
}

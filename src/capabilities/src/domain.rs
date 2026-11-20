//! A zone in the account, such as `pigeon.dev`, and its apex. The zone is added and its
//! nameservers moved by hand; this capability checks it is active and builds on [`zero_trust`]
//! to put the apex behind Access.

use serde::{Deserialize, Serialize};
use worker::{Error, Method, Result};

use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};
use crate::zero_trust::{Protected, Protection};
use crate::{Account, zero_trust};

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
    let token = Scoped::open(
        account,
        "domain",
        vec![Policy::new(Scope::AllZones, &["Zone Read"])],
    )
    .await?;
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

/// What [`protect`] made.
#[derive(Serialize)]
pub struct Guarded {
    pub zone: Zone,
    pub access: Protected,
}

/// Puts zone `zone`'s apex behind Access; idempotent. Its DNS and whatever it points at are left
/// as they are.
pub async fn protect(account: &Account, zone: &str, protection: &Protection) -> Result<Guarded> {
    let zone = active(account, zone).await?;
    let access = zero_trust::protect(account, &zone.name, protection).await?;
    Ok(Guarded { zone, access })
}

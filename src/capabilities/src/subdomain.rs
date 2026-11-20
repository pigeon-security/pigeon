//! A hostname in one of the account's zones, served by a Worker behind Access. It builds on
//! [`domain`] for the zone and [`zero_trust`] for Access, then attaches the hostname to the
//! Worker.

use serde::Serialize;
use worker::Result;

use crate::domain::Zone;
use crate::zero_trust::{Protected, Protection};
use crate::{Account, custom_domain, domain, zero_trust};

/// A hostname to serve.
pub struct Spec {
    /// The zone it is in, such as `pigeon.dev`.
    pub zone: String,
    /// The hostname, such as `platform.pigeon.dev`.
    pub host: String,
    /// The Worker that serves it.
    pub worker: String,
    /// What protects it.
    pub protection: Protection,
}

/// What [`serve`] made.
#[derive(Serialize)]
pub struct Served {
    pub zone: Zone,
    pub host: String,
    pub access: Protected,
    pub custom_domain: String,
}

/// Serves `spec.host` from `spec.worker` behind Access; idempotent. In order, so the hostname is
/// never served before Access holds it:
/// 1. the zone is in the account and active;
/// 2. Access protects the hostname;
/// 3. the hostname is attached to the Worker.
pub async fn serve(account: &Account, spec: &Spec) -> Result<Served> {
    let zone = domain::active(account, &spec.zone).await?;
    let access = zero_trust::protect(account, &spec.host, &spec.protection).await?;
    let custom_domain = custom_domain::attach(account, &zone.id, &spec.host, &spec.worker).await?;
    Ok(Served {
        zone,
        host: spec.host.clone(),
        access,
        custom_domain,
    })
}

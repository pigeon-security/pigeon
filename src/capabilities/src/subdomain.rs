//! A hostname in one of the account's zones, served by a Worker behind Access. It builds on
//! [`domain`] for the zone and [`zero_trust`] for Access, then attaches the hostname as a
//! Workers Custom Domain, so Cloudflare creates its DNS record and certificate.

use serde::Serialize;
use serde_json::json;
use worker::{Error, Method, Result};

use crate::cloudflare::call;
use crate::domain::Zone;
use crate::tokens::{Policy, Scope, Scoped};
use crate::zero_trust::{Protected, Protection};
use crate::{Account, domain, zero_trust};

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
    let token = Scoped::open(account, "subdomain", policies()).await?;
    let attached = attach_worker(
        token.value(),
        &account.id,
        &zone.id,
        &spec.host,
        &spec.worker,
    )
    .await;
    let custom_domain = token.finish(attached).await?;
    Ok(Served {
        zone,
        host: spec.host.clone(),
        access,
        custom_domain,
    })
}

/// What the attaching token holds.
fn policies() -> Vec<Policy> {
    vec![
        Policy::new(Scope::Account, &["Workers Scripts Write"]),
        Policy::new(
            Scope::AllZones,
            &[
                "Zone Read",
                "DNS Write",
                "Workers Routes Write",
                "SSL and Certificates Write",
            ],
        ),
    ]
}

/// Serves `hostname`, in zone `zone_id`, from Worker `service`; idempotent. Answers the custom
/// domain's ID.
async fn attach_worker(
    token: &str,
    account: &str,
    zone_id: &str,
    hostname: &str,
    service: &str,
) -> Result<String> {
    let body = json!({
        "hostname": hostname,
        "zone_id": zone_id,
        "service": service,
        "environment": "production",
    });
    let path = format!("/accounts/{account}/workers/domains");
    let v = call(token, Method::Put, &path, Some(&body)).await?;
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("`{hostname}` came back without its id")))
}

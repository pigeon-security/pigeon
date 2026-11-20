//! A hostname in one of the account's zones, served by a Worker as a Workers Custom Domain:
//! Cloudflare creates its DNS record and certificate.

use serde_json::json;
use worker::{Error, Method, Result};

use crate::cloudflare::call;
use crate::tokens::{Policy, Scope};

/// What a token for this capability holds.
pub fn policies() -> Vec<Policy> {
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
pub async fn attach_worker(
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
    let v = call(
        token,
        Method::Put,
        &format!("/accounts/{account}/workers/domains"),
        Some(&body),
    )
    .await?;
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("`{hostname}` came back without its id")))
}

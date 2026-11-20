//! Attaches a hostname to a Worker as a Workers Custom Domain, so Cloudflare creates its DNS
//! record and certificate. Shared by [`crate::domain`] and [`crate::subdomain`].

use serde_json::json;
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// Serves `hostname`, in zone `zone_id`, from Worker `worker`; idempotent. Answers the custom
/// domain's ID.
pub(crate) async fn attach(
    account: &Account,
    zone_id: &str,
    hostname: &str,
    worker: &str,
) -> Result<String> {
    let token = Scoped::open(account, "custom-domain", policies()).await?;
    let attached = put(token.value(), &account.id, zone_id, hostname, worker).await;
    token.finish(attached).await
}

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

async fn put(
    token: &str,
    account: &str,
    zone_id: &str,
    hostname: &str,
    worker: &str,
) -> Result<String> {
    let body = json!({
        "hostname": hostname,
        "zone_id": zone_id,
        "service": worker,
        "environment": "production",
    });
    let path = format!("/accounts/{account}/workers/domains");
    let v = call(token, Method::Put, &path, Some(&body)).await?;
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("`{hostname}` came back without its id")))
}

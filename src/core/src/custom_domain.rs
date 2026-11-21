//! Attaches a hostname to a Worker as a Workers Custom Domain, so Cloudflare creates its DNS
//! record and certificate.

use serde_json::json;
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// Serves `hostname`, in zone `zone_id`, from Worker `worker`, moving it from any other Worker;
/// idempotent. Answers the custom domain's ID.
pub async fn attach(
    account: &Account,
    zone_id: &str,
    hostname: &str,
    worker: &str,
) -> Result<String> {
    let token = Scoped::open(account, "custom-domain", policies()).await?;
    let attached = put(token.value(), &account.id, zone_id, hostname, worker).await;
    token.finish(attached).await
}

/// The Worker serving `hostname` as a custom domain, if one does.
pub async fn serving(account: &Account, hostname: &str) -> Result<Option<String>> {
    let policies = vec![Policy::new(Scope::Account, &["Workers Scripts Read"])];
    let token = Scoped::open(account, "custom-domain-read", policies).await?;
    let path = format!(
        "/accounts/{}/workers/domains?hostname={hostname}",
        account.id
    );
    let found = call(token.value(), Method::Get, &path, None)
        .await
        .map(|v| {
            v.as_array()?.iter().find(|d| {
                d["hostname"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(hostname))
            })?["service"]
                .as_str()
                .map(String::from)
        });
    token.finish(found).await
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
        // Setup decides which Worker serves a hostname, so it moves one from another Worker.
        "override_existing_origin": true,
    });
    let path = format!("/accounts/{account}/workers/domains");
    let v = call(token, Method::Put, &path, Some(&body)).await?;
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("`{hostname}` came back without its id")))
}

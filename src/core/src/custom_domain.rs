//! Attaches a hostname to a Worker as a Workers Custom Domain, so Cloudflare creates its DNS
//! record and certificate; and detaches it again, only from the Worker named.

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

/// Stops serving `hostname` from Worker `worker`: deletes the custom domain only when it is
/// that Worker's, so Cloudflare removes the DNS record and certificate it made. Answers whether
/// one was deleted; a hostname served by another Worker, or by none, is left as it is.
pub async fn detach(account: &Account, hostname: &str, worker: &str) -> Result<bool> {
    if !crate::workers::valid_hostname(hostname) || !crate::workers::valid_segment(worker) {
        return Err(Error::RustError(
            "the host name or Worker is not valid".into(),
        ));
    }
    let token = Scoped::open(account, "custom-domain detach", policies()).await?;
    let removed = async {
        let path = format!(
            "/accounts/{}/workers/domains?hostname={hostname}",
            account.id
        );
        let listed = call(token.value(), Method::Get, &path, None).await?;
        let Some(id) = attached_id(&listed, hostname, worker) else {
            return Ok(false);
        };
        let path = format!("/accounts/{}/workers/domains/{id}", account.id);
        match call(token.value(), Method::Delete, &path, None).await {
            Ok(_) => Ok(true),
            Err(e) if crate::workers::is_not_found(&e.to_string()) => Ok(false),
            Err(e) => Err(e),
        }
    }
    .await;
    token.finish(removed).await
}

/// The id of the custom domain in `listed` that serves `hostname` from `worker`, if one does.
/// Pure.
pub fn attached_id(listed: &serde_json::Value, hostname: &str, worker: &str) -> Option<String> {
    listed
        .as_array()?
        .iter()
        .find(|d| d["hostname"].as_str() == Some(hostname) && d["service"].as_str() == Some(worker))
        .and_then(|d| d["id"].as_str())
        .filter(|id| crate::workers::valid_segment(id))
        .map(str::to_string)
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

//! Read-only checks for an approved instance's future compute connection.

use serde_json::Value;
use worker::{Error, Result};

use crate::transport::{DIGITALOCEAN_ACCOUNT_URL, HttpRequest, Transport};

/// Whether a DigitalOcean team identity is well formed for an instance binding.
pub fn is_digitalocean_team_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Probe the team selected in an approved instance record. A successful
/// probe confirms team identity and does not establish compute permissions.
pub async fn probe_digitalocean(transport: &dyn Transport, team: &str, secret: &str) -> Result<()> {
    if !is_digitalocean_team_id(team) {
        return Err(Error::RustError(
            "invalid approved DigitalOcean team".into(),
        ));
    }
    if secret.is_empty() || !secret.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::RustError("invalid DigitalOcean credential".into()));
    }
    let provider = "DigitalOcean";
    let response = transport
        .send(HttpRequest {
            method: "GET".into(),
            url: DIGITALOCEAN_ACCOUNT_URL.into(),
            headers: vec![("Authorization".into(), format!("Bearer {secret}"))],
            body: String::new(),
        })
        .await
        .map_err(|_| Error::RustError(format!("{provider} compute probe transport failed")))?;
    if response.status != 200 {
        return Err(Error::RustError(format!(
            "{provider} compute probe returned HTTP {}",
            response.status
        )));
    }
    let body: Value = serde_json::from_str(&response.body)
        .map_err(|_| Error::RustError(format!("{provider} compute probe returned invalid JSON")))?;
    let matches = body["account"]["team"]["uuid"]
        .as_str()
        .is_some_and(|actual| actual == team);
    if !matches {
        return Err(Error::RustError(format!(
            "{provider} compute probe could not confirm the approved target"
        )));
    }
    Ok(())
}

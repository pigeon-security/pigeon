//! The DigitalOcean API subset for the bounded Pigeon runner health check.

use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::transport::{HttpRequest, HttpResponse, Transport};

const API: &str = "https://api.digitalocean.com/v2/droplets";
const TAG: &str = "pigeon-health-check";
const QUERY: &str = "?tag_name=pigeon-health-check&per_page=2&name=";
pub const REGION: &str = "tor1";
pub const SIZE: &str = "s-2vcpu-4gb";
pub const IMAGE: &str = "ubuntu-24-04-x64";
pub const NAME_PREFIX: &str = "pigeon-health-";

// DigitalOcean's published hourly rate for the fixed Basic Droplet size.
const USD_MICROS_PER_HOUR: u64 = 35_710;
const MILLIS_PER_HOUR: u64 = 3_600_000;

#[derive(Debug, PartialEq, Serialize)]
pub struct ComputeEstimate {
    pub duration_ms: u64,
    pub estimated_usd_micros: u64,
    pub usd_micros_per_hour: u64,
    pub deletion_confirmed: bool,
}

/// Prorate the published compute rate from the recorded creation timestamp. While
/// deletion is unconfirmed, the estimate runs through the supplied observation time.
pub fn compute_estimate(
    start_ms: u64,
    observed_until_ms: u64,
    confirmed_delete_ms: Option<u64>,
) -> Option<ComputeEstimate> {
    let duration_ms = confirmed_delete_ms
        .unwrap_or(observed_until_ms)
        .checked_sub(start_ms)?;
    let estimated_usd_micros = u64::try_from(
        (u128::from(duration_ms) * u128::from(USD_MICROS_PER_HOUR))
            .div_ceil(u128::from(MILLIS_PER_HOUR)),
    )
    .ok()?;
    Some(ComputeEstimate {
        duration_ms,
        estimated_usd_micros,
        usd_micros_per_hour: USD_MICROS_PER_HOUR,
        deletion_confirmed: confirmed_delete_ms.is_some(),
    })
}

fn name_valid(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

fn droplet_id(path: &str) -> Option<u64> {
    let id = path.strip_prefix('/')?.parse::<u64>().ok()?;
    (id > 0 && path == format!("/{id}")).then_some(id)
}

pub(crate) fn allowed(method: &str, url: &str) -> bool {
    let Some(path) = url.strip_prefix(API) else {
        return false;
    };
    match method {
        "POST" => path.is_empty(),
        "GET" => path.strip_prefix(QUERY).is_some_and(name_valid) || droplet_id(path).is_some(),
        "DELETE" => droplet_id(path).is_some(),
        _ => false,
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

async fn request(
    transport: &dyn Transport,
    method: &str,
    tail: &str,
    token: &str,
    body: String,
) -> Result<HttpResponse> {
    transport
        .send(HttpRequest {
            method: method.into(),
            url: format!("{API}{tail}"),
            headers: vec![
                ("Authorization".into(), format!("Bearer {token}")),
                ("Content-Type".into(), "application/json".into()),
            ],
            body,
        })
        .await
        .map_err(|_| bad("DigitalOcean Droplet request transport failed"))
}

fn read(response: &HttpResponse) -> Result<Value> {
    serde_json::from_str(&response.body)
        .map_err(|_| bad("DigitalOcean returned an invalid Droplet response"))
}

fn identify(droplet: &Value, name: &str) -> Option<u64> {
    if droplet["name"].as_str() != Some(name)
        || !droplet["tags"]
            .as_array()?
            .iter()
            .any(|tag| tag.as_str() == Some(TAG))
    {
        return None;
    }
    droplet["id"].as_u64().filter(|id| *id > 0)
}

pub enum Created {
    Accepted(u64),
    /// Provider definitely refused this request. No resources to reconcile.
    Rejected(u16),
    /// No safe result. Reconcile the exact job identity and never send POST again.
    Unknown,
}

pub async fn create(transport: &dyn Transport, token: &str, name: &str, script: &str) -> Created {
    if !name_valid(name) {
        return Created::Unknown;
    }
    let body = json!({
        "name": name,
        "region": REGION,
        "size": SIZE,
        "image": IMAGE,
        "tags": [TAG],
        "backups": false,
        "ipv6": false,
        "monitoring": false,
        "user_data": script,
    });
    let Ok(response) = request(transport, "POST", "", token, body.to_string()).await else {
        return Created::Unknown;
    };
    match response.status {
        202 => match read(&response)
            .ok()
            .and_then(|v| identify(&v["droplet"], name))
        {
            Some(id) => Created::Accepted(id),
            None => Created::Unknown,
        },
        400 | 401 | 403 | 404 | 422 => Created::Rejected(response.status),
        _ => Created::Unknown,
    }
}

pub async fn reconcile(transport: &dyn Transport, token: &str, name: &str) -> Result<Option<u64>> {
    if !name_valid(name) {
        return Err(bad("invalid Droplet job identity"));
    }
    let response = request(
        transport,
        "GET",
        &format!("{QUERY}{name}"),
        token,
        String::new(),
    )
    .await?;
    if response.status != 200 {
        return Err(bad(format!(
            "DigitalOcean Droplet lookup returned HTTP {}",
            response.status
        )));
    }
    let body = read(&response)?;
    let rows = body["droplets"]
        .as_array()
        .ok_or_else(|| bad("missing DigitalOcean Droplet list"))?;
    if !body["links"]["pages"]["next"].is_null() {
        return Err(bad("DigitalOcean did not return a unique Droplet result"));
    }
    let mut matches = rows.iter().filter_map(|row| identify(row, name));
    let found = matches.next();
    if matches.next().is_some() {
        return Err(bad("DigitalOcean did not return a unique Droplet result"));
    }
    Ok(found)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropletStatus {
    New,
    Active,
    Off,
    Archive,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "presence", content = "status", rename_all = "snake_case")]
pub enum Observation {
    Absent,
    Present(DropletStatus),
}

/// Read one known Droplet with the existing Droplet read scope. Status is only
/// returned after confirming the exact ID, job name and health-check tag.
pub async fn observe(
    transport: &dyn Transport,
    token: &str,
    name: &str,
    id: u64,
) -> Result<Observation> {
    if id == 0 || !name_valid(name) {
        return Err(bad("invalid Droplet job identity"));
    }
    let path = format!("/{id}");
    let response = request(transport, "GET", &path, token, String::new()).await?;
    match response.status {
        404 => Ok(Observation::Absent),
        200 => {
            let body = read(&response)?;
            let droplet = &body["droplet"];
            if identify(droplet, name) != Some(id) {
                return Err(bad("DigitalOcean Droplet ownership could not be confirmed"));
            }
            let status = match droplet["status"].as_str() {
                Some("new") => DropletStatus::New,
                Some("active") => DropletStatus::Active,
                Some("off") => DropletStatus::Off,
                Some("archive") => DropletStatus::Archive,
                _ => DropletStatus::Unknown,
            };
            Ok(Observation::Present(status))
        }
        status => Err(bad(format!(
            "DigitalOcean Droplet lookup returned HTTP {status}"
        ))),
    }
}

pub enum Removal {
    /// No Droplet with this known id currently exists.
    Absent,
    /// Delete was requested. A later independent GET must confirm absence.
    Requested,
}

pub async fn cleanup(
    transport: &dyn Transport,
    token: &str,
    name: &str,
    id: u64,
) -> Result<Removal> {
    if observe(transport, token, name, id).await? == Observation::Absent {
        return Ok(Removal::Absent);
    }
    let path = format!("/{id}");
    let response = request(transport, "DELETE", &path, token, String::new()).await?;
    match response.status {
        204 | 404 => Ok(Removal::Requested),
        status => Err(bad(format!(
            "DigitalOcean Droplet delete returned HTTP {status}"
        ))),
    }
}

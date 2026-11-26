//! The DigitalOcean API subset for the bounded Pigeon runner health check.

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
    if id == 0 || !name_valid(name) {
        return Err(bad("invalid Droplet job identity"));
    }
    let path = format!("/{id}");
    let response = request(transport, "GET", &path, token, String::new()).await?;
    match response.status {
        404 => return Ok(Removal::Absent),
        200 if identify(&read(&response)?["droplet"], name) == Some(id) => {}
        200 => return Err(bad("DigitalOcean Droplet ownership could not be confirmed")),
        status => {
            return Err(bad(format!(
                "DigitalOcean Droplet lookup returned HTTP {status}"
            )));
        }
    }
    let response = request(transport, "DELETE", &path, token, String::new()).await?;
    match response.status {
        204 | 404 => Ok(Removal::Requested),
        status => Err(bad(format!(
            "DigitalOcean Droplet delete returned HTTP {status}"
        ))),
    }
}

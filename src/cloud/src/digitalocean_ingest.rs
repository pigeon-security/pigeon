//! DigitalOcean resources owned by a single bounded ingest job.

use serde_json::{Value, json};
use worker::{Error, Result};

use crate::digitalocean_spaces::REGIONS;
use crate::transport::{HttpRequest, HttpResponse, Transport};

const DROPLETS: &str = "https://api.digitalocean.com/v2/droplets";
const VOLUMES: &str = "https://api.digitalocean.com/v2/volumes";
const TAG: &str = "pigeon-ingest";
const NAME_PREFIX: &str = "pigeon-ingest-";

pub const SIZE: &str = "s-4vcpu-8gb";
pub const IMAGE: &str = "ubuntu-24-04-x64";
pub const VOLUME_FILESYSTEM: &str = "ext4";

fn lower_hex(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn run_id_valid(id: &str) -> bool {
    id.len() == 32 && lower_hex(id.as_bytes())
}

fn name_valid(name: &str) -> bool {
    name.strip_prefix(NAME_PREFIX).is_some_and(run_id_valid)
}

fn uuid_valid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                lower_hex(&[byte])
            }
        })
}

fn canonical_id(path: &str) -> bool {
    let Some(id) = path.strip_prefix('/') else {
        return false;
    };
    id.parse::<u64>()
        .is_ok_and(|number| number > 0 && id == number.to_string())
}

pub(crate) fn allowed(method: &str, url: &str) -> bool {
    if let Some(path) = url.strip_prefix(DROPLETS) {
        return method == "GET"
            && path
                .strip_prefix("?tag_name=pigeon-ingest&per_page=2&name=")
                .is_some_and(name_valid);
    }
    let Some(path) = url.strip_prefix(VOLUMES) else {
        return false;
    };
    match method {
        "POST" => path.is_empty(),
        "DELETE" => path.strip_prefix('/').is_some_and(uuid_valid),
        "GET" => {
            path.strip_prefix('/').is_some_and(uuid_valid)
                || path.strip_prefix("?name=").is_some_and(|query| {
                    query.split_once("&region=").is_some_and(|(name, rest)| {
                        name_valid(name)
                            && rest
                                .strip_suffix("&per_page=2")
                                .is_some_and(|region| REGIONS.contains(&region))
                    })
                })
        }
        _ => false,
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// A run has one resource name in its approved region. Neither ID alone authorizes deletion.
pub struct Target<'a> {
    name: String,
    region: &'a str,
}

impl<'a> Target<'a> {
    pub fn new(run_id: &str, region: &'a str) -> Result<Self> {
        if !run_id_valid(run_id) || !REGIONS.contains(&region) {
            return Err(bad("Invalid DigitalOcean ingest resource identity."));
        }
        Ok(Self {
            name: format!("{NAME_PREFIX}{run_id}"),
            region,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> String {
        format!("Pigeon ingest scratch for {}", self.name)
    }
}

/// A create request is sent once. An ambiguous result requires reconciliation by identity.
#[derive(Debug, PartialEq, Eq)]
pub enum Created<T> {
    Accepted(T),
    Rejected(u16),
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Absent,
    Requested,
    Attached,
}

async fn request(
    transport: &dyn Transport,
    token: &str,
    method: &str,
    url: String,
    body: Option<Value>,
) -> Result<HttpResponse> {
    transport
        .send(HttpRequest {
            method: method.into(),
            url,
            headers: vec![
                ("Authorization".into(), format!("Bearer {token}")),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: body.map_or_else(String::new, |value| value.to_string()),
        })
        .await
        .map_err(|_| bad("DigitalOcean ingest request failed."))
}

fn read(response: &HttpResponse) -> Result<Value> {
    serde_json::from_str(&response.body).map_err(|_| bad("Invalid DigitalOcean ingest response."))
}

fn outcome<T>(
    result: Result<HttpResponse>,
    accepted: u16,
    identify: impl FnOnce(&Value) -> Option<T>,
) -> Created<T> {
    let Ok(response) = result else {
        return Created::Unknown;
    };
    match response.status {
        status if status == accepted => read(&response)
            .ok()
            .and_then(|value| identify(&value))
            .map_or(Created::Unknown, Created::Accepted),
        400 | 401 | 403 | 404 | 422 => Created::Rejected(response.status),
        _ => Created::Unknown,
    }
}

fn identifies(row: &Value, target: &Target<'_>) -> bool {
    row["name"].as_str() == Some(target.name())
        && row["region"]["slug"].as_str() == Some(target.region)
}

fn volume_id<'a>(row: &'a Value, target: &Target<'_>) -> Option<&'a str> {
    if !identifies(row, target) || row["description"].as_str() != Some(&target.description()) {
        return None;
    }
    row["id"].as_str().filter(|id| uuid_valid(id))
}

fn droplet_id(row: &Value, target: &Target<'_>) -> Option<u64> {
    if !identifies(row, target)
        || !row["tags"]
            .as_array()?
            .iter()
            .any(|tag| tag.as_str() == Some(TAG))
    {
        return None;
    }
    row["id"].as_u64().filter(|id| *id > 0)
}

pub async fn create_volume(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
    gib: u16,
) -> Created<String> {
    if !(1..=1024).contains(&gib) {
        return Created::Rejected(400);
    }
    let body = json!({
        "name": target.name(),
        "description": target.description(),
        "region": target.region,
        "size_gigabytes": gib,
        "filesystem_type": VOLUME_FILESYSTEM,
    });
    let response = request(transport, token, "POST", VOLUMES.into(), Some(body)).await;
    outcome(response, 201, |value| {
        volume_id(&value["volume"], target).map(str::to_owned)
    })
}

pub async fn create_droplet(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
    volume: &str,
    script: &str,
) -> Created<u64> {
    if !uuid_valid(volume) {
        return Created::Rejected(400);
    }
    let body = json!({
        "name": target.name(),
        "region": target.region,
        "size": SIZE,
        "image": IMAGE,
        "tags": [TAG],
        "backups": false,
        "ipv6": false,
        "monitoring": false,
        "volumes": [volume],
        "user_data": script,
    });
    let response = request(transport, token, "POST", DROPLETS.into(), Some(body)).await;
    outcome(response, 202, |value| droplet_id(&value["droplet"], target))
}

fn unique<T>(
    response: &HttpResponse,
    key: &str,
    identify: impl Fn(&Value) -> Option<T>,
) -> Result<Option<T>> {
    if response.status != 200 {
        return Err(bad(format!(
            "DigitalOcean ingest resource lookup returned HTTP {}.",
            response.status
        )));
    }
    let body = read(response)?;
    let rows = body[key]
        .as_array()
        .ok_or_else(|| bad("DigitalOcean ingest resource list was missing."))?;
    let mut found = rows.iter().filter_map(identify);
    let one = found.next();
    if !body["links"]["pages"]["next"].is_null() || found.next().is_some() {
        return Err(bad("DigitalOcean ingest resource identity was ambiguous."));
    }
    Ok(one)
}

pub async fn find_volume(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
) -> Result<Option<String>> {
    let url = format!(
        "{VOLUMES}?name={}&region={}&per_page=2",
        target.name(),
        target.region
    );
    let response = request(transport, token, "GET", url, None).await?;
    unique(&response, "volumes", |row| {
        volume_id(row, target).map(str::to_owned)
    })
}

pub async fn find_droplet(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
) -> Result<Option<u64>> {
    let url = format!(
        "{DROPLETS}?tag_name={TAG}&per_page=2&name={}",
        target.name()
    );
    let response = request(transport, token, "GET", url, None).await?;
    unique(&response, "droplets", |row| droplet_id(row, target))
}

async fn delete(transport: &dyn Transport, token: &str, url: String) -> Result<Removal> {
    let response = request(transport, token, "DELETE", url, None).await?;
    match response.status {
        204 | 404 => Ok(Removal::Requested),
        status => Err(bad(format!(
            "DigitalOcean ingest resource deletion returned HTTP {status}."
        ))),
    }
}

pub async fn remove_droplet(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
    id: u64,
) -> Result<Removal> {
    let path = format!("/{id}");
    if !canonical_id(&path) {
        return Err(bad("Invalid DigitalOcean Droplet ID."));
    }
    let url = format!("{DROPLETS}{path}");
    let response = request(transport, token, "GET", url.clone(), None).await?;
    match response.status {
        404 => Ok(Removal::Absent),
        200 if droplet_id(&read(&response)?["droplet"], target) == Some(id) => {
            delete(transport, token, url).await
        }
        _ => Err(bad(
            "Could not verify DigitalOcean ingest Droplet ownership.",
        )),
    }
}

/// Check a bootstrap request against the exact owned Droplet's public IPv4 address.
pub async fn public_ipv4(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
    id: u64,
) -> Result<Option<std::net::Ipv4Addr>> {
    if id == 0 {
        return Err(bad("Invalid DigitalOcean Droplet ID."));
    }
    let response = request(transport, token, "GET", format!("{DROPLETS}/{id}"), None).await?;
    if response.status != 200 {
        return Ok(None);
    }
    let body = read(&response)?;
    let droplet = &body["droplet"];
    if droplet_id(droplet, target) != Some(id) {
        return Err(bad(
            "Could not verify DigitalOcean ingest Droplet ownership.",
        ));
    }
    let mut public = droplet["networks"]["v4"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|network| network["type"].as_str() == Some("public"))
        .filter_map(|network| network["ip_address"].as_str()?.parse().ok());
    let address = public.next();
    if public.next().is_some() {
        return Err(bad("DigitalOcean returned ambiguous public addresses."));
    }
    Ok(address)
}

/// A volume is deleted only after the provider reports it detached. A later GET confirms absence.
pub async fn remove_volume(
    transport: &dyn Transport,
    token: &str,
    target: &Target<'_>,
    id: &str,
) -> Result<Removal> {
    if !uuid_valid(id) {
        return Err(bad("Invalid DigitalOcean Volume ID."));
    }
    let url = format!("{VOLUMES}/{id}");
    let response = request(transport, token, "GET", url.clone(), None).await?;
    match response.status {
        404 => Ok(Removal::Absent),
        200 => {
            let body = read(&response)?;
            let row = &body["volume"];
            if volume_id(row, target) != Some(id) {
                return Err(bad(
                    "Could not verify DigitalOcean ingest Volume ownership.",
                ));
            }
            if !row
                .get("droplet_ids")
                .is_some_and(|ids| ids.is_null() || ids.as_array().is_some_and(Vec::is_empty))
            {
                return Ok(Removal::Attached);
            }
            delete(transport, token, url).await
        }
        _ => Err(bad(
            "Could not verify DigitalOcean ingest Volume ownership.",
        )),
    }
}

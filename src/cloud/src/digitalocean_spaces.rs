//! Read-only connection check for one approved DigitalOcean Spaces bucket and prefix.

use pigeon_core::s3::{self, Endpoint, Keys};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Error, Result};

use crate::transport::{HttpRequest, Transport};

const API: &str = "https://api.digitalocean.com/v2/spaces/keys/";
const DOMAIN: &str = ".digitaloceanspaces.com";
const QUERY: &str = "list-type=2&max-keys=1";
/// Regions offering DigitalOcean Spaces Standard Storage.
pub const REGIONS: &[&str] = &[
    "ams3", "atl1", "blr1", "fra1", "lon1", "mkc1", "nyc3", "ric1", "sfo2", "sfo3", "sgp1", "syd1",
    "tor1",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub region: String,
    pub bucket: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

fn bucket_name(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Binding {
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if !REGIONS.contains(&self.region.as_str()) {
            return Err("select a supported DigitalOcean Spaces region");
        }
        if !bucket_name(&self.bucket) {
            return Err("enter a valid DigitalOcean Spaces bucket name");
        }
        if self.prefix.len() > 1024 || self.prefix.chars().any(char::is_control) {
            return Err(
                "a Spaces prefix must be at most 1024 bytes and contain no control characters",
            );
        }
        Ok(())
    }
}

fn key_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Only the exact by-ID metadata route and the bounded listing shape are allowed.
pub(crate) fn allowed_url(url: &str) -> bool {
    if let Some(id) = url.strip_prefix(API) {
        return key_id(id);
    }
    let Some((host, tail)) = url.strip_prefix("https://").and_then(|s| s.split_once('/')) else {
        return false;
    };
    let Some(region) = host.strip_suffix(DOMAIN) else {
        return false;
    };
    let Some((bucket, query)) = tail.split_once('?') else {
        return false;
    };
    REGIONS.contains(&region)
        && bucket_name(bucket)
        && (query == QUERY
            || query
                .strip_prefix("list-type=2&max-keys=1&prefix=")
                .is_some_and(|prefix| {
                    !prefix.is_empty()
                        && prefix.bytes().all(|b| {
                            b.is_ascii_alphanumeric()
                                || matches!(b, b'-' | b'_' | b'.' | b'~' | b'%')
                        })
                }))
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

fn list_request(binding: &Binding, keys: &Keys, now_ms: i64) -> HttpRequest {
    let endpoint = Endpoint {
        base: format!("https://{}{DOMAIN}", binding.region),
        region: binding.region.clone(),
    };
    let path = s3::object_path(&binding.bucket, "");
    let query = s3::list_objects_query(Some(&binding.prefix), None, 1, None);
    let url = if binding.prefix.is_empty() {
        format!("{}{path}?{QUERY}", endpoint.base)
    } else {
        format!(
            "{}{path}?{QUERY}&prefix={}",
            endpoint.base,
            s3::uri_encode(&binding.prefix, true)
        )
    };
    let headers = s3::sign(
        "GET",
        &endpoint,
        &path,
        &query,
        &[],
        s3::EMPTY_SHA256,
        keys,
        &s3::amz_date(now_ms as f64),
    );
    HttpRequest {
        method: "GET".into(),
        url,
        headers,
        body: String::new(),
    }
}

/// Verify the PAT's team, the Spaces key's exact grant, and access to the approved prefix.
/// The returned result contains no provider payload, access key ID, or object name.
pub async fn probe(
    transport: &dyn Transport,
    team: &str,
    pat: &str,
    binding: &Binding,
    keys: &Keys,
    now_ms: i64,
) -> Result<()> {
    binding.validate().map_err(bad)?;
    if !key_id(&keys.access_key_id)
        || !(1..=512).contains(&keys.secret_access_key.len())
        || !keys.secret_access_key.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(bad("invalid DigitalOcean Spaces credential"));
    }
    crate::compute::probe_digitalocean(transport, team, pat).await?;
    let response = transport
        .send(HttpRequest {
            method: "GET".into(),
            url: format!("{API}{}", keys.access_key_id),
            headers: vec![("Authorization".into(), format!("Bearer {pat}"))],
            body: String::new(),
        })
        .await
        .map_err(|_| bad("DigitalOcean Spaces key lookup failed"))?;
    if response.status != 200 {
        return Err(bad(format!(
            "DigitalOcean Spaces key lookup returned HTTP {}; check the team and spaces_key:read scope",
            response.status
        )));
    }
    let key: Value = serde_json::from_str(&response.body)
        .map_err(|_| bad("DigitalOcean Spaces key lookup returned invalid JSON"))?;
    let key = &key["key"];
    let read_only = key["grants"].as_array().is_some_and(|grants| {
        matches!(grants.as_slice(), [grant]
            if grant["bucket"].as_str() == Some(binding.bucket.as_str())
                && grant["permission"].as_str() == Some("read"))
    });
    if key["access_key"].as_str() != Some(keys.access_key_id.as_str()) || !read_only {
        return Err(bad(
            "use a DigitalOcean Spaces key with only a read grant for the approved bucket",
        ));
    }

    let listing = transport
        .send(list_request(binding, keys, now_ms))
        .await
        .map_err(|_| bad("DigitalOcean Spaces listing failed"))?;
    if listing.status != 200 {
        return Err(bad(format!(
            "DigitalOcean Spaces listing returned HTTP {}",
            listing.status
        )));
    }
    let contents = s3::parse_list_objects(&listing.body)
        .map_err(|_| bad("DigitalOcean Spaces returned an invalid object listing"))?;
    if contents.objects.len() > 1
        || !contents.prefixes.is_empty()
        || contents
            .objects
            .iter()
            .any(|object| !object.key.starts_with(&binding.prefix))
    {
        return Err(bad(
            "DigitalOcean Spaces listing fell outside the approved scope",
        ));
    }
    Ok(())
}

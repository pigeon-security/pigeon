//! Scaleway Object Storage requests: the S3 API at `https://s3.<region>.scw.cloud`, path-style,
//! signed with `pigeon_core::s3::sign`. Building and reading are pure. What core's `s3` module
//! lacks (ListObjectVersions, HeadObject's answer, the name rules) is here.

use pigeon_core::s3::{self, Endpoint, Keys};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::transport::{HttpRequest, HttpResponse};

/// The Object Storage regions the gateway reaches.
pub const REGIONS: [&str; 3] = ["fr-par", "nl-ams", "pl-waw"];

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// `region` itself, when it is one of [`REGIONS`]. A region becomes part of a host name, so
/// nothing else passes.
pub fn check_region(region: &str) -> Result<&str> {
    REGIONS
        .iter()
        .copied()
        .find(|r| *r == region)
        .ok_or_else(|| bad(format!("region must be one of {}", REGIONS.join(", "))))
}

/// Checks a bucket name against S3's rules for new buckets: 3 to 63 characters of lowercase
/// letters, digits and hyphens, starting and ending with a letter or digit. A bucket name is
/// part of a path here and never of a host, but the stricter rules keep it portable.
pub fn check_bucket(name: &str) -> Result<()> {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let ok = (3..=63).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && edge(name.chars().next())
        && edge(name.chars().last())
        && !name.starts_with("xn--")
        && !name.starts_with("sthree-")
        && !name.ends_with("-s3alias")
        && !name.ends_with("--ol-s3");
    if ok {
        Ok(())
    } else {
        Err(bad(
            "a bucket name is 3 to 63 characters of a-z, 0-9 and `-`, starting and ending with \
             a letter or digit",
        ))
    }
}

/// The query as it goes on the URL: percent-encoded and sorted, as it was signed.
pub fn query_string(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (s3::uri_encode(k, true), s3::uri_encode(v, true)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// A signed request to `region`'s endpoint, with an empty body. `path` is already encoded (see
/// `pigeon_core::s3::object_path`). `now_ms` dates the signature.
pub fn request(
    method: &str,
    region: &str,
    path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    keys: &Keys,
    now_ms: i64,
) -> Result<HttpRequest> {
    let region = check_region(region)?;
    let endpoint = Endpoint {
        base: format!("https://s3.{region}.scw.cloud"),
        region: region.into(),
    };
    let signed = s3::sign(
        method,
        &endpoint,
        path,
        query,
        headers,
        s3::EMPTY_SHA256,
        keys,
        &s3::amz_date(now_ms as f64),
    );
    let mut url = format!("{}{path}", endpoint.base);
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query_string(query));
    }
    Ok(HttpRequest {
        method: method.into(),
        url,
        headers: signed.into_iter().filter(|(k, _)| k != "host").collect(),
        body: String::new(),
    })
}

/// The query of ListObjectVersions that asks for one entry: enough to tell whether a bucket
/// holds any object, old version or delete marker.
pub fn versions_query() -> Vec<(String, String)> {
    vec![
        ("versions".into(), String::new()),
        ("max-keys".into(), "1".into()),
    ]
}

/// Whether a `ListVersionsResult` holds no version and no delete marker.
pub fn versions_empty(xml: &str) -> Result<bool> {
    if !xml.contains("<ListVersionsResult") {
        return Err(bad(
            "the answer is not a version list (no ListVersionsResult)",
        ));
    }
    Ok(!xml.contains("<Version>") && !xml.contains("<DeleteMarker>"))
}

/// What HeadObject answered, as the tool shows it.
pub fn head_view(bucket: &str, key: &str, res: &HttpResponse) -> Value {
    let header = |name: &str| res.header(name).unwrap_or_default().to_string();
    json!({
        "bucket": bucket,
        "key": key,
        "size": res.header("content-length").and_then(|s| s.trim().parse::<u64>().ok()),
        "etag": header("etag").replace('"', ""),
        "last_modified": header("last-modified"),
        "content_type": header("content-type"),
    })
}

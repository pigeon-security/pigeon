//! A small S3 client for S3-compatible object storage (Scaleway, DigitalOcean Spaces,
//! Backblaze B2). Path-style addressing only. Request signing (AWS Signature Version 4) and the
//! XML readers are pure, so they can be checked against recorded inputs. Only [`send`] touches
//! the network. A secret key never appears in an error or in `Debug` output.

use serde::Serialize;
use sha2::{Digest, Sha256};
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Response, Result};

use crate::sign::{hex, hmac_sha256};

/// The SHA-256 of an empty body, for requests that carry none.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Payload hash to send when the body is not hashed.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// An access key pair. Its `Debug` never shows the secret.
pub struct Keys {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<hidden>")
            .finish()
    }
}

/// Reads keys from the stored form `ACCESS_KEY_ID:SECRET_ACCESS_KEY`. The error never holds
/// the value.
pub fn parse_keys(value: &str) -> Result<Keys> {
    let bad = |why: &str| {
        Error::RustError(format!(
            "the storage key is not `ACCESS_KEY_ID:SECRET_ACCESS_KEY`: {why}"
        ))
    };
    let (id, secret) = value
        .trim()
        .split_once(':')
        .ok_or_else(|| bad("there is no `:`"))?;
    let (id, secret) = (id.trim(), secret.trim());
    if id.is_empty() || secret.is_empty() {
        return Err(bad("one part is empty"));
    }
    if id.chars().any(char::is_whitespace) || secret.chars().any(char::is_whitespace) {
        return Err(bad("a part holds whitespace"));
    }
    Ok(Keys {
        access_key_id: id.into(),
        secret_access_key: secret.into(),
    })
}

/// Where a provider's S3 API is.
pub struct Endpoint {
    /// Such as `https://s3.fr-par.scw.cloud`, with no trailing slash.
    pub base: String,
    /// The region requests are signed for.
    pub region: String,
}

impl Endpoint {
    /// The host part of `base`, which is what the `host` header carries.
    pub fn host(&self) -> &str {
        let rest = self.base.split_once("://").map_or(&*self.base, |(_, r)| r);
        rest.split('/').next().unwrap_or(rest)
    }
}

/// Percent-encodes `s` as AWS asks: `A-Za-z0-9-._~` stay, all else is `%XX` in capitals. A `/`
/// stays when `encode_slash` is false.
pub fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The path-style request path for `key` in `bucket`: `/{bucket}/{encoded key}`. An empty key
/// answers `/{bucket}`.
pub fn object_path(bucket: &str, key: &str) -> String {
    if key.is_empty() {
        format!("/{bucket}")
    } else {
        format!("/{bucket}/{}", uri_encode(key, false))
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn scope(amz_date: &str, region: &str) -> String {
    let day = amz_date.get(..8).unwrap_or(amz_date);
    format!("{day}/{region}/s3/aws4_request")
}

fn signing_key(keys: &Keys, amz_date: &str, region: &str) -> [u8; 32] {
    let day = amz_date.get(..8).unwrap_or(amz_date);
    let k = hmac_sha256(
        format!("AWS4{}", keys.secret_access_key).as_bytes(),
        day.as_bytes(),
    );
    let k = hmac_sha256(&k, region.as_bytes());
    let k = hmac_sha256(&k, b"s3");
    hmac_sha256(&k, b"aws4_request")
}

fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The signature over a canonical request.
fn signature(canonical: &str, keys: &Keys, amz_date: &str, region: &str) -> String {
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{}\n{}",
        scope(amz_date, region),
        sha256_hex(canonical.as_bytes())
    );
    hex(&hmac_sha256(
        &signing_key(keys, amz_date, region),
        to_sign.as_bytes(),
    ))
}

/// Signs a request. `path` is the already-encoded request path. Answers the headers to send:
/// `headers` plus `host`, `x-amz-date`, `x-amz-content-sha256` and `authorization`.
/// `payload_sha256` is the body's hash, or [`UNSIGNED_PAYLOAD`].
#[allow(clippy::too_many_arguments)]
pub fn sign(
    method: &str,
    endpoint: &Endpoint,
    path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    payload_sha256: &str,
    keys: &Keys,
    amz_date: &str,
) -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    all.push(("host".into(), endpoint.host().into()));
    all.push(("x-amz-date".into(), amz_date.into()));
    all.push(("x-amz-content-sha256".into(), payload_sha256.into()));
    all.sort();
    let canonical_headers: String = all.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = all
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{signed}\n{payload_sha256}",
        canonical_query(query)
    );
    let sig = signature(&canonical, keys, amz_date, &endpoint.region);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={signed},Signature={sig}",
        keys.access_key_id,
        scope(amz_date, &endpoint.region)
    );
    all.push(("authorization".into(), authorization));
    all
}

/// The longest a presigned link may live through the tools, in seconds.
pub const PRESIGN_MAX_S: u32 = 3600;

/// A presigned GET URL for `path`, valid for `expires_s` seconds (1 to 3600).
pub fn presign_get(
    endpoint: &Endpoint,
    path: &str,
    keys: &Keys,
    amz_date: &str,
    expires_s: u32,
) -> Result<String> {
    if !(1..=PRESIGN_MAX_S).contains(&expires_s) {
        return Err(Error::RustError(format!(
            "`expires_s` must be 1 to {PRESIGN_MAX_S}"
        )));
    }
    Ok(presign_url(endpoint, path, keys, amz_date, expires_s))
}

/// Like [`presign_get`], without the limit on `expires_s`.
pub fn presign_url(
    endpoint: &Endpoint,
    path: &str,
    keys: &Keys,
    amz_date: &str,
    expires_s: u32,
) -> String {
    let query = vec![
        (
            "X-Amz-Algorithm".to_string(),
            "AWS4-HMAC-SHA256".to_string(),
        ),
        (
            "X-Amz-Credential".to_string(),
            format!(
                "{}/{}",
                keys.access_key_id,
                scope(amz_date, &endpoint.region)
            ),
        ),
        ("X-Amz-Date".to_string(), amz_date.to_string()),
        ("X-Amz-Expires".to_string(), expires_s.to_string()),
        ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
    ];
    let cq = canonical_query(&query);
    let canonical = format!(
        "GET\n{path}\n{cq}\nhost:{}\n\nhost\n{UNSIGNED_PAYLOAD}",
        endpoint.host()
    );
    let sig = signature(&canonical, keys, amz_date, &endpoint.region);
    format!("{}{path}?{cq}&X-Amz-Signature={sig}", endpoint.base)
}

/// `ms` since the Unix epoch as `YYYYMMDDTHHMMSSZ`.
pub fn amz_date(ms: f64) -> String {
    let secs = if ms.is_finite() && ms > 0.0 {
        (ms / 1000.0) as i64
    } else {
        0
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// ---- XML readers ----

/// Text with the five XML entities unescaped.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let table = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ];
        match table.iter().find(|(e, _)| rest.starts_with(e)) {
            Some((e, c)) => {
                out.push(*c);
                rest = &rest[e.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The inner text of each `<tag>…</tag>` in `xml`, raw.
fn blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        match after.find(&close) {
            Some(j) => {
                out.push(&after[..j]);
                rest = &after[j + close.len()..];
            }
            None => break,
        }
    }
    out
}

/// The unescaped text of the first `<tag>` in `xml`.
fn first(xml: &str, tag: &str) -> Option<String> {
    blocks(xml, tag).first().map(|s| unescape(s))
}

/// A bucket as ListBuckets answers it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Bucket {
    pub name: String,
    pub created: String,
}

/// Reads a `ListAllMyBucketsResult`.
pub fn parse_list_buckets(xml: &str) -> Result<Vec<Bucket>> {
    if !xml.contains("<ListAllMyBucketsResult") {
        return Err(Error::RustError(
            "the answer is not a bucket list (no ListAllMyBucketsResult)".into(),
        ));
    }
    Ok(blocks(xml, "Bucket")
        .into_iter()
        .filter_map(|b| {
            Some(Bucket {
                name: first(b, "Name")?,
                created: first(b, "CreationDate").unwrap_or_default(),
            })
        })
        .collect())
}

/// An object as ListObjectsV2 answers it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub last_modified: String,
}

/// One page of ListObjectsV2.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Listing {
    pub objects: Vec<Object>,
    /// The CommonPrefixes.
    pub prefixes: Vec<String>,
    /// The token for the next page, when the list is truncated.
    pub next: Option<String>,
}

/// Reads a ListObjectsV2 `ListBucketResult`.
pub fn parse_list_objects(xml: &str) -> Result<Listing> {
    if !xml.contains("<ListBucketResult") {
        return Err(Error::RustError(
            "the answer is not an object list (no ListBucketResult)".into(),
        ));
    }
    let objects = blocks(xml, "Contents")
        .into_iter()
        .filter_map(|c| {
            Some(Object {
                key: first(c, "Key")?,
                size: first(c, "Size").and_then(|s| s.trim().parse().ok())?,
                etag: first(c, "ETag").unwrap_or_default().replace('"', ""),
                last_modified: first(c, "LastModified").unwrap_or_default(),
            })
        })
        .collect();
    let prefixes = blocks(xml, "CommonPrefixes")
        .into_iter()
        .filter_map(|c| first(c, "Prefix"))
        .collect();
    let truncated = first(xml, "IsTruncated").is_some_and(|t| t.trim() == "true");
    let next = if truncated {
        first(xml, "NextContinuationToken")
    } else {
        None
    };
    Ok(Listing {
        objects,
        prefixes,
        next,
    })
}

/// `S3 {status} {Code}: {Message}` from an `<Error>` body, at most 200 characters.
pub fn parse_error(status: u16, xml: &str) -> String {
    let text = match (first(xml, "Code"), first(xml, "Message")) {
        (Some(c), Some(m)) => format!("S3 {status} {c}: {m}"),
        (Some(c), None) => format!("S3 {status} {c}"),
        _ => format!("S3 {status}"),
    };
    text.chars().take(200).collect()
}

/// The query of a ListObjectsV2 call. `max_keys` is held to 1..=1000.
pub fn list_objects_query(
    prefix: Option<&str>,
    delimiter: Option<&str>,
    max_keys: u32,
    token: Option<&str>,
) -> Vec<(String, String)> {
    let mut q = vec![
        ("list-type".to_string(), "2".to_string()),
        ("max-keys".to_string(), max_keys.clamp(1, 1000).to_string()),
    ];
    if let Some(p) = prefix.filter(|p| !p.is_empty()) {
        q.push(("prefix".into(), p.into()));
    }
    if let Some(d) = delimiter.filter(|d| !d.is_empty()) {
        q.push(("delimiter".into(), d.into()));
    }
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        q.push(("continuation-token".into(), t.into()));
    }
    q
}

// ---- the network ----

/// Sends a signed request and answers the response. A status of 400 or more becomes an error
/// from [`parse_error`]. Only GET and HEAD are signed here: the body is empty.
pub async fn send(
    method: Method,
    endpoint: &Endpoint,
    path: &str,
    query: &[(String, String)],
    extra_headers: &[(String, String)],
    keys: &Keys,
) -> Result<Response> {
    let date = amz_date(worker::js_sys::Date::now());
    let signed = sign(
        method.as_ref(),
        endpoint,
        path,
        query,
        extra_headers,
        EMPTY_SHA256,
        keys,
        &date,
    );
    let headers = Headers::new();
    for (k, v) in signed.iter().filter(|(k, _)| k != "host") {
        headers.set(k, v)?;
    }
    let mut url = format!("{}{path}", endpoint.base);
    if !query.is_empty() {
        url.push('?');
        url.push_str(&canonical_query(query));
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    let mut res = Fetch::Request(Request::new_with_init(&url, &init)?)
        .send()
        .await?;
    let status = res.status_code();
    if status >= 400 {
        let body = res.text().await.unwrap_or_default();
        return Err(Error::RustError(parse_error(status, &body)));
    }
    Ok(res)
}

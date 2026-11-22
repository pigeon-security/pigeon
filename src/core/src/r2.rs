//! R2 buckets and objects through Cloudflare's REST API, and the layout pigeon deploys a static site in.
//!
//! This block wraps the bucket and object endpoints of the Cloudflare API. It does not use the S3 API,
//! whose keys come only from an API token: a linked account's OAuth token can't make them.
//! Each I/O function opens one short-lived token, does its work and revokes the token.
//! The `*_with` variants take a token that is already open, so a deploy can use one token for many calls.
//!
//! A site lives in one bucket. File contents are stored once under `blobs/{sha256}`.
//! Each deploy is a manifest at `deploys/{commit}.json` that maps site paths to blobs.
//! The `live` object names the commit that is being served.
//! Everything that decides something (keys, manifests, lookups, pruning) is a pure function.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::Account;
use crate::cloudflare::call_with;
use crate::sign::hex;
use crate::tokens::{Policy, Scope, Scoped};

/// The key of the object that names the commit being served.
pub const LIVE: &str = "live";

/// The base URL of the Cloudflare API.
const API: &str = "https://api.cloudflare.com/client/v4";

/// The header that selects an R2 jurisdiction.
const JURISDICTION_HEADER: &str = "cf-r2-jurisdiction";

/// An R2 bucket, with its jurisdiction if it has one (for example `eu`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bucket {
    /// The bucket name.
    pub name: String,
    /// The jurisdiction the bucket lives in, if any.
    pub jurisdiction: Option<String>,
}

/// The outcome of `ensure_bucket`.
#[derive(Clone, Debug, Serialize)]
pub struct Ensured {
    /// The bucket name.
    pub name: String,
    /// True when this call created the bucket, false when it already existed.
    pub created: bool,
}

/// One file of a deployed site.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    /// The lowercase hex SHA-256 of the file content. It names the blob.
    pub blob: String,
    /// The content type to serve the file with.
    pub content_type: String,
    /// The size of the file in bytes.
    pub size: u64,
}

/// A deploy: site paths such as `/posts/x/index.html` mapped to their files.
pub type Manifest = BTreeMap<String, Entry>;

// ---------------------------------------------------------------------------
// Pure: keys, paths, bodies, parsing
// ---------------------------------------------------------------------------

/// Answers the object key of a blob with the given SHA-256 hex.
pub fn blob_key(sha256_hex: &str) -> String {
    format!("blobs/{sha256_hex}")
}

/// Answers the object key of the manifest for a commit.
pub fn manifest_key(commit: &str) -> String {
    format!("deploys/{commit}.json")
}

/// Answers the SHA-256 hex in a blob key, or None if the key is not a blob key.
pub fn blob_of_key(key: &str) -> Option<&str> {
    key.strip_prefix("blobs/").filter(|rest| !rest.is_empty())
}

/// Answers the commit in a manifest key, or None if the key is not a manifest key.
pub fn commit_of_key(key: &str) -> Option<&str> {
    key.strip_prefix("deploys/")
        .and_then(|rest| rest.strip_suffix(".json"))
        .filter(|commit| !commit.is_empty())
}

/// Percent-encodes text. Slashes are kept when `keep_slash` is true.
fn percent(input: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Percent-encodes an object key one path segment at a time, keeping the slashes.
pub fn encode_key(key: &str) -> String {
    percent(key, true)
}

/// Answers the API path of a bucket.
fn bucket_path(account_id: &str, bucket: &Bucket) -> String {
    format!(
        "/accounts/{account_id}/r2/buckets/{}",
        percent(&bucket.name, false)
    )
}

/// Answers the API path of one object.
fn object_path(account_id: &str, bucket: &Bucket, key: &str) -> String {
    format!(
        "{}/objects/{}",
        bucket_path(account_id, bucket),
        encode_key(key)
    )
}

/// Answers the API path of one page of an object listing.
fn list_path(account_id: &str, bucket: &Bucket, prefix: &str, cursor: Option<&str>) -> String {
    let mut path = format!("{}/objects?per_page=1000", bucket_path(account_id, bucket));
    if !prefix.is_empty() {
        path.push_str("&prefix=");
        path.push_str(&percent(prefix, false));
    }
    if let Some(cursor) = cursor {
        path.push_str("&cursor=");
        path.push_str(&percent(cursor, false));
    }
    path
}

/// Answers the extra headers every call on this bucket must carry.
fn bucket_headers(bucket: &Bucket) -> Vec<(&str, &str)> {
    bucket
        .jurisdiction
        .as_deref()
        .map(|jurisdiction| (JURISDICTION_HEADER, jurisdiction))
        .into_iter()
        .collect()
}

/// Answers the body that creates a bucket.
fn create_body(bucket: &Bucket) -> Vec<u8> {
    serde_json::json!({ "name": bucket.name })
        .to_string()
        .into_bytes()
}

/// Answers the token policies every R2 call needs.
fn policies() -> Vec<Policy> {
    vec![Policy::new(Scope::Account, &["Workers R2 Storage Write"])]
}

/// Tells whether an API error carries the given HTTP status.
fn has_status(error: &Error, code: u16) -> bool {
    error.to_string().contains(&format!(": {code}:"))
}

/// Builds the error text for a failed raw call. It never includes the token.
fn failure_message(path: &str, status: u16, body: &[u8]) -> String {
    let errors = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("errors").map(ToString::to_string))
        .unwrap_or_else(|| "no details".to_string());
    format!("{path}: {status}: {errors}")
}

/// One page of an object listing.
struct Page {
    /// The keys on this page.
    keys: Vec<String>,
    /// The cursor for the next page, if there is one.
    cursor: Option<String>,
    /// True when more pages follow.
    truncated: bool,
}

/// The JSON envelope of a listing answer.
#[derive(Deserialize)]
struct ListEnvelope {
    #[serde(default)]
    result: Option<Vec<ListItem>>,
    #[serde(default)]
    result_info: Option<ResultInfo>,
}

/// One object in a listing answer.
#[derive(Deserialize)]
struct ListItem {
    key: String,
}

/// The paging information of a listing answer.
#[derive(Deserialize, Default)]
struct ResultInfo {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    is_truncated: bool,
}

/// Parses the raw answer of a listing call into a page.
fn parse_list(status: u16, path: &str, body: &[u8]) -> Result<Page> {
    if !(200..300).contains(&status) {
        return Err(Error::RustError(failure_message(path, status, body)));
    }
    let envelope: ListEnvelope = serde_json::from_slice(body)
        .map_err(|_| Error::RustError(format!("{path}: {status}: unreadable list response")))?;
    let info = envelope.result_info.unwrap_or_default();
    Ok(Page {
        keys: envelope
            .result
            .unwrap_or_default()
            .into_iter()
            .map(|item| item.key)
            .collect(),
        cursor: info.cursor.filter(|cursor| !cursor.is_empty()),
        truncated: info.is_truncated,
    })
}

// ---------------------------------------------------------------------------
// Pure: site layout
// ---------------------------------------------------------------------------

/// Answers the content type for a file path, chosen by its extension.
pub fn content_type(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = match name.rsplit_once('.') {
        Some((_, extension)) => extension.to_ascii_lowercase(),
        None => return "application/octet-stream",
    };
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "webmanifest" => "application/manifest+json",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Makes a path start with a slash.
fn with_leading_slash(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Builds the manifest for a set of files.
///
/// Paths are made to start with a slash. Each blob is the lowercase hex SHA-256 of the content.
pub fn manifest_of(files: &[(String, Vec<u8>)]) -> Manifest {
    files
        .iter()
        .map(|(path, content)| {
            let path = with_leading_slash(path);
            let entry = Entry {
                blob: hex(&Sha256::digest(content)),
                content_type: content_type(&path).to_string(),
                size: content.len() as u64,
            };
            (path, entry)
        })
        .collect()
}

/// Answers the blobs a manifest needs that are not in the bucket yet.
///
/// The answer is sorted and has no duplicates.
pub fn missing(manifest: &Manifest, present: &BTreeSet<String>) -> Vec<String> {
    manifest
        .values()
        .map(|entry| entry.blob.as_str())
        .filter(|blob| !present.contains(*blob))
        .map(str::to_string)
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Answers the manifest path that serves a request path.
///
/// An exact match wins. Otherwise `{path}/index.html` is tried, with or without a trailing slash.
/// Paths with `..` segments are refused.
pub fn resolve(manifest: &Manifest, path: &str) -> Option<String> {
    let path = with_leading_slash(path);
    if path.split('/').any(|segment| segment == "..") {
        return None;
    }
    if manifest.contains_key(&path) {
        return Some(path);
    }
    let candidate = format!("{}/index.html", path.trim_end_matches('/'));
    manifest.contains_key(&candidate).then_some(candidate)
}

/// Answers the commits to delete.
///
/// All but the newest `keep` deploys (by time) are chosen, newest first. The live commit is never chosen.
pub fn prune(deploys: &[(String, u64)], live: &str, keep: usize) -> Vec<String> {
    let mut sorted: Vec<&(String, u64)> = deploys.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    sorted
        .into_iter()
        .skip(keep)
        .filter(|(commit, _)| commit.as_str() != live)
        .map(|(commit, _)| commit.clone())
        .collect()
}

/// Answers the blobs that none of the kept manifests use, sorted.
pub fn unreferenced(all_blobs: &BTreeSet<String>, kept: &[&Manifest]) -> Vec<String> {
    let used: BTreeSet<&str> = kept
        .iter()
        .flat_map(|manifest| manifest.values())
        .map(|entry| entry.blob.as_str())
        .collect();
    all_blobs
        .iter()
        .filter(|blob| !used.contains(blob.as_str()))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------

/// Opens one token for R2 work.
async fn open<'a>(account: &'a Account, purpose: &str) -> Result<Scoped<'a>> {
    Scoped::open(account, purpose, policies()).await
}

/// Fetches an API path with the token and answers the status and the raw body.
async fn raw_get(token: &str, path: &str, extra: &[(&str, &str)]) -> Result<(u16, Vec<u8>)> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    for (name, value) in extra {
        headers.set(name, value)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let request = Request::new_with_init(&format!("{API}{path}"), &init)?;
    let mut response = Fetch::Request(request).send().await?;
    let status = response.status_code();
    let body = response.bytes().await?;
    Ok((status, body))
}

/// Makes sure the bucket exists, using an open token. Creates it only when it is missing.
pub async fn ensure_bucket_with(token: &str, account_id: &str, bucket: &Bucket) -> Result<Ensured> {
    let headers = bucket_headers(bucket);
    let path = bucket_path(account_id, bucket);
    match call_with(token, Method::Get, &path, &headers, None).await {
        Ok(_) => Ok(Ensured {
            name: bucket.name.clone(),
            created: false,
        }),
        Err(error) if has_status(&error, 404) => {
            let create = format!("/accounts/{account_id}/r2/buckets");
            let body = Some(("application/json", create_body(bucket)));
            match call_with(token, Method::Post, &create, &headers, body).await {
                Ok(_) => Ok(Ensured {
                    name: bucket.name.clone(),
                    created: true,
                }),
                Err(error) if has_status(&error, 409) => Ok(Ensured {
                    name: bucket.name.clone(),
                    created: false,
                }),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Tells whether the bucket exists. Reads only.
pub async fn bucket_exists(account: &Account, bucket: &Bucket) -> Result<bool> {
    let token = open(account, "r2 read bucket").await?;
    let headers = bucket_headers(bucket);
    let path = bucket_path(&account.id, bucket);
    let r = match call_with(token.value(), Method::Get, &path, &headers, None).await {
        Ok(_) => Ok(true),
        Err(error) if has_status(&error, 404) => Ok(false),
        Err(error) => Err(error),
    };
    token.finish(r).await
}

/// Makes sure the bucket exists. It is safe to run twice.
pub async fn ensure_bucket(account: &Account, bucket: &Bucket) -> Result<Ensured> {
    let token = open(account, "r2 ensure bucket").await?;
    let result = ensure_bucket_with(token.value(), &account.id, bucket).await;
    token.finish(result).await
}

/// Stores an object, using an open token.
pub async fn put_with(
    token: &str,
    account_id: &str,
    bucket: &Bucket,
    key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let path = object_path(account_id, bucket, key);
    call_with(
        token,
        Method::Put,
        &path,
        &bucket_headers(bucket),
        Some((content_type, body)),
    )
    .await
    .map(|_| ())
}

/// Stores an object.
pub async fn put(
    account: &Account,
    bucket: &Bucket,
    key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let token = open(account, "r2 put").await?;
    let result = put_with(token.value(), &account.id, bucket, key, content_type, body).await;
    token.finish(result).await
}

/// Reads an object, using an open token. Answers None when the object does not exist.
pub async fn get_with(
    token: &str,
    account_id: &str,
    bucket: &Bucket,
    key: &str,
) -> Result<Option<Vec<u8>>> {
    let path = object_path(account_id, bucket, key);
    let (status, body) = raw_get(token, &path, &bucket_headers(bucket)).await?;
    match status {
        404 => Ok(None),
        200..=299 => Ok(Some(body)),
        _ => Err(Error::RustError(failure_message(&path, status, &body))),
    }
}

/// Reads an object. Answers None when the object does not exist.
pub async fn get(account: &Account, bucket: &Bucket, key: &str) -> Result<Option<Vec<u8>>> {
    let token = open(account, "r2 get").await?;
    let result = get_with(token.value(), &account.id, bucket, key).await;
    token.finish(result).await
}

/// Deletes an object, using an open token. A missing object is not an error.
pub async fn delete_with(token: &str, account_id: &str, bucket: &Bucket, key: &str) -> Result<()> {
    let path = object_path(account_id, bucket, key);
    match call_with(token, Method::Delete, &path, &bucket_headers(bucket), None).await {
        Ok(_) => Ok(()),
        Err(error) if has_status(&error, 404) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Deletes an object. A missing object is not an error.
pub async fn delete(account: &Account, bucket: &Bucket, key: &str) -> Result<()> {
    let token = open(account, "r2 delete").await?;
    let result = delete_with(token.value(), &account.id, bucket, key).await;
    token.finish(result).await
}

/// Lists the keys that start with a prefix, using an open token. It follows the cursor to the end.
pub async fn list_with(
    token: &str,
    account_id: &str,
    bucket: &Bucket,
    prefix: &str,
) -> Result<Vec<String>> {
    let headers = bucket_headers(bucket);
    let mut keys = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = list_path(account_id, bucket, prefix, cursor.as_deref());
        let (status, body) = raw_get(token, &path, &headers).await?;
        let page = parse_list(status, &path, &body)?;
        keys.extend(page.keys);
        if !page.truncated {
            return Ok(keys);
        }
        let Some(next) = page
            .cursor
            .filter(|next| cursor.as_deref() != Some(next.as_str()))
        else {
            return Err(Error::RustError(format!(
                "{path}: list cursor did not advance"
            )));
        };
        cursor = Some(next);
    }
}

/// Lists the keys that start with a prefix.
pub async fn list(account: &Account, bucket: &Bucket, prefix: &str) -> Result<Vec<String>> {
    let token = open(account, "r2 list").await?;
    let result = list_with(token.value(), &account.id, bucket, prefix).await;
    token.finish(result).await
}

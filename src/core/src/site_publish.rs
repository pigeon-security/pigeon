//! Shared publishing profile, release identifiers, and the release code every publisher uses:
//! pigeon-sites for linked instances, core for the home instance. A release is staged into a
//! [`Store`] (the blobs it lacks, `deploys/{release}.json`, then `release-meta/{release}.json`)
//! and made live by one write of [`crate::r2::LIVE`], so the previous release stays whole until
//! that last write; a rollback checks a staged release belongs to the same target and moves
//! `live` back; a listing reads the releases and the live one. A Worker serving a release itself
//! (core, on the home instance's apex) reads it with [`live`] and [`pick`], as `site_worker.js`
//! does for linked instances.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::Account;
use crate::r2::{self, Bucket, Manifest, ObjectWrite};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled { site: String },
    Disabled,
}

/// A record's `publishing` profile: at `site.publishing` (docs/plans/record-reshape.md), or at
/// `publishing` in a record of the layout before it.
pub fn profile(record: &Value) -> &Value {
    match &record["site"]["publishing"] {
        Value::Null => &record["publishing"],
        p => p,
    }
}

/// Whether `site` (one of `record`'s `sites`) signs its visitors in with passkeys: an entry
/// for it under the record's `site.passkeys`, or `auth = "passkey"` on the site in a record of
/// the layout before it.
pub fn protected(record: &Value, site: &Value) -> bool {
    site["name"]
        .as_str()
        .is_some_and(|name| record["site"]["passkeys"][name].is_object())
        || site["auth"] == "passkey"
}

/// Resolve the site an approved `publishing` profile names. A record without a profile, or
/// with a disabled one, publishes nothing.
pub fn site(record: &Value) -> Result<&Value, String> {
    let sites = record["sites"].as_array().ok_or("instance has no sites")?;
    let publishing = profile(record);
    if publishing.is_null() {
        return Err("instance has no publishing profile".into());
    }
    match serde_json::from_value::<Profile>(publishing.clone())
        .map_err(|_| "invalid publishing profile".to_string())?
    {
        Profile::Disabled => Err("site publishing is disabled for this instance".into()),
        Profile::Enabled { site } => sites
            .iter()
            .find(|entry| entry["name"] == site)
            .ok_or_else(|| "publishing profile names an unknown site".into()),
    }
}

/// The site's canonical public URL. An approved direct hostname takes precedence over a
/// platform subdomain. A linked Worker's preview origin is never used as a canonical URL.
pub fn base_url(site: &Value, platform_zone: &str) -> Option<String> {
    let targets = site["targets"].as_array()?;
    if let Some(host) = targets
        .iter()
        .find(|target| matches!(target["kind"].as_str(), Some("domain" | "zone")))
        .and_then(|target| target["hostname"].as_str())
    {
        return Some(format!("https://{host}"));
    }

    let dns_label = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let label = targets
        .iter()
        .find(|target| target["kind"] == "subdomain")?["label"]
        .as_str()?;
    (dns_label(label)
        && platform_zone.contains('.')
        && platform_zone.split('.').all(dns_label)
        && label.len() + 1 + platform_zone.len() <= 253)
        .then(|| format!("https://{label}.{platform_zone}"))
}

/// Identifies the complete release inputs and manifest. An identical deployment is idempotent.
pub fn release_id(provenance: &Value, manifest: &crate::r2::Manifest) -> String {
    let data = serde_json::json!([provenance, manifest]);
    format!("release-{}", crate::oauth::hash(&data.to_string()))
}

pub fn release_key(id: &str) -> String {
    format!("release-meta/{id}.json")
}

pub fn is_release_id(id: &str) -> bool {
    id.strip_prefix("release-").is_some_and(|digest| {
        digest.len() == 43
            && digest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Where a site's releases live: one bucket, read and written with one credential.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait Store {
    /// The keys starting with `prefix`.
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;
    /// The object at `key`, or `None`.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// Writes one object.
    async fn put(&self, key: &str, content_type: &str, body: Vec<u8>) -> Result<()>;
    /// Writes independent objects.
    async fn put_objects(&self, objects: Vec<ObjectWrite>) -> Result<()>;
}

/// An R2 bucket of `account`, with its credential.
pub struct R2<'a> {
    pub account: &'a Account,
    pub bucket: &'a Bucket,
}

impl Store for R2<'_> {
    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        r2::list(self.account, self.bucket, prefix).await
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        r2::get(self.account, self.bucket, key).await
    }
    async fn put(&self, key: &str, content_type: &str, body: Vec<u8>) -> Result<()> {
        r2::put(self.account, self.bucket, key, content_type, body).await
    }
    async fn put_objects(&self, objects: Vec<ObjectWrite>) -> Result<()> {
        r2::put_objects(self.account, self.bucket, objects).await
    }
}

/// A staged release: its id, its file count, and how many blobs were new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub release: String,
    pub files: usize,
    pub uploaded: usize,
}

/// Stages `files` (site path, bytes, content type) as the immutable release `provenance`
/// describes: the blobs the bucket lacks, then the manifest, then the provenance. Never moves
/// `live`. `stage` hears `storage_plan`, `blobs=<n>`, `blobs_staged` and `manifest_staged` as
/// they happen.
pub async fn stage<S: Store>(
    store: &S,
    provenance: &Value,
    files: Vec<(String, Vec<u8>, String)>,
    progress: &mut dyn FnMut(&str),
) -> Result<Staged> {
    let mut present: BTreeSet<String> = store
        .list("blobs/")
        .await?
        .iter()
        .filter_map(|k| r2::blob_of_key(k).map(str::to_string))
        .collect();
    let mut manifest = Manifest::new();
    let mut fresh = Vec::new();
    for (path, bytes, ctype) in files {
        let pair = [(path, bytes)];
        let (p, mut e) = r2::manifest_of(&pair)
            .into_iter()
            .next()
            .expect("one file gives one entry");
        let [(_, bytes)] = pair;
        e.content_type = ctype.clone();
        if present.insert(e.blob.clone()) {
            fresh.push(ObjectWrite {
                key: r2::blob_key(&e.blob),
                content_type: ctype,
                body: bytes,
            });
        }
        manifest.insert(p, e);
    }
    let uploaded = fresh.len();
    progress("storage_plan");
    progress(&format!("blobs={uploaded}"));
    store.put_objects(fresh).await?;
    progress("blobs_staged");
    let files = manifest.len();
    let release = release_id(provenance, &manifest);
    store
        .put(
            &r2::manifest_key(&release),
            "application/json",
            serde_json::to_vec(&manifest).map_err(|e| bad(e.to_string()))?,
        )
        .await?;
    store
        .put(
            &release_key(&release),
            "application/json",
            serde_json::to_vec(provenance)?,
        )
        .await?;
    progress("manifest_staged");
    Ok(Staged {
        release,
        files,
        uploaded,
    })
}

/// Makes `release` live: the one write that moves `live`.
pub async fn activate<S: Store>(store: &S, release: &str) -> Result<()> {
    store
        .put(r2::LIVE, "text/plain", release.as_bytes().to_vec())
        .await
}

/// The publishing target a release must belong to: what its provenance names.
pub struct Owner<'a> {
    pub instance: &'a str,
    pub base_url: &'a str,
    pub site: &'a str,
}

/// The provenance of staged release `release`, checked: it exists, belongs to `owner`, and its
/// manifest matches its id. Reads only.
pub async fn staged<S: Store>(store: &S, release: &str, owner: &Owner<'_>) -> Result<Value> {
    if !is_release_id(release) {
        return Err(bad("expected a site release ID"));
    }
    let provenance = store
        .get(&release_key(release))
        .await?
        .ok_or_else(|| bad("unknown site release"))?;
    let provenance: Value = serde_json::from_slice(&provenance)?;
    if provenance["instance"] != owner.instance
        || provenance["base_url"] != owner.base_url
        || provenance["site"] != owner.site
    {
        return Err(bad("release belongs to a different publishing target"));
    }
    let manifest = store
        .get(&r2::manifest_key(release))
        .await?
        .ok_or_else(|| bad("site release is missing its manifest"))?;
    let manifest: Manifest = serde_json::from_slice(&manifest)?;
    if release_id(&provenance, &manifest) != release {
        return Err(bad("site release manifest does not match its identity"));
    }
    Ok(provenance)
}

/// The most releases [`releases`] answers.
pub const MAX_RELEASES: usize = 50;

/// The immutable releases in `store` and the one `live` points at: each release's provenance
/// (commit, template, base URL), live first, then by id, at most `limit` (1 to
/// [`MAX_RELEASES`]). Reads only.
pub async fn releases<S: Store>(
    store: &S,
    instance: &str,
    site: &str,
    limit: usize,
) -> Result<Value> {
    let live = store
        .get(r2::LIVE)
        .await?
        .map(|b| String::from_utf8_lossy(&b).trim().to_string());
    let mut ids: Vec<String> = store
        .list("release-meta/")
        .await?
        .iter()
        .filter_map(|k| {
            k.strip_prefix("release-meta/")?
                .strip_suffix(".json")
                .filter(|id| is_release_id(id))
                .map(str::to_string)
        })
        .collect();
    let total = ids.len();
    ids.sort_by_key(|id| (Some(id) != live.as_ref(), id.clone()));
    ids.truncate(limit.clamp(1, MAX_RELEASES));
    let mut out = Vec::new();
    for id in ids {
        let provenance: Value = match store.get(&release_key(&id)).await? {
            Some(b) => serde_json::from_slice(&b).unwrap_or(Value::Null),
            None => Value::Null,
        };
        out.push(json!({
            "release": id,
            "live": Some(&id) == live.as_ref(),
            "commit": provenance["commit"],
            "template": provenance["template"],
            "template_version": provenance["template_version"],
            "base_url": provenance["base_url"],
        }));
    }
    Ok(json!({ "instance": instance, "site": site, "live": live, "total": total, "releases": out }))
}

// ---------------------------------------------------------------------------
// Serving: what `src/capabilities/src/instances/site_worker.js` does for a linked instance's
// Worker, for a Worker that serves a release itself (core, for the home instance's apex). The
// same layout: `live` names a release, `deploys/{release}.json` maps each site path to its blob,
// `release-meta/{release}.json` holds its provenance, `blobs/{sha256}` each file.
// ---------------------------------------------------------------------------

/// Reads one object: all that serving a release needs. Every [`Store`] reads.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait Read {
    /// The object at `key`, or `None`.
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>>;
}

impl<S: Store> Read for S {
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get(key).await
    }
}

/// The release `live` names, with its manifest and the commit its provenance names.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub release: String,
    pub commit: Option<String>,
    pub manifest: Manifest,
}

/// The live release in `store`, as `site_worker.js`'s `current` reads it: `None` when nothing
/// is live (no `live`, a `live` naming anything but a release, or a release whose manifest is
/// missing). When `known` is already that release it is answered again without reading its
/// manifest. Reads only.
pub async fn live<R: Read>(store: &R, known: Option<&Live>) -> Result<Option<Live>> {
    let Some(named) = store.read(r2::LIVE).await? else {
        return Ok(None);
    };
    let release = String::from_utf8_lossy(&named).trim().to_string();
    // Only immutable releases are served; anything else serves nothing until the next publish.
    if !is_release_id(&release) {
        return Ok(None);
    }
    if let Some(k) = known.filter(|k| k.release == release) {
        return Ok(Some(k.clone()));
    }
    let Some(manifest) = store.read(&r2::manifest_key(&release)).await? else {
        return Ok(None);
    };
    let manifest: Manifest = serde_json::from_slice(&manifest)?;
    let commit = match store.read(&release_key(&release)).await? {
        Some(meta) => serde_json::from_slice::<Value>(&meta)
            .ok()
            .and_then(|m| m["commit"].as_str().map(str::to_string)),
        None => None,
    };
    Ok(Some(Live {
        release,
        commit,
        manifest,
    }))
}

/// The manifest key that serves the decoded `path`: the path itself, or its `index.html`
/// (`/` is `/index.html`, `/about/` and `/about` are `/about/index.html`). A path with a `..`
/// segment never resolves. Pure.
pub fn resolve<'a>(manifest: &'a Manifest, path: &str) -> Option<&'a str> {
    if path.split('/').any(|s| s == "..") {
        return None;
    }
    if let Some((k, _)) = manifest.get_key_value(path) {
        return Some(k);
    }
    let candidate = format!("{}/index.html", path.trim_end_matches('/'));
    manifest.get_key_value(&candidate).map(|(k, _)| k.as_str())
}

/// The release's page for a path it does not have.
pub const NOT_FOUND_PAGE: &str = "/404.html";

/// What a release answers for the decoded `path`: its file with 200, or the release's
/// [`NOT_FOUND_PAGE`] with 404; `None` when it has neither. Pure.
pub fn pick<'a>(manifest: &'a Manifest, path: &str) -> Option<(u16, &'a str, &'a r2::Entry)> {
    let (status, key) = match resolve(manifest, path) {
        Some(k) => (200, k),
        None => (404, manifest.get_key_value(NOT_FOUND_PAGE)?.0.as_str()),
    };
    Some((status, key, &manifest[key]))
}

/// The `Cache-Control` of a release file: a minute for pages and feeds, an hour for the rest.
pub fn cache_control(key: &str) -> &'static str {
    if key.ends_with(".html") || key.ends_with(".xml") {
        "public, max-age=60"
    } else {
        "public, max-age=3600"
    }
}

/// The `ETag` of a release file: its blob, quoted.
pub fn etag(entry: &r2::Entry) -> String {
    format!("\"{}\"", entry.blob)
}

/// A URL path, percent-decoded as `decodeURIComponent` does; `None` when an escape is malformed
/// or the bytes are not UTF-8. Pure.
pub fn decode_path(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = raw.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Whether a file of `content_type` is a script, which a script-free origin never serves.
pub fn is_script_type(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    ["javascript", "ecmascript", "wasm"]
        .iter()
        .any(|s| t.contains(s))
}

/// Whether a file of `content_type` is text a browser may run script from (pages, SVG, XML,
/// stylesheets), so it is read whole and checked by [`scriptless`] before it is served.
pub fn needs_scan(content_type: &str) -> bool {
    let t = content_type.to_ascii_lowercase();
    t.starts_with("text/") || t.contains("xml") || t.contains("svg") || t.contains("html")
}

/// Whether `body`, of `content_type`, is free of script: not a script type, and, when it is
/// text, no `<script`, no `javascript:` and no `on…=` event-handler attribute in any tag. The
/// templates never write any (L1); this is the serving side's check of the same, so a release
/// that somehow carries one is not served. Pure.
pub fn scriptless(content_type: &str, body: &[u8]) -> bool {
    if is_script_type(content_type) {
        return false;
    }
    if !needs_scan(content_type) {
        return true;
    }
    let lower = String::from_utf8_lossy(body).to_ascii_lowercase();
    !(lower.contains("<script") || lower.contains("javascript:") || has_event_handler(&lower))
}

/// Whether lowercase markup has an `on…=` attribute inside a tag.
fn has_event_handler(lower: &str) -> bool {
    let b = lower.as_bytes();
    let mut in_tag = false;
    for i in 0..b.len() {
        match b[i] {
            b'<' => in_tag = true,
            b'>' => in_tag = false,
            c if in_tag
                && (c.is_ascii_whitespace() || c == b'/' || c == b'"' || c == b'\'')
                && b[i + 1..].starts_with(b"on") =>
            {
                let rest = &b[i + 3..];
                let name = rest.iter().take_while(|c| c.is_ascii_alphabetic()).count();
                if name > 0
                    && rest[name..]
                        .iter()
                        .find(|c| !c.is_ascii_whitespace())
                        .is_some_and(|c| *c == b'=')
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

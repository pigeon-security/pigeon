//! The owning Data Worker authorizes source reads without giving this runner a provider key.
//!
//! Connections to the Data Worker and to the provider are kept and reused across the engine's
//! threads, and downloads are signed ahead in batches of the listing's next keys
//! (docs/plans/ingest-bytes.md): one Worker request per batch rather than per object.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pigeon_core::s3::{self, Listing};
use pigeon_ingest::{
    Error,
    engine::{Object, Page, Source},
    managed_destination::valid_origin,
    source_credentials as wire,
};

/// How long a URL signed ahead is used: presigned URLs last at most
/// [`wire::GET_MAX_SECONDS`], so leave room for the request to start.
const FRESH: Duration = Duration::from_secs(40);
/// Most URLs kept signed ahead.
const CACHE: usize = 4096;

pub struct ManagedSource {
    signer: String,
    grant: String,
    provider: String,
    bucket: String,
    control: ureq::Agent,
    download: ureq::Agent,
    /// Listed keys not yet signed, for signing ahead.
    upcoming: Mutex<BTreeSet<String>>,
    /// URLs signed ahead, by key, with when they were signed.
    signed: Mutex<HashMap<String, (String, Instant)>>,
    /// One batch is signed at a time, so threads that need the same keys wait for it.
    signing: Mutex<()>,
    /// The Data Worker signs batches; cleared when it does not know `get_many`.
    batches: AtomicBool,
}

/// An agent that keeps up to 128 idle connections per host for the engine's threads (ureq keeps
/// one by default, so most requests would open a new TLS connection).
fn agent(read: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(read)
        .max_idle_connections(256)
        .max_idle_connections_per_host(128)
        .build()
}

impl ManagedSource {
    pub fn new(source: wire::RunnerSource, endpoint: &str, bucket: &str) -> Result<Self, Error> {
        if !valid_origin(&source.origin)
            || !valid_origin(endpoint)
            || source.grant.is_empty()
            || source.grant.len() > wire::REQUEST_MAX
        {
            return Err(Error::Config("invalid owning-account source".into()));
        }
        Ok(Self {
            signer: format!("{}{}", source.origin, wire::RUN_PATH),
            grant: source.grant,
            provider: endpoint.to_owned(),
            bucket: bucket.to_owned(),
            control: agent(Duration::from_secs(90)),
            download: agent(Duration::from_secs(90)),
            upcoming: Mutex::new(BTreeSet::new()),
            signed: Mutex::new(HashMap::new()),
            signing: Mutex::new(()),
            batches: AtomicBool::new(true),
        })
    }

    /// One signed operation; the HTTP status when the Data Worker refused it.
    fn call(&self, operation: &wire::Read) -> Result<Vec<u8>, Option<u16>> {
        let body = serde_json::to_vec(operation).map_err(|_| None)?;
        let response = self
            .control
            .post(&self.signer)
            .set("content-type", "application/json")
            .set("authorization", &format!("Bearer {}", self.grant))
            .send_bytes(&body)
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => Some(code),
                ureq::Error::Transport(_) => None,
            })?;
        if response.status() != 200 {
            return Err(Some(response.status()));
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(wire::RESPONSE_MAX as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| None)?;
        if body.len() > wire::RESPONSE_MAX {
            return Err(None);
        }
        Ok(body)
    }

    fn read(&self, operation: wire::Read) -> Result<Vec<u8>, Error> {
        self.call(&operation).map_err(|status| match status {
            Some(_) => Error::Source("the owning Data Worker refused a source read".into()),
            None => Error::Source("cannot read the Data Worker's source response".into()),
        })
    }

    /// A URL for `key` signed ahead and still fresh, taken from the cache.
    fn take(&self, key: &str) -> Option<String> {
        let mut signed = self.signed.lock().ok()?;
        let (url, at) = signed.remove(key)?;
        (at.elapsed() < FRESH).then_some(url)
    }

    /// Signs `key` and the listing's next unsigned keys in one request; answers `key`'s URL, or
    /// none when the batch was not signed (the caller signs `key` alone).
    fn sign_ahead(&self, key: &str) -> Option<String> {
        let _one = self.signing.lock().ok()?;
        if let Some(url) = self.take(key) {
            return Some(url);
        }
        let mut keys = vec![key.to_string()];
        if let Ok(mut upcoming) = self.upcoming.lock() {
            upcoming.remove(key);
            let mut bytes = key.len();
            let next: Vec<String> = upcoming
                .range::<str, _>((Excluded(key), Unbounded))
                .take_while(|k| {
                    bytes += k.len();
                    bytes <= wire::GET_MANY_KEY_BYTES
                })
                .take(wire::GET_MANY - 1)
                .cloned()
                .collect();
            for k in &next {
                upcoming.remove(k);
            }
            keys.extend(next);
        }
        if !wire::valid_batch(&keys) {
            return None;
        }
        let at = Instant::now();
        let urls = match self.call(&wire::Read::GetMany { keys: keys.clone() }) {
            Ok(body) => serde_json::from_slice::<wire::Downloads>(&body).ok()?.urls,
            // An older Data Worker does not know `get_many`: sign one at a time from now on.
            Err(Some(400)) => {
                self.batches.store(false, Ordering::SeqCst);
                return None;
            }
            Err(_) => return None,
        };
        if urls.len() != keys.len() {
            return None;
        }
        let mut pairs = keys.into_iter().zip(urls);
        let (_, mine) = pairs.next()?;
        if let Ok(mut signed) = self.signed.lock() {
            if signed.len() > CACHE {
                signed.retain(|_, (_, at)| at.elapsed() < FRESH);
            }
            for (k, url) in pairs {
                signed.insert(k, (url, at));
            }
        }
        Some(mine)
    }

    /// A presigned download URL for `key`.
    fn url(&self, key: &str) -> Result<String, Error> {
        if let Some(url) = self.take(key) {
            return Ok(url);
        }
        if self.batches.load(Ordering::SeqCst)
            && let Some(url) = self.sign_ahead(key)
        {
            return Ok(url);
        }
        let body = self.read(wire::Read::Get { key: key.into() })?;
        let download: wire::Download = serde_json::from_slice(&body)
            .map_err(|_| Error::Source("invalid Data Worker source download".into()))?;
        Ok(download.url)
    }
}

impl Source for ManagedSource {
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
        let body = self.read(wire::Read::List {
            after: after.map(str::to_owned),
            cursor: token.map(str::to_owned),
        })?;
        let listing: Listing = serde_json::from_slice(&body)
            .map_err(|_| Error::Source("invalid Data Worker source listing".into()))?;
        if self.batches.load(Ordering::SeqCst)
            && let Ok(mut upcoming) = self.upcoming.lock()
        {
            upcoming.extend(listing.objects.iter().map(|o| o.key.clone()));
        }
        Ok(Page {
            objects: listing
                .objects
                .into_iter()
                .map(|object| Object {
                    key: object.key,
                    size: object.size,
                    etag: Some(object.etag).filter(|value| !value.is_empty()),
                    modified: Some(object.last_modified).filter(|value| !value.is_empty()),
                })
                .collect(),
            next: listing.next,
        })
    }

    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
        let url = self.url(key)?;
        let path = s3::object_path(&self.bucket, key);
        let expected = format!("{}{path}?", self.provider);
        if !url.starts_with(&expected) {
            return Err(Error::Source(
                "the signed download was outside the approved object".into(),
            ));
        }
        let response =
            self.download.get(&url).call().map_err(|_| {
                Error::Source("the source provider refused the signed download".into())
            })?;
        if response.status() != 200 {
            return Err(Error::Source(
                "unexpected response from the source provider".into(),
            ));
        }
        Ok(Box::new(response.into_reader()))
    }
}

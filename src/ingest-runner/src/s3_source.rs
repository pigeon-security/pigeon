//! The S3 source: lists a bucket and streams its objects. It has no write method.

use std::io::Read;

use pigeon_core::s3::{EMPTY_SHA256, Keys, list_objects_query, object_path, parse_list_objects};
use pigeon_ingest::Error;
use pigeon_ingest::engine::{Object, Page, Source};

use crate::client::{Body, Client};

/// A bucket on an S3-compatible endpoint, read through path-style requests.
pub struct S3Source {
    client: Client,
    prefix: String,
}

impl S3Source {
    /// A source for `bucket` at `endpoint`, listing keys under `prefix` (empty for all).
    pub fn new(endpoint: &str, region: &str, bucket: &str, prefix: &str, keys: Keys) -> Self {
        Self {
            client: Client::new(endpoint, region, bucket, keys),
            prefix: prefix.to_string(),
        }
    }
}

impl Source for S3Source {
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
        let mut query = list_objects_query(Some(&self.prefix), None, 1000, token);
        if let (Some(a), None) = (after, token) {
            query.push(("start-after".into(), a.into()));
        }
        let path = object_path(&self.client.bucket, "");
        let res = self
            .client
            .call("GET", &path, &query, &[], EMPTY_SHA256, Body::Empty)
            .map_err(|f| Error::Source(format!("listing failed: {}", f.text)))?;
        let xml = res
            .into_string()
            .map_err(|e| Error::Source(format!("the listing could not be read: {e}")))?;
        let listing = parse_list_objects(&xml).map_err(|e| Error::Source(e.to_string()))?;
        Ok(Page {
            objects: listing
                .objects
                .into_iter()
                .map(|o| Object {
                    key: o.key,
                    size: o.size,
                    etag: Some(o.etag).filter(|e| !e.is_empty()),
                    modified: Some(o.last_modified).filter(|m| !m.is_empty()),
                })
                .collect(),
            next: listing.next,
        })
    }

    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
        let path = object_path(&self.client.bucket, key);
        let res = self
            .client
            .call("GET", &path, &[], &[], EMPTY_SHA256, Body::Empty)
            .map_err(|f| Error::Source(format!("cannot read {key}: {}", f.text)))?;
        Ok(Box::new(res.into_reader()))
    }
}

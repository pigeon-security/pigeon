//! The owning Data Worker authorizes source reads without giving this runner a provider key.

use std::{io::Read, time::Duration};

use pigeon_core::s3::{self, Listing};
use pigeon_ingest::{
    Error,
    engine::{Object, Page, Source},
    managed_destination::valid_origin,
    source_credentials as wire,
};

pub struct ManagedSource {
    signer: String,
    grant: String,
    provider: String,
    bucket: String,
    control: ureq::Agent,
    download: ureq::Agent,
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
            control: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(20))
                .timeout(Duration::from_secs(90))
                .build(),
            download: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(20))
                .timeout_read(Duration::from_secs(90))
                .build(),
        })
    }

    fn read(&self, operation: wire::Read) -> Result<Vec<u8>, Error> {
        let body = serde_json::to_vec(&operation)
            .map_err(|_| Error::Source("cannot encode the source read".into()))?;
        let response = self
            .control
            .post(&self.signer)
            .set("content-type", "application/json")
            .set("authorization", &format!("Bearer {}", self.grant))
            .send_bytes(&body)
            .map_err(|_| Error::Source("the owning Data Worker refused a source read".into()))?;
        if response.status() != 200 {
            return Err(Error::Source(
                "unexpected response from the owning Data Worker".into(),
            ));
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(wire::RESPONSE_MAX as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|_| Error::Source("cannot read the Data Worker's source response".into()))?;
        if body.len() > wire::RESPONSE_MAX {
            return Err(Error::Source(
                "the Data Worker's source response is too large".into(),
            ));
        }
        Ok(body)
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
        let body = self.read(wire::Read::Get { key: key.into() })?;
        let download: wire::Download = serde_json::from_slice(&body)
            .map_err(|_| Error::Source("invalid Data Worker source download".into()))?;
        let path = s3::object_path(&self.bucket, key);
        let expected = format!("{}{path}?", self.provider);
        if !download.url.starts_with(&expected) {
            return Err(Error::Source(
                "the signed download was outside the approved object".into(),
            ));
        }
        let response =
            self.download.get(&download.url).call().map_err(|_| {
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

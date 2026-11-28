//! Transport backed by the instance's Data Worker and an expiring run grant.

use std::{io::Read, time::Duration};

use pigeon_ingest::{
    Error, decide::Head, engine::Dest, managed_destination as wire, metrics::Metrics,
};

pub struct ManagedDest {
    target: String,
    grant: String,
    agent: ureq::Agent,
}

impl ManagedDest {
    pub fn new(dest: wire::Destination) -> Result<Self, Error> {
        if !wire::valid_origin(&dest.origin) || dest.grant.is_empty() || dest.grant.len() > 2048 {
            return Err(Error::Config("invalid managed destination".into()));
        }
        Ok(Self {
            target: format!("{}{}", dest.origin, wire::PATH),
            grant: dest.grant,
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(20))
                .timeout(Duration::from_secs(90))
                .build(),
        })
    }

    fn request(&self, op: &str, key: &str) -> ureq::Request {
        self.agent
            .post(&self.target)
            .set("authorization", &format!("Bearer {}", self.grant))
            .set(wire::OPERATION, op)
            .set(wire::OBJECT, key)
    }

    fn check(result: Result<ureq::Response, ureq::Error>) -> Result<Option<ureq::Response>, Error> {
        match result {
            Ok(reply) if matches!(reply.status(), 200 | 201) => Ok(Some(reply)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(ureq::Error::Status(status, _)) => Err(Error::Dest(format!(
                "managed destination refused HTTP {status}"
            ))),
            _ => Err(Error::Dest("managed destination request failed".into())),
        }
    }

    fn read(reply: ureq::Response, limit: u64) -> Result<Vec<u8>, Error> {
        let mut body = Vec::new();
        reply
            .into_reader()
            .take(limit + 1)
            .read_to_end(&mut body)
            .map_err(|_| Error::Dest("managed destination reply failed".into()))?;
        if body.len() as u64 > limit {
            return Err(Error::Dest("managed destination reply too large".into()));
        }
        Ok(body)
    }
}

impl Dest for ManagedDest {
    fn head(&self, key: &str) -> Result<Option<Head>, Error> {
        Self::check(self.request("head", key).call())?
            .map(|reply| {
                serde_json::from_slice(&Self::read(reply, 1024)?)
                    .map_err(|_| Error::Dest("invalid managed payload identity".into()))
            })
            .transpose()
    }

    fn put_blob(
        &self,
        key: &str,
        body: &mut dyn Read,
        size: u64,
        metrics: &Metrics,
    ) -> Result<(), Error> {
        if size > wire::MAX_BLOB {
            return Err(Error::Dest(
                "object exceeds the managed atomic write limit".into(),
            ));
        }
        let mut bytes = Vec::new();
        body.take(size + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Dest("could not verify managed payload bytes".into()))?;
        if bytes.len() as u64 != size {
            return Err(Error::Dest(
                "managed payload has an unexpected length".into(),
            ));
        }
        Self::check(
            self.request("put-blob", key)
                .set(wire::SHA256, &metrics.sha256)
                .set(wire::BLAKE3, &metrics.blake3)
                .send_bytes(&bytes),
        )?
        .ok_or_else(|| Error::Dest("managed payload route is unavailable".into()))?;
        Ok(())
    }

    fn put_object(&self, key: &str, body: &[u8], _content_type: &str) -> Result<(), Error> {
        if body.len() as u64 > wire::MAX_CONTROL {
            return Err(Error::Dest("control object exceeds managed limit".into()));
        }
        Self::check(self.request("put-object", key).send_bytes(body))?
            .ok_or_else(|| Error::Dest("managed control route is unavailable".into()))?;
        Ok(())
    }

    fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        Self::check(self.request("get", key).call())?
            .map(|reply| Self::read(reply, wire::MAX_CONTROL))
            .transpose()
    }
}

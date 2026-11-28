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
                .max_idle_connections(256)
                .max_idle_connections_per_host(128)
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

    fn stage_request(
        &self,
        op: &str,
        key: &str,
        sha: &str,
        field: Option<(&str, &str)>,
        bytes: &[u8],
    ) -> Result<String, Error> {
        for attempt in 0..3 {
            let mut request = self.request(op, key).set(wire::SHA256, sha);
            if let Some((name, value)) = field {
                request = request.set(name, value);
            }
            match request.send_bytes(bytes) {
                Ok(response) if response.status() == 200 => {
                    let malformed = || Error::Dest("invalid managed multipart receipt".into());
                    let receipt: serde_json::Value =
                        serde_json::from_slice(&Self::read(response, 1024)?)
                            .map_err(|_| malformed())?;
                    return receipt["state"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(malformed);
                }
                Err(ureq::Error::Status(409 | 503, _)) | Err(ureq::Error::Transport(_))
                    if attempt < 2 =>
                {
                    std::thread::sleep(Duration::from_millis(250 << attempt));
                }
                Err(ureq::Error::Status(status, _)) => {
                    return Err(Error::Dest(format!(
                        "managed multipart refused HTTP {status}"
                    )));
                }
                _ => return Err(Error::Dest("managed multipart request failed".into())),
            }
        }
        Err(Error::Dest("managed multipart request failed".into()))
    }

    fn put_multipart(
        &self,
        key: &str,
        body: &mut dyn Read,
        size: u64,
        sha: &str,
    ) -> Result<(), Error> {
        use pigeon_ingest::immutable_multipart::{MAX_BYTES, PART_BYTES};

        if size > MAX_BYTES || key != pigeon_ingest::blob_key(sha) {
            return Err(Error::Dest(
                "payload is outside managed multipart limits".into(),
            ));
        }
        let status = self.stage_request(
            "multipart-start",
            key,
            sha,
            Some(("x-ingest-size", &size.to_string())),
            &[],
        )?;
        if status == "published" {
            self.stage_request("multipart-complete", key, sha, None, &[])?;
            return Err(Error::Exists);
        }
        let mut bytes = vec![0; PART_BYTES as usize];
        for part_number in 1..=size.div_ceil(PART_BYTES) {
            let length = (size - (part_number - 1) * PART_BYTES).min(PART_BYTES) as usize;
            let part = &mut bytes[..length];
            body.read_exact(part)
                .map_err(|_| Error::Dest("could not read a managed multipart payload".into()))?;
            self.stage_request(
                "multipart-part",
                key,
                sha,
                Some(("x-ingest-part", &part_number.to_string())),
                part,
            )?;
        }
        if body
            .read(&mut [0])
            .map_err(|_| Error::Dest("could not verify managed payload length".into()))?
            != 0
        {
            return Err(Error::Dest(
                "managed payload exceeds its expected length".into(),
            ));
        }
        self.stage_request("multipart-complete", key, sha, None, &[])?;
        Ok(())
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
            return self.put_multipart(key, body, size, &metrics.sha256);
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
        let reply = Self::check(
            self.request("put-blob", key)
                .set(wire::SHA256, &metrics.sha256)
                .set(wire::BLAKE3, &metrics.blake3)
                .send_bytes(&bytes),
        )?
        .ok_or_else(|| Error::Dest("managed payload route is unavailable".into()))?;
        // The write is create-only: `created: false` is the same blob already stored.
        let status = reply.status();
        let receipt: serde_json::Value =
            serde_json::from_slice(&Self::read(reply, 1024)?).unwrap_or_default();
        if status == 200 && receipt["created"] == false {
            return Err(Error::Exists);
        }
        Ok(())
    }

    fn head_many(&self, keys: &[String]) -> Result<Vec<Option<Head>>, Error> {
        if keys.is_empty() || keys.len() > wire::HEAD_MANY {
            return Err(Error::Dest("invalid managed existence batch".into()));
        }
        let body = serde_json::to_vec(&wire::HeadMany {
            keys: keys.to_vec(),
        })
        .map_err(|_| Error::Dest("invalid managed existence batch".into()))?;
        let reply = Self::check(self.request("head-many", "").send_bytes(&body))?
            .ok_or_else(|| Error::Dest("managed existence batch is unavailable".into()))?;
        let answer: wire::HeadManyReply =
            serde_json::from_slice(&Self::read(reply, wire::HEAD_MANY_REPLY)?)
                .map_err(|_| Error::Dest("invalid managed existence batch reply".into()))?;
        if answer.heads.len() != keys.len() {
            return Err(Error::Dest("invalid managed existence batch reply".into()));
        }
        Ok(answer.heads)
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

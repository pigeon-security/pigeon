//! A blocking, signed S3 client shared by the source and the destination. It reuses
//! `pigeon_core::s3` for signing and XML. Keys and signed headers are never put in an error.

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pigeon_core::s3::{Endpoint, Keys, amz_date, parse_error, sign, uri_encode};

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Why a call failed: the HTTP status when there was one, and a text with no secret in it.
#[derive(Debug)]
pub(crate) struct Failure {
    pub status: Option<u16>,
    pub text: String,
}

/// What a call sends as its body.
pub(crate) enum Body<'a> {
    /// No body.
    Empty,
    /// Bytes in memory.
    Bytes(&'a [u8]),
    /// `len` bytes read from a stream; sent with a `Content-Length`.
    Stream(&'a mut dyn Read, u64),
}

/// A bucket on an S3-compatible endpoint, with its keys.
pub(crate) struct Client {
    pub endpoint: Endpoint,
    pub bucket: String,
    keys: Keys,
    agent: ureq::Agent,
}

impl Client {
    /// A client for `bucket` at `base` (no trailing `/`), signing for `region`.
    pub fn new(base: &str, region: &str, bucket: &str, keys: Keys) -> Self {
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(300))
            .timeout_write(Duration::from_secs(300))
            // Keep connections for the engine's worker threads (docs/plans/ingest-throughput.md).
            .max_idle_connections(256)
            .max_idle_connections_per_host(128)
            .build();
        Self {
            endpoint: Endpoint {
                base: base.trim_end_matches('/').to_string(),
                region: region.to_string(),
            },
            bucket: bucket.to_string(),
            keys,
            agent,
        }
    }

    /// Sends one signed request. A status of 400 or more is a [`Failure`].
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        headers: &[(String, String)],
        payload_sha256: &str,
        body: Body<'_>,
    ) -> Result<ureq::Response, Failure> {
        let date = amz_date(now_ms());
        let signed = sign(
            method,
            &self.endpoint,
            path,
            query,
            headers,
            payload_sha256,
            &self.keys,
            &date,
        );
        let mut url = format!("{}{path}", self.endpoint.base);
        if !query.is_empty() {
            let mut pairs: Vec<(String, String)> = query
                .iter()
                .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
                .collect();
            pairs.sort();
            let q: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            url.push('?');
            url.push_str(&q.join("&"));
        }
        let mut req = self.agent.request(method, &url);
        for (k, v) in signed.iter().filter(|(k, _)| k != "host") {
            req = req.set(k, v);
        }
        let result = match body {
            Body::Empty => req.call(),
            Body::Bytes(b) => req
                .set("content-length", &b.len().to_string())
                .send_bytes(b),
            Body::Stream(r, len) => req.set("content-length", &len.to_string()).send(r),
        };
        match result {
            Ok(res) => Ok(res),
            Err(ureq::Error::Status(code, res)) => {
                let mut text = String::new();
                let _ = res.into_reader().take(4096).read_to_string(&mut text);
                Err(Failure {
                    status: Some(code),
                    text: parse_error(code, &text),
                })
            }
            Err(ureq::Error::Transport(t)) => Err(Failure {
                status: None,
                text: format!("{method} {} failed: {}", self.endpoint.host(), t.kind()),
            }),
        }
    }
}

/// A reader that gives exactly `left` bytes of `inner`. A short stream is an error. When
/// `check_end` is set it then reads once more, so a body that fails its check at its end
/// (see `engine::Verify`) fails the request, and a longer stream is an error.
pub(crate) struct Exact<'a> {
    pub inner: &'a mut dyn Read,
    pub left: u64,
    pub check_end: bool,
}

impl Read for Exact<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            if self.check_end {
                let mut one = [0u8; 1];
                if self.inner.read(&mut one)? != 0 {
                    return Err(std::io::Error::other("the body is longer than its size"));
                }
                self.check_end = false;
            }
            return Ok(0);
        }
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::Error::other("the body is shorter than its size"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

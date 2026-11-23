//! The R2 destination: HEAD, single and multipart PUT, and small objects, over R2's S3 API.

use std::io::Read;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pigeon_core::s3::{EMPTY_SHA256, UNSIGNED_PAYLOAD, object_path};
use pigeon_ingest::Error;
use pigeon_ingest::decide::Head;
use pigeon_ingest::engine::Dest;
use pigeon_ingest::metrics::Metrics;
use sha2::{Digest, Sha256};

use crate::client::{Body, Client, Exact, hex};

/// Default size above which a blob goes multipart: 1 GiB.
const DEFAULT_THRESHOLD: u64 = 1024 * 1024 * 1024;
/// Default part size: 128 MiB.
const DEFAULT_PART: u64 = 128 * 1024 * 1024;
/// The most parts S3 allows.
const MAX_PARTS: u64 = 10_000;
/// The largest small object `get_object` reads: 64 MiB.
const SMALL_MAX: u64 = 64 * 1024 * 1024;

/// An R2 bucket, written through its S3 API (region `auto`).
pub struct R2Dest {
    client: Client,
    threshold: u64,
    part_size: u64,
}

impl R2Dest {
    /// A destination for `bucket` at `endpoint`, with the default multipart settings.
    pub fn new(endpoint: &str, bucket: &str, keys: pigeon_core::s3::Keys) -> Self {
        Self {
            client: Client::new(endpoint, "auto", bucket, keys),
            threshold: DEFAULT_THRESHOLD,
            part_size: DEFAULT_PART,
        }
    }

    /// Blobs larger than `threshold` bytes go multipart in parts of `part_size` bytes.
    pub fn with_multipart(mut self, threshold: u64, part_size: u64) -> Self {
        self.threshold = threshold;
        self.part_size = part_size.max(1);
        self
    }

    fn fail(what: &str, f: crate::client::Failure) -> Error {
        Error::Dest(format!("{what}: {}", f.text))
    }

    fn meta_headers(size: u64, m: &Metrics) -> Vec<(String, String)> {
        let mime = if !m.mime.is_empty() && m.mime.bytes().all(|b| (0x21..0x7f).contains(&b)) {
            m.mime.as_str()
        } else {
            "application/octet-stream"
        };
        vec![
            ("x-amz-meta-sha256".into(), m.sha256.clone()),
            ("x-amz-meta-blake3".into(), m.blake3.clone()),
            ("x-amz-meta-size".into(), size.to_string()),
            ("x-amz-meta-mime".into(), mime.to_string()),
            ("content-type".into(), mime.to_string()),
        ]
    }

    fn multipart(
        &self,
        path: &str,
        body: &mut dyn Read,
        size: u64,
        headers: &[(String, String)],
    ) -> Result<(), Error> {
        let c = &self.client;
        let q = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect()
        };
        let res = c
            .call(
                "POST",
                path,
                &q(&[("uploads", "")]),
                headers,
                EMPTY_SHA256,
                Body::Empty,
            )
            .map_err(|f| Self::fail("cannot start a multipart upload", f))?;
        let xml = res
            .into_string()
            .map_err(|e| Error::Dest(format!("the multipart answer could not be read: {e}")))?;
        let id = xml
            .split_once("<UploadId>")
            .and_then(|(_, r)| r.split_once("</UploadId>"))
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| Error::Dest("the multipart answer has no upload id".into()))?;
        let done = self.upload_parts(path, &id, body, size);
        if done.is_err() {
            let _ = c.call(
                "DELETE",
                path,
                &q(&[("uploadId", &id)]),
                &[],
                EMPTY_SHA256,
                Body::Empty,
            );
        }
        done
    }

    fn upload_parts(
        &self,
        path: &str,
        id: &str,
        body: &mut dyn Read,
        size: u64,
    ) -> Result<(), Error> {
        let c = &self.client;
        let part = self.part_size.max(size.div_ceil(MAX_PARTS)).max(1);
        let mut etags = Vec::new();
        let mut left = size;
        let mut number = 1u64;
        while left > 0 {
            let len = left.min(part);
            let query = vec![
                ("partNumber".to_string(), number.to_string()),
                ("uploadId".to_string(), id.to_string()),
            ];
            let mut reader = Exact {
                inner: body,
                left: len,
                check_end: len == left,
            };
            let res = c
                .call(
                    "PUT",
                    path,
                    &query,
                    &[],
                    UNSIGNED_PAYLOAD,
                    Body::Stream(&mut reader, len),
                )
                .map_err(|f| Self::fail(&format!("part {number} failed"), f))?;
            let etag = res
                .header("etag")
                .ok_or_else(|| Error::Dest(format!("part {number} came back with no ETag")))?
                .to_string();
            etags.push(etag);
            left -= len;
            number += 1;
        }
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (i, e) in etags.iter().enumerate() {
            xml.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{e}</ETag></Part>",
                i + 1
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let query = vec![("uploadId".to_string(), id.to_string())];
        let res = c
            .call(
                "POST",
                path,
                &query,
                &[("content-type".to_string(), "application/xml".to_string())],
                &hex(&Sha256::digest(xml.as_bytes())),
                Body::Bytes(xml.as_bytes()),
            )
            .map_err(|f| Self::fail("cannot complete the multipart upload", f))?;
        // S3 may answer 200 with an error in the body.
        let answer = res.into_string().unwrap_or_default();
        if answer.contains("<Error>") {
            return Err(Error::Dest(
                "the multipart upload was refused at its end".into(),
            ));
        }
        Ok(())
    }
}

impl Dest for R2Dest {
    fn head(&self, key: &str) -> Result<Option<Head>, Error> {
        let path = object_path(&self.client.bucket, key);
        match self
            .client
            .call("HEAD", &path, &[], &[], EMPTY_SHA256, Body::Empty)
        {
            Ok(res) => {
                let size = res
                    .header("content-length")
                    .and_then(|v| v.trim().parse().ok())
                    .ok_or_else(|| Error::Dest("HEAD answered without a content-length".into()))?;
                Ok(Some(Head {
                    size,
                    sha256: res.header("x-amz-meta-sha256").map(str::to_string),
                    blake3: res.header("x-amz-meta-blake3").map(str::to_string),
                }))
            }
            Err(f) if f.status == Some(404) => Ok(None),
            Err(f) => Err(Self::fail("HEAD failed", f)),
        }
    }

    fn put_blob(
        &self,
        key: &str,
        body: &mut dyn Read,
        size: u64,
        metrics: &Metrics,
    ) -> Result<(), Error> {
        let path = object_path(&self.client.bucket, key);
        let mut headers = Self::meta_headers(size, metrics);
        if size > 0 && size > self.threshold {
            return self.multipart(&path, body, size, &headers);
        }
        let digest = (0..32)
            .map(|i| {
                metrics
                    .sha256
                    .get(i * 2..i * 2 + 2)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .filter(|_| metrics.sha256.len() == 64)
            .ok_or_else(|| Error::Dest("the SHA-256 is not 64 hex characters".into()))?;
        headers.push(("x-amz-checksum-sha256".into(), STANDARD.encode(digest)));
        let mut reader = Exact {
            inner: body,
            left: size,
            check_end: true,
        };
        self.client
            .call(
                "PUT",
                &path,
                &[],
                &headers,
                UNSIGNED_PAYLOAD,
                Body::Stream(&mut reader, size),
            )
            .map(|_| ())
            .map_err(|f| Self::fail(&format!("cannot write {key}"), f))
    }

    fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), Error> {
        let path = object_path(&self.client.bucket, key);
        self.client
            .call(
                "PUT",
                &path,
                &[],
                &[("content-type".to_string(), content_type.to_string())],
                &hex(&Sha256::digest(body)),
                Body::Bytes(body),
            )
            .map(|_| ())
            .map_err(|f| Self::fail(&format!("cannot write {key}"), f))
    }

    fn get_object(&self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let path = object_path(&self.client.bucket, key);
        match self
            .client
            .call("GET", &path, &[], &[], EMPTY_SHA256, Body::Empty)
        {
            Ok(res) => {
                let mut out = Vec::new();
                res.into_reader()
                    .take(SMALL_MAX + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| Error::Dest(format!("cannot read {key}: {e}")))?;
                if out.len() as u64 > SMALL_MAX {
                    return Err(Error::Dest(format!("{key} is too large to read whole")));
                }
                Ok(Some(out))
            }
            Err(f) if f.status == Some(404) => Ok(None),
            Err(f) => Err(Self::fail(&format!("cannot read {key}"), f)),
        }
    }
}

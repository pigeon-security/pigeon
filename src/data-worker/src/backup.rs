//! Copies immutable versions of Data objects into an independent B2 repository.
use futures_util::StreamExt;
use pigeon_core::{s3, webauthn};
use pigeon_ingest::datastore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use worker::{
    Conditional, Env, Fetch, Headers, Method, Range, Request, RequestInit, RequestRedirect,
    Response, Result,
};

const PART_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PARTS: u64 = 10_000;

#[derive(Deserialize)]
struct Backup {
    endpoint: String,
    region: String,
    bucket: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Copy {
    pub size: u64,
    pub stored: bool,
}

fn fail(reason: impl Into<String>) -> worker::Error {
    worker::Error::RustError(reason.into())
}

struct Target {
    endpoint: s3::Endpoint,
    bucket: String,
    keys: s3::Keys,
}

impl Target {
    async fn open(env: &Env) -> Result<Self> {
        let config: Backup = serde_json::from_str(&env.var("BACKUP")?.to_string())
            .map_err(|_| fail("BACKUP is not set"))?;
        if config.endpoint.trim_end_matches('/')
            != format!("https://s3.{}.backblazeb2.com", config.region)
            || config.region.is_empty()
            || !config
                .region
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !(3..=63).contains(&config.bucket.len())
            || !config
                .bucket
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(fail("the backup destination is invalid"));
        }
        let secret = env
            .secret_store("B2_KEYS")?
            .get()
            .await?
            .ok_or_else(|| fail("the backup key is not set"))?;
        Ok(Self {
            endpoint: s3::Endpoint {
                base: config.endpoint.trim_end_matches('/').into(),
                region: config.region,
            },
            bucket: config.bucket,
            keys: s3::parse_keys(&secret)?,
        })
    }

    async fn request(
        &self,
        method: Method,
        key: &str,
        query: &[(String, String)],
        extra: &[(String, String)],
        body: Option<&[u8]>,
    ) -> Result<Response> {
        let path = s3::object_path(&self.bucket, key);
        let date = s3::amz_date(worker::js_sys::Date::now());
        let signed = s3::sign(
            method.as_ref(),
            &self.endpoint,
            &path,
            query,
            extra,
            &datastore::sha256_hex(body.unwrap_or_default()),
            &self.keys,
            &date,
        );
        let headers = Headers::new();
        for (k, v) in signed.iter().filter(|(k, _)| k != "host") {
            headers.set(k, v)?;
        }
        let mut req = RequestInit::new();
        req.with_method(method)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error);
        if let Some(body) = body {
            req.with_body(Some(worker::js_sys::Uint8Array::from(body).into()));
        }
        let mut url = format!("{}{path}", self.endpoint.base);
        if !query.is_empty() {
            let mut pairs = query
                .iter()
                .map(|(k, v)| (s3::uri_encode(k, true), s3::uri_encode(v, true)))
                .collect::<Vec<_>>();
            pairs.sort();
            url.push('?');
            url.push_str(
                &pairs
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        Fetch::Request(Request::new_with_init(&url, &req)?)
            .send()
            .await
    }

    async fn require_restricted_key(&self) -> Result<()> {
        let credentials = format!(
            "{}:{}",
            self.keys.access_key_id, self.keys.secret_access_key
        );
        let headers = Headers::new();
        headers.set(
            "authorization",
            &format!("Basic {}", pigeon_core::git::base64(credentials.as_bytes())),
        )?;
        let mut init = RequestInit::new();
        init.with_method(Method::Get)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error);
        let mut res = Fetch::Request(Request::new_with_init(
            "https://api.backblazeb2.com/b2api/v4/b2_authorize_account",
            &init,
        )?)
        .send()
        .await?;
        require_ok(&res)?;
        let result: serde_json::Value = res.json().await?;
        let access = &result["apiInfo"]["storageApi"];
        if access["s3ApiUrl"].as_str() != Some(self.endpoint.base.as_str())
            || !datastore::backup_key_scope(&access["allowed"], &self.bucket)
        {
            return Err(fail(
                "the B2 backup credential must have only the approved bucket and backup permissions",
            ));
        }
        Ok(())
    }

    async fn require_private(&self) -> Result<()> {
        let mut res = self
            .request(Method::Get, "", &[("acl".into(), String::new())], &[], None)
            .await?;
        require_ok(&res)?;
        if !datastore::backup_is_private(&res.text().await?) {
            return Err(fail("the B2 backup bucket is not private"));
        }
        Ok(())
    }

    async fn require_governance(&self) -> Result<()> {
        let mut res = self
            .request(
                Method::Get,
                "",
                &[("object-lock".into(), String::new())],
                &[],
                None,
            )
            .await?;
        require_ok(&res)?;
        if !datastore::backup_governance(&res.text().await?) {
            return Err(fail(
                "the backup bucket must have default Governance Object Lock retention",
            ));
        }
        Ok(())
    }

    async fn exists(&self, key: &str, sha: &str, size: u64) -> Result<bool> {
        let res = self.request(Method::Head, key, &[], &[], None).await?;
        if res.status_code() == 404 {
            return Ok(false);
        }
        require_ok(&res)?;
        let headers = res.headers();
        let checksum = headers.get("x-amz-meta-sha256")?;
        let length = headers
            .get("content-length")?
            .and_then(|n| n.parse::<u64>().ok());
        if checksum.as_deref() != Some(sha) || length != Some(size) {
            return Err(fail(
                "an existing backup at this object key has a different checksum or size",
            ));
        }
        let mut retention = self
            .request(
                Method::Get,
                key,
                &[("retention".into(), String::new())],
                &[],
                None,
            )
            .await?;
        require_ok(&retention)?;
        let retention = retention.text().await?;
        if !datastore::backup_retention(&retention)
            .is_some_and(|date| worker::js_sys::Date::parse(date) > worker::js_sys::Date::now())
        {
            return Err(fail("the backup object has no active retention lock"));
        }
        Ok(true)
    }

    async fn write(&self, source: &Source<'_>, key: &str, sha: &str) -> Result<()> {
        let metadata = [
            ("x-amz-meta-sha256".into(), sha.to_string()),
            (
                "x-amz-meta-pigeon-r2-key".into(),
                webauthn::b64url(source.key.as_bytes()),
            ),
            ("x-amz-acl".into(), "private".into()),
            ("x-amz-server-side-encryption".into(), "AES256".into()),
        ];
        if source.size <= PART_BYTES {
            let bytes = source.read(0, source.size).await?;
            if datastore::sha256_hex(&bytes) != sha {
                return Err(fail("the Data object changed while copying"));
            }
            return require_ok(
                &self
                    .request(Method::Put, key, &[], &metadata, Some(&bytes))
                    .await?,
            );
        }

        let mut res = self
            .request(
                Method::Post,
                key,
                &[("uploads".into(), String::new())],
                &metadata,
                None,
            )
            .await?;
        require_ok(&res)?;
        let xml = res.text().await?;
        let upload_id =
            object_id(&xml).ok_or_else(|| fail("B2 did not return a multipart upload ID"))?;
        let copied = self.write_parts(source, key, sha, upload_id).await;
        if copied.is_err() {
            // Release only this unfinished multipart upload. The backup key is never deleted.
            let _ = self
                .request(
                    Method::Delete,
                    key,
                    &[("uploadId".into(), upload_id.into())],
                    &[],
                    None,
                )
                .await;
        }
        copied
    }

    async fn write_parts(
        &self,
        source: &Source<'_>,
        key: &str,
        sha: &str,
        upload_id: &str,
    ) -> Result<()> {
        let mut manifest = String::from(
            "<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        );
        let mut digest = Sha256::new();
        for (i, start) in (0..source.size).step_by(PART_BYTES as usize).enumerate() {
            let number = (i + 1).to_string();
            let bytes = source
                .read(start, (source.size - start).min(PART_BYTES))
                .await?;
            digest.update(&bytes);
            let query = [
                ("partNumber".into(), number.clone()),
                ("uploadId".into(), upload_id.into()),
            ];
            let res = self
                .request(Method::Put, key, &query, &[], Some(&bytes))
                .await?;
            require_ok(&res)?;
            let etag = res
                .headers()
                .get("etag")?
                .filter(|v| {
                    v.len() == 34
                        && v.starts_with('"')
                        && v.ends_with('"')
                        && v[1..33].bytes().all(|c| c.is_ascii_hexdigit())
                })
                .ok_or_else(|| fail("B2 did not return a multipart ETag"))?;
            manifest.push_str(&format!(
                "<Part><ETag>{etag}</ETag><PartNumber>{number}</PartNumber></Part>"
            ));
        }
        if format!("{:x}", digest.finalize()) != sha {
            return Err(fail("the Data object changed while copying"));
        }
        manifest.push_str("</CompleteMultipartUpload>");
        let mut res = self
            .request(
                Method::Post,
                key,
                &[("uploadId".into(), upload_id.into())],
                &[("content-type".into(), "application/xml".into())],
                Some(manifest.as_bytes()),
            )
            .await?;
        require_ok(&res)?;
        if !res.text().await?.contains("<CompleteMultipartUploadResult") {
            return Err(fail("B2 did not confirm multipart completion"));
        }
        Ok(())
    }
}

fn require_ok(response: &Response) -> Result<()> {
    if (200..300).contains(&response.status_code()) {
        Ok(())
    } else {
        Err(fail(format!(
            "B2 refused the backup request ({})",
            response.status_code()
        )))
    }
}

fn object_id(xml: &str) -> Option<&str> {
    let id = xml.split_once("<UploadId>")?.1.split_once("</UploadId>")?.0;
    (!id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'=')))
    .then_some(id)
}

struct Source<'a> {
    bucket: worker::Bucket,
    key: &'a str,
    etag: String,
    size: u64,
}

impl Source<'_> {
    async fn read(&self, offset: u64, length: u64) -> Result<Vec<u8>> {
        let mut get = self.bucket.get(self.key).only_if(Conditional {
            etag_matches: Some(self.etag.clone()),
            ..Default::default()
        });
        if length > 0 {
            get = get.range(Range::OffsetWithLength { offset, length });
        }
        let object = get
            .execute()
            .await?
            .ok_or_else(|| fail("the Data object disappeared while copying"))?;
        let bytes = object
            .body()
            .ok_or_else(|| fail("the Data object changed while copying"))?
            .bytes()
            .await?;
        if bytes.len() as u64 != length {
            return Err(fail("the Data object returned an unexpected part size"));
        }
        Ok(bytes)
    }
}

/// Returns None for an object already removed from Data. There is no delete on the B2 side.
pub async fn copy(env: &Env, key: &str, etag: Option<&str>) -> Result<Option<Copy>> {
    if key.is_empty() || key.len() > 1024 {
        return Err(fail(
            "the Data object key cannot be preserved in backup metadata",
        ));
    }
    if env.var("BACKUP_AUDIT_REQUIRED").is_ok() {
        env.bucket("JOBS_AUDIT")?;
    }
    let bucket = env.bucket("DATA")?;
    let Some(object) = bucket.get(key).execute().await? else {
        return Ok(None);
    };
    if etag.is_some_and(|etag| object.etag() != etag) {
        // The normal R2 notification carries any replacement. Avoid copying older inventory
        // work against a different object version.
        return Ok(None);
    }
    let source = Source {
        bucket,
        key,
        size: object.size(),
        etag: object.etag(),
    };
    if source.size.div_ceil(PART_BYTES) > MAX_PARTS {
        return Err(fail("the Data object exceeds the backup multipart limit"));
    }

    let target = Target::open(env).await?;
    target.require_restricted_key().await?;
    target.require_private().await?;
    target.require_governance().await?;
    let mut digest = Sha256::new();
    let mut read = 0;
    let mut body = object
        .body()
        .ok_or_else(|| fail("the Data object has no body"))?
        .stream()?;
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        read += chunk.len() as u64;
        digest.update(&chunk);
    }
    if read != source.size {
        return Err(fail("the Data object size changed while hashing"));
    }
    let sha = format!("{:x}", digest.finalize());
    if key.starts_with("blobs/sha256/") && key != pigeon_ingest::blob_key(&sha) {
        return Err(fail("the Data blob does not match the checksum in its key"));
    }

    let destination = datastore::backup_object_key(source.key, &sha);
    if target.exists(&destination, &sha, source.size).await? {
        return Ok(Some(Copy {
            size: source.size,
            stored: false,
        }));
    }
    target.write(&source, &destination, &sha).await?;
    if !target.exists(&destination, &sha, source.size).await? {
        return Err(fail("B2 did not confirm the protected backup object"));
    }
    Ok(Some(Copy {
        size: source.size,
        stored: true,
    }))
}

/// Export one redacted terminal receipt for an R2 create event, without exposing object keys.
pub async fn audit(
    env: &Env,
    event: &str,
    started_ms: u64,
    result: &Result<Option<Copy>>,
) -> Result<()> {
    use pigeon_ingest::audit::{PipelineCounts, PipelineKind, PipelineOutcome, PipelineTerminal};

    let Ok(bucket) = env.bucket("JOBS_AUDIT") else {
        // Existing home backups do not support instance Jobs.
        return Ok(());
    };
    let instance = env.var("INSTANCE")?.to_string();
    let mut counts = PipelineCounts {
        examined: 1,
        ..Default::default()
    };
    let outcome = match result {
        Ok(Some(copy)) => {
            counts.bytes_read = copy.size;
            if copy.stored {
                counts.stored = 1;
                counts.bytes_written = copy.size;
            } else {
                counts.duplicates = 1;
            }
            PipelineOutcome::Succeeded
        }
        _ => {
            counts.failed = 1;
            PipelineOutcome::Failed
        }
    };
    let terminal = PipelineTerminal {
        run_id: datastore::sha256_hex(event.as_bytes()),
        started_ms,
        finished_ms: (worker::js_sys::Date::now() as u64).max(started_ms),
        outcome,
        cleanup: None,
        counts,
        estimated_usd_micros: None,
    };
    let record =
        pigeon_ingest::audit::terminal_object(&instance, PipelineKind::BackblazeBackup, &terminal)
            .ok_or_else(|| fail("the backup audit receipt is invalid"))?;
    bucket
        .put(&record.key, record.json)
        .http_metadata(worker::HttpMetadata {
            content_type: Some("application/json".into()),
            ..Default::default()
        })
        .execute()
        .await?;
    Ok(())
}

/// Fan out one inventory page and its continuation only after R2 lists it successfully.
pub async fn scan(env: &Env, generation: &str, cursor: Option<String>) -> Result<()> {
    use pigeon_ingest::backup_inventory;
    let bucket = env.bucket("DATA")?;
    let mut list = bucket.list().limit(backup_inventory::PAGE_SIZE);
    if let Some(cursor) = &cursor {
        list = list.cursor(cursor);
    }
    let listed = list.execute().await?;
    let next = if listed.truncated() {
        Some(
            listed
                .cursor()
                .ok_or_else(|| fail("R2 omitted the backup inventory continuation"))?,
        )
    } else {
        None
    };
    let entries = listed
        .objects()
        .into_iter()
        .map(|o| (o.key(), o.etag()))
        .collect();
    let messages = backup_inventory::next(generation, cursor.as_deref(), entries, next)
        .ok_or_else(|| fail("R2 returned an invalid backup inventory page"))?;
    if !messages.is_empty() {
        env.queue("BACKUP_QUEUE")?.send_batch(messages).await?;
    }
    Ok(())
}

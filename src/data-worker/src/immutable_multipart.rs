//! Worker-owned staging and atomic publication of an admitted Data blob.
//!
//! The R2 binding handles upload creation and part writes. Only the signed R2 S3 completion can
//! publish it, with a mandatory create-only condition. The client never receives an upload ID
//! or an S3 credential.

use std::collections::HashMap;
use std::future::Future;

use pigeon_core::{git::base64, s3, sign::hmac_sha256, webauthn};
use pigeon_ingest::datastore;
use pigeon_ingest::immutable_multipart::{
    CREATE_ONLY, Checkpoint, PartAction, Reconciliation, Write, reconcile,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::{
    Bucket, D1Database, Env, Error, Fetch, Headers, Method, Request, RequestInit, RequestRedirect,
    Response, Result,
};

#[derive(Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Saved {
    Starting { write: Write },
    Open { id: String, checkpoint: Checkpoint },
    Published { write: Write },
    Closed { write: Write },
}

impl Saved {
    fn write(&self) -> &Write {
        match self {
            Self::Starting { write } | Self::Published { write } | Self::Closed { write } => write,
            Self::Open { checkpoint, .. } => &checkpoint.write,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    account: String,
    bucket: String,
    keys: String,
}

struct Publisher {
    account: String,
    endpoint: s3::Endpoint,
    bucket: String,
    keys: s3::Keys,
}

impl Publisher {
    async fn open(env: &Env) -> Result<Self> {
        // Account and bucket are derived from the provisioned binding by the instance planner.
        // The credential remains in the same account's Worker secret store.
        let account = env.var("INGEST_R2_ACCOUNT")?.to_string();
        let bucket = env.var("INGEST_R2_BUCKET")?.to_string();
        let raw = env
            .secret_store("INGEST_R2_PUBLISH")?
            .get()
            .await?
            .ok_or_else(|| fail("the Data Worker has no multipart publishing credential"))?;
        let fingerprint = env.var("INGEST_R2_PUBLISH_SHA256")?.to_string();
        if datastore::sha256_hex(raw.as_bytes()) != fingerprint {
            return Err(fail("the multipart publishing credential changed"));
        }
        let credential: Credential = serde_json::from_str(&raw)
            .map_err(|_| fail("invalid multipart publishing credential"))?;
        if credential.account != account
            || credential.bucket != bucket
            || account.len() != 32
            || !account
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || !(3..=63).contains(&bucket.len())
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(fail(
                "the multipart publisher does not match the approved Data bucket",
            ));
        }
        Ok(Self {
            account: account.clone(),
            endpoint: s3::Endpoint {
                base: format!("https://{account}.r2.cloudflarestorage.com"),
                region: "auto".into(),
            },
            bucket,
            keys: s3::parse_keys(&credential.keys)?,
        })
    }

    /// The parent secret stays in the Worker. Cloudflare validates this temporary credential's
    /// exact object key and S3 action in addition to the atomic condition on the request.
    fn completion_credential(&self, key: &str, now_seconds: i64) -> (s3::Keys, String) {
        let claims = json!({
            "sub": self.account,
            "iss": self.keys.access_key_id,
            "aud": self.endpoint.host(),
            "iat": now_seconds,
            "exp": now_seconds + 300,
            "bucket": self.bucket,
            "actions": ["CompleteMultipartUpload"],
            "paths": {"prefixPaths": [], "objectPaths": [key]},
        });
        let body = format!(
            "{}.{}",
            webauthn::b64url(br#"{"typ":"JWT","alg":"HS256"}"#),
            webauthn::b64url(claims.to_string().as_bytes()),
        );
        let signature = hmac_sha256(self.keys.secret_access_key.as_bytes(), body.as_bytes());
        let jwt = format!("{body}.{}", webauthn::b64url(&signature));
        (
            s3::Keys {
                access_key_id: self.keys.access_key_id.clone(),
                secret_access_key: datastore::sha256_hex(jwt.as_bytes()),
            },
            base64(format!("jwt/{jwt}").as_bytes()),
        )
    }

    async fn complete(&self, key: &str, id: &str, xml: &str) -> Result<Response> {
        let path = s3::object_path(&self.bucket, key);
        let query = [("uploadId".into(), id.to_string())];
        let now = worker::js_sys::Date::now();
        let (keys, token) = self.completion_credential(key, (now / 1_000.0) as i64);
        let extra = [
            (CREATE_ONLY.0.into(), CREATE_ONLY.1.into()),
            ("x-amz-security-token".into(), token),
            ("content-type".into(), "application/xml".into()),
        ];
        let signed = s3::sign(
            "POST",
            &self.endpoint,
            &path,
            &query,
            &extra,
            &datastore::sha256_hex(xml.as_bytes()),
            &keys,
            &s3::amz_date(now),
        );
        let headers = Headers::new();
        for (name, value) in signed.iter().filter(|(name, _)| name != "host") {
            headers.set(name, value)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_redirect(RequestRedirect::Error)
            .with_headers(headers)
            .with_body(Some(
                worker::js_sys::Uint8Array::from(xml.as_bytes()).into(),
            ));
        let url = format!(
            "{}{path}?uploadId={}",
            self.endpoint.base,
            s3::uri_encode(id, true),
        );
        Fetch::Request(Request::new_with_init(&url, &init)?)
            .send()
            .await
    }
}

fn fail(message: &str) -> Error {
    Error::RustError(message.into())
}

fn reply(code: u16, state: &str) -> Result<Response> {
    Ok(Response::from_json(&json!({ "state": state }))?.with_status(code))
}

fn encode(saved: &Saved) -> Result<String> {
    serde_json::to_string(saved).map_err(|_| fail("could not encode the upload checkpoint"))
}

async fn load(db: &D1Database, slot: &str) -> Result<Option<(String, Saved)>> {
    let raw = db
        .prepare("SELECT body FROM ingest_multipart WHERE id = ?1")
        .bind(&[crate::store::js(&json!(slot))])?
        .first::<String>(Some("body"))
        .await?;
    raw.map(|raw| {
        let saved = serde_json::from_str(&raw).map_err(|_| fail("invalid upload checkpoint"))?;
        Ok((raw, saved))
    })
    .transpose()
}

async fn replace(db: &D1Database, slot: &str, previous: &str, next: &Saved) -> Result<bool> {
    let result = db
        .prepare("UPDATE ingest_multipart SET body = ?1 WHERE id = ?2 AND body = ?3")
        .bind(&[
            crate::store::js(&json!(encode(next)?)),
            crate::store::js(&json!(slot)),
            crate::store::js(&json!(previous)),
        ])?
        .run()
        .await?;
    Ok(result.meta()?.and_then(|meta| meta.changes) == Some(1))
}

async fn head(bucket: &Bucket, write: &Write) -> Result<Reconciliation> {
    let Some(object) = bucket.head(write.key()).await? else {
        return Ok(Reconciliation::Unresolved);
    };
    let metadata = object.custom_metadata()?;
    Ok(reconcile(
        write,
        Some((
            object.size(),
            metadata.get("sha256").map(String::as_str),
            metadata.get("write-id").map(String::as_str),
        )),
    ))
}

async fn start<F, R>(
    request: &Request,
    db: &D1Database,
    bucket: &Bucket,
    slot: &str,
    key: &str,
    reserve: F,
) -> Result<Response>
where
    F: FnOnce(u64) -> R,
    R: Future<Output = Result<bool>>,
{
    let size = request
        .headers()
        .get("x-ingest-size")?
        .and_then(|s| s.parse().ok())
        .unwrap_or_default();
    if let Some((_, old)) = load(db, slot).await? {
        if old.write().size != size || old.write().key() != key {
            return reply(409, "different upload already reserved");
        }
        return match old {
            Saved::Open { .. } => reply(200, "open"),
            Saved::Published { .. } => reply(200, "published"),
            Saved::Starting { .. } => reply(409, "upload initiation unresolved"),
            Saved::Closed { .. } => reply(409, "upload closed"),
        };
    }
    let write_id = datastore::sha256_hex(webauthn::random()?.as_bytes());
    let Some(write) = Write::new(key, size, write_id) else {
        return reply(400, "invalid multipart blob");
    };
    if head(bucket, &write).await? != Reconciliation::Unresolved {
        // This request did not independently verify the existing object's bytes.
        return reply(412, "destination already exists");
    }
    let prior = encode(&Saved::Starting { write })?;
    let result = db
        .prepare("INSERT OR IGNORE INTO ingest_multipart (id, body) VALUES (?1, ?2)")
        .bind(&[
            crate::store::js(&json!(slot)),
            crate::store::js(&json!(&prior)),
        ])?
        .run()
        .await?;
    if result.meta()?.and_then(|meta| meta.changes) != Some(1) {
        return reply(409, "another initiation is in progress");
    }
    let Saved::Starting { write } =
        serde_json::from_str(&prior).map_err(|_| fail("invalid initiation checkpoint"))?
    else {
        return Err(fail("invalid initiation checkpoint"));
    };
    if !reserve(size).await? {
        db.prepare("DELETE FROM ingest_multipart WHERE id = ?1 AND body = ?2")
            .bind(&[
                crate::store::js(&json!(slot)),
                crate::store::js(&json!(prior)),
            ])?
            .run()
            .await?;
        return reply(429, "upload exceeds the admitted run budget");
    }
    let metadata = HashMap::from([
        ("sha256".to_string(), write.sha256.clone()),
        ("write-id".to_string(), write.write_id.clone()),
    ]);
    let upload = bucket
        .create_multipart_upload(key)
        .custom_metadata(metadata)
        .execute()
        .await?;
    let id = upload.upload_id().await;
    if !replace(
        db,
        slot,
        &prior,
        &Saved::Open {
            id,
            checkpoint: Checkpoint::new(write),
        },
    )
    .await?
    {
        upload.abort().await?;
        return reply(409, "multipart initiation checkpoint changed");
    }
    reply(200, "open")
}

async fn part(
    request: &mut Request,
    db: &D1Database,
    bucket: &Bucket,
    slot: &str,
) -> Result<Response> {
    let number = request
        .headers()
        .get("x-ingest-part")?
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_default();
    let length = request
        .headers()
        .get("content-length")?
        .and_then(|s| s.parse::<u64>().ok());
    let Some((before, Saved::Open { id, mut checkpoint })) = load(db, slot).await? else {
        return reply(404, "no open upload");
    };
    let Some(expected) = checkpoint.part_bytes(number) else {
        return reply(400, "invalid part number");
    };
    if length != Some(expected) {
        return reply(413, "part length does not match its admitted range");
    }
    // Reading a bounded stream avoids trusting the caller's declared HTTP length.
    use futures_util::StreamExt;
    let mut stream = request.stream()?;
    let mut bytes = Vec::with_capacity(expected as usize);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > expected {
            return reply(413, "part exceeds its admitted range");
        }
        bytes.extend_from_slice(&chunk);
    }
    match checkpoint.pin(number, &bytes) {
        Some(PartAction::Replay) => return reply(200, "confirmed"),
        Some(PartAction::Upload) => {}
        None => return reply(409, "part sequence, size or digest does not match"),
    }
    let pinned = Saved::Open { id, checkpoint };
    let raw = encode(&pinned)?;
    if raw != before && !replace(db, slot, &before, &pinned).await? {
        return reply(409, "multipart checkpoint advanced, retry the same part");
    }
    let Saved::Open { id, mut checkpoint } = pinned else {
        return Err(fail("invalid pinned checkpoint"));
    };
    let upload = bucket.resume_multipart_upload(checkpoint.write.key(), &id)?;
    let uploaded = upload
        .upload_part(
            u16::try_from(number).map_err(|_| fail("invalid part number"))?,
            bytes.clone(),
        )
        .await?;
    checkpoint
        .confirm(number, &bytes, &uploaded.etag())
        .ok_or_else(|| fail("R2 returned an invalid part receipt"))?;
    if !replace(db, slot, &raw, &Saved::Open { id, checkpoint }).await? {
        return reply(409, "multipart checkpoint advanced, retry the same part");
    }
    reply(200, "confirmed")
}

async fn complete(
    db: &D1Database,
    bucket: &Bucket,
    publisher: &Publisher,
    slot: &str,
) -> Result<Response> {
    let Some((before, saved)) = load(db, slot).await? else {
        return reply(404, "no open upload");
    };
    let (id, checkpoint) = match saved {
        Saved::Published { write } => {
            return if head(bucket, &write).await? == Reconciliation::Ours {
                reply(200, "published")
            } else {
                reply(409, "published object could not be confirmed")
            };
        }
        Saved::Open { id, checkpoint } => (id, checkpoint),
        _ => return reply(409, "upload is not ready"),
    };
    let Some(xml) = checkpoint.completion_xml() else {
        return reply(409, "upload is not ready");
    };
    let mut receipt = publisher.complete(&checkpoint.write.key(), &id, &xml).await;
    let definitive = match receipt.as_mut() {
        Ok(response) if matches!(response.status_code(), 404 | 412) => {
            let status = response.status_code();
            let code = if status == 404 {
                "NoSuchUpload"
            } else {
                "PreconditionFailed"
            };
            response
                .text()
                .await
                .is_ok_and(|body| body.contains(&format!("<Code>{code}</Code>")))
        }
        _ => false,
    };
    match head(bucket, &checkpoint.write).await? {
        Reconciliation::Ours => {
            replace(
                db,
                slot,
                &before,
                &Saved::Published {
                    write: checkpoint.write,
                },
            )
            .await?;
            reply(200, "published")
        }
        Reconciliation::AlreadyPresent => {
            if !definitive {
                // R2's documented 409 does not abort the upload. Cancel only our staging ID.
                bucket
                    .resume_multipart_upload(checkpoint.write.key(), &id)?
                    .abort()
                    .await?;
            }
            replace(
                db,
                slot,
                &before,
                &Saved::Closed {
                    write: checkpoint.write,
                },
            )
            .await?;
            reply(412, "destination already exists")
        }
        Reconciliation::Unresolved if definitive => {
            // A definitive NoSuchUpload or failed precondition cannot later publish this ID.
            replace(
                db,
                slot,
                &before,
                &Saved::Closed {
                    write: checkpoint.write,
                },
            )
            .await?;
            reply(409, "upload no longer exists")
        }
        Reconciliation::Unresolved => reply(503, "completion unresolved, retry the same upload"),
    }
}

async fn abort(db: &D1Database, bucket: &Bucket, slot: &str) -> Result<Response> {
    let Some((raw, Saved::Open { id, checkpoint })) = load(db, slot).await? else {
        return reply(409, "no open upload");
    };
    bucket
        .resume_multipart_upload(checkpoint.write.key(), &id)?
        .abort()
        .await?;
    if head(bucket, &checkpoint.write).await? == Reconciliation::Ours {
        replace(
            db,
            slot,
            &raw,
            &Saved::Published {
                write: checkpoint.write,
            },
        )
        .await?;
        return reply(200, "published");
    }
    replace(
        db,
        slot,
        &raw,
        &Saved::Closed {
            write: checkpoint.write,
        },
    )
    .await?;
    reply(200, "closed")
}

/// Configuration discovery only. This does not verify a live provider write.
pub async fn configured(env: &Env) -> bool {
    let Ok(db) = env.d1("DB") else {
        return false;
    };
    db.prepare("SELECT id FROM ingest_multipart LIMIT 0")
        .run()
        .await
        .is_ok()
        && Publisher::open(env).await.is_ok()
}

/// Called only by the managed destination after validating its signed, per-run grant.
pub async fn fetch<F, R>(
    request: &mut Request,
    env: &Env,
    run: &str,
    key: &str,
    operation: &str,
    reserve: F,
) -> Result<Response>
where
    F: FnOnce(u64) -> R,
    R: Future<Output = Result<bool>>,
{
    let Some(sha) = key.rsplit('/').next() else {
        return reply(400, "invalid blob key");
    };
    if Write::new(
        key,
        pigeon_ingest::immutable_multipart::PART_BYTES + 1,
        sha.into(),
    )
    .is_none()
    {
        return reply(400, "invalid blob key");
    }
    let publisher = if matches!(operation, "multipart-start" | "multipart-complete") {
        match Publisher::open(env).await {
            Ok(publisher) => Some(publisher),
            Err(_) => return reply(503, "immutable multipart publication is unavailable"),
        }
    } else {
        None
    };
    let slot = datastore::sha256_hex(format!("{run}\0{key}").as_bytes());
    let db = env.d1("DB")?;
    let bucket = env.bucket("DATA")?;
    match operation {
        "multipart-start" => start(request, &db, &bucket, &slot, key, reserve).await,
        "multipart-part" => part(request, &db, &bucket, &slot).await,
        "multipart-complete" => {
            let publisher = publisher.ok_or_else(|| fail("multipart publisher missing"))?;
            complete(&db, &bucket, &publisher, &slot).await
        }
        "multipart-abort" => abort(&db, &bucket, &slot).await,
        _ => reply(400, "unsupported multipart operation"),
    }
}

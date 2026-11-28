//! The only R2 authority available to a managed ingest runner.

use futures_util::StreamExt;
use pigeon_ingest::{decide::Head, managed_destination as wire};
use serde_json::json;
use std::collections::HashMap;
use worker::{
    Bucket, Conditional, Env, HttpMetadata, Request, Response, Result, wasm_bindgen::JsValue,
};

use crate::{json_answer, now_ms};

fn error(status: u16, message: &str) -> Result<Response> {
    json_answer(status, json!({"error": message}))
}

pub async fn ready(req: &mut Request, env: &Env) -> Result<Response> {
    let key = env.secret("DATA_KEY")?.to_string();
    let signature = req
        .headers()
        .get(pigeon_ingest::protocol::SIGNATURE_HEADER)?;
    let Some(signature) = signature else {
        return error(401, "unauthorized");
    };
    let Some(body) = bounded_body(req, 1024).await? else {
        return error(413, "readiness body too large");
    };
    if pigeon_ingest::protocol::verify(
        &signature,
        "POST",
        wire::READY_PATH,
        &body,
        key.as_bytes(),
        now_ms(),
    )
    .is_err()
    {
        return error(401, "unauthorized");
    }
    env.bucket("DATA")?;
    env.d1("DB")?
        .prepare("SELECT run FROM ingest_grants LIMIT 0")
        .run()
        .await?;
    json_answer(
        200,
        json!({
            "version": 1,
            "instance": env.var("INSTANCE")?.to_string(),
            "multipart_configured": crate::immutable_multipart::configured(env).await,
        }),
    )
}

pub(crate) async fn charge(env: &Env, grant: &wire::Grant, bytes: u64) -> Result<bool> {
    if now_ms() >= grant.expires_ms || bytes > grant.max_bytes {
        return Ok(false);
    }
    // An UPSERT condition, rather than a separate read and update, enforces both limits
    // across concurrent requests. Failed writes are also charged to bound retries/abuse.
    let row = env
        .d1("DB")?
        .prepare(wire::CHARGE_SQL)
        .bind(&[
            JsValue::from_str(&grant.run),
            JsValue::from_f64(grant.expires_ms as f64),
            JsValue::from_f64(bytes as f64),
            JsValue::from_f64(grant.max_bytes as f64),
            JsValue::from_f64(grant.max_objects as f64),
        ])?
        .first::<u64>(Some("writes"))
        .await?;
    Ok(row.is_some())
}

async fn bounded_body(req: &mut Request, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut body = Vec::new();
    let mut stream = match req.stream() {
        Ok(stream) => stream,
        Err(worker::Error::RustError(error)) if limit == 0 && error == "no body for request" => {
            return Ok(Some(body));
        }
        Err(error) => return Err(error),
    };
    while let Some(part) = stream.next().await {
        let part = part?;
        if body.len().saturating_add(part.len()) as u64 > limit {
            return Ok(None);
        }
        body.extend_from_slice(&part);
    }
    Ok(Some(body))
}

async fn existing_control(bucket: &Bucket, key: &str, bytes: &[u8]) -> Result<bool> {
    let Some(object) = bucket.get(key).execute().await? else {
        return Ok(false);
    };
    if object.size() != bytes.len() as u64 {
        return Ok(false);
    }
    let Some(body) = object.body() else {
        return Ok(false);
    };
    Ok(body.bytes().await? == bytes)
}

pub async fn fetch(req: &mut Request, env: &Env) -> Result<Response> {
    let token = req.headers().get("authorization")?;
    let key = env.secret("DATA_KEY")?.to_string();
    let instance = env.var("INSTANCE")?.to_string();
    let grant = token
        .as_deref()
        .and_then(|token| token.strip_prefix("Bearer "))
        .and_then(|token| wire::verify(token, key.as_bytes(), &instance, now_ms()));
    let Some(grant) = grant else {
        return error(401, "invalid or expired destination grant");
    };
    let key = req.headers().get(wire::OBJECT)?.unwrap_or_default();
    let bucket = env.bucket("DATA")?;
    let op = req.headers().get(wire::OPERATION)?.unwrap_or_default();
    match op.as_str() {
        "multipart-start" | "multipart-part" | "multipart-complete" | "multipart-abort" => {
            let sha = req.headers().get(wire::SHA256)?.unwrap_or_default();
            if grant.dry_run || !wire::payload_key(&grant.run, &key, &sha) {
                return error(403, "multipart write outside grant");
            }
            crate::immutable_multipart::fetch(req, env, &grant.run, &key, &op, |size| {
                charge(env, &grant, size)
            })
            .await
        }
        "head" if wire::payload_read_key(&grant.run, &key) => {
            let Some(object) = bucket.head(&key).await? else {
                return error(404, "payload absent");
            };
            let fields = object.custom_metadata()?;
            let head = Head {
                size: object.size(),
                sha256: fields.get("sha256").cloned(),
                blake3: fields.get("blake3").cloned(),
            };
            Response::from_json(&head)
        }
        "get" if key == pigeon_ingest::manifest::checkpoint_key(&grant.run) => {
            let Some(object) = bucket.get(&key).execute().await? else {
                return error(404, "checkpoint absent");
            };
            if object.size() > wire::MAX_CONTROL {
                return error(413, "checkpoint exceeds the managed limit");
            }
            let Some(body) = object.body() else {
                return error(503, "checkpoint has no body");
            };
            Response::from_bytes(body.bytes().await?)
        }
        "put-blob" => {
            let sha = req.headers().get(wire::SHA256)?.unwrap_or_default();
            let blake = req.headers().get(wire::BLAKE3)?.unwrap_or_default();
            if grant.dry_run || !wire::payload_key(&grant.run, &key, &sha) || !wire::digest(&blake)
            {
                return error(403, "payload write outside grant");
            }
            let Some(expected) = req
                .headers()
                .get("content-length")?
                .and_then(|n| n.parse::<u64>().ok())
            else {
                return error(411, "payload length required");
            };
            if expected > wire::MAX_BLOB {
                return error(413, "payload exceeds the atomic single-write limit");
            }
            if !charge(env, &grant, expected).await? {
                return error(429, "run write budget exhausted");
            }
            let Some(body) = bounded_body(req, expected).await? else {
                return error(413, "payload exceeds its declared size");
            };
            if body.len() as u64 != expected || wire::hashes(&body) != (sha.clone(), blake.clone())
            {
                return error(422, "payload length or digest mismatch");
            }
            let fields = HashMap::from([
                ("sha256".into(), sha.clone()),
                ("blake3".into(), blake.clone()),
                ("size".into(), expected.to_string()),
            ]);
            let put = bucket
                .put(&key, body)
                .custom_metadata(fields)
                .only_if(create_only())
                .execute()
                .await?;
            if put.is_some() {
                return json_answer(201, json!({"created": true}));
            }
            let Some(existing) = bucket.head(&key).await? else {
                return error(409, "payload changed during conditional write");
            };
            let fields = existing.custom_metadata()?;
            if existing.size() == expected
                && fields.get("sha256") == Some(&sha)
                && fields.get("blake3") == Some(&blake)
            {
                return json_answer(200, json!({"created": false}));
            }
            error(409, "existing payload identity differs")
        }
        "put-object" => {
            let Some(immutable) = wire::control_key(&grant.run, &key) else {
                return error(403, "control write outside grant");
            };
            let Some(expected) = req
                .headers()
                .get("content-length")?
                .and_then(|n| n.parse::<u64>().ok())
            else {
                return error(411, "control length required");
            };
            if expected > wire::MAX_CONTROL {
                return error(413, "control object exceeds the managed limit");
            }
            if !charge(env, &grant, expected).await? {
                return error(429, "run write budget exhausted");
            }
            let Some(body) = bounded_body(req, expected).await? else {
                return error(413, "control body exceeds declared size");
            };
            if body.len() as u64 != expected {
                return error(422, "control body length mismatch");
            };
            let mut write = bucket.put(&key, body.clone()).http_metadata(HttpMetadata {
                content_type: Some(
                    if key.ends_with(".jsonl") {
                        "application/x-ndjson"
                    } else {
                        "application/json"
                    }
                    .into(),
                ),
                ..Default::default()
            });
            if immutable {
                write = write.only_if(create_only());
            }
            if write.execute().await?.is_some()
                || (immutable && existing_control(&bucket, &key, &body).await?)
            {
                return json_answer(200, json!({"stored": true}));
            }
            error(409, "immutable control object differs")
        }
        _ => error(403, "operation outside grant"),
    }
}

fn create_only() -> Conditional {
    Conditional {
        etag_does_not_match: Some("*".into()),
        ..Default::default()
    }
}

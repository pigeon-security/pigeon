//! The only R2 authority available to a managed ingest runner.

use futures_util::StreamExt;
use pigeon_ingest::{decide::Head, managed_destination as wire};
use serde_json::json;
use std::collections::HashMap;
use worker::{
    Bucket, Conditional, Env, HttpMetadata, Request, Response, Result, console_log,
    wasm_bindgen::JsValue,
};

use crate::{json_answer, now_ms};

fn error(status: u16, message: &str) -> Result<Response> {
    json_answer(status, json!({"error": message}))
}

/// Whether `req` is core's request for `path`, signed with the data key: `Some` refusal if not.
async fn core_signed(req: &mut Request, env: &Env, path: &str) -> Result<Option<Response>> {
    let key = env.secret("DATA_KEY")?.to_string();
    let signature = req
        .headers()
        .get(pigeon_ingest::protocol::SIGNATURE_HEADER)?;
    let Some(signature) = signature else {
        return error(401, "unauthorized").map(Some);
    };
    let Some(body) = bounded_body(req, 1024).await? else {
        return error(413, "request body too large").map(Some);
    };
    if pigeon_ingest::protocol::verify(&signature, "POST", path, &body, key.as_bytes(), now_ms())
        .is_err()
    {
        return error(401, "unauthorized").map(Some);
    }
    Ok(None)
}

/// Core's `data_ingest_multipart_check`: run the multipart canary and answer its result.
pub async fn canary(req: &mut Request, env: &Env) -> Result<Response> {
    if let Some(refused) = core_signed(req, env, wire::CANARY_PATH).await? {
        return Ok(refused);
    }
    if !crate::immutable_multipart::configured(env).await {
        return error(503, "the multipart publisher is not configured");
    }
    json_answer(200, crate::immutable_multipart::canary(env).await?)
}

pub async fn ready(req: &mut Request, env: &Env) -> Result<Response> {
    if let Some(refused) = core_signed(req, env, wire::READY_PATH).await? {
        return Ok(refused);
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
            // Ready for ingest only once the canary passed for the bound credential.
            "multipart_configured": crate::immutable_multipart::ready(env).await,
            "multipart_publisher": crate::immutable_multipart::configured(env).await,
            "multipart_canary": crate::immutable_multipart::canary_state(env).await,
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

pub(crate) async fn bounded_body(req: &mut Request, limit: u64) -> Result<Option<Vec<u8>>> {
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
        // Up to 500 payload identities in one request, a hint for the runner's decisions. Once
        // the index migration is applied, the D1 `index_blobs` rows answer first (a row is made
        // only from a live run's proof that R2 holds the blob; dry runs index nothing), and
        // only the misses are R2 HEADs (concurrently). Before it every key is an R2 HEAD
        // (docs/plans/ingest-index-dry-run.md). Create-only uploads stay the final guard.
        "head-many" => {
            let Some(body) = bounded_body(req, wire::HEAD_MANY_BODY).await? else {
                return error(413, "existence batch too large");
            };
            let Ok(batch) = serde_json::from_slice::<wire::HeadMany>(&body) else {
                return error(400, "invalid existence batch");
            };
            if !wire::head_many_keys(&grant.run, &batch) {
                return error(403, "existence batch outside grant");
            }
            let known = known(env, &batch.keys).await;
            let misses = wire::index_misses(&batch.keys, &known);
            let mut heads = Vec::with_capacity(misses.len());
            for chunk in misses.chunks(50) {
                let found =
                    futures_util::future::join_all(chunk.iter().map(|key| bucket.head(key))).await;
                for object in found {
                    heads.push(match object? {
                        Some(object) => {
                            let fields = object.custom_metadata()?;
                            Some(Head {
                                size: object.size(),
                                sha256: fields.get("sha256").cloned(),
                                blake3: fields.get("blake3").cloned(),
                            })
                        }
                        None => None,
                    });
                }
            }
            let Some(heads) = wire::merge_heads(&batch.keys, &known, heads) else {
                return error(500, "existence batch lost its order");
            };
            Response::from_json(&wire::HeadManyReply { heads })
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
                // A manifest part is indexed here, with the dry-run flag of the signed grant.
                // A failure leaves index misses only (head-many then asks R2), never a false
                // hit, so the write still succeeds.
                if pigeon_ingest::protocol::is_manifest_part(&key) && crate::index_ready(env).await
                {
                    let text = String::from_utf8_lossy(&body);
                    if let Err(e) = crate::index_part(env, &key, &text, grant.dry_run).await {
                        console_log!("indexing {key} failed: {e}");
                    }
                }
                return json_answer(200, json!({"stored": true}));
            }
            error(409, "immutable control object differs")
        }
        _ => error(403, "operation outside grant"),
    }
}

/// The index's identities of the blob keys among `keys`, once the index migration is applied;
/// none before it, or when D1 fails (every key is then an R2 HEAD).
async fn known(env: &Env, keys: &[String]) -> HashMap<String, Head> {
    if !crate::index_ready(env).await {
        return HashMap::new();
    }
    let shas: Vec<String> = keys
        .iter()
        .filter_map(|key| wire::indexable_sha(key).map(str::to_string))
        .collect();
    let mut rows = Vec::new();
    for s in pigeon_ingest::index::known_blobs(&shas) {
        match crate::store::rows(env, &s).await {
            Ok(found) => rows.extend(found),
            Err(e) => {
                console_log!("head-many index lookup failed: {e}");
                return HashMap::new();
            }
        }
    }
    wire::known_heads(&rows)
}

fn create_only() -> Conditional {
    Conditional {
        etag_does_not_match: Some("*".into()),
        ..Default::default()
    }
}

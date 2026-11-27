//! A write-only historical mail import route with its own opt-in signing credential.

use futures_util::StreamExt;
use pigeon_ingest::{
    audit::{PipelineKind, PipelineTerminal, terminal_object},
    datastore, protocol,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use worker::{Conditional, Env, Request, Response, Result};

use crate::{json_answer, mail, now_ms, store};

pub const PATH: &str = "/data/mail-import";
pub const TERMINAL_PATH: &str = "/data/mail-import/terminal";
const AUDIT_LIMIT: usize = 4096;
const DIGEST_HEADER: &str = "x-pigeon-body-sha256";
const KEY: &str = "MAIL_IMPORT_KEY";
const METADATA_LIMIT: usize = 8192;
const BODY_LIMIT: usize = datastore::MAX_MESSAGE as usize + METADATA_LIMIT + 4;

fn missing() -> Result<Response> {
    Response::empty().map(|r| r.with_status(404))
}

enum Verified {
    Body(Vec<u8>),
    Rejected(Response),
}

async fn authenticated_body(
    req: &mut Request,
    env: &Env,
    path: &str,
    limit: usize,
) -> Result<Verified> {
    let (Ok(instance), Ok(approved)) = (env.var("INSTANCE"), env.var("MAIL_IMPORT_INSTANCE"))
    else {
        return Ok(Verified::Rejected(missing()?));
    };
    if instance.to_string() != approved.to_string() {
        return Ok(Verified::Rejected(missing()?));
    }
    let (Ok(key), Some(signature), Some(digest)) = (
        env.secret(KEY),
        req.headers().get(protocol::SIGNATURE_HEADER)?,
        req.headers().get(DIGEST_HEADER)?,
    ) else {
        return Ok(Verified::Rejected(missing()?));
    };
    let key = key.to_string();
    if key.trim().len() < 32
        || protocol::verify_digest(&signature, "POST", path, &digest, key.as_bytes(), now_ms())
            .is_err()
    {
        return Ok(Verified::Rejected(missing()?));
    }
    let mut hasher = Sha256::new();
    let mut body = Vec::new();
    let mut stream = req.stream()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > limit - body.len() {
            return Ok(Verified::Rejected(json_answer(
                413,
                json!({ "error": "body exceeds import limit" }),
            )?));
        }
        hasher.update(&chunk);
        body.extend_from_slice(&chunk);
    }
    if format!("{:x}", hasher.finalize()) != digest {
        return Ok(Verified::Rejected(missing()?));
    }
    Ok(Verified::Body(body))
}

pub async fn receive(req: &mut Request, env: &Env) -> Result<Response> {
    let body = match authenticated_body(req, env, PATH, BODY_LIMIT).await? {
        Verified::Body(body) => body,
        Verified::Rejected(response) => return Ok(response),
    };
    let invalid = || json_answer(400, json!({ "error": "invalid historical message" }));
    let Some(length_bytes) = body.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()) else {
        return invalid();
    };
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length > METADATA_LIMIT {
        return invalid();
    }
    let Some(metadata) = body.get(4..4 + length) else {
        return invalid();
    };
    let Ok(source) = serde_json::from_slice(metadata) else {
        return invalid();
    };
    let raw = &body[4 + length..];
    match mail::import(&store::Runtime { env }, raw, source, &store::now_rfc3339()).await {
        Ok((id, duplicate)) => json_answer(200, json!({ "id": id, "duplicate": duplicate })),
        Err(mail::NOT_STORED) => json_answer(503, json!({ "error": mail::NOT_STORED })),
        Err(mail::TOO_LARGE) => json_answer(413, json!({ "error": mail::TOO_LARGE })),
        Err(_) => invalid(),
    }
}

pub async fn terminal(req: &mut Request, env: &Env) -> Result<Response> {
    let body = match authenticated_body(req, env, TERMINAL_PATH, AUDIT_LIMIT).await? {
        Verified::Body(body) => body,
        Verified::Rejected(response) => return Ok(response),
    };
    let invalid = || json_answer(400, json!({ "error": "invalid terminal audit" }));
    let Ok(terminal) = serde_json::from_slice::<PipelineTerminal>(&body) else {
        return invalid();
    };
    // This route can only write redacted terminal records for its approved email import
    // capability. Local import invocations never start compute or have a provider bill.
    if terminal.run_id.len() != 32
        || !terminal.run_id.bytes().all(|b| b.is_ascii_hexdigit())
        || terminal.cleanup.is_some()
        || terminal.estimated_usd_micros.is_some()
    {
        return invalid();
    }
    let instance = env.var("MAIL_IMPORT_INSTANCE")?.to_string();
    let Some(object) = terminal_object(&instance, PipelineKind::EmailIngest, &terminal) else {
        return invalid();
    };
    let (label, bucket, key) = match env.bucket("JOBS_AUDIT") {
        Ok(audit) => ("stored", audit, object.key),
        Err(_) => (
            "pending",
            env.bucket("DATA")?,
            format!("mail-import/audit-outbox/{}", object.key),
        ),
    };
    // The key includes the complete record's content hash. Replays are idempotent and do not
    // change a prior record. An R2 failure is still visible to the client and can be replayed.
    bucket
        .put(&key, object.json)
        .only_if(Conditional {
            etag_does_not_match: Some("*".into()),
            ..Default::default()
        })
        .execute()
        .await?;
    json_answer(200, json!({ "run_id": terminal.run_id, "audit": label }))
}

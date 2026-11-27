//! A write-only historical mail import route with its own opt-in signing credential.

use futures_util::StreamExt;
use pigeon_ingest::{datastore, protocol};
use serde_json::json;
use sha2::{Digest, Sha256};
use worker::{Env, Request, Response, Result};

use crate::{json_answer, mail, now_ms, store};

pub const PATH: &str = "/data/mail-import";
const DIGEST_HEADER: &str = "x-pigeon-body-sha256";
const KEY: &str = "MAIL_IMPORT_KEY";
const METADATA_LIMIT: usize = 8192;
const BODY_LIMIT: usize = datastore::MAX_MESSAGE as usize + METADATA_LIMIT + 4;

pub async fn receive(req: &mut Request, env: &Env) -> Result<Response> {
    let missing = || Response::empty().map(|r| r.with_status(404));
    let (Ok(instance), Ok(approved)) = (env.var("INSTANCE"), env.var("MAIL_IMPORT_INSTANCE"))
    else {
        return missing();
    };
    if instance.to_string() != approved.to_string() {
        return missing();
    }
    let (Ok(key), Some(signature), Some(digest)) = (
        env.secret(KEY),
        req.headers().get(protocol::SIGNATURE_HEADER)?,
        req.headers().get(DIGEST_HEADER)?,
    ) else {
        return missing();
    };
    let key = key.to_string();
    if key.trim().len() < 32
        || protocol::verify_digest(&signature, "POST", PATH, &digest, key.as_bytes(), now_ms())
            .is_err()
    {
        return missing();
    }
    let mut hasher = Sha256::new();
    let mut body = Vec::new();
    let mut stream = req.stream()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > BODY_LIMIT - body.len() {
            return json_answer(413, json!({ "error": "message exceeds import limit" }));
        }
        hasher.update(&chunk);
        body.extend_from_slice(&chunk);
    }
    if format!("{:x}", hasher.finalize()) != digest {
        return missing();
    }
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

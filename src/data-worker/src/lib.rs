//! The instance's data Worker: for now, the ingest job runner for Backblaze B2 as a Cloudflare
//! Container and the manifest applier for the D1 dedupe index (docs/plans/ingest.md, decision
//! 13); and the lite tier of the unified data store (docs/plans/email.md): the `email` handler
//! ([`mail`], [`store`]), the signed read ops at `/data/call`, and the B2 backup consumer.
//!
//! Not a platform Worker: no landing deploys it; an instance plan uploads it.
//!
//! Bindings: `DATA_KEY` (secret), `DB` (D1 `{instance}-data`), `DATA` (the instance bucket),
//! `INSTANCE` (text, the instance id), `BACKUP` (text, the B2 bucket), `B2_KEYS` (Secrets Store),
//! `INGEST_JOB` (the Durable Object, ingest only). It consumes `{instance}-ingest-manifests` and
//! `{instance}-data-backup`.

mod ingest_job;
pub mod mail;
mod mail_import;
pub mod store;

pub use ingest_job::IngestJob;
pub use pigeon_ingest;

use pigeon_ingest::container_job::{self, Start};
use pigeon_ingest::index;
use pigeon_ingest::manifest::ManifestLine;
use pigeon_ingest::protocol::{self, StopRequest};
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{
    Context, D1PreparedStatement, Env, ForwardableEmailMessage, MessageBatch, MessageExt, Method,
    Request, RequestInit, Response, Result, console_log, event,
};

fn now_ms() -> i64 {
    worker::Date::now().as_millis() as i64
}

fn json_answer(status: u16, body: Value) -> Result<Response> {
    Ok(Response::from_json(&body)?.with_status(status))
}

fn fail(reason: impl Into<String>) -> worker::Error {
    worker::Error::RustError(reason.into())
}

/// Verifies a signed request and answers its body, or the 401 to send back.
async fn signed_body(
    req: &mut Request,
    env: &Env,
) -> Result<std::result::Result<Vec<u8>, Response>> {
    let header = req.headers().get(protocol::SIGNATURE_HEADER)?;
    let path = req.path();
    let body = req.bytes().await?;
    let key = env.secret("DATA_KEY")?.to_string();
    let ok = match header {
        Some(h) => protocol::verify(&h, "POST", &path, &body, key.as_bytes(), now_ms()).is_ok(),
        None => false,
    };
    if ok {
        Ok(Ok(body))
    } else {
        Ok(Err(json_answer(401, json!({ "error": "unauthorized" }))?))
    }
}

/// Sends `body` to the `IngestJob` object named `run`, at `path`.
async fn to_job(env: &Env, run: &str, path: &str, body: Vec<u8>) -> Result<Response> {
    let text = String::from_utf8(body).map_err(|_| fail("body is not text"))?;
    let stub = env
        .durable_object("INGEST_JOB")?
        .id_from_name(run)?
        .get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_body(Some(JsValue::from_str(&text)));
    let req = Request::new_with_init(&format!("https://ingest-job{path}"), &init)?;
    stub.fetch_with_request(req).await
}

/// The HTTP surface: signed `POST /ingest/start` and `/ingest/stop`, open `GET /ingest/health`,
/// and the data store's signed `POST /data/call`, which answers 404 with no body unless signed.
#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Post, pigeon_ingest::datastore::CALL_PATH) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(_) => return Response::empty().map(|r| r.with_status(404)),
            };
            store::call(&env, &body).await
        }
        (Method::Post, mail_import::PATH) => mail_import::receive(&mut req, &env).await,
        (Method::Post, mail_import::TERMINAL_PATH) => mail_import::terminal(&mut req, &env).await,
        (Method::Get, "/ingest/health") => json_answer(200, json!({ "ok": true })),
        (Method::Post, protocol::START_PATH) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(denied) => return Ok(denied),
            };
            let Ok(start) = serde_json::from_slice::<Start>(&body) else {
                return json_answer(400, json!({ "error": "not a start request" }));
            };
            let job = &start.request.job;
            if !container_job::valid_run(&job.run) {
                return json_answer(400, json!({ "error": "invalid run id" }));
            }
            if start.image_revision.is_some() && job.instance != env.var("INSTANCE")?.to_string() {
                return json_answer(403, json!({ "error": "run belongs to another instance" }));
            }
            to_job(&env, &job.run, "/start", body).await
        }
        (Method::Post, container_job::READY_PATH) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(denied) => return Ok(denied),
            };
            to_job(&env, "readiness", "/ready", body).await
        }
        (Method::Post, path @ (protocol::STOP_PATH | container_job::STATUS_PATH)) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(denied) => return Ok(denied),
            };
            let Ok(stop) = serde_json::from_slice::<StopRequest>(&body) else {
                return json_answer(400, json!({ "error": "not a stop request" }));
            };
            if !container_job::valid_run(&stop.run) {
                return json_answer(400, json!({ "error": "invalid run id" }));
            }
            let op = if path == protocol::STOP_PATH {
                "/stop"
            } else {
                "/status"
            };
            to_job(&env, &stop.run, op, body).await
        }
        _ => json_answer(404, json!({ "error": "not found" })),
    }
}

use store::js;

/// Email Routing's handler (docs/plans/email.md): the catch-all rule sends every message here.
#[event(email)]
async fn email(message: ForwardableEmailMessage, env: Env, _ctx: Context) -> Result<()> {
    store::email(message, &env).await
}

/// Parses manifest JSONL; answers the good lines and how many were bad. Bad lines are logged
/// by number, never by content.
pub fn parse_lines(text: &str) -> (Vec<ManifestLine>, usize) {
    let mut lines = Vec::new();
    let mut bad = 0;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ManifestLine>(line) {
            Ok(l) => lines.push(l),
            Err(e) => {
                bad += 1;
                console_log!("manifest line {} skipped: {e}", n + 1);
            }
        }
    }
    (lines, bad)
}

/// Applies one manifest part to D1 as one batch.
async fn apply(env: &Env, key: &str) -> Result<()> {
    let bucket = env.bucket("DATA")?;
    let Some(object) = bucket.get(key).execute().await? else {
        // Gone: nothing to apply, and a retry would not bring it back.
        console_log!("manifest part {key} is not in the bucket");
        return Ok(());
    };
    let Some(body) = object.body() else {
        return Ok(());
    };
    let (lines, bad) = parse_lines(&body.text().await?);
    if bad > 0 {
        console_log!("manifest part {key}: {bad} bad lines skipped");
    }
    let db = env.d1("DB")?;
    let mut prepared: Vec<D1PreparedStatement> = Vec::new();
    for s in index::apply_part(key, &lines, now_ms()) {
        let params: Vec<JsValue> = s.params.iter().map(js).collect();
        prepared.push(db.prepare(&s.sql).bind(&params)?);
    }
    for result in db.batch(prepared).await? {
        if let Some(e) = result.error() {
            return Err(fail(format!("D1: {e}")));
        }
    }
    Ok(())
}

mod backup;

/// The queue consumer: each message is an R2 event notification. On the backup queue
/// (`…-backup`) each new object is copied to B2, deletes never mirrored; otherwise a manifest
/// part is applied to the index and any other key is acknowledged and ignored. A D1, R2 or B2
/// error retries the message.
#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
    if batch.queue().ends_with("-backup") {
        for message in batch.messages()? {
            match protocol::parse_event(message.body()) {
                Some(e) => match backup::copy(&env, &e.key).await {
                    Ok(_) => message.ack(),
                    Err(err) => {
                        console_log!("backup of an object failed: {err}");
                        message.retry();
                    }
                },
                // A delete, or not an event: nothing to copy.
                None => message.ack(),
            }
        }
        return Ok(());
    }
    for message in batch.messages()? {
        match protocol::parse_event(message.body()) {
            Some(e) if e.is_manifest_part => match apply(&env, &e.key).await {
                Ok(()) => message.ack(),
                Err(err) => {
                    console_log!("applying {} failed: {err}", e.key);
                    message.retry();
                }
            },
            _ => message.ack(),
        }
    }
    Ok(())
}

// Rebuilt on 2026-11-27: its first artifact build waited on the proxy redeploy after incident 24.
// Built again on 2026-11-27: the proxy now serves the artifact lane through an R2 binding.

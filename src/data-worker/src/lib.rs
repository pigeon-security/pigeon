//! The instance's data Worker: for now, the ingest job runner for Backblaze B2 as a Cloudflare
//! Container, and the manifest applier for the D1 dedupe index (docs/plans/ingest.md,
//! decision 13). Not a platform Worker; nothing deploys it yet (see `wrangler.example.toml`).
//!
//! Bindings: `DATA_KEY` (secret), `DB` (D1 `{instance}-data`), `DATA` (the instance bucket),
//! `INGEST_JOB` (the Durable Object). The queue `{instance}-ingest-manifests` is consumed here.

pub use pigeon_ingest;

use pigeon_core::containers::{self, Start};
use pigeon_ingest::index;
use pigeon_ingest::manifest::ManifestLine;
use pigeon_ingest::protocol::{self, StartRequest, StopRequest};
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{
    Context, D1PreparedStatement, DurableObject, Env, MessageBatch, MessageExt, Method, Request,
    RequestInit, Response, Result, State, console_log, event, wasm_bindgen,
};

/// The image's key in the object's `ctx.container.images`.
const IMAGE: &str = "ingest";
/// The container's size, decision 13.6.
const INSTANCE: &str = "standard-4";
/// The runner's program in the image.
const RUNNER: &str = "/usr/local/bin/pigeon-ingest-runner";

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

/// The HTTP surface: signed `POST /ingest/start` and `/ingest/stop`, open `GET /ingest/health`.
#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Get, "/ingest/health") => json_answer(200, json!({ "ok": true })),
        (Method::Post, protocol::START_PATH) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(denied) => return Ok(denied),
            };
            let Ok(start) = serde_json::from_slice::<StartRequest>(&body) else {
                return json_answer(400, json!({ "error": "not a start request" }));
            };
            to_job(&env, &start.job.run, "/start", body).await
        }
        (Method::Post, protocol::STOP_PATH) => {
            let body = match signed_body(&mut req, &env).await? {
                Ok(b) => b,
                Err(denied) => return Ok(denied),
            };
            let Ok(stop) = serde_json::from_slice::<StopRequest>(&body) else {
                return json_answer(400, json!({ "error": "not a stop request" }));
            };
            to_job(&env, &stop.run, "/stop", body).await
        }
        _ => json_answer(404, json!({ "error": "not found" })),
    }
}

/// One ingest run: the object named by the run, holding its container. It keeps no keys in
/// storage: they live only in the container's environment.
#[worker::durable_object]
pub struct IngestJob {
    state: State,
}

impl DurableObject for IngestJob {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        match req.path().as_str() {
            "/start" => {
                let Ok(start) = req.json::<StartRequest>().await else {
                    return json_answer(400, json!({ "error": "not a start request" }));
                };
                if containers::running(&self.state) {
                    return json_answer(202, json!({ "state": "running" }));
                }
                let env = protocol::start_env(&start).map_err(|e| fail(e.to_string()))?;
                let pairs: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                containers::start(
                    &self.state,
                    &Start {
                        image: IMAGE,
                        instance: INSTANCE,
                        env: &pairs,
                        entrypoint: &[RUNNER],
                        internet: true,
                    },
                )?;
                containers::keep_for(&self.state, containers::MAX_KEEP_MS).await?;
                json_answer(202, json!({ "state": "starting" }))
            }
            "/stop" => {
                containers::destroy(&self.state, "stopped by pigeon").await?;
                json_answer(200, json!({ "state": "stopped" }))
            }
            _ => json_answer(404, json!({ "error": "not found" })),
        }
    }
}

/// A JSON parameter as a D1 value.
fn js(v: &Value) -> JsValue {
    match v {
        Value::Null => JsValue::NULL,
        Value::Bool(b) => JsValue::from_bool(*b),
        Value::Number(n) => JsValue::from_f64(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => JsValue::from_str(s),
        other => JsValue::from_str(&other.to_string()),
    }
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

/// The queue consumer: each message is an R2 event notification. A manifest part is applied to
/// the index; any other key is acknowledged and ignored. A D1 or R2 error retries the message.
#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
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

//! The lite data store's runtime (docs/plans/email.md): the email handler's bindings over R2
//! and D1, the signed read ops at `/data/call`, and the B2 backup consumer (ingest.md
//! decision 10). The decisions are in [`crate::mail`] and `pigeon_ingest::datastore`; this file
//! only moves bytes and rows.

use futures_util::StreamExt;
use pigeon_core::s3;
use pigeon_ingest::datastore::{self, CallRequest, DataCall, EmailConfig};
use pigeon_ingest::index::Statement;
use serde::Deserialize;
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{
    D1PreparedStatement, Env, Fetch, ForwardableEmailMessage, Headers, Method, Request,
    RequestInit, Response, Result, console_log,
};

use crate::mail::{self, Bindings, Verdict};

/// The data bucket, `{instance}-data`.
const DATA: &str = "DATA";
/// The data D1 database, `{instance}-data`.
const DB: &str = "DB";

fn fail(reason: impl Into<String>) -> worker::Error {
    worker::Error::RustError(reason.into())
}

/// Now, RFC 3339 UTC.
pub fn now_rfc3339() -> String {
    String::from(worker::js_sys::Date::new_0().to_iso_string())
}

/// A JSON parameter as a D1 value.
pub fn js(v: &Value) -> JsValue {
    match v {
        Value::Null => JsValue::NULL,
        Value::Bool(b) => JsValue::from_bool(*b),
        Value::Number(n) => JsValue::from_f64(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => JsValue::from_str(s),
        other => JsValue::from_str(&other.to_string()),
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// Runs `statements` as one D1 batch.
async fn batch(env: &Env, statements: Vec<Statement>) -> Result<()> {
    let db = env.d1(DB)?;
    let mut prepared: Vec<D1PreparedStatement> = Vec::new();
    for s in statements {
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

/// The rows of one query.
async fn rows(env: &Env, s: &Statement) -> Result<Vec<Value>> {
    let params: Vec<JsValue> = s.params.iter().map(js).collect();
    let result = env.d1(DB)?.prepare(&s.sql).bind(&params)?.all().await?;
    result.results::<Value>()
}

/// The handler's bindings in the Workers runtime.
pub struct Runtime<'a> {
    pub env: &'a Env,
}

impl Bindings for Runtime<'_> {
    async fn has(&self, key: &str) -> std::result::Result<bool, String> {
        let bucket = self.env.bucket(DATA).map_err(|_| "no bucket".to_string())?;
        bucket
            .head(key)
            .await
            .map(|o| o.is_some())
            .map_err(|_| "R2 head failed".into())
    }

    async fn put(&self, key: &str, bytes: &[u8], sha256: &str) -> std::result::Result<(), String> {
        let bucket = self.env.bucket(DATA).map_err(|_| "no bucket".to_string())?;
        bucket
            .put(key, bytes.to_vec())
            .sha256(unhex(sha256))
            .execute()
            .await
            .map(|_| ())
            .map_err(|_| "R2 put failed".into())
    }

    async fn run(&self, statements: Vec<Statement>) -> std::result::Result<(), String> {
        batch(self.env, statements)
            .await
            .map_err(|_| "D1 batch failed".into())
    }

    async fn config(&self) -> std::result::Result<Option<EmailConfig>, String> {
        let found = rows(self.env, &datastore::config_query())
            .await
            .map_err(|_| "D1 read failed".to_string())?;
        let Some(row) = found.first() else {
            return Ok(None);
        };
        let text = row["value"].as_str().unwrap_or_default();
        serde_json::from_str(text)
            .map(Some)
            .map_err(|_| "the configuration is not readable".into())
    }
}

/// Email Routing's handler: stream and hash the bytes, store them once with their rows, then
/// forward when the configuration says so. Logs only the id, size, mailbox and outcome.
pub async fn email(message: ForwardableEmailMessage, env: &Env) -> Result<()> {
    let received_at = now_rfc3339();
    let envelope_from = message.from();
    let envelope_to = message.to();
    let loop_marks: Vec<String> = message
        .headers()
        .get(datastore::LOOP_HEADER)
        .ok()
        .flatten()
        .into_iter()
        .collect();
    let mut hasher = mail::Hasher::default();
    let mut raw = Vec::new();
    let mut stream = message.raw_byte_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        if hasher.size() > datastore::MAX_MESSAGE {
            message.set_reject(mail::TOO_LARGE);
            console_log!("{}", mail::log_line("-", hasher.size(), "-", "rejected"));
            return Ok(());
        }
        raw.extend_from_slice(&chunk);
    }
    let content = hasher.finish();
    let runtime = Runtime { env };
    let incoming = mail::Incoming {
        envelope_from: &envelope_from,
        envelope_to: &envelope_to,
        raw: &raw,
        content: content.clone(),
        loop_marks,
        received_at: received_at.clone(),
    };
    match mail::receive(&runtime, &incoming).await {
        Verdict::Reject(reason) => {
            message.set_reject(&reason);
            console_log!(
                "{}",
                mail::log_line(&content.sha256, content.size, "-", "rejected")
            );
        }
        Verdict::Stored(s) => {
            let (outcome, error) = if s.looped {
                ("loop", None)
            } else if let Some(to) = &s.forward {
                let headers = Headers::new();
                headers.set(datastore::LOOP_HEADER, &s.instance)?;
                match message.forward_with_headers(to, &headers.0).await {
                    Ok(_) => ("forwarded", None),
                    // The reason, never the destination or the content.
                    Err(_) => ("forward_failed", Some("Email Routing refused the forward")),
                }
            } else {
                ("stored", None)
            };
            let row = mail::delivery_row(&s, &envelope_to, &received_at, outcome, error);
            if runtime.run(vec![row]).await.is_err() {
                console_log!("email {} delivery not recorded", s.item_id);
            }
            console_log!(
                "{}",
                mail::log_line(&s.item_id, s.size, &s.mailbox, outcome)
            );
        }
    }
    Ok(())
}

/// Reads object `key` as text, at most `max` bytes; `None` when it is absent or larger.
async fn small_text(env: &Env, key: &str, max: u64) -> Result<Option<String>> {
    let bucket = env.bucket(DATA)?;
    let Some(object) = bucket.get(key).execute().await? else {
        return Ok(None);
    };
    if object.size() > max {
        return Ok(None);
    }
    let Some(body) = object.body() else {
        return Ok(None);
    };
    Ok(String::from_utf8(body.bytes().await?).ok())
}

/// Answers one checked read op.
pub async fn answer(env: &Env, call: &DataCall) -> Result<Value> {
    match call {
        DataCall::Status => {
            let q = datastore::status_queries();
            let counts = rows(env, &q[0]).await?;
            let deliveries = rows(env, &q[1]).await?;
            let config = Runtime { env }.config().await.ok().flatten();
            Ok(json!({
                "configured": config.is_some(),
                "config_digest": config.as_ref().map(EmailConfig::digest),
                "counts": counts.first().cloned().unwrap_or(Value::Null),
                "deliveries": deliveries,
            }))
        }
        DataCall::Mailboxes => Ok(json!({
            "mailboxes": rows(env, &datastore::mailboxes_query()).await?
        })),
        DataCall::List(q) => {
            let found = rows(env, &datastore::list_query(q)).await?;
            Ok(datastore::list_answer(&found, q.limit))
        }
        DataCall::Get { id } => {
            let qs = datastore::get_queries(id);
            let item = rows(env, &qs[0]).await?;
            let email = rows(env, &qs[1]).await?;
            let sightings = rows(env, &qs[2]).await?;
            let attachments = rows(env, &qs[3]).await?;
            let max = datastore::OBJECT_INLINE_MAX;
            let text = small_text(env, &mail::text_key(id), max).await?;
            let html = match email.first().and_then(|e| e["html_ref"].as_str()) {
                Some(sha) => small_text(env, &pigeon_ingest::blob_key(sha), max).await?,
                None => None,
            };
            Ok(
                datastore::get_answer(&item, &email, &sightings, &attachments, text, html)
                    .unwrap_or(Value::Null),
            )
        }
        DataCall::Objects {
            prefix,
            cursor,
            limit,
        } => {
            if prefix.is_empty() {
                return Ok(json!({
                    "objects": [],
                    "prefixes": datastore::READABLE_PREFIXES,
                    "next": null,
                }));
            }
            let bucket = env.bucket(DATA)?;
            let mut list = bucket
                .list()
                .prefix(prefix.clone())
                .delimiter("/")
                .limit(limit.unwrap_or(100).clamp(1, 1000));
            if let Some(c) = cursor {
                list = list.cursor(c.clone());
            }
            let page = list.execute().await?;
            let objects: Vec<Value> = page
                .objects()
                .iter()
                .map(|o| {
                    json!({
                        "key": o.key(),
                        "size": o.size(),
                        "etag": o.etag(),
                        "last_modified": String::from(
                            worker::js_sys::Date::new(&JsValue::from_f64(o.uploaded().as_millis() as f64))
                                .to_iso_string()
                        ),
                    })
                })
                .collect();
            Ok(json!({
                "objects": objects,
                "prefixes": page.delimited_prefixes(),
                "next": if page.truncated() { page.cursor() } else { None },
            }))
        }
        DataCall::Object { key } => {
            let bucket = env.bucket(DATA)?;
            let Some(head) = bucket.head(key.clone()).await? else {
                return Err(fail("no such object"));
            };
            if head.size() > datastore::OBJECT_INLINE_MAX {
                return Err(fail(format!(
                    "the object is {} bytes, over the {} byte inline limit",
                    head.size(),
                    datastore::OBJECT_INLINE_MAX
                )));
            }
            let Some(object) = bucket.get(key.clone()).execute().await? else {
                return Err(fail("no such object"));
            };
            let content_type = object.http_metadata().content_type;
            let bytes = match object.body() {
                Some(b) => b.bytes().await?,
                None => Vec::new(),
            };
            let size = bytes.len();
            let (encoding, body) = match String::from_utf8(bytes) {
                Ok(text) => ("utf-8", text),
                Err(e) => ("base64", pigeon_core::git::base64(e.as_bytes())),
            };
            Ok(json!({
                "key": key, "content_type": content_type, "size": size,
                "encoding": encoding, "body": body,
            }))
        }
    }
}

/// Serves a signed `/data/call` whose body `body` was verified: refuses another instance's
/// request with 404 and a bad op with 400, else answers it.
pub async fn call(env: &Env, body: &[u8]) -> Result<Response> {
    let Ok(request) = serde_json::from_slice::<CallRequest>(body) else {
        return Ok(Response::from_json(&json!({ "error": "not a data call" }))?.with_status(400));
    };
    let instance = env.var("INSTANCE").map(|v| v.to_string()).ok();
    if instance.as_deref() != Some(request.instance.as_str()) {
        return Response::empty().map(|r| r.with_status(404));
    }
    if let Some(why) = datastore::call_problem(&request.call) {
        return Ok(Response::from_json(&json!({ "error": why }))?.with_status(400));
    }
    match answer(env, &request.call).await {
        Ok(v) => Response::from_json(&v),
        Err(e) => Ok(Response::from_json(&json!({ "error": e.to_string() }))?.with_status(502)),
    }
}

/// Where the backup goes: the B2 bucket's S3 endpoint, region and bucket, from the plain-text
/// binding `BACKUP` the plan sets from the record.
#[derive(Deserialize)]
struct Backup {
    endpoint: String,
    region: String,
    bucket: String,
}

/// Copies object `key` to the B2 backup bucket at the same key (ingest.md decision 10), with
/// its SHA-256 in `x-amz-meta-sha256`. An absent object is done; a missing key or binding, or
/// a refusal, is an error so the message is retried and then dead-lettered.
pub async fn back_up(env: &Env, key: &str) -> Result<()> {
    let config: Backup = serde_json::from_str(&env.var("BACKUP")?.to_string())
        .map_err(|_| fail("BACKUP is not set"))?;
    let secret = env
        .secret_store("B2_KEYS")?
        .get()
        .await?
        .ok_or_else(|| fail("the B2 key is not set"))?;
    let keys = s3::parse_keys(&secret)?;
    let Some(object) = env.bucket(DATA)?.get(key).execute().await? else {
        return Ok(());
    };
    let bytes = match object.body() {
        Some(b) => b.bytes().await?,
        None => Vec::new(),
    };
    let sha = datastore::sha256_hex(&bytes);
    let endpoint = s3::Endpoint {
        base: config.endpoint.trim_end_matches('/').to_string(),
        region: config.region,
    };
    let path = s3::object_path(&config.bucket, key);
    let extra = vec![("x-amz-meta-sha256".to_string(), sha.clone())];
    let date = s3::amz_date(worker::js_sys::Date::now());
    let signed = s3::sign("PUT", &endpoint, &path, &[], &extra, &sha, &keys, &date);
    let headers = Headers::new();
    for (k, v) in signed.iter().filter(|(k, _)| k != "host") {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Put)
        .with_headers(headers)
        .with_body(Some(worker::js_sys::Uint8Array::from(&bytes[..]).into()));
    let url = format!("{}{path}", endpoint.base);
    let res = Fetch::Request(Request::new_with_init(&url, &init)?)
        .send()
        .await?;
    if (200..300).contains(&res.status_code()) {
        Ok(())
    } else {
        Err(fail(format!("B2 refused the copy ({})", res.status_code())))
    }
}

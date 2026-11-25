//! The core side: asking a capability Worker, through its service binding, and a capability
//! Worker asking core, through its `PLATFORM` binding.

use std::sync::OnceLock;

use serde_json::Value;
use worker::wasm_bindgen::JsValue;
use worker::{Fetcher, Headers, Method, Request, RequestInit};

use crate::caller::{HEADER, WorkerContext, sign, sign_with_context};
use crate::now_ms;
use crate::wire::{Answer, FailureKind, Manifest, WorkerManifest, accepts};

/// The header a capability Worker sends on each request it signs, naming itself, so core checks
/// the signature with that Worker's own key first (docs/plans/caller-keys.md). The name is a
/// claim only: a signature made with another key does not pass as that Worker's.
pub const WORKER_HEADER: &str = "X-Pigeon-Worker";

/// This Worker's name, once [`identify`] has set it.
static WORKER: OnceLock<&'static str> = OnceLock::new();

/// Names this Worker, by its `name` in ci/workers.toml, so every request it signs also sends
/// [`WORKER_HEADER`]. A capability Worker calls it once from its `#[event(start)]`, which runs
/// before any fetch or Durable Object code. Core never calls it, so core sends no header. The
/// first name set stays.
pub fn identify(name: &'static str) {
    let _ = WORKER.set(name);
}

/// The name [`identify`] set; `None` until then.
pub fn identity() -> Option<&'static str> {
    WORKER.get().copied()
}

/// Asks `fetcher` (a service binding) op `op` with `body` as JSON, signed for `caller` when
/// given as (key, who). `base` is `https://capability`, or `https://core` for core. A peer that
/// answers non-JSON, or HTTP 5xx, is a `failed` answer with the status and up to 300 bytes of
/// its body.
pub async fn ask(
    fetcher: &Fetcher,
    base: &str,
    op: &str,
    body: &Value,
    caller: Option<(&[u8], &Value)>,
    nonce: &str,
) -> worker::Result<Answer> {
    ask_inner(fetcher, base, op, body, caller, nonce, None).await
}

/// Asks a Worker with authenticated destination and scope.
/// Callers should negotiate support before selecting this path and never retry a scoped request
/// with a legacy envelope if the receiver rejects its context.
pub async fn ask_with_context(
    fetcher: &Fetcher,
    base: &str,
    op: &str,
    body: &Value,
    caller: (&[u8], &Value),
    nonce: &str,
    context: &WorkerContext,
) -> worker::Result<Answer> {
    ask_inner(fetcher, base, op, body, Some(caller), nonce, Some(context)).await
}

async fn ask_inner(
    fetcher: &Fetcher,
    base: &str,
    op: &str,
    body: &Value,
    caller: Option<(&[u8], &Value)>,
    nonce: &str,
    context: Option<&WorkerContext>,
) -> worker::Result<Answer> {
    let text = serde_json::to_string(body).map_err(|e| worker::Error::RustError(e.to_string()))?;
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    if let Some((key, who)) = caller {
        let at = now_ms();
        let value = match context {
            Some(context) => sign_with_context(key, who, op, text.as_bytes(), at, nonce, context),
            None => sign(key, who, op, text.as_bytes(), at, nonce),
        };
        headers.set(HEADER, &value)?;
        if let Some(name) = identity() {
            headers.set(WORKER_HEADER, name)?;
        }
    }
    let url = format!("{}/{op}", base.trim_end_matches('/'));
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&text)));
    let request = Request::new_with_init(&url, &init)?;
    let mut response = fetcher.fetch_request(request).await?;
    let status = response.status_code();
    let reply = response.text().await?;
    if status >= 500 {
        return Ok(failed(status, &reply));
    }
    Ok(serde_json::from_str::<Answer>(&reply).unwrap_or_else(|_| failed(status, &reply)))
}

/// The manifest at `fetcher`, or why not: no answer, one that does not parse, or a contract
/// this build does not understand.
pub async fn manifest(fetcher: &Fetcher) -> worker::Result<Manifest> {
    Ok(manifest_with_context(fetcher).await?.manifest)
}

/// The manifest including the transport extensions the receiver actually supports.
pub async fn manifest_with_context(fetcher: &Fetcher) -> worker::Result<WorkerManifest> {
    let answer = ask(
        fetcher,
        "https://capability",
        "manifest",
        &Value::Null,
        None,
        "",
    )
    .await?;
    match answer {
        Answer::Ok(value) => {
            let described: WorkerManifest = serde_json::from_value(value).map_err(|e| {
                worker::Error::RustError(format!("the manifest does not parse: {e}"))
            })?;
            let manifest = &described.manifest;
            if accepts(manifest.contract) {
                Ok(described)
            } else {
                Err(worker::Error::RustError(format!(
                    "{} speaks contract {}, which this build does not understand",
                    manifest.worker, manifest.contract
                )))
            }
        }
        Answer::Error(failure) => Err(worker::Error::RustError(format!(
            "the manifest was refused ({:?}): {}",
            failure.kind, failure.message
        ))),
    }
}

/// A `failed` answer for a peer that did not speak the contract.
fn failed(status: u16, body: &str) -> Answer {
    Answer::error(
        FailureKind::Failed,
        format!("HTTP {status}: {}", snippet(body)),
    )
}

/// Up to 300 bytes of `text`, cut on a character boundary.
fn snippet(text: &str) -> &str {
    let mut end = text.len().min(300);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

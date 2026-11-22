//! The core side: asking a capability Worker, through its service binding, and a capability
//! Worker asking core, through its `PLATFORM` binding.

use serde_json::Value;
use worker::wasm_bindgen::JsValue;
use worker::{Fetcher, Headers, Method, Request, RequestInit};

use crate::caller::{HEADER, sign};
use crate::now_ms;
use crate::wire::{Answer, FailureKind, Manifest, accepts};

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
    let text = serde_json::to_string(body).map_err(|e| worker::Error::RustError(e.to_string()))?;
    let headers = Headers::new();
    headers.set("Content-Type", "application/json")?;
    if let Some((key, who)) = caller {
        let value = sign(key, who, op, text.as_bytes(), now_ms(), nonce);
        headers.set(HEADER, &value)?;
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
            let manifest: Manifest = serde_json::from_value(value).map_err(|e| {
                worker::Error::RustError(format!("the manifest does not parse: {e}"))
            })?;
            if accepts(manifest.contract) {
                Ok(manifest)
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

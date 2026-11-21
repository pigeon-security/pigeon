//! A minimal Cloudflare API client: one call, its `result` or its `errors`.

use serde_json::Value;
use worker::wasm_bindgen::JsValue;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

const API: &str = "https://api.cloudflare.com/client/v4";

/// Calls `method path` as `token`, sending `body` as JSON; answers the response's `result`.
pub async fn call(token: &str, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    let mut init = RequestInit::new();
    init.with_method(method);
    if let Some(body) = body {
        headers.set("Content-Type", "application/json")?;
        init.with_body(Some(JsValue::from_str(&body.to_string())));
    }
    init.with_headers(headers);
    let request = Request::new_with_init(&format!("{API}{path}"), &init)?;
    let mut response = Fetch::Request(request).send().await?;
    let status = response.status_code();
    let text = response.text().await?;
    // Some calls, such as Artifacts' deletes, answer success with no body.
    if text.trim().is_empty() && (200..300).contains(&status) {
        return Ok(Value::Null);
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| Error::RustError(format!("{path}: {status}: the answer is not JSON: {e}")))?;
    if v["success"].as_bool() == Some(true) {
        Ok(v["result"].clone())
    } else {
        Err(Error::RustError(format!(
            "{path}: {status}: {}",
            v["errors"]
        )))
    }
}

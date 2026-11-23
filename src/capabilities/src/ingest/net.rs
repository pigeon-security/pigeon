//! The network calls of an ingest run: DigitalOcean, Scaleway and the data Worker. Each call
//! answers the status and body, and an error text that never holds a token or a key.

use pigeon_core::sign;
use pigeon_ingest::protocol;
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use super::hosts;

/// The most of a provider's answer put in an error, in characters.
const SNIPPET: usize = 300;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// One HTTPS call. `body` is a content type and text. Answers the status and the body text.
async fn http(
    method: Method,
    url: &str,
    headers: &[(&str, String)],
    body: Option<(&str, String)>,
) -> Result<(u16, String)> {
    let h = Headers::new();
    for (k, v) in headers {
        h.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method);
    if let Some((content_type, text)) = body {
        h.set("content-type", content_type)?;
        init.with_body(Some(JsValue::from_str(&text)));
    }
    init.with_headers(h);
    let mut res = Fetch::Request(Request::new_with_init(url, &init)?)
        .send()
        .await?;
    let status = res.status_code();
    let text = res.text().await.unwrap_or_default();
    Ok((status, text))
}

/// A short, single-line piece of a provider's answer, for an error.
fn snippet(text: &str) -> String {
    text.chars()
        .take(SNIPPET)
        .collect::<String>()
        .replace('\n', " ")
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn failed(what: &str, status: u16, text: &str) -> Error {
    bad(format!("{what} failed ({status}): {}", snippet(text)))
}

// ---- DigitalOcean ----

const DO_API: &str = "https://api.digitalocean.com/v2";

fn do_auth(token: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {token}"))]
}

/// `GET /v2/account`: whether the token works. Answers the account's status text.
pub async fn do_account(token: &str) -> Result<String> {
    let (s, t) = http(
        Method::Get,
        &format!("{DO_API}/account"),
        &do_auth(token),
        None,
    )
    .await?;
    if s != 200 {
        return Err(failed("DigitalOcean's account check", s, &t));
    }
    Ok(parse(&t)["account"]["status"]
        .as_str()
        .unwrap_or("active")
        .to_string())
}

/// `POST /v2/droplets`: creates the Droplet and answers its id.
pub async fn do_create(token: &str, body: &Value) -> Result<String> {
    let (s, t) = http(
        Method::Post,
        &format!("{DO_API}/droplets"),
        &do_auth(token),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if !(200..300).contains(&s) {
        return Err(failed("creating the Droplet", s, &t));
    }
    parse(&t)["droplet"]["id"]
        .as_u64()
        .map(|n| n.to_string())
        .ok_or_else(|| bad("DigitalOcean answered no Droplet id"))
}

/// `GET /v2/droplets/{id}`: the Droplet's status, or none when it is gone.
pub async fn do_state(token: &str, id: &str) -> Result<Option<String>> {
    let (s, t) = http(
        Method::Get,
        &format!("{DO_API}/droplets/{id}"),
        &do_auth(token),
        None,
    )
    .await?;
    match s {
        404 => Ok(None),
        200 => Ok(Some(
            parse(&t)["droplet"]["status"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
        )),
        _ => Err(failed("reading the Droplet", s, &t)),
    }
}

/// `DELETE /v2/droplets/{id}`. A Droplet already gone is fine.
pub async fn do_delete(token: &str, id: &str) -> Result<()> {
    let (s, t) = http(
        Method::Delete,
        &format!("{DO_API}/droplets/{id}"),
        &do_auth(token),
        None,
    )
    .await?;
    if matches!(s, 200..=299 | 404) {
        Ok(())
    } else {
        Err(failed("deleting the Droplet", s, &t))
    }
}

// ---- Scaleway ----

const SCW_API: &str = "https://api.scaleway.com";

fn scw_auth(secret: &str) -> Vec<(&'static str, String)> {
    vec![("x-auth-token", secret.to_string())]
}

/// Lists one server in `zone`: whether the key works.
pub async fn scw_check(secret: &str, zone: &str) -> Result<()> {
    let url = format!("{SCW_API}{}?per_page=1", hosts::scaleway_servers_path(zone));
    let (s, t) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    if s == 200 {
        Ok(())
    } else {
        Err(failed("Scaleway's server list", s, &t))
    }
}

/// Creates the server and answers its id.
pub async fn scw_create(secret: &str, zone: &str, body: &Value) -> Result<String> {
    let url = format!("{SCW_API}{}", hosts::scaleway_servers_path(zone));
    let (s, t) = http(
        Method::Post,
        &url,
        &scw_auth(secret),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if !(200..300).contains(&s) {
        return Err(failed("creating the server", s, &t));
    }
    parse(&t)["server"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| bad("Scaleway answered no server id"))
}

/// Sets the server's cloud-init user data.
pub async fn scw_user_data(secret: &str, zone: &str, id: &str, user_data: &str) -> Result<()> {
    let url = format!("{SCW_API}{}", hosts::scaleway_user_data_path(zone, id));
    let (s, t) = http(
        Method::Patch,
        &url,
        &scw_auth(secret),
        Some(("text/plain", user_data.to_string())),
    )
    .await?;
    if (200..300).contains(&s) {
        Ok(())
    } else {
        Err(failed("setting the server's user data", s, &t))
    }
}

/// Posts an action body (`poweron` or `terminate`). A server already gone is fine for terminate.
pub async fn scw_action(secret: &str, zone: &str, id: &str, body: &Value) -> Result<()> {
    let url = format!("{SCW_API}{}", hosts::scaleway_action_path(zone, id));
    let (s, t) = http(
        Method::Post,
        &url,
        &scw_auth(secret),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if (200..300).contains(&s) || (s == 404 && body["action"] == "terminate") {
        Ok(())
    } else {
        Err(failed("the server action", s, &t))
    }
}

/// The server's state, or none when it is gone.
pub async fn scw_state(secret: &str, zone: &str, id: &str) -> Result<Option<String>> {
    let url = format!("{SCW_API}{}/{id}", hosts::scaleway_servers_path(zone));
    let (s, t) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    match s {
        404 => Ok(None),
        200 => Ok(Some(
            parse(&t)["server"]["state"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
        )),
        _ => Err(failed("reading the server", s, &t)),
    }
}

// ---- the data Worker ----

/// Posts `body` to the data Worker at `base` + `path`, signed with `key` as
/// `protocol::signing_message` says. Answers the body text.
pub async fn data_post(
    base: &str,
    path: &str,
    key: &str,
    at_ms: i64,
    body: &Value,
) -> Result<String> {
    let text = body.to_string();
    let message = protocol::signing_message("POST", path, at_ms, text.as_bytes());
    let mac = sign::hex(&sign::hmac_sha256(key.as_bytes(), message.as_bytes()));
    let header = format!("t={at_ms},v1={mac}");
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let (s, t) = http(
        Method::Post,
        &url,
        &[(protocol::SIGNATURE_HEADER, header)],
        Some(("application/json", text)),
    )
    .await?;
    if (200..300).contains(&s) {
        Ok(t)
    } else {
        Err(failed("the data Worker's answer", s, &t))
    }
}

/// A one-line note for a host that has no VM state to read.
pub fn cloudflare_note() -> Value {
    json!("runs in the instance's Cloudflare Container; see status.json for its progress")
}

//! The network calls of an ingest run: DigitalOcean, Scaleway and the data Worker. Each call
//! answers the status and body, and an error text that never holds a token or a key.

use pigeon_core::sign;
use pigeon_ingest::protocol;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

/// The most of a provider's answer put in an error, in characters.
const SNIPPET: usize = 300;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

pub(super) use pigeon_cloud::scaleway_ingest::http;

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

pub use pigeon_cloud::scaleway_ingest::{
    scw_action, scw_check, scw_create, scw_delete_server, scw_find_created, scw_find_volume,
    scw_remove_volume, scw_state, scw_user_data, scw_uuid,
};

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
        Err(bad(format!("the data Worker rejected the request ({s})")))
    }
}

/// A one-line note for a host that has no VM state to read.
pub fn cloudflare_note() -> Value {
    json!("runs in the instance's Cloudflare Container; see status.json for its progress")
}

//! The App Store Connect API client (docs/plans/ios.md, §2): the ES256 JWT that every request
//! carries, the checks on the ids the person enters, and one read-only GET. The key never leaves
//! this Worker, and no error holds the key or a token.
//!
//! The pure functions build and check; [`sign_jwt`] and [`get`] call WebCrypto and the network.

use serde::Serialize;
use worker::js_sys::{self, Array, Function, JSON, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::webauthn::b64url;

/// App Store Connect's API.
pub const API: &str = "https://api.appstoreconnect.apple.com";

/// The bundle id of the app (docs/plans/ios.md: it takes over the existing `dev.pigeon.app`).
pub const BUNDLE_ID: &str = "dev.pigeon.app";

/// How long a token lives. Apple allows at most 20 minutes; we use 10.
pub const TOKEN_SECS: u64 = 600;

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// Whether `s` is a team id or a key id: 10 characters of A-Z and 0-9.
pub fn is_apple_id(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Whether `s` is an issuer id: a UUID (8-4-4-4-12 hex digits, either case).
pub fn is_issuer_id(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The DER inside an App Store Connect key (`.p8`: PEM `PRIVATE KEY`, PKCS#8). None for anything
/// else, such as an RSA key or a certificate. Never echoes the input.
pub fn p8_der(pem: &str) -> Option<Vec<u8>> {
    if !pem.contains("-----BEGIN PRIVATE KEY-----") {
        return None;
    }
    let der = crate::github_app::private_key_der(pem)?;
    // A PKCS#8 P-256 key is about 138 bytes; an RSA key is over 1 kB, so far from that is not
    // a .p8.
    if (100..=200).contains(&der.len()) && der.first() == Some(&0x30) {
        Some(der)
    } else {
        None
    }
}

#[derive(Serialize)]
struct Header<'a> {
    alg: &'a str,
    kid: &'a str,
    typ: &'a str,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
    aud: &'a str,
}

/// The text a token signs: base64url header, a dot, base64url claims.
pub fn jwt_signing_input(key_id: &str, issuer_id: &str, now_secs: u64) -> String {
    let header = Header {
        alg: "ES256",
        kid: key_id,
        typ: "JWT",
    };
    let claims = Claims {
        iss: issuer_id,
        iat: now_secs,
        exp: now_secs + TOKEN_SECS,
        aud: "appstoreconnect-v1",
    };
    let h = serde_json::to_vec(&header).unwrap_or_default();
    let c = serde_json::to_vec(&claims).unwrap_or_default();
    format!("{}.{}", b64url(&h), b64url(&c))
}

/// Signs a token with the `.p8`'s DER through WebCrypto (ECDSA P-256, SHA-256). WebCrypto's
/// signature is raw `r‖s`, which is what a JWT carries. A key WebCrypto refuses gives an error
/// that names no part of it.
pub async fn sign_jwt(
    p8_der: &[u8],
    key_id: &str,
    issuer_id: &str,
    now_secs: u64,
) -> Result<String> {
    let input = jwt_signing_input(key_id, issuer_id, now_secs);
    let refused = || bad("WebCrypto refused the key: it is not a P-256 private key");
    let subtle = Reflect::get(
        &Reflect::get(&js_sys::global(), &"crypto".into())?,
        &"subtle".into(),
    )?;
    let import_alg = JSON::parse(r#"{"name":"ECDSA","namedCurve":"P-256"}"#)?;
    let import: Function = Reflect::get(&subtle, &"importKey".into())?.dyn_into()?;
    let args = Array::of5(
        &"pkcs8".into(),
        &Uint8Array::from(p8_der),
        &import_alg,
        &JsValue::FALSE,
        &Array::of1(&"sign".into()),
    );
    let promise = import.apply(&subtle, &args).map_err(|_| refused())?;
    let key = JsFuture::from(Promise::resolve(&promise))
        .await
        .map_err(|_| refused())?;
    let sign_alg = JSON::parse(r#"{"name":"ECDSA","hash":"SHA-256"}"#)?;
    let sign: Function = Reflect::get(&subtle, &"sign".into())?.dyn_into()?;
    let data = Uint8Array::from(input.as_bytes());
    let promise = sign
        .apply(&subtle, &Array::of3(&sign_alg, &key, &data))
        .map_err(|_| refused())?;
    let buffer = JsFuture::from(Promise::resolve(&promise))
        .await
        .map_err(|_| refused())?;
    let signature = Uint8Array::new(&buffer).to_vec();
    if signature.len() != 64 {
        return Err(bad(
            "the key gave a signature of the wrong length for ES256",
        ));
    }
    Ok(format!("{input}.{}", b64url(&signature)))
}

/// Answers the error text for an App Store Connect error body: the first error's status, code
/// and title, at most 200 characters. Never the request's token.
pub fn api_error(status: u16, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let e = &v["errors"][0];
    let text = match (e["code"].as_str(), e["title"].as_str()) {
        (Some(c), Some(t)) => format!("App Store Connect {status} {c}: {t}"),
        _ => format!("App Store Connect {status}"),
    };
    text.chars().take(200).collect()
}

/// GETs `path` (with its query) from the API with `token`, and answers the JSON body.
pub async fn get(token: &str, path: &str) -> Result<serde_json::Value> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    headers.set("Accept", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let mut res = Fetch::Request(Request::new_with_init(&format!("{API}{path}"), &init)?)
        .send()
        .await?;
    let status = res.status_code();
    let body = res.text().await.unwrap_or_default();
    if status >= 400 {
        return Err(bad(&api_error(status, &body)));
    }
    serde_json::from_str(&body).map_err(|_| bad("App Store Connect answered something not JSON"))
}

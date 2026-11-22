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
    send(Method::Get, token, path, None).await
}

/// PATCHes a JSON:API document to `path` with `token`, and answers the JSON body.
pub async fn patch(token: &str, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    send(Method::Patch, token, path, Some(body)).await
}

/// One part of a file to send to the URL Apple gave for it, as App Store Connect's
/// `uploadOperations` describe it. Its URL is presigned, so no token goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub method: String,
    pub url: String,
    pub offset: usize,
    pub length: usize,
    pub headers: Vec<(String, String)>,
}

/// The parts `operations` ask for, checked against a file of `size` bytes: each an https URL
/// with PUT or POST, inside the file, together covering it exactly once. None with the reason
/// otherwise, before anything is sent.
pub fn parts(
    operations: &serde_json::Value,
    size: usize,
) -> std::result::Result<Vec<Part>, String> {
    let ops = operations
        .as_array()
        .ok_or("Apple gave no upload operations")?;
    let mut out = Vec::new();
    for op in ops {
        let method = op["method"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if method != "PUT" && method != "POST" {
            return Err(format!("an upload operation asks for {method}"));
        }
        let url = op["url"].as_str().unwrap_or_default();
        if !url.starts_with("https://") {
            return Err("an upload operation's URL is not https".into());
        }
        let (Some(offset), Some(length)) = (op["offset"].as_u64(), op["length"].as_u64()) else {
            return Err("an upload operation has no offset or length".into());
        };
        let headers = op["requestHeaders"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|h| {
                Some((
                    h["name"].as_str()?.to_string(),
                    h["value"].as_str()?.to_string(),
                ))
            })
            .collect();
        out.push(Part {
            method,
            url: url.to_string(),
            offset: offset as usize,
            length: length as usize,
            headers,
        });
    }
    out.sort_by_key(|p| p.offset);
    let mut next = 0;
    for p in &out {
        if p.offset != next || p.length == 0 {
            return Err("the upload operations do not cover the file in order".into());
        }
        next = p.offset + p.length;
    }
    if next != size {
        return Err(format!(
            "the upload operations cover {next} bytes of {size}"
        ));
    }
    Ok(out)
}

/// Sends `bytes` as `part` to its presigned URL. Answers the status, or an error over 299.
pub async fn send_part(part: &Part, bytes: &[u8]) -> Result<u16> {
    let headers = Headers::new();
    for (k, v) in &part.headers {
        headers.set(k, v)?;
    }
    let body = js_sys::Uint8Array::from(&bytes[part.offset..part.offset + part.length]);
    let mut init = RequestInit::new();
    init.with_method(Method::from(part.method.clone()))
        .with_headers(headers)
        .with_body(Some(body.into()));
    let res = Fetch::Request(Request::new_with_init(&part.url, &init)?)
        .send()
        .await?;
    let status = res.status_code();
    if status > 299 {
        return Err(bad(&format!(
            "Apple's upload URL answered {status} for bytes {}..{}",
            part.offset,
            part.offset + part.length
        )));
    }
    Ok(status)
}

/// A PKCS#8 DER private key as PEM (`PRIVATE KEY`, 64-character lines).
pub fn pkcs8_pem(der: &[u8]) -> String {
    let body = b64(der);
    let mut out = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END PRIVATE KEY-----\n");
    out
}

/// Whether `v` is a version string App Store Connect takes: one to three dot-separated numbers.
pub fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (1..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// POSTs a JSON:API document to `path` with `token`, and answers the JSON body.
pub async fn post(token: &str, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    send(Method::Post, token, path, Some(body)).await
}

async fn send(
    method: Method,
    token: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    headers.set("Accept", "application/json")?;
    if body.is_some() {
        headers.set("Content-Type", "application/json")?;
    }
    let mut init = RequestInit::new();
    init.with_method(method)
        .with_headers(headers)
        .with_body(body.map(|b| JsValue::from_str(&b.to_string())));
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

/// Standard base64 with padding, as App Store Connect takes a CSR and gives a certificate.
pub fn b64(bytes: &[u8]) -> String {
    let mut s: String = b64url(bytes)
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            c => c,
        })
        .collect();
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    s
}

/// The bytes of standard base64 (padding and whitespace allowed). None for anything else.
pub fn unb64(s: &str) -> Option<Vec<u8>> {
    if s.contains(['-', '_']) {
        return None;
    }
    let url: String = s
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    crate::webauthn::unb64url(&url).ok()
}

/// A certificate signing request (PKCS#10) for an Apple Distribution certificate, in pure DER.
/// WebCrypto makes the RSA key and signs; this module only lays out the bytes.
pub mod csr {
    /// sha256WithRSAEncryption, 1.2.840.113549.1.1.11.
    const SHA256_WITH_RSA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
    /// commonName, 2.5.4.3.
    const COMMON_NAME: [u8; 3] = [0x55, 0x04, 0x03];

    /// One DER element: tag, length (short or long form), content.
    pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let n = content.len();
        if n < 0x80 {
            out.push(n as u8);
        } else {
            let bytes: Vec<u8> = n
                .to_be_bytes()
                .into_iter()
                .skip_while(|b| *b == 0)
                .collect();
            out.push(0x80 | bytes.len() as u8);
            out.extend(bytes);
        }
        out.extend_from_slice(content);
        out
    }

    fn seq(parts: &[&[u8]]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }

    /// The signature algorithm: `SEQUENCE { OID sha256WithRSAEncryption, NULL }`.
    pub fn algorithm() -> Vec<u8> {
        seq(&[&tlv(0x06, &SHA256_WITH_RSA), &[0x05, 0x00]])
    }

    /// `CertificationRequestInfo`: version 0, the subject `CN=common_name`, the public key as
    /// WebCrypto exports it (`spki`, a `SubjectPublicKeyInfo`), and no attributes. These are the
    /// bytes the key signs.
    pub fn request_info(spki_der: &[u8], common_name: &str) -> Vec<u8> {
        let cn = seq(&[&tlv(0x06, &COMMON_NAME), &tlv(0x0c, common_name.as_bytes())]);
        let subject = seq(&[&tlv(0x31, &cn)]);
        seq(&[&[0x02, 0x01, 0x00], &subject, spki_der, &[0xa0, 0x00]])
    }

    /// The request: the info, the algorithm, and the signature as a BIT STRING.
    pub fn request(info: &[u8], signature: &[u8]) -> Vec<u8> {
        let mut bits = vec![0x00];
        bits.extend_from_slice(signature);
        seq(&[info, &algorithm(), &tlv(0x03, &bits)])
    }
}

/// What "Create signing identity" makes before Apple sees anything: a fresh RSA-2048 key
/// (PKCS#8 DER, to be sealed) and a CSR for it (DER). The key is made by WebCrypto, extractable
/// only so that it can be sealed, and signs its own request.
pub struct NewKey {
    pub pkcs8: Vec<u8>,
    pub csr: Vec<u8>,
}

/// Makes a [`NewKey`] whose request names `common_name`.
pub async fn new_key(common_name: &str) -> Result<NewKey> {
    let subtle = Reflect::get(
        &Reflect::get(&js_sys::global(), &"crypto".into())?,
        &"subtle".into(),
    )?;
    let call = |name: &str, args: &Array| -> Result<JsFuture> {
        let f: Function = Reflect::get(&subtle, &name.into())?.dyn_into()?;
        let p = f.apply(&subtle, args)?;
        Ok(JsFuture::from(Promise::resolve(&p)))
    };
    let alg = JSON::parse(r#"{"name":"RSASSA-PKCS1-v1_5","modulusLength":2048,"hash":"SHA-256"}"#)?;
    Reflect::set(
        &alg,
        &"publicExponent".into(),
        &Uint8Array::from(&[0x01u8, 0x00, 0x01][..]),
    )?;
    let pair = call(
        "generateKey",
        &Array::of3(
            &alg,
            &JsValue::TRUE,
            &Array::of2(&"sign".into(), &"verify".into()),
        ),
    )?
    .await?;
    let private = Reflect::get(&pair, &"privateKey".into())?;
    let public = Reflect::get(&pair, &"publicKey".into())?;
    let export = |format: &str, key: &JsValue| call("exportKey", &Array::of2(&format.into(), key));
    let spki = Uint8Array::new(&export("spki", &public)?.await?).to_vec();
    let pkcs8 = Uint8Array::new(&export("pkcs8", &private)?.await?).to_vec();
    let info = csr::request_info(&spki, common_name);
    let signature = call(
        "sign",
        &Array::of3(
            &"RSASSA-PKCS1-v1_5".into(),
            &private,
            &Uint8Array::from(info.as_slice()),
        ),
    )?
    .await?;
    let signature = Uint8Array::new(&signature).to_vec();
    if signature.len() != 256 {
        return Err(bad(
            "the new key gave a signature of the wrong length for RSA-2048",
        ));
    }
    Ok(NewKey {
        pkcs8,
        csr: csr::request(&info, &signature),
    })
}

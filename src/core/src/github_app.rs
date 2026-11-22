//! GitHub App authentication and the few REST calls pigeon needs.
//!
//! This block wraps three things. First, turning a downloaded GitHub App
//! private key (PEM) into PKCS#8 DER and signing an app JWT with WebCrypto.
//! Second, checking and reading webhook deliveries. Third, a small set of
//! GitHub REST calls: installation tokens, deployments and deployment statuses.
//! pigeon talks to GitHub over its MCP server everywhere else; these are what
//! that server has no tool for.
//!
//! Everything that can be pure is pure, so it runs in native tests. The async
//! functions at the bottom are thin wrappers over those pure functions.

use serde::Serialize;
use serde_json::{Map, Value, json};
use worker::js_sys::{self, Array, Function, JSON, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

const API_BASE: &str = "https://api.github.com";
const JWT_HEADER: &str = r#"{"alg":"RS256","typ":"JWT"}"#;
const MAX_DESCRIPTION: usize = 140;

/// Decodes standard base64 (padded, with whitespace allowed anywhere).
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut count: usize = 0;
    let mut padding: usize = 0;
    for byte in text.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return None;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(value);
        bits += 6;
        count += 1;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1u32 << bits) - 1;
        }
    }
    if count % 4 == 1 || padding > 2 {
        return None;
    }
    if padding > 0 && !(count + padding).is_multiple_of(4) {
        return None;
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Splits a PEM text into its label and its base64 body.
fn pem_block(pem: &str) -> Option<(&str, &str)> {
    const BEGIN: &str = "-----BEGIN ";
    const DASHES: &str = "-----";
    const END: &str = "-----END ";
    let start = pem.find(BEGIN)? + BEGIN.len();
    let rest = pem.get(start..)?;
    let label_end = rest.find(DASHES)?;
    let label = rest.get(..label_end)?;
    let after = rest.get(label_end + DASHES.len()..)?;
    let end = after.find(END)?;
    let body = after.get(..end)?;
    Some((label.trim(), body))
}

/// Returns the DER bytes inside a PEM block of any label.
///
/// The body must be standard base64. Line breaks and spaces are ignored.
pub fn pem_der(pem: &str) -> Option<Vec<u8>> {
    let (_, body) = pem_block(pem)?;
    base64_decode(body)
}

/// Encodes a DER length in short or long form.
fn der_length(len: usize) -> Vec<u8> {
    if len < 128 {
        return vec![len as u8];
    }
    let significant: Vec<u8> = len
        .to_be_bytes()
        .iter()
        .copied()
        .skip_while(|byte| *byte == 0)
        .collect();
    let mut out = Vec::with_capacity(significant.len() + 1);
    out.push(0x80 | significant.len() as u8);
    out.extend_from_slice(&significant);
    out
}

/// Builds one DER element: tag, length, content.
fn der_element(tag: u8, content: &[u8]) -> Vec<u8> {
    let length = der_length(content.len());
    let mut out = Vec::with_capacity(1 + length.len() + content.len());
    out.push(tag);
    out.extend_from_slice(&length);
    out.extend_from_slice(content);
    out
}

/// Wraps an RSA PKCS#1 `RSAPrivateKey` DER in a PKCS#8 `PrivateKeyInfo`.
///
/// The result is `SEQUENCE { INTEGER 0, SEQUENCE { rsaEncryption OID, NULL },
/// OCTET STRING { der } }`.
pub fn pkcs1_to_pkcs8(der: &[u8]) -> Vec<u8> {
    let version: [u8; 3] = [0x02, 0x01, 0x00];
    let algorithm: [u8; 15] = [
        0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01, 0x05, 0x00,
    ];
    let key = der_element(0x04, der);
    let mut inner = Vec::with_capacity(version.len() + algorithm.len() + key.len());
    inner.extend_from_slice(&version);
    inner.extend_from_slice(&algorithm);
    inner.extend_from_slice(&key);
    der_element(0x30, &inner)
}

/// Returns PKCS#8 DER for a GitHub App private key in PEM form.
///
/// "BEGIN PRIVATE KEY" is returned as it is. "BEGIN RSA PRIVATE KEY" is
/// wrapped into PKCS#8. Any other label gives `None`.
pub fn private_key_der(pem: &str) -> Option<Vec<u8>> {
    let (label, body) = pem_block(pem)?;
    let der = base64_decode(body)?;
    match label {
        "PRIVATE KEY" => Some(der),
        "RSA PRIVATE KEY" => Some(pkcs1_to_pkcs8(&der)),
        _ => None,
    }
}

/// The claims of the app JWT, in the order GitHub documents them.
#[derive(Serialize)]
struct Claims<'a> {
    iat: u64,
    exp: u64,
    iss: &'a str,
}

/// Builds the text that gets signed for an app JWT.
///
/// It is the base64url header, a dot, and the base64url claims. The claims
/// are `iat` (now minus 60), `exp` (now plus 540) and `iss` (the client id).
pub fn jwt_signing_input(client_id: &str, now_secs: u64) -> String {
    let claims = Claims {
        iat: now_secs.saturating_sub(60),
        exp: now_secs.saturating_add(540),
        iss: client_id,
    };
    let claims_json = serde_json::to_string(&claims).unwrap_or_default();
    format!(
        "{}.{}",
        crate::webauthn::b64url(JWT_HEADER.as_bytes()),
        crate::webauthn::b64url(claims_json.as_bytes())
    )
}

/// Checks the `X-Hub-Signature-256` header of a webhook delivery.
///
/// The header must look like `sha256=<hex>`. The comparison is constant time.
/// A missing or malformed header gives `false`.
pub fn verify_webhook(secret: &[u8], body: &[u8], signature_header: Option<&str>) -> bool {
    let Some(header) = signature_header else {
        return false;
    };
    let Some(hex) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    crate::sign::verify_hex(secret, body, hex)
}

/// The parts of a push event that pigeon cares about.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Push {
    /// The repository as `owner/name`.
    pub repo: String,
    /// The pushed ref, for example `refs/heads/main`.
    pub git_ref: String,
    /// The commit id the ref points at after the push.
    pub after: String,
    /// The id of the GitHub App installation that got the event.
    pub installation: u64,
    /// True when the push deleted the ref.
    pub deleted: bool,
    /// The message of the head commit, if there is one.
    pub head_message: Option<String>,
}

/// Reads a webhook delivery into a [`Push`].
///
/// Events other than "push" give `Ok(None)`. A push with a missing required
/// field gives an `Err` with a short reason.
pub fn read_push(event: Option<&str>, body: &[u8]) -> std::result::Result<Option<Push>, String> {
    if event != Some("push") {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "body is not valid JSON".to_string())?;
    let text = |pointer: &str, name: &str| -> std::result::Result<String, String> {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing {name}"))
    };
    let repo = text("/repository/full_name", "repository.full_name")?;
    let git_ref = text("/ref", "ref")?;
    let after = text("/after", "after")?;
    let installation = value
        .pointer("/installation/id")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing installation.id".to_string())?;
    let deleted = value
        .pointer("/deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head_message = value
        .pointer("/head_commit/message")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(Some(Push {
        repo,
        git_ref,
        after,
        installation,
        deleted,
        head_message,
    }))
}

/// Builds the body for creating an installation token.
///
/// Repositories are given as `owner/name` or `name`. Only the name is sent.
/// A key is left out when its list is empty.
pub fn token_request(repos: &[&str], permissions: &[(&str, &str)]) -> Value {
    let mut body = Map::new();
    if !repos.is_empty() {
        let names: Vec<Value> = repos
            .iter()
            .map(|repo| {
                let name = repo.rsplit_once('/').map_or(*repo, |(_, name)| name);
                Value::String(name.to_string())
            })
            .collect();
        body.insert("repositories".to_string(), Value::Array(names));
    }
    if !permissions.is_empty() {
        let mut map = Map::new();
        for (key, level) in permissions {
            map.insert((*key).to_string(), Value::String((*level).to_string()));
        }
        body.insert("permissions".to_string(), Value::Object(map));
    }
    Value::Object(body)
}

/// Builds the body for creating a deployment.
///
/// It never auto-merges and skips required status checks.
pub fn deployment_body(git_ref: &str, environment: &str, description: &str) -> Value {
    json!({
        "ref": git_ref,
        "environment": environment,
        "description": description,
        "auto_merge": false,
        "required_contexts": [],
        "production_environment": true,
    })
}

/// Builds the body for a deployment status.
///
/// The description is cut to 140 characters. Absent URLs are left out.
pub fn status_body(
    state: &str,
    environment_url: Option<&str>,
    log_url: Option<&str>,
    description: &str,
) -> Value {
    let description: String = description.chars().take(MAX_DESCRIPTION).collect();
    let mut body = Map::new();
    body.insert("state".to_string(), Value::String(state.to_string()));
    body.insert("description".to_string(), Value::String(description));
    body.insert("auto_inactive".to_string(), Value::Bool(true));
    if let Some(url) = environment_url {
        body.insert(
            "environment_url".to_string(),
            Value::String(url.to_string()),
        );
    }
    if let Some(url) = log_url {
        body.insert("log_url".to_string(), Value::String(url.to_string()));
    }
    Value::Object(body)
}

/// Signs an app JWT with the PKCS#8 private key using WebCrypto.
///
/// The result is the signing input, a dot, and the base64url signature.
pub async fn sign_jwt(pkcs8_der: &[u8], client_id: &str, now_secs: u64) -> Result<String> {
    let input = jwt_signing_input(client_id, now_secs);
    let subtle = Reflect::get(
        &Reflect::get(&js_sys::global(), &"crypto".into())?,
        &"subtle".into(),
    )?;
    let alg = JSON::parse(r#"{"name":"RSASSA-PKCS1-v1_5","hash":"SHA-256"}"#)?;
    let key_bytes = Uint8Array::from(pkcs8_der);
    let import: Function = Reflect::get(&subtle, &"importKey".into())?.dyn_into()?;
    let args = Array::of5(
        &"pkcs8".into(),
        &key_bytes,
        &alg,
        &JsValue::FALSE,
        &Array::of1(&"sign".into()),
    );
    let key = JsFuture::from(Promise::resolve(&import.apply(&subtle, &args)?)).await?;
    let sign: Function = Reflect::get(&subtle, &"sign".into())?.dyn_into()?;
    let data = Uint8Array::from(input.as_bytes());
    let sign_args = Array::of3(&"RSASSA-PKCS1-v1_5".into(), &key, &data);
    let buffer = JsFuture::from(Promise::resolve(&sign.apply(&subtle, &sign_args)?)).await?;
    let signature = Uint8Array::new(&buffer).to_vec();
    Ok(format!("{input}.{}", crate::webauthn::b64url(&signature)))
}

/// An installation access token and when it expires.
///
/// There is no `Debug` derive on purpose: the token is a secret.
pub struct Installation {
    /// The installation access token.
    pub token: String,
    /// The expiry time as GitHub sends it (ISO 8601).
    pub expires_at: String,
}

/// Calls the GitHub API and answers the parsed JSON body.
///
/// Errors look like `{path}: {status}: {message}`. The token is never part
/// of an error.
async fn api(
    token_kind: &str,
    token: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let headers = Headers::new();
    headers.set("Accept", "application/vnd.github+json")?;
    headers.set("X-GitHub-Api-Version", "2022-11-28")?;
    headers.set("User-Agent", "pigeon")?;
    headers.set("Authorization", &format!("{token_kind} {token}"))?;
    if body.is_some() {
        headers.set("Content-Type", "application/json")?;
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(body) = body {
        init.with_body(Some(JsValue::from_str(&body.to_string())));
    }
    let request = Request::new_with_init(&format!("{API_BASE}{path}"), &init)?;
    let mut response = Fetch::Request(request).send().await?;
    let status = response.status_code();
    let text = response.text().await?;
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let message = parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("request failed");
        return Err(Error::RustError(format!("{path}: {status}: {message}")));
    }
    Ok(parsed)
}

/// Creates an installation access token, authenticating with an app JWT.
pub async fn installation_token(
    jwt: &str,
    installation: u64,
    body: &Value,
) -> Result<Installation> {
    let path = format!("/app/installations/{installation}/access_tokens");
    let answer = api("Bearer", jwt, Method::Post, &path, Some(body)).await?;
    let token = answer.get("token").and_then(Value::as_str);
    let expires_at = answer.get("expires_at").and_then(Value::as_str);
    match (token, expires_at) {
        (Some(token), Some(expires_at)) => Ok(Installation {
            token: token.to_string(),
            expires_at: expires_at.to_string(),
        }),
        _ => Err(Error::RustError(format!(
            "{path}: answer had no token or expiry"
        ))),
    }
}

/// The App itself, as GitHub describes it to the App's JWT: answers its `id`, `slug` and the
/// rest. A key that signs a JWT GitHub accepts here belongs to that App.
pub async fn app(jwt: &str) -> Result<Value> {
    api("Bearer", jwt, Method::Get, "/app", None).await
}

/// Creates a deployment and answers its id.
///
/// GitHub answers 202 without an id when it merged the base branch instead
/// of deploying. That case is an error.
pub async fn create_deployment(token: &str, repo: &str, body: &Value) -> Result<u64> {
    let path = format!("/repos/{repo}/deployments");
    let answer = api("Bearer", token, Method::Post, &path, Some(body)).await?;
    answer
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::RustError("GitHub merged instead of deploying".to_string()))
}

/// Sets the status of a deployment.
pub async fn create_status(token: &str, repo: &str, deployment: u64, body: &Value) -> Result<()> {
    let path = format!("/repos/{repo}/deployments/{deployment}/statuses");
    api("Bearer", token, Method::Post, &path, Some(body)).await?;
    Ok(())
}

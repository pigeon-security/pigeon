//! Cloudflare as an OAuth provider, with pigeon as the client.
//!
//! This block wraps the authorization code flow with PKCE S256 and a
//! client secret sent by HTTP Basic. A person uses it to link another
//! Cloudflare account. The pure functions build URLs and forms and read
//! answers. The thin async functions send them with `worker::Fetch`.
//!
//! A refresh token may be single-use. Callers must store the tokens that
//! `refresh` returns before they use the new access token.
//!
//! Tokens and secrets are never put into error text or Debug output.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::git::base64;
use crate::oauth::{encode, form};
use crate::webauthn::{b64url, random};

const AUTHORIZE_URL: &str = "https://dash.cloudflare.com/oauth2/auth";
const TOKEN_URL: &str = "https://dash.cloudflare.com/oauth2/token";
const REVOKE_URL: &str = "https://dash.cloudflare.com/oauth2/revoke";

/// Refresh when fewer than this many milliseconds remain (5 minutes).
const REFRESH_MARGIN_MS: f64 = 300_000.0;

/// The OAuth client settings that are not secret.
#[derive(Clone, Debug)]
pub struct Client {
    /// The OAuth client ID issued by Cloudflare.
    pub client_id: String,
    /// The redirect URI registered for this client.
    pub redirect_uri: String,
    /// The scope IDs to request, such as `account.read`.
    pub scopes: Vec<String>,
}

/// The tokens we hold for one linked account.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Tokens {
    /// The access token.
    pub access: String,
    /// The refresh token, when Cloudflare gave one.
    pub refresh: Option<String>,
    /// When the access token expires, in milliseconds since the epoch.
    pub expires_at_ms: f64,
    /// The granted scopes, space-separated, when reported.
    pub scope: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// The JSON shape of a token endpoint answer, success or error.
#[derive(Deserialize)]
struct Answer {
    access_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<f64>,
    refresh_token: Option<String>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Make a new PKCE code verifier from 32 random bytes.
pub fn verifier() -> Result<String> {
    random()
}

/// Compute the S256 code challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    b64url(&digest)
}

/// Join key and value pairs into a form string, encoding each value.
fn encode_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Build the URL that sends the person to Cloudflare to approve access.
pub fn authorize_url(client: &Client, state: &str, challenge: &str) -> String {
    let scope = client.scopes.join(" ");
    let query = encode_pairs(&[
        ("response_type", "code"),
        ("client_id", &client.client_id),
        ("redirect_uri", &client.redirect_uri),
        ("scope", &scope),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
    ]);
    format!("{AUTHORIZE_URL}?{query}")
}

/// Build the form body that trades an authorization code for tokens.
pub fn code_form(client: &Client, code: &str, verifier: &str) -> String {
    encode_pairs(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &client.redirect_uri),
        ("code_verifier", verifier),
    ])
}

/// Build the form body that trades a refresh token for new tokens.
pub fn refresh_form(refresh: &str) -> String {
    encode_pairs(&[("grant_type", "refresh_token"), ("refresh_token", refresh)])
}

/// Build the form body that revokes a token.
pub fn revoke_form(token: &str, hint: &str) -> String {
    encode_pairs(&[("token", token), ("token_type_hint", hint)])
}

/// Build the `Authorization` header value for `client_secret_basic`.
pub fn basic(client_id: &str, secret: &str) -> String {
    let pair = format!("{}:{}", encode(client_id), encode(secret));
    format!("Basic {}", base64(pair.as_bytes()))
}

/// Join an error code and its optional description into one line.
fn error_line(error: &str, description: Option<&str>) -> String {
    match description {
        Some(d) if !d.is_empty() => format!("{error}: {d}"),
        _ => error.to_string(),
    }
}

/// Read a token endpoint answer into tokens.
///
/// A missing `refresh_token` falls back to `previous_refresh`. The error
/// text never contains a token.
pub fn read_tokens(
    body: &str,
    now_ms: f64,
    previous_refresh: Option<&str>,
) -> std::result::Result<Tokens, String> {
    let answer: Answer =
        serde_json::from_str(body).map_err(|_| "token answer is not valid JSON".to_string())?;
    if let Some(error) = answer.error.as_deref() {
        return Err(error_line(error, answer.error_description.as_deref()));
    }
    if let Some(kind) = answer.token_type.as_deref()
        && !kind.eq_ignore_ascii_case("bearer")
    {
        return Err("token answer has a token_type that is not bearer".to_string());
    }
    let access = match answer.access_token {
        Some(a) if !a.is_empty() => a,
        _ => return Err("token answer has no access_token".to_string()),
    };
    let expires_in = answer
        .expires_in
        .ok_or_else(|| "token answer has no expires_in".to_string())?;
    if !expires_in.is_finite() || expires_in < 0.0 {
        return Err("token answer has an invalid expires_in".to_string());
    }
    let refresh = answer
        .refresh_token
        .filter(|r| !r.is_empty())
        .or_else(|| previous_refresh.map(str::to_string));
    Ok(Tokens {
        access,
        refresh,
        expires_at_ms: now_ms + expires_in * 1000.0,
        scope: answer.scope,
    })
}

/// Tell whether the access token has under five minutes left.
pub fn needs_refresh(tokens: &Tokens, now_ms: f64) -> bool {
    let remaining = tokens.expires_at_ms - now_ms;
    remaining.is_nan() || remaining < REFRESH_MARGIN_MS
}

/// Find the first value for a key in parsed form pairs.
fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Read the code and state from the redirect's query string.
///
/// The query is given without the leading `?`. An `error` parameter
/// becomes an error with its description when there is one.
pub fn callback_params(query: &str) -> std::result::Result<(String, String), String> {
    let pairs = form(query);
    if let Some(error) = lookup(&pairs, "error") {
        return Err(error_line(error, lookup(&pairs, "error_description")));
    }
    let code = lookup(&pairs, "code")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "callback has no code".to_string())?;
    let state = lookup(&pairs, "state")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "callback has no state".to_string())?;
    Ok((code.to_string(), state.to_string()))
}

/// Build the error for a non-2xx answer, with the status and any detail.
fn failure(what: &str, status: u16, body: &str) -> Error {
    let detail = match read_tokens(body, 0.0, None) {
        Err(text) => text,
        Ok(_) => "no error detail".to_string(),
    };
    Error::RustError(format!("{what} failed with status {status}: {detail}"))
}

/// Send one form POST to Cloudflare with client authentication.
async fn post_form(
    url: &str,
    client_id: &str,
    secret: &str,
    body: String,
) -> Result<(u16, String)> {
    let headers = Headers::new();
    headers.set("Authorization", &basic(client_id, secret))?;
    headers.set("Content-Type", "application/x-www-form-urlencoded")?;
    headers.set("Accept", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    init.with_headers(headers);
    init.with_body(Some(body.into()));
    let req = Request::new_with_init(url, &init)?;
    let mut res = Fetch::Request(req).send().await?;
    let status = res.status_code();
    let text = res.text().await?;
    Ok((status, text))
}

/// Trade an authorization code for tokens.
pub async fn exchange(
    client: &Client,
    secret: &str,
    code: &str,
    verifier: &str,
    now_ms: f64,
) -> Result<Tokens> {
    let body = code_form(client, code, verifier);
    let (status, text) = post_form(TOKEN_URL, &client.client_id, secret, body).await?;
    if !(200..300).contains(&status) {
        return Err(failure("token request", status, &text));
    }
    read_tokens(&text, now_ms, None).map_err(Error::RustError)
}

/// Trade a refresh token for new tokens.
///
/// A missing refresh token in the answer keeps the old one. Store the
/// result before using the new access token, as the old refresh token may
/// no longer work.
pub async fn refresh(client_id: &str, secret: &str, refresh: &str, now_ms: f64) -> Result<Tokens> {
    let body = refresh_form(refresh);
    let (status, text) = post_form(TOKEN_URL, client_id, secret, body).await?;
    if !(200..300).contains(&status) {
        return Err(failure("token refresh", status, &text));
    }
    read_tokens(&text, now_ms, Some(refresh)).map_err(Error::RustError)
}

/// Revoke a refresh token. Any 2xx answer counts as success.
pub async fn revoke(client_id: &str, secret: &str, token: &str) -> Result<()> {
    let body = revoke_form(token, "refresh_token");
    let (status, text) = post_form(REVOKE_URL, client_id, secret, body).await?;
    if !(200..300).contains(&status) {
        return Err(failure("token revoke", status, &text));
    }
    Ok(())
}

/// Read `(id, name)` pairs from the `result` of an accounts listing.
fn accounts_from(value: &Value) -> Result<Vec<(String, String)>> {
    let list = value
        .as_array()
        .ok_or_else(|| Error::RustError("accounts answer is not a list".to_string()))?;
    Ok(list
        .iter()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?;
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            Some((id.to_string(), name.to_string()))
        })
        .collect())
}

/// List the accounts the access token was granted, as `(id, name)` pairs.
pub async fn granted_accounts(access: &str) -> Result<Vec<(String, String)>> {
    let value = crate::cloudflare::call(access, Method::Get, "/accounts?per_page=50", None).await?;
    accounts_from(&value)
}

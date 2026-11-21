//! OAuth 2.1 pieces for an MCP server, as the MCP authorization spec asks: the protected
//! resource and authorization server metadata, PKCE (S256 only), redirect URI checks, and
//! tokens, which are random and kept only as hashes.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use worker::Result;

use crate::webauthn::{b64url, random};

/// The resource metadata (RFC 9728) of `resource`, served by `issuer`'s authorization server.
pub fn resource_metadata(resource: &str, issuer: &str) -> Value {
    json!({
        "resource": resource,
        "authorization_servers": [issuer],
        "bearer_methods_supported": ["header"],
    })
}

/// The authorization server metadata (RFC 8414) of `issuer`, with its endpoints under
/// `/oauth/`.
pub fn server_metadata(issuer: &str) -> Value {
    json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    })
}

/// The `WWW-Authenticate` value that sends an unauthenticated client to `resource_metadata`.
pub fn challenge(resource_metadata_url: &str) -> String {
    format!(r#"Bearer resource_metadata="{resource_metadata_url}""#)
}

/// Whether `verifier` answers PKCE `challenge`: base64url(SHA-256(verifier)).
pub fn pkce(verifier: &str, challenge: &str) -> bool {
    (43..=128).contains(&verifier.len())
        && b64url(&Sha256::digest(verifier.as_bytes())) == challenge
}

/// Whether `uri` may be a redirect: HTTPS, or HTTP to the loopback address.
pub fn redirect_allowed(uri: &str) -> bool {
    uri.starts_with("https://")
        || uri.starts_with("http://localhost")
        || uri.starts_with("http://127.0.0.1")
}

/// A new token with `prefix` (such as `pgn_at_`), and the hash it is kept as.
pub fn token(prefix: &str) -> Result<(String, String)> {
    let token = format!("{prefix}{}", random()?);
    let hash = hash(&token);
    Ok((token, hash))
}

/// How a token, code or secret is kept: base64url(SHA-256).
pub fn hash(secret: &str) -> String {
    b64url(&Sha256::digest(secret.as_bytes()))
}

/// `body`'s `application/x-www-form-urlencoded` fields, decoded.
pub fn form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `value` percent-encoded for a URL query.
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

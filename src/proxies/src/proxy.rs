//! The proxy credential at the proxies: what a cloud session holds to reach `/git/` and `/cf/`,
//! and nothing else. A proxy credential is a bearer token of its own kind (`pgn_px_…`), so the
//! MCP endpoint, which looks up only access tokens, never accepts one; and the proxies accept
//! nothing else. Who holds one is asked of the [`Host`]. Every proxied request is logged as one
//! audit line: who, which route, and what came of it. Issuing and revoking credentials is
//! core's (`pigeon_capabilities::proxy::Tokens`).

use serde::Serialize;
use serde_json::json;
use worker::{Error, Request, Result, console_log};

use crate::Host;

pub use pigeon_core::proxy_caller::{ID_LEN, ProxyCaller};

/// What every proxy credential starts with.
pub const PREFIX: &str = "pgn_px_";

/// Whether `path` holds an encoded separator or dot, a backslash, or a dot segment.
fn suspicious(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%2e")
        || path.contains('\\')
        || path.split('/').any(|seg| seg == ".." || seg == ".")
}

/// Whether a bearer token of `kind` may reach raw `path`: a `proxy` token only the proxies
/// under `/git/` and `/cf/`, an `access` token only `/mcp`, any other kind nothing. No kind
/// reaches a path with an encoded separator or dot, a backslash, or a dot segment.
pub fn reaches(kind: &str, path: &str) -> bool {
    if suspicious(path) {
        return false;
    }
    match kind {
        "proxy" => path.starts_with("/git/") || path.starts_with("/cf/"),
        "access" => path == "/mcp",
        _ => false,
    }
}

/// What came of a proxied request.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Sent on.
    Forwarded,
    /// Refused before anything was sent on.
    Refused,
    /// Sent on, or about to be, and it failed.
    Failed,
}

/// One proxied request, for the audit line.
pub struct Audit<'a> {
    /// `git` or `cf`.
    pub route: &'a str,
    /// The HTTP method.
    pub method: &'a str,
    /// The request's path; a query string is cut off before it is logged.
    pub path: &'a str,
    /// What came of it.
    pub outcome: Outcome,
    /// The response's status, when there is one.
    pub status: Option<u16>,
    /// Why, when refused or failed.
    pub why: Option<&'a str>,
}

/// `a` as one JSON audit line. It holds the credential's id, never its value or hash.
pub fn audit_line(caller: Option<&ProxyCaller>, a: &Audit<'_>) -> String {
    let path = a.path.split_once('?').map_or(a.path, |(p, _)| p);
    json!({
        "audit": "proxy",
        "route": a.route,
        "caller": caller.map(|c| format!("{} ({})", c.label, c.person)),
        "token": caller.map(|c| c.id.as_str()),
        "method": a.method,
        "path": path,
        "outcome": a.outcome,
        "status": a.status,
        "why": a.why,
    })
    .to_string()
}

/// Whether a request for `path` on `host` is served, when the proxies have `proxy_host` to
/// themselves: there, only the proxies and `/health`; anywhere else, everything but the
/// proxies. So the credential a cloud session's network proxy adds for the proxy host never
/// reaches the platform's other paths, and the proxies never answer on its other hosts.
pub fn host_serves(host: &str, proxy_host: &str, path: &str) -> bool {
    let proxied = path.starts_with("/git/") || path.starts_with("/cf/");
    if host.eq_ignore_ascii_case(proxy_host) {
        proxied || path == "/health"
    } else {
        !proxied
    }
}

/// Writes `a`'s audit line to the Worker's log.
pub fn audit(caller: Option<&ProxyCaller>, a: &Audit<'_>) {
    console_log!("{}", audit_line(caller, a));
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// The bearer token `req` carries, if any.
pub fn bearer(req: &Request) -> Result<Option<String>> {
    Ok(req.headers().get("Authorization")?.and_then(|h| {
        let (scheme, rest) = h.trim().split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| rest.trim().to_string())
    }))
}

/// The proxy credential `req` carries as `Authorization: Bearer`, when it has a proxy
/// credential's prefix and the request's path is one a proxy credential reaches; why not
/// otherwise. Errors never repeat the header.
pub fn presented(req: &Request) -> Result<String> {
    let header = req
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .ok_or_else(|| bad("no proxy credential"))?;
    let (scheme, rest) = header
        .trim()
        .split_once(' ')
        .ok_or_else(|| bad("no proxy credential"))?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(bad("no proxy credential"));
    }
    let presented = rest.trim();
    if !presented.starts_with(PREFIX) {
        return Err(bad("not a proxy credential"));
    }
    if !reaches("proxy", &req.path()) {
        return Err(bad("a proxy credential does not reach this path"));
    }
    Ok(presented.to_string())
}

/// The holder of the proxy credential `req` carries as `Authorization: Bearer`, if it is live
/// and reaches the request's path, as `host` knows it. Errors never repeat the header.
pub async fn caller(host: &dyn Host, req: &Request) -> Result<ProxyCaller> {
    let unknown = || bad("the proxy credential is unknown or has expired");
    let presented = presented(req)?;
    match host.verify(&presented).await {
        Ok(Some(c)) => Ok(c),
        Ok(None) | Err(_) => Err(unknown()),
    }
}

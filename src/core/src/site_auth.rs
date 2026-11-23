//! Passkey sign-in for a site pigeon-sites serves (docs/plans/sites.md, part A). Everything
//! here is pure: the config read from a host's entry, the session cookie, who may sign in, and
//! which requests the gate lets through. pigeon-sites calls it; nothing here calls a Worker.
//! A host with no config is never touched.

use serde_json::Value;

use crate::sign::{equal, hex, hmac_sha256, unhex};

/// How long a session lasts, in milliseconds: 12 hours.
pub const SESSION_MS: i64 = 12 * 3_600_000;

/// The session cookie's name. The `__Host-` prefix makes a browser keep it host-only and secure.
pub const COOKIE: &str = "__Host-pigeon-site";

/// What a gated host asks for: who may sign in.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// User ids, or "site" for every site user of the host.
    pub allow: Vec<String>,
}

/// A verified session.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// The user the session was issued to.
    pub user_id: String,
    /// When the session ends, in milliseconds since the epoch.
    pub expires_ms: i64,
}

/// The pages and calls pigeon-sites answers itself on a gated host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reserved {
    /// `GET /_pigeon/passkey.js`: the browser's script.
    Script,
    /// `GET /_pigeon/sign-in`: the sign-in page.
    SignInPage,
    /// `POST /_pigeon/sign-in/begin`.
    SignInBegin,
    /// `POST /_pigeon/sign-in/finish`.
    SignInFinish,
    /// `GET /_pigeon/enrol`: the page where a passkey is registered.
    EnrolPage,
    /// `POST /_pigeon/enrol/begin`.
    EnrolBegin,
    /// `POST /_pigeon/enrol/finish`.
    EnrolFinish,
    /// `POST /_pigeon/sign-out`.
    SignOut,
}

/// What the gate decides for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Serve as before.
    Pass,
    /// Answer it here.
    Reserved(Reserved),
    /// Show the sign-in page, with 401.
    SignIn,
    /// Answer 401.
    Unauthorized,
}

/// The config in a host's `instance_subdomains` entry: `None` unless it sets `auth` to
/// "passkey".
pub fn config_of(entry: &Value) -> Option<Config> {
    if entry["auth"].as_str() != Some("passkey") {
        return None;
    }
    let allow = entry["allow"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    Some(Config { allow })
}

/// The key a host's sessions are signed with: derived from the caller key, so no new secret
/// exists and a cookie of one host never verifies on another.
pub fn session_key(caller_key: &[u8], host: &str) -> [u8; 32] {
    let mut message = b"pigeon-site-session\0".to_vec();
    message.extend_from_slice(host.as_bytes());
    hmac_sha256(caller_key, &message)
}

/// The text a session's signature covers. Each field is hex, so no separator can be inside one.
fn payload(user_id: &str, host: &str, expires_ms: i64) -> String {
    format!(
        "{}.{}.{expires_ms}",
        hex(user_id.as_bytes()),
        hex(host.as_bytes())
    )
}

/// A cookie value for `user_id` on `host`, good for [`SESSION_MS`] from `now_ms`: the user, host
/// and expiry, then a hex HMAC over them.
pub fn issue(key: &[u8; 32], host: &str, user_id: &str, now_ms: i64) -> String {
    let text = payload(user_id, host, now_ms.saturating_add(SESSION_MS));
    let signature = hex(&hmac_sha256(key, text.as_bytes()));
    format!("{text}.{signature}")
}

/// The session in cookie `value`, when its signature is good (compared in constant time), it
/// names `host`, and it has not expired.
pub fn verify(key: &[u8; 32], host: &str, value: &str, now_ms: i64) -> Option<Session> {
    let mut parts = value.split('.');
    let (user_hex, host_hex, expires, signature) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let text = format!("{user_hex}.{host_hex}.{expires}");
    let wanted = hmac_sha256(key, text.as_bytes());
    if !equal(&wanted, &unhex(signature)?) {
        return None;
    }
    let named = String::from_utf8(unhex(host_hex)?).ok()?;
    if named != host {
        return None;
    }
    let expires_ms: i64 = expires.parse().ok()?;
    if expires_ms <= now_ms {
        return None;
    }
    let user_id = String::from_utf8(unhex(user_hex)?).ok()?;
    Some(Session {
        user_id,
        expires_ms,
    })
}

/// The `Set-Cookie` header value that holds `value`: host-only, no Domain.
pub fn set_cookie(value: &str) -> String {
    format!(
        "{COOKIE}={value}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={}",
        SESSION_MS / 1000
    )
}

/// The `Set-Cookie` header value that ends the session.
pub fn clear_cookie() -> String {
    format!("{COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// The session cookie's value in a `Cookie` header, if it is there.
pub fn cookie_value(cookie_header: Option<&str>) -> Option<&str> {
    cookie_header?.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == COOKIE).then_some(value)
    })
}

/// Whether a user may sign in to `host`. A listed id is let in when the user is standard or a
/// site user of this very host; "site" lets in every site user of this host. Service users and
/// the site users of other hosts never get in.
pub fn allowed(config: &Config, host: &str, user_id: &str, user_kind: &str) -> bool {
    let here = user_kind == format!("site:{host}");
    let listed = config.allow.iter().any(|a| a == user_id);
    let all_site = config.allow.iter().any(|a| a == "site");
    (listed && (user_kind == "standard" || here)) || (all_site && here)
}

/// What a session holds as its user: the kind and the id, so each request can ask
/// [`allowed`] again.
pub fn subject(user_kind: &str, user_id: &str) -> String {
    format!("{user_kind}\n{user_id}")
}

/// The kind and id in a [`subject`].
pub fn split_subject(subject: &str) -> Option<(&str, &str)> {
    subject.split_once('\n')
}

/// `next` when it is a path on this host, else "/": one leading "/", no "//", no "\", no
/// control characters. A scheme or host cannot follow a single leading "/".
pub fn safe_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(char::is_control);
    if ok {
        next.to_string()
    } else {
        "/".to_string()
    }
}

/// What to do with a request. With no config it is always [`Gate::Pass`], `/_pigeon/` paths
/// included. With one, the reserved paths are answered here, and any other path passes only
/// with a session; without one a GET or HEAD gets the sign-in page, anything else 401.
pub fn gate(config: Option<&Config>, method: &str, path: &str, session_ok: bool) -> Gate {
    if config.is_none() {
        return Gate::Pass;
    }
    let reserved = match (method, path) {
        ("GET", "/_pigeon/passkey.js") => Some(Reserved::Script),
        ("GET", "/_pigeon/sign-in") => Some(Reserved::SignInPage),
        ("POST", "/_pigeon/sign-in/begin") => Some(Reserved::SignInBegin),
        ("POST", "/_pigeon/sign-in/finish") => Some(Reserved::SignInFinish),
        ("GET", "/_pigeon/enrol") => Some(Reserved::EnrolPage),
        ("POST", "/_pigeon/enrol/begin") => Some(Reserved::EnrolBegin),
        ("POST", "/_pigeon/enrol/finish") => Some(Reserved::EnrolFinish),
        ("POST", "/_pigeon/sign-out") => Some(Reserved::SignOut),
        _ => None,
    };
    if let Some(r) = reserved {
        return Gate::Reserved(r);
    }
    if session_ok {
        Gate::Pass
    } else if method == "GET" || method == "HEAD" {
        Gate::SignIn
    } else {
        Gate::Unauthorized
    }
}

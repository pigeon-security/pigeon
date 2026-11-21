//! The pure parts of the console's sign-in session.
//!
//! After one passkey sign-in the console sets a session cookie. This module builds and clears
//! that cookie, reads the token back out of a request, decides whether a stored session is still
//! fresh, and checks the request headers that guard state-changing calls. It performs no I/O and
//! uses no `worker` types, so it is unit-tested on the native target.

/// The cookie's name. The `__Host-` prefix makes the browser refuse it unless it is Secure, has
/// Path=/ and no Domain.
pub const COOKIE: &str = "__Host-pigeon_session";
/// What every session token starts with.
pub const PREFIX: &str = "pgn_ss_";
/// A session ends after this long without a request, in milliseconds (30 minutes).
pub const IDLE_MS: i64 = 30 * 60 * 1000;
/// And this long after sign-in whatever happens, in milliseconds (8 hours).
pub const MAX_MS: i64 = 8 * 60 * 60 * 1000;

/// The longest session token accepted, in bytes.
const MAX_TOKEN_LEN: usize = 100;
/// How far in the future of now a stored time may be, in milliseconds.
const CLOCK_SKEW_MS: i64 = 60 * 1000;
/// The cookie attributes shared by starting and ending a session.
const ATTRIBUTES: &str = "Path=/; Secure; HttpOnly; SameSite=Strict";

/// The Set-Cookie value that starts a session: `__Host-pigeon_session=<token>; Path=/; Secure;
/// HttpOnly; SameSite=Strict; Max-Age=<MAX_MS/1000>`.
pub fn cookie(token: &str) -> String {
    format!("{COOKIE}={token}; {ATTRIBUTES}; Max-Age={}", MAX_MS / 1000)
}

/// The Set-Cookie value that ends it: same attributes, empty value, Max-Age=0.
pub fn clear() -> String {
    format!("{COOKIE}=; {ATTRIBUTES}; Max-Age=0")
}

/// The session token in a Cookie request header, if one is there: the value of
/// `__Host-pigeon_session` among `;`-separated pairs, only when it starts with [`PREFIX`], is at
/// most 100 bytes and holds only base64url characters after the prefix. A cookie named
/// `pigeon_session` without the prefix is not it.
pub fn token_from(header: &str) -> Option<&str> {
    header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, _)| name.trim() == COOKIE)
        .map(|(_, value)| value.trim())
        .find(|value| well_formed(value))
}

/// Whether a token has the session prefix, a bounded length and a base64url body.
fn well_formed(token: &str) -> bool {
    if token.len() > MAX_TOKEN_LEN {
        return false;
    }
    match token.strip_prefix(PREFIX) {
        Some(body) => !body.is_empty() && body.bytes().all(base64url),
        None => false,
    }
}

/// Whether a byte belongs to the base64url alphabet.
fn base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// Whether a session created at `created_at` and last seen at `last_seen_at` is still good at
/// `now` (all ms since the epoch): false once it has been idle for [`IDLE_MS`] or alive for
/// [`MAX_MS`], and false when either time is more than a minute ahead of `now`, since a clock
/// that went backwards is not trusted.
pub fn fresh(created_at: i64, last_seen_at: i64, now: i64) -> bool {
    if from_future(created_at, now) || from_future(last_seen_at, now) {
        return false;
    }
    now.saturating_sub(last_seen_at) < IDLE_MS && now.saturating_sub(created_at) < MAX_MS
}

/// Whether a time lies more than the allowed skew after now.
fn from_future(time: i64, now: i64) -> bool {
    time.saturating_sub(now) > CLOCK_SKEW_MS
}

/// Whether a POST's Origin header names this host: exactly `https://{host}`, no port, no path,
/// no trailing slash, the host in any case. `None`, `null`, `http://`, another host, or a host
/// that merely ends with ours is refused.
pub fn same_origin(origin: Option<&str>, host: &str) -> bool {
    match origin.and_then(|value| value.strip_prefix("https://")) {
        Some(named) => !host.is_empty() && named.eq_ignore_ascii_case(host),
        None => false,
    }
}

/// Whether a request's Content-Type is JSON: `application/json`, optionally followed by `;`
/// parameters, in any case.
pub fn json_content(content_type: Option<&str>) -> bool {
    match content_type {
        Some(value) => {
            let media = value.split(';').next().unwrap_or_default();
            media.trim().eq_ignore_ascii_case("application/json")
        }
        None => false,
    }
}

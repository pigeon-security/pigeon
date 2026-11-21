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

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "platform.pigeon.dev";
    const TOKEN: &str = "pgn_ss_AbC-12_xyz";

    #[test]
    fn the_cookie_holds_every_attribute() {
        let value = cookie(TOKEN);
        assert!(value.starts_with("__Host-pigeon_session=pgn_ss_AbC-12_xyz; "));
        for attribute in [
            "Path=/",
            "Secure",
            "HttpOnly",
            "SameSite=Strict",
            "Max-Age=28800",
        ] {
            assert!(
                value.split("; ").any(|part| part == attribute),
                "{attribute}"
            );
        }
        assert!(!value.contains("Domain"));
    }

    #[test]
    fn clear_has_max_age_zero_and_an_empty_value() {
        let value = clear();
        assert!(value.starts_with("__Host-pigeon_session=; "));
        assert!(value.ends_with("; Max-Age=0"));
        assert!(value.contains("Secure"));
        assert!(value.contains("Path=/"));
    }

    #[test]
    fn a_token_among_other_cookies_is_found() {
        let header = format!("theme=dark;  {COOKIE}={TOKEN};lang=en");
        assert_eq!(token_from(&header), Some(TOKEN));
    }

    #[test]
    fn a_token_alone_is_found() {
        let header = format!("{COOKIE}={TOKEN}");
        assert_eq!(token_from(&header), Some(TOKEN));
    }

    #[test]
    fn a_cookie_without_the_host_prefix_is_ignored() {
        let header = format!("pigeon_session={TOKEN}");
        assert_eq!(token_from(&header), None);
    }

    #[test]
    fn a_token_without_the_prefix_is_ignored() {
        let header = format!("{COOKIE}=pgn_xx_AbC123");
        assert_eq!(token_from(&header), None);
    }

    #[test]
    fn a_token_with_a_bad_character_is_ignored() {
        let header = format!("{COOKIE}=pgn_ss_AbC+123");
        assert_eq!(token_from(&header), None);
    }

    #[test]
    fn a_token_of_only_the_prefix_is_ignored() {
        let header = format!("{COOKIE}={PREFIX}");
        assert_eq!(token_from(&header), None);
    }

    #[test]
    fn a_token_of_exactly_one_hundred_bytes_is_found() {
        let token = format!("{PREFIX}{}", "a".repeat(MAX_TOKEN_LEN - PREFIX.len()));
        let header = format!("{COOKIE}={token}");
        assert_eq!(token_from(&header), Some(token.as_str()));
    }

    #[test]
    fn a_token_that_is_too_long_is_ignored() {
        let token = format!("{PREFIX}{}", "a".repeat(MAX_TOKEN_LEN - PREFIX.len() + 1));
        let header = format!("{COOKIE}={token}");
        assert_eq!(token_from(&header), None);
    }

    #[test]
    fn a_session_expires_when_idle_for_exactly_idle_ms() {
        let now = 10 * MAX_MS;
        assert!(fresh(now - 1000, now - IDLE_MS + 1, now));
        assert!(!fresh(now - IDLE_MS, now - IDLE_MS, now));
    }

    #[test]
    fn an_active_session_expires_at_exactly_max_ms() {
        let now = 10 * MAX_MS;
        assert!(fresh(now - MAX_MS + 1, now, now));
        assert!(!fresh(now - MAX_MS, now, now));
    }

    #[test]
    fn a_future_timestamp_is_refused() {
        let now = 10 * MAX_MS;
        assert!(!fresh(now, now + CLOCK_SKEW_MS + 1, now));
        assert!(!fresh(now + CLOCK_SKEW_MS + 1, now, now));
        assert!(fresh(now + CLOCK_SKEW_MS, now + CLOCK_SKEW_MS, now));
    }

    #[test]
    fn only_this_host_over_https_is_the_same_origin() {
        assert!(same_origin(Some("https://platform.pigeon.dev"), HOST));
        assert!(same_origin(Some("https://PLATFORM.pigeon.dev"), HOST));
        for refused in [
            "null",
            "http://platform.pigeon.dev",
            "https://platform.pigeon.dev:8443",
            "https://platform.pigeon.dev/",
            "https://evilplatform.pigeon.dev",
            "https://platform.pigeon.dev.evil.example",
        ] {
            assert!(!same_origin(Some(refused), HOST), "{refused}");
        }
        assert!(!same_origin(None, HOST));
        assert!(!same_origin(Some("https://"), ""));
    }

    #[test]
    fn only_json_is_json_content() {
        assert!(json_content(Some("application/json")));
        assert!(json_content(Some("Application/JSON; charset=utf-8")));
        assert!(!json_content(Some("text/plain")));
        assert!(!json_content(Some("application/jsonp")));
        assert!(!json_content(None));
    }
}

//! The signed POST the gateway sends to a job's `notify` URL on each state change, and which URLs
//! it may be sent to: `https` to a DNS name, no port, no user, no IP literal, nothing local and
//! never the platform itself. Each job has its own signing key, handed to the caller once.

use pigeon_core::sign::{equal, hex, hmac_sha256};

/// The header carrying the signature.
pub const HEADER: &str = "Pigeon-Signature";
/// The longest notify URL accepted, in bytes.
pub const MAX_URL: usize = 2000;

/// Checks a caller's notify URL; answers it unchanged when it may be used.
pub fn check_url(url: &str, own_host: &str) -> Result<String, String> {
    if url.len() > MAX_URL {
        return Err(format!("the notify URL is longer than {MAX_URL} bytes"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the notify URL holds whitespace or control characters".into());
    }
    let rest = url
        .strip_prefix("https://")
        .ok_or("the notify URL must start with https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err("the notify URL must not hold a user name or password".into());
    }
    if authority.starts_with('[') {
        return Err("the notify URL must not use an IP address".into());
    }
    if authority.contains(':') {
        return Err("the notify URL must not name a port".into());
    }
    let host = authority.to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return Err("the notify URL's host must be 1 to 253 characters".into());
    }
    let labels: Vec<&str> = host.split('.').collect();
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err("each part of the host name must be 1 to 63 characters".into());
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("the host name may use only letters, digits and hyphens".into());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err("a part of the host name must not start or end with a hyphen".into());
        }
    }
    if labels.len() < 2 {
        return Err("the host name must have at least two parts".into());
    }
    let all_digits = |l: &str| l.bytes().all(|b| b.is_ascii_digit());
    if labels.last().is_some_and(|l| all_digits(l)) {
        return Err("the notify URL must not use an IP address".into());
    }
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
    {
        return Err("the notify URL must not point at a local or internal host".into());
    }
    let own = own_host.to_ascii_lowercase();
    if !own.is_empty() && (host == own || host.ends_with(&format!(".{own}"))) {
        return Err("the notify URL must not point at the platform".into());
    }
    Ok(url.to_string())
}

fn expected_hex(key: &[u8], unix_secs: i64, body: &[u8]) -> String {
    let mut message = format!("{unix_secs}.").into_bytes();
    message.extend_from_slice(body);
    hex(&hmac_sha256(key, &message))
}

/// The signature header's value for `body` sent at `unix_secs`:
/// `t=<secs>,v1=<hex HMAC-SHA256(key, "<secs>.<body>")>`.
pub fn signature(key: &[u8], unix_secs: i64, body: &[u8]) -> String {
    format!("t={unix_secs},v1={}", expected_hex(key, unix_secs, body))
}

/// Whether `header` signs `body` with `key` and is within `tolerance_secs` of `now_secs`: what a
/// receiver checks.
pub fn verify(key: &[u8], header: &str, body: &[u8], now_secs: i64, tolerance_secs: i64) -> bool {
    let mut t: Option<&str> = None;
    let mut v1: Option<&str> = None;
    for part in header.split(',') {
        if let Some(value) = part.strip_prefix("t=") {
            if t.replace(value).is_some() {
                return false;
            }
        } else if let Some(value) = part.strip_prefix("v1=") {
            if v1.replace(value).is_some() {
                return false;
            }
        }
    }
    let (Some(t), Some(v1)) = (t, v1) else {
        return false;
    };
    let Ok(secs) = t.parse::<i64>() else {
        return false;
    };
    if (i128::from(now_secs) - i128::from(secs)).abs() > i128::from(tolerance_secs) {
        return false;
    }
    equal(expected_hex(key, secs, body).as_bytes(), v1.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: &str = "platform.pigeon.dev";

    fn refused(url: &str) -> bool {
        check_url(url, OWN).is_err()
    }

    #[test]
    fn a_plain_https_url_is_taken_unchanged() {
        let url = "https://hooks.example.com/ci?x=1";
        assert_eq!(check_url(url, OWN), Ok(url.to_string()));
        assert!(!refused("https://hooks.example.com"));
    }

    #[test]
    fn other_schemes_and_an_empty_host_are_refused() {
        assert!(refused("http://hooks.example.com/"));
        assert!(refused("HTTPS://hooks.example.com/"));
        assert!(refused("https://"));
        assert!(refused("https:///x"));
    }

    #[test]
    fn users_ports_and_ip_literals_are_refused() {
        assert!(refused("https://a@b.com"));
        assert!(refused("https://b.com:8443/"));
        assert!(refused("https://[::1]/"));
        assert!(refused("https://127.0.0.1/"));
        assert!(refused("https://10.0.0.1/x"));
        assert!(refused("https://example.com.1/"));
    }

    #[test]
    fn local_and_internal_hosts_are_refused() {
        assert!(refused("https://localhost/"));
        assert!(refused("https://x.localhost/"));
        assert!(refused("https://foo.internal/"));
        assert!(refused("https://printer.local/"));
    }

    #[test]
    fn the_platform_and_its_subdomains_are_refused() {
        assert!(refused("https://platform.pigeon.dev/"));
        assert!(refused("https://PLATFORM.pigeon.dev/hook"));
        assert!(refused("https://a.platform.pigeon.dev/"));
        assert!(!refused("https://pigeon.dev.example.com/"));
    }

    #[test]
    fn malformed_hosts_and_long_urls_are_refused() {
        assert!(refused("https://intranet/"));
        assert!(refused("https://a_b.example.com/"));
        assert!(refused("https://exa mple.com/"));
        assert!(refused("https://-a.example.com/"));
        assert!(refused(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_URL)
        )));
    }

    #[test]
    fn the_signature_has_its_format_and_is_deterministic() {
        let sig = signature(b"key", 1_700_000_000, b"{\"state\":\"ok\"}");
        let hex_part = sig.strip_prefix("t=1700000000,v1=").unwrap();
        assert_eq!(hex_part.len(), 64);
        assert!(hex_part.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(sig, signature(b"key", 1_700_000_000, b"{\"state\":\"ok\"}"));
    }

    #[test]
    fn verify_takes_a_good_signature_and_refuses_tampering() {
        let body = b"{\"state\":\"ok\"}";
        let sig = signature(b"key", 1_700_000_000, body);
        assert!(verify(b"key", &sig, body, 1_700_000_010, 300));
        assert!(!verify(
            b"key",
            &sig,
            b"{\"state\":\"bad\"}",
            1_700_000_010,
            300
        ));
        assert!(!verify(b"other", &sig, body, 1_700_000_010, 300));
        assert!(!verify(b"key", &sig, body, 1_700_001_000, 300));
        assert!(!verify(b"key", "t=1700000000", body, 1_700_000_010, 300));
        assert!(!verify(b"key", "garbage", body, 1_700_000_010, 300));
        assert!(!verify(
            b"key",
            &format!("{sig},t=1"),
            body,
            1_700_000_010,
            300
        ));
    }
}

//! Buildkite pipeline webhooks: a delivery's `X-Buildkite-Signature` checked before anything in
//! it is read (HMAC-SHA256 with the webhook token over `"<timestamp>.<body>"`, within a replay
//! window), then what it says about one build. No error repeats the signature, the token or
//! the body.

use serde_json::Value;

use crate::sign::verify_hex;

/// Checks a delivery's signature header against `body` with the webhook token `key`, within
/// `window_secs` of `now_secs` either way.
pub fn verify(
    signature_header: Option<&str>,
    body: &[u8],
    key: &[u8],
    now_secs: i64,
    window_secs: i64,
) -> Result<(), String> {
    if key.is_empty() {
        return Err("no webhook token is set".into());
    }
    let header = signature_header.ok_or_else(|| "no signature".to_string())?;
    let mut ts_raw: Option<&str> = None;
    let mut sig: Option<&str> = None;
    for part in header.split(',') {
        let (k, v) = part
            .trim()
            .split_once('=')
            .ok_or_else(|| "the signature header is malformed".to_string())?;
        let slot = match k.trim() {
            "timestamp" => &mut ts_raw,
            "signature" => &mut sig,
            _ => continue,
        };
        if slot.is_some() {
            return Err("the signature header has a repeated part".into());
        }
        *slot = Some(v.trim());
    }
    let ts_raw = ts_raw.ok_or_else(|| "the signature header has no timestamp".to_string())?;
    let sig = sig.ok_or_else(|| "the signature header has no signature".to_string())?;
    if ts_raw.is_empty() || ts_raw.len() > 12 || !ts_raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err("the signature timestamp is not valid".into());
    }
    let ts: i64 = ts_raw
        .parse()
        .map_err(|_| "the signature timestamp is not valid".to_string())?;
    if (i128::from(now_secs) - i128::from(ts)).abs() > i128::from(window_secs) {
        return Err("the signature is too old or from the future".into());
    }
    if sig.len() != 64 || !sig.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("the signature is not 64 hex characters".into());
    }
    let mut msg = format!("{ts}.").into_bytes();
    msg.extend_from_slice(body);
    if verify_hex(key, &msg, sig) {
        Ok(())
    } else {
        Err("the signature does not match".into())
    }
}

/// What a verified delivery says about one build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// The build's UUID.
    pub id: String,
    /// Its number in the pipeline.
    pub number: u64,
    /// Such as `running` or `passed`.
    pub state: String,
    pub web_url: Option<String>,
    /// The `pigeon_job` meta-data the gateway set, when it started the build.
    pub job: Option<String>,
}

/// What a verified delivery is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Ping,
    Running(Build),
    Finished(Build),
    /// Any other event, by name.
    Other(String),
}

/// Reads a delivery that has been verified; `event_header` is `X-Buildkite-Event`.
pub fn read(event_header: Option<&str>, body: &[u8]) -> Result<Event, String> {
    let name = event_header.map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err("no event".into());
    }
    match name {
        "ping" => Ok(Event::Ping),
        "build.running" => parse_build(body).map(Event::Running),
        "build.finished" => parse_build(body).map(Event::Finished),
        other => Ok(Event::Other(other.to_string())),
    }
}

fn opt_str(v: Option<&Value>) -> Option<Option<String>> {
    match v {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => None,
    }
}

fn parse_build(body: &[u8]) -> Result<Build, String> {
    let bad = || "not a build event body".to_string();
    let v: Value = serde_json::from_slice(body).map_err(|_| bad())?;
    let b = v.get("build").and_then(Value::as_object).ok_or_else(bad)?;
    let id = b
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(bad)?;
    let number = b.get("number").and_then(Value::as_u64).ok_or_else(bad)?;
    let state = b.get("state").and_then(Value::as_str).ok_or_else(bad)?;
    let web_url = opt_str(b.get("web_url")).ok_or_else(bad)?;
    let job = match b.get("meta_data") {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => opt_str(m.get("pigeon_job")).ok_or_else(bad)?,
        Some(_) => return Err(bad()),
    };
    Ok(Build {
        id: id.to_string(),
        number,
        state: state.to_string(),
        web_url,
        job,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::{hex, hmac_sha256};

    const KEY: &[u8] = b"token";
    const BODY: &[u8] = b"{\"event\":\"ping\"}";
    const NOW: i64 = 1_700_000_000;

    fn sig(key: &[u8], ts: i64, body: &[u8]) -> String {
        let mut m = format!("{ts}.").into_bytes();
        m.extend_from_slice(body);
        hex(&hmac_sha256(key, &m))
    }

    fn header(ts: i64, body: &[u8]) -> String {
        format!("timestamp={ts}, signature={}", sig(KEY, ts, body))
    }

    fn check(h: Option<&str>, body: &[u8], key: &[u8]) -> Result<(), String> {
        verify(h, body, key, NOW, 300)
    }

    #[test]
    fn only_a_fresh_signature_over_this_body_with_this_token_verifies() {
        let h = header(NOW, BODY);
        assert!(check(Some(&h), BODY, KEY).is_ok());
        assert!(check(Some(&h), b"{}", KEY).is_err());
        assert!(check(Some(&h), BODY, b"other").is_err());
        assert!(check(Some(&header(NOW - 300, BODY)), BODY, KEY).is_ok());
        assert!(check(Some(&header(NOW - 301, BODY)), BODY, KEY).is_err());
        assert!(check(Some(&header(NOW + 301, BODY)), BODY, KEY).is_err());
        assert_eq!(check(None, BODY, KEY), Err("no signature".into()));
        assert!(check(Some(&format!("timestamp={NOW}")), BODY, KEY).is_err());
        assert!(check(Some(&format!("timestamp={NOW},{h}")), BODY, KEY).is_err());
        let nonhex = format!("timestamp={NOW},signature={}", "z".repeat(64));
        assert!(check(Some(&nonhex), BODY, KEY).is_err());
        let upper = format!(
            "timestamp={NOW},signature={}",
            sig(KEY, NOW, BODY).to_uppercase()
        );
        assert!(check(Some(&upper), BODY, KEY).is_ok());
        assert_eq!(
            check(Some(&h), BODY, b""),
            Err("no webhook token is set".into())
        );
    }

    fn build_body(meta: &str, id: &str) -> Vec<u8> {
        format!(
            "{{\"build\":{{{id}\"number\":7,\"state\":\"passed\",\"web_url\":\"https://x\"{meta}}}}}"
        )
        .into_bytes()
    }

    #[test]
    fn a_delivery_reads_as_its_event_and_build() {
        assert_eq!(read(Some("ping"), b""), Ok(Event::Ping));
        let b = build_body(",\"meta_data\":{\"pigeon_job\":\"j1\"}", "\"id\":\"b1\",");
        match read(Some("build.finished"), &b) {
            Ok(Event::Finished(x)) => {
                assert_eq!(x.job.as_deref(), Some("j1"));
                assert_eq!((x.number, x.state.as_str()), (7, "passed"));
                assert_eq!(x.web_url.as_deref(), Some("https://x"));
            }
            other => panic!("{other:?}"),
        }
        match read(Some("build.running"), &build_body("", "\"id\":\"b1\",")) {
            Ok(Event::Running(x)) => assert_eq!(x.job, None),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            read(Some("job.started"), b""),
            Ok(Event::Other("job.started".into()))
        );
        assert!(read(Some("build.finished"), &build_body("", "")).is_err());
        assert!(read(Some("build.finished"), b"not json").is_err());
        assert!(read(None, b"").is_err());
    }
}

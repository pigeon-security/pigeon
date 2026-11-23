//! APNs, the pure parts: provider token input, hosts, headers, payloads, the error table and
//! the send rate limit. Nothing here calls the network or holds a key (docs/plans/ios.md, §3).

use serde_json::{Map, Value, json};

use crate::webauthn::b64url;

/// APNs refuses a payload over this many bytes.
pub const MAX_PAYLOAD: usize = 4096;
/// A cached provider token is reused while it is younger than this many seconds (50 minutes).
pub const TOKEN_REUSE_SECS: u64 = 50 * 60;
/// A provider token is never refreshed more often than this many seconds (20 minutes).
pub const TOKEN_MIN_REFRESH_SECS: u64 = 20 * 60;
/// Sends allowed per person and app in one minute.
pub const PER_MINUTE: usize = 30;
/// Sends allowed per person and app in one day.
pub const PER_DAY: usize = 500;

/// The text a provider token signs: base64url header `{alg, kid}`, a dot, base64url claims
/// `{iss, iat}`.
pub fn signing_input(key_id: &str, team_id: &str, now_secs: u64) -> String {
    let header = json!({ "alg": "ES256", "kid": key_id });
    let claims = json!({ "iss": team_id, "iat": now_secs });
    format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(claims.to_string().as_bytes())
    )
}

/// What to do with a cached token issued at `issued_at`: true when it may be reused.
/// It is reused under 50 minutes. A token younger than 20 minutes is never replaced, so a
/// forced refresh (after a 403) asks `may_refresh` too.
pub fn token_fresh(issued_at: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(issued_at) < TOKEN_REUSE_SECS
}

/// True when a new token may be signed: the cached one is at least 20 minutes old.
pub fn may_refresh(issued_at: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(issued_at) >= TOKEN_MIN_REFRESH_SECS
}

/// True for the two environments a device token can belong to.
pub fn is_environment(env: &str) -> bool {
    env == "development" || env == "production"
}

/// The APNs host for an environment, or None for an unknown one.
pub fn host(environment: &str) -> Option<&'static str> {
    match environment {
        "development" => Some("api.sandbox.push.apple.com"),
        "production" => Some("api.push.apple.com"),
        _ => None,
    }
}

/// True when `token` has the form of a device token: hex, even length, 64 to 200 characters.
pub fn is_device_token(token: &str) -> bool {
    let n = token.len();
    (64..=200).contains(&n) && n.is_multiple_of(2) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The bundle id (topic) of a declared app, or None. The caller names the app, never the topic.
pub fn bundle_id(app: &str) -> Option<&'static str> {
    match app {
        "pigeon" => Some("dev.pigeon.app"),
        _ => None,
    }
}

/// The kind of push: `alert` (priority 10) or `background` (priority 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushType {
    /// A visible notification.
    Alert,
    /// A silent push that wakes the app.
    Background,
}

impl PushType {
    /// Parses `alert` or `background`; None for anything else.
    pub fn parse(s: &str) -> Option<PushType> {
        match s {
            "alert" => Some(PushType::Alert),
            "background" => Some(PushType::Background),
            _ => None,
        }
    }

    /// The `apns-push-type` header value.
    pub fn as_str(self) -> &'static str {
        match self {
            PushType::Alert => "alert",
            PushType::Background => "background",
        }
    }

    /// The `apns-priority` header value.
    pub fn priority(self) -> &'static str {
        match self {
            PushType::Alert => "10",
            PushType::Background => "5",
        }
    }
}

/// Builds the JSON payload. `data` must be a flat object of strings. Errors name the problem
/// and never carry the data. The result is at most [`MAX_PAYLOAD`] bytes.
pub fn payload(
    push_type: PushType,
    alert: Option<(&str, &str)>,
    data: Option<&Value>,
) -> Result<String, String> {
    let mut root = Map::new();
    let aps = match push_type {
        PushType::Alert => {
            let (title, body) = alert.ok_or("an alert push needs a title and a body")?;
            json!({ "alert": { "title": title, "body": body } })
        }
        PushType::Background => json!({ "content-available": 1 }),
    };
    root.insert("aps".into(), aps);
    if let Some(data) = data {
        let obj = data.as_object().ok_or("data must be an object")?;
        for (k, v) in obj {
            if k == "aps" {
                return Err("data may not use the key `aps`".into());
            }
            if !v.is_string() {
                return Err("data values must be strings".into());
            }
            root.insert(k.clone(), v.clone());
        }
    }
    let text = Value::Object(root).to_string();
    if text.len() > MAX_PAYLOAD {
        return Err(format!(
            "the payload is {} bytes; APNs allows {MAX_PAYLOAD}",
            text.len()
        ));
    }
    Ok(text)
}

/// The request headers for one send, as (name, value) pairs. The topic is always the app's
/// bundle id. Errors for an unknown app.
pub fn headers(
    app: &str,
    push_type: PushType,
    collapse_id: Option<&str>,
) -> Result<Vec<(&'static str, String)>, String> {
    let topic = bundle_id(app).ok_or("that app is not declared")?;
    let mut h = vec![
        ("apns-topic", topic.to_string()),
        ("apns-push-type", push_type.as_str().to_string()),
        ("apns-priority", push_type.priority().to_string()),
    ];
    if let Some(c) = collapse_id {
        if c.is_empty() || c.len() > 64 {
            return Err("collapse_id is 1 to 64 bytes".into());
        }
        h.push(("apns-collapse-id", c.to_string()));
    }
    Ok(h)
}

/// What pigeon does with an APNs answer (the table in docs/plans/ios.md, §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Sent: record `last_ok_at`, answer the `apns-id`.
    Ok,
    /// Mark the row; delete it if its environment was already corrected once.
    MarkDevice,
    /// Stop and report: the key or bundle id is wrong.
    StopReport,
    /// Refuse to the caller.
    RefuseCaller,
    /// Refresh the provider token once, then report.
    RefreshToken,
    /// Delete the row if APNs's timestamp is later than its `created_at`.
    DeleteIfNewer,
    /// Keep the cached token and report.
    KeepTokenReport,
    /// Back that device off for 10 minutes.
    BackOff,
    /// Retry twice with backoff, then report.
    Retry,
    /// Not in the table: report as is.
    Report,
}

/// Maps an APNs (status, reason) to what to do.
pub fn action(status: u16, reason: &str) -> Action {
    match (status, reason) {
        (200, _) => Action::Ok,
        (400, "BadDeviceToken") => Action::MarkDevice,
        (400, "DeviceTokenNotForTopic" | "BadTopic" | "TopicDisallowed") => Action::StopReport,
        (400, "PayloadEmpty") | (413, _) => Action::RefuseCaller,
        (403, "ExpiredProviderToken" | "InvalidProviderToken") => Action::RefreshToken,
        (410, _) => Action::DeleteIfNewer,
        (429, "TooManyProviderTokenUpdates") => Action::KeepTokenReport,
        (429, _) => Action::BackOff,
        (500, _) | (503, _) => Action::Retry,
        _ => Action::Report,
    }
}

/// Why a send is refused by the rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limited {
    /// More than 30 in the last minute.
    Minute,
    /// More than 500 in the last day.
    Day,
}

/// Decides a send from the timestamps (seconds) of earlier sends for one person and app.
/// Ok means it may go.
pub fn rate_limit(past_sends: &[u64], now_secs: u64) -> Result<(), Limited> {
    let in_last = |secs: u64| {
        past_sends
            .iter()
            .filter(|t| now_secs.saturating_sub(**t) < secs)
            .count()
    };
    if in_last(86_400) >= PER_DAY {
        return Err(Limited::Day);
    }
    if in_last(60) >= PER_MINUTE {
        return Err(Limited::Minute);
    }
    Ok(())
}

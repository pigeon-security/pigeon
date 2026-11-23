//! A section's last answer, kept in the Directory so the console shows it at once. A section
//! whose data comes from Cloudflare's APIs (credentials, cost) can take seconds to answer; its
//! snapshot answers in one Directory read, says when it was made, and says when it is older
//! than its freshness, so the page shows it and asks for a fresh one behind it. Pure: names,
//! times and freshness. The reads and writes are in `console.rs`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long a section's snapshot keeps it in the Directory at all, so an unused one goes.
pub const KEPT_MS: i64 = 7 * 86_400_000;

/// A section's answer and when it was made, in ms since the epoch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub at_ms: i64,
    pub data: Value,
}

/// How long section `id`'s snapshot is fresh, for the sections that keep one: those whose data
/// comes from Cloudflare's APIs. Directory-only sections answer fast enough without one.
pub fn fresh_ms(id: &str) -> Option<i64> {
    match id {
        "credentials" => Some(5 * 60_000),
        "cost" => Some(10 * 60_000),
        "platform" => Some(10 * 60_000),
        _ => None,
    }
}

/// The Directory setting that holds section `id`'s snapshot over `days` days.
pub fn key(id: &str, days: i64) -> String {
    format!("console:snapshot:{id}:{days}")
}

/// Whether a snapshot made at `at_ms` is past its freshness `fresh_ms` at `now_ms`.
pub fn stale(at_ms: i64, fresh_ms: i64, now_ms: i64) -> bool {
    now_ms - at_ms > fresh_ms
}

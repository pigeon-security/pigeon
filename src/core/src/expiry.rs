//! When a secret stops working, as its issuer reports it: the type and the parsing of an
//! issuer's expiry field, shared by core and the capability Workers.

use serde::{Deserialize, Serialize};

/// When a secret stops working, as its issuer reports it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Expiry {
    /// It stops working at `at_ms`, milliseconds since the epoch.
    At { at_ms: i64 },
    /// It does not expire.
    Never,
    /// The issuer refuses it now: expired, revoked or disabled.
    Refused { why: String },
    /// No one can say: there is no way to ask, or asking failed.
    Unknown { why: String },
}

/// The seconds east of UTC that a time's zone says: empty, `Z` and `UTC` are 0; else `+HH:MM`,
/// `+HHMM` or `-` either. Anything else is refused rather than misread.
fn offset_secs(zone: &str) -> Option<i64> {
    if matches!(zone, "" | "Z" | "UTC") {
        return Some(0);
    }
    let sign = match zone.get(0..1)? {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
    let shaped = digits.len() == 4
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (zone.len() == 5 || (zone.len() == 6 && zone.get(3..4) == Some(":")));
    if !shaped {
        return None;
    }
    let (h, m): (i64, i64) = (digits[..2].parse().ok()?, digits[2..].parse().ok()?);
    (h <= 14 && m <= 59).then_some(sign * (h * 3600 + m * 60))
}

/// A time as the issuers write it, in ms since the epoch: `2026-12-21T10:00:00Z`,
/// `2026-12-21T10:00:00.000Z`, GitHub's `2026-12-21 10:00:00 UTC` or `… -0700`, an offset such
/// as `+02:00`, or a bare date (midnight UTC). Anything else is refused rather than misread.
pub fn parse_time(text: &str) -> Option<i64> {
    let t = text.trim();
    let num = |s: Option<&str>| -> Option<i64> {
        let s = s?;
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (y, m, d) = (num(t.get(0..4))?, num(t.get(5..7))?, num(t.get(8..10))?);
    if t.get(4..5)? != "-"
        || t.get(7..8)? != "-"
        || !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
    {
        return None;
    }
    let rest = &t[10..];
    let secs = if rest.is_empty() {
        0
    } else {
        let sep = rest.get(0..1)?;
        if sep != "T" && sep != " " {
            return None;
        }
        let (hh, mm, ss) = (
            num(rest.get(1..3))?,
            num(rest.get(4..6))?,
            num(rest.get(7..9))?,
        );
        if rest.get(3..4)? != ":" || rest.get(6..7)? != ":" || hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        // What follows the seconds: fractions, then a UTC marker.
        let mut tail = &rest[9..];
        if let Some(f) = tail.strip_prefix('.') {
            tail = f.trim_start_matches(|c: char| c.is_ascii_digit());
        }
        hh * 3600 + mm * 60 + ss - offset_secs(tail.trim())?
    };
    Some((days_from_civil(y, m, d) * 86_400 + secs) * 1000)
}

/// The expiry an issuer's time field gives: `At` when it parses, `Never` when there is none,
/// and `Unknown` when it is there but unreadable.
pub fn from_field(field: Option<&str>) -> Expiry {
    match field.map(str::trim).filter(|s| !s.is_empty()) {
        None => Expiry::Never,
        Some(s) => parse_time(s).map_or_else(
            || Expiry::Unknown {
                why: format!("the issuer's expiry `{s}` is not a time pigeon reads"),
            },
            |at_ms| Expiry::At { at_ms },
        ),
    }
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

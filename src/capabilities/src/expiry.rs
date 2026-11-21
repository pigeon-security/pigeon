//! Credential expiry, seen before it bites. Each capability can say when its secrets stop
//! working ([`crate::Capability::secret_expiry`]), as the issuer reports it; the answer is
//! cached for a day in the Directory, so the console and `credentials_expiry` ask an issuer at
//! most daily. Proxy credentials carry their own `expires_at`, so they need no check. A row
//! warns [`WARN_DAYS`] ahead and flags a credential that has expired or that its issuer
//! refuses. Nothing here ever holds or shows a value.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Result};

use crate::users::directory;
use crate::{Capability, Context};

/// How many days ahead a credential's expiry is warned of.
pub const WARN_DAYS: i64 = 7;
/// How long an issuer's answer is kept before it is asked again.
pub const CACHE_MS: i64 = 86_400_000;
/// How long a failed check is kept, so a flaky issuer is asked again sooner.
pub const RETRY_MS: i64 = 3_600_000;
/// How long an expired proxy credential stays listed, flagged.
pub const EXPIRED_SHOWN_MS: i64 = 14 * 86_400_000;
const DAY_MS: i64 = 86_400_000;

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

/// A credential's state against now, for the page.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// More than [`WARN_DAYS`] left, or no expiry.
    Ok,
    /// [`WARN_DAYS`] or fewer left.
    Soon,
    /// Past its expiry, or refused.
    Expired,
    /// Not known.
    Unknown,
    /// Declared but not set, so nothing to check.
    Missing,
}

/// The status of `expiry` at `now_ms`.
pub fn status(expiry: &Expiry, now_ms: i64) -> Status {
    match expiry {
        Expiry::At { at_ms } if *at_ms <= now_ms => Status::Expired,
        Expiry::At { at_ms } if *at_ms - now_ms <= WARN_DAYS * DAY_MS => Status::Soon,
        Expiry::At { .. } | Expiry::Never => Status::Ok,
        Expiry::Refused { .. } => Status::Expired,
        Expiry::Unknown { .. } => Status::Unknown,
    }
}

/// Whole days from `now_ms` to `at_ms`, rounded down; negative once past.
pub fn days_left(at_ms: i64, now_ms: i64) -> i64 {
    (at_ms - now_ms).div_euclid(DAY_MS)
}

/// One credential and when it expires: never its value.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Row {
    /// A secret's name, or a proxy credential's label.
    pub name: String,
    /// `secret` or `proxy`.
    pub kind: &'static str,
    /// The capability that needs it, or for a proxy credential who issued it.
    pub owner: String,
    pub expiry: Option<Expiry>,
    /// `YYYY-MM-DD` of the expiry, when there is one.
    pub expires: Option<String>,
    pub days_left: Option<i64>,
    pub status: Status,
    /// When the issuer was last asked, ms since the epoch; none for a proxy credential.
    pub checked_ms: Option<i64>,
}

impl Row {
    /// `expiry` for credential `name` of `kind`, at `now_ms`.
    pub fn new(
        name: &str,
        kind: &'static str,
        owner: &str,
        expiry: Option<Expiry>,
        checked_ms: Option<i64>,
        now_ms: i64,
    ) -> Self {
        let at = match &expiry {
            Some(Expiry::At { at_ms }) => Some(*at_ms),
            _ => None,
        };
        Row {
            name: name.into(),
            kind,
            owner: owner.into(),
            status: expiry
                .as_ref()
                .map_or(Status::Missing, |e| status(e, now_ms)),
            expires: at.map(date),
            days_left: at.map(|a| days_left(a, now_ms)),
            expiry,
            checked_ms,
        }
    }

    /// Whether it needs a person: expiring soon or expired.
    pub fn needs_you(&self) -> bool {
        matches!(self.status, Status::Soon | Status::Expired)
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

/// The civil date of a count of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `ms` since the epoch as its UTC `YYYY-MM-DD`.
pub fn date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(DAY_MS));
    format!("{y:04}-{m:02}-{d:02}")
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

/// What the Directory keeps for one secret.
#[derive(Serialize, Deserialize)]
struct Cached {
    expiry: Expiry,
    checked_ms: i64,
}

fn cache_key(name: &str) -> String {
    format!("expiry:{name}")
}

fn now_ms() -> i64 {
    worker::js_sys::Date::now() as i64
}

/// Forgets what is known of secret `name`'s expiry, so the next look asks its issuer: for
/// when a new value has just been written.
pub async fn forget(env: &Env, name: &str) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": cache_key(name), "value": "", "ttl_ms": 1 }),
    )
    .await
    .map(|_| ())
}

/// Secret `name`'s expiry, from the Directory when it was asked within a day, else from `owner`.
async fn secret_expiry(cx: &Context<'_>, owner: &dyn Capability, name: &str) -> (Expiry, i64) {
    let key = cache_key(name);
    if let Ok(Value::String(kept)) = directory(cx.env, "setting.get", json!({ "key": key })).await
        && let Ok(c) = serde_json::from_str::<Cached>(&kept)
    {
        return (c.expiry, c.checked_ms);
    }
    let expiry = owner
        .secret_expiry(cx, name)
        .await
        .unwrap_or_else(|e| Expiry::Unknown {
            why: format!("the check failed: {e}"),
        });
    let checked_ms = now_ms();
    let ttl_ms = match expiry {
        Expiry::Unknown { .. } => RETRY_MS,
        _ => CACHE_MS,
    };
    let kept = Cached {
        expiry: expiry.clone(),
        checked_ms,
    };
    if let Ok(value) = serde_json::to_string(&kept) {
        // A cache that cannot be written only means asking again next time.
        let _ = directory(
            cx.env,
            "setting.put",
            json!({ "key": key, "value": value, "ttl_ms": ttl_ms }),
        )
        .await;
    }
    (expiry, checked_ms)
}

/// Every declared secret's expiry, by owner; `set` names the secrets the store holds, and a
/// declared secret it lacks is `missing`, not checked.
pub async fn secrets(cx: &Context<'_>, set: &[String]) -> Vec<Row> {
    let now = now_ms();
    let mut rows = Vec::new();
    for c in cx.hosted.capabilities {
        for slot in c.secrets() {
            if rows.iter().any(|r: &Row| r.name == slot.name) {
                continue;
            }
            let row = if set.contains(&slot.name) {
                let (expiry, checked) = secret_expiry(cx, c.as_ref(), &slot.name).await;
                Row::new(
                    &slot.name,
                    "secret",
                    c.name(),
                    Some(expiry),
                    Some(checked),
                    now,
                )
            } else {
                Row::new(&slot.name, "secret", c.name(), None, None, now)
            };
            rows.push(row);
        }
    }
    rows
}

/// A proxy credential as the Directory lists it.
#[derive(Deserialize)]
struct Listed {
    id: String,
    label: String,
    #[serde(default)]
    person: Option<String>,
    expires_at: i64,
}

/// The proxy credentials' expiry: the live ones and those expired within
/// [`EXPIRED_SHOWN_MS`], from the Directory, which needs no issuer.
pub async fn proxy(env: &Env) -> Result<Vec<Row>> {
    let listed = directory(
        env,
        "proxy.tokens",
        json!({ "expired_within_ms": EXPIRED_SHOWN_MS }),
    )
    .await?;
    let listed: Vec<Listed> = serde_json::from_value(listed)?;
    Ok(proxy_rows(&listed, now_ms()))
}

fn proxy_rows(listed: &[Listed], now: i64) -> Vec<Row> {
    listed
        .iter()
        .map(|t| {
            let owner = format!(
                "{} · issued by {}",
                t.id,
                t.person.as_deref().unwrap_or("someone removed")
            );
            let expiry = Expiry::At {
                at_ms: t.expires_at,
            };
            Row::new(&t.label, "proxy", &owner, Some(expiry), None, now)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEC_21: i64 = 1_797_811_200_000; // 2026-12-21T00:00:00Z

    #[test]
    fn times_parse_in_the_issuers_shapes() {
        assert_eq!(parse_time("2026-12-21T00:00:00Z"), Some(DEC_21));
        assert_eq!(parse_time("2026-12-21"), Some(DEC_21));
        assert_eq!(parse_time("2026-12-21T00:00:00.123Z"), Some(DEC_21));
        assert_eq!(parse_time("2026-12-21 00:00:00 UTC"), Some(DEC_21));
        assert_eq!(
            parse_time("2026-12-21T01:02:03+00:00"),
            Some(DEC_21 + 3_723_000)
        );
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-12-21T02:00:00+02:00"), Some(DEC_21));
        assert_eq!(parse_time("2026-12-20 17:00:00 -0700"), Some(DEC_21));
    }

    #[test]
    fn zones_are_read_or_refused() {
        assert_eq!(offset_secs(""), Some(0));
        assert_eq!(offset_secs("UTC"), Some(0));
        assert_eq!(offset_secs("+0530"), Some(19_800));
        assert_eq!(offset_secs("-07:00"), Some(-25_200));
        for z in ["PST", "+7", "+07:0", "+0760", "+1500", "+07:00:00", "0700"] {
            assert_eq!(offset_secs(z), None, "{z}");
        }
    }

    #[test]
    fn times_with_offsets_or_junk_are_refused() {
        for t in [
            "",
            "2026-12-21T00:00:00 PST",
            "2026-13-01",
            "2026-12-32",
            "2026/12/21",
            "2026-12-21X00:00:00Z",
            "2026-12-21T24:00:00Z",
            "not a time",
            "2026-1-1",
        ] {
            assert_eq!(parse_time(t), None, "{t}");
        }
    }

    #[test]
    fn a_missing_field_means_never_and_an_unreadable_one_unknown() {
        assert_eq!(from_field(None), Expiry::Never);
        assert_eq!(from_field(Some("  ")), Expiry::Never);
        assert_eq!(
            from_field(Some("2026-12-21T00:00:00Z")),
            Expiry::At { at_ms: DEC_21 }
        );
        assert!(matches!(from_field(Some("soon")), Expiry::Unknown { .. }));
    }

    #[test]
    fn the_status_warns_a_week_ahead_and_flags_the_expired() {
        let at = Expiry::At { at_ms: DEC_21 };
        assert_eq!(status(&at, DEC_21 - 8 * DAY_MS), Status::Ok);
        assert_eq!(status(&at, DEC_21 - 7 * DAY_MS), Status::Soon);
        assert_eq!(status(&at, DEC_21 - 1), Status::Soon);
        assert_eq!(status(&at, DEC_21), Status::Expired);
        assert_eq!(status(&Expiry::Never, DEC_21), Status::Ok);
        let refused = Expiry::Refused { why: "401".into() };
        assert_eq!(status(&refused, 0), Status::Expired);
        let unknown = Expiry::Unknown {
            why: "no API".into(),
        };
        assert_eq!(status(&unknown, 0), Status::Unknown);
    }

    #[test]
    fn rows_carry_the_date_and_days_left_and_say_when_they_need_you() {
        let now = DEC_21 - 30 * DAY_MS + 5;
        let row = Row::new(
            "CF_PARENT_TOKEN",
            "secret",
            "secrets",
            Some(Expiry::At { at_ms: DEC_21 }),
            Some(now),
            now,
        );
        assert_eq!(row.expires.as_deref(), Some("2026-12-21"));
        assert_eq!(row.days_left, Some(29));
        assert!(!row.needs_you());
        let missing = Row::new("X", "secret", "ci", None, None, now);
        assert_eq!(missing.status, Status::Missing);
        assert!(!missing.needs_you());
        let past = Row::new(
            "Y",
            "secret",
            "ci",
            Some(Expiry::At { at_ms: 0 }),
            None,
            now,
        );
        assert!(past.needs_you());
        assert!(past.days_left.unwrap() < 0);
    }

    #[test]
    fn proxy_rows_name_the_issuer_and_flag_the_expired() {
        let listed = vec![
            Listed {
                id: "abcdefghijkl".into(),
                label: "pigeon env".into(),
                person: Some("Ana".into()),
                expires_at: DEC_21,
            },
            Listed {
                id: "bcdefghijklm".into(),
                label: "old env".into(),
                person: None,
                expires_at: DEC_21 - 40 * DAY_MS,
            },
        ];
        let rows = proxy_rows(&listed, DEC_21 - 30 * DAY_MS);
        assert_eq!(rows[0].status, Status::Ok);
        assert_eq!(rows[0].owner, "abcdefghijkl · issued by Ana");
        assert_eq!(rows[0].kind, "proxy");
        assert_eq!(rows[1].status, Status::Expired);
        assert_eq!(rows[1].owner, "bcdefghijklm · issued by someone removed");
    }

    #[test]
    fn rows_never_carry_a_value_field() {
        let row = Row::new("X", "secret", "ci", Some(Expiry::Never), Some(1), 1);
        let v = serde_json::to_value(&row).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        for k in keys {
            assert!(!["value", "token", "secret"].contains(&k.as_str()), "{k}");
        }
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(date(DEC_21), "2026-12-21");
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(DEC_21 - 1), "2026-12-20");
    }
}

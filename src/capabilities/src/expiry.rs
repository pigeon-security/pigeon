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

pub use pigeon_core::expiry::{Expiry, from_field, parse_time};

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
    // Each declared secret once, with the first capability that declares it.
    let mut slots: Vec<(&dyn Capability, String)> = Vec::new();
    for c in cx.hosted.capabilities {
        for slot in c.secrets() {
            if !slots.iter().any(|(_, n)| *n == slot.name) {
                slots.push((c.as_ref(), slot.name));
            }
        }
    }
    // The set ones are read at once, from the cache or their issuers: one slow issuer no
    // longer holds up the others.
    let rows = slots.iter().map(|(c, name)| async move {
        if set.contains(name) {
            let (expiry, checked) = secret_expiry(cx, *c, name).await;
            Row::new(name, "secret", c.name(), Some(expiry), Some(checked), now)
        } else {
            Row::new(name, "secret", c.name(), None, None, now)
        }
    });
    futures_util::future::join_all(rows).await
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

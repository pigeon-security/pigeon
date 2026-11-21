//! The AI Gateway's cost per UTC day and per model family, for the console's chart. It sums the
//! gateway logs' costs over a window of days. Pure: no I/O.

use pigeon_core::ai_gateway::{Log, canonical_model};
use serde::Serialize;

/// The families the chart draws, in its fixed order: their colours follow this order.
pub const FAMILIES: [&str; 3] = ["Opus", "Sonnet", "Haiku"];

/// A model's family: Opus, Sonnet or Haiku when its canonical name holds that word, else Other.
pub fn family(model: &str) -> &'static str {
    let name = canonical_model(model);
    FAMILIES
        .iter()
        .find(|f| name.contains(&f.to_lowercase()))
        .copied()
        .unwrap_or("Other")
}

/// The UTC day (`YYYY-MM-DD`) of an RFC 3339 time: its first ten characters, when they are a
/// valid date (digits, dashes in place, month 1–12, day 1–31); none otherwise.
pub fn day_of(time: &str) -> Option<&str> {
    let d = time.get(..10)?;
    let b = d.as_bytes();
    let shaped = b.iter().enumerate().all(|(i, c)| {
        if i == 4 || i == 7 {
            *c == b'-'
        } else {
            c.is_ascii_digit()
        }
    });
    if !shaped {
        return None;
    }
    let month = two_digits(b[5], b[6]);
    let day = two_digits(b[8], b[9]);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some(d)
}

/// The value of two ASCII digits.
fn two_digits(tens: u8, ones: u8) -> u32 {
    u32::from(tens - b'0') * 10 + u32::from(ones - b'0')
}

/// The year, month and day of a date.
fn parse_date(text: &str) -> Option<(i64, i64, i64)> {
    let d = day_of(text)?;
    Some((
        d[..4].parse().ok()?,
        d[5..7].parse().ok()?,
        d[8..10].parse().ok()?,
    ))
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

/// The civil date of a count of days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The `n` days ending with `today` (a `YYYY-MM-DD`), oldest first; empty when `today` is not a
/// date or `n` is 0.
pub fn days_ending(today: &str, n: usize) -> Vec<String> {
    let Some((y, m, d)) = parse_date(today) else {
        return Vec::new();
    };
    let end = days_from_civil(y, m, d);
    let n = n as i64;
    (0..n)
        .map(|i| {
            let (y, m, d) = civil_from_days(end - (n - 1 - i));
            format!("{y:04}-{m:02}-{d:02}")
        })
        .collect()
}

/// One family's cost on each day, and over all of them.
#[derive(Serialize, Debug, PartialEq)]
pub struct Family {
    pub name: &'static str,
    /// Its cost on each day of the window, oldest first.
    pub costs: Vec<f64>,
    pub total: f64,
}

/// Cost per day per family over a window.
#[derive(Serialize, Debug, PartialEq)]
pub struct Daily {
    /// The window's days, oldest first.
    pub days: Vec<String>,
    /// Every family in [`FAMILIES`], in that order, even at 0; then Other, only when it cost
    /// something.
    pub families: Vec<Family>,
    /// All families' cost over the window.
    pub cost: f64,
    /// The last 7 days' cost, whatever the window.
    pub cost_7d: f64,
}

/// `x` rounded to a millionth.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// `logs` summed by day and family over the `n` days ending `today`. A log outside the window,
/// or with no cost, adds nothing. `cost_7d` sums the window's last 7 days, or all of a shorter
/// one.
pub fn daily(logs: &[Log], today: &str, n: usize) -> Daily {
    let days = days_ending(today, n);
    let names: Vec<&'static str> = FAMILIES.iter().copied().chain(["Other"]).collect();
    let mut rows = vec![vec![0.0; days.len()]; names.len()];
    for log in logs {
        let Some(cost) = log.cost.filter(|c| c.is_finite()) else {
            continue;
        };
        let Some(day) = day_of(&log.created_at).and_then(|d| days.iter().position(|x| x == d))
        else {
            continue;
        };
        let row = names
            .iter()
            .position(|f| *f == family(&log.model))
            .unwrap_or(names.len() - 1);
        rows[row][day] += cost;
    }
    let families: Vec<Family> = names
        .iter()
        .zip(rows)
        .map(|(name, costs)| {
            let total = round6(costs.iter().sum());
            Family {
                name,
                costs: costs.into_iter().map(round6).collect(),
                total,
            }
        })
        .filter(|f| f.name != "Other" || f.total > 0.0)
        .collect();
    let last7 = days.len().saturating_sub(7);
    let cost = round6(families.iter().map(|f| f.total).sum());
    let cost_7d = round6(families.iter().flat_map(|f| f.costs[last7..].iter()).sum());
    Daily {
        days,
        families,
        cost,
        cost_7d,
    }
}

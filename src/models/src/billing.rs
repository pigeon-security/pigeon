//! A provider's own billed usage: Anthropic's Admin API usage and cost reports, read into days,
//! and a reconciliation of them with the gateway's own figures. Pure: no I/O, no clock.

use crate::daily::{day_of, days_ending};
use pigeon_core::ai_gateway::canonical_model;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The Secrets Store binding (and secret) holding the Anthropic Admin API key.
pub const ADMIN_KEY: &str = "ANTHROPIC_ADMIN_KEY";
/// The prefixes an Anthropic key that can read the Admin API starts with: an organization-scoped
/// user key (`sk-ant-usr-`, made with "Create API key", Scope Organization) or an older admin key
/// (`sk-ant-admin`).
pub const ADMIN_KEY_PREFIXES: [&str; 2] = ["sk-ant-usr-", "sk-ant-admin"];

/// Whether `value` has the shape of a key for the Admin API: `Ok(())`, or why not. Only the
/// prefix is checked; the live one-row usage read is the real test.
pub fn admin_key_shape(value: &str) -> Result<(), &'static str> {
    if ADMIN_KEY_PREFIXES.iter().any(|p| value.starts_with(p)) {
        Ok(())
    } else {
        Err(
            "an Anthropic Admin API key starts with `sk-ant-usr-` (an organization-scoped key \
             from Create API key in the Claude Console) or `sk-ant-admin` (an older admin key)",
        )
    }
}
/// Where the Admin API's reports live.
const BASE: &str = "https://api.anthropic.com/v1/organizations";
/// The most one-day buckets a usage page holds.
pub const USAGE_LIMIT: u32 = 31;
/// The most pages read of one report.
pub const MAX_PAGES: usize = 10;
/// A day matches when the costs differ by no more than this many dollars...
pub const MATCH_FLOOR_USD: f64 = 0.05;
/// ...or by this share of Anthropic's cost, whichever is more.
pub const MATCH_SHARE: f64 = 0.03;
/// Token counts agree when they differ by no more than this share.
pub const TOKEN_TOLERANCE: f64 = 0.03;

/// `s` percent-encoded for a query value.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The URL of one page of the usage report, one-day buckets grouped by model, from
/// `starting_at` to `ending_at` (RFC 3339), at most `limit` buckets, from `page` on.
pub fn usage_url(starting_at: &str, ending_at: &str, limit: u32, page: Option<&str>) -> String {
    let mut url = format!(
        "{BASE}/usage_report/messages?starting_at={}&ending_at={}&bucket_width=1d&group_by%5B%5D=model&limit={limit}",
        encode(starting_at),
        encode(ending_at)
    );
    if let Some(p) = page {
        url.push_str(&format!("&page={}", encode(p)));
    }
    url
}

/// The URL of one page of the cost report, grouped by description, from `page` on.
pub fn cost_url(starting_at: &str, ending_at: &str, page: Option<&str>) -> String {
    let mut url = format!(
        "{BASE}/cost_report?starting_at={}&ending_at={}&group_by%5B%5D=description",
        encode(starting_at),
        encode(ending_at)
    );
    if let Some(p) = page {
        url.push_str(&format!("&page={}", encode(p)));
    }
    url
}

/// `x` rounded to a millionth.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// A number sent as a JSON number or a string; 0 for anything else.
fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
}

/// Token counts of one day or one model.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tokens {
    /// Input tokens not served from the cache.
    pub uncached_input: f64,
    /// Output tokens.
    pub output: f64,
    /// Input tokens read from the cache.
    pub cache_read: f64,
    /// Input tokens written to the cache (5 minute and 1 hour).
    pub cache_write: f64,
}

impl Tokens {
    fn add(&mut self, o: &Tokens) {
        self.uncached_input += o.uncached_input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
    }

    fn json(&self) -> Value {
        json!({
            "uncached_input": self.uncached_input.round() as u64,
            "output": self.output.round() as u64,
            "cache_read": self.cache_read.round() as u64,
            "cache_write": self.cache_write.round() as u64,
        })
    }
}

/// Cost and tokens of one model on one day.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Entry {
    /// Cost in US dollars.
    pub cost_usd: f64,
    /// Tokens used.
    pub tokens: Tokens,
}

/// One day of Anthropic's usage.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Day {
    /// The day's cost in US dollars.
    pub cost_usd: f64,
    /// The day's tokens.
    pub tokens: Tokens,
    /// Cost and tokens per canonical model name.
    pub by_model: BTreeMap<String, Entry>,
}

/// What a page says of the pages after it.
#[derive(Debug, Default, PartialEq)]
pub struct More {
    /// Whether another page follows.
    pub has_more: bool,
    /// The token of the next page.
    pub next_page: Option<String>,
}

/// The `(day, results)` of each bucket of a page, and what follows it.
fn buckets(page: &Value) -> (Vec<(String, Vec<Value>)>, More) {
    let list = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let day = day_of(b["starting_at"].as_str()?)?.to_string();
            let results = b["results"].as_array().cloned().unwrap_or_default();
            Some((day, results))
        })
        .collect();
    let more = More {
        has_more: page["has_more"].as_bool().unwrap_or(false),
        next_page: page["next_page"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(String::from),
    };
    (list, more)
}

/// A model name as the answer shows it: canonical, or `other` when there is none.
fn model_name(v: &Value) -> String {
    match v.as_str().map(canonical_model) {
        Some(m) if !m.is_empty() => m,
        _ => "other".into(),
    }
}

/// Adds a usage report page's tokens to `days`, and says what follows the page.
pub fn add_usage(days: &mut BTreeMap<String, Day>, page: &Value) -> More {
    let (list, more) = buckets(page);
    for (day, results) in list {
        for r in results {
            let t = Tokens {
                uncached_input: num(&r["uncached_input_tokens"]),
                output: num(&r["output_tokens"]),
                cache_read: num(&r["cache_read_input_tokens"]),
                cache_write: num(&r["cache_creation"]["ephemeral_5m_input_tokens"])
                    + num(&r["cache_creation"]["ephemeral_1h_input_tokens"]),
            };
            let d = days.entry(day.clone()).or_default();
            d.tokens.add(&t);
            d.by_model
                .entry(model_name(&r["model"]))
                .or_default()
                .tokens
                .add(&t);
        }
    }
    more
}

/// Adds a cost report page's cost to `days`, and says what follows the page. `amount` is a
/// decimal string in cents.
pub fn add_cost(days: &mut BTreeMap<String, Day>, page: &Value) -> More {
    let (list, more) = buckets(page);
    for (day, results) in list {
        for r in results {
            let usd = num(&r["amount"]) / 100.0;
            let d = days.entry(day.clone()).or_default();
            d.cost_usd += usd;
            d.by_model
                .entry(model_name(&r["model"]))
                .or_default()
                .cost_usd += usd;
        }
    }
    more
}

/// The `models_billing` answer: `days` are the requested days, oldest first; `by_day` keeps only
/// those that have data; `unavailable` is `[{source, why}]` for a report that failed.
pub fn answer(days: &[String], by_day: &BTreeMap<String, Day>, unavailable: &[Value]) -> Value {
    let mut out = Map::new();
    let mut total = 0.0;
    for day in days {
        let Some(d) = by_day.get(day) else { continue };
        total += d.cost_usd;
        let models: Map<String, Value> = d
            .by_model
            .iter()
            .map(|(m, e)| {
                (
                    m.clone(),
                    json!({ "cost_usd": round6(e.cost_usd), "tokens": e.tokens.json() }),
                )
            })
            .collect();
        out.insert(
            day.clone(),
            json!({
                "cost_usd": round6(d.cost_usd),
                "tokens": d.tokens.json(),
                "by_model": models,
            }),
        );
    }
    json!({
        "provider": "anthropic",
        "source": "Anthropic Admin API (usage_report/messages, cost_report)",
        "days": days,
        "total_usd": round6(total),
        "by_day": out,
        "unavailable": unavailable,
    })
}

/// `x` as signed dollars, such as `+$0.40`.
fn signed(x: f64) -> String {
    if x < 0.0 {
        format!("-${:.2}", -x)
    } else {
        format!("+${x:.2}")
    }
}

/// The gateway's Anthropic cost and tokens on `day`, from `by_provider_model["anthropic"]`:
/// `(cost, tokens in + out, whether it has any request)`.
fn gateway_day(gateway: &Value, day: &str) -> (f64, f64, bool) {
    let (mut cost, mut tokens, mut any) = (0.0, 0.0, false);
    for by_day in gateway.as_object().into_iter().flat_map(|m| m.values()) {
        let d = &by_day[day];
        if d.is_object() {
            any = true;
            cost += num(&d["cost_usd"]);
            tokens += num(&d["tokens_in"]) + num(&d["tokens_out"]);
        }
    }
    (cost, tokens, any)
}

/// Gateway against Anthropic, per day of `days` and over the range. `gateway` is `models_daily`'s
/// `by_provider_model["anthropic"]`, `truncated` says its log listing was cut short, `anthropic`
/// is the `models_billing` answer, and `today` is `YYYY-MM-DD`. A day with no data on either
/// side is left out unless it is today or yesterday (then it is pending). Answers `{days: [{day, gateway_usd, anthropic_usd, difference_usd,
/// tokens_ratio, status}], gateway_usd, anthropic_usd, difference_usd, notes}`.
pub fn reconcile(
    gateway: &Value,
    truncated: bool,
    anthropic: &Value,
    days: &[String],
    today: &str,
) -> Value {
    let recent: Vec<String> = days_ending(today, 2);
    let (mut g_total, mut a_total) = (0.0, 0.0);
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for day in days {
        let (g_usd, g_tok, g_any) = gateway_day(gateway, day);
        let a = &anthropic["by_day"][day.as_str()];
        let has_a = a.is_object();
        g_total += g_usd;
        let (status, a_usd, diff, ratio) = if has_a {
            let a_usd = num(&a["cost_usd"]);
            let a_tok = num(&a["tokens"]["uncached_input"]) + num(&a["tokens"]["output"]);
            a_total += a_usd;
            let diff = a_usd - g_usd;
            let ratio = (a_tok > 0.0 && g_tok > 0.0).then(|| a_tok / g_tok);
            let agree = ratio.is_some_and(|r| (r - 1.0).abs() <= TOKEN_TOLERANCE);
            let status = if diff.abs() <= MATCH_FLOOR_USD.max(MATCH_SHARE * a_usd) {
                "match"
            } else if agree {
                "pricing"
            } else if a_tok > g_tok * (1.0 + TOKEN_TOLERANCE) && !truncated {
                "outside_gateway"
            } else if truncated {
                "gateway_truncated"
            } else {
                "pricing"
            };
            (status, Some(a_usd), Some(diff), ratio)
        } else if recent.contains(day) {
            ("anthropic_pending", None, None, None)
        } else if g_any {
            ("gateway_only", None, None, None)
        } else {
            continue;
        };
        if let (Some(a_usd), Some(diff)) = (a_usd, diff) {
            let why = match status {
                "pricing" => Some("tokens agree, so a pricing difference".to_string()),
                "outside_gateway" => Some(
                    "Anthropic counted more tokens than the gateway saw, so some use did not go through the gateway"
                        .to_string(),
                ),
                "gateway_truncated" => Some(
                    "the gateway's log listing was cut short, so its figure is a lower bound"
                        .to_string(),
                ),
                _ => None,
            };
            if let Some(why) = why {
                notes.push(format!(
                    "{day}: Anthropic billed ${a_usd:.2}, the gateway recorded ${g_usd:.2} ({}; {why}).",
                    signed(diff)
                ));
            }
        } else if status == "gateway_only" {
            notes.push(format!(
                "{day}: the gateway recorded ${g_usd:.2}; Anthropic reported nothing for the day."
            ));
        }
        rows.push(json!({
            "day": day,
            "gateway_usd": round6(g_usd),
            "anthropic_usd": a_usd.map(round6),
            "difference_usd": diff.map(round6),
            "tokens_ratio": ratio.map(round6),
            "status": status,
        }));
    }
    json!({
        "days": rows,
        "gateway_usd": round6(g_total),
        "anthropic_usd": round6(a_total),
        "difference_usd": round6(a_total - g_total),
        "notes": notes,
    })
}

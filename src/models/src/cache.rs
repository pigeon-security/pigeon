//! Prompt-cache tokens and what they cost. AI Gateway's logs carry only the uncached input and
//! its cost leaves cache reads and writes out, so the model gateway keeps its own rows of them
//! and `models_usage` adds them to the gateway's groups. The prices follow the AI agents'
//! budget (`pigeon-ai-agents`, `budget.rs`), which this crate cannot depend on. Pure: values
//! in, values out.

use std::collections::BTreeMap;

use pigeon_core::ai_gateway::{Dimension, Filter, Usage, canonical_model, tag};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long rows are kept, in hours: `models_usage`'s longest window.
pub const KEEP_HOURS: i64 = 744;

/// Micro-dollars per dollar.
const MICROS: f64 = 1_000_000.0;

/// Tokens per price unit: prices are micro-dollars per million tokens.
const PER_MILLION: u128 = 1_000_000;

/// A model's cache prices, in micro-dollars per million tokens.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    /// Input tokens read from the cache.
    pub cache_read: u64,
    /// Input tokens written to the 5-minute cache.
    pub cache_write_5m: u64,
    /// Input tokens written to the 1-hour cache.
    pub cache_write_1h: u64,
}

const OPUS: Price = Price {
    cache_read: 200_000,
    cache_write_5m: 5_000_000,
    cache_write_1h: 8_000_000,
};
const SONNET: Price = Price {
    cache_read: 100_000,
    cache_write_5m: 2_500_000,
    cache_write_1h: 4_000_000,
};
const HAIKU: Price = Price {
    cache_read: 10_000,
    cache_write_5m: 125_000,
    cache_write_1h: 200_000,
};
/// The dearest, and so the price of any unknown model.
const FABLE: Price = Price {
    cache_read: 800_000,
    cache_write_5m: 10_000_000,
    cache_write_1h: 16_000_000,
};

/// True when `model` is `name` or is `name` followed by a dash and a suffix.
fn is_model(model: &str, name: &str) -> bool {
    match model.strip_prefix(name) {
        Some(rest) => rest.is_empty() || rest.starts_with('-'),
        None => false,
    }
}

/// The cache prices of `model`, matched by prefix; an unknown model gets the dearest.
pub fn price(model: &str) -> Price {
    if is_model(model, "claude-opus-5-5") {
        OPUS
    } else if is_model(model, "claude-sonnet-5-5") {
        SONNET
    } else if is_model(model, "claude-haiku-5-5") {
        HAIKU
    } else {
        FABLE
    }
}

/// Cache tokens of one request.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Read from the cache.
    pub read: u64,
    /// Written to the 5-minute cache.
    pub write_5m: u64,
    /// Written to the 1-hour cache.
    pub write_1h: u64,
}

/// The cache tokens of an Anthropic `usage` object. A `cache_creation` split is used when it
/// is there; otherwise all writes count as 5-minute writes, as the budget does.
pub fn tokens(usage: &Value) -> Tokens {
    let n = |v: &Value, k: &str| v[k].as_u64().unwrap_or(0);
    let (write_5m, write_1h) = match usage.get("cache_creation") {
        Some(s) if s.is_object() => (
            n(s, "ephemeral_5m_input_tokens"),
            n(s, "ephemeral_1h_input_tokens"),
        ),
        _ => (n(usage, "cache_creation_input_tokens"), 0),
    };
    Tokens {
        read: n(usage, "cache_read_input_tokens"),
        write_5m,
        write_1h,
    }
}

/// What `t` costs on `model`, in whole micro-dollars, rounded up.
pub fn cost_micros(model: &str, t: Tokens) -> u64 {
    let p = price(model);
    let total = [
        (t.read, p.cache_read),
        (t.write_5m, p.cache_write_5m),
        (t.write_1h, p.cache_write_1h),
    ]
    .iter()
    .fold(0u128, |sum, (n, rate)| {
        sum.saturating_add(u128::from(*n) * u128::from(*rate))
    });
    u64::try_from(total.saturating_add(PER_MILLION - 1) / PER_MILLION).unwrap_or(u64::MAX)
}

/// One finished request's cache tokens and cost, as the index keeps them.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// When the job finished, in milliseconds since the epoch.
    pub finished_at: i64,
    /// The gateway's name for the provider.
    pub provider: String,
    /// The model, in [`canonical_model`]'s form.
    pub model: String,
    /// The `caller` tag.
    pub caller: Option<String>,
    /// The `caller_name` tag.
    pub caller_name: Option<String>,
    /// The `for` tag.
    pub for_id: Option<String>,
    /// The `for_name` tag.
    pub for_name: Option<String>,
    /// The `task` tag.
    pub task: Option<String>,
    /// Tokens read from the cache.
    pub cache_read: i64,
    /// Tokens written to the 5-minute cache.
    pub cache_write_5m: i64,
    /// Tokens written to the 1-hour cache.
    pub cache_write_1h: i64,
    /// What the cache tokens cost, in micro-dollars.
    pub cost_micros: i64,
}

impl Row {
    /// A row for a request on `model` with `tags` and `usage`, finished at `at`. `None` when the
    /// usage holds no cache tokens.
    pub fn new(
        at: i64,
        provider: &str,
        model: &str,
        tags: &[(String, String)],
        usage: &Value,
    ) -> Option<Row> {
        let t = tokens(usage);
        if t == Tokens::default() {
            return None;
        }
        let model = canonical_model(model);
        let get = |k: &str| tags.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        Some(Row {
            finished_at: at,
            provider: provider.into(),
            cost_micros: clamp(cost_micros(&model, t)),
            model,
            caller: get(tag::CALLER),
            caller_name: get(tag::CALLER_NAME),
            for_id: get(tag::FOR),
            for_name: get(tag::FOR_NAME),
            task: get(tag::TASK),
            cache_read: clamp(t.read),
            cache_write_5m: clamp(t.write_5m),
            cache_write_1h: clamp(t.write_1h),
        })
    }

    /// The row's tag `k`, as a log's `tag` would answer.
    fn tag(&self, k: &str) -> Option<&str> {
        match k {
            tag::CALLER => self.caller.as_deref(),
            tag::CALLER_NAME => self.caller_name.as_deref(),
            tag::FOR => self.for_id.as_deref(),
            tag::FOR_NAME => self.for_name.as_deref(),
            tag::TASK => self.task.as_deref(),
            _ => None,
        }
    }

    /// The row's group key along `by`, the same keys and values as `Usage.group`.
    fn key(&self, by: &[Dimension]) -> BTreeMap<String, Option<String>> {
        let tags = |keys: &[&str]| -> Vec<(String, Option<String>)> {
            keys.iter()
                .map(|k| (k.to_string(), self.tag(k).map(String::from)))
                .collect()
        };
        by.iter()
            .flat_map(|d| match d {
                Dimension::Model => vec![
                    ("provider".to_string(), Some(self.provider.clone())),
                    ("model".to_string(), Some(self.model.clone())),
                ],
                Dimension::Caller => {
                    tags(&[tag::CALLER, tag::CALLER_NAME, tag::FOR, tag::FOR_NAME])
                }
                Dimension::Task => tags(&[tag::TASK]),
            })
            .collect()
    }
}

/// Whether `row` is one to count: `filter` as for logs, and `task_prefix` when given. A row
/// is never a failure, so `failed` keeps none.
pub fn keeps(row: &Row, filter: &Filter, task_prefix: Option<&str>) -> bool {
    let caller = |c: &str| {
        [tag::CALLER, tag::CALLER_NAME, tag::FOR, tag::FOR_NAME]
            .iter()
            .any(|k| row.tag(k) == Some(c))
    };
    !filter.failed
        && filter.provider.as_deref().is_none_or(|p| row.provider == p)
        && filter
            .model
            .as_deref()
            .is_none_or(|m| row.model == canonical_model(m))
        && filter.caller.as_deref().is_none_or(caller)
        && filter
            .task
            .as_deref()
            .is_none_or(|t| row.tag(tag::TASK) == Some(t))
        && task_prefix.is_none_or(|p| row.tag(tag::TASK).is_some_and(|t| t.starts_with(p)))
}

/// Cache totals of one group.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    /// Tokens read from the cache.
    pub read: u64,
    /// Tokens written to either cache.
    pub write: u64,
    /// Micro-dollars.
    pub micros: u64,
}

impl Totals {
    /// Adds `row`.
    fn add(&mut self, row: &Row) {
        let n = |v: i64| v.max(0) as u64;
        self.read += n(row.cache_read);
        self.write += n(row.cache_write_5m) + n(row.cache_write_1h);
        self.micros += n(row.cost_micros);
    }

    /// The cache cost in dollars.
    pub fn dollars(&self) -> f64 {
        self.micros as f64 / MICROS
    }
}

/// A group's JSON: `usage` as it is, plus the cache fields and `cost_with_cache`.
fn group_json(usage: &Usage, t: Totals) -> Value {
    let mut v = serde_json::to_value(usage).unwrap_or(Value::Null);
    let cache = t.dollars();
    v["cache_read_tokens"] = json!(t.read);
    v["cache_write_tokens"] = json!(t.write);
    v["cache_cost"] = json!(cache);
    v["cost_with_cache"] = json!(usage.cost.unwrap_or(0.0) + cache);
    v
}

/// The gateway's `groups` with `rows` merged in by group key along `by`: a group the rows alone
/// make is new. The costliest first. Also the cache totals of all rows.
pub fn merge(groups: Vec<Usage>, rows: &[Row], by: &[Dimension]) -> (Vec<Value>, Totals) {
    let mut cache: BTreeMap<BTreeMap<String, Option<String>>, Totals> = BTreeMap::new();
    let mut all = Totals::default();
    for r in rows {
        cache.entry(r.key(by)).or_default().add(r);
        all.add(r);
    }
    let mut out: Vec<Value> = Vec::new();
    for u in groups {
        let t = cache.remove(&u.group).unwrap_or_default();
        out.push(group_json(&u, t));
    }
    for (key, t) in cache {
        let u = Usage {
            group: key,
            ..Usage::default()
        };
        out.push(group_json(&u, t));
    }
    let by_cost = |v: &Value| v["cost_with_cache"].as_f64().unwrap_or(0.0);
    out.sort_by(|a, b| {
        by_cost(b)
            .total_cmp(&by_cost(a))
            .then(b["requests"].as_u64().cmp(&a["requests"].as_u64()))
    });
    (out, all)
}

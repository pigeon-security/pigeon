//! Budgets for AI agent runs: what a run may use, what it has used, and model prices.
//!
//! This module is pure. It does no I/O and reads no clock. Times arrive as `i64` milliseconds.
//! Money is whole micro-dollars in `u64`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Role;

/// One million, the token count that prices are quoted per.
const PER_MILLION: u128 = 1_000_000;

/// One minute in milliseconds.
const MINUTE_MS: i64 = 60_000;

/// The limits placed on one AI agent run.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The most model turns the run may take.
    pub turns: u32,
    /// The most the run may spend, in micro-dollars.
    pub cost_micros: u64,
    /// The most wall-clock time the run may last, in milliseconds.
    pub wall_ms: i64,
    /// The most sandbox time the run may use, in milliseconds.
    pub sandbox_ms: i64,
    /// The most executors the run may spawn.
    pub executors: u32,
}

impl Budget {
    /// The hard ceiling. No run may ask for more than this.
    pub const HARD: Budget = Budget {
        turns: 500,
        cost_micros: 100_000_000,
        wall_ms: 24 * 60 * MINUTE_MS,
        sandbox_ms: 6 * 60 * MINUTE_MS,
        executors: 30,
    };

    /// The budget a run gets when it does not ask for one.
    pub fn default_for(role: Role) -> Budget {
        match role {
            Role::Planner => Budget {
                turns: 200,
                cost_micros: 25_000_000,
                wall_ms: 8 * 60 * MINUTE_MS,
                sandbox_ms: 30 * MINUTE_MS,
                executors: 12,
            },
            Role::Executor | Role::Mechanical => Budget {
                turns: 60,
                cost_micros: 5_000_000,
                wall_ms: 90 * MINUTE_MS,
                sandbox_ms: 60 * MINUTE_MS,
                executors: 0,
            },
        }
    }

    /// Builds a budget from what a caller asked for.
    ///
    /// It starts from the role's default. Then it applies each field given. It refuses values
    /// that are zero (except executors), above the hard ceiling, of the wrong type, or unknown.
    /// Only a planner may ask for executors.
    pub fn asked(role: Role, asked: &Value) -> Result<Budget, String> {
        let mut budget = Budget::default_for(role);
        let map = match asked {
            Value::Null => return Ok(budget),
            Value::Object(map) => map,
            _ => {
                return Err("refused the budget request: it must be null or an object".to_string());
            }
        };

        const KNOWN: [&str; 5] = ["turns", "cost_usd", "wall_min", "sandbox_min", "executors"];
        let unknown: Vec<&str> = map
            .keys()
            .map(String::as_str)
            .filter(|key| !KNOWN.contains(key))
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "refused the budget request: unknown key(s) {}; known keys are {}",
                unknown.join(", "),
                KNOWN.join(", ")
            ));
        }

        let hard = Budget::HARD;

        if let Some(n) = pick_whole(map, "turns", u64::from(hard.turns), false)? {
            budget.turns = u32::try_from(n).unwrap_or(hard.turns);
        }

        match map.get("cost_usd") {
            None | Some(Value::Null) => {}
            Some(value) => {
                let usd = value.as_f64().ok_or_else(|| {
                    "refused `cost_usd`: it must be a number of dollars".to_string()
                })?;
                if !usd.is_finite() || usd < 0.0 {
                    return Err(
                        "refused `cost_usd`: it must be a number greater than zero".to_string()
                    );
                }
                let micros = (usd * 1e6).round();
                if micros < 1.0 {
                    return Err(
                        "refused `cost_usd`: it must be greater than zero micro-dollars"
                            .to_string(),
                    );
                }
                if micros > hard.cost_micros as f64 {
                    return Err(format!(
                        "refused `cost_usd`: {usd} is over the cap of {}",
                        money(hard.cost_micros)
                    ));
                }
                budget.cost_micros = micros as u64;
            }
        }

        let wall_cap = (hard.wall_ms / MINUTE_MS) as u64;
        if let Some(n) = pick_whole(map, "wall_min", wall_cap, false)? {
            budget.wall_ms = (n as i64).saturating_mul(MINUTE_MS);
        }

        let sandbox_cap = (hard.sandbox_ms / MINUTE_MS) as u64;
        if let Some(n) = pick_whole(map, "sandbox_min", sandbox_cap, false)? {
            budget.sandbox_ms = (n as i64).saturating_mul(MINUTE_MS);
        }

        if let Some(n) = pick_whole(map, "executors", u64::from(hard.executors), true)? {
            budget.executors = u32::try_from(n).unwrap_or(hard.executors);
        }

        if role != Role::Planner && budget.executors > 0 {
            return Err("refused `executors`: only a planner spawns executors".to_string());
        }

        Ok(budget)
    }

    /// Checks that this child budget fits in what its planner has left.
    ///
    /// It compares turns, cost, wall time and sandbox time, in that order. The error names the
    /// first field that does not fit.
    pub fn within(&self, parent_left: &Budget) -> Result<(), String> {
        if self.turns > parent_left.turns {
            return Err(format!(
                "refused: the child asks for {} turns but the planner has {} left",
                self.turns, parent_left.turns
            ));
        }
        if self.cost_micros > parent_left.cost_micros {
            return Err(format!(
                "refused: the child asks for {} of cost but the planner has {} left",
                money(self.cost_micros),
                money(parent_left.cost_micros)
            ));
        }
        if self.wall_ms > parent_left.wall_ms {
            return Err(format!(
                "refused: the child asks for {} of wall time but the planner has {} left",
                duration(self.wall_ms),
                duration(parent_left.wall_ms)
            ));
        }
        if self.sandbox_ms > parent_left.sandbox_ms {
            return Err(format!(
                "refused: the child asks for {} of sandbox time but the planner has {} left",
                duration(self.sandbox_ms),
                duration(parent_left.sandbox_ms)
            ));
        }
        Ok(())
    }
}

/// Reads an optional whole-number field from a request object.
///
/// Missing or null gives `None`. Zero is refused unless `zero_ok` is set. Values above `cap`
/// are refused. The error names the field and the cap.
fn pick_whole(
    map: &serde_json::Map<String, Value>,
    key: &str,
    cap: u64,
    zero_ok: bool,
) -> Result<Option<u64>, String> {
    let value = match map.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(value) => value,
    };
    let n = value
        .as_u64()
        .ok_or_else(|| format!("refused `{key}`: it must be a whole number, not negative"))?;
    if n == 0 && !zero_ok {
        return Err(format!("refused `{key}`: it must be greater than zero"));
    }
    if n > cap {
        return Err(format!("refused `{key}`: {n} is over the cap of {cap}"));
    }
    Ok(Some(n))
}

/// Writes micro-dollars as a short dollar string, such as `$5` or `$0.25`.
pub fn money(micros: u64) -> String {
    let whole = micros / 1_000_000;
    let frac = micros % 1_000_000;
    if frac == 0 {
        return format!("${whole}");
    }
    let digits = format!("{frac:06}");
    format!("${whole}.{}", digits.trim_end_matches('0'))
}

/// Writes milliseconds as whole hours or minutes, such as `8 hours` or `90 minutes`.
fn duration(ms: i64) -> String {
    let minutes = ms / MINUTE_MS;
    if minutes != 0 && minutes % 60 == 0 {
        let hours = minutes / 60;
        let unit = if hours == 1 { "hour" } else { "hours" };
        return format!("{hours} {unit}");
    }
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!("{minutes} {unit}")
}

/// What a run has used so far.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Spent {
    /// Model turns taken.
    pub turns: u32,
    /// Money spent, in micro-dollars.
    pub cost_micros: u64,
    /// Sandbox time used, in milliseconds.
    pub sandbox_ms: i64,
    /// Executors spawned.
    pub executors: u32,
}

impl Spent {
    /// Works out what is left of a budget.
    ///
    /// Differences saturate at zero. Wall time left is the wall budget less the time since
    /// `started_at`, and never below zero.
    pub fn left(budget: &Budget, spent: &Spent, started_at: i64, now: i64) -> Budget {
        Budget {
            turns: budget.turns.saturating_sub(spent.turns),
            cost_micros: budget.cost_micros.saturating_sub(spent.cost_micros),
            wall_ms: budget
                .wall_ms
                .saturating_sub(elapsed(started_at, now))
                .max(0),
            sandbox_ms: budget.sandbox_ms.saturating_sub(spent.sandbox_ms).max(0),
            executors: budget.executors.saturating_sub(spent.executors),
        }
    }
}

/// Milliseconds since the start. A clock that ran backwards counts as zero.
fn elapsed(started_at: i64, now: i64) -> i64 {
    now.saturating_sub(started_at).max(0)
}

/// The next thing a run wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// A model turn, with an upper estimate of its cost.
    Turn {
        /// The most the turn is expected to cost, in micro-dollars.
        estimate_micros: u64,
    },
    /// A tool call that uses no sandbox time and no money.
    Tool,
    /// A sandbox run with a timeout.
    Sandbox {
        /// The longest the sandbox run may last, in milliseconds.
        timeout_ms: i64,
    },
    /// Spawning an executor, with the cost set aside for it.
    Spawn {
        /// The cost reserved for the new executor, in micro-dollars.
        cost_micros: u64,
    },
}

/// Which limit a run hit.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Over {
    /// The turn limit.
    Turns,
    /// The cost limit.
    Cost,
    /// The wall-clock limit.
    Wall,
    /// The sandbox time limit.
    Sandbox,
    /// The executor limit.
    Executors,
}

impl Over {
    /// A short stable name for the limit, the same as its JSON form.
    pub fn name(self) -> &'static str {
        match self {
            Over::Turns => "turns",
            Over::Cost => "cost",
            Over::Wall => "wall",
            Over::Sandbox => "sandbox",
            Over::Executors => "executors",
        }
    }

    /// A plain sentence saying which limit was hit, using the budget's numbers.
    pub fn why(self, budget: &Budget) -> String {
        match self {
            Over::Turns => format!("the run used its {} model turns", budget.turns),
            Over::Cost => format!(
                "the run spent its {} cost budget",
                money(budget.cost_micros)
            ),
            Over::Wall => format!("the run used its {} of wall time", duration(budget.wall_ms)),
            Over::Sandbox => format!(
                "the run used its {} of sandbox time",
                duration(budget.sandbox_ms)
            ),
            Over::Executors => format!("the run spawned its {} executors", budget.executors),
        }
    }
}

/// Decides whether the next step may go ahead.
///
/// Wall time is checked first, for every kind of step. Then the checks for that kind of step
/// run. The error says which limit was hit.
pub fn check(
    budget: &Budget,
    spent: &Spent,
    started_at: i64,
    now: i64,
    next: Next,
) -> Result<(), Over> {
    if elapsed(started_at, now) >= budget.wall_ms {
        return Err(Over::Wall);
    }
    match next {
        Next::Tool => Ok(()),
        Next::Turn { estimate_micros } => {
            if spent.turns >= budget.turns {
                return Err(Over::Turns);
            }
            if spent.cost_micros.saturating_add(estimate_micros) > budget.cost_micros {
                return Err(Over::Cost);
            }
            Ok(())
        }
        Next::Sandbox { timeout_ms } => {
            if spent.sandbox_ms.saturating_add(timeout_ms) > budget.sandbox_ms {
                return Err(Over::Sandbox);
            }
            Ok(())
        }
        Next::Spawn { cost_micros } => {
            if spent.executors >= budget.executors {
                return Err(Over::Executors);
            }
            if spent.cost_micros.saturating_add(cost_micros) > budget.cost_micros {
                return Err(Over::Cost);
            }
            Ok(())
        }
    }
}

/// A model's prices, in micro-dollars per million tokens.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    /// Fresh input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the cache.
    pub cache_read: u64,
    /// Input tokens written to the 5-minute cache.
    pub cache_write_5m: u64,
    /// Input tokens written to the 1-hour cache.
    pub cache_write_1h: u64,
}

/// The price of `claude-opus-5-5`.
const OPUS: Price = Price {
    input: 4_000_000,
    output: 20_000_000,
    cache_read: 200_000,
    cache_write_5m: 5_000_000,
    cache_write_1h: 8_000_000,
};

/// The price of `claude-sonnet-5-5`.
const SONNET: Price = Price {
    input: 2_000_000,
    output: 10_000_000,
    cache_read: 100_000,
    cache_write_5m: 2_500_000,
    cache_write_1h: 4_000_000,
};

/// The price of `claude-haiku-5-5`.
const HAIKU: Price = Price {
    input: 100_000,
    output: 500_000,
    cache_read: 10_000,
    cache_write_5m: 125_000,
    cache_write_1h: 200_000,
};

/// The price of `claude-fable-5-2`. It is also the fallback, being the dearest.
const FABLE: Price = Price {
    input: 8_000_000,
    output: 40_000_000,
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

/// Looks up the price for a model.
///
/// A dated name such as `claude-sonnet-5-5-20261101` matches by prefix. An unknown model gets
/// the dearest price, so it is never under-counted.
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

/// Reads a token count from a usage object. Missing, null or odd values are 0.
fn tokens(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Turns a sum of token-times-price into whole micro-dollars, rounding up.
fn round_up_micros(total: u128) -> u64 {
    let rounded = total.saturating_add(PER_MILLION - 1) / PER_MILLION;
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// Works out what a finished turn cost, in micro-dollars, rounded up.
///
/// If `cache_creation` gives a 5-minute and 1-hour split, it is used. Otherwise all
/// cache-creation tokens are priced as 5-minute writes.
pub fn cost_micros(model: &str, usage: &Value) -> u64 {
    let p = price(model);

    let (write_5m, write_1h) = match usage.get("cache_creation") {
        Some(split) if split.is_object() => (
            tokens(split, "ephemeral_5m_input_tokens"),
            tokens(split, "ephemeral_1h_input_tokens"),
        ),
        _ => (tokens(usage, "cache_creation_input_tokens"), 0),
    };

    let parts: [(u64, u64); 5] = [
        (tokens(usage, "input_tokens"), p.input),
        (tokens(usage, "output_tokens"), p.output),
        (tokens(usage, "cache_read_input_tokens"), p.cache_read),
        (write_5m, p.cache_write_5m),
        (write_1h, p.cache_write_1h),
    ];

    let total = parts.iter().fold(0u128, |sum, (count, rate)| {
        sum.saturating_add(u128::from(*count) * u128::from(*rate))
    });
    round_up_micros(total)
}

/// An upper estimate of a turn's cost before it runs, in micro-dollars.
///
/// All input is priced as fresh input. Then `max_tokens` is priced as output. The result is
/// rounded up.
pub fn estimate_micros(model: &str, input_tokens: u64, max_tokens: u64) -> u64 {
    let p = price(model);
    let total = (u128::from(input_tokens) * u128::from(p.input))
        .saturating_add(u128::from(max_tokens) * u128::from(p.output));
    round_up_micros(total)
}

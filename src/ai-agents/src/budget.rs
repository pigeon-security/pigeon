//! Budgets for AI agent runs: what a run may use, what it has used, and model prices.
//!
//! This module is pure. It does no I/O and reads no clock. Times arrive as `i64` milliseconds.
//! Money is whole micro-dollars in `u64`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Role;

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

/// Raises a live run's budget by `add` (`ai_agents_extend`): each field given is added to the
/// current limit. The result stays within [`Budget::HARD`]; an addition that would pass it is
/// refused, naming the field and the room left. Only a planner's executors may be raised.
/// Answers the new budget and what was added, in the request's units.
pub fn extend(role: Role, current: &Budget, add: &Value) -> Result<(Budget, Value), String> {
    let Value::Object(map) = add else {
        return Err(
            "give at least one of cost_usd, turns, wall_min, sandbox_min, executors".into(),
        );
    };
    const KNOWN: [&str; 5] = ["turns", "cost_usd", "wall_min", "sandbox_min", "executors"];
    if let Some(k) = map.keys().find(|k| !KNOWN.contains(&k.as_str())) {
        return Err(format!(
            "refused the extension: unknown key `{k}`; known keys are {}",
            KNOWN.join(", ")
        ));
    }
    let hard = Budget::HARD;
    let mut next = *current;
    let mut added = serde_json::Map::new();
    let room = |key: &str, now: u64, cap: u64, more: u64| -> Result<u64, String> {
        let total = now.saturating_add(more);
        if total > cap {
            return Err(format!(
                "refused `{key}`: adding {more} gives {total}, over the cap of {cap} ({} left)",
                cap.saturating_sub(now)
            ));
        }
        Ok(total)
    };
    if let Some(n) = pick_whole(map, "turns", u64::from(hard.turns), false)? {
        next.turns = room("turns", u64::from(current.turns), u64::from(hard.turns), n)? as u32;
        added.insert("turns".into(), n.into());
    }
    match map.get("cost_usd") {
        None | Some(Value::Null) => {}
        Some(v) => {
            let usd = v
                .as_f64()
                .filter(|u| u.is_finite() && *u > 0.0)
                .ok_or("refused `cost_usd`: it must be a number of dollars greater than zero")?;
            let micros = (usd * 1e6).round() as u64;
            let total = current.cost_micros.saturating_add(micros);
            if micros == 0 {
                return Err(
                    "refused `cost_usd`: it must be greater than zero micro-dollars".into(),
                );
            }
            if total > hard.cost_micros {
                return Err(format!(
                    "refused `cost_usd`: adding {} gives {}, over the cap of {} ({} left)",
                    money(micros),
                    money(total),
                    money(hard.cost_micros),
                    money(hard.cost_micros.saturating_sub(current.cost_micros))
                ));
            }
            next.cost_micros = total;
            added.insert("cost_usd".into(), usd.into());
        }
    }
    let minutes = |ms: i64| u64::try_from(ms / MINUTE_MS).unwrap_or(0);
    if let Some(n) = pick_whole(map, "wall_min", minutes(hard.wall_ms), false)? {
        room(
            "wall_min",
            minutes(current.wall_ms),
            minutes(hard.wall_ms),
            n,
        )?;
        next.wall_ms = current
            .wall_ms
            .saturating_add((n as i64).saturating_mul(MINUTE_MS));
        added.insert("wall_min".into(), n.into());
    }
    if let Some(n) = pick_whole(map, "sandbox_min", minutes(hard.sandbox_ms), false)? {
        room(
            "sandbox_min",
            minutes(current.sandbox_ms),
            minutes(hard.sandbox_ms),
            n,
        )?;
        next.sandbox_ms = current
            .sandbox_ms
            .saturating_add((n as i64).saturating_mul(MINUTE_MS));
        added.insert("sandbox_min".into(), n.into());
    }
    if let Some(n) = pick_whole(map, "executors", u64::from(hard.executors), false)? {
        if role != Role::Planner {
            return Err("refused `executors`: only a planner spawns executors".into());
        }
        next.executors = room(
            "executors",
            u64::from(current.executors),
            u64::from(hard.executors),
            n,
        )? as u32;
        added.insert("executors".into(), n.into());
    }
    if added.is_empty() {
        return Err(
            "give at least one of cost_usd, turns, wall_min, sandbox_min, executors".into(),
        );
    }
    Ok((next, Value::Object(added)))
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
pub fn duration(ms: i64) -> String {
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
    /// The part of `cost_micros` that was read from the gateway.
    #[serde(default)]
    pub gateway_micros: u64,
    /// Turns whose cost is still the local estimate and can be read from the gateway.
    #[serde(default)]
    pub unreconciled: u32,
    /// For planners: what their executors hold against the budget, in micro-dollars.
    #[serde(default)]
    pub executors_micros: u64,
    /// The format. 0 is old (cost holds executor reservations); 1 is current.
    #[serde(default)]
    pub v: u32,
}

/// The current [`Spent`] format: `cost_micros` holds no executor reservations.
pub const SPENT_VERSION: u32 = 1;

impl Spent {
    /// The cost counted against the budget: the run's own cost plus its executors' hold.
    pub fn committed_micros(&self) -> u64 {
        self.cost_micros.saturating_add(self.executors_micros)
    }

    /// Works out what is left of a budget.
    ///
    /// Differences saturate at zero. Wall time left is the wall budget less the time since
    /// `started_at`, and never below zero.
    pub fn left(budget: &Budget, spent: &Spent, started_at: i64, now: i64) -> Budget {
        Budget {
            turns: budget.turns.saturating_sub(spent.turns),
            cost_micros: budget.cost_micros.saturating_sub(spent.committed_micros()),
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

/// The share of its cost budget, in percent, at which a run gets its last turn.
pub const LAST_TURN_PERCENT: u64 = 90;

/// Whether the run's next turn must be its last: it has spent at least [`LAST_TURN_PERCENT`] of
/// its cost budget, or a normal turn (costing `estimate_micros`) would go over the budget.
pub fn last_turn_due(budget: &Budget, spent: &Spent, estimate_micros: u64) -> bool {
    let at_percent = u128::from(spent.committed_micros()).saturating_mul(100)
        >= u128::from(budget.cost_micros).saturating_mul(u128::from(LAST_TURN_PERCENT));
    at_percent || spent.committed_micros().saturating_add(estimate_micros) > budget.cost_micros
}

/// The share of `wall_ms`, in percent, kept as the wall-clock reserve for the last turn.
pub const LAST_TURN_WALL_PERCENT: i64 = 10;

/// The smallest wall-clock reserve for the last turn, in milliseconds (5 minutes).
pub const LAST_TURN_WALL_MIN_MS: i64 = 300_000;

/// The wall time kept for the last turn, in milliseconds: the larger of
/// [`LAST_TURN_WALL_PERCENT`] of `wall_ms` and [`LAST_TURN_WALL_MIN_MS`], never more than half
/// of `wall_ms`.
pub fn wall_reserve_ms(wall_ms: i64) -> i64 {
    let share = wall_ms.saturating_mul(LAST_TURN_WALL_PERCENT) / 100;
    share.max(LAST_TURN_WALL_MIN_MS).min(wall_ms / 2).max(0)
}

/// Which limit makes the next turn the run's last one, if any. Cost is checked first, then
/// turns (the next turn is the last the budget allows), then wall time (less than
/// [`wall_reserve_ms`] left). Only [`Over::Cost`], [`Over::Turns`] and [`Over::Wall`] are
/// answered.
pub fn last_turn_over(
    budget: &Budget,
    spent: &Spent,
    started_at: i64,
    now: i64,
    estimate_micros: u64,
) -> Option<Over> {
    if last_turn_due(budget, spent, estimate_micros) {
        Some(Over::Cost)
    } else if spent.turns.saturating_add(1) >= budget.turns {
        Some(Over::Turns)
    } else if budget.wall_ms.saturating_sub(elapsed(started_at, now))
        < wall_reserve_ms(budget.wall_ms)
    {
        Some(Over::Wall)
    } else {
        None
    }
}

/// The number stored in the `last_turn` column for a limit: 1 cost, 2 turns, 3 wall. Other
/// limits give 1.
pub fn last_turn_code(over: Over) -> i64 {
    match over {
        Over::Turns => 2,
        Over::Wall => 3,
        _ => 1,
    }
}

/// The limit a stored `last_turn` code stands for. Zero is no last turn; unknown codes read
/// as cost.
pub fn last_turn_from_code(code: i64) -> Option<Over> {
    match code {
        0 => None,
        2 => Some(Over::Turns),
        3 => Some(Over::Wall),
        _ => Some(Over::Cost),
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
            if spent.committed_micros().saturating_add(estimate_micros) > budget.cost_micros {
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
            if spent.committed_micros().saturating_add(cost_micros) > budget.cost_micros {
                return Err(Over::Cost);
            }
            Ok(())
        }
    }
}

/// One turn's cost row, as `reconcile` needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnCost {
    /// The row id (the turn number).
    pub id: i64,
    /// The gateway log id of the turn.
    pub log_id: String,
    /// What the run counted for the turn locally, in micro-dollars.
    pub local_micros: u64,
}

/// Turns dollars into whole micro-dollars, rounded. Bad numbers give `None`.
fn usd_to_micros(usd: f64) -> Option<u64> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    Some((usd * 1e6).round() as u64)
}

/// Reads a money string such as `$1.25` into micro-dollars.
fn parse_money(text: &str) -> Option<u64> {
    text.trim()
        .trim_start_matches('$')
        .parse::<f64>()
        .ok()
        .and_then(usd_to_micros)
}

/// What the executors of a planner hold against its budget, from the index's `children` answer.
///
/// A live child holds the larger of its cost budget and its own spend. An ended child holds
/// its own spend. Children are read from the `children` array (or the value itself if it is
/// an array). Numeric `cost_usd` fields are preferred; money strings are the fallback.
pub fn executors_committed(children: &Value) -> u64 {
    let list = children
        .get("children")
        .and_then(Value::as_array)
        .or_else(|| children.as_array());
    let Some(list) = list else {
        return 0;
    };
    let mut total: u64 = 0;
    for child in list {
        let spent = child
            .pointer("/spent/cost_usd")
            .and_then(Value::as_f64)
            .and_then(usd_to_micros)
            .or_else(|| {
                child
                    .pointer("/spent/cost")
                    .and_then(Value::as_str)
                    .and_then(parse_money)
            })
            .unwrap_or(0);
        let ended = child.get("ended_at").is_some_and(|v| !v.is_null());
        let held = if ended {
            spent
        } else {
            let budget = child
                .pointer("/budget/budget_cost_usd")
                .and_then(Value::as_f64)
                .and_then(usd_to_micros)
                .or_else(|| {
                    child
                        .pointer("/budget/cost")
                        .and_then(Value::as_str)
                        .and_then(parse_money)
                })
                .unwrap_or(0);
            budget.max(spent)
        };
        total = total.saturating_add(held);
    }
    total
}

/// Applies the gateway's `models_cost` answer to a run's spend.
///
/// For each row whose log was found with a cost, the local estimate is swapped for the gateway
/// cost. Rows not found, or with a null cost, change nothing. Answers the new spend and the
/// (row id, gateway micros) pairs to store.
pub fn reconcile(spent: Spent, rows: &[TurnCost], logs: &Value) -> (Spent, Vec<(i64, u64)>) {
    let mut spent = spent;
    let mut done: Vec<(i64, u64)> = Vec::new();
    let Some(list) = logs.get("logs").and_then(Value::as_array) else {
        return (spent, done);
    };
    for row in rows {
        let found = list.iter().find(|l| {
            l.get("log_id").and_then(Value::as_str) == Some(row.log_id.as_str())
                && l.get("found").and_then(Value::as_bool) == Some(true)
        });
        let Some(micros) = found
            .and_then(|l| l.get("cost_usd"))
            .and_then(Value::as_f64)
            .and_then(usd_to_micros)
        else {
            continue;
        };
        if done.iter().any(|(id, _)| *id == row.id) {
            continue;
        }
        spent.cost_micros = spent
            .cost_micros
            .saturating_sub(row.local_micros)
            .saturating_add(micros);
        spent.gateway_micros = spent.gateway_micros.saturating_add(micros);
        spent.unreconciled = spent.unreconciled.saturating_sub(1);
        done.push((row.id, micros));
    }
    (spent, done)
}

/// Moves an old (v 0) planner to the current format: its children's cost budgets were added
/// to `cost_micros` at spawn, so they are subtracted once (saturating). Other versions are
/// left as they are.
pub fn migrate_planner(spent: Spent, child_budgets_micros: u64) -> Spent {
    let mut spent = spent;
    if spent.v == 0 {
        spent.cost_micros = spent.cost_micros.saturating_sub(child_budgets_micros);
        spent.v = SPENT_VERSION;
    }
    spent
}

/// A model's prices, in micro-dollars per million tokens: the shared table's.
pub use pigeon_core::model_prices::Price;

/// Looks up the price for a model, from the shared table (`pigeon_core::model_prices`).
pub fn price(model: &str) -> Price {
    pigeon_core::model_prices::price(model)
}

/// What a finished turn cost, in micro-dollars, rounded up; the shared table's logic.
pub fn cost_micros(model: &str, usage: &Value) -> u64 {
    pigeon_core::model_prices::cost_micros(model, usage)
}

/// An upper estimate of a turn's cost before it runs, in micro-dollars; the shared table's.
pub fn estimate_micros(model: &str, input_tokens: u64, max_tokens: u64) -> u64 {
    pigeon_core::model_prices::estimate_micros(model, input_tokens, max_tokens)
}

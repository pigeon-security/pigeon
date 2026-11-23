//! Estimates what an AI agent run will cost, from similar past runs. It only suggests: nothing
//! here limits a run.
//!
//! This module is pure. It does no I/O and reads no clock.
//!
//! A past run is "similar" when it has the same role. Among those, the ten nearest by brief
//! size and kind are used. The distance is `|ln(size ratio)|`, plus 0.5 when the kinds differ.
//! Ties keep the order the runs were given in.
//!
//! Percentiles use the nearest-rank method: sort the `n` values, take the value at rank
//! `ceil(p / 100 * n)` (counting from 1). The "median" is the 50th percentile by that method.

use serde_json::{Value, json};

use crate::Role;
use crate::budget::Budget;

/// How many nearest past runs an estimate uses.
const NEAREST: usize = 10;
/// The fewest similar past runs an estimate needs. Below this, the role's default is suggested.
const MIN_SAMPLES: usize = 3;
/// The suggested cost is rounded up to a multiple of this, in micro-dollars ($0.50).
const STEP_MICROS: u64 = 500_000;

/// Words that mark a brief as documentation work.
const DOCS_WORDS: [&str; 4] = ["docs-only", "plan", "readme", "documentation"];
/// Words that mark a brief as a fix.
const FIX_WORDS: [&str; 4] = ["fix", "bug", "broken", "regression"];
/// Words that mark a brief as test work.
const TEST_WORDS: [&str; 2] = ["smoke", "test scenario"];
/// Words that mark a brief as a mechanical change.
const MECHANICAL_WORDS: [&str; 4] = ["rename", "move", "mechanical", "bump"];

/// The kind of work a brief asks for: `docs`, `fix`, `test`, `mechanical`, else `build`. The
/// match is by keyword, ignores case, and the first class (in that order) that matches wins.
pub fn kind(brief: &str) -> &'static str {
    let b = brief.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| b.contains(w));
    if has(&DOCS_WORDS) {
        "docs"
    } else if has(&FIX_WORDS) {
        "fix"
    } else if has(&TEST_WORDS) {
        "test"
    } else if has(&MECHANICAL_WORDS) {
        "mechanical"
    } else {
        "build"
    }
}

/// One ended past run, as the estimate sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Past {
    /// The run's role.
    pub role: Role,
    /// The size of its brief, in bytes.
    pub brief_bytes: usize,
    /// Its brief's kind, from [`kind`].
    pub kind: &'static str,
    /// How many executors it spawned (a planner's children).
    pub executors: u32,
    /// Its actual total cost in micro-dollars: its own, plus its executors' for a planner.
    pub cost_micros: u64,
}

/// A run's `spent` cost, in micro-dollars, from its `spent` JSON.
fn own_cost(spent: &Value) -> u64 {
    spent["cost_micros"].as_u64().unwrap_or(0)
}

/// The ended past runs in `rows`, with their actual costs.
///
/// Each row is a JSON object with `id`, `role`, `state`, `parent_run`, `brief` (text), `budget`
/// and `spent` (JSON objects) and `ended_at`. Rows that have not ended are not samples, and
/// neither are runs not `done`, `failed` or `over_budget`, nor `stopped` runs with no turns.
/// Rows of any state may serve as a planner's children.
///
/// A planner whose `spent` has no `v`, or `v == 0`, had each executor's cost budget added to its
/// cost at spawn: the sum of its children's budgets is taken off (never below zero). Its total
/// is then its own cost plus its children's own costs.
pub fn actual_costs(rows: &[Value]) -> Vec<Past> {
    let mut out = Vec::new();
    for row in rows {
        if row["ended_at"].as_i64().is_none() {
            continue;
        }
        let state = row["state"].as_str().unwrap_or_default();
        let turns = row["spent"]["turns"].as_u64().unwrap_or(0);
        match state {
            "done" | "failed" | "over_budget" => {}
            "stopped" if turns > 0 => {}
            _ => continue,
        }
        let Ok(role) = serde_json::from_value::<Role>(row["role"].clone()) else {
            continue;
        };
        let brief = row["brief"].as_str().unwrap_or_default();
        let mut cost = own_cost(&row["spent"]);
        let mut executors = 0u32;
        if role == Role::Planner {
            let id = row["id"].as_str().unwrap_or_default();
            let children = rows
                .iter()
                .filter(|c| !id.is_empty() && c["parent_run"].as_str() == Some(id));
            let (mut budgets, mut own) = (0u64, 0u64);
            for c in children {
                executors = executors.saturating_add(1);
                budgets = budgets.saturating_add(c["budget"]["cost_micros"].as_u64().unwrap_or(0));
                own = own.saturating_add(own_cost(&c["spent"]));
            }
            if row["spent"]["v"].as_u64().unwrap_or(0) == 0 {
                cost = cost.saturating_sub(budgets);
            }
            cost = cost.saturating_add(own);
        }
        out.push(Past {
            role,
            brief_bytes: brief.len(),
            kind: kind(brief),
            executors,
            cost_micros: cost,
        });
    }
    out
}

/// A budget suggested for a run. Only a suggestion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggested {
    /// The cost to suggest, in micro-dollars.
    pub cost_micros: u64,
    /// The turns to suggest, when there is a basis for it (the role's default).
    pub turns: Option<u32>,
    /// The executors to suggest, for a planner.
    pub executors: Option<u32>,
}

/// What an estimate says about a coming run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Estimate {
    /// How many past runs it rests on.
    pub samples: usize,
    /// The brief's kind.
    pub kind: &'static str,
    /// The median cost of those runs, in micro-dollars.
    pub median_micros: u64,
    /// Their 10th percentile cost.
    pub low_micros: u64,
    /// Their 90th percentile cost.
    pub high_micros: u64,
    /// For a planner: the median number of executors.
    pub executors_median: Option<u32>,
    /// For a planner: the 90th percentile number of executors.
    pub executors_high: Option<u32>,
    /// The budget to suggest.
    pub suggested: Suggested,
    /// A plain sentence on what the estimate rests on.
    pub note: String,
}

/// The `p`th percentile of `sorted` (ascending) by nearest rank; 0 when empty.
fn percentile<T: Copy + Default>(sorted: &[T], p: usize) -> T {
    if sorted.is_empty() {
        return T::default();
    }
    let rank = (p * sorted.len()).div_ceil(100).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// `high` raised by a quarter, rounded up to a multiple of $0.50, at least $0.50 and at most the
/// hard cap.
fn suggest_cost(high: u64) -> u64 {
    let raised = (u128::from(high) * 5).div_ceil(4);
    let step = u128::from(STEP_MICROS);
    let rounded = raised.div_ceil(step).max(1) * step;
    let cap = u128::from(Budget::HARD.cost_micros);
    u64::try_from(rounded.min(cap)).unwrap_or(Budget::HARD.cost_micros)
}

/// Estimates a run of `role` with `brief` from `past` runs. See the module docs for the method.
/// The suggested cost is the 90th percentile times 1.25, rounded up to $0.50. With fewer than
/// three similar runs it is the role's default.
pub fn estimate(role: Role, brief: &str, past: &[Past]) -> Estimate {
    let k = kind(brief);
    let size = brief.len().max(1) as f64;
    let mut near: Vec<(f64, &Past)> = past
        .iter()
        .filter(|p| p.role == role)
        .map(|p| {
            let ratio = (p.brief_bytes.max(1) as f64 / size).ln().abs();
            (ratio + if p.kind == k { 0.0 } else { 0.5 }, p)
        })
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(NEAREST);
    let n = near.len();
    let mut costs: Vec<u64> = near.iter().map(|(_, p)| p.cost_micros).collect();
    costs.sort_unstable();
    let mut execs: Vec<u32> = near.iter().map(|(_, p)| p.executors).collect();
    execs.sort_unstable();
    let planner = role == Role::Planner;
    let high = percentile(&costs, 90);
    let (suggested, note) = if n < MIN_SAMPLES {
        let d = Budget::default_for(role);
        (
            Suggested {
                cost_micros: d.cost_micros,
                turns: Some(d.turns),
                executors: planner.then_some(d.executors),
            },
            format!("not enough similar past runs ({n}); the role's default"),
        )
    } else {
        let executors = planner.then(|| {
            let raised = percentile(&execs, 90).saturating_mul(5).div_ceil(4);
            raised.clamp(1, Budget::HARD.executors)
        });
        (
            Suggested {
                cost_micros: suggest_cost(high),
                turns: None,
                executors,
            },
            format!(
                "from {n} similar past {} runs (kind {k}): the high end times 1.25, rounded up to $0.50",
                role.name()
            ),
        )
    };
    Estimate {
        samples: n,
        kind: k,
        median_micros: percentile(&costs, 50),
        low_micros: percentile(&costs, 10),
        high_micros: high,
        executors_median: planner.then(|| percentile(&execs, 50)),
        executors_high: planner.then(|| percentile(&execs, 90)),
        suggested,
        note,
    }
}

/// Dollars, as a JSON number, from micro-dollars.
fn usd(micros: u64) -> f64 {
    micros as f64 / 1e6
}

impl Estimate {
    /// The estimate as the tools show it: dollars as numbers.
    pub fn to_json(&self) -> Value {
        let mut budget = json!({ "cost_usd": usd(self.suggested.cost_micros) });
        if let Some(t) = self.suggested.turns {
            budget["turns"] = json!(t);
        }
        if let Some(e) = self.suggested.executors {
            budget["executors"] = json!(e);
        }
        let mut v = json!({
            "samples": self.samples,
            "kind": self.kind,
            "median_usd": usd(self.median_micros),
            "low_usd": usd(self.low_micros),
            "high_usd": usd(self.high_micros),
            "suggested_budget": budget,
            "note": self.note,
        });
        if let (Some(m), Some(h)) = (self.executors_median, self.executors_high) {
            v["executors"] = json!({ "median": m, "high": h });
        }
        v
    }
}

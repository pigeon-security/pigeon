//! Compares ended runs by role and model, for `ai_agents_compare`. Pure: no I/O, no clock.
//!
//! Percentiles use the nearest-rank method: sort the `n` values, take the value at rank
//! `ceil(p / 100 * n)` (counting from 1). The median is the 50th percentile, the "high" cost the 90th.

use serde_json::{Value, json};

use crate::Role;
use crate::models::{Slot, builtin};

/// The states of a run that has ended, as compared.
pub const ENDED: [&str; 4] = ["done", "failed", "over_budget", "stopped"];

/// The value at nearest rank `p` percent of `values` (sorted here); 0 when there are none.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    v[rank.min(v.len()) - 1]
}

/// Rounds to six decimals, so sums of floats read cleanly.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// Groups `rows` (index rows as JSON: `role`, `model`, `state`, `spent`, `created_at`,
/// `started_at`, `ended_at`) that ended, by role and model, one entry per group, in role then
/// model order. `role` keeps one role only. A run with no stored model counts as its role's
/// built-in default. Runs with a role that is not known, or that have not ended, are left out.
pub fn groups(rows: &[Value], role: Option<Role>) -> Vec<Value> {
    struct G {
        role: &'static str,
        model: String,
        kind: Option<String>,
        runs: u64,
        states: [u64; 4],
        costs: Vec<f64>,
        turns: Vec<f64>,
        minutes: Vec<f64>,
    }
    let mut out: Vec<G> = Vec::new();
    for r in rows {
        let Ok(run_role) = serde_json::from_value::<Role>(r["role"].clone()) else {
            continue;
        };
        if role.is_some_and(|w| w != run_role) {
            continue;
        }
        let Some(state) = r["state"].as_str() else {
            continue;
        };
        let Some(si) = ENDED.iter().position(|s| *s == state) else {
            continue;
        };
        let Some(ended) = r["ended_at"].as_i64() else {
            continue;
        };
        let model = r["model"]
            .as_str()
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| builtin(Slot::of(run_role)).text());
        let kind = r["kind"]
            .as_str()
            .filter(|k| !k.is_empty())
            .map(str::to_string);
        let begun = r["started_at"]
            .as_i64()
            .or_else(|| r["created_at"].as_i64())
            .unwrap_or(ended);
        let i = match out
            .iter()
            .position(|g| g.role == run_role.name() && g.model == model && g.kind == kind)
        {
            Some(i) => i,
            None => {
                out.push(G {
                    role: run_role.name(),
                    model,
                    kind,
                    runs: 0,
                    states: [0; 4],
                    costs: Vec::new(),
                    turns: Vec::new(),
                    minutes: Vec::new(),
                });
                out.len() - 1
            }
        };
        let g = &mut out[i];
        g.runs += 1;
        g.states[si] += 1;
        g.costs
            .push(r["spent"]["cost_micros"].as_u64().unwrap_or(0) as f64 / 1e6);
        g.turns
            .push(r["spent"]["turns"].as_u64().unwrap_or(0) as f64);
        g.minutes
            .push(((ended - begun).max(0) as f64 / 60_000.0 * 10.0).round() / 10.0);
    }
    out.sort_by(|a, b| (a.role, &a.model, &a.kind).cmp(&(b.role, &b.model, &b.kind)));
    out.iter()
        .map(|g| {
            json!({
                "role": g.role,
                "model": g.model,
                "kind": g.kind,
                "route": crate::models::route(g.model.split('/').next().unwrap_or_default()),
                "runs": g.runs,
                "done": g.states[0],
                "failed": g.states[1],
                "over_budget": g.states[2],
                "stopped": g.states[3],
                "cost_usd": {
                    "total": round6(g.costs.iter().sum()),
                    "median": round6(percentile(&g.costs, 50.0)),
                    "high": round6(percentile(&g.costs, 90.0)),
                },
                "turns": { "median": percentile(&g.turns, 50.0) },
                "minutes": { "median": percentile(&g.minutes, 50.0) },
            })
        })
        .collect()
}

/// The `days` argument of `ai_agents_compare`: 30 by default, 1 to 365.
pub fn days_arg(v: &Value) -> Result<u32, String> {
    match v {
        Value::Null => Ok(30),
        v => match v.as_u64() {
            Some(d) if (1..=365).contains(&d) => Ok(d as u32),
            _ => Err("`days` must be a whole number from 1 to 365".into()),
        },
    }
}

/// The `role` argument of `ai_agents_compare`, if given.
pub fn role_arg(v: &Value) -> Result<Option<Role>, String> {
    match v.as_str() {
        None if v.is_null() => Ok(None),
        Some("planner") => Ok(Some(Role::Planner)),
        Some("executor") => Ok(Some(Role::Executor)),
        Some("mechanical") => Ok(Some(Role::Mechanical)),
        _ => Err("`role` must be `planner`, `executor` or `mechanical`".into()),
    }
}

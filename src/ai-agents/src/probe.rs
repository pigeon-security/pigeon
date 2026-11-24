//! The two read-only agent tools `model_probe` and `run_spend`: their input checks, the probe
//! limits, how a probe's answer is shaped, and what `run_spend` answers.
//!
//! This module is pure. It does no I/O and reads no clock. The run's object does the gateway
//! call and the storage.

use serde_json::{Map, Value, json};

use crate::Role;
use crate::budget::{self, Budget, Over, Spent};
use crate::models::{Choice, Slot};

/// The most probes one run may send.
pub const MAX_PROBES: u32 = 10;

/// The most output tokens a probe may ask for.
pub const MAX_TOKENS: u64 = 64;

/// The output tokens a probe asks for when it says nothing.
pub const DEFAULT_TOKENS: u64 = 16;

/// The longest probe prompt, in characters.
pub const MAX_PROMPT_CHARS: usize = 500;

/// The prompt a probe sends when it says nothing.
pub const DEFAULT_PROMPT: &str = "Reply with the single word: ok";

/// The longest a probe may take, in milliseconds.
pub const TIMEOUT_MS: u64 = 30_000;

/// The most characters of the reply a probe answers.
const TEXT_CHARS: usize = 200;

/// The most characters of the gateway's error text a probe answers.
const ERROR_CHARS: usize = 500;

/// The keys `model_probe` takes.
const PROBE_KEYS: [&str; 3] = ["model", "prompt", "max_tokens"];

/// A checked probe request.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    /// The provider to ask: `anthropic`, `cf-anthropic`, `cf-openai` or `workers-ai`.
    pub provider: String,
    /// The model id that provider knows.
    pub model: String,
    /// The prompt to send.
    pub prompt: String,
    /// The most output tokens to ask for.
    pub max_tokens: u64,
}

/// Refuses keys other than `allowed`.
fn only_keys(obj: &Map<String, Value>, allowed: &[&str], tool: &str) -> Result<(), String> {
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        let known = if allowed.is_empty() {
            "none".to_string()
        } else {
            allowed.join(", ")
        };
        return Err(format!(
            "`{k}` is not an input of `{tool}`; its inputs are: {known}"
        ));
    }
    Ok(())
}

/// Checks a `model_probe` input and reads it. The model is checked with the rules of a run's
/// `model` field, for the slot of `role`.
pub fn parse(role: Role, input: &Value) -> Result<Request, String> {
    let obj = input
        .as_object()
        .ok_or("the input to `model_probe` must be a JSON object; send named fields")?;
    only_keys(obj, &PROBE_KEYS, "model_probe")?;
    let model = match obj.get("model") {
        Some(Value::String(s)) => s,
        Some(_) => return Err("`model` must be a string like cf-openai/gpt-6.2-sol".into()),
        None => return Err("`model` is required: a model like cf-openai/gpt-6.2-sol".into()),
    };
    let choice = Choice::parse(model, Slot::of(role)).map_err(|e| format!("`model`: {e}"))?;
    let prompt = match obj.get("prompt") {
        None | Some(Value::Null) => DEFAULT_PROMPT.to_string(),
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n == 0 {
                return Err("`prompt` must not be empty; leave it out for the default".into());
            }
            if n > MAX_PROMPT_CHARS {
                return Err(format!(
                    "`prompt` is {n} characters long and the limit is {MAX_PROMPT_CHARS}; shorten it"
                ));
            }
            s.clone()
        }
        Some(_) => return Err("`prompt` must be a string; send it as text".into()),
    };
    let max_tokens = match obj.get("max_tokens") {
        None | Some(Value::Null) => DEFAULT_TOKENS,
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_TOKENS).contains(&n) => n,
            Some(n) => {
                return Err(format!(
                    "`max_tokens` is {n} but must be between 1 and {MAX_TOKENS}; pick a value in that range"
                ));
            }
            None => {
                return Err(format!(
                    "`max_tokens` must be a whole number from 1 to {MAX_TOKENS}"
                ));
            }
        },
    };
    Ok(Request {
        provider: choice.provider,
        model: choice.model,
        prompt,
        max_tokens,
    })
}

/// Checks a `run_spend` input: an object with no keys.
pub fn check_spend(input: &Value) -> Result<(), String> {
    let obj = input
        .as_object()
        .ok_or("the input to `run_spend` must be a JSON object; send {}")?;
    only_keys(obj, &[], "run_spend")
}

/// The gateway task label of probe `n` of `run`: it sits under the run's cost tag prefix.
pub fn task(run: &str, n: u32) -> String {
    let t = format!("ai-agent/{run}/probe/{n}");
    t.chars().take(100).collect()
}

/// Whether a run that has sent `used` probes may send another.
pub fn may_send(used: u32) -> Result<(), String> {
    if used >= MAX_PROBES {
        return Err(format!(
            "refused: this run has used its {MAX_PROBES} probes; spawn an executor to test further"
        ));
    }
    Ok(())
}

/// Whether the run's budget allows a probe that may cost up to `estimate_micros`. The wall, cost
/// and last-turn checks apply, as for a turn.
pub fn budget_allows(
    budget: &Budget,
    spent: &Spent,
    started_at: i64,
    now: i64,
    estimate_micros: u64,
) -> Result<(), String> {
    let refused = |over: Over| {
        format!(
            "refused: {}; finish with what you have and report",
            over.why(budget)
        )
    };
    // A probe takes no turn, so the turn count does not refuse it; wall time and cost do.
    budget::check(budget, spent, started_at, now, budget::Next::Tool).map_err(refused)?;
    if spent.committed_micros().saturating_add(estimate_micros) > budget.cost_micros {
        return Err(refused(Over::Cost));
    }
    match budget::last_turn_over(budget, spent, started_at, now, estimate_micros) {
        Some(over @ (Over::Cost | Over::Wall)) => Err(format!(
            "refused: the run is at its last turn ({}); finish with what you have and report",
            over.name()
        )),
        _ => Ok(()),
    }
}

/// What the gateway gave back for a probe.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The gateway answered: its reply in the common shape.
    Reply(Value),
    /// The gateway refused, or the provider failed: the text exactly as given.
    Failed(String),
    /// Nothing came back within [`TIMEOUT_MS`].
    TimedOut,
}

/// A probe's answer, and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Shaped {
    /// The JSON the tool answers.
    pub answer: Value,
    /// The cost to count against the run, in micro-dollars.
    pub cost_micros: u64,
    /// Whether that cost came from the gateway (else it is an estimate).
    pub from_gateway: bool,
}

/// Cuts `text` to at most `max` characters.
fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The HTTP-style status in a gateway error such as `Anthropic answered 429: …`, if any.
pub fn status_of(error: &str) -> Option<u64> {
    let rest = error.split_once(" answered ")?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() == 3 {
        digits.parse().ok()
    } else {
        None
    }
}

/// Shapes the gateway's outcome for `model` into the tool's answer. `latency_ms` is how long the
/// call took. The cost is the gateway's when its reply gives `cost_usd`, else estimated with
/// [`budget::cost_micros`]; `cost_source` says which. A failure costs nothing.
pub fn shape(model: &str, outcome: &Outcome, latency_ms: i64) -> Shaped {
    match outcome {
        Outcome::Reply(r) => {
            let input = r["tokens_in"].as_u64().unwrap_or(0);
            let output = r["tokens_out"].as_u64().unwrap_or(0);
            let gateway = r["cost_usd"]
                .as_f64()
                .filter(|c| c.is_finite() && *c >= 0.0)
                .map(|c| (c * 1e6).round() as u64);
            let usage = json!({ "input_tokens": input, "output_tokens": output });
            let cost = gateway.unwrap_or_else(|| budget::cost_micros(model, &usage));
            let source = if gateway.is_some() {
                "gateway"
            } else {
                "estimate"
            };
            Shaped {
                answer: json!({
                    "ok": true,
                    "status": 200,
                    "latency_ms": latency_ms,
                    "input_tokens": input,
                    "output_tokens": output,
                    "cost_usd": cost as f64 / 1e6,
                    "cost_source": source,
                    "text": cut(r["text"].as_str().unwrap_or_default(), TEXT_CHARS),
                    "error": null,
                }),
                cost_micros: cost,
                from_gateway: gateway.is_some(),
            }
        }
        Outcome::Failed(e) => Shaped {
            answer: failure(
                json!(status_of(e).map_or(json!("error"), |s| json!(s))),
                e,
                latency_ms,
            ),
            cost_micros: 0,
            from_gateway: false,
        },
        Outcome::TimedOut => Shaped {
            answer: failure(
                json!("timeout"),
                "no answer from the gateway within 30 s; the cost of the request is not counted",
                latency_ms,
            ),
            cost_micros: 0,
            from_gateway: false,
        },
    }
}

/// The answer for a probe that did not work.
fn failure(status: Value, error: &str, latency_ms: i64) -> Value {
    json!({
        "ok": false,
        "status": status,
        "latency_ms": latency_ms,
        "input_tokens": 0,
        "output_tokens": 0,
        "cost_usd": 0.0,
        "cost_source": "estimate",
        "text": "",
        "error": cut(error, ERROR_CHARS),
    })
}

/// Adds a probe's cost to a run's spend. Gateway money also counts in `gateway_micros`. No
/// `turn_costs` row is made, so the reconcile pass never touches it and `unreconciled` stays.
pub fn charge(spent: &Spent, shaped: &Shaped) -> Spent {
    let mut s = *spent;
    s.cost_micros = s.cost_micros.saturating_add(shaped.cost_micros);
    if shaped.from_gateway {
        s.gateway_micros = s.gateway_micros.saturating_add(shaped.cost_micros);
    }
    s
}

/// What `run_spend` answers: this run's own spend and what is left on every axis. It uses only
/// what the run keeps; it asks nobody.
pub fn spend_answer(
    budget: &Budget,
    spent: &Spent,
    started_at: i64,
    now: i64,
    probes_used: u32,
) -> Value {
    let left = Spent::left(budget, spent, started_at, now);
    let minutes = |ms: i64| ms.max(0) / 60_000;
    let source = if spent.cost_micros == 0 || spent.gateway_micros >= spent.cost_micros {
        "gateway"
    } else if spent.gateway_micros == 0 {
        "estimate"
    } else {
        "mixed"
    };
    json!({
        "turns": spent.turns,
        "cost": budget::money(spent.cost_micros),
        "cost_usd": spent.cost_micros as f64 / 1e6,
        "cost_source": source,
        "gateway_cost_usd": spent.gateway_micros as f64 / 1e6,
        "turns_unreconciled": spent.unreconciled,
        "executors_cost": budget::money(spent.executors_micros),
        "executors_cost_usd": spent.executors_micros as f64 / 1e6,
        "probes_used": probes_used,
        "left": {
            "turns": left.turns,
            "cost": budget::money(left.cost_micros),
            "cost_usd": left.cost_micros as f64 / 1e6,
            "wall_min": minutes(left.wall_ms),
            "sandbox_min": minutes(left.sandbox_ms),
            "executors": left.executors,
            "probes": MAX_PROBES.saturating_sub(probes_used),
        },
        "budget": {
            "turns": budget.turns,
            "cost": budget::money(budget.cost_micros),
            "wall_min": minutes(budget.wall_ms),
            "sandbox_min": minutes(budget.sandbox_ms),
            "executors": budget.executors,
        },
        "executors_spawned": spent.executors,
        "sandbox_min_used": minutes(spent.sandbox_ms),
        "note": "cost counts this run and what its executors hold; probes are in cost",
    })
}

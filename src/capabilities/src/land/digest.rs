//! Compact answers for AI agent planners: a landing as one small row, the list filter of
//! `land_list`, and the `work_status` digest. Everything here is pure: it shapes the Values the
//! sources answered, so it can be tested without a Worker.

use serde_json::{Map, Value, json};

use super::state::State;

/// The size `work_status` aims to stay under, in bytes.
pub const DIGEST_BYTES: usize = 8000;

/// `s` cut to at most `n` characters.
pub fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The first non-empty line of `s`, cut to at most `n` characters.
pub fn one_line(s: &str, n: usize) -> String {
    cut(
        s.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or(""),
        n,
    )
}

/// A landing as `land_status` answers it (its `public` shape), as one small row. `incident` says
/// whether an incident included it.
pub fn compact(row: &Value, incident: bool) -> Value {
    let branch = row["branch"].as_str().unwrap_or_default();
    json!({
        "id": row["id"],
        "branch": branch.strip_prefix("refs/heads/").unwrap_or(branch),
        "commit": row["commit"].as_str().map(|c| cut(c, 12)),
        "state": row["state"],
        "workers": row["workers"],
        "incident": incident,
        "detail": one_line(row["detail"].as_str().unwrap_or_default(), 160),
        "updated_at": row["updated_at"],
    })
}

/// A full `land_status` answer as the compact one: the compact row, its last step only, its
/// incidents as `{id, worker, state}`, and the queue as it was.
pub fn compact_status(full: &Value) -> Value {
    let incidents: Vec<Value> = full["incidents"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| json!({ "id": i["id"], "worker": i["worker"], "state": i["state"] }))
        .collect();
    let mut out = compact(full, !incidents.is_empty());
    out["last_step"] = full["steps"]
        .as_array()
        .and_then(|s| s.last())
        .map(|s| {
            json!({
                "state": s["state"], "at": s["at"],
                "detail": cut(s["detail"].as_str().unwrap_or_default(), 200),
            })
        })
        .unwrap_or(Value::Null);
    out["incidents"] = json!(incidents);
    out["queue"] = full["queue"].clone();
    out
}

/// The states a `land_list` or `work_status` filter names: `None` for no filter, `live` for the
/// live states, `ended` for the terminal ones, else the one state named.
pub fn state_filter(filter: Option<&str>) -> Result<Option<Vec<State>>, String> {
    let Some(f) = filter.map(str::trim).filter(|f| !f.is_empty()) else {
        return Ok(None);
    };
    match f {
        "live" => Ok(Some(
            State::ALL.into_iter().filter(|s| !s.terminal()).collect(),
        )),
        "ended" => Ok(Some(
            State::ALL.into_iter().filter(|s| s.terminal()).collect(),
        )),
        name => match State::named(name) {
            Some(s) => Ok(Some(vec![s])),
            None => Err(format!(
                "`state` is `live`, `ended` or one of: {}",
                State::ALL.map(State::name).join(", ")
            )),
        },
    }
}

/// What `work_status` was asked to answer in full.
#[derive(Debug, PartialEq, Eq)]
pub enum Detail {
    /// `run:<id>`: the run as `ai_agents_status` answers it.
    Run(String),
    /// `landing:<id>`: the landing as `land_status` answers it.
    Landing(String),
}

/// Parses the `detail` argument: `run:<id>` or `landing:<id>`.
pub fn parse_detail(detail: &str) -> Result<Detail, String> {
    let bad = || {
        format!(
            "`detail` is `run:<id>` or `landing:<id>`, not `{}`",
            cut(detail, 60)
        )
    };
    let (kind, id) = detail.split_once(':').ok_or_else(bad)?;
    let id = id.trim();
    if id.is_empty() {
        return Err(bad());
    }
    match kind.trim() {
        "run" => Ok(Detail::Run(id.to_string())),
        "landing" => Ok(Detail::Landing(id.to_string())),
        _ => Err(bad()),
    }
}

/// A money string such as `$0.25` as dollars.
fn dollars(v: &Value) -> Option<f64> {
    v.as_str()?.trim_start_matches('$').parse().ok()
}

/// Dollars rounded to four places, so the digest stays short.
fn usd(x: f64) -> Value {
    json!((x * 10_000.0).round() / 10_000.0)
}

/// One run of `ai_agents_status` as a digest entry: no brief and no report.
pub fn run_entry(run: &Value, ended: bool) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            m.insert(k.to_string(), v);
        }
    };
    put("run", run["run"].clone());
    put("role", run["role"].clone());
    put("parent", run["parent_run"].clone());
    put(
        "title",
        json!(one_line(run["brief"].as_str().unwrap_or_default(), 80)),
    );
    put("state", run["state"].clone());
    put(
        "step",
        run["step"]
            .as_str()
            .map(|s| json!(cut(s, 100)))
            .unwrap_or(Value::Null),
    );
    let spent = &run["spent"];
    put(
        "spent_usd",
        spent["cost_usd"].as_f64().map(usd).unwrap_or(Value::Null),
    );
    put(
        "gateway_usd",
        dollars(&spent["gateway_cost"])
            .map(usd)
            .unwrap_or(Value::Null),
    );
    put("cost_source", spent["cost_source"].clone());
    put(
        "total_usd",
        dollars(&spent["total_cost"])
            .map(usd)
            .unwrap_or(Value::Null),
    );
    put(
        "budget_usd",
        run["budget"]["budget_cost_usd"]
            .as_f64()
            .map(usd)
            .unwrap_or(Value::Null),
    );
    if let Some(used) = spent["turns"].as_i64() {
        let limit = run["budget"]["turns"]
            .as_i64()
            .map_or("?".to_string(), |l| l.to_string());
        put("turns", json!(format!("{used}/{limit}")));
    }
    let wait = run["waiting"]["for"].as_str();
    put(
        "waiting_on_person",
        json!(matches!(wait, Some("approval" | "answer"))),
    );
    if ended {
        put("ended", json!(true));
    }
    Value::Object(m)
}

/// The runs of an `ai_agents_status` list worth showing: those not ended, then (when `since` is
/// given) those ended after it, marked `ended`.
pub fn runs(status: &Value, since: Option<i64>) -> Vec<Value> {
    let all = status["runs"].as_array().cloned().unwrap_or_default();
    let mut live: Vec<Value> = Vec::new();
    let mut ended: Vec<Value> = Vec::new();
    for r in &all {
        match (r["ended_at"].as_i64(), since) {
            (None, _) => live.push(run_entry(r, false)),
            (Some(at), Some(s)) if at > s => ended.push(run_entry(r, true)),
            _ => {}
        }
    }
    live.extend(ended);
    live
}

/// The counts of what waits for a person, from an `approvals_list` answer.
pub fn waiting_counts(approvals: &Value) -> Value {
    let n = |k: &str| approvals[k].as_array().map_or(0, Vec::len);
    json!({
        "calls": n("calls"), "drafts": n("drafts"), "plans": n("plans"),
        "other": n("other"), "landings": n("landings"),
    })
}

/// The runs of an `ai_agents_inbox` answer that wait on a person: each question (cut to 200
/// characters) and how many wait on a passkey.
pub fn questions(inbox: &Value) -> (Vec<Value>, usize) {
    let mut asks = Vec::new();
    let mut passkeys = 0;
    for r in inbox["waiting"].as_array().into_iter().flatten() {
        match r["waiting"]["for"].as_str() {
            Some("answer") => asks.push(json!({
                "run": r["run"],
                "question": cut(r["waiting"]["question"].as_str().unwrap_or_default(), 200),
            })),
            Some("approval") => passkeys += 1,
            _ => {}
        }
    }
    (asks, passkeys)
}

/// The latest release of an `ios_releases` answer, or null.
pub fn release(releases: &Value) -> Value {
    let Some(r) = releases["releases"].as_array().and_then(|a| a.first()) else {
        return Value::Null;
    };
    json!({
        "job": r["job"], "build_number": r["build_number"], "version": r["version"],
        "state": r["state"], "apple_processing": r["apple_processing"],
        "commit": r["commit"].as_str().map(|c| cut(c, 12)),
    })
}

/// The landings of two compact lists: `live` is the queue, `recent` the last 8 (those updated
/// after `since` when given).
pub fn landings(live: &Value, recent: &Value, since: Option<i64>) -> Value {
    let rows = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let recent: Vec<Value> = rows(recent)
        .into_iter()
        .filter(|r| since.is_none_or(|s| r["updated_at"].as_i64().unwrap_or(0) > s))
        .take(8)
        .collect();
    json!({ "queue": rows(live), "recent": recent })
}

/// What each source of the digest answered: its Value, or why it did not.
pub struct Sources {
    /// `ai_agents_status` with a limit.
    pub runs: Result<Value, String>,
    /// The Lander's compact live list and compact recent list.
    pub landings: Result<(Value, Value), String>,
    /// `approvals_list`.
    pub approvals: Result<Value, String>,
    /// `ai_agents_inbox`.
    pub inbox: Result<Value, String>,
    /// `ios_releases`.
    pub release: Result<Value, String>,
    /// The token gauge (`live`, `soft_ceiling`, `hard_cap`, `as_of`); `None` when not asked.
    pub tokens: Option<Result<Value, String>>,
}

/// The digest's short `tokens` entry from a gauge: its numbers, and a plain `note` when live
/// tokens are at or above the soft ceiling. Pure.
pub fn tokens_entry(gauge: &Value) -> Value {
    let mut out = json!({
        "live": gauge["live"],
        "soft_ceiling": gauge["soft_ceiling"],
        "hard_cap": gauge["hard_cap"],
        "as_of": gauge["as_of"],
    });
    if let (Some(live), Some(ceiling)) = (gauge["live"].as_u64(), gauge["soft_ceiling"].as_u64())
        && live >= ceiling
    {
        out["note"] = json!(format!(
            "{live} live Cloudflare tokens, at or above the soft ceiling of {ceiling}: new agent runs wait"
        ));
    }
    out
}

/// The digest from what the sources answered. A source that failed is named under `unavailable`
/// and the rest is still answered. Kept under [`DIGEST_BYTES`]: when the runs would not fit, their
/// steps go first, then the last runs, counted in `runs_omitted`.
pub fn build(sources: &Sources, since: Option<i64>) -> Value {
    let mut unavailable: Vec<Value> = Vec::new();
    let mut miss = |source: &str, why: &str| {
        unavailable.push(json!({ "source": source, "why": cut(why, 160) }));
    };
    let runs_list = match &sources.runs {
        Ok(v) => runs(v, since),
        Err(why) => {
            miss("runs", why);
            Vec::new()
        }
    };
    let landings_v = match &sources.landings {
        Ok((live, recent)) => landings(live, recent, since),
        Err(why) => {
            miss("landings", why);
            Value::Null
        }
    };
    let mut waiting = Map::new();
    match &sources.approvals {
        Ok(v) => {
            waiting.insert("approvals".into(), waiting_counts(v));
        }
        Err(why) => miss("approvals", why),
    }
    match &sources.inbox {
        Ok(v) => {
            let (asks, passkeys) = questions(v);
            waiting.insert("questions".into(), json!(asks));
            waiting.insert("passkey_waits".into(), json!(passkeys));
        }
        Err(why) => miss("questions", why),
    }
    let release_v = match &sources.release {
        Ok(v) => release(v),
        Err(why) => {
            miss("ios_release", why);
            Value::Null
        }
    };
    let mut out = json!({
        "runs": runs_list,
        "landings": landings_v,
        "waiting_on_person": Value::Object(waiting),
        "ios_release": release_v,
        "unavailable": unavailable,
    });
    match &sources.tokens {
        Some(Ok(v)) => out["tokens"] = tokens_entry(v),
        Some(Err(why)) => {
            if let Some(list) = out["unavailable"].as_array_mut() {
                list.push(json!({ "source": "tokens", "why": cut(why, 160) }));
            }
        }
        None => {}
    }
    fit(&mut out);
    out
}

/// Shrinks the digest's runs until it is under [`DIGEST_BYTES`].
fn fit(out: &mut Value) {
    let size = |v: &Value| v.to_string().len();
    if size(out) <= DIGEST_BYTES {
        return;
    }
    if let Some(runs) = out["runs"].as_array_mut() {
        for r in runs.iter_mut() {
            if let Some(m) = r.as_object_mut() {
                m.remove("step");
            }
        }
    }
    let mut omitted = 0;
    while size(out) > DIGEST_BYTES {
        let Some(runs) = out["runs"].as_array_mut() else {
            break;
        };
        if runs.pop().is_none() {
            break;
        }
        omitted += 1;
    }
    if omitted > 0 {
        out["runs_omitted"] = json!(omitted);
    }
}

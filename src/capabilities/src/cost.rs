//! `cost_daily`: read-only daily cost buckets for the iOS app's Cost screen. It joins Cloudflare's
//! billable usage, the AI Gateway's cost per model family and run (`models_daily`), and the AI
//! agent runs' roles. Closed days are kept in the Directory's settings, one key per month, so the
//! `all` range reaches back past the gateway's 31-day window. The folding is pure; only
//! [`cost_daily`] reads or writes anything.

use std::collections::{BTreeMap, BTreeSet};

use pigeon_models::daily::{day_of, days_ending};
use serde_json::{Map, Value, json};
use worker::js_sys::Date;
use worker::{Error, Method, Result};

use crate::observability::{BILLING_PERMISSION, with_token, within};
use crate::{Context, dispatch};

/// The Cloudflare lines that are always answered, in order. `Other` follows when it cost something.
const LINES: [&str; 4] = ["CI Containers", "Workers", "D1", "R2"];
/// The model families that are always answered, in order. `Other` follows when it cost something.
const MODELS: [&str; 4] = ["Opus", "Sonnet", "Haiku", "OpenAI"];
/// The roles that are always answered, in order. `other` follows when it cost something.
const ROLES: [&str; 2] = ["planner", "executor"];
/// The most days one answer spans, so a bad stored day cannot make the answer huge.
const SPAN_CAP: usize = 4_000;
/// The key of the sorted list of months that have a stored rollup.
const INDEX_KEY: &str = "cost.daily.months";

/// The settings key of a month's rollup, such as `cost.daily.2026-03`.
fn month_key(month: &str) -> String {
    format!("cost.daily.{month}")
}

/// `x` rounded to a millionth.
fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// A number that may be sent as a JSON number or a string; 0 for anything else.
fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|x| x.is_finite())
        .unwrap_or(0.0)
}

/// The answer's Cloudflare line for a billing `ServiceName`.
pub fn line_of(service: &str) -> &'static str {
    if service.contains("Container") {
        "CI Containers"
    } else if service.contains("Workers") {
        "Workers"
    } else if service.contains("D1") {
        "D1"
    } else if service.contains("R2") {
        "R2"
    } else {
        "Other"
    }
}

/// Billing rows folded into days: each day's `{"cloudflare": {line: usd}, "granularity":
/// {line: "day"|"period"}}`.
#[derive(Debug, Default, PartialEq)]
pub struct Billing {
    /// The days that have any cost row, up to today.
    pub days: BTreeMap<String, Value>,
}

/// Billable usage rows folded into days up to `today`. Only `ServiceName`, `BilledCost` and the
/// charge period are read, so the billing account's name and id never get this far. A row of one
/// UTC day goes on that day; a longer one is spread evenly over its days up to today and marked
/// `period`. A row with no readable start is skipped.
pub fn billing_days(result: &Value, today: &str) -> Billing {
    let rows = result
        .as_array()
        .or_else(|| result["data"].as_array())
        .cloned()
        .unwrap_or_default();
    type Cell = (f64, bool);
    let mut by: BTreeMap<String, BTreeMap<&'static str, Cell>> = BTreeMap::new();
    for row in &rows {
        let Some(name) = row["ServiceName"].as_str() else {
            continue;
        };
        let usd = num(&row["BilledCost"]);
        let Some(start) = row["ChargePeriodStart"].as_str().and_then(day_of) else {
            continue;
        };
        let end = row["ChargePeriodEnd"].as_str();
        let end_day = end.and_then(day_of).unwrap_or(start);
        // An end at midnight is exclusive: the period stops the day before.
        let at_midnight = end
            .and_then(|e| e.get(11..))
            .is_some_and(|t| t.starts_with("00:00:00"));
        let last = if end_day > start && at_midnight {
            days_ending(end_day, 2).into_iter().next()
        } else {
            None
        }
        .unwrap_or_else(|| end_day.to_string());
        let last = if last.as_str() < start {
            start.to_string()
        } else {
            last
        };
        let span: Vec<String> = days_ending(&last, SPAN_CAP)
            .into_iter()
            .filter(|d| d.as_str() >= start)
            .collect();
        let period = span.len() > 1;
        let covered: Vec<&String> = span.iter().filter(|d| d.as_str() <= today).collect();
        if covered.is_empty() {
            continue;
        }
        let share = usd / covered.len() as f64;
        for day in covered {
            let cell = by
                .entry(day.clone())
                .or_default()
                .entry(line_of(name))
                .or_insert((0.0, false));
            cell.0 += share;
            cell.1 |= period;
        }
    }
    let days = by
        .into_iter()
        .map(|(day, lines)| {
            let mut cloudflare = Map::new();
            let mut granularity = Map::new();
            for (line, (usd, period)) in lines {
                cloudflare.insert(line.to_string(), json!(round6(usd)));
                let g = if period { "period" } else { "day" };
                granularity.insert(line.to_string(), json!(g));
            }
            (
                day,
                json!({ "cloudflare": cloudflare, "granularity": granularity }),
            )
        })
        .collect();
    Billing { days }
}

/// A run id's role from `ai_agents_status`' list (`{runs: [{run, role}]}`): planner or executor.
pub fn roles_of(agents: &Value) -> BTreeMap<String, String> {
    agents["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter_map(|r| {
                    Some((
                        r["run"].as_str()?.to_string(),
                        r["role"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The live per-day records: Cloudflare cost from `billing`, and the gateway's cost per family
/// and per role from `models` (the `models_daily` answer) joined with `roles`. A source that is
/// `None` leaves its keys out of every record, so it never overwrites a stored value. A run not
/// in `roles`, and cost with no run, is role `other`.
pub fn live_days(
    billing: Option<&Billing>,
    models: Option<&Value>,
    roles: Option<&BTreeMap<String, String>>,
    anthropic: Option<&Value>,
) -> BTreeMap<String, Value> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(b) = billing {
        for (day, rec) in &b.days {
            out.insert(day.clone(), merge_day(&Value::Null, rec, false));
        }
    }
    if let Some(models) = models {
        gateway_days(&mut out, models, roles);
    }
    // Anthropic's own figures, on the days it has them.
    for (day, d) in anthropic
        .and_then(|a| a["by_day"].as_object())
        .into_iter()
        .flatten()
    {
        let by_model: Map<String, Value> = d["by_model"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(m, e)| (m.clone(), json!(num(&e["cost_usd"]))))
            .collect();
        let rec = json!({ "anthropic": {
            "cost_usd": num(&d["cost_usd"]),
            "tokens": d["tokens"],
            "by_model": by_model,
        }});
        let old = out.get(day).cloned().unwrap_or(Value::Null);
        out.insert(day.clone(), merge_day(&old, &rec, false));
    }
    out
}

/// Lays the gateway's per-day records from `models` (the `models_daily` answer) over `out`.
fn gateway_days(
    out: &mut BTreeMap<String, Value>,
    models: &Value,
    roles: Option<&BTreeMap<String, String>>,
) {
    let days: Vec<String> = models["days"]
        .as_array()
        .map(|d| {
            d.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for day in days {
        let mut families = Map::new();
        for (name, by_day) in models["by_family"].as_object().into_iter().flatten() {
            let usd = num(&by_day[&day]);
            if usd != 0.0 {
                families.insert(name.clone(), json!(usd));
            }
        }
        let mut by_role: BTreeMap<&str, f64> = BTreeMap::new();
        if let Some(roles) = roles {
            for (run, by_day) in models["by_run"].as_object().into_iter().flatten() {
                let role = match roles.get(run).map(String::as_str) {
                    Some("planner") => "planner",
                    Some("executor") => "executor",
                    _ => "other",
                };
                *by_role.entry(role).or_default() += num(&by_day[&day]);
            }
            *by_role.entry("other").or_default() += num(&models["unattributed"][&day]);
        }
        // A day with no cost in the listing says nothing: the gateway's logs may be shorter
        // than what is stored, so an empty day never replaces a stored one.
        if families.is_empty() && by_role.values().all(|v| *v == 0.0) {
            continue;
        }
        let mut rec = Map::new();
        rec.insert("models".into(), Value::Object(families));
        let mut providers = Map::new();
        for (name, by_day) in models["by_provider"].as_object().into_iter().flatten() {
            let usd = num(&by_day[&day]);
            if usd != 0.0 {
                providers.insert(name.clone(), json!(usd));
            }
        }
        rec.insert("gateway_providers".into(), Value::Object(providers));
        let mut anth = Map::new();
        for (model, by_day) in models["by_provider_model"]["anthropic"]
            .as_object()
            .into_iter()
            .flatten()
        {
            let d = &by_day[&day];
            if d.is_object() {
                anth.insert(
                    model.clone(),
                    json!({
                        "cost_usd": num(&d["cost_usd"]),
                        "tokens_in": num(&d["tokens_in"]),
                        "tokens_out": num(&d["tokens_out"]),
                    }),
                );
            }
        }
        rec.insert("gateway_anthropic".into(), Value::Object(anth));
        if roles.is_some() {
            let r: Map<String, Value> = by_role
                .into_iter()
                .filter(|(_, v)| *v != 0.0)
                .map(|(k, v)| (k.to_string(), json!(round6(v))))
                .collect();
            rec.insert("roles".into(), Value::Object(r));
        }
        let old = out.get(&day).cloned().unwrap_or(Value::Null);
        out.insert(day, merge_day(&old, &Value::Object(rec), false));
    }
}

/// A stored or live day `old` with the keys of `new` laid over it. The gateway's keys (`models`,
/// `roles`, `gateway_providers`, `gateway_anthropic`) of `old` stay when `truncated` says `new`
/// came from a cut-short log listing. Anthropic's own `anthropic` always overwrites.
pub fn merge_day(old: &Value, new: &Value, truncated: bool) -> Value {
    let mut out = old.as_object().cloned().unwrap_or_default();
    for (key, value) in new.as_object().into_iter().flatten() {
        let gateway = matches!(
            key.as_str(),
            "models" | "roles" | "gateway_providers" | "gateway_anthropic"
        );
        if gateway && truncated && out.contains_key(key) {
            continue;
        }
        out.insert(key.clone(), value.clone());
    }
    Value::Object(out)
}

/// A stored month (`{day: record}`, or null for none) with the `fresh` days laid over it by
/// [`merge_day`]. Days of the month that `fresh` lacks stay as they were.
pub fn merge_month(existing: &Value, fresh: &BTreeMap<String, Value>, truncated: bool) -> Value {
    let mut out = existing.as_object().cloned().unwrap_or_default();
    for (day, rec) in fresh {
        let merged = merge_day(
            &out.get(day).cloned().unwrap_or(Value::Null),
            rec,
            truncated,
        );
        out.insert(day.clone(), merged);
    }
    Value::Object(out)
}

/// The closed days (before `today`) of `live`, grouped by their `YYYY-MM` month: what to store.
pub fn closed_by_month(
    live: &BTreeMap<String, Value>,
    today: &str,
) -> BTreeMap<String, BTreeMap<String, Value>> {
    let mut months: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for (day, rec) in live {
        if day.as_str() < today && day_of(day).is_some() {
            months
                .entry(day[..7].to_string())
                .or_default()
                .insert(day.clone(), rec.clone());
        }
    }
    months
}

/// A name as one of `known`, or `other` when it is none of them.
fn bucket(name: &str, known: &[&str], other: &str) -> String {
    if known.contains(&name) {
        name.to_string()
    } else {
        other.to_string()
    }
}

/// The cost of each bucket of `group` on each of `days`, from the merged day records.
fn series(
    days: &[String],
    merged: &BTreeMap<String, Value>,
    group: &str,
    known: &[&str],
    other: &str,
) -> BTreeMap<String, BTreeMap<String, f64>> {
    let mut out: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for day in days {
        for (name, usd) in merged
            .get(day)
            .and_then(|r| r[group].as_object())
            .into_iter()
            .flatten()
        {
            *out.entry(bucket(name, known, other))
                .or_default()
                .entry(day.clone())
                .or_default() += num(usd);
        }
    }
    out
}

/// One answer entry: `name`, its total and its non-zero days, all rounded to a millionth.
fn entry(name: &str, by_day: Option<&BTreeMap<String, f64>>) -> (Value, f64) {
    let days: BTreeMap<String, f64> = by_day
        .into_iter()
        .flatten()
        .map(|(d, v)| (d.clone(), round6(*v)))
        .filter(|(_, v)| *v != 0.0)
        .collect();
    let total = round6(days.values().sum());
    (
        json!({ "name": name, "total": total, "by_day": days }),
        total,
    )
}

/// The entries for `fixed` names (always) and `other` (only when above 0), and their sum.
fn entries(
    fixed: &[&str],
    other: &str,
    data: &BTreeMap<String, BTreeMap<String, f64>>,
) -> (Vec<Value>, f64) {
    let mut out = Vec::new();
    let mut sum = 0.0;
    for name in fixed {
        let (e, t) = entry(name, data.get(*name));
        out.push(e);
        sum += t;
    }
    let (e, t) = entry(other, data.get(other));
    if t > 0.0 {
        out.push(e);
        sum += t;
    }
    (out, round6(sum))
}

/// The answer for `range` (`today`, `7d` or `all`; anything else is `7d`) on `today`: the
/// `stored` months (month to `{day: record}`) with the `live` days laid over them, the live
/// ones winning (see [`merge_day`] for a `truncated` listing). Pure.
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    range: &str,
    today: &str,
    stored: &BTreeMap<String, Value>,
    live: &BTreeMap<String, Value>,
    truncated: bool,
    anthropic_unavailable: bool,
    mut notes: Vec<String>,
    unavailable: Vec<Value>,
) -> Value {
    let mut merged: BTreeMap<String, Value> = BTreeMap::new();
    for month in stored.values() {
        for (day, rec) in month.as_object().into_iter().flatten() {
            if day_of(day) == Some(day.as_str()) {
                merged.insert(day.clone(), rec.clone());
            }
        }
    }
    let stored_from = merged.keys().next().cloned();
    for (day, rec) in live {
        let old = merged.get(day).cloned().unwrap_or(Value::Null);
        merged.insert(day.clone(), merge_day(&old, rec, truncated));
    }
    let week = days_ending(today, 7);
    let days = match range {
        "today" => days_ending(today, 1),
        "all" => {
            let first = match (&stored_from, week.first()) {
                (Some(s), Some(w)) if s < w => s.clone(),
                (_, Some(w)) => w.clone(),
                _ => today.to_string(),
            };
            let n = days_ending(today, SPAN_CAP)
                .iter()
                .filter(|d| **d >= first)
                .count();
            days_ending(today, n.max(7))
        }
        _ => week,
    };

    let cloudflare = series(&days, &merged, "cloudflare", &LINES, "Other");
    let (mut lines, cf_total) = entries(&LINES, "Other", &cloudflare);
    let mut periodic = Vec::new();
    for line in &mut lines {
        let name = line["name"].as_str().unwrap_or_default().to_string();
        let period = days.iter().any(|d| {
            merged
                .get(d)
                .and_then(|r| r["granularity"].as_object())
                .is_some_and(|g| {
                    g.iter()
                        .any(|(k, v)| v == "period" && bucket(k, &LINES, "Other") == name)
                })
        });
        line["kind"] = json!("billed");
        line["granularity"] = json!(if period { "period" } else { "day" });
        if period {
            periodic.push(name);
        }
    }
    if !periodic.is_empty() {
        notes.push(format!(
            "Cloudflare bills {} per billing period, so each day shows an even share of the \
             period.",
            periodic.join(", ")
        ));
    }
    let models = series(&days, &merged, "models", &MODELS, "Other");
    let (by_model, ai_total) = entries(&MODELS, "Other", &models);
    let roles = series(&days, &merged, "roles", &ROLES, "other");
    let (by_role, _) = entries(&ROLES, "other", &roles);

    // Gateway cost per provider, by the answer's names.
    let mut providers: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut gateway_anthropic: BTreeMap<String, f64> = BTreeMap::new();
    for day in &days {
        let rec = merged.get(day);
        for (slug, usd) in rec
            .and_then(|r| r["gateway_providers"].as_object())
            .into_iter()
            .flatten()
        {
            let name = match slug.as_str() {
                "anthropic" => "Anthropic",
                "openai" => "OpenAI",
                _ => "Other",
            };
            *providers
                .entry(name.to_string())
                .or_default()
                .entry(day.clone())
                .or_default() += num(usd);
            if slug == "anthropic" {
                *gateway_anthropic.entry(day.clone()).or_default() += num(usd);
            }
        }
    }
    let (by_provider, _) = entries(&["Anthropic", "OpenAI"], "Other", &providers);

    // Anthropic's own figures replace the gateway's Anthropic cost on the days it reported.
    let mut a_days = serde_json::Map::new();
    let mut a_models: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut gateway_models = Map::new();
    let mut swap = 0.0;
    for day in &days {
        let Some(rec) = merged.get(day) else { continue };
        for (model, e) in rec["gateway_anthropic"].as_object().into_iter().flatten() {
            let slot = gateway_models
                .entry(model.clone())
                .or_insert_with(|| json!({}));
            slot[day.as_str()] = json!({
                "cost_usd": num(&e["cost_usd"]),
                "tokens_in": num(&e["tokens_in"]),
                "tokens_out": num(&e["tokens_out"]),
                "requests": 1,
            });
        }
        let a = &rec["anthropic"];
        if !a.is_object() {
            continue;
        }
        swap += num(&a["cost_usd"]) - gateway_anthropic.get(day).copied().unwrap_or(0.0);
        a_days.insert(
            day.clone(),
            json!({ "cost_usd": num(&a["cost_usd"]), "tokens": a["tokens"] }),
        );
        for (m, usd) in a["by_model"].as_object().into_iter().flatten() {
            *a_models
                .entry(m.clone())
                .or_default()
                .entry(day.clone())
                .or_default() += num(usd);
        }
    }
    let anthropic = if a_days.is_empty() && anthropic_unavailable {
        Value::Null
    } else {
        let mut names: Vec<Value> = Vec::new();
        for (model, by_day) in &a_models {
            names.push(entry(model, Some(by_day)).0);
        }
        let a_total_all: f64 = a_days.values().map(|d| num(&d["cost_usd"])).sum();
        let known = json!({ "by_day": a_days });
        let recon = pigeon_models::billing::reconcile(
            &Value::Object(gateway_models),
            truncated,
            &known,
            &days,
            today,
        );
        for n in recon["notes"].as_array().into_iter().flatten() {
            if let Some(n) = n.as_str() {
                notes.push(n.to_string());
            }
        }
        json!({
            "total": round6(a_total_all),
            "by_model": names,
            "reconciliation": recon,
            "source": "Anthropic Admin API",
        })
    };
    json!({
        "range": match range { "today" | "all" => range, _ => "7d" },
        "days": days,
        "total": round6(cf_total + ai_total + swap),
        "cloudflare": { "total": cf_total, "lines": lines },
        "ai_gateway": { "total": ai_total, "by_model": by_model, "by_role": by_role, "by_provider": by_provider },
        "anthropic": anthropic,
        "notes": notes,
        "unavailable": unavailable,
        "truncated": truncated,
        "stored_from": stored_from,
    })
}

/// The settings value at `key` as JSON; null when none is kept or it does not parse.
async fn read_setting(cx: &Context<'_>, key: &str) -> Result<Value> {
    let kept = crate::users::directory(cx.env, "setting.get", json!({ "key": key })).await?;
    Ok(kept
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null))
}

/// The months that have a stored rollup, sorted.
async fn read_index(cx: &Context<'_>) -> Result<Vec<String>> {
    let index = read_setting(cx, INDEX_KEY).await?;
    let mut months: Vec<String> = index
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    months.sort();
    Ok(months)
}

/// The stored rollups of `wanted` months, month to its `{day: record}`.
async fn read_months(cx: &Context<'_>, wanted: &[String]) -> Result<BTreeMap<String, Value>> {
    let reads = wanted.iter().map(|m| async move {
        let v = read_setting(cx, &month_key(m)).await?;
        Ok::<_, Error>((m.clone(), v))
    });
    futures_util::future::join_all(reads)
        .await
        .into_iter()
        .collect()
}

/// Stores `fresh` months (see [`closed_by_month`]) and adds them to the index.
async fn store(
    cx: &Context<'_>,
    fresh: &BTreeMap<String, BTreeMap<String, Value>>,
    truncated: bool,
) -> Result<()> {
    if fresh.is_empty() {
        return Ok(());
    }
    for (month, days) in fresh {
        let key = month_key(month);
        let merged = merge_month(&read_setting(cx, &key).await?, days, truncated);
        let value = merged.to_string();
        crate::users::directory(cx.env, "setting.put", json!({ "key": key, "value": value }))
            .await?;
    }
    let before = read_index(cx).await?;
    let mut all: BTreeSet<String> = before.iter().cloned().collect();
    all.extend(fresh.keys().cloned());
    let after: Vec<String> = all.into_iter().collect();
    if after != before {
        let value = json!(after).to_string();
        crate::users::directory(
            cx.env,
            "setting.put",
            json!({ "key": INDEX_KEY, "value": value }),
        )
        .await?;
    }
    Ok(())
}

/// A source that failed, for `unavailable`.
fn missing(source: &str, why: &str) -> Value {
    json!({ "source": source, "why": why })
}

/// `cost_daily`: the answer for `args.range` (`today`, `7d` or `all`; `7d` when absent). The
/// sources run together, each within its limit; one that fails goes to `unavailable` and the
/// rest still answers. Closed days it computed are stored; a failure to store is logged and
/// noted, never fatal.
pub async fn cost_daily(cx: &Context<'_>, args: &Value) -> Result<Value> {
    let range = args["range"].as_str().unwrap_or("7d");
    if !matches!(range, "today" | "7d" | "all") {
        return Err(Error::RustError("`range` is today, 7d or all".into()));
    }
    let now = String::from(Date::new_0().to_iso_string());
    let today = now.get(..10).unwrap_or_default().to_string();
    let window = match range {
        "today" => 1,
        "7d" => 7,
        _ => 31,
    };
    let id = cx.account.id.as_str();
    let billing_work = with_token(cx, BILLING_PERMISSION, |t| async move {
        let path = format!("/accounts/{id}/billable-usage");
        pigeon_core::cloudflare::call_fresh(&t, Method::Get, &path, None).await
    });
    let models_args = json!({ "days": window });
    let agents_args = json!({ "limit": 100 });
    let models_work = within(dispatch(cx, "models_daily", &models_args));
    let anthropic_args = json!({ "provider": "anthropic", "days": window });
    let anthropic_work = within(dispatch(cx, "models_billing", &anthropic_args));
    let agents_work = within(dispatch(cx, "ai_agents_status", &agents_args));
    let index_work = within(read_index(cx));
    let (billing_v, models_v, agents_v, index_v, anthropic_v) = futures_util::future::join5(
        billing_work,
        models_work,
        agents_work,
        index_work,
        anthropic_work,
    )
    .await;

    let mut unavailable = Vec::new();
    let hide = |e: Error| e.to_string().replace(id, "{account}");
    let billing = match billing_v {
        Ok(v) => Some(billing_days(&v, &today)),
        Err(why) => {
            unavailable.push(missing("billable usage", &why));
            None
        }
    };
    let models = match models_v {
        Ok(v) => Some(v),
        Err(e) => {
            unavailable.push(missing("AI Gateway", &hide(e)));
            None
        }
    };
    let roles = match agents_v {
        Ok(v) => Some(roles_of(&v)),
        Err(e) => {
            unavailable.push(missing("agent roles", &hide(e)));
            None
        }
    };
    let anthropic = match anthropic_v {
        Ok(v) => match v["unavailable"]
            .as_array()
            .and_then(|u| u.iter().find(|x| x["source"] == "cost_report"))
        {
            // Without the cost report there is no billed figure to put in place of the gateway's.
            Some(x) => {
                let why = x["why"].as_str().unwrap_or("the cost report failed");
                unavailable.push(missing("Anthropic", why));
                None
            }
            None => Some(v),
        },
        Err(e) => {
            unavailable.push(missing("Anthropic", &hide(e)));
            None
        }
    };
    let truncated = models.as_ref().is_some_and(|m| m["truncated"] == true);
    let live = live_days(
        billing.as_ref(),
        models.as_ref(),
        roles.as_ref(),
        anthropic.as_ref(),
    );

    // Which months to read: all of them for `all`; else the first, the window's and today's.
    let index = match index_v {
        Ok(i) => i,
        Err(e) => {
            unavailable.push(missing("stored rollups", &hide(e)));
            Vec::new()
        }
    };
    let wanted: Vec<String> = if range == "all" {
        index.clone()
    } else {
        let mut w: BTreeSet<String> = index.first().cloned().into_iter().collect();
        for d in days_ending(&today, window).iter().chain([&today]) {
            let month = d[..7].to_string();
            if index.contains(&month) {
                w.insert(month);
            }
        }
        w.into_iter().collect()
    };
    let stored = match within(read_months(cx, &wanted)).await {
        Ok(s) => s,
        Err(e) => {
            unavailable.push(missing("stored rollups", &hide(e)));
            BTreeMap::new()
        }
    };

    let fresh = closed_by_month(&live, &today);
    if let Err(e) = within(store(cx, &fresh, truncated)).await {
        worker::console_error!("cost_daily: storing the rollups: {e}");
        unavailable.push(missing("rollup store", &hide(e)));
    }
    Ok(assemble(
        range,
        &today,
        &stored,
        &live,
        truncated,
        anthropic.is_none(),
        Vec::new(),
        unavailable,
    ))
}

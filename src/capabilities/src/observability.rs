//! `observability_summary`: a read-only look at how the platform's Workers are doing and what
//! the platform costs, from Cloudflare's metrics, logs and billable usage, plus pigeon's own
//! model cost and deploys. The folding is plain functions with no Worker types, so the smoke
//! suite runs them on fixtures. See docs/plans/observability.md. It also answers
//! `containers_images` ([`images`]), the read-only look at container applications and images.

pub mod images;
pub mod instance;

use std::collections::BTreeMap;

use pigeon_core::lanes::Map;
use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{Policy, Scope, Scoped};
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::{Call, Capability, Context};

/// The metrics permission: GraphQL `workersInvocationsAdaptive`.
const ANALYTICS_PERMISSION: &str = "Account Analytics Read";
/// The only permission the logs query endpoint accepts.
const OBSERVABILITY_PERMISSION: &str = "Workers Observability Write";
/// The billable usage permission.
pub(crate) const BILLING_PERMISSION: &str = "Billing Read";
/// How long the token lives, in seconds.
const TOKEN_TTL: u64 = 300;
/// The longest a message is kept, in characters.
const MESSAGE_CHARS: usize = 200;
/// How long one source may take, in milliseconds: a mint, its call (with `call_fresh`'s waits
/// for a new token, 15 s at most) and the revoke. The sources run together, so the tool answers
/// in about this long even when an upstream hangs.
const SOURCE_LIMIT_MS: u64 = 25_000;
/// How many error messages are kept per Worker.
const TOP_ERRORS: usize = 5;
/// Workers list price, dollars per million requests, before included usage.
const PER_MILLION_REQUESTS: f64 = 0.30;
/// Workers list price, dollars per million CPU milliseconds, before included usage.
const PER_MILLION_CPU_MS: f64 = 0.02;

/// The GraphQL query for per-script invocations, and its variables, for `account` between the
/// ISO times `from` and `to`.
pub fn graphql_query(account: &str, from: &str, to: &str) -> Value {
    let query = "query($tag: string, $from: Time, $to: Time) { viewer { accounts(filter: \
        {accountTag: $tag}) { workersInvocationsAdaptive(limit: 10000, filter: \
        {datetime_geq: $from, datetime_leq: $to}) { sum { requests errors subrequests } \
        quantiles { cpuTimeP50 cpuTimeP99 wallTimeP50 wallTimeP99 } \
        dimensions { scriptName status datetimeHour } } } } }";
    json!({ "query": query, "variables": { "tag": account, "from": from, "to": to } })
}

/// What one script did in the window.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Script {
    pub requests: f64,
    pub errors: f64,
    pub by_status: BTreeMap<String, f64>,
    cpu_p50_weighted: f64,
    pub cpu_p99_us: f64,
    wall_p50_weighted: f64,
    pub wall_p99_us: f64,
}

impl Script {
    /// The error rate, 0 when there were no requests.
    pub fn error_rate(&self) -> f64 {
        if self.requests > 0.0 {
            self.errors / self.requests
        } else {
            0.0
        }
    }

    /// The script as JSON. Times are answered in milliseconds; Cloudflare gives microseconds.
    pub fn public(&self) -> Value {
        let p50 = |w: f64| {
            if self.requests > 0.0 {
                w / self.requests / 1000.0
            } else {
                0.0
            }
        };
        json!({
            "requests": self.requests, "errors": self.errors, "error_rate": self.error_rate(),
            "by_status": self.by_status,
            "cpu_ms_p50": p50(self.cpu_p50_weighted), "cpu_ms_p99": self.cpu_p99_us / 1000.0,
            "wall_ms_p50": p50(self.wall_p50_weighted), "wall_ms_p99": self.wall_p99_us / 1000.0,
        })
    }

    /// The CPU milliseconds the window used, roughly: the median times the requests.
    pub fn cpu_ms_estimate(&self) -> f64 {
        self.cpu_p50_weighted / 1000.0
    }
}

/// Folds the GraphQL answer's rows (one per script and status) into one entry per script.
/// The median is weighted by requests and the p99 is the largest, since quantiles cannot be
/// merged exactly.
pub fn fold_invocations(answer: &Value) -> BTreeMap<String, Script> {
    let rows = answer["data"]["viewer"]["accounts"][0]["workersInvocationsAdaptive"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out: BTreeMap<String, Script> = BTreeMap::new();
    for row in &rows {
        let Some(name) = row["dimensions"]["scriptName"].as_str() else {
            continue;
        };
        let n = |v: &Value| v.as_f64().unwrap_or(0.0);
        let s = out.entry(name.to_string()).or_default();
        let requests = n(&row["sum"]["requests"]);
        s.requests += requests;
        s.errors += n(&row["sum"]["errors"]);
        let status = row["dimensions"]["status"].as_str().unwrap_or("unknown");
        *s.by_status.entry(status.to_string()).or_default() += requests;
        let q = &row["quantiles"];
        s.cpu_p50_weighted += n(&q["cpuTimeP50"]) * requests;
        s.wall_p50_weighted += n(&q["wallTimeP50"]) * requests;
        s.cpu_p99_us = s.cpu_p99_us.max(n(&q["cpuTimeP99"]));
        s.wall_p99_us = s.wall_p99_us.max(n(&q["wallTimeP99"]));
    }
    out
}

/// The fewest requests in the two hours after a deploy for a spike to be judged.
const SPIKE_MIN_REQUESTS: f64 = 20.0;
/// The fewest errors in the two hours after a deploy that can count as a spike.
const SPIKE_MIN_ERRORS: f64 = 3.0;
/// How many times the window's error rate the rate after a deploy must exceed.
const SPIKE_FACTOR: f64 = 2.0;
/// One hour in milliseconds.
const HOUR_MS: i64 = 3_600_000;

/// Milliseconds since the epoch for exactly `YYYY-MM-DDTHH:MM:SSZ`, or None for any other text.
fn parse_utc(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> {
        let part = text.get(a..z)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // Days from civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + s * 1000)
}

/// Per script, per hour start (ms, UTC): (requests, errors). Rows without a readable
/// `datetimeHour` are skipped.
pub fn fold_hourly(answer: &Value) -> BTreeMap<String, BTreeMap<i64, (f64, f64)>> {
    let rows = answer["data"]["viewer"]["accounts"][0]["workersInvocationsAdaptive"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out: BTreeMap<String, BTreeMap<i64, (f64, f64)>> = BTreeMap::new();
    for row in &rows {
        let (Some(name), Some(hour)) = (
            row["dimensions"]["scriptName"].as_str(),
            row["dimensions"]["datetimeHour"]
                .as_str()
                .and_then(parse_utc),
        ) else {
            continue;
        };
        let e = out
            .entry(name.to_string())
            .or_default()
            .entry(hour)
            .or_default();
        e.0 += row["sum"]["requests"].as_f64().unwrap_or(0.0);
        e.1 += row["sum"]["errors"].as_f64().unwrap_or(0.0);
    }
    out
}

/// Deploys that succeeded and were followed by an error spike: in the hour bucket holding
/// `finished_at` plus the next one, at least 20 requests, at least 3 errors and an error rate
/// above twice `window_rate`.
pub fn spike_after_deploy(
    hourly: &BTreeMap<i64, (f64, f64)>,
    deploys: &[Value],
    window_rate: f64,
) -> Vec<Value> {
    let ok = crate::land::lanes::DeployState::Succeeded.name();
    let mut out = Vec::new();
    for d in deploys {
        let Some(finished) = d["finished_at"].as_i64() else {
            continue;
        };
        if d["state"] != ok {
            continue;
        }
        let start = finished.div_euclid(HOUR_MS) * HOUR_MS;
        let (mut requests, mut errors) = (0.0, 0.0);
        for key in [start, start + HOUR_MS] {
            if let Some((r, e)) = hourly.get(&key) {
                requests += r;
                errors += e;
            }
        }
        let rate = if requests > 0.0 {
            errors / requests
        } else {
            0.0
        };
        if requests >= SPIKE_MIN_REQUESTS
            && errors >= SPIKE_MIN_ERRORS
            && rate > SPIKE_FACTOR * window_rate
        {
            out.push(json!({
                "deploy": d["id"], "landing": d["landing"], "commit": d["commit"],
                "finished_at": finished, "requests_after": requests,
                "errors_after": errors, "rate_after": rate, "window_rate": window_rate,
            }));
        }
    }
    out
}

/// The logs query body: error lines counted by Worker and message, between `from_ms` and
/// `to_ms`. `id` is any ad-hoc query id.
pub fn telemetry_query(id: &str, from_ms: i64, to_ms: i64) -> Value {
    json!({
        "queryId": id,
        "timeframe": { "from": from_ms, "to": to_ms },
        "view": "calculations",
        "limit": 50,
        "parameters": {
            "calculations": [{ "operator": "count" }],
            "filters": [{ "key": "$metadata.level", "operation": "eq", "type": "string", "value": "error" }],
            "groupBys": [
                { "type": "string", "value": "$metadata.service" },
                { "type": "string", "value": "$metadata.message" }
            ],
        },
    })
}

/// The logs query body that counts every event by Worker, any level: which Workers' logs hold
/// anything in the window.
pub fn telemetry_services_query(id: &str, from_ms: i64, to_ms: i64) -> Value {
    json!({
        "queryId": id,
        "timeframe": { "from": from_ms, "to": to_ms },
        "view": "calculations",
        "limit": 100,
        "parameters": {
            "calculations": [{ "operator": "count" }],
            "groupBys": [{ "type": "string", "value": "$metadata.service" }],
        },
    })
}

/// The Workers that [`telemetry_services_query`]'s result has any events for.
pub fn fold_services(result: &Value) -> std::collections::BTreeSet<String> {
    result["calculations"][0]["aggregates"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|row| row["groups"].as_array().into_iter().flatten())
        .filter(|g| g["key"] == "$metadata.service")
        .filter_map(|g| g["value"].as_str().map(str::to_string))
        .collect()
}

/// Folds the logs query's result into the top five messages per Worker, each redacted
/// ([`redact`]) and cut to 200 characters. Groups are read as `{key, value}` pairs next to a `count`.
pub fn fold_errors(result: &Value) -> BTreeMap<String, Vec<Value>> {
    fold_errors_top(result, TOP_ERRORS)
}

/// The most error messages `observability_errors` keeps per Worker.
const ERRORS_MAX_LIMIT: usize = 20;
/// The longest Worker name `observability_errors` accepts, in bytes.
const ERRORS_MAX_WORKER: usize = 100;
/// What `observability_errors` says about its missing `run` filter.
const NO_RUN_FILTER: &str = "no run filter exists: log lines carry no run tag, so errors cannot \
                             be narrowed to one AI agent run";

/// The checked arguments of `observability_errors`: the Worker script to keep, the window in
/// hours and the most messages per Worker.
#[derive(Debug, PartialEq)]
pub struct ErrorsArgs {
    /// One script name, or `None` for every Worker.
    pub worker: Option<String>,
    /// The window, 1 to 168 hours.
    pub hours: u64,
    /// The most messages kept per Worker, 1 to 20.
    pub limit: usize,
}

/// Checks `observability_errors`'s arguments. `hours` is capped like the summary's (1 to 168,
/// 24 by default) and `limit` is 1 to 20 (5 by default). A value of the wrong type is refused.
pub fn errors_args(args: &Value) -> std::result::Result<ErrorsArgs, String> {
    let worker = match &args["worker"] {
        Value::Null => None,
        Value::String(s) if !s.trim().is_empty() && s.len() <= ERRORS_MAX_WORKER => {
            Some(s.trim().to_string())
        }
        _ => {
            return Err(format!(
                "`worker` must be a script name of 1 to {ERRORS_MAX_WORKER} bytes"
            ));
        }
    };
    let whole = |key: &str, default: u64| -> std::result::Result<u64, String> {
        match &args[key] {
            Value::Null => Ok(default),
            v => v
                .as_u64()
                .ok_or_else(|| format!("`{key}` must be a whole number, 1 or more")),
        }
    };
    Ok(ErrorsArgs {
        worker,
        hours: whole("hours", 24)?.clamp(1, 168),
        limit: (whole("limit", TOP_ERRORS as u64)?.clamp(1, ERRORS_MAX_LIMIT as u64)) as usize,
    })
}

/// The answer of `observability_errors` from folded data. `scripts` is the metrics fold (counts
/// and `by_status`), `messages` the log fold, `known` the platform's script names. A `worker`
/// that is not known answers no Workers and lists the known names.
pub fn errors_answer(
    scripts: &BTreeMap<String, Script>,
    messages: &BTreeMap<String, Vec<Value>>,
    known: &[String],
    ask: &ErrorsArgs,
) -> Value {
    let mut names: std::collections::BTreeSet<String> = known.iter().cloned().collect();
    names.extend(scripts.keys().cloned());
    names.extend(messages.keys().cloned());
    let listed: Vec<String> = names.iter().cloned().collect();
    let chosen: Vec<&String> = match &ask.worker {
        Some(w) => names.get(w).into_iter().collect(),
        None => names.iter().collect(),
    };
    let workers: Vec<Value> = chosen
        .into_iter()
        .map(|name| {
            let s = scripts.get(name).cloned().unwrap_or_default();
            let top: Vec<Value> = messages
                .get(name)
                .map(|m| m.iter().take(ask.limit).cloned().collect())
                .unwrap_or_default();
            json!({
                "script": name, "requests": s.requests, "errors": s.errors,
                "by_status": s.by_status, "top_errors": top,
            })
        })
        .collect();
    let mut out = json!({
        "window_hours": ask.hours,
        "workers": workers,
        "run_filter": NO_RUN_FILTER,
    });
    if ask.worker.is_some() && workers_empty(&out) {
        out["known_workers"] = json!(listed);
        out["note"] = json!("no Worker has that script name; known names are listed");
    }
    out
}

/// Whether `answer` lists no Workers.
fn workers_empty(answer: &Value) -> bool {
    answer["workers"].as_array().is_none_or(Vec::is_empty)
}

/// [`fold_errors`] keeping `keep` messages per Worker.
pub fn fold_errors_top(result: &Value, keep: usize) -> BTreeMap<String, Vec<Value>> {
    let rows = result["calculations"][0]["aggregates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out: BTreeMap<String, Vec<(f64, String)>> = BTreeMap::new();
    for row in &rows {
        let mut service = None;
        let mut message = String::new();
        for g in row["groups"].as_array().into_iter().flatten() {
            let value = g["value"].as_str().unwrap_or_default();
            match g["key"].as_str() {
                Some("$metadata.service") => service = Some(value.to_string()),
                Some("$metadata.message") => message = value.to_string(),
                _ => {}
            }
        }
        if let Some(service) = service {
            let count = row["count"].as_f64().unwrap_or(0.0);
            out.entry(service).or_default().push((count, message));
        }
    }
    out.into_iter()
        .map(|(service, mut v)| {
            v.sort_by(|a, b| b.0.total_cmp(&a.0));
            let top = v
                .into_iter()
                .take(keep)
                .map(|(count, m)| json!({ "count": count, "message": cut(&redact(&m)) }))
                .collect();
            (service, top)
        })
        .collect()
}

/// The characters that can be part of a URL, an email or a token: what [`redact`] looks at.
fn wordy(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._%+-/=:@?&~#".contains(c)
}

/// Whether `piece` looks like a secret: 20 characters or more of letters, digits, `_`, `-` and
/// `.`, with both letters and digits. Tokens, keys, JWTs, hex and UUID ids all match; words and
/// version numbers do not.
fn secret_like(piece: &str) -> bool {
    piece.len() >= 20
        && piece
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
        && piece.chars().any(|c| c.is_ascii_digit())
        && piece.chars().any(|c| c.is_ascii_alphabetic())
}

/// One run of [`wordy`] characters, redacted: a URL query becomes `?[query]`, an email
/// `[email]`, and each piece between `/`, `:`, `=`, `&`, `?`, `#`, `@`, `+`, `%` or `~` that is
/// [`secret_like`] becomes `[redacted]`.
fn redact_run(run: &str) -> String {
    let (head, query) = match run.find('?') {
        Some(i) if run[i..].len() > 1 => (&run[..i], true),
        _ => (run, false),
    };
    let at = head.find('@');
    if at.is_some_and(|i| i > 0 && head[i + 1..].contains('.')) && !head.contains("://") {
        return "[email]".to_string();
    }
    let mut out = String::new();
    let mut piece = String::new();
    let flush = |piece: &mut String, out: &mut String| {
        out.push_str(if secret_like(piece) {
            "[redacted]"
        } else {
            piece
        });
        piece.clear();
    };
    for c in head.chars() {
        if "/:=&?#@+%~".contains(c) {
            flush(&mut piece, &mut out);
            out.push(c);
        } else {
            piece.push(c);
        }
    }
    flush(&mut piece, &mut out);
    if query {
        out.push_str("?[query]");
    }
    out
}

/// `text` with what could be a secret or a person taken out: URL queries (one-use codes ride in
/// them), emails, and token-like strings. Error-level log lines are returned word for word
/// otherwise, and an upstream error or an uncaught exception can carry a response body.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if wordy(c) {
            run.push(c);
        } else {
            if !run.is_empty() {
                out.push_str(&redact_run(&run));
                run.clear();
            }
            out.push(c);
        }
    }
    if !run.is_empty() {
        out.push_str(&redact_run(&run));
    }
    out
}

/// `text` cut to 200 characters.
pub fn cut(text: &str) -> String {
    text.chars().take(MESSAGE_CHARS).collect()
}

/// Billable usage rows kept to the whitelisted FOCUS fields, summed per `ServiceName` and
/// `ConsumedUnit`, so requests and CPU time never add together. Every
/// other field, such as the billing account's name and id, is dropped.
pub fn fold_billing(result: &Value) -> Vec<Value> {
    let rows = result
        .as_array()
        .or_else(|| result["data"].as_array())
        .cloned()
        .unwrap_or_default();
    let mut by: BTreeMap<(String, String), Value> = BTreeMap::new();
    for row in &rows {
        let Some(name) = row["ServiceName"].as_str() else {
            continue;
        };
        // Live rows often carry an empty `ConsumedUnit` (Workers requests, Durable Objects
        // requests): fall back to `PricingUnit`, then `ChargeDescription`, so two meters of one
        // service never add together under an empty unit.
        let unit = ["ConsumedUnit", "PricingUnit", "ChargeDescription"]
            .iter()
            .filter_map(|k| row[*k].as_str())
            .find(|s| !s.trim().is_empty())
            .unwrap_or("unspecified");
        let entry = by
            .entry((name.to_string(), unit.to_string()))
            .or_insert_with(|| {
                let mut kept =
                    json!({ "ServiceName": name, "ConsumedQuantity": 0.0, "BilledCost": 0.0 });
                kept["ConsumedUnit"] = json!(unit);
                for k in [
                    "ServiceFamilyName",
                    "BillingCurrency",
                    "ChargePeriodStart",
                    "ChargePeriodEnd",
                ] {
                    if let Some(s) = row[k].as_str() {
                        kept[k] = json!(s);
                    }
                }
                kept
            });
        for k in ["ConsumedQuantity", "BilledCost"] {
            let add = number(&row[k]);
            entry[k] = json!(entry[k].as_f64().unwrap_or(0.0) + add);
        }
    }
    by.into_values().collect()
}

/// A number that Cloudflare may send as a JSON number or a string.
fn number(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

/// The Workers list-price estimate in dollars for `requests` and `cpu_ms`, before included usage.
pub fn workers_estimate(requests: f64, cpu_ms: f64) -> f64 {
    requests / 1e6 * PER_MILLION_REQUESTS + cpu_ms / 1e6 * PER_MILLION_CPU_MS
}

/// One cost line.
fn line(name: &str, kind: &str, source: &str, usd: Option<f64>, detail: Value) -> Value {
    json!({ "name": name, "kind": kind, "source": source, "usd": usd, "detail": detail })
}

/// A source that failed or is missing.
fn missing(source: &str, why: &str) -> Value {
    json!({ "source": source, "why": why })
}

/// A GraphQL call: its answer is `{data, errors}`, not the v4 envelope.
async fn graphql(token: &str, body: &Value) -> Result<Value> {
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    headers.set("Content-Type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    init.with_headers(headers);
    init.with_body(Some(body.to_string().into()));
    let request = Request::new_with_init("https://api.cloudflare.com/client/v4/graphql", &init)?;
    let mut response = Fetch::Request(request).send().await?;
    let status = response.status_code();
    let v: Value = serde_json::from_str(&response.text().await?)
        .map_err(|e| Error::RustError(format!("graphql: {status}: not JSON: {e}")))?;
    if v["errors"].as_array().is_some_and(|e| !e.is_empty()) {
        return Err(Error::RustError(format!(
            "graphql: {status}: {}",
            v["errors"]
        )));
    }
    Ok(v)
}

/// `ms` since the epoch as an ISO time.
fn iso(ms: f64) -> String {
    String::from(Date::new(&ms.into()).to_iso_string())
}

/// The `observability` capability: Worker metrics, errors, monitors, cost, token capacity and
/// container images, each read only but for the monitors.
pub struct Observability {
    /// The platform Workers (ci/workers.toml), whose scripts are reported.
    pub map: Map,
}

impl Capability for Observability {
    fn name(&self) -> &'static str {
        "observability"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![
            pigeon_core::versions::Component::began(
                "observability",
                pigeon_core::versions::Kind::Capability,
            )
            .entry(
                "1.1.0",
                "`containers_images` moved here from the `images` capability, which is gone; the \
                 tool's name, schema and answer are unchanged.",
            ),
        ]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            instance::tool(),
            images::tool(),
            Tool {
                name: "observability_monitors_plan",
                description: "Dry run of the notification policies pigeon wants (Workers \
                              observability alerts): those the account lacks (created) and \
                              existing `pigeon: ...` ones whose filters, enabled flag or \
                              delivery differ (updated; delivery is never removed). With \
                              `email`, every policy gets email delivery to that address. Never \
                              deletes; only `pigeon: ...` policies. With `billing`, also \
                              billing usage watches per product and limit. Asks the person's \
                              approval when something changes. With `check`, only reads: \
                              the shape of Cloudflare's alert types and policies, field names \
                              and counts, and asks nothing.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "check": { "type": "boolean", "description": "Read-only shape check; no approval is asked." },
                        "email": { "type": "string", "description": "One address to deliver every `pigeon: ...` policy to by email. Only what the person gives; it is shown once in the plan the person approves." },
                        "filters": { "type": "object", "additionalProperties": true, "description": "Filters for a policy, as {\"<alert type>\": {\"<key>\": [\"value\", ...]}}. Needed for a key Cloudflare requires and lists no IDs for; when it lists IDs, values must be among them." },
                        "billing": { "type": "object", "additionalProperties": true, "description": "Billing usage watches, as {\"<product ID>\": \"<limit>\"}, such as {\"worker_requests\": \"100\"}; one `pigeon: billing <product>` policy each (billing_usage_alert). The limit is in Cloudflare's unit for the product. Without it no billing watch is planned. Run `check` to see the product IDs Cloudflare lists." }
                    }
                }),
            },
            Tool {
                name: "observability_monitors_apply",
                description: "Creates or updates the policies of an approved monitors plan, \
                              once. Never deletes; only policies named `pigeon: ...`.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "plan": { "type": "string", "description": "The approval id from the plan." } },
                    "required": ["plan"]
                }),
            },
            Tool {
                name: "observability_summary",
                description: "Read-only. Per-Worker requests, errors, CPU and wall time, top error \
                          messages and deploys, and what the platform costs, each line tagged \
                          billed (Cloudflare's invoice data) or estimated (list price or a \
                          gateway's estimate). Sources that failed are listed.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "hours": { "type": "integer", "description": "Window, 1 to 168; 24 by default." } }
                }),
            },
            Tool {
                name: "observability_errors",
                description: "Read-only. Recent errors per Worker (script name): requests, error \
                              count, `by_status`, and the top error messages with counts, each \
                              redacted and cut short. An unknown `worker` answers no Workers and \
                              lists the known names. There is no run filter: log lines carry no \
                              run tag.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "worker": { "type": "string", "description": "One Worker's script name; every Worker by default." },
                        "hours": { "type": "integer", "description": "Window, 1 to 168; 24 by default." },
                        "limit": { "type": "integer", "description": "Most messages per Worker, 1 to 20; 5 by default." }
                    }
                }),
            },
            Tool {
                name: "tokens_status",
                description: "Pigeon's live Cloudflare API tokens (`live`), the configured mint \
                              ceiling (`soft_ceiling`, default 300), and Cloudflare's hard cap \
                              (`hard_cap`, 500). Includes the account-wide count \
                              (`account_total`) and its timestamp (`account_total_as_of_ms`), \
                              and `last_sweep_succeeded_at_ms`. An unavailable total or sweep \
                              timestamp is null. Only a person or their delegate can set \
                              `soft_ceiling` (10 to 450).",
                input_schema: json!({
                    "type": "object",
                    "properties": { "soft_ceiling": { "type": "integer", "description": "New soft ceiling, 10 to 450." } }
                }),
            },
            Tool {
                name: "cost_daily",
                description: "Read-only. Daily cost buckets in US dollars: Cloudflare lines and AI \
                              gateway cost by gateway, model and role; promotional grants and net cost. `range` is today, 7d or all.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "range": { "type": "string", "enum": ["today", "7d", "all"], "description": "7d by default." } }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                instance::METRICS => return instance::metrics(cx, args).await,
                images::TOOL => return images::containers_images(cx).await,
                "cost_daily" => return crate::cost::cost_daily(cx, args).await,
                "tokens_status" => return tokens_status(cx, args).await,
                "observability_errors" => return errors(self, cx, args).await,
                "observability_monitors_plan" if args["check"] == true => {
                    return monitors_check(cx).await;
                }
                "observability_monitors_plan" => return monitors_plan(self, cx, args).await,
                "observability_monitors_apply" => return monitors_apply(self, cx, args).await,
                "observability_summary" => {}
                _ => return Err(crate::unknown(tool)),
            }
            let hours = args["hours"].as_u64().unwrap_or(24).clamp(1, 168);
            summary(self, cx, hours).await
        })
    }
}

/// Permission to read the account's notification policies and available alert types.
const NOTIFICATIONS_READ_PERMISSION: &str = "Notifications Read";
/// Permission to create notification policies.
const NOTIFICATIONS_WRITE_PERMISSION: &str = "Notifications Write";
/// Prefix of every policy name pigeon creates; apply touches no other.
const MONITOR_PREFIX: &str = "pigeon: ";
/// Kind name of the approval for a monitors plan.
const MONITORS_KIND: &str = "observability_monitors";
/// How long a monitors plan waits for the person, like a linked plan: 30 minutes.
const MONITORS_PENDING_MS: i64 = 30 * 60 * 1000;
/// The alert types pigeon plans without arguments. Billing usage watches are planned only when
/// the person passes `billing` (see `validate_billing`).
const WANTED_TYPES: [&str; 2] = [
    "workers_observability_alert",
    "workers_observability_real_time_issue",
];

/// The alert types that `available` (the `available_alerts` result: groups of entries with a
/// `type`) offers, each with its `filter_options` keys.
pub fn available_types(available: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let groups: Vec<&Value> = match available {
        Value::Object(m) => m.values().collect(),
        Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    for entry in groups.into_iter().flat_map(|g| match g {
        Value::Array(a) => a.iter().collect::<Vec<_>>(),
        other => vec![other],
    }) {
        let Some(kind) = entry["type"].as_str() else {
            continue;
        };
        let keys = entry["filter_options"]
            .as_array()
            .map(|o| {
                o.iter()
                    .filter_map(|f| f["Key"].as_str().or_else(|| f["key"].as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        out.insert(kind.to_string(), keys);
    }
    out
}

/// The values `available` lists for each filter option, as `type -> key -> values`.
///
/// Sources: (1) Cloudflare's OpenAPI schema `aaa_filter_options` (github.com/cloudflare/api-schemas)
/// lists `AvailableValues` as objects `{ID, Description}`; (2) Cloudflare's MCP server
/// (github.com/cloudflare/mcp, `src/events/ans-bridge.ts`, `AvailableAlert`) reads them the same
/// way. So `ID` is read first; `Value`, `Name` and plain strings stay as fallbacks. A key with no
/// list has an empty vector.
pub fn available_filter_values(
    available: &Value,
) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    let groups: Vec<&Value> = match available {
        Value::Object(m) => m.values().collect(),
        Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    for entry in groups.into_iter().flat_map(|g| match g {
        Value::Array(a) => a.iter().collect::<Vec<_>>(),
        other => vec![other],
    }) {
        let Some(kind) = entry["type"].as_str() else {
            continue;
        };
        let mut keys = BTreeMap::new();
        for f in entry["filter_options"].as_array().into_iter().flatten() {
            let Some(key) = f["Key"].as_str().or_else(|| f["key"].as_str()) else {
                continue;
            };
            let list = f["AvailableValues"]
                .as_array()
                .or_else(|| f["available_values"].as_array());
            let values = list
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    v.as_str()
                        .or_else(|| v["ID"].as_str())
                        .or_else(|| v["id"].as_str())
                        .or_else(|| v["Value"].as_str())
                        .or_else(|| v["value"].as_str())
                        .or_else(|| v["Name"].as_str())
                        .or_else(|| v["name"].as_str())
                })
                .map(str::to_string)
                .collect();
            keys.insert(key.to_string(), values);
        }
        out.insert(kind.to_string(), keys);
    }
    out
}

/// One filter option of an alert type, as Cloudflare lists it in `filter_options`.
///
/// Source: Cloudflare's OpenAPI schema `aaa_filter_options` (github.com/cloudflare/api-schemas).
/// `Range` "1-n" reads as required (at least one value), "0-1" as optional.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterOption {
    /// The filter key, such as `status` or `services`.
    pub key: String,
    /// Cloudflare's `Range`, such as "1-n" or "0-1", when it gives one.
    pub range: Option<String>,
    /// Cloudflare's `ComparisonOperator`, such as "==", when it gives one.
    pub operator: Option<String>,
    /// The listed value IDs (`AvailableValues`); empty when the key lists none.
    pub ids: Vec<String>,
}

impl FilterOption {
    /// True when a policy needs at least one value for this key: `Range` starts with "1", or
    /// Cloudflare gives no `Range`.
    pub fn required(&self) -> bool {
        self.range
            .as_deref()
            .is_none_or(|r| r.trim().starts_with('1'))
    }
}

/// The filter options of each alert type in `available`, as `type -> options` in the order the
/// API lists them. Sources: `aaa_filter_options` in Cloudflare's OpenAPI schema, and the MCP
/// server's `AvailableAlert` (github.com/cloudflare/mcp, `src/events/ans-bridge.ts`), whose items
/// are `{Key, Range?, ComparisonOperator?, AvailableValues?: [{ID, Description}] | null}`.
pub fn filter_options(available: &Value) -> BTreeMap<String, Vec<FilterOption>> {
    let ids = available_filter_values(available);
    let mut out = BTreeMap::new();
    let groups: Vec<&Value> = match available {
        Value::Object(m) => m.values().collect(),
        Value::Array(a) => a.iter().collect(),
        _ => Vec::new(),
    };
    for entry in groups.into_iter().flat_map(|g| match g {
        Value::Array(a) => a.iter().collect::<Vec<_>>(),
        other => vec![other],
    }) {
        let Some(kind) = entry["type"].as_str() else {
            continue;
        };
        let mut options = Vec::new();
        for f in entry["filter_options"].as_array().into_iter().flatten() {
            let Some(key) = f["Key"].as_str().or_else(|| f["key"].as_str()) else {
                continue;
            };
            let text =
                |a: &str, b: &str| f[a].as_str().or_else(|| f[b].as_str()).map(str::to_string);
            options.push(FilterOption {
                key: key.to_string(),
                range: text("Range", "range"),
                operator: text("ComparisonOperator", "comparison_operator"),
                ids: ids
                    .get(kind)
                    .and_then(|m| m.get(key))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
        out.insert(kind.to_string(), options);
    }
    out
}

/// The `Range`, `ComparisonOperator` and listed IDs of each filter key of the watched types, as
/// `type -> key -> {range, comparison_operator, ids}`. Cloudflare's own metadata, no policy values.
pub fn filter_metadata(available: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for (kind, options) in filter_options(available) {
        if !(kind.starts_with("workers_observability") || kind.starts_with("billing")) {
            continue;
        }
        let keys: serde_json::Map<String, Value> = options
            .into_iter()
            .map(|o| {
                (
                    o.key,
                    json!({ "range": o.range, "comparison_operator": o.operator, "ids": o.ids }),
                )
            })
            .collect();
        out.insert(kind, Value::Object(keys));
    }
    Value::Object(out)
}

/// True for the filter keys that name Workers.
fn is_script_key(key: &str) -> bool {
    matches!(key, "services" | "worker_name" | "script_name")
}

/// The most values one filter key may be given by the person.
const MAX_FILTER_VALUES: usize = 20;

/// Checks the person's `filters` argument (`alert_type -> {key: [values]}`) against what
/// `available` lists. Only wanted types, only keys the type lists, values that are non-empty
/// strings (at most 20 per key), and, when the API lists IDs for a key, only those IDs. Returns
/// the filters as a map, or the first problem in plain words.
pub fn validate_filters(
    available: &Value,
    filters: &Value,
) -> std::result::Result<BTreeMap<String, BTreeMap<String, Vec<String>>>, String> {
    let mut out = BTreeMap::new();
    if filters.is_null() {
        return Ok(out);
    }
    let by_type = filters
        .as_object()
        .ok_or("`filters` must be an object: alert type -> {key: [values]}")?;
    let options = filter_options(available);
    for (kind, keys) in by_type {
        if !WANTED_TYPES.contains(&kind.as_str()) {
            return Err(format!(
                "`filters` names `{kind}`, which pigeon does not plan; the types are {}",
                WANTED_TYPES.join(", ")
            ));
        }
        let listed = options
            .get(kind)
            .ok_or_else(|| format!("`{kind}` is not in the account's available alerts"))?;
        let keys = keys
            .as_object()
            .ok_or_else(|| format!("`filters.{kind}` must be an object: key -> [values]"))?;
        let mut got = BTreeMap::new();
        for (key, values) in keys {
            let option = listed.iter().find(|o| &o.key == key).ok_or_else(|| {
                let have: Vec<&str> = listed.iter().map(|o| o.key.as_str()).collect();
                format!(
                    "`{kind}` has no filter key `{key}`; it lists [{}]",
                    have.join(", ")
                )
            })?;
            let list = values
                .as_array()
                .ok_or_else(|| format!("`filters.{kind}.{key}` must be a list of strings"))?;
            if list.is_empty() || list.len() > MAX_FILTER_VALUES {
                return Err(format!(
                    "`filters.{kind}.{key}` needs 1 to {MAX_FILTER_VALUES} values, not {}",
                    list.len()
                ));
            }
            let mut texts = Vec::new();
            for v in list {
                let t = v.as_str().filter(|t| !t.trim().is_empty()).ok_or_else(|| {
                    format!("`filters.{kind}.{key}` has a value that is not a non-empty string")
                })?;
                if !option.ids.is_empty() && !option.ids.iter().any(|i| i == t) {
                    return Err(format!(
                        "`{t}` is not a listed value of `{kind}` key `{key}`; allowed: [{}]",
                        option.ids.join(", ")
                    ));
                }
                texts.push(t.to_string());
            }
            got.insert(key.clone(), texts);
        }
        out.insert(kind.clone(), got);
    }
    Ok(out)
}

/// The alert type of a billing usage watch. It is not in `WANTED_TYPES`: it is planned only when
/// the person passes `billing`.
const BILLING_TYPE: &str = "billing_usage_alert";
/// The most products one `billing` argument may name.
const MAX_BILLING_PRODUCTS: usize = 10;

/// Checks the person's `billing` argument (`product ID -> limit string`) against what `available`
/// lists for `billing_usage_alert`. Null is no billing watches. Otherwise the type must be
/// offered, at most 10 products, each product matches `^[a-z0-9_]{1,64}$` and, when the API lists
/// IDs for `product`, is one of them; each limit is a plain non-negative decimal string of at most
/// 20 characters and, when the API lists IDs for `limit`, one of them. Returns `product -> limit`
/// or the first problem in plain words.
///
/// Sources: Cloudflare's OpenAPI schema `aaa_filters` (github.com/cloudflare/api-schemas): `product`
/// and `limit` are "Used for configuring billing_usage_alert", arrays of strings. The Terraform
/// provider's acceptance test (github.com/cloudflare/terraform-provider-cloudflare,
/// `internal/services/notification_policy/resource_test.go` and
/// `migration/v500/testdata/v5_billing_filters.tf`) creates such a policy with
/// `filters = { product = ["worker_requests"], limit = ["100"] }`, the only primary-source product
/// value. The unit of `limit` is Cloudflare's for the product and is not documented ("Thresholds
/// depend on the product chosen", developers.cloudflare.com/notifications/notification-available/).
pub fn validate_billing(
    available: &Value,
    billing: &Value,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    if billing.is_null() {
        return Ok(out);
    }
    let map = billing
        .as_object()
        .ok_or("`billing` must be an object: product ID -> limit string")?;
    let options = filter_options(available);
    let listed = options.get(BILLING_TYPE).ok_or_else(|| {
        format!("`{BILLING_TYPE}` is not in the account's available alerts, so `billing` cannot be planned")
    })?;
    if map.len() > MAX_BILLING_PRODUCTS {
        return Err(format!(
            "`billing` names {} products; at most {MAX_BILLING_PRODUCTS}",
            map.len()
        ));
    }
    let ids_of = |key: &str| -> Vec<String> {
        listed
            .iter()
            .find(|o| o.key == key)
            .map(|o| o.ids.clone())
            .unwrap_or_default()
    };
    let products = ids_of("product");
    let limits = ids_of("limit");
    for (product, limit) in map {
        let shaped = !product.is_empty()
            && product.len() <= 64
            && product
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !shaped {
            return Err(format!(
                "billing product `{product}` must match ^[a-z0-9_]{{1,64}}$"
            ));
        }
        if !products.is_empty() && !products.contains(product) {
            return Err(format!(
                "`{product}` is not a listed billing product; allowed: [{}]",
                products.join(", ")
            ));
        }
        let limit = limit
            .as_str()
            .ok_or_else(|| format!("the limit of billing product `{product}` must be a string"))?;
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let plain = limit.len() <= 20
            && match limit.split_once('.') {
                Some((a, b)) => digits(a) && digits(b),
                None => digits(limit),
            };
        if !plain {
            return Err(format!(
                "the limit `{limit}` of billing product `{product}` must be a plain non-negative \
                 number such as 100, at most 20 characters"
            ));
        }
        if !limits.is_empty() && !limits.iter().any(|i| i == limit) {
            return Err(format!(
                "`{limit}` is not a listed billing limit; allowed: [{}]",
                limits.join(", ")
            ));
        }
        out.insert(product.clone(), limit.to_string());
    }
    Ok(out)
}

/// The wanted policies of the checked `billing` map, one per product, as
/// `{name: "pigeon: billing <product>", alert_type, filters: {product: [p], limit: [l]},
/// enabled: true}`. Empty without `billing`.
pub fn billing_policies(billing: &BTreeMap<String, String>) -> Vec<Value> {
    billing
        .iter()
        .map(|(p, l)| {
            json!({
                "name": format!("{MONITOR_PREFIX}billing {p}"),
                "alert_type": BILLING_TYPE,
                "filters": { "product": [p], "limit": [l] },
                "enabled": true,
            })
        })
        .collect()
}

/// A note for the plan when `billing` is given and the API lists no `product` IDs: the product
/// values then come from the Terraform provider's test, not from this account.
pub fn billing_notes(available: &Value, billing: &BTreeMap<String, String>) -> Vec<String> {
    if billing.is_empty() {
        return Vec::new();
    }
    let listed = filter_options(available)
        .get(BILLING_TYPE)
        .is_some_and(|l| l.iter().any(|o| o.key == "product" && !o.ids.is_empty()));
    if listed {
        return Vec::new();
    }
    vec![
        "note: Cloudflare listed no billing product IDs for this account; the product value is \
         not confirmed here (`worker_requests` is the one in the Terraform provider's test)"
            .to_string(),
    ]
}

/// The wanted alert types that `available` does not offer.
pub fn absent_types(available: &Value) -> Vec<String> {
    let have = available_types(available);
    WANTED_TYPES
        .into_iter()
        .filter(|t| !have.contains_key(*t))
        .map(str::to_string)
        .collect()
}

/// What the plan found: the policies to create, and the ones that cannot be planned yet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MonitorsPlan {
    /// Items `{name, alert_type, filters, enabled}` for the policies to create.
    pub policies: Vec<Value>,
    /// Items `{name, alert_type, key, how}` for policies not planned because Cloudflare requires
    /// a filter key that lists no IDs and the person passed none.
    pub needs_filters: Vec<Value>,
}

/// The notification policies pigeon wants that `existing` (the policies list) lacks by name.
/// Types missing from `available` are skipped. Each item is `{name, alert_type, filters,
/// enabled}`. Uses no explicit filters; see [`plan_monitors_with`].
pub fn plan_monitors(available: &Value, existing: &Value, scripts: &[String]) -> Vec<Value> {
    plan_monitors_with(available, existing, scripts, &BTreeMap::new()).policies
}

/// [`plan_monitors`] with the person's explicit `filters` (`type -> key -> values`, already
/// checked by [`validate_filters`]). Per filter key the API lists: the explicit values win; else
/// a script key (`services`, `worker_name`, `script_name`) gets `scripts`; else a required key
/// (see [`FilterOption::required`]) with listed IDs gets all of them; else a required key with
/// no IDs puts the policy in `needs_filters`; an optional key is left out. Cloudflare refuses a
/// policy without the filters it requires (error 17103); the sources are `aaa_filter_options` in
/// its OpenAPI schema and the MCP server (github.com/cloudflare/mcp, `src/events/service.ts`).
pub fn plan_monitors_with(
    available: &Value,
    existing: &Value,
    scripts: &[String],
    explicit: &BTreeMap<String, BTreeMap<String, Vec<String>>>,
) -> MonitorsPlan {
    let options = filter_options(available);
    let names: Vec<&str> = existing
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["name"].as_str()).collect())
        .unwrap_or_default();
    let mut plan = MonitorsPlan::default();
    for (name, kind) in [
        ("pigeon: Workers errors", "workers_observability_alert"),
        (
            "pigeon: Workers real-time issues",
            "workers_observability_real_time_issue",
        ),
    ] {
        let Some(listed) = options.get(kind) else {
            continue;
        };
        if names.contains(&name) {
            continue;
        }
        let mut filters = serde_json::Map::new();
        let mut missing = Vec::new();
        for o in listed {
            if let Some(values) = explicit.get(kind).and_then(|m| m.get(&o.key)) {
                filters.insert(o.key.clone(), json!(values));
            } else if is_script_key(&o.key) {
                filters.insert(o.key.clone(), json!(scripts));
            } else if o.required() && !o.ids.is_empty() {
                filters.insert(o.key.clone(), json!(o.ids));
            } else if o.required() {
                missing.push(json!({
                    "name": name,
                    "alert_type": kind,
                    "key": o.key,
                    "how": format!(
                        "pass filters: {{\"{kind}\": {{\"{}\": [\"...\"]}}}}",
                        o.key
                    ),
                }));
            }
        }
        if missing.is_empty() {
            plan.policies.push(json!({
                "name": name, "alert_type": kind, "filters": Value::Object(filters),
                "enabled": true,
            }));
        } else {
            plan.needs_filters.extend(missing);
        }
    }
    plan
}

/// Checks the one delivery address the person gives and returns it trimmed. It must be 3 to 254
/// characters with exactly one `@`, a non-empty local part, a domain with a dot, and no
/// whitespace, control characters, commas or semicolons. The error never repeats the input.
pub fn validate_email(input: &str) -> Result<String, String> {
    let bad = |why: &str| Err(format!("`email` is not a valid address: {why}"));
    let email = input.trim();
    if email.len() < 3 || email.len() > 254 {
        return bad("it must be 3 to 254 characters");
    }
    if email
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == ',' || c == ';')
    {
        return bad("it holds a space, control character, comma or semicolon");
    }
    if email.matches('@').count() != 1 {
        return bad("it must hold exactly one `@`");
    }
    let (local, domain) = email.split_once('@').unwrap_or_default();
    if local.is_empty() {
        return bad("the part before `@` is empty");
    }
    if !domain.contains('.') {
        return bad("the domain needs a dot");
    }
    Ok(email.to_string())
}

/// The `mechanisms` object for one delivery address, `{"email": [{"id": "<address>"}]}`, or `{}`
/// with none. Source: Cloudflare's OpenAPI schema `aaa_mechanisms` (github.com/cloudflare/
/// api-schemas, openapi.json): "IDs for email type will be the email address", with the example
/// `{"email":[{"id":"test@example.com"}]}`.
pub fn email_mechanisms(email: Option<&str>) -> Value {
    match email {
        Some(address) => json!({ "email": [{ "id": address }] }),
        None => json!({}),
    }
}

/// Sets the email `mechanisms` on every planned policy. With no address the items are unchanged.
pub fn with_email(policies: Vec<Value>, email: Option<&str>) -> Vec<Value> {
    let Some(address) = email else {
        return policies;
    };
    policies
        .into_iter()
        .map(|mut p| {
            p["mechanisms"] = email_mechanisms(Some(address));
            p
        })
        .collect()
}

/// The one plan line that names the delivery address. No other line may hold it.
pub fn email_line(email: &str) -> String {
    format!("deliver by email to {email} (every policy in this plan)")
}

/// Hides the account id and the delivery address in Cloudflare's error text.
fn scrub(text: &str, account: &str, email: Option<&str>) -> String {
    let text = text.replace(account, "{account}");
    match email {
        Some(address) => text.replace(address, "{email}"),
        None => text,
    }
}

/// The JSON body posted to create a planned policy. `filters` is left out when empty, as
/// Cloudflare's MCP server does for `workers_observability_real_time_issue`
/// (github.com/cloudflare/mcp, `src/events/service.ts`), rather than sent as `{}`. `mechanisms` is
/// the planned item's own (see [`email_mechanisms`]), `{}` when it has none.
pub fn policy_body(p: &Value) -> Value {
    let mut body = json!({
        "name": p["name"], "alert_type": p["alert_type"],
        "enabled": p["enabled"],
        "mechanisms": if p["mechanisms"].is_object() { p["mechanisms"].clone() } else { json!({}) },
    });
    let filled = match &p["filters"] {
        Value::Object(f) => !f.is_empty(),
        Value::Null => false,
        _ => true,
    };
    if filled {
        body["filters"] = p["filters"].clone();
    }
    body
}

/// The approval line for a planned policy: what it is, what fires it, and what it watches.
///
/// Sources: cloudflare-docs `notifications/reference/webhook-payload-schema.mdx` ("Workers
/// observability alert": a state change of a Workers Observability Monitor, with `status` an
/// alert state) and the MCP server, where a real-time issue policy fires only through a Workers
/// Issues automation (`/workers/observability/issues/automations`) that names it.
pub fn plan_line(p: &Value) -> String {
    let kind = p["alert_type"].as_str().unwrap_or("");
    if kind == BILLING_TYPE {
        let first = |k: &str| p["filters"][k][0].as_str().unwrap_or("?").to_string();
        return format!(
            "create `{}` ({kind}; fires when {} usage passes {}, in Cloudflare's unit for this product)",
            p["name"].as_str().unwrap_or(""),
            first("product"),
            first("limit")
        );
    }
    let fires = match kind {
        "workers_observability_alert" => {
            "fires when a Workers Observability Monitor (made in the dashboard) changes state"
        }
        "workers_observability_real_time_issue" => {
            "fires only through a Workers Issues automation naming the policy, which pigeon \
             does not create"
        }
        _ => "",
    };
    let mut scope = Vec::new();
    let mut scripts = false;
    for (k, v) in p["filters"].as_object().into_iter().flatten() {
        if is_script_key(k) {
            scripts = true;
        } else {
            scope.push(format!("{k} = {v}"));
        }
    }
    if scripts {
        scope.insert(0, "pigeon's Workers only".to_string());
    }
    if scope.is_empty() {
        scope.push("every Worker in the account".to_string());
    }
    format!(
        "create `{}` ({kind}; {fires}; {})",
        p["name"].as_str().unwrap_or(""),
        scope.join(", ")
    )
}

/// The delivery types of a policy's `mechanisms`, each a list of `{id}` items.
const MECHANISM_TYPES: [&str; 3] = ["email", "webhooks", "pagerduty"];

/// A filter value as a set of strings, so the order of a list does not matter.
fn filter_set(v: &Value) -> std::collections::BTreeSet<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
            .collect(),
        Value::Null => Default::default(),
        Value::String(s) => [s.clone()].into(),
        other => [other.to_string()].into(),
    }
}

/// A policy's filters as `key -> set`; a missing or null `filters` is `{}`.
fn filter_sets(filters: &Value) -> BTreeMap<String, std::collections::BTreeSet<String>> {
    filters
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), filter_set(v))).collect())
        .unwrap_or_default()
}

/// The ids under one delivery type of `mechanisms`, in order.
fn mechanism_ids(mechanisms: &Value, kind: &str) -> Vec<String> {
    mechanisms[kind]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// How many deliveries of each type `mechanisms` holds. Counts only: ids can be email addresses.
fn mechanism_counts(mechanisms: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for kind in MECHANISM_TYPES {
        let n = mechanisms[kind].as_array().map_or(0, Vec::len);
        if n > 0 {
            out.insert(kind.to_string(), json!(n));
        }
    }
    Value::Object(out)
}

/// `existing` mechanisms with the `wanted` ones added per type by `id`. Nothing is removed, and
/// unknown keys and items stay as they are.
fn merge_mechanisms(existing: &Value, wanted: &Value) -> Value {
    let mut out = existing.as_object().cloned().unwrap_or_default();
    for kind in MECHANISM_TYPES {
        let Some(add) = wanted[kind].as_array() else {
            continue;
        };
        let mut items = out
            .get(kind)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut have = mechanism_ids(&Value::Object(out.clone()), kind);
        for m in add {
            if let Some(id) = m["id"].as_str()
                && !have.iter().any(|h| h == id)
            {
                have.push(id.to_string());
                items.push(m.clone());
            }
        }
        if !items.is_empty() {
            out.insert(kind.to_string(), Value::Array(items));
        }
    }
    Value::Object(out)
}

/// The existing policy named exactly `name`, only when `name` starts with `MONITOR_PREFIX`.
fn pigeon_policy<'a>(existing: &'a Value, name: &str) -> Option<&'a Value> {
    if !name.starts_with(MONITOR_PREFIX) {
        return None;
    }
    existing
        .as_array()?
        .iter()
        .find(|p| p["name"].as_str() == Some(name))
}

/// The updates the `pigeon: ...` policies in `existing` need to match `wanted` (what
/// `plan_monitors(available, &json!([]), scripts)` gives). Only a policy whose name starts with
/// `MONITOR_PREFIX` is looked at, and one of another `alert_type` is left alone (see
/// `monitor_left_alone`). Filters compare per key as sets, a missing `filters` is `{}`, and
/// `enabled` compares as is. An update never removes a delivery: wanted `mechanisms` are merged
/// into the existing ones per type by `id`.
///
/// Each item is `{id, name, alert_type, changes, body}`: `changes` is `{field: {from, to}}`
/// (mechanisms as counts per type, never ids) and `body` is the full PUT body. Cloudflare's
/// `PUT /accounts/{account_id}/alerting/v3/policies/{policy_id}` takes these fields with none
/// required (cloudflare/api-schemas, openapi.json), and Cloudflare's own MCP server
/// (github.com/cloudflare/mcp, src/events/service.ts) updates with a full-body PUT, so the whole
/// body is sent, with the existing `description` and `alert_interval` kept.
pub fn plan_monitor_updates(wanted: &[Value], existing: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for w in wanted {
        let Some(name) = w["name"].as_str() else {
            continue;
        };
        let Some(e) = pigeon_policy(existing, name) else {
            continue;
        };
        let Some(id) = e["id"].as_str() else {
            continue;
        };
        if e["alert_type"] != w["alert_type"] {
            continue;
        }
        let mut changes = serde_json::Map::new();
        let want_filters = if w["filters"].is_object() {
            w["filters"].clone()
        } else {
            json!({})
        };
        let have_filters = if e["filters"].is_object() {
            e["filters"].clone()
        } else {
            json!({})
        };
        if filter_sets(&want_filters) != filter_sets(&have_filters) {
            changes.insert(
                "filters".into(),
                json!({ "from": have_filters, "to": want_filters }),
            );
        }
        let want_enabled = w["enabled"].as_bool().unwrap_or(true);
        let have_enabled = e["enabled"].as_bool().unwrap_or(false);
        if want_enabled != have_enabled {
            changes.insert(
                "enabled".into(),
                json!({ "from": have_enabled, "to": want_enabled }),
            );
        }
        let have_mech = if e["mechanisms"].is_object() {
            e["mechanisms"].clone()
        } else {
            json!({})
        };
        let union = merge_mechanisms(&have_mech, &w["mechanisms"]);
        if MECHANISM_TYPES
            .iter()
            .any(|k| mechanism_ids(&union, k) != mechanism_ids(&have_mech, k))
        {
            changes.insert(
                "mechanisms".into(),
                json!({ "from": mechanism_counts(&have_mech), "to": mechanism_counts(&union) }),
            );
        }
        if changes.is_empty() {
            continue;
        }
        let mut body = json!({
            "name": name, "alert_type": w["alert_type"], "enabled": want_enabled,
            "filters": want_filters, "mechanisms": union,
        });
        for keep in ["description", "alert_interval"] {
            if !e[keep].is_null() {
                body[keep] = e[keep].clone();
            }
        }
        out.push(json!({
            "id": id, "name": name, "alert_type": w["alert_type"],
            "changes": Value::Object(changes), "body": body,
        }));
    }
    out
}

/// Plan lines for `pigeon: ...` policies that exist under another `alert_type` than pigeon wants.
/// They are not updated.
pub fn monitor_left_alone(wanted: &[Value], existing: &Value) -> Vec<String> {
    wanted
        .iter()
        .filter_map(|w| {
            let name = w["name"].as_str()?;
            let e = pigeon_policy(existing, name)?;
            (e["alert_type"] != w["alert_type"]).then(|| {
                format!(
                    "`{name}` exists as {}; left alone",
                    e["alert_type"].as_str().unwrap_or("an unknown type")
                )
            })
        })
        .collect()
}

/// A filters object as text, like `{status: [a, b]}`, keys and values sorted.
fn filters_text(filters: &Value) -> String {
    let parts: Vec<String> = filter_sets(filters)
        .into_iter()
        .map(|(k, v)| format!("{k}: [{}]", v.into_iter().collect::<Vec<_>>().join(", ")))
        .collect();
    format!("{{{}}}", parts.join(", "))
}

/// The approval line for one update from `plan_monitor_updates`, like
/// "update `pigeon: Workers errors`: filters {status: [a]} -> {status: [b]}; enabled false ->
/// true; delivery email 1 -> 2". Delivery shows types and counts, never ids.
pub fn update_line(update: &Value) -> String {
    let c = &update["changes"];
    let mut parts = Vec::new();
    if c["filters"].is_object() {
        parts.push(format!(
            "filters {} -> {}",
            filters_text(&c["filters"]["from"]),
            filters_text(&c["filters"]["to"])
        ));
    }
    if c["enabled"].is_object() {
        parts.push(format!(
            "enabled {} -> {}",
            c["enabled"]["from"], c["enabled"]["to"]
        ));
    }
    if c["mechanisms"].is_object() {
        let count = |side: &str, kind: &str| c["mechanisms"][side][kind].as_u64().unwrap_or(0);
        let per_type: Vec<String> = MECHANISM_TYPES
            .iter()
            .filter(|k| count("from", k) != count("to", k))
            .map(|k| format!("{k} {} -> {}", count("from", k), count("to", k)))
            .collect();
        parts.push(format!("delivery {}", per_type.join(", ")));
    }
    format!(
        "update `{}`: {}",
        update["name"].as_str().unwrap_or(""),
        parts.join("; ")
    )
}

/// The account's available alert types and its notification policies, read with a
/// Notifications Read token. A `pigeon: ...` policy whose list entry has no `filters` is read
/// on its own (GET .../policies/{id}) and that record is used; no other policy is.
async fn read_alerts(cx: &Context<'_>) -> Result<(Value, Value)> {
    let token = open(cx, NOTIFICATIONS_READ_PERMISSION).await?;
    let id = &cx.account.id;
    let work = async {
        let get = |path: String| {
            let t = token.value().to_string();
            async move { pigeon_core::cloudflare::call_fresh(&t, Method::Get, &path, None).await }
        };
        let a = get(format!("/accounts/{id}/alerting/v3/available_alerts")).await?;
        let mut p = get(format!("/accounts/{id}/alerting/v3/policies")).await?;
        if let Some(list) = p.as_array_mut() {
            for entry in list.iter_mut() {
                let mine = entry["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with(MONITOR_PREFIX));
                let Some(pid) = entry["id"].as_str().map(str::to_string) else {
                    continue;
                };
                if mine && entry.get("filters").is_none() {
                    let full = get(format!("/accounts/{id}/alerting/v3/policies/{pid}")).await?;
                    if full.is_object() {
                        *entry = full;
                    }
                }
            }
        }
        Ok((a, p))
    }
    .await;
    token.finish(work).await
}

/// What `available` and `existing` look like, without their values: the alert type groups, the
/// filter keys of the types pigeon wants or may want, how many policies there are and which are
/// pigeon's, and the field names of a policy and of its `mechanisms`. For checking the API's
/// shapes before a first apply.
pub fn shape_report(available: &Value, existing: &Value) -> Value {
    let types = available_types(available);
    let groups: Vec<String> = match available {
        Value::Object(m) => m.keys().cloned().collect(),
        _ => Vec::new(),
    };
    let watched: BTreeMap<&String, &Vec<String>> = types
        .iter()
        .filter(|(t, _)| t.starts_with("workers_observability") || t.starts_with("billing"))
        .collect();
    let values: BTreeMap<String, BTreeMap<String, Vec<String>>> =
        available_filter_values(available)
            .into_iter()
            .filter(|(t, _)| t.starts_with("workers_observability") || t.starts_with("billing"))
            .collect();
    let policies = existing.as_array().cloned().unwrap_or_default();
    let keys = |v: &Value| -> Vec<String> {
        v.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let first = policies.first().cloned().unwrap_or(Value::Null);
    json!({
        "available_alerts": {
            "is": if available.is_object() { "object" } else if available.is_array() { "array" } else { "other" },
            "groups": groups,
            "types_recognised": types.len(),
            "wanted_absent": WANTED_TYPES.iter().filter(|t| !types.contains_key(**t)).collect::<Vec<_>>(),
            "filter_keys": watched,
            "filter_values": values,
            "filter_options": filter_metadata(available),
        },
        "policies": {
            "is_list": existing.is_array(),
            "count": policies.len(),
            "pigeon": policies.iter().filter_map(|p| p["name"].as_str()).filter(|n| n.starts_with(MONITOR_PREFIX)).collect::<Vec<_>>(),
            "fields": keys(&first),
            "mechanisms_fields": keys(&first["mechanisms"]),
            "filters_fields": keys(&first["filters"]),
        },
    })
}

/// `observability_monitors_plan {check: true}`: the shape report, read-only.
async fn monitors_check(cx: &Context<'_>) -> Result<Value> {
    let (available, existing) = read_alerts(cx).await?;
    Ok(shape_report(&available, &existing))
}

/// What a monitors plan holds: policies to create, updates to existing ones, lines for policies
/// left alone, policies that need the person's filters, and wanted types the account does not
/// offer.
struct Planned {
    creates: Vec<Value>,
    updates: Vec<Value>,
    notes: Vec<String>,
    needs_filters: Vec<Value>,
    skipped: Vec<String>,
}

/// Reads the account's alert types and policies, and plans the missing monitors and the updates
/// to existing ones, with the person's explicit `filters` (null for none).
async fn monitors_planned(
    this: &Observability,
    cx: &Context<'_>,
    filters: &Value,
    email: Option<&str>,
    billing: &Value,
) -> Result<Planned> {
    let (available, existing) = read_alerts(cx).await?;
    if available_types(&available).is_empty() {
        return Err(Error::RustError(
            "couldn't read the account's available alerts: no alert type was recognised in \
             Cloudflare's answer, so nothing is planned"
                .into(),
        ));
    }
    if !existing.is_array() {
        return Err(Error::RustError(
            "couldn't read the account's notification policies: Cloudflare's answer is not a \
             list, so nothing is planned"
                .into(),
        ));
    }
    let explicit = validate_filters(&available, filters).map_err(Error::RustError)?;
    let scripts: Vec<String> = this.map.workers.iter().map(|w| w.script.clone()).collect();
    // Every policy pigeon wants, existing or not, is what existing ones are compared against.
    let billing = validate_billing(&available, billing).map_err(Error::RustError)?;
    let mut wanted = plan_monitors_with(&available, &json!([]), &scripts, &explicit).policies;
    let mut plan = plan_monitors_with(&available, &existing, &scripts, &explicit);
    // Billing watches are wanted only when `billing` names them.
    let wanted_billing = billing_policies(&billing);
    let have: Vec<&str> = existing
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["name"].as_str()).collect())
        .unwrap_or_default();
    plan.policies.extend(
        wanted_billing
            .iter()
            .filter(|p| !p["name"].as_str().is_some_and(|n| have.contains(&n)))
            .cloned(),
    );
    wanted.extend(wanted_billing);
    let mut notes = monitor_left_alone(&wanted, &existing);
    notes.extend(billing_notes(&available, &billing));
    // Email delivery goes on every wanted policy, billing watches included.
    let wanted = with_email(wanted, email);
    Ok(Planned {
        creates: with_email(plan.policies, email),
        updates: plan_monitor_updates(&wanted, &existing),
        notes,
        needs_filters: plan.needs_filters,
        skipped: absent_types(&available),
    })
}

/// The approval payload of a monitors plan: the policies to create, the updates, and the
/// person's inputs (the delivery address too, when given). Apply re-plans from the inputs and the
/// digest of this value must match.
fn monitors_payload(
    planned: &[Value],
    updates: &[Value],
    filters: &Value,
    email: Option<&str>,
    billing: &Value,
) -> Value {
    let or_empty = |v: &Value| if v.is_null() { json!({}) } else { v.clone() };
    let mut inputs = json!({ "filters": or_empty(filters), "billing": or_empty(billing) });
    if let Some(address) = email {
        inputs["email"] = json!(address);
    }
    json!({ "policies": planned, "updates": updates, "inputs": inputs })
}

/// `observability_monitors_plan`: the dry run, and an approval when something is missing.
async fn monitors_plan(this: &Observability, cx: &Context<'_>, args: &Value) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| Error::RustError("planning needs a known caller".into()))?;
    let person = caller.acting_for.as_ref().ok_or_else(|| {
        Error::RustError("the caller acts for no person who could approve this".into())
    })?;
    let filters = &args["filters"];
    let email = match &args["email"] {
        Value::Null => None,
        Value::String(s) => Some(validate_email(s).map_err(Error::RustError)?),
        _ => {
            return Err(Error::RustError(
                "`email` is not a valid address: it must be a string".into(),
            ));
        }
    };
    let email = email.as_deref();
    let billing = &args["billing"];
    let Planned {
        creates: planned,
        updates,
        notes,
        needs_filters: needs,
        skipped,
    } = monitors_planned(this, cx, filters, email, billing).await?;
    let skipped_note: Vec<String> = skipped
        .iter()
        .map(|t| format!("skipped: `{t}` is not in the account's available alerts"))
        .collect();
    if planned.is_empty() && updates.is_empty() {
        return Ok(
            json!({ "plan": notes, "applies": false, "skipped": skipped_note,
            "needs_filters": needs,
            "note": if !needs.is_empty() {
                "Nothing to create or update yet: some policies need filters Cloudflare requires and does not list (see needs_filters)."
            } else if skipped.is_empty() {
                "Every policy pigeon wants is already in place."
            } else {
                "Nothing to create or update: the policies pigeon wants are in place or their alert types are not offered (see skipped)."
            } }),
        );
    }
    let mut lines: Vec<String> = email.map(email_line).into_iter().collect();
    lines.extend(planned.iter().map(plan_line));
    lines.extend(updates.iter().map(update_line));
    lines.extend(notes);
    let payload = monitors_payload(&planned, &updates, filters, email, billing);
    let kind = pigeon_core::approval::Kind::Other(MONITORS_KIND.into());
    let digest = pigeon_core::approval::payload_digest(&kind, "monitors", &payload);
    let made = crate::users::directory(
        cx.env,
        "approval.create",
        json!({
            "kind": kind.name(),
            "subject": "monitors",
            "digest": digest,
            "summary": { "plan": lines, "skipped": skipped_note },
            "payload": payload,
            "caller_id": caller.user.id,
            "approver_id": person.id,
            "pending_ms": MONITORS_PENDING_MS,
        }),
    )
    .await?;
    Ok(json!({
        "plan": lines,
        "skipped": skipped_note,
        "needs_filters": needs,
        "applies": true,
        "approval": made["record"]["id"],
        "waits_until_ms": made["record"]["expires_ms"],
        "note": "Nothing is created or changed until the person approves this plan on the \
                 console's Approvals tab; then observability_monitors_apply {plan: <this \
                 approval>} creates and updates the policies once. Updates keep every existing \
                 delivery. Delivery by email is set on every policy only when `email` is \
                 given; otherwise the person adds it in the dashboard.",
    }))
}

/// `observability_monitors_apply`: creates and updates the approved policies if the account
/// still gives the same plan. Only policies named `pigeon: ...` are touched; none is deleted.
async fn monitors_apply(this: &Observability, cx: &Context<'_>, args: &Value) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| Error::RustError("applying needs a known caller".into()))?;
    let approval_id = crate::arg(args, "plan")?;
    let got = crate::users::directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
    if got["kind"] != MONITORS_KIND {
        return Err(Error::RustError(format!(
            "there is no monitors plan approval {approval_id}"
        )));
    }
    // The person's inputs come back from the approval record's payload, so the re-plan is the
    // one that was approved; the digest must still match.
    let filters = got["payload"]["inputs"]["filters"].clone();
    let email = match got["payload"]["inputs"]["email"].as_str() {
        Some(s) => Some(validate_email(s).map_err(Error::RustError)?),
        None => None,
    };
    let email = email.as_deref();
    let billing = got["payload"]["inputs"]["billing"].clone();
    let Planned {
        creates: planned,
        updates,
        ..
    } = monitors_planned(this, cx, &filters, email, &billing).await?;
    let kind = pigeon_core::approval::Kind::Other(MONITORS_KIND.into());
    let digest = pigeon_core::approval::payload_digest(
        &kind,
        "monitors",
        &monitors_payload(&planned, &updates, &filters, email, &billing),
    );
    let taken = crate::users::directory(
        cx.env,
        "approval.take",
        json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(Error::RustError(format!(
            "the plan does not run: {why}. If the account changed since it was approved, plan again."
        )));
    }
    let token = Scoped::open_for(
        cx.account,
        "observability",
        vec![Policy::new(
            Scope::Account,
            &[NOTIFICATIONS_WRITE_PERMISSION],
        )],
        TOKEN_TTL,
    )
    .await?;
    let path = format!("/accounts/{}/alerting/v3/policies", cx.account.id);
    // Each policy is posted on its own; the first refusal stops the rest, and the answer still
    // lists what was created before it, since the approval is spent.
    let work = async {
        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut failed = Value::Null;
        for p in planned.iter().filter(|p| {
            p["name"]
                .as_str()
                .is_some_and(|n| n.starts_with(MONITOR_PREFIX))
        }) {
            // The body carries email delivery only when the person gave an address.
            let body = policy_body(p);
            match pigeon_core::cloudflare::call_fresh(
                token.value(),
                Method::Post,
                &path,
                Some(&body),
            )
            .await
            {
                Ok(made) => created.push(json!({ "name": p["name"], "id": made["id"] })),
                Err(e) => {
                    // Cloudflare's own text, code and message, with only the account id hidden.
                    failed = json!({
                        "name": p["name"],
                        "why": scrub(&e.to_string(), cx.account.id.as_str(), email),
                    });
                    break;
                }
            }
        }
        if failed.is_null() {
            for u in updates.iter().filter(|u| {
                u["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with(MONITOR_PREFIX))
            }) {
                let Some(pid) = u["id"].as_str() else {
                    continue;
                };
                match pigeon_core::cloudflare::call_fresh(
                    token.value(),
                    Method::Put,
                    &format!("{path}/{pid}"),
                    Some(&u["body"]),
                )
                .await
                {
                    Ok(_) => updated.push(json!({ "name": u["name"], "id": pid })),
                    Err(e) => {
                        failed = json!({
                            "name": u["name"],
                            "why": scrub(&e.to_string(), cx.account.id.as_str(), email),
                        });
                        break;
                    }
                }
            }
        }
        Ok((created, updated, failed))
    }
    .await;
    let (created, updated, failed) = token.finish(work).await?;
    let delivery = if email.is_some() {
        "Email delivery was set as approved."
    } else {
        "No delivery was set: the person adds email in the dashboard (Alerts > Notifications), or plans again with `email`."
    };
    let note = if failed.is_null() {
        format!("{delivery} Updates kept every existing delivery.")
    } else {
        format!(
            "Stopped at `failed`; the policies in `created` and `updated` are done. This \
             approval is spent: plan again for the rest (created ones are not planned again). \
             {delivery}"
        )
    };
    Ok(json!({ "created": created, "updated": updated, "failed": failed, "note": note }))
}

/// The agent runs created in `[from_ms, to_ms]`, from `ai_agents_status`'s list: how many and
/// their spend in dollars. Titles, briefs and names are never read. An error says what the
/// shape lacked.
pub fn fold_agent_runs(
    answer: &Value,
    from_ms: i64,
    to_ms: i64,
) -> std::result::Result<(u64, f64), String> {
    let runs = answer["runs"]
        .as_array()
        .ok_or_else(|| "the answer has no `runs` list".to_string())?;
    let (mut count, mut usd) = (0, 0.0);
    for r in runs {
        let Some(created) = r["created_at"].as_i64() else {
            continue;
        };
        if created < from_ms || created > to_ms {
            continue;
        }
        count += 1;
        // A run's own cost: AI Gateway's cost per turn, the local estimate for turns not yet
        // read. Executors are runs of their own, so a planner's executors are not added here.
        let spent = &r["spent"];
        let cost = &spent["cost"];
        usd += spent["cost_usd"]
            .as_f64()
            .or_else(|| cost.as_f64())
            .or_else(|| {
                cost.as_str()
                    .and_then(|s| s.trim_start_matches('$').parse().ok())
            })
            .unwrap_or(0.0);
    }
    Ok((count, usd))
}

/// The turns of the agent runs created in `[from_ms, to_ms]` whose cost is still the local
/// estimate, not yet read from AI Gateway (`spent.unreconciled_turns`; absent counts as 0).
pub fn unreconciled_agent_turns(answer: &Value, from_ms: i64, to_ms: i64) -> u64 {
    answer["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| {
            r["created_at"]
                .as_i64()
                .is_some_and(|c| c >= from_ms && c <= to_ms)
        })
        .filter_map(|r| r["spent"]["unreconciled_turns"].as_u64())
        .sum()
}

/// Seconds of Containers time per class for jobs that finished in `[from_ms, to_ms]`, and how
/// many jobs came back. Only the `containers` connector counts.
pub fn fold_ci_jobs(
    answer: &Value,
    from_ms: i64,
    to_ms: i64,
) -> std::result::Result<(BTreeMap<String, f64>, usize), String> {
    let jobs = answer["jobs"]
        .as_array()
        .ok_or_else(|| "the answer has no `jobs` list".to_string())?;
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for j in jobs {
        let finished = j["finished_at"].as_i64().unwrap_or(-1);
        if j["connector"] != "containers" || finished < from_ms || finished > to_ms {
            continue;
        }
        let Some(ms) = j["duration_ms"].as_f64() else {
            continue;
        };
        let class = j["class"].as_str().unwrap_or("unknown");
        *out.entry(class.to_string()).or_default() += ms / 1000.0;
    }
    Ok((out, jobs.len()))
}

/// Containers list price in dollars per vCPU-second.
const CONTAINERS_PER_VCPU_SECOND: f64 = 0.000020;
/// Containers list price in dollars per GiB-second of memory.
const CONTAINERS_PER_GIB_SECOND: f64 = 0.0000025;
/// Containers list price in dollars per GB-second of disk.
const CONTAINERS_PER_DISK_GB_SECOND: f64 = 0.00000007;

/// List price in dollars of Containers time: seconds per class, priced on each class's
/// provisioned size. Returns the total, dollars per class, and the classes with no known size.
pub fn ci_price(seconds: &BTreeMap<String, f64>) -> (f64, BTreeMap<String, f64>, Vec<String>) {
    let (mut total, mut by_class, mut unpriced) = (0.0, BTreeMap::new(), Vec::new());
    for (class, secs) in seconds {
        match pigeon_ci::containers::class_resources(class) {
            Some((cpu, mem, disk)) => {
                let usd = secs
                    * (f64::from(cpu) * CONTAINERS_PER_VCPU_SECOND
                        + f64::from(mem) * CONTAINERS_PER_GIB_SECOND
                        + f64::from(disk) * CONTAINERS_PER_DISK_GB_SECOND);
                total += usd;
                by_class.insert(class.clone(), usd);
            }
            None => unpriced.push(class.clone()),
        }
    }
    (total, by_class, unpriced)
}

/// Opens a token for one source, living five minutes.
async fn open<'a>(cx: &Context<'a>, permission: &str) -> Result<Scoped<'a>> {
    Scoped::open_for(
        cx.account,
        "observability",
        vec![Policy::new(Scope::Account, &[permission])],
        TOKEN_TTL,
    )
    .await
}

/// `work`, or an error once [`SOURCE_LIMIT_MS`] has passed.
pub(crate) async fn within<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let limit = worker::Delay::from(std::time::Duration::from_millis(SOURCE_LIMIT_MS));
    futures_util::pin_mut!(work);
    futures_util::pin_mut!(limit);
    match futures_util::future::select(work, limit).await {
        futures_util::future::Either::Left((v, _)) => v,
        futures_util::future::Either::Right(_) => Err(Error::RustError(format!(
            "no answer within {} s",
            SOURCE_LIMIT_MS / 1000
        ))),
    }
}

/// Mints a token with `permission`, runs `work` with it and revokes it, all within
/// [`SOURCE_LIMIT_MS`]. The error says which step failed, with the account id taken out.
pub(crate) async fn with_token<'a, F, Fut>(
    cx: &Context<'a>,
    permission: &str,
    work: F,
) -> std::result::Result<Value, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let hide = |e: Error| e.to_string().replace(cx.account.id.as_str(), "{account}");
    let outcome = within(async {
        let token = open(cx, permission)
            .await
            .map_err(|e| Error::RustError(format!("the token was not minted: {e}")))?;
        let done = work(token.value().to_string()).await;
        token.finish(done).await
    })
    .await;
    outcome.map_err(hide)
}

/// `tokens_status`: the live-token gauge, after setting the soft ceiling when `soft_ceiling` is
/// given. Setting it is for a person or an agent acting for one; a value outside 10 to 450 is
/// refused.
async fn tokens_status(cx: &Context<'_>, args: &Value) -> Result<Value> {
    if !args["soft_ceiling"].is_null() {
        let may = cx
            .caller
            .is_some_and(|c| c.user.kind == "standard" || c.acting_for.is_some());
        if !may {
            return Err(Error::RustError(
                "only a person, or an agent acting for one, may set the soft ceiling".into(),
            ));
        }
        let want = args["soft_ceiling"].as_i64().ok_or_else(|| {
            Error::RustError("soft_ceiling must be a whole number from 10 to 450".into())
        })?;
        crate::ledger::ceiling_set(cx.env, want).await?;
    }
    Ok(serde_json::to_value(crate::ledger::gauge(cx.env).await?)?)
}

/// The gauge as the summary and `work_status` show it: live, soft ceiling, hard cap and when the
/// count was taken (`as_of`, ISO time).
pub(crate) fn gauge_view(g: &crate::ledger::Gauge) -> Value {
    json!({
        "live": g.live,
        "soft_ceiling": g.soft_ceiling,
        "hard_cap": g.hard_cap,
        "as_of": iso(g.as_of_ms as f64),
        "source": g.source,
        "account_total": g.account_total,
        "account_total_as_of_ms": g.account_total_as_of_ms,
        "last_sweep_succeeded_at_ms": g.last_sweep_succeeded_at_ms,
    })
}

/// `observability_errors`: the summary's metrics and error-log sources, folded the same way,
/// for one Worker or all. A source that cannot be read goes to `unavailable`.
async fn errors(this: &Observability, cx: &Context<'_>, args: &Value) -> Result<Value> {
    let ask = errors_args(args).map_err(Error::RustError)?;
    let to = Date::now();
    let from = to - ask.hours as f64 * 3_600_000.0;
    let id = cx.account.id.as_str();
    let metrics_work = with_token(cx, ANALYTICS_PERMISSION, |t| async move {
        graphql(&t, &graphql_query(id, &iso(from), &iso(to))).await
    });
    let logs_work = with_token(cx, OBSERVABILITY_PERMISSION, |t| async move {
        let path = format!("/accounts/{id}/workers/observability/telemetry/query");
        let body = telemetry_query(&format!("pigeon-e-{}", to as i64), from as i64, to as i64);
        pigeon_core::cloudflare::call_fresh(&t, Method::Post, &path, Some(&body)).await
    });
    let (metrics_v, logs_v) = futures_util::future::join(metrics_work, logs_work).await;
    let mut unavailable = Vec::new();
    let scripts = match metrics_v {
        Ok(v) => fold_invocations(&v),
        Err(why) => {
            unavailable.push(missing("metrics", &why));
            BTreeMap::new()
        }
    };
    let messages = match logs_v {
        Ok(v) => fold_errors_top(&v, ask.limit),
        Err(why) => {
            unavailable.push(missing("logs", &why));
            BTreeMap::new()
        }
    };
    let known: Vec<String> = this.map.workers.iter().map(|w| w.script.clone()).collect();
    let mut out = errors_answer(&scripts, &messages, &known, &ask);
    out["unavailable"] = json!(unavailable);
    Ok(out)
}

/// Builds the answer for the last `hours`. The sources run together, each within
/// [`SOURCE_LIMIT_MS`]; a source that cannot be read goes to `unavailable` and the rest is
/// still answered.
async fn summary(this: &Observability, cx: &Context<'_>, hours: u64) -> Result<Value> {
    let to = Date::now();
    let from = to - hours as f64 * 3_600_000.0;
    let mut unavailable = Vec::new();
    let id = cx.account.id.as_str();
    let hide = |e: Error| e.to_string().replace(id, "{account}");

    // Metrics: Account Analytics Read.
    let metrics_work = with_token(cx, ANALYTICS_PERMISSION, |t| async move {
        graphql(&t, &graphql_query(id, &iso(from), &iso(to))).await
    });
    // Logs: Workers Observability Write, the only permission the query endpoint takes, for two
    // POSTs: the error lines, and which Workers have any events at all.
    let logs_work = with_token(cx, OBSERVABILITY_PERMISSION, |t| async move {
        let path = format!("/accounts/{id}/workers/observability/telemetry/query");
        let errors = telemetry_query(&format!("pigeon-{}", to as i64), from as i64, to as i64);
        let services =
            telemetry_services_query(&format!("pigeon-s-{}", to as i64), from as i64, to as i64);
        let post = |body: Value| {
            let (t, path) = (t.clone(), path.clone());
            async move {
                pigeon_core::cloudflare::call_fresh(&t, Method::Post, &path, Some(&body)).await
            }
        };
        let (errors, services) = futures_util::future::join(post(errors), post(services)).await;
        Ok(json!({ "errors": errors?, "services": services? }))
    });
    // Billable usage: Billing Read.
    let billing_work = with_token(cx, BILLING_PERMISSION, |t| async move {
        let path = format!("/accounts/{id}/billable-usage");
        pigeon_core::cloudflare::call_fresh(&t, Method::Get, &path, None).await
    });
    let deploys_work = within(crate::land::deploys_since(cx.env, from as i64));
    let (models_args, agents_args, ci_args) = (
        json!({ "hours": hours }),
        json!({ "limit": 100 }),
        json!({ "limit": 200 }),
    );
    let models_work = within(crate::dispatch(cx, "models_usage", &models_args));
    let agents_work = within(crate::dispatch(cx, "ai_agents_status", &agents_args));
    let ci_work = within(crate::dispatch(cx, "ci_jobs", &ci_args));
    let tokens_work = within(crate::ledger::gauge(cx.env));

    let ((metrics_v, logs_v, billing_v), (deploys_v, models_v, agents_v, ci_v, tokens_v)) =
        futures_util::future::join(
            futures_util::future::join3(metrics_work, logs_work, billing_work),
            futures_util::future::join5(
                deploys_work,
                models_work,
                agents_work,
                ci_work,
                tokens_work,
            ),
        )
        .await;
    let tokens = match tokens_v {
        Ok(g) => gauge_view(&g),
        Err(e) => {
            unavailable.push(missing("Cloudflare tokens", &hide(e)));
            Value::Null
        }
    };

    let mut hourly = BTreeMap::new();
    let mut metrics_ok = true;
    let metrics = match metrics_v {
        Ok(v) => {
            hourly = fold_hourly(&v);
            fold_invocations(&v)
        }
        Err(why) => {
            metrics_ok = false;
            unavailable.push(missing("metrics", &why));
            BTreeMap::new()
        }
    };
    let (errors, services) = match logs_v {
        Ok(v) => (
            fold_errors(&v["errors"]),
            Some(fold_services(&v["services"])),
        ),
        Err(why) => {
            unavailable.push(missing("logs", &why));
            (BTreeMap::new(), None)
        }
    };
    let billed = match billing_v {
        Ok(v) => fold_billing(&v),
        Err(why) => {
            unavailable.push(missing("billable usage", &why));
            Vec::new()
        }
    };
    let deploys = match deploys_v {
        Ok(v) => v,
        Err(e) => {
            unavailable.push(missing("deploys", &hide(e)));
            Vec::new()
        }
    };
    let mut workers = Vec::new();
    let (mut requests, mut cpu_ms) = (0.0, 0.0);
    for w in &this.map.workers {
        let s = metrics.get(&w.script).cloned().unwrap_or_default();
        requests += s.requests;
        cpu_ms += s.cpu_ms_estimate();
        let mut entry = s.public();
        entry["name"] = json!(w.name);
        entry["script"] = json!(w.script);
        // A Worker that served requests but has no log events at all: its logs are probably
        // off, so "no errors" would be a guess. Say so rather than answer an empty list.
        let silent = services
            .as_ref()
            .is_some_and(|seen| s.requests > 0.0 && !seen.contains(&w.script));
        if silent {
            entry["top_errors"] = Value::Null;
            unavailable.push(missing(
                &format!("logs for {}", w.name),
                &format!(
                    "no log events in the window although it served {} requests; its Workers \
                     Logs may be off, so its errors are unknown",
                    s.requests
                ),
            ));
        } else {
            entry["top_errors"] = json!(errors.get(&w.script).cloned().unwrap_or_default());
        }
        let mine: Vec<Value> = deploys
            .iter()
            .filter(|d| d["worker"] == w.name.as_str() || d["worker"] == w.script.as_str())
            .cloned()
            .collect();
        entry["spike_after_deploy"] = if metrics_ok {
            let none = BTreeMap::new();
            let hours = hourly.get(&w.script).unwrap_or(&none);
            json!(spike_after_deploy(hours, &mine, s.error_rate()))
        } else {
            Value::Null
        };
        entry["deploys"] = json!(mine);
        workers.push(entry);
    }

    let mut cost = Vec::new();
    for b in &billed {
        cost.push(line(
            b["ServiceName"].as_str().unwrap_or("unknown"),
            "billed",
            "Cloudflare billable usage, current period",
            b["BilledCost"].as_f64(),
            b.clone(),
        ));
    }
    if !metrics.is_empty() {
        cost.push(line(
            "Workers",
            "estimated",
            "list price on the window's requests and CPU time, before included usage",
            Some(workers_estimate(requests, cpu_ms)),
            json!({ "requests": requests, "cpu_ms": cpu_ms }),
        ));
    }
    match models_v {
        Ok(v) => {
            let cut_short = v["truncated"] == true;
            cost.push(line(
                "Models",
                "estimated",
                if cut_short {
                    "AI Gateway's own cost per request, prompt-cache reads and writes included; the log listing was cut short, so this is a lower bound"
                } else {
                    "AI Gateway's own cost per request, prompt-cache reads and writes included"
                },
                v["cost"].as_f64(),
                json!({ "requests": v["requests"], "tokens_in": v["tokens_in"], "tokens_out": v["tokens_out"], "truncated": cut_short }),
            ));
        }
        Err(e) => unavailable.push(missing("models cost", &hide(e))),
    }

    // Agent run spend: metered, and already inside the Models line, so never summed.
    match agents_v {
        Ok(v) => match fold_agent_runs(&v, from as i64, to as i64) {
            Ok((count, usd)) => cost.push(line(
                "AI agent runs",
                "metered",
                "each run's own turns at AI Gateway's cost (the local estimate for turns not yet read from the gateway), every executor counted once as its own run. The same model use as the Models line, so it is not added to either total",
                Some(usd),
                json!({
                    "runs": count,
                    "listed": v["runs"].as_array().map_or(0, Vec::len),
                    "unreconciled_turns": unreconciled_agent_turns(&v, from as i64, to as i64),
                }),
            )),
            Err(why) => unavailable.push(missing("AI agent run spend", &why)),
        },
        Err(e) => unavailable.push(missing("AI agent run spend", &hide(e))),
    }

    // CI Containers time, priced at list on each class's provisioned size.
    match ci_v {
        Ok(v) => match fold_ci_jobs(&v, from as i64, to as i64) {
            Ok((seconds, listed)) => {
                let (usd, usd_by_class, unpriced) = ci_price(&seconds);
                cost.push(line(
                    "CI Containers",
                    "estimated",
                    if listed >= 200 {
                        "CI scheduler; 200 jobs came back, so older jobs in the window are missing. List price on the provisioned instance size, before included usage, so an upper bound"
                    } else {
                        "CI scheduler. List price on the provisioned instance size, before included usage, so an upper bound"
                    },
                    Some(usd),
                    json!({ "seconds_by_class": seconds, "usd_by_class": usd_by_class, "unpriced": unpriced, "jobs_listed": listed, "truncated": listed >= 200 }),
                ));
            }
            Err(why) => unavailable.push(missing("CI Containers time", &why)),
        },
        Err(e) => unavailable.push(missing("CI Containers time", &hide(e))),
    }
    unavailable.push(missing(
        "Buildkite cost",
        "Buildkite's API has no cost data",
    ));

    let sum = |kind: &str| -> f64 {
        cost.iter()
            .filter(|c| c["kind"] == kind)
            .filter_map(|c| c["usd"].as_f64())
            .sum()
    };
    Ok(json!({
        "window": { "from": iso(from), "to": iso(to), "hours": hours },
        "workers": workers,
        "cost": cost,
        "totals": { "estimated_usd": sum("estimated"), "billed_usd": sum("billed") },
        "tokens": tokens,
        "unavailable": unavailable,
    }))
}

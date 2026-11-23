//! `observability_summary`: a read-only look at how the platform's Workers are doing and what
//! the platform costs, from Cloudflare's metrics, logs and billable usage, plus pigeon's own
//! model cost and deploys. The folding is plain functions with no Worker types, so the smoke
//! suite runs them on fixtures. See docs/plans/observability.md.

use std::collections::BTreeMap;

use pigeon_core::lanes::Map;
use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{Policy, Scope, Scoped};
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::console::sections::{Section, Tab};
use crate::users::User;
use crate::{Call, Capability, Context};

/// The metrics permission: GraphQL `workersInvocationsAdaptive`.
const ANALYTICS_PERMISSION: &str = "Account Analytics Read";
/// The only permission the logs query endpoint accepts.
const OBSERVABILITY_PERMISSION: &str = "Workers Observability Write";
/// The billable usage permission.
const BILLING_PERMISSION: &str = "Billing Read";
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
                .take(TOP_ERRORS)
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

/// The `observability` capability: its one tool.
pub struct Observability {
    /// The platform Workers (ci/workers.toml), whose scripts are reported.
    pub map: Map,
}

impl Capability for Observability {
    fn name(&self) -> &'static str {
        "observability"
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Cost,
            id: "platform",
            title: "Platform",
            description: "Cloudflare metrics, logs and billable usage for pigeon's Workers; \
                          estimated and billed cost kept apart.",
            order: 2,
            // Its Models line is already the Models section's cost, and its window is at most
            // 7 days, so adding it to the overview total would double count and mix windows.
            cost: None,
        }]
    }

    /// The same answer as `observability_summary`, over the last `days` days (at most 168
    /// hours), for the console's Platform section.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let days = query["days"].as_u64().unwrap_or(7).clamp(1, 30);
            summary(self, cx, (days * 24).min(168)).await
        })
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "observability_monitors_plan",
                description: "Dry run of the notification policies pigeon wants (Workers \
                              observability alerts) and the account lacks. Asks the person's \
                              approval when something is missing. With `check`, only reads: \
                              the shape of Cloudflare's alert types and policies, field names \
                              and counts, and asks nothing.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "check": { "type": "boolean", "description": "Read-only shape check; no approval is asked." } }
                }),
            },
            Tool {
                name: "observability_monitors_apply",
                description: "Creates the policies of an approved monitors plan, once. Only \
                              policies named `pigeon: ...` are created.",
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
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "observability_monitors_plan" if args["check"] == true => {
                    return monitors_check(cx).await;
                }
                "observability_monitors_plan" => return monitors_plan(self, cx).await,
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
/// The alert types pigeon wants. The billing usage watches are left out until their product
/// and filter names are checked against the live API (docs/plans/observability.md).
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

/// The values `available` lists for each filter option, as `type -> key -> values`. Cloudflare
/// gives them as `AvailableValues` (objects with `Value` or `Name`, or plain strings); a key with
/// no list has an empty vector.
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

/// Cloudflare's documented `status` value for Workers Observability alerts, used when the API
/// lists no values for the filter. Not confirmed live: the check report shows what the API gives.
const DEFAULT_ERROR_STATUS: &str = "error";

/// The `status` values that mean errors or failures, from the API's list when it has one.
fn error_statuses(listed: &[String]) -> Vec<String> {
    let picked: Vec<String> = listed
        .iter()
        .filter(|v| {
            let l = v.to_lowercase();
            ["error", "fail", "exception"].iter().any(|w| l.contains(w))
        })
        .cloned()
        .collect();
    if picked.is_empty() {
        vec![DEFAULT_ERROR_STATUS.to_string()]
    } else {
        picked
    }
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

/// The notification policies pigeon wants that `existing` (the policies list) lacks by name.
/// Types missing from `available` are skipped. Worker alerts are filtered to `scripts` when
/// the type has a script filter. Each item is `{name, alert_type, filters, enabled}`.
pub fn plan_monitors(available: &Value, existing: &Value, scripts: &[String]) -> Vec<Value> {
    let have = available_types(available);
    let values = available_filter_values(available);
    let names: Vec<&str> = existing
        .as_array()
        .map(|a| a.iter().filter_map(|p| p["name"].as_str()).collect())
        .unwrap_or_default();
    let mut wanted = Vec::new();
    for (name, kind) in [
        ("pigeon: Workers errors", "workers_observability_alert"),
        (
            "pigeon: Workers real-time issues",
            "workers_observability_real_time_issue",
        ),
    ] {
        let Some(keys) = have.get(kind) else {
            continue;
        };
        let key = keys
            .iter()
            .find(|k| matches!(k.as_str(), "services" | "worker_name" | "script_name"));
        // Cloudflare refuses a policy without the filters it lists (error 17103), so a
        // `status` filter is always filled. A type with no filter keys gets an explicit `{}`.
        let filters = match key {
            Some(k) => json!({ k.as_str(): scripts }),
            None if keys.iter().any(|k| k == "status") => {
                let listed = values
                    .get(kind)
                    .and_then(|m| m.get("status"))
                    .cloned()
                    .unwrap_or_default();
                json!({ "status": error_statuses(&listed) })
            }
            None => json!({}),
        };
        wanted
            .push(json!({ "name": name, "alert_type": kind, "filters": filters, "enabled": true }));
    }
    wanted
        .into_iter()
        .filter(|w| w["name"].as_str().is_some_and(|n| !names.contains(&n)))
        .collect()
}

/// The account's available alert types and its notification policies, read with a
/// Notifications Read token.
async fn read_alerts(cx: &Context<'_>) -> Result<(Value, Value)> {
    let token = open(cx, NOTIFICATIONS_READ_PERMISSION).await?;
    let id = &cx.account.id;
    let work = async {
        let get = |path: String| {
            let t = token.value().to_string();
            async move { pigeon_core::cloudflare::call_fresh(&t, Method::Get, &path, None).await }
        };
        let a = get(format!("/accounts/{id}/alerting/v3/available_alerts")).await?;
        let p = get(format!("/accounts/{id}/alerting/v3/policies")).await?;
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

/// Reads the account's alert types and policies, and plans the missing monitors. Answers the
/// planned policies and the types that were skipped.
async fn monitors_planned(
    this: &Observability,
    cx: &Context<'_>,
) -> Result<(Vec<Value>, Vec<String>)> {
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
    let scripts: Vec<String> = this.map.workers.iter().map(|w| w.script.clone()).collect();
    Ok((
        plan_monitors(&available, &existing, &scripts),
        absent_types(&available),
    ))
}

/// `observability_monitors_plan`: the dry run, and an approval when something is missing.
async fn monitors_plan(this: &Observability, cx: &Context<'_>) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| Error::RustError("planning needs a known caller".into()))?;
    let person = caller.acting_for.as_ref().ok_or_else(|| {
        Error::RustError("the caller acts for no person who could approve this".into())
    })?;
    let (planned, skipped) = monitors_planned(this, cx).await?;
    let skipped_note: Vec<String> = skipped
        .iter()
        .map(|t| format!("skipped: `{t}` is not in the account's available alerts"))
        .collect();
    if planned.is_empty() {
        return Ok(
            json!({ "plan": [], "applies": false, "skipped": skipped_note,
            "note": if skipped.is_empty() {
                "Every policy pigeon wants is already in place."
            } else {
                "Nothing to create: the policies pigeon wants are in place or their alert types are not offered (see skipped)."
            } }),
        );
    }
    let lines: Vec<String> = planned
        .iter()
        .map(|p| {
            // Live check (2026-11-23): neither Workers alert type offers a script filter, so
            // such a policy watches every Worker in the account. The approval says so.
            let scope = if p["filters"].as_object().is_some_and(|f| f.is_empty()) {
                ", every Worker in the account: Cloudflare offers no script filter for this type"
            } else {
                ", pigeon's Workers only"
            };
            format!(
                "create `{}` ({}{scope})",
                p["name"].as_str().unwrap_or(""),
                p["alert_type"].as_str().unwrap_or("")
            )
        })
        .collect();
    let payload = json!({ "policies": planned });
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
        "applies": true,
        "approval": made["record"]["id"],
        "waits_until_ms": made["record"]["expires_ms"],
        "note": "Nothing is created until the person approves this plan on the console's \
                 Approvals tab; then observability_monitors_apply {plan: <this approval>} \
                 creates the policies once. Delivery is not set: the person adds it in the \
                 dashboard.",
    }))
}

/// `observability_monitors_apply`: creates the approved policies if the account still gives
/// the same plan. Only policies named `pigeon: ...` are created; none is changed or deleted.
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
    let (planned, _) = monitors_planned(this, cx).await?;
    let kind = pigeon_core::approval::Kind::Other(MONITORS_KIND.into());
    let digest =
        pigeon_core::approval::payload_digest(&kind, "monitors", &json!({ "policies": planned }));
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
        let mut failed = Value::Null;
        for p in planned.iter().filter(|p| {
            p["name"]
                .as_str()
                .is_some_and(|n| n.starts_with(MONITOR_PREFIX))
        }) {
            // The directory's user record has no email, so no delivery is set here.
            let body = json!({
                "name": p["name"], "alert_type": p["alert_type"], "filters": p["filters"],
                "enabled": p["enabled"], "mechanisms": {},
            });
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
                    failed = json!({
                        "name": p["name"],
                        "why": e.to_string().replace(cx.account.id.as_str(), "{account}"),
                    });
                    break;
                }
            }
        }
        Ok((created, failed))
    }
    .await;
    let (created, failed) = token.finish(work).await?;
    let note = if failed.is_null() {
        "No delivery is set: the person adds email in the dashboard (Alerts > Notifications)."
    } else {
        "Stopped at `failed`; the policies in `created` exist. This approval is spent: plan \
         again for the rest (created ones are not planned again). No delivery is set: the \
         person adds email in the dashboard (Alerts > Notifications)."
    };
    Ok(json!({ "created": created, "failed": failed, "note": note }))
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
        let cost = &r["spent"]["cost"];
        usd += cost
            .as_f64()
            .or_else(|| {
                cost.as_str()
                    .and_then(|s| s.trim_start_matches('$').parse().ok())
            })
            .unwrap_or(0.0);
    }
    Ok((count, usd))
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
async fn within<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
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
async fn with_token<'a, F, Fut>(
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

    let ((metrics_v, logs_v, billing_v), (deploys_v, models_v, agents_v, ci_v)) =
        futures_util::future::join(
            futures_util::future::join3(metrics_work, logs_work, billing_work),
            futures_util::future::join4(deploys_work, models_work, agents_work, ci_work),
        )
        .await;

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
                    "AI Gateway cost plus prompt-cache reads and writes priced from the model gateway's jobs; the log listing was cut short, so this is a lower bound. Requests not sent as model-gateway jobs, such as models_run, carry no cache count"
                } else {
                    "AI Gateway cost plus prompt-cache reads and writes priced from the model gateway's jobs, such as AI agent turns. Requests not sent as model-gateway jobs, such as models_run, carry no cache count"
                },
                v["cost_with_cache"].as_f64().or(v["cost"].as_f64()),
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
                "AI agents Worker, priced with prompt-cache tokens, so it can differ a little from the Models line, which counts cache tokens only for model-gateway jobs. The same model use, so it is not added to either total",
                Some(usd),
                json!({ "runs": count, "listed": v["runs"].as_array().map_or(0, Vec::len) }),
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
        "unavailable": unavailable,
    }))
}

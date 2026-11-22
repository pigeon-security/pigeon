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
        dimensions { scriptName status } } } } }";
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
        let unit = row["ConsumedUnit"].as_str().unwrap_or_default();
        let entry = by
            .entry((name.to_string(), unit.to_string()))
            .or_insert_with(|| {
                let mut kept =
                    json!({ "ServiceName": name, "ConsumedQuantity": 0.0, "BilledCost": 0.0 });
                for k in [
                    "ServiceFamilyName",
                    "ConsumedUnit",
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

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "observability_summary",
            description: "Read-only. Per-Worker requests, errors, CPU and wall time, top error \
                          messages and deploys, and what the platform costs, each line tagged \
                          billed (Cloudflare's invoice data) or estimated (list price or a \
                          gateway's estimate). Sources that failed are listed.",
            input_schema: json!({
                "type": "object",
                "properties": { "hours": { "type": "integer", "description": "Window, 1 to 168; 24 by default." } }
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != "observability_summary" {
                return Err(crate::unknown(tool));
            }
            let hours = args["hours"].as_u64().unwrap_or(24).clamp(1, 168);
            summary(self, cx, hours).await
        })
    }
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

    let metrics = match metrics_v {
        Ok(v) => fold_invocations(&v),
        Err(why) => {
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
        entry["deploys"] = json!(
            deploys
                .iter()
                .filter(|d| d["worker"] == w.name.as_str() || d["worker"] == w.script.as_str())
                .collect::<Vec<_>>()
        );
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
                    "AI Gateway; the log listing was cut short, so this is a lower bound"
                } else {
                    "AI Gateway"
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
                "AI agents Worker; this is model use already counted in the Models line, so it is not added to either total",
                Some(usd),
                json!({ "runs": count, "listed": v["runs"].as_array().map_or(0, Vec::len) }),
            )),
            Err(why) => unavailable.push(missing("AI agent run spend", &why)),
        },
        Err(e) => unavailable.push(missing("AI agent run spend", &hide(e))),
    }

    // CI Containers time: seconds only, since the class sizes are not readable from a pure
    // function in the CI crate (they sit inside the connector's method).
    match ci_v {
        Ok(v) => match fold_ci_jobs(&v, from as i64, to as i64) {
            Ok((seconds, listed)) => cost.push(line(
                "CI Containers",
                "estimated",
                if listed >= 200 {
                    "CI scheduler; 200 jobs came back, so older jobs in the window are missing. Seconds only: class sizes are not readable as a pure function, so no price is given"
                } else {
                    "CI scheduler. Seconds only: class sizes are not readable as a pure function, so no price is given"
                },
                None,
                json!({ "seconds_by_class": seconds, "jobs_listed": listed, "truncated": listed >= 200 }),
            )),
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

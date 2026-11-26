//! A read-only metrics view over an instance's approved Workers: a linked instance's one
//! approved Worker, or, for the home instance, every Worker the platform deploys (the lane map,
//! ci/workers.toml) besides its own. One GraphQL query and one analytics token per call.

use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{Policy, Scope, Scoped};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::Context;
use crate::instances::record::{AccountRef, Instance, Toggle, label};
use crate::instances::tools::View;

pub const METRICS: &str = "observability_worker_metrics";

/// The approved instance's Worker is the sole metrics workload.
pub fn approved_worker(instance: &Instance) -> std::result::Result<&str, String> {
    if !instance.observability.is_some_and(Toggle::enabled) {
        return Err("observability metrics are disabled for this instance".into());
    }
    let worker = instance
        .worker
        .as_deref()
        .filter(|name| label(name))
        .ok_or("observability metrics require an approved Worker")?;
    if matches!(instance.account, AccountRef::Linked { account_id: None }) {
        return Err("observability metrics require an approved linked account".into());
    }
    Ok(worker)
}

/// Record validation only requires a workload when the feature is enabled.
pub fn validate(instance: &Instance) -> std::result::Result<(), String> {
    if instance.observability.is_some_and(Toggle::enabled) {
        approved_worker(instance)?;
    }
    Ok(())
}

const SCOPED_TOOLS: &[crate::instances::tools::Registration] =
    &[crate::instances::tools::Registration {
        name: METRICS,
        description: None,
        arguments: &["hours"],
    }];

/// The Metrics area's view. Errors joins once an instance-scoped errors read exists.
const METRICS_VIEW: View = View {
    area: "metrics",
    id: "workers",
    title: "Workers",
    order: 10,
    since: None,
};

/// A Cost area view: `cost_daily` over one of its ranges.
const fn cost_view(id: &'static str, title: &'static str, order: u16) -> View {
    View {
        area: "cost",
        id,
        title,
        order,
        since: None,
    }
}

/// The views of an instance whose record enables observability.
const VIEWS: &[View] = &[METRICS_VIEW];

/// The home instance's views when its record enables observability: Metrics, and Cost over
/// `cost_daily`'s ranges. `cost_daily` reads the platform's spend as a whole, so only home shows
/// it; a linked instance's own cost is a later step (docs/plans/capability-scoping.md, M4).
const HOME_VIEWS: &[View] = &[
    METRICS_VIEW,
    cost_view("7d", "7 days", 10),
    cost_view("today", "Today", 20),
    cost_view("all", "30 days", 30),
];

/// What the record's `observability` toggle adds: Metrics over its approved Worker, and on home
/// also Cost. Nothing while it is off.
pub fn instance_tools(instance: &Instance) -> serde_json::Result<crate::instances::tools::Enabled> {
    let views = if instance.account == AccountRef::Home {
        HOME_VIEWS
    } else {
        VIEWS
    };
    Ok(crate::instances::tools::Enabled::when(
        approved_worker(instance).is_ok(),
        SCOPED_TOOLS,
        views,
    ))
}

pub(super) fn tool() -> Tool {
    Tool {
        name: METRICS,
        description: concat!(
            "Read request counts, errors, status counts, CPU and wall time, and an estimated ",
            "list-price usage cost for the approved Worker: on the home instance, for every ",
            "platform Worker, per Worker (`workers`) and in total (`metrics`). Excludes logs, ",
            "billing and token capacity."
        ),
        input_schema: json!({
            "type": "object",
            "properties": {
                "instance": { "type": "string", "description": "Approved instance; supplied automatically on its scoped endpoint." },
                "hours": { "type": "integer", "minimum": 1, "maximum": 168, "description": "Window size in hours; defaults to 24." }
            },
            "required": ["instance"],
            "additionalProperties": false
        }),
    }
}

/// The Workers a metrics call reads for `instance`: its approved Worker, and on the home
/// instance also every platform Worker in `platform` (the lane map's scripts, in its order).
/// A linked instance reads only its own Worker, whatever `platform` names.
pub fn metrics_scripts(
    instance: &Instance,
    platform: &[&str],
) -> std::result::Result<Vec<String>, String> {
    let own = approved_worker(instance)?;
    let mut out = vec![own.to_string()];
    if instance.account == AccountRef::Home {
        for script in platform {
            if label(script) && !out.iter().any(|s| s == script) {
                out.push((*script).to_string());
            }
        }
    }
    Ok(out)
}

/// Select the Workers in the provider request as well as checking the response: one query
/// whose `scriptName_in` filter names every script.
pub fn query(account: &str, scripts: &[String], from: &str, to: &str) -> Value {
    json!({
        "query": concat!(
            "query($tag: string, $scripts: [string!], $from: Time, $to: Time) { viewer { accounts(filter: ",
            "{accountTag: $tag}) { workersInvocationsAdaptive(limit: 10000, filter: ",
            "{scriptName_in: $scripts, datetime_geq: $from, datetime_leq: $to}) { sum { requests errors subrequests } ",
            "quantiles { cpuTimeP50 cpuTimeP99 wallTimeP50 wallTimeP99 } ",
            "dimensions { scriptName status datetimeHour } } } } }"
        ),
        "variables": { "tag": account, "scripts": scripts, "from": from, "to": to }
    })
}

/// What one metrics call asks of Cloudflare: the Workers, the one GraphQL body that covers
/// them all, and the policies of the one analytics token it mints.
pub struct MetricsRequest {
    pub scripts: Vec<String>,
    pub body: Value,
    pub policies: Vec<Policy>,
}

/// The request for `instance`'s metrics in `account` between the ISO times `from` and `to`.
pub fn metrics_request(
    instance: &Instance,
    platform: &[&str],
    account: &str,
    from: &str,
    to: &str,
) -> std::result::Result<MetricsRequest, String> {
    let scripts = metrics_scripts(instance, platform)?;
    let body = query(account, &scripts, from, to);
    let policies = vec![Policy::new(Scope::Account, &[super::ANALYTICS_PERMISSION])];
    Ok(MetricsRequest {
        scripts,
        body,
        policies,
    })
}

const BASIS: &str =
    "Workers list price before included usage, with CPU estimated from the request-weighted median";

/// Reject provider responses missing the metrics rows. Ignore rows for all other scripts.
/// `metrics` and `estimated_cost` are the totals over `scripts` (for one Worker, that Worker's);
/// `workers` has one row per script, in `scripts`' order, zero when it had no requests.
pub fn worker_result(answer: &Value, scripts: &[String]) -> std::result::Result<Value, String> {
    let Some(rows) =
        answer["data"]["viewer"]["accounts"][0]["workersInvocationsAdaptive"].as_array()
    else {
        return Err("Cloudflare did not return Worker metrics for this account".into());
    };
    let mut folded = super::fold_invocations(answer);
    let workers: Vec<Value> = scripts
        .iter()
        .map(|script| {
            let w = folded.remove(script).unwrap_or_default();
            json!({
                "worker": script,
                "metrics": w.public(),
                "estimated_cost": { "usd": super::workers_estimate(w.requests, w.cpu_ms_estimate()) },
            })
        })
        .collect();
    // The totals fold the approved scripts' rows as though they were one script's.
    let total: Vec<Value> = rows
        .iter()
        .filter(|row| {
            row["dimensions"]["scriptName"]
                .as_str()
                .is_some_and(|name| scripts.iter().any(|s| s == name))
        })
        .map(|row| {
            let mut row = row.clone();
            row["dimensions"]["scriptName"] = json!("");
            row
        })
        .collect();
    let total = super::fold_invocations(
        &json!({"data": {"viewer": {"accounts": [{"workersInvocationsAdaptive": total}]}}}),
    )
    .remove("")
    .unwrap_or_default();
    Ok(json!({
        "metrics": total.public(),
        "estimated_cost": {
            "usd": super::workers_estimate(total.requests, total.cpu_ms_estimate()),
            "basis": BASIS,
        },
        "workers": workers,
    }))
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The instance and window of a metrics call whose arguments are only `instance` and `hours`:
/// a caller cannot name a `worker`, account or other workload.
pub fn metrics_arguments(args: &Value) -> Result<(&str, u64)> {
    if !args.as_object().is_some_and(|fields| {
        fields
            .keys()
            .all(|name| matches!(name.as_str(), "instance" | "hours"))
    }) {
        return Err(bad("only instance-bound metrics arguments are accepted"));
    }
    let instance = crate::arg(args, "instance")?;
    let hours = match args.get("hours") {
        None => 24,
        Some(value) => value
            .as_u64()
            .filter(|hours| (1..=168).contains(hours))
            .ok_or_else(|| bad("hours must be an integer from 1 to 168"))?,
    };
    Ok((instance, hours))
}

pub(super) async fn metrics(cx: &Context<'_>, platform: &[&str], args: &Value) -> Result<Value> {
    let (instance, hours) = metrics_arguments(args)?;
    let record = crate::instances::one(cx.env, instance)
        .await?
        .ok_or_else(|| bad("unknown observability instance"))?;
    let script = approved_worker(&record).map_err(bad)?.to_string();

    let linked;
    let account = match &record.account {
        AccountRef::Home => cx.account,
        AccountRef::Linked { account_id } => {
            let host = cx.env.var("PLATFORM_HOST")?.to_string();
            linked = super::within(crate::linked::account_for(cx.env, &host, instance))
                .await
                .map_err(|_| bad("the approved linked account is unavailable"))?;
            if account_id.as_deref() != Some(linked.id.as_str()) {
                return Err(bad("the linked account differs from the approved account"));
            }
            &linked
        }
    };

    let to = worker::js_sys::Date::now();
    let from = to - hours as f64 * 3_600_000.0;
    let from = super::iso(from);
    let to = super::iso(to);
    let request = metrics_request(&record, platform, &account.id, &from, &to).map_err(bad)?;
    // One token and one query for every Worker of the call.
    let token = Scoped::open_for(account, "observability", request.policies, super::TOKEN_TTL)
        .await
        .map_err(|_| bad("an analytics credential is unavailable for the approved account"))?;
    let result = super::within(super::graphql(token.value(), &request.body)).await;
    let result = token
        .finish(result)
        .await
        .map_err(|_| bad("Cloudflare Worker metrics are unavailable for the approved account"))?;
    let mut result = worker_result(&result, &request.scripts).map_err(bad)?;
    result["instance"] = json!(record.id);
    result["worker"] = json!(script);
    result["scope"] = json!(if request.scripts.len() > 1 {
        "platform"
    } else {
        "worker"
    });
    result["from"] = json!(from);
    result["to"] = json!(to);
    Ok(result)
}

//! A read-only metrics view over one explicitly approved instance Worker.

use pigeon_core::mcp::Tool;
use pigeon_core::tokens::{Policy, Scope, Scoped};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::Context;
use crate::instances::record::{AccountRef, Instance, label};

pub const METRICS: &str = "observability_worker_metrics";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled,
    Disabled,
}

impl Profile {
    pub fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// The approved instance's Worker is the sole metrics workload.
pub fn approved_worker(instance: &Instance) -> std::result::Result<&str, String> {
    if !instance.observability.is_some_and(Profile::enabled) {
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
    if instance.observability.is_some_and(Profile::enabled) {
        approved_worker(instance)?;
    }
    Ok(())
}

const SCOPED_TOOLS: &[crate::instances::tools::Registration] =
    &[crate::instances::tools::Registration {
        name: METRICS,
        description: None,
        hidden: &[],
        arguments: &["hours"],
    }];

pub fn instance_tools(
    instance: &Instance,
) -> serde_json::Result<&'static [crate::instances::tools::Registration]> {
    Ok(if approved_worker(instance).is_ok() {
        SCOPED_TOOLS
    } else {
        &[]
    })
}

pub(super) fn tool() -> Tool {
    Tool {
        name: METRICS,
        description: concat!(
            "Read request counts, errors, status counts, CPU and wall time, and an estimated ",
            "list-price usage cost for the approved Worker. Excludes logs, billing and token capacity."
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

/// Select the Worker in the provider request as well as checking the response.
pub fn query(account: &str, script: &str, from: &str, to: &str) -> Value {
    json!({
        "query": concat!(
            "query($tag: string, $script: string, $from: Time, $to: Time) { viewer { accounts(filter: ",
            "{accountTag: $tag}) { workersInvocationsAdaptive(limit: 10000, filter: ",
            "{scriptName: $script, datetime_geq: $from, datetime_leq: $to}) { sum { requests errors subrequests } ",
            "quantiles { cpuTimeP50 cpuTimeP99 wallTimeP50 wallTimeP99 } ",
            "dimensions { scriptName status datetimeHour } } } } }"
        ),
        "variables": { "tag": account, "script": script, "from": from, "to": to }
    })
}

/// Reject provider responses missing the metrics rows. Ignore rows for all other scripts.
pub fn worker_result(answer: &Value, script: &str) -> std::result::Result<Value, String> {
    if !answer["data"]["viewer"]["accounts"][0]["workersInvocationsAdaptive"].is_array() {
        return Err("Cloudflare did not return Worker metrics for this account".into());
    }
    let worker = super::fold_invocations(answer)
        .remove(script)
        .unwrap_or_default();
    Ok(json!({
        "metrics": worker.public(),
        "estimated_cost": {
            "usd": super::workers_estimate(worker.requests, worker.cpu_ms_estimate()),
            "basis": "Workers list price before included usage, with CPU estimated from the request-weighted median",
        }
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

pub(super) async fn metrics(cx: &Context<'_>, args: &Value) -> Result<Value> {
    let (instance, hours) = metrics_arguments(args)?;
    let record = crate::instances::one(cx.env, instance)
        .await?
        .ok_or_else(|| bad("unknown observability instance"))?;
    let script = approved_worker(&record).map_err(bad)?;

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
    let body = query(&account.id, script, &from, &to);
    let policy = Policy::new(Scope::Account, &[super::ANALYTICS_PERMISSION]);
    let token = Scoped::open_for(account, "observability", vec![policy], super::TOKEN_TTL)
        .await
        .map_err(|_| bad("an analytics credential is unavailable for the approved account"))?;
    let result = super::within(super::graphql(token.value(), &body)).await;
    let result = token
        .finish(result)
        .await
        .map_err(|_| bad("Cloudflare Worker metrics are unavailable for the approved account"))?;
    let mut result = worker_result(&result, script).map_err(bad)?;
    result["instance"] = json!(instance);
    result["worker"] = json!(script);
    result["from"] = json!(from);
    result["to"] = json!(to);
    Ok(result)
}

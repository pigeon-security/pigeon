//! The cloud capability's tools and the one function that runs them. Reads answer at once. A
//! plan tool reads the provider, plans, and creates an approval; `cloud_apply` makes the change
//! once the person approved it, after checking the provider has not moved.

use pigeon_contract::ToolSpec;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::approvals::KIND;
use crate::iam::strip_secrets;
use crate::provider::{Action, PlanPayload, Ports, Provider, differences};
use crate::redact::redact;
use crate::scaleway::Scaleway;

/// Every tool the capability offers.
pub const TOOLS: [&str; 10] = [
    "cloud_status",
    "cloud_tokens_list",
    "cloud_tokens_plan_create",
    "cloud_tokens_plan_delete",
    "cloud_buckets_list",
    "cloud_objects_list",
    "cloud_object_head",
    "cloud_buckets_plan_create",
    "cloud_buckets_plan_delete",
    "cloud_apply",
];

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The providers the gateway has: Scaleway.
fn providers() -> Vec<Box<dyn Provider>> {
    vec![Box::new(Scaleway)]
}

/// The tools as the manifest lists them, each with its JSON Schema and a plain description.
pub fn tools() -> Vec<ToolSpec> {
    let region = json!({ "type": "string", "enum": ["fr-par", "nl-ams", "pl-waw"] });
    let spec = |name: &str, description: &str, schema: Value| ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: schema,
    };
    let none = json!({ "type": "object", "properties": {} });
    vec![
        spec(
            "cloud_status",
            "Which cloud providers are set up and ready, and why not: each secret slot bound \
             and set (never its value), the hosts the gateway calls, and, when ready, a live \
             check of the gateway's own key.",
            none,
        ),
        spec(
            "cloud_tokens_list",
            "Scaleway API keys of the organization, with their application and policies. A \
             key's secret is never shown.",
            json!({
                "type": "object",
                "properties": {
                    "page": { "type": "integer", "minimum": 1 },
                    "page_size": { "type": "integer", "minimum": 1, "maximum": 100, "description": "50 by default." },
                    "application_id": { "type": "string", "description": "Only this application's keys." }
                }
            }),
        ),
        spec(
            "cloud_tokens_plan_create",
            "Plans a new Scaleway API key: an IAM application `pigeon-<name>`, a policy with \
             Object Storage permission sets on named projects, and a key that expires. Changes \
             nothing: answers the plan and an approval id for the person's passkey, then call \
             cloud_apply. The key's secret is written to the Secrets Store secret `slot` as \
             ACCESS_KEY:SECRET_KEY and never shown.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "1 to 50 of a-z, 0-9 and `-`." },
                    "permission_sets": {
                        "type": "array",
                        "items": { "type": "string", "enum": crate::scaleway::PERMISSION_SETS },
                        "minItems": 1
                    },
                    "project_ids": { "type": "array", "items": { "type": "string" }, "description": "Project IDs; the default project when none. The first is the key's default project." },
                    "expires_days": { "type": "integer", "minimum": 1, "maximum": 365, "description": "30 by default." },
                    "slot": { "type": "string", "description": "A new secret's name: CLOUD_ and 1 to 56 of A-Z, 0-9, _." }
                },
                "required": ["name", "permission_sets", "slot"]
            }),
        ),
        spec(
            "cloud_tokens_plan_delete",
            "Plans deleting one Scaleway API key (never the gateway's own). With \
             with_application it also deletes the key's application and policies, only when \
             the application is tagged `pigeon` and holds no other key. Changes nothing: \
             answers the plan and an approval id; then call cloud_apply.",
            json!({
                "type": "object",
                "properties": {
                    "access_key": { "type": "string" },
                    "with_application": { "type": "boolean" }
                },
                "required": ["access_key"]
            }),
        ),
        spec(
            "cloud_buckets_list",
            "Object Storage buckets, in one region or in all three.",
            json!({ "type": "object", "properties": { "region": region.clone() } }),
        ),
        spec(
            "cloud_objects_list",
            "One page of a bucket's objects. Pass the answer's next_token as token for the next page.",
            json!({
                "type": "object",
                "properties": {
                    "region": region.clone(),
                    "bucket": { "type": "string" },
                    "prefix": { "type": "string" },
                    "delimiter": { "type": "string" },
                    "max_keys": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "100 by default." },
                    "token": { "type": "string" }
                },
                "required": ["region", "bucket"]
            }),
        ),
        spec(
            "cloud_object_head",
            "One object's size, type, ETag and time, without its content.",
            json!({
                "type": "object",
                "properties": { "region": region.clone(), "bucket": { "type": "string" }, "key": { "type": "string" } },
                "required": ["region", "bucket", "key"]
            }),
        ),
        spec(
            "cloud_buckets_plan_create",
            "Plans a new private bucket. Changes nothing: answers the plan and an approval id; \
             then call cloud_apply.",
            json!({
                "type": "object",
                "properties": { "region": region.clone(), "bucket": { "type": "string" } },
                "required": ["region", "bucket"]
            }),
        ),
        spec(
            "cloud_buckets_plan_delete",
            "Plans deleting an empty bucket (old versions and delete markers count as content). \
             Changes nothing: answers the plan and an approval id; then call cloud_apply.",
            json!({
                "type": "object",
                "properties": { "region": region, "bucket": { "type": "string" } },
                "required": ["region", "bucket"]
            }),
        ),
        spec(
            "cloud_apply",
            "Makes a planned change once the person approved it with their passkey on the \
             Approvals tab. It reads the provider again first and refuses, naming what moved, \
             if the plan no longer holds. It reports any difference from what the plan expected.",
            json!({
                "type": "object",
                "properties": { "plan": { "type": "string", "description": "The approval id a plan tool answered." } },
                "required": ["plan"]
            }),
        ),
    ]
}

/// Runs `tool` with `args`. Every provider error is redacted, and no `secret_key` field is in
/// the answer.
pub async fn run_tool(ports: &Ports<'_>, tool: &str, args: &Value) -> Result<Value> {
    match dispatch(ports, tool, args).await {
        Ok(mut answer) => {
            strip_secrets(&mut answer);
            Ok(answer)
        }
        Err(e) => Err(bad(redact(&e.to_string(), &[]))),
    }
}

async fn dispatch(ports: &Ports<'_>, tool: &str, args: &Value) -> Result<Value> {
    let all = providers();
    if tool == "cloud_status" {
        let mut out = Vec::new();
        for p in &all {
            out.push(p.status(ports).await?);
        }
        return Ok(json!({ "providers": out }));
    }
    if !TOOLS.contains(&tool) {
        return Err(bad(format!("no tool `{tool}`")));
    }
    // Every other tool says first what is missing.
    let provider = all
        .first()
        .ok_or_else(|| bad("no cloud provider"))?
        .as_ref();
    provider.ready(ports).await?;
    match tool {
        "cloud_tokens_list" => provider.list(ports, "tokens", args).await,
        "cloud_buckets_list" => provider.list(ports, "buckets", args).await,
        "cloud_objects_list" => provider.list(ports, "objects", args).await,
        "cloud_object_head" => provider.list(ports, "object", args).await,
        "cloud_tokens_plan_create" => {
            plan_tool(ports, provider, "tokens", Action::Create, args).await
        }
        "cloud_tokens_plan_delete" => {
            plan_tool(ports, provider, "tokens", Action::Delete, args).await
        }
        "cloud_buckets_plan_create" => {
            plan_tool(ports, provider, "buckets", Action::Create, args).await
        }
        "cloud_buckets_plan_delete" => {
            plan_tool(ports, provider, "buckets", Action::Delete, args).await
        }
        _ => apply_tool(ports, &all, args).await,
    }
}

/// Plans, and asks core for an approval of the plan.
async fn plan_tool(
    ports: &Ports<'_>,
    provider: &dyn Provider,
    kind: &str,
    action: Action,
    args: &Value,
) -> Result<Value> {
    let plan = provider.plan(ports, kind, action, args).await?;
    let payload = plan.payload();
    let created = ports
        .approvals
        .create(&payload.subject(), &payload.to_value(), &plan.lines)
        .await?;
    Ok(json!({
        "plan": plan.lines,
        "args": plan.args,
        "approval": created.id,
        "existing": created.existing,
        "expires_ms": created.expires_ms,
        "next": format!(
            "Nothing has changed. The person approves it on the Approvals tab with their passkey; \
             then call cloud_apply with plan `{}`.",
            created.id
        ),
    }))
}

/// Applies an approved plan: re-reads, refuses on drift without spending the approval, then
/// takes the approval and acts.
async fn apply_tool(ports: &Ports<'_>, all: &[Box<dyn Provider>], args: &Value) -> Result<Value> {
    let id = args["plan"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("`plan` is required: the approval id a plan tool answered"))?;
    let record = ports.approvals.get(id).await?;
    if record.kind != KIND {
        return Err(bad("that approval is not a cloud change"));
    }
    let payload: PlanPayload = serde_json::from_value(record.payload.clone())
        .map_err(|_| bad("the approval does not hold a cloud plan"))?;
    if record.subject != payload.subject() {
        return Err(bad("the approval's subject does not match its plan"));
    }
    match record.state.as_str() {
        "approved" => {}
        "pending" => {
            return Err(bad(
                "the plan is waiting for the person: they approve it on the Approvals tab with \
                 their passkey",
            ));
        }
        other => return Err(bad(format!("the plan is {other}: plan again"))),
    }
    let provider = all
        .iter()
        .find(|p| p.name() == payload.provider)
        .ok_or_else(|| bad(format!("no provider `{}`", payload.provider)))?;
    // Re-read and re-plan. A plan that no longer holds, or holds differently, is not applied.
    let replan = provider
        .plan(ports, &payload.kind, payload.action, &payload.args)
        .await
        .map_err(|e| {
            bad(format!(
                "the plan no longer holds ({e}). Nothing was changed: plan again"
            ))
        })?;
    let moved = differences(&payload.to_value(), &replan.payload().to_value());
    if !moved.is_empty() {
        return Err(bad(format!(
            "the provider changed since the plan: {}. Nothing was changed: plan again",
            moved.join("; ")
        )));
    }
    ports
        .approvals
        .take(id, &payload.subject(), &payload.to_value())
        .await?;
    provider.apply(ports, &replan).await
}

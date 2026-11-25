//! Read authority issued by core for one approved instance's CI jobs.

use pigeon_contract::WorkerContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::{Cx, PLATFORM, random};

/// A scope resolved from the approved instance record, never from tool arguments.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub instance: String,
    pub repo: String,
    pub jobs: Vec<String>,
}

impl Scope {
    /// Legacy jobs carry no owner. A repository match alone never grants access.
    pub fn allows(&self, owner: Option<&str>, repo: &str, job: &str) -> bool {
        owner == Some(self.instance.as_str())
            && repo == self.repo
            && self.jobs.iter().any(|name| name == job)
    }

    fn valid(&self) -> bool {
        !self.instance.is_empty()
            && !self.repo.is_empty()
            && !self.jobs.is_empty()
            && self.jobs.iter().all(|job| !job.is_empty())
    }
}

async fn lookup(cx: &Cx<'_>, body: Value, context: Option<WorkerContext>) -> Result<Option<Scope>> {
    let (Some(key), Some(caller)) = (cx.key, cx.caller) else {
        return Err(Error::RustError("CI scope requires a signed caller".into()));
    };
    let fetcher = cx.env.service(PLATFORM)?;
    let nonce = random()?;
    // A contextual rejection is final. Do not retry a scoped request with a legacy envelope.
    let answer = if let Some(context) = context {
        pigeon_contract::ask_with_context(
            &fetcher,
            "https://core",
            "ci_scope",
            &body,
            (key, caller),
            &nonce,
            &context,
        )
        .await?
    } else {
        pigeon_contract::ask(
            &fetcher,
            "https://core",
            "ci_scope",
            &body,
            Some((key, caller)),
            &nonce,
        )
        .await?
    };
    match answer {
        pigeon_contract::Answer::Ok(v) => {
            let scope: Option<Scope> = serde_json::from_value(v)?;
            if scope.as_ref().is_some_and(|scope| !scope.valid()) {
                return Err(Error::RustError("core returned an invalid CI scope".into()));
            }
            Ok(scope)
        }
        pigeon_contract::Answer::Error(_) => Err(Error::RustError(
            "could not resolve the approved CI scope".into(),
        )),
    }
}

/// Carry scope only when the inbound gateway request authenticated it.
pub fn read_context(instance: &str, signed: Option<&str>) -> Result<Option<WorkerContext>> {
    match signed {
        Some(signed) if signed != instance => Err(Error::RustError(
            "the CI request does not match its signed instance".into(),
        )),
        Some(signed) => Ok(Some(WorkerContext::new("pigeon-platform", Some(signed)))),
        None => Ok(None),
    }
}

pub(crate) async fn for_read(cx: &Cx<'_>, instance: &str, signed: Option<&str>) -> Result<Scope> {
    let context = read_context(instance, signed)?;
    lookup(cx, json!({ "instance": instance }), context)
        .await?
        .filter(|scope| scope.instance == instance)
        .ok_or_else(|| Error::RustError("CI is unavailable on this instance".into()))
}

/// Existing main-platform submissions still run when attribution is unavailable.
pub(crate) async fn owner_for(cx: &Cx<'_>, repo: &str, job: &str) -> Option<String> {
    let scope = lookup(cx, json!({ "repo": repo, "job": job }), None)
        .await
        .ok()??;
    scope
        .allows(Some(scope.instance.as_str()), repo, job)
        .then_some(scope.instance)
}

/// Accept only the filter fields that belong to these read tools.
pub fn read_args(tool: &str, args: &Value) -> Result<Value> {
    let keys = match tool {
        "ci_status" => &["id"][..],
        "ci_jobs" => &["commit", "state", "task", "caller", "limit"][..],
        _ => {
            return Err(Error::RustError(
                "tool is unavailable on this instance".into(),
            ));
        }
    };
    let fields = args
        .as_object()
        .ok_or_else(|| Error::RustError("expected CI tool arguments".into()))?;
    if fields
        .keys()
        .any(|name| name != "instance" && !keys.contains(&name.as_str()))
    {
        return Err(Error::RustError("CI scope is fixed by the instance".into()));
    }
    let mut query = fields.clone();
    query.remove("instance");
    Ok(Value::Object(query))
}

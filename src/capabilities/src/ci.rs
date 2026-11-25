//! CI, served by core from the `pigeon-ci` crate: a thin [`Capability`] over [`pigeon_ci::Ci`],
//! so core keeps serving CI's tools and `/ci/` routes itself until it binds `pigeon-ci-gateway`
//! (docs/plans/workers-split.md, phase 4). Core hosts the scheduler and the runners, and acts
//! with the parent token ([`pigeon_ci::Accounts::Parent`]).

pub use pigeon_ci::{
    BINDING, BUILD, Ci, EVENTS, EVENTS_QUEUE, OBJECT, job_grant, job_status, same_build, setup,
    setup_summary, token,
};

use pigeon_core::mcp::Tool;
use serde_json::Value;
use worker::Request;

use crate::expiry::Expiry;
use crate::{Call, Capability, Context, Expiring, Footprint, Serve, Slot};

/// Approved job names for read-only CI on the instance's home Artifacts repository.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled { jobs: Vec<String> },
    Disabled,
}

type Record = crate::instances::record::Instance;

pub fn scope(instance: &Record) -> std::result::Result<Option<(String, &[String])>, String> {
    use crate::instances::record::{AccountRef, RepoRef};
    let Some(Profile::Enabled { jobs }) = &instance.ci else {
        return Ok(None);
    };
    let (AccountRef::Home, Some(RepoRef::Artifacts { namespace, repo })) =
        (&instance.account, &instance.repo)
    else {
        return Err("CI requires a home-account Artifacts repository".into());
    };
    if jobs.is_empty() || jobs.len() > 64 {
        return Err("CI requires between 1 and 64 approved job names".into());
    }
    let mut unique = std::collections::BTreeSet::new();
    for job in jobs {
        pigeon_ci::config::job_name(job)?;
        if !unique.insert(job) {
            return Err("CI job names must be unique".into());
        }
    }
    Ok(Some((format!("{namespace}/{repo}"), jobs)))
}

#[derive(serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ScopeRequest {
    Read { instance: String },
    Attribute { repo: String, job: String },
}

pub fn resolve_scope(records: &[Record], request: &ScopeRequest) -> Value {
    let mut resolved = records.iter().filter_map(|record| {
        let (repo, jobs) = scope(record).ok().flatten()?;
        let matches = match request {
            ScopeRequest::Read { instance } => &record.id == instance,
            ScopeRequest::Attribute {
                repo: requested,
                job,
            } => &repo == requested && jobs.contains(job),
        };
        matches.then(|| serde_json::json!({ "instance": record.id, "repo": repo, "jobs": jobs }))
    });
    match (resolved.next(), resolved.next()) {
        (Some(scope), None) => scope,
        _ => Value::Null,
    }
}

pub async fn instance_scope(env: &worker::Env, request: &ScopeRequest) -> worker::Result<Value> {
    Ok(resolve_scope(&crate::instances::all(env).await?, request))
}

const SCOPED_TOOLS: &[crate::instances::tools::Registration] = &[
    crate::instances::tools::Registration {
        name: "ci_jobs",
        description: Some("List recent CI jobs owned by this instance."),
        hidden: &["repo"],
        arguments: &["commit", "state", "task", "caller", "limit"],
    },
    crate::instances::tools::Registration {
        name: "ci_status",
        description: Some("Read the status and results of a CI job owned by this instance."),
        hidden: &[],
        arguments: &["id"],
    },
];

/// The explicit instance on a read from the global connector, when one was supplied.
pub fn connector_read_instance<'a>(tool: &str, args: &'a Value) -> worker::Result<Option<&'a str>> {
    if args.get("instance").is_none() || !SCOPED_TOOLS.iter().any(|entry| entry.name == tool) {
        return Ok(None);
    }
    pigeon_ci::scope::read_args(tool, args)?;
    crate::arg(args, "instance").map(Some)
}

/// Derive the callback boundary from an approved profile, using the same checks as CI discovery.
pub(crate) async fn approved_connector_instance<'a>(
    env: &worker::Env,
    tool: &str,
    args: &'a Value,
) -> worker::Result<Option<&'a str>> {
    let Some(id) = connector_read_instance(tool, args)? else {
        return Ok(None);
    };
    let unavailable = || worker::Error::RustError("CI is unavailable on this instance".into());
    let record = crate::instances::one(env, id)
        .await?
        .ok_or_else(unavailable)?;
    if scope(&record).map_err(worker::Error::RustError)?.is_none() {
        return Err(unavailable());
    }
    Ok(Some(id))
}

pub fn instance_tools(
    instance: &Record,
) -> serde_json::Result<&'static [crate::instances::tools::Registration]> {
    Ok(if scope(instance).ok().flatten().is_some() {
        SCOPED_TOOLS
    } else {
        &[]
    })
}

/// CI as one of core's capabilities.
pub struct Local {
    pub ci: Ci,
}

/// The caller as JSON, as `pigeon_ci` reads it.
fn caller_json(cx: &Context<'_>) -> Option<Value> {
    cx.caller.and_then(|c| serde_json::to_value(c).ok())
}

impl Capability for Local {
    fn name(&self) -> &'static str {
        Ci::NAME
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        pigeon_ci::versions()
    }

    fn tools(&self) -> Vec<Tool> {
        self.ci.tools()
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            let caller = caller_json(cx);
            let ccx = self.ci.cx(cx.env, caller.as_ref(), None);
            self.ci.call(&ccx, tool, args).await
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec![pigeon_ci::ROUTE]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.ci.serve(cx.env, req))
    }

    fn secrets(&self) -> Vec<Slot> {
        self.ci
            .secrets()
            .into_iter()
            .map(|s| Slot {
                name: s.name,
                purpose: s.purpose,
                scopes: crate::remote::intern_scopes(&s.scopes),
            })
            .collect()
    }

    /// The connector that declares `name` answers.
    fn secret_expiry<'a>(&'a self, cx: &'a Context<'a>, name: &'a str) -> Expiring<'a> {
        Box::pin(async move {
            let ccx = self.ci.cx(cx.env, None, None);
            let expiry: Expiry = self.ci.secret_expiry(&ccx, name).await?;
            Ok(expiry)
        })
    }

    fn footprint(&self) -> Footprint {
        serde_json::from_value(self.ci.footprint()).unwrap_or_default()
    }
}

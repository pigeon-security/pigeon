//! pigeon's capabilities: the features a Worker hosts, each an end result built from the
//! building blocks in `pigeon-core`. Every capability answers the same small interface: its MCP
//! tools and how to call one, the events it acts on, and its footprint (the hostnames, Workers
//! and resources it brings), which the architecture page draws.

pub mod approvals;
pub mod architecture;
pub mod cf_proxy;
pub mod ci;
pub mod console;
pub mod deploys;
pub mod dispatch;
pub mod expiry;
pub mod git_proxy;
pub mod github;
pub mod images;
pub mod instances;
pub mod land;
pub mod linked;
pub mod mirror;
pub mod models;
pub mod notify;
pub mod oauth;
pub mod passkeys;
pub mod policy;
pub mod protected_host;
pub mod proxy;
pub mod public_host;
pub mod remote;
pub mod secrets;
pub mod site;
pub mod source;
pub mod split;
pub mod users;

use std::future::Future;
use std::pin::Pin;

use pigeon_core::Account;
use pigeon_core::mcp::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::{Env, Error, Request, Response, Result};

/// A tool call or event in flight.
pub type Call<'a> = Pin<Box<dyn Future<Output = Result<Value>> + 'a>>;

/// A secret's expiry being asked of its issuer.
pub type Expiring<'a> = Pin<Box<dyn Future<Output = Result<expiry::Expiry>> + 'a>>;

/// A request a capability serves, in flight.
pub type Serve<'a> = Pin<Box<dyn Future<Output = Result<Response>> + 'a>>;

/// What a capability acts with: the account, the hosting Worker's environment, and the other
/// capabilities hosted beside it.
pub struct Context<'a> {
    pub account: &'a Account,
    pub env: &'a Env,
    pub hosted: &'a Hosted<'a>,
    /// Who is calling, when the request says: a service user through an Access service token.
    pub caller: Option<&'a users::Caller>,
    /// Why there is no caller, when there is none.
    pub caller_note: Option<&'a str>,
    /// The Worker's context for the request being served, to finish work after the answer
    /// has gone, such as revoking a token once a streamed body ends. `None` outside `fetch`.
    pub worker: Option<&'a worker::Context>,
}

/// A Worker and the capabilities it hosts, with the repository's sources it was built from.
pub struct Hosted<'a> {
    pub worker: &'a str,
    pub capabilities: &'a [Box<dyn Capability>],
    /// By path from the repository root.
    pub sources: &'a [(&'a str, &'a str)],
}

/// A feature a Worker hosts.
pub trait Capability {
    /// Its module's name, such as `protected_host`.
    fn name(&self) -> &'static str;

    /// The MCP tools it offers.
    fn tools(&self) -> Vec<Tool>;

    /// Runs `tool`, one of its own.
    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a>;

    /// Whether it acts on queue message `event`.
    fn wants(&self, _event: &Value) -> bool {
        false
    }

    /// Acts on the events it wants, once per batch.
    fn on_events<'a>(&'a self, _cx: &'a Context<'a>) -> Call<'a> {
        Box::pin(async { Ok(Value::Null) })
    }

    /// The path prefixes it serves over HTTP, such as `/auth/`.
    fn routes(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Serves a request under one of its routes.
    fn fetch<'a>(&'a self, _cx: &'a Context<'a>, _req: Request) -> Serve<'a> {
        Box::pin(async { Response::error("not found", 404) })
    }

    /// What it brings to the platform, for the architecture page.
    fn footprint(&self) -> Footprint {
        Footprint::default()
    }

    /// The console sections it fills.
    fn sections(&self) -> Vec<console::sections::Section> {
        Vec::new()
    }
    /// Its data for console section `id`, for `person`, a signed-in person; `query` holds the
    /// section's parameters, such as `days`. A section with a cost answers `cost_7d` in it.
    fn section_data<'a>(
        &'a self,
        _cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a users::User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async { Err(Error::RustError("no such section".into())) })
    }
    /// The secrets it needs set, which the `secrets` page lists and lets a person change.
    fn secrets(&self) -> Vec<Slot> {
        Vec::new()
    }

    /// Checks `value` would work as its secret `name` before it is written, so a mistyped value
    /// never replaces a working one. Answers what it found, for the person.
    fn check_secret<'a>(
        &'a self,
        _cx: &'a Context<'a>,
        _name: &'a str,
        _value: &'a str,
    ) -> Call<'a> {
        Box::pin(async { Ok(Value::String("not checked".into())) })
    }

    /// Acts on its secret `name` having just been written, as Secrets Store secret `id`: such
    /// as handing it to the service that uses it. Answers what it did, for the person.
    fn secret_set<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str, _id: &'a str) -> Call<'a> {
        Box::pin(async { Ok(Value::Null) })
    }

    /// When its secret `name`, which is set, stops working, as the secret's issuer reports it.
    /// Asked at most daily: [`expiry`] caches the answer. Never answers the value.
    fn secret_expiry<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str) -> Expiring<'a> {
        Box::pin(async {
            Ok(expiry::Expiry::Unknown {
                why: "there is no way to ask when this secret expires".into(),
            })
        })
    }
}

/// A secret a capability needs: a Secrets Store secret, bound to Workers by its name.
#[derive(Serialize, Clone)]
pub struct Slot {
    /// Such as `GITHUB_MIRROR_TOKEN`.
    pub name: String,
    /// What it is and where to get one, for the person setting it.
    pub purpose: String,
    /// The services that may read it, such as `workers` or `ai_gateway`.
    pub scopes: &'static [&'static str],
}

/// The hostnames, Workers and resources a capability brings. A capability Worker sends its own
/// in its manifest, read back here.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Footprint {
    pub hostnames: Vec<HostFootprint>,
    pub workers: Vec<WorkerFootprint>,
    pub zone: Option<String>,
    pub secrets: Vec<String>,
    pub queues: Vec<String>,
    pub repos: Vec<String>,
    pub mirrors: Vec<String>,
    /// AI gateways, by ID.
    pub gateways: Vec<String>,
}

/// A hostname a capability serves.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct HostFootprint {
    pub host: String,
    /// The Worker serving it; empty for the Worker hosting the capability.
    pub worker: String,
    /// The capability serving it.
    pub by: String,
    /// The service tokens Access admits; empty when public.
    pub access: Vec<String>,
    /// Paths on it that Access lets everyone through, which check callers themselves.
    pub public_paths: Vec<String>,
}

/// A Worker a capability publishes, other than the one hosting it.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct WorkerFootprint {
    pub name: String,
    pub routes: Vec<String>,
}

/// The hosted capability that offers tool `name`, if one does.
pub fn owner<'a>(
    capabilities: &'a [Box<dyn Capability>],
    name: &str,
) -> Option<&'a dyn Capability> {
    capabilities
        .iter()
        .find(|c| c.tools().iter().any(|t| t.name == name))
        .map(|c| c.as_ref())
}

/// Runs tool `name` with `args` as `cx`'s caller: the one way a tool is run, whether `/mcp`
/// was asked for it directly or through `call`, or the Lander runs it, so all find it and fail
/// the same way. The person's policy is asked first ([`approvals::gate`]): a tool it gates runs
/// only with the person's passkey approval of that exact call, used once.
pub async fn dispatch(cx: &Context<'_>, name: &str, args: &Value) -> Result<Value> {
    let owner = owner(cx.hosted.capabilities, name)
        .ok_or_else(|| Error::RustError(pigeon_core::mcp::unknown_tool(name)))?;
    match approvals::gate(cx, name, args).await? {
        approvals::Gate::Run(args) => owner.call(cx, name, &args).await,
        approvals::Gate::Approved(args, record) => {
            let result = owner.call(cx, name, &args).await;
            // The call has run and its approval is used: a failed audit line never loses its answer.
            if let Err(e) = approvals::ran(cx.env, &record, result.is_ok()).await {
                worker::console_error!("approval {} ran, not recorded: {e}", record.id);
            }
            result
        }
    }
}

/// `v` as JSON.
pub fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The error for a tool a capability does not have.
pub fn unknown(tool: &str) -> Error {
    Error::RustError(format!("no tool `{tool}`"))
}

/// Argument `name` of a tool call, as a string, required.
pub fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args[name]
        .as_str()
        .ok_or_else(|| Error::RustError(format!("`{name}` is required")))
}

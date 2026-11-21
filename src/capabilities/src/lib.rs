//! pigeon's capabilities: the features a Worker hosts, each an end result built from the
//! building blocks in `pigeon-core`. Every capability answers the same small interface: its MCP
//! tools and how to call one, the events it acts on, and its footprint (the hostnames, Workers
//! and resources it brings), which the architecture page draws.

pub mod architecture;
pub mod deploys;
pub mod mirror;
pub mod models;
pub mod oauth;
pub mod passkeys;
pub mod protected_host;
pub mod public_host;
pub mod secrets;
pub mod site;
pub mod source;
pub mod users;

use std::future::Future;
use std::pin::Pin;

use pigeon_core::Account;
use pigeon_core::mcp::Tool;
use serde::Serialize;
use serde_json::Value;
use worker::{Env, Error, Request, Response, Result};

/// A tool call or event in flight.
pub type Call<'a> = Pin<Box<dyn Future<Output = Result<Value>> + 'a>>;

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

/// The hostnames, Workers and resources a capability brings.
#[derive(Serialize, Default, Clone)]
pub struct Footprint {
    pub hostnames: Vec<HostFootprint>,
    pub workers: Vec<WorkerFootprint>,
    pub zone: Option<String>,
    pub secrets: Vec<String>,
    pub queues: Vec<String>,
    pub repos: Vec<String>,
    pub mirrors: Vec<String>,
    /// Durable Objects, by binding.
    pub objects: Vec<String>,
    /// AI gateways, by ID.
    pub gateways: Vec<String>,
}

/// A hostname a capability serves.
#[derive(Serialize, Clone)]
pub struct HostFootprint {
    pub host: String,
    pub worker: String,
    /// The capability serving it.
    pub by: String,
    /// The service tokens Access admits; empty when public.
    pub access: Vec<String>,
    /// Paths on it that Access lets everyone through, which check callers themselves.
    pub public_paths: Vec<String>,
}

/// A Worker a capability publishes, other than the one hosting it.
#[derive(Serialize, Clone)]
pub struct WorkerFootprint {
    pub name: String,
    pub routes: Vec<String>,
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

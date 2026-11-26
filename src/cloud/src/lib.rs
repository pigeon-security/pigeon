//! The cloud gateway: cloud providers behind one capability Worker, as the model gateway is for
//! model providers (docs/plans/cloud-gateway.md). Scaleway is the first provider. A provider
//! trait ([`provider::Provider`]) lists resources and plans and applies changes; the tools
//! ([`tools`]) are the same whatever the provider. Every change is a plan, an approval the
//! person gives with their passkey, and an apply that checks the provider has not moved.
//!
//! All outside effects go through traits ([`transport::Transport`], [`slots::Slots`],
//! [`approvals::Approvals`], [`sink::Sink`]), so the smoke suite runs the whole flow against
//! stubs. [`CloudGateway`] is the Worker's side of the contract: it fills those traits from the
//! Worker's environment and its `PLATFORM` binding. A new key's secret is never in an answer,
//! an error or a log line, and the Worker enforces [fixed outbound endpoints](transport::allowed).

use pigeon_contract::serve::Answering;
use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, SecretOp,
    SecretRequest, Service, Signed, SlotSpec, ToolSpec,
};
use pigeon_core::versions::{Component, Kind};
use serde_json::Value;
use worker::{Date, Env};

pub mod approvals;
pub mod changes;
pub mod compute;
pub mod digitalocean_droplet;
pub mod digitalocean_spaces;
pub mod iam;
pub mod link;
pub mod provider;
pub mod redact;
pub mod s3api;
pub mod scaleway;
pub mod sink;
pub mod slots;
pub mod tools;
pub mod transport;

use approvals::CoreApprovals;
use link::Link;
use provider::{Ports, Provider};
use sink::CoreSink;
use slots::EnvSlots;
use transport::FetchTransport;

/// The capability's name.
pub const NAME: &str = "cloud";

/// The cloud gateway Worker's script name.
pub const WORKER: &str = "pigeon-cloud-gateway";

/// The versions the cloud gateway declares: the `cloud` capability.
pub fn versions() -> Vec<Component> {
    vec![Component::began(NAME, Kind::Capability)]
}

/// The secrets the capability needs: every provider's slots.
pub fn secrets() -> Vec<SlotSpec> {
    scaleway::Scaleway.slots()
}

/// The capability as a manifest entry: its tools, its secrets and the outside hosts it calls.
pub fn capability() -> CapabilityManifest {
    let tools: Vec<ToolSpec> = tools::tools();
    CapabilityManifest {
        name: NAME.into(),
        tools,
        sections: Vec::new(),
        routes: Vec::new(),
        footprint: serde_json::json!({
            "externals": [{
                "name": "Scaleway",
                "kind": "cloud provider",
                "detail": transport::HOSTS.join(", "),
                "place": "outside",
                "out": "API: keys, buckets, objects, each a plan approved first",
            }],
        }),
        secrets: secrets(),
        wants: Vec::new(),
        versions: versions(),
    }
}

/// The time now, in milliseconds since the epoch.
fn now_ms() -> i64 {
    i64::try_from(Date::now().as_millis()).unwrap_or(i64::MAX)
}

/// The cloud gateway's side of the contract.
#[derive(Debug, Default)]
pub struct CloudGateway;

impl CloudGateway {
    /// The gateway.
    pub fn new() -> Self {
        Self
    }
}

impl Service for CloudGateway {
    fn manifest(&self) -> Manifest {
        Manifest {
            contract: CONTRACT,
            worker: WORKER.into(),
            commit: option_env!("PIGEON_COMMIT").unwrap_or_default().into(),
            capabilities: vec![capability()],
        }
    }

    fn call<'a>(&'a self, env: &'a Env, caller: &'a Signed, req: CallRequest) -> Answering<'a> {
        Box::pin(async move {
            if !tools::TOOLS.contains(&req.tool.as_str()) {
                return Answer::error(FailureKind::UnknownTool, format!("no tool `{}`", req.tool));
            }
            let approvals = CoreApprovals(Link { env, caller });
            let sink = CoreSink(Link { env, caller });
            let ports = Ports {
                transport: &FetchTransport,
                slots: &EnvSlots(env),
                approvals: &approvals,
                sink: &sink,
                now_ms: now_ms(),
                caller: &caller.caller,
            };
            match tools::run_tool(&ports, &req.tool, &req.args).await {
                Ok(v) => Answer::Ok(v),
                Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
            }
        })
    }

    fn secret<'a>(&'a self, env: &'a Env, caller: &'a Signed, req: SecretRequest) -> Answering<'a> {
        Box::pin(async move {
            match req.op {
                SecretOp::Check => {
                    let value = req.value.as_deref().unwrap_or_default();
                    match scaleway::check_secret(&req.name, value) {
                        Ok(note) => Answer::Ok(Value::String(note)),
                        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
                    }
                }
                SecretOp::Set => Answer::Ok(Value::String(scaleway::SET_NOTE.into())),
                SecretOp::Expiry => {
                    let approvals = CoreApprovals(Link { env, caller });
                    let sink = CoreSink(Link { env, caller });
                    let ports = Ports {
                        transport: &FetchTransport,
                        slots: &EnvSlots(env),
                        approvals: &approvals,
                        sink: &sink,
                        now_ms: now_ms(),
                        caller: &caller.caller,
                    };
                    let expiry = scaleway::key_expiry(&ports, &req.name).await;
                    Answer::Ok(serde_json::to_value(expiry).unwrap_or(Value::Null))
                }
            }
        })
    }
}

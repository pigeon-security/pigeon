//! The git proxy as core serves it: [`pigeon_proxies::git_proxy`], whose request handling it
//! shares with the proxy Worker (`pigeon-proxy`), behind core's own [`Local`] host. Its pure
//! helpers (the policy's vars, push checks, `moves_forward`) and the platform's own git reads
//! (`tip`, `contains`, `fast_forward`, `read_file`) are re-exported here for the Lander and CI.

use pigeon_core::mcp::Tool;
use serde_json::Value;
use worker::Request;

pub use pigeon_proxies::git_proxy::*;

use crate::proxy::Local;
use crate::{Call as ToolCall, Capability, Context, Footprint, HostFootprint, Serve, unknown};

impl Capability for GitProxy {
    fn name(&self) -> &'static str {
        "git_proxy"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "git_proxy",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> ToolCall<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/git/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(async move { self.serve(&Local { cx }, cx.worker, req).await })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            hostnames: vec![HostFootprint {
                host: self.host.clone(),
                worker: String::new(),
                by: self.name().into(),
                access: Vec::new(),
                public_paths: self.routes().iter().map(|r| (*r).to_string()).collect(),
            }],
            repos: vec![format!("{}/{}", self.namespace, self.repo)],
            ..Footprint::default()
        }
    }
}

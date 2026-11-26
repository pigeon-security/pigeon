//! The Cloudflare API proxy as core serves it: [`pigeon_proxies::cf_proxy`], whose request
//! handling and allowlist it shares with the proxy Worker (`pigeon-proxy`), behind core's own
//! [`Local`] host, which mints each call's token from the parent token.

use pigeon_core::mcp::Tool;
use serde_json::Value;
use worker::Request;

pub use pigeon_proxies::cf_proxy::*;

use crate::proxy::Local;
use crate::{Call, Capability, Context, Footprint, HostFootprint, Serve, unknown};

impl Capability for CfProxy {
    fn name(&self) -> &'static str {
        "cf_proxy"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "cf_proxy",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/cf/"] // route: token
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
            ..Footprint::default()
        }
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(async move { self.serve(&Local { cx }, req).await })
    }
}

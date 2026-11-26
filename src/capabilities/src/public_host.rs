//! A hostname served by a Worker in public, with no Access in front: every path on it checks
//! its callers itself or is open to all.

use pigeon_core::mcp::Tool;
use pigeon_core::{Account, custom_domain, zones};
use serde_json::{Value, json};
use worker::Result;

use crate::{Call, Capability, Context, Footprint, HostFootprint, to_value, unknown};

/// A public hostname.
pub struct PublicHost {
    pub zone: String,
    pub host: String,
    /// The Worker serving it.
    pub worker: String,
    /// Other Workers the tool may attach the host to instead, when its `worker` argument names
    /// one: for moving a host between Workers and back in one call (the proxy host's cutover to
    /// `pigeon-proxy`). Without the argument the host goes to [`PublicHost::worker`].
    pub also: Vec<String>,
    /// The tool that sets it up, such as `setup_platform`.
    pub tool: &'static str,
}

impl PublicHost {
    /// Attaches the host to the Worker.
    async fn serve(&self, account: &Account, worker: &str) -> Result<Value> {
        let zone = zones::active(account, &self.zone).await?;
        let custom_domain = custom_domain::attach(account, &zone.id, &self.host, worker).await?;
        Ok(json!({
            "zone": zone,
            "host": self.host,
            "worker": worker,
            "custom_domain": custom_domain,
        }))
    }
}

impl Capability for PublicHost {
    fn name(&self) -> &'static str {
        "public_host"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![
            pigeon_core::versions::Component::began(
                "public_host",
                pigeon_core::versions::Kind::Capability,
            )
            .entry(
                "1.0.1",
                "Setting the host up no longer retires an Access setup: none was left to retire.",
            ),
        ]
    }

    fn tools(&self) -> Vec<Tool> {
        let description = "Make sure the hostname is served by its Worker in public.";
        if self.also.is_empty() {
            return vec![Tool::bare(self.tool, description)];
        }
        let mut workers = vec![self.worker.clone()];
        workers.extend(self.also.iter().cloned());
        vec![Tool {
            name: self.tool,
            description,
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker": {
                        "type": "string",
                        "enum": workers,
                        "description": "The Worker to attach the hostname to; its own Worker when not given.",
                    }
                }
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != self.tool {
                return Err(unknown(tool));
            }
            let target = match args.get("worker").and_then(Value::as_str) {
                None => self.worker.as_str(),
                Some(w) if w == self.worker || self.also.iter().any(|a| a == w) => w,
                Some(w) => {
                    return Err(worker::Error::RustError(format!(
                        "{} is not served by Worker `{w}`; it is served by `{}`{}",
                        self.host,
                        self.worker,
                        if self.also.is_empty() {
                            String::new()
                        } else {
                            format!(" or {}", self.also.join(", "))
                        }
                    )));
                }
            };
            Ok(to_value(self.serve(cx.account, target).await?))
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            hostnames: vec![HostFootprint {
                host: self.host.clone(),
                worker: self.worker.clone(),
                by: self.name().into(),
                access: Vec::new(),
                public_paths: Vec::new(),
            }],
            zone: Some(self.zone.clone()),
            ..Footprint::default()
        }
    }
}

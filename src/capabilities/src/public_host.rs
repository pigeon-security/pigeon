//! A hostname served by a Worker in public, with no Access in front: every path on it checks
//! its callers itself or is open to all. Serving it retires a named Access setup the host had:
//! its applications, their policies, their service tokens and the tokens' stored secrets, each
//! skipped when already gone.

use pigeon_core::access::Session;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::Writer;
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
    /// An Access setup the host had, removed when it is served.
    pub retire: Retire,
    /// The tool that sets it up, such as `setup_platform`.
    pub tool: &'static str,
}

/// Names of an Access setup to remove.
#[derive(Default)]
pub struct Retire {
    /// Its applications, as (name, domain), such as (`pigeon-platform`, `platform.pigeon.dev`)
    /// and (`pigeon-platform/auth`, `platform.pigeon.dev/auth`): only a self-hosted application
    /// with both that name and that domain is removed.
    pub apps: Vec<(String, String)>,
    /// Its reusable policies, by name.
    pub policies: Vec<String>,
    /// Its service tokens, by name; every policy holding one goes too.
    pub service_tokens: Vec<String>,
    /// Secrets Store names that kept those tokens.
    pub secrets: Vec<String>,
}

impl PublicHost {
    /// Attaches the host to the Worker, then retires the Access setup, so the host is served
    /// throughout.
    async fn serve(&self, account: &Account, worker: &str) -> Result<Value> {
        let zone = zones::active(account, &self.zone).await?;
        let custom_domain = custom_domain::attach(account, &zone.id, &self.host, worker).await?;
        let retired = self.retire(account).await?;
        Ok(json!({
            "zone": zone,
            "host": self.host,
            "worker": worker,
            "custom_domain": custom_domain,
            "retired": retired,
        }))
    }

    async fn retire(&self, account: &Account) -> Result<Value> {
        let r = &self.retire;
        let session = Session::open(account).await?;
        let removed = async {
            let mut apps = Vec::new();
            for (name, domain) in &r.apps {
                apps.extend(session.remove_app(name, domain).await?);
            }
            let mut tokens = Vec::new();
            for name in &r.service_tokens {
                tokens.extend(session.find_service_token(name).await?);
            }
            let mut policies = Vec::new();
            for name in &r.policies {
                policies.extend(session.remove_policies(name, None).await?);
            }
            for id in &tokens {
                policies.extend(session.remove_policies("", Some(id)).await?);
                session.remove_service_token(id).await?;
            }
            Ok(json!({ "apps": apps, "policies": policies, "service_tokens": tokens }))
        }
        .await;
        let mut removed = session.finish(removed).await?;
        let writer = Writer::open(account).await?;
        let secrets = async {
            let mut gone = Vec::new();
            for secret in writer.list().await? {
                if r.secrets.contains(&secret.name) {
                    writer.delete(&secret.id).await?;
                    gone.push(secret.name);
                }
            }
            Ok(gone)
        }
        .await;
        removed["secrets"] = json!(writer.finish(secrets).await?);
        Ok(removed)
    }
}

impl Capability for PublicHost {
    fn name(&self) -> &'static str {
        "public_host"
    }

    fn tools(&self) -> Vec<Tool> {
        let description = "Make sure the hostname is served by its Worker in public, and that \
                           the Access setup it had is gone.";
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

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
    /// An Access setup the host had, removed when it is served.
    pub retire: Retire,
    /// The tool that sets it up, such as `setup_platform`.
    pub tool: &'static str,
}

/// Names of an Access setup to remove.
#[derive(Default)]
pub struct Retire {
    /// The domains of its applications, such as `platform.pigeon.dev` and
    /// `platform.pigeon.dev/auth`.
    pub apps: Vec<String>,
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
    async fn serve(&self, account: &Account) -> Result<Value> {
        let zone = zones::active(account, &self.zone).await?;
        let custom_domain =
            custom_domain::attach(account, &zone.id, &self.host, &self.worker).await?;
        let retired = self.retire(account).await?;
        Ok(json!({
            "zone": zone,
            "host": self.host,
            "custom_domain": custom_domain,
            "retired": retired,
        }))
    }

    async fn retire(&self, account: &Account) -> Result<Value> {
        let r = &self.retire;
        let session = Session::open(account).await?;
        let removed = async {
            let mut apps = Vec::new();
            for domain in &r.apps {
                apps.extend(session.remove_app(domain).await?);
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
        vec![Tool::bare(
            self.tool,
            "Make sure the hostname is served by its Worker in public, and that the Access setup \
             it had is gone.",
        )]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != self.tool {
                return Err(unknown(tool));
            }
            Ok(to_value(self.serve(cx.account).await?))
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

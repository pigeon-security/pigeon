//! CI behind the contract: what `pigeon-ci-gateway` serves core over its Service Binding
//! (docs/plans/workers-split.md). Core has already authenticated the caller and run the gate;
//! the caller arrives signed, and is what a scoped credential is asked for.

use std::pin::Pin;

use pigeon_contract::serve::Answering;
use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, SecretOp,
    SecretRequest, Service, Signed, ToolSpec,
};
use pigeon_core::expiry::Expiry;
use serde_json::{Value, json};
use worker::{Env, Request, Response};

use crate::{BUILD, Ci, ROUTE, Settings};

/// The Worker that serves [`CiGateway`], as deployed.
pub const WORKER: &str = "pigeon-ci-gateway";

/// CI as a capability Worker's [`Service`]: its tools, its `/ci/` routes and its secrets.
pub struct CiGateway {
    ci: Ci,
    /// The key core signs callers with, which this Worker signs its own asks of core with.
    key: Vec<u8>,
}

impl CiGateway {
    /// The gateway with `settings` (whose accounts are [`crate::Accounts::Scoped`]) and the
    /// caller key.
    pub fn new(settings: Settings, key: Vec<u8>) -> Self {
        Self {
            ci: Ci { settings },
            key,
        }
    }
}

/// The signed caller, unless it is null.
fn caller_of(signed: &Signed) -> Option<&Value> {
    Some(&signed.caller).filter(|c| !c.is_null())
}

impl Service for CiGateway {
    fn manifest(&self) -> Manifest {
        Manifest {
            contract: CONTRACT,
            worker: WORKER.into(),
            commit: BUILD.into(),
            capabilities: vec![CapabilityManifest {
                name: Ci::NAME.into(),
                tools: self
                    .ci
                    .tools()
                    .into_iter()
                    .map(|t| ToolSpec {
                        name: t.name.into(),
                        description: t.description.into(),
                        input_schema: t.input_schema,
                    })
                    .collect(),
                routes: vec![ROUTE.into()],
                footprint: self.ci.footprint(),
                secrets: self.ci.secrets(),
                versions: crate::versions(),
                ..CapabilityManifest::default()
            }],
        }
    }

    fn call<'a>(&'a self, env: &'a Env, caller: &'a Signed, req: CallRequest) -> Answering<'a> {
        Box::pin(async move {
            if !self.ci.tools().iter().any(|t| t.name == req.tool) {
                return Answer::error(FailureKind::UnknownTool, format!("no tool `{}`", req.tool));
            }
            let cx = self.ci.cx(env, caller_of(caller), Some(&self.key));
            let answer = if let Some(instance) = caller.instance() {
                self.ci
                    .scoped_read(&cx, &req.tool, &req.args, Some(instance))
                    .await
            } else {
                self.ci.call(&cx, &req.tool, &req.args).await
            };
            match answer {
                Ok(v) => Answer::Ok(v),
                Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
            }
        })
    }

    fn fetch<'a>(
        &'a self,
        env: &'a Env,
        req: Request,
    ) -> Pin<Box<dyn Future<Output = worker::Result<Response>> + 'a>> {
        Box::pin(self.ci.serve(env, req))
    }

    fn secret<'a>(&'a self, env: &'a Env, caller: &'a Signed, req: SecretRequest) -> Answering<'a> {
        Box::pin(async move {
            if !self.ci.secrets().iter().any(|s| s.name == req.name) {
                return Answer::error(
                    FailureKind::BadRequest,
                    format!("CI has no secret `{}`", req.name),
                );
            }
            match req.op {
                SecretOp::Check => Answer::Ok(json!("not checked")),
                SecretOp::Set => Answer::Ok(Value::Null),
                SecretOp::Expiry => {
                    let cx = self.ci.cx(env, caller_of(caller), Some(&self.key));
                    let expiry = match self.ci.secret_expiry(&cx, &req.name).await {
                        Ok(e) => e,
                        Err(e) => Expiry::Unknown { why: e.to_string() },
                    };
                    match serde_json::to_value(&expiry) {
                        Ok(v) => Answer::Ok(v),
                        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
                    }
                }
            }
        })
    }
}

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

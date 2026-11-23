//! Models, as core hosts them until its binding to `pigeon-model-gateway` lands
//! (docs/plans/workers-split.md, phase 3). The capability's logic is the `pigeon-models` crate,
//! which the gateway Worker serves too; this is only its `Capability`, acting on core's own
//! account and the caller core resolved. The job objects' class is still core's, over
//! [`runner`].

use pigeon_core::ai_gateway::SECRET_SCOPE;
use pigeon_core::mcp::Tool;
use serde_json::Value;

pub use pigeon_models::{
    Ask, Dialect, Models, Provider, Reply, ask, daily, jobs, runner, stream, tags,
};

use crate::console::sections::{Section, Tab};
use crate::users::User;
use crate::{Call, Capability, Context, Expiring, External, Footprint, Slot, to_value};

/// The caller in `cx` as JSON, as core signs it for a capability Worker; null for none.
fn caller_json(cx: &Context<'_>) -> Value {
    to_value(cx.caller)
}

impl Capability for Models {
    fn name(&self) -> &'static str {
        pigeon_models::NAME
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        pigeon_models::versions()
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Cost,
            id: self.section_id(),
            title: pigeon_models::SECTION_TITLE,
            description: pigeon_models::SECTION_DESCRIPTION,
            order: pigeon_models::SECTION_ORDER,
            cost: Some(pigeon_models::SECTION_COST),
        }]
    }

    /// Cost per day per model family over the last `days` days, from the gateway's logs.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move { self.section(cx.account, query).await })
    }

    fn tools(&self) -> Vec<Tool> {
        Models::tools(self)
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if !pigeon_models::TOOLS.contains(&tool) {
                return Err(crate::unknown(tool));
            }
            Models::call(self, cx.env, Some(cx.account), &caller_json(cx), tool, args).await
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        Models::secrets(self)
            .into_iter()
            .map(|s| Slot {
                name: s.name,
                purpose: s.purpose,
                scopes: &[SECRET_SCOPE],
            })
            .collect()
    }

    fn check_secret<'a>(&'a self, _cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            Ok(Value::String(
                Models::check_secret(self, name, value).await?,
            ))
        })
    }

    fn secret_expiry<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str) -> Expiring<'a> {
        Box::pin(async { Ok(Models::secret_expiry(self)) })
    }

    /// A key just set: the gateway is set up and given it.
    fn secret_set<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, id: &'a str) -> Call<'a> {
        Box::pin(async move {
            Ok(Value::String(
                Models::secret_set(self, cx.account, name, id).await?,
            ))
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            gateways: vec![self.gateway.clone()],
            externals: vec![External::new(
                "AI Gateway",
                "model gateway",
                format!("gateway {}", self.gateway),
            )],
            ..Footprint::default()
        }
    }
}

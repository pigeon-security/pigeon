//! Container images, read only: what the account's container applications run and what its
//! registry holds, so checking a deploy's image needs no minted token in a transcript. The
//! registry's pull credentials stay inside the Worker (`pigeon_core::registry`).

use pigeon_core::mcp::Tool;
use pigeon_core::registry;
use serde_json::{Value, json};

use crate::{Call, Capability, Context, to_value, unknown};

/// The `containers_images` tool.
pub struct Images;

impl Capability for Images {
    fn name(&self) -> &'static str {
        "images"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool::bare(
            "containers_images",
            "The account's container applications (name, image, instance type, instances, \
             the Durable Object namespace each serves) and the repositories and tags its \
             registry, registry.cloudflare.com, holds. Read only; no credential is answered.",
        )]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != "containers_images" {
                return Err(unknown(tool));
            }
            // One failing half leaves the other answered, with why.
            let mut out = json!({ "registry": registry::HOST });
            match registry::applications(cx.account).await {
                Ok(a) => out["applications"] = to_value(a),
                Err(e) => out["applications_error"] = json!(e.to_string()),
            }
            match registry::repositories(cx.account).await {
                Ok(r) => out["repositories"] = to_value(r),
                Err(e) => out["repositories_error"] = json!(e.to_string()),
            }
            Ok(out)
        })
    }
}

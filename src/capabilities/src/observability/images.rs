//! `containers_images`: container images, read only: what the account's container applications
//! run and what its registry holds, so checking a deploy's image needs no minted token in a
//! transcript. The registry's pull credentials stay inside the Worker (`pigeon_core::registry`).
//! Once a capability of its own (`images`); now part of `observability`.

use pigeon_core::mcp::Tool;
use pigeon_core::registry;
use serde_json::{Value, json};
use worker::Result;

use crate::{Context, to_value};

/// The tool's name.
pub const TOOL: &str = "containers_images";

/// The `containers_images` tool.
pub(super) fn tool() -> Tool {
    Tool::bare(
        TOOL,
        "The account's container applications (name, image, instance type, instances, \
         the Durable Object namespace each serves) and the repositories and tags its \
         registry, registry.cloudflare.com, holds. Read only; no credential is answered.",
    )
}

/// The applications and the registry's repositories. One failing half leaves the other
/// answered, with why.
pub(super) async fn containers_images(cx: &Context<'_>) -> Result<Value> {
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
}

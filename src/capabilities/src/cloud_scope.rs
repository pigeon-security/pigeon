//! Cloud Gateway instance opt-in. Scoped status answers from the approved
//! instance record. Provider operations require a future instance-bound implementation.

use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::catalogue;
use crate::instances::record::Instance;
use crate::instances::tools::{Enabled, Registration, Scope};

pub const NAME: &str = "cloud";
pub const STATUS: &str = "cloud_status";
const TOOLS: &[Registration] = &[Registration {
    name: STATUS,
    description: Some("Cloud Gateway setup status for this approved instance."),
    arguments: &[],
}];

pub fn instance_tools(record: &Instance) -> serde_json::Result<Enabled> {
    Ok(Enabled::when(
        catalogue::hosted_enabled(record, NAME),
        TOOLS,
        &[],
    ))
}

/// An absent home profile keeps the legacy provider tools off.
pub fn home_enabled(home: Option<&Instance>) -> bool {
    home.is_some_and(|record| catalogue::is_home(record) && catalogue::hosted_enabled(record, NAME))
}

pub async fn platform_enabled(env: &Env) -> Result<bool> {
    let records = crate::instances::all(env).await?;
    Ok(home_enabled(records.iter().find(|r| catalogue::is_home(r))))
}

/// Whether this hosted capability is visible in global MCP discovery.
pub fn discovered_globally(capability: &str, home_enabled: bool) -> bool {
    capability != NAME || home_enabled
}

/// A scoped call may answer status locally. An unscoped provider call must
/// have opted in on home and cannot name an instance to change its credentials.
pub fn route(
    scope: Option<&Scope>,
    platform_enabled: bool,
    tool: &str,
    args: &Value,
) -> std::result::Result<Option<Value>, String> {
    if let Some(scope) = scope {
        scope.bind(tool, args)?;
        if tool != STATUS {
            return Err("provider operations are unavailable on instance endpoints".into());
        }
        return Ok(Some(json!({
            "instance": scope.instance(),
            "enabled": true,
            "providers_configured": false,
            "detail": "Instance-scoped provider connections are not available yet."
        })));
    }
    if args.get("instance").is_some() {
        return Err("use the instance MCP endpoint for scoped Cloud Gateway status".into());
    }
    if !platform_enabled {
        return Err("Cloud Gateway is disabled on the home instance".into());
    }
    Ok(None)
}

pub async fn authorize(
    env: &Env,
    scope: Option<&Scope>,
    tool: &str,
    args: &Value,
) -> Result<Option<Value>> {
    let enabled = if scope.is_none() && args.get("instance").is_none() {
        platform_enabled(env).await?
    } else {
        false
    };
    route(scope, enabled, tool, args).map_err(Error::RustError)
}

/// Inline capability for setup status, independent of any remote manifest.
pub struct Inline;

impl crate::Capability for Inline {
    fn name(&self) -> &'static str {
        NAME
    }

    fn tools(&self) -> Vec<pigeon_core::mcp::Tool> {
        vec![pigeon_core::mcp::Tool::bare(
            STATUS,
            "Cloud Gateway setup status for the approved instance.",
        )]
    }

    fn call<'a>(
        &'a self,
        _cx: &'a crate::Context<'a>,
        _tool: &'a str,
        _args: &'a Value,
    ) -> crate::Call<'a> {
        Box::pin(async {
            Ok(json!({
                "enabled": true,
                "providers_configured": false,
                "detail": "Instance-scoped provider connections are not available yet."
            }))
        })
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            NAME,
            pigeon_core::versions::Kind::Capability,
        )]
    }
}

//! The signed instance context `site_deploy` and `site_rollback` require.
//!
//! Every core sender (the scoped MCP route, the global connector and the GitHub scheduler's
//! private `site_deploy_queued`) signs these calls with a [`pigeon_contract::WorkerContext`]
//! naming pigeon-sites and the approved instance. A call without that context, or for another
//! instance than its `instance` argument, is refused here, before any core callback or
//! credential request: there is no legacy platform path left to fall back to.

use pigeon_contract::Signed;
use serde_json::Value;

/// Why a call without signed instance context is refused.
pub const MISSING: &str = "site_deploy and site_rollback need signed instance context; this call carries none (a legacy envelope), so nothing was read or written";

/// Why a call signed for another instance is refused.
pub const MISMATCH: &str =
    "the signed instance context does not match `instance`; nothing was read or written";

/// The instance `args` names, when `caller` was signed for exactly that instance. Pure.
pub fn require<'a>(caller: &Signed, args: &'a Value) -> Result<&'a str, &'static str> {
    let instance = args["instance"].as_str().ok_or("`instance` is required")?;
    if caller.instance().is_none() {
        return Err(MISSING);
    }
    caller.require_instance(instance).map_err(|_| MISMATCH)?;
    Ok(instance)
}

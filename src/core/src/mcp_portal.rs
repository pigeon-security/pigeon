//! Cloudflare MCP server portals: a portal lists an MCP server's tools as it last synced them,
//! about every two hours on its own. A sync asks it to read them again now, so tools a deploy
//! added or removed show at once.

use serde_json::Value;
use worker::{Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// What a sync's token holds.
pub const PERMISSIONS: [&str; 1] = ["MCP Portals Write"];

/// Syncs MCP server `server` (its server ID, such as `pigeon`) on `account`'s portals: its
/// state afterwards, with `status` one of `waiting`, `ready`, `stale` or `error`.
pub async fn sync(account: &Account, server: &str) -> Result<Value> {
    let token = Scoped::open(
        account,
        "mcp-portal",
        vec![Policy::new(Scope::Account, &PERMISSIONS)],
    )
    .await?;
    let path = format!(
        "/accounts/{}/access/ai-controls/mcp/servers/{server}/sync",
        account.id
    );
    let synced = call(token.value(), Method::Post, &path, None).await;
    token.finish(synced).await
}

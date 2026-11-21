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

/// One call on the account's MCP servers (`…/access/ai-controls/mcp/servers{rest}`) with a
/// token minted for it alone.
async fn servers_call(
    account: &Account,
    method: Method,
    rest: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let token = Scoped::open(
        account,
        "mcp-portal",
        vec![Policy::new(Scope::Account, &PERMISSIONS)],
    )
    .await?;
    let path = format!(
        "/accounts/{}/access/ai-controls/mcp/servers{rest}",
        account.id
    );
    let answered = call(token.value(), method, &path, body).await;
    token.finish(answered).await
}

/// The account's MCP servers, as Cloudflare lists them (never their credentials).
pub async fn servers(account: &Account) -> Result<Vec<Value>> {
    match servers_call(account, Method::Get, "", None).await? {
        Value::Array(a) => Ok(a),
        _ => Err(worker::Error::RustError(
            "Cloudflare answered MCP servers that are not a list".into(),
        )),
    }
}

/// Creates MCP server `body` (its `id`, `name`, `hostname`, `auth_type`, …).
pub async fn create_server(account: &Account, body: &Value) -> Result<Value> {
    servers_call(account, Method::Post, "", Some(body)).await
}

/// Updates MCP server `id`. A body without `auth_credentials` keeps the stored ones.
pub async fn update_server(account: &Account, id: &str, body: &Value) -> Result<Value> {
    if id.is_empty() || id.len() > 32 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(worker::Error::RustError("not an MCP server ID".into()));
    }
    servers_call(account, Method::Put, &format!("/{id}"), Some(body)).await
}

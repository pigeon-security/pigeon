//! The pigeon Worker: its entry point and bindings. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-core builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use pigeon_capabilities::{secrets_store, tokens};
use worker::{Context, Env, Error, Method, Request, Response, Result, event};

/// The Secrets Store binding holding the parent token.
const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

#[event(fetch)]
async fn fetch(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        _ => Response::error("not found", 404),
    }
}

/// The Cloudflare account the Worker runs in.
pub fn account(env: &Env) -> Result<String> {
    Ok(env.var("CF_ACCOUNT_ID")?.to_string())
}

/// The parent token: the one long-lived credential, which mints every other.
pub async fn parent_token(env: &Env) -> Result<String> {
    secrets_store::read(&env.secret_store(PARENT_TOKEN)?)
        .await?
        .ok_or_else(|| Error::RustError(format!("`{PARENT_TOKEN}` is not set in the Secrets Store")))
}

/// Mints `request` from the parent token.
pub async fn mint(env: &Env, request: &tokens::MintRequest) -> Result<tokens::Minted> {
    tokens::mint(&parent_token(env).await?, &account(env)?, request).await
}

/// Revokes minted token `id`.
pub async fn revoke(env: &Env, id: &str) -> Result<()> {
    tokens::revoke(&parent_token(env).await?, &account(env)?, id).await
}

/// A writer on the account's Secrets Store; call `finish` to revoke its token.
pub async fn secrets_writer(env: &Env) -> Result<secrets_store::Writer> {
    let store = env.var("CF_SECRETS_STORE_ID")?.to_string();
    secrets_store::Writer::open(&parent_token(env).await?, &account(env)?, &store).await
}

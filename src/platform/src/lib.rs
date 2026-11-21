//! pigeon-platform: the one Worker, hosting pigeon's capabilities behind Access. It answers
//! `/health` and the MCP endpoint, hands queue events to the capabilities that want them, and
//! does nothing else itself. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-platform builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

mod settings;

include!(concat!(env!("OUT_DIR"), "/sources.rs"));

use pigeon_capabilities::{Capability, Context, Hosted};
use pigeon_core::mcp::{self, Server};
use pigeon_core::{Account, secrets_store};
use serde_json::Value;
use worker::{
    Context as WorkerContext, Env, Error, MessageBatch, Method, Request, Response, Result,
    console_error, console_log, event,
};

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: WorkerContext) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        (Method::Post, "/mcp") => mcp_post(req, &env).await,
        (_, "/mcp") => Response::error("the MCP endpoint takes POST only", 405),
        _ => Response::error("not found", 404),
    }
}

/// Queue messages: each capability that wants one of the batch acts once. A failure fails the
/// batch, so the queue retries it.
#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: WorkerContext) -> Result<()> {
    let messages = batch.messages()?;
    let caps = settings::capabilities();
    let hosted = hosted(&caps);
    let wanting: Vec<&Box<dyn Capability>> = caps
        .iter()
        .filter(|c| messages.iter().any(|m| c.wants(m.body())))
        .collect();
    if wanting.is_empty() {
        return Ok(());
    }
    let account = account(&env).await?;
    let cx = Context {
        account: &account,
        env: &env,
        hosted: &hosted,
    };
    for c in wanting {
        match c.on_events(&cx).await {
            Ok(v) => console_log!("{}: {}", c.name(), v),
            Err(e) => {
                console_error!("{} failed: {e}", c.name());
                return Err(e);
            }
        }
    }
    Ok(())
}

fn hosted(caps: &[Box<dyn Capability>]) -> Hosted<'_> {
    Hosted {
        worker: settings::WORKER,
        capabilities: caps,
        sources: SOURCES,
    }
}

/// The account this Worker acts on, with the parent token read from its binding.
async fn account(env: &Env) -> Result<Account> {
    let parent = secrets_store::read(&env.secret_store(settings::PARENT_TOKEN)?)
        .await?
        .ok_or_else(|| {
            Error::RustError(format!(
                "`{}` is not set in the Secrets Store",
                settings::PARENT_TOKEN
            ))
        })?;
    Ok(Account {
        id: env.var("CF_ACCOUNT_ID")?.to_string(),
        parent,
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
    })
}

/// One MCP message, answered as JSON; a notification gets 202 and no body. The tools are every
/// hosted capability's.
async fn mcp_post(mut req: Request, env: &Env) -> Result<Response> {
    let message: Value = match req.json().await {
        Ok(v) => v,
        Err(e) => return Response::error(format!("not JSON: {e}"), 400),
    };
    let caps = settings::capabilities();
    let hosted = hosted(&caps);
    let tools: Vec<mcp::Tool> = caps.iter().flat_map(|c| c.tools()).collect();
    let server = Server {
        name: settings::WORKER,
        version: env!("CARGO_PKG_VERSION"),
        instructions: "pigeon's platform. Source code lives in Cloudflare Artifacts; GitHub is \
                       a read-only copy. To push, mint a write token with source_token and send \
                       it to git as `Authorization: Bearer <token>`.",
    };
    let call = async |name: &str, args: &Value| -> Result<Value> {
        let owner = caps
            .iter()
            .find(|c| c.tools().iter().any(|t| t.name == name))
            .ok_or_else(|| Error::RustError(format!("no tool `{name}`")))?;
        let account = account(env).await?;
        let cx = Context {
            account: &account,
            env,
            hosted: &hosted,
        };
        owner.call(&cx, name, args).await
    };
    match mcp::answer(&server, &tools, &message, call).await {
        Some(answer) => Response::from_json(&answer),
        None => Ok(Response::empty()?.with_status(202)),
    }
}

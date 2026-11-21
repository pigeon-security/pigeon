//! pigeon-platform: the one Worker, hosting pigeon's capabilities behind Access. It answers
//! `/health` and the MCP endpoint, hands the paths a capability serves (such as `/auth/`) to it,
//! keeps the proxies (`/git/`, `/cf/`) to their own hostname and that hostname to them,
//! hands queue events to the capabilities that want them, and does nothing else itself. Built
//! for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-platform builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

mod settings;

include!(concat!(env!("OUT_DIR"), "/sources.rs"));

use pigeon_capabilities::oauth;
use pigeon_capabilities::proxy;
use pigeon_capabilities::users::{self, Caller};
use pigeon_capabilities::{Capability, Context, Hosted};

/// The Directory's class, exported from this Worker for its Durable Object binding.
pub use pigeon_capabilities::users::Directory;

/// The CI scheduler's class: `ci::Scheduler` with the connectors from this Worker's settings,
/// which the capabilities crate cannot see.
#[worker::durable_object]
pub struct CiScheduler {
    inner: pigeon_capabilities::ci::scheduler::Scheduler,
}

impl worker::DurableObject for CiScheduler {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_capabilities::ci::scheduler::Scheduler::new(state, env, settings::ci())
            .expect("the CI scheduler's migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.inner.alarm().await
    }
}

/// The CI runner's class: one per job attempt, holding that attempt's container.
#[worker::durable_object]
pub struct CiRunner {
    inner: pigeon_capabilities::ci::containers::Runner,
}

impl worker::DurableObject for CiRunner {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_capabilities::ci::containers::Runner::new(state, env)
            .expect("the CI runner's migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.inner.alarm().await
    }
}
use pigeon_core::mcp::{self, Server};
use pigeon_core::oauth as oauth_core;
use pigeon_core::{Account, secrets_store};
use serde_json::Value;
use worker::wasm_bindgen;
use worker::{
    Context as WorkerContext, Env, Error, Headers, MessageBatch, Method, Request, Response, Result,
    console_error, console_log, event,
};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: WorkerContext) -> Result<Response> {
    let path = req.path();
    let host = req.url()?.host_str().unwrap_or_default().to_string();
    if !proxy::host_serves(&host, settings::PROXY_HOST, &path) {
        return Response::error("not found", 404);
    }
    let caps = settings::capabilities();
    // A route is a prefix, so the root, the console's Overview, is matched by name.
    if let Some(c) = caps.iter().find(|c| {
        if path == "/" {
            c.name() == "console"
        } else {
            c.routes().iter().any(|r| path.starts_with(r))
        }
    }) {
        let hosted = hosted(&caps);
        let account = account(&env).await?;
        let cx = Context {
            account: &account,
            env: &env,
            hosted: &hosted,
            caller: None,
            caller_note: None,
            worker: Some(&ctx),
        };
        return c.fetch(&cx, req).await;
    }
    match (req.method(), path.as_str()) {
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
        caller: None,
        caller_note: None,
        worker: None,
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
    let account = account(env).await?;
    // An OAuth bearer token first, then the Access token; with neither, no tool runs.
    let bearer = req
        .headers()
        .get("Authorization")?
        .and_then(|a| a.strip_prefix("Bearer ").map(str::to_string));
    let resolved = match &bearer {
        Some(token) => oauth::caller(env, token).await,
        None => users::caller(env, &account, settings::HOST, &req).await,
    };
    let caller: Caller = match resolved {
        Ok(c) => c,
        Err(e) => {
            console_error!("caller unknown: {e}");
            let metadata = format!(
                "https://{}/.well-known/oauth-protected-resource",
                settings::HOST
            );
            let headers = Headers::new();
            headers.set("WWW-Authenticate", &oauth_core::challenge(&metadata))?;
            let body =
                serde_json::json!({ "error": "unauthorized", "error_description": e.to_string() });
            return Ok(Response::from_json(&body)?
                .with_status(401)
                .with_headers(headers));
        }
    };
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
                       a read-only copy of main. A cloud session reaches git at \
                       https://proxy.pigeon.dev/git/pigeon-security/pigeon.git and deploys with \
                       CLOUDFLARE_API_BASE_URL=https://proxy.pigeon.dev/cf/client/v4; its \
                       environment's network secret for proxy.pigeon.dev authenticates both, so \
                       no token goes in a command. Push branches under claude/; main only moves \
                       forward. source_token and deploy_token remain for callers without the \
                       proxy credential.",
    };
    let call = async |name: &str, args: &Value| -> Result<Value> {
        let owner = caps
            .iter()
            .find(|c| c.tools().iter().any(|t| t.name == name))
            .ok_or_else(|| Error::RustError(format!("no tool `{name}`")))?;
        let cx = Context {
            account: &account,
            env,
            hosted: &hosted,
            caller: Some(&caller),
            caller_note: None,
            worker: None,
        };
        owner.call(&cx, name, args).await
    };
    match mcp::answer(&server, &tools, &message, call).await {
        Some(answer) => Response::from_json(&answer),
        None => Ok(Response::empty()?.with_status(202)),
    }
}

//! pigeon-platform: the one Worker, hosting pigeon's capabilities. It answers `/health` and the
//! MCP endpoint, hands the paths a capability serves (such as `/auth/`) to it, keeps the proxies
//! (`/git/`, `/cf/`) to their own hostname and that hostname to them, serves the public site on
//! the zone's apex and nothing else there,
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
use pigeon_capabilities::site;
use pigeon_capabilities::split;
use pigeon_capabilities::users::{self, Caller};
use pigeon_capabilities::{Capability, Context, Hosted};

/// The Directory's class, exported from this Worker for its Durable Object binding.
pub use pigeon_capabilities::users::Directory;

/// The CI scheduler's class, kept only for an old migration. It is served from
/// `pigeon-ci-gateway` now: its v1 migration transferred the class there
/// (`transferred_classes`), with its objects, and this Worker's `CI_SCHEDULER` binding names that
/// Worker (`script_name`). Nothing here runs it.
///
/// It stays because this Worker's migration v2 (`src/platform/wrangler.toml`) still declares
/// `CiScheduler` in `new_sqlite_classes`, with no later `deleted_classes`, and no one has checked
/// that Cloudflare accepts an upload of a script that no longer exports a class its migrations
/// once created and that was transferred away (incident 5, `docs/plans/workers-split.md`; the
/// planned check is the second rehearsal in `docs/plans/do-rehearsal.md`). Removing it is the
/// person's decision, after that rehearsal passes. Never add a `deleted_classes` migration for it
/// here without a reviewed plan: a deletion is irreversible and could reach the gateway's objects.
#[worker::durable_object]
pub struct CiScheduler {
    inner: pigeon_ci::scheduler::Scheduler,
}

impl worker::DurableObject for CiScheduler {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_ci::scheduler::Scheduler::new(state, env, settings::ci())
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

/// The Lander: landings, one live at a time, followed by its alarm. It runs CI's tools through
/// this Worker's capabilities, as `/mcp` would.
#[worker::durable_object]
pub struct Lander {
    inner: pigeon_capabilities::land::Lander,
}

impl worker::DurableObject for Lander {
    fn new(state: worker::State, env: Env) -> Self {
        let hosting = pigeon_capabilities::land::Hosting {
            worker: settings::WORKER,
            capabilities: settings::lander_capabilities,
            sources: SOURCES,
            bindings: settings::BINDINGS,
        };
        let inner = pigeon_capabilities::land::Lander::new(state, env, settings::land(), hosting)
            .expect("the Lander's migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.inner.alarm().await
    }
}

/// The token ledger: one shared Cloudflare token per caller and permission set
/// (docs/plans/token-minting.md). Core leases, releases and sweeps through it.
#[worker::durable_object]
pub struct TokenLedger {
    inner: pigeon_capabilities::ledger::TokenLedger,
}

impl worker::DurableObject for TokenLedger {
    fn new(state: worker::State, env: Env) -> Self {
        let inner =
            pigeon_capabilities::ledger::TokenLedger::new(state, env, settings::PARENT_TOKEN)
                .expect("the token ledger's migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }
}

/// Async model jobs' class (the index and each job's own object), kept only for an old
/// migration. It is served from `pigeon-model-gateway` now: its v1 migration transferred the
/// class there (`transferred_classes`), with its objects, and this Worker's `MODEL_JOBS` binding
/// names that Worker (`script_name`). Nothing here runs it.
///
/// It stays because this Worker's migration v4 (`src/platform/wrangler.toml`) still declares
/// `ModelJobs` in `new_sqlite_classes`, with no later `deleted_classes`, and no one has checked
/// that Cloudflare accepts an upload of a script that no longer exports a class its migrations
/// once created and that was transferred away (incident 5, `docs/plans/workers-split.md`; the
/// planned check is the second rehearsal in `docs/plans/do-rehearsal.md`). Removing it is the
/// person's decision, after that rehearsal passes. Never add a `deleted_classes` migration for it
/// here without a reviewed plan: a deletion is irreversible and could reach the gateway's objects.
#[worker::durable_object]
pub struct ModelJobs {
    inner: pigeon_models::runner::ModelJobs,
}

impl worker::DurableObject for ModelJobs {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_models::runner::ModelJobs::new(state, env, settings::model_jobs())
            .expect("the model jobs' migrations apply");
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
    inner: pigeon_ci::containers::Runner,
}

impl worker::DurableObject for CiRunner {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_ci::containers::Runner::new(state, env)
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
    // A capability Worker asking core, over its `PLATFORM` binding; never from the internet,
    // which reaches this Worker only on its own hostnames.
    if host == split::CORE_HOST {
        // `dispatch` runs any tool, so it needs the bound Workers' capabilities too (an AI agent
        // run's `models_submit` is the model gateway's); the proxy's checks need core's alone.
        let caps = split::for_op(
            &env,
            path.trim_start_matches('/'),
            settings::capabilities(),
            settings::BINDINGS,
        )
        .await;
        let hosted = hosted(&caps);
        let account = account(&env).await?;
        let cx = Context {
            account: &account,
            env: &env,
            hosted: &hosted,
            caller: None,
            caller_note: None,
            instance: None,
            worker: Some(&ctx),
        };
        return split::serve(req, &cx).await;
    }
    if !proxy::host_serves(&host, settings::PROXY_HOST, &path) {
        return Response::error("not found", 404);
    }
    let caps = split::all(&env, settings::capabilities(), settings::BINDINGS).await;
    // The apex is the public site's alone: none of the platform's paths answer there.
    if site::is_apex(&host, settings::ZONE) {
        let hosted = hosted(&caps);
        return settings::site().serve(&env, &hosted, req).await;
    }
    // A route is a prefix, so the root, the console's first tab, is matched by name.
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
            instance: None,
            worker: Some(&ctx),
        };
        return c.fetch(&cx, req).await;
    }
    match (req.method(), path.as_str()) {
        (Method::Get, "/health") => Response::ok("ok"), // route: open
        (Method::Options, "/mcp") => Ok(mcp_cors(Response::empty()?.with_status(204))), // route: open
        (Method::Post, "/mcp") => mcp_post(req, &env).await.map(mcp_cors), // route: OAuth
        (_, "/mcp") => Response::error("the MCP endpoint takes POST only", 405),
        _ => Response::error("not found", 404),
    }
}

/// Queue messages: each capability that wants one of the batch acts once. A failure fails the
/// batch, so the queue retries it.
/// The minute's cron processes pending site deploys and sweeps Pigeon's API tokens.
#[event(scheduled)]
async fn scheduled(_event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    sweep_tokens(&env).await;
    match pigeon_capabilities::github::run_pending(&env).await {
        Ok(ran) if !ran.is_empty() => console_log!("github deploys: {ran:?}"),
        Ok(_) => {}
        Err(e) => console_error!("github deploys failed: {e}"),
    }
}

/// Reconciles pending/released leases and expired tokens in the account ledger.
async fn sweep_tokens(env: &Env) {
    match pigeon_capabilities::ledger::reap(env).await {
        Ok(r) => console_log!(
            "token sweep: deleted {}, forgotten {}, failed {}",
            r.deleted,
            r.forgotten,
            r.failed
        ),
        Err(_) => {
            console_error!("token sweep failed; check tokens_status for the last successful sweep")
        }
    }
}

#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: WorkerContext) -> Result<()> {
    let messages = batch.messages()?;
    let caps = split::all(&env, settings::capabilities(), settings::BINDINGS).await;
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
        instance: None,
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
        bearer: None,
        token_ledger: Some(env.clone()),
    })
}

/// Lets a browser client such as ChatGPT call `/mcp`: any origin, the headers an MCP client sends,
/// and `WWW-Authenticate` readable so it can start the OAuth flow from a 401.
fn mcp_cors(res: Response) -> Response {
    let headers = res.headers().clone();
    let _ = headers.set("Access-Control-Allow-Origin", "*");
    let _ = headers.set(
        "Access-Control-Allow-Headers",
        "Authorization, Content-Type, MCP-Protocol-Version, Mcp-Session-Id",
    );
    let _ = headers.set("Access-Control-Allow-Methods", "POST, OPTIONS");
    let _ = headers.set("Access-Control-Expose-Headers", "WWW-Authenticate");
    res.with_headers(headers)
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
    let url = req.url()?;
    let mut targets = url.query_pairs().filter(|(key, _)| key == "instance");
    let instance = targets.next().map(|(_, value)| value.into_owned());
    if targets.next().is_some() {
        return Response::error("specify one target instance", 400);
    }
    let scope = if let Some(id) = &instance {
        let Some(record) = pigeon_capabilities::instances::one(env, id).await? else {
            return Response::error("unknown instance", 404);
        };
        Some(pigeon_capabilities::instances::tools::Scope::new(&record)?)
    } else {
        None
    };
    let message: Value = match req.json().await {
        Ok(v) => v,
        Err(e) => return Response::error(format!("not JSON: {e}"), 400),
    };
    let caps = split::all(env, settings::capabilities(), settings::BINDINGS).await;
    let hosted = hosted(&caps);
    let mut tools: Vec<mcp::Tool> = caps.iter().flat_map(|c| c.tools()).collect();
    if let Some(scope) = &scope {
        scope.restrict_tools(&mut tools);
    }
    pigeon_capabilities::approvals::annotate(&mut tools);
    let server = Server {
        name: settings::WORKER,
        version: env!("CARGO_PKG_VERSION"),
        instructions: if scope.is_some() {
            "The instance is selected by this endpoint. Tools use its approved resources."
        } else {
            "pigeon's platform. Source code lives in Cloudflare Artifacts; GitHub is \
                       a read-only copy of main. A cloud session reaches git at \
                       https://proxy.pigeon.dev/git/pigeon-security/pigeon.git and deploys with \
                       the pinned wrangler (`npm ci`, then `npx wrangler deploy`, in \
                       src/platform: 4.166.0) and \
                       CLOUDFLARE_API_BASE_URL=https://proxy.pigeon.dev/cf/client/v4; its \
                       environment's network secret for proxy.pigeon.dev authenticates both, so \
                       no token goes in a command. Push, rewrite and delete branches under \
                       claude/; main moves only by a landing: `land {branch}` takes a claude/ \
                       branch through CI's `test` job, into main and onto the deployed \
                       platform, with land_status following it. The person's policy, \
                       .pigeon/policy.toml, says what agents may do: call `policy_check` \
                       before acting and after any refusal. Act on `allow` as the person's \
                       standing instruction; on `deny`, stop and tell the person the rule and \
                       reason. Never propose new rules or loosen one; only the person changes \
                       the policy. `call {tool, args}` reaches any tool deployed after this \
                       session started. source_token remains for callers without the proxy \
                       credential."
        },
    };
    let call = async |name: &str, args: &Value| -> Result<Value> {
        let cx = Context {
            account: &account,
            env,
            hosted: &hosted,
            caller: Some(&caller),
            caller_note: None,
            instance: scope.as_ref(),
            worker: None,
        };
        pigeon_capabilities::dispatch(&cx, name, args).await
    };
    match mcp::answer(&server, &tools, &message, call).await {
        Some(mut answer) => {
            pigeon_capabilities::ci::annotate_discovery(scope.is_some(), &message, &mut answer);
            Response::from_json(&answer)
        }
        None => Ok(Response::empty()?.with_status(202)),
    }
}

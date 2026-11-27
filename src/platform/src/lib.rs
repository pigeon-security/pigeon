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

/// The Lander: landings, one live at a time, followed by its alarm. It runs CI's tools through
/// this Worker's capabilities, as `/mcp` would.
#[worker::durable_object]
pub struct Lander {
    inner: pigeon_capabilities::land::Lander,
    env: Env,
}

impl worker::DurableObject for Lander {
    fn new(state: worker::State, env: Env) -> Self {
        let hosting = pigeon_capabilities::land::Hosting {
            worker: settings::WORKER,
            capabilities: settings::lander_capabilities,
            sources: SOURCES,
            bindings: settings::BINDINGS,
        };
        let inner =
            pigeon_capabilities::land::Lander::new(state, env.clone(), settings::land(), hosting)
                .expect("the Lander's migrations apply");
        Self { inner, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let answer = self.inner.fetch(req).await;
        pigeon_capabilities::ledger::flush_calls(&self.env, false).await;
        answer
    }

    /// A landing's Artifacts and deploy calls are counted in this object's isolate, which may
    /// not be the Worker's: its counts go to the ledger from here.
    async fn alarm(&self) -> Result<Response> {
        let answer = self.inner.alarm().await;
        pigeon_capabilities::ledger::flush_calls(&self.env, false).await;
        answer
    }
}

/// The Approver: runs what an approval stands for once the person approves it, from its alarm,
/// as the caller that asked (docs/plans/approvals-auto-run.md).
#[worker::durable_object]
pub struct Approver {
    inner: pigeon_capabilities::approver::Approver,
    env: Env,
}

impl worker::DurableObject for Approver {
    fn new(state: worker::State, env: Env) -> Self {
        let hosting = pigeon_capabilities::land::Hosting {
            worker: settings::WORKER,
            capabilities: settings::capabilities,
            sources: SOURCES,
            bindings: settings::BINDINGS,
        };
        let inner = pigeon_capabilities::approver::Approver::new(
            state,
            env.clone(),
            hosting,
            settings::PARENT_TOKEN,
        )
        .expect("the Approver's migrations apply");
        Self { inner, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        let answer = self.inner.alarm().await;
        pigeon_capabilities::ledger::flush_calls(&self.env, false).await;
        answer
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
    let answer = route(req, env.clone(), &ctx).await;
    // This isolate's Cloudflare API call counts, at most once a minute, after the answer.
    ctx.wait_until(async move { pigeon_capabilities::ledger::flush_calls(&env, false).await });
    answer
}

async fn route(req: Request, env: Env, ctx: &WorkerContext) -> Result<Response> {
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
            worker: Some(ctx),
        };
        return split::serve(req, &cx).await;
    }
    if !proxy::host_serves(&host, settings::PROXY_HOST, &path) {
        return Response::error("not found", 404);
    }
    if host == settings::HOST && path == pigeon_capabilities::ingest::digitalocean::BOOT_PATH {
        if req.method() == Method::Post {
            return pigeon_capabilities::ingest::digitalocean::bootstrap(&env, settings::HOST, req)
                .await;
        }
        return Response::error("method not allowed", 405);
    }
    if host == settings::HOST && path == pigeon_capabilities::digitalocean::health::CALLBACK_PATH {
        if req.method() != Method::Post {
            return Response::error("method not allowed", 405);
        }
        return pigeon_capabilities::digitalocean::health::callback(&env, req).await;
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
            worker: Some(ctx),
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
/// The minute's cron processes pending site deploys and sweeps Pigeon's API tokens, then sends
/// this isolate's Cloudflare API call counts to the ledger.
#[event(scheduled)]
async fn scheduled(_event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    sweep_tokens(&env).await;
    match pigeon_capabilities::github::run_pending(&env).await {
        Ok(ran) if !ran.is_empty() => console_log!("github deploys: {ran:?}"),
        Ok(_) => {}
        Err(e) => console_error!("github deploys failed: {e}"),
    }
    if pigeon_capabilities::ingest::scaleway::reap(&env, settings::HOST)
        .await
        .is_err()
    {
        console_error!("Scaleway ingest maintenance failed");
    }
    if pigeon_capabilities::digitalocean::health::reap(&env, settings::HOST)
        .await
        .is_err()
    {
        console_error!("Droplet health maintenance tick failed; check the instance's status");
    }
    if pigeon_capabilities::ingest::digitalocean::reap(&env, settings::HOST)
        .await
        .is_err()
    {
        console_error!("DigitalOcean ingest maintenance tick failed");
    }
    pigeon_capabilities::ledger::flush_calls(&env, true).await;
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
    use pigeon_capabilities::cloud_scope;
    let global_discovery = scope.is_none() && message["method"] == "tools/list";
    let show_global_cloud = !global_discovery || cloud_scope::platform_enabled(env).await?;
    let mut tools: Vec<mcp::Tool> = caps
        .iter()
        .filter(|c| scope.is_some() || c.name() != pigeon_capabilities::digitalocean::NAME)
        .filter(|c| cloud_scope::discovered_globally(c.name(), show_global_cloud))
        .flat_map(|c| c.tools())
        .collect();
    use pigeon_capabilities::federation;
    let federated_request = scope.is_none()
        && bearer.is_some()
        && (global_discovery
            || message["params"]["name"].as_str().is_some_and(|name| {
                name == federation::DISCOVER || federation::READS.contains(&name)
            }));
    let available = if federated_request {
        Some(federation::Available::load(env, &caller, true).await?)
    } else {
        None
    };
    if let Some(scope) = &scope {
        scope.restrict_tools(&mut tools);
    } else if let Some(available) = &available {
        tools.extend(available.tools(&hosted));
    }
    pigeon_capabilities::approvals::annotate(&mut tools);
    let server = Server {
        name: settings::WORKER,
        version: env!("CARGO_PKG_VERSION"),
        instructions: if scope.is_some() {
            "The instance is selected by this endpoint. Tools use its approved resources."
        } else {
            "pigeon's platform. Source code lives in Cloudflare Artifacts; GitHub is \
                       a read-only copy of main. A coding agent pushes with `branch_push` (a \
                       git bundle of its commits, no token) to its app's namespace: claude/ \
                       for Claude, codex/ for Codex; for a push too large for that, \
                       `session_credential` mints a short-lived git credential for \
                       https://proxy.pigeon.dev/git/pigeon-security/pigeon.git. A cloud \
                       session holding the person's proxy credential (a network secret for \
                       proxy.pigeon.dev, break-glass) can also push there and deploy with the \
                       pinned wrangler (`npm ci`, then `npx wrangler deploy`, in src/platform: \
                       4.166.0) and CLOUDFLARE_API_BASE_URL=https://proxy.pigeon.dev/cf/client/v4. \
                       main moves only by a landing: `land {branch, commit}` takes a claude/ \
                       or codex/ branch through CI's `test` job, into main and onto the \
                       deployed platform, with land_status following it. The person's policy, \
                       .pigeon/policy.toml, says what agents may do: call `policy_check` \
                       before acting and after any refusal. Act on `allow` as the person's \
                       standing instruction; on `deny`, stop and tell the person the rule and \
                       reason. Never propose new rules or loosen one; only the person changes \
                       the policy. `call {tool, args}` reaches any tool deployed after this \
                       session started. Never call source_token or deploy_token unless the \
                       person asks for break-glass."
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
        if let Some(available) = &available
            && (name == federation::DISCOVER || federation::READS.contains(&name))
        {
            return available.call(&cx, name, args).await;
        }
        pigeon_capabilities::dispatch_checked(&cx, name, args).await
    };
    match mcp::answer(&server, &tools, &message, call).await {
        Some(mut answer) => {
            pigeon_capabilities::ci::annotate_discovery(scope.is_some(), &message, &mut answer);
            Response::from_json(&answer)
        }
        None => Ok(Response::empty()?.with_status(202)),
    }
}

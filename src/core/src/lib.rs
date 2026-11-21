//! The pigeon Worker: its entry point and bindings. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-core builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use pigeon_capabilities::artifacts::{self, Access};
use pigeon_capabilities::domain;
use pigeon_capabilities::mcp;
use pigeon_capabilities::mirror::{self, Copy, Report, Source};
use pigeon_capabilities::queues;
use pigeon_capabilities::subdomain::{self, Served, Spec};
use pigeon_capabilities::zero_trust::Protection;
use pigeon_capabilities::{Account, secrets_store};
use serde::Serialize;
use serde_json::Value;
use worker::{
    Context, Env, Error, MessageBatch, Method, Request, Response, Result, console_error,
    console_log, event,
};

/// The Secrets Store binding holding the parent token.
const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-platform";

/// The site Worker, which serves the zone's apex.
const SITE_WORKER: &str = "pigeon";

/// The source repository in Artifacts, imported from GitHub when created.
const SOURCE_NAMESPACE: &str = "pigeon-security";
const SOURCE_REPO: &str = "pigeon";
const SOURCE_BRANCH: &str = "main";
const SOURCE_GITHUB: &str = "https://github.com/pigeon-security/pigeon";

/// The read-only copy on GitHub, for disaster recovery, and the Secrets Store binding of the
/// token that pushes to it.
const MIRROR_URL: &str = "https://github.com/pigeon-security/pigeon.git";
const MIRROR_TOKEN: &str = "GITHUB_MIRROR_TOKEN";

/// The queue Artifacts push events go to; this Worker consumes it.
const SOURCE_EVENTS: &str = "pigeon-source-events";

/// How long a git token lives unless asked otherwise, and at most.
const GIT_TOKEN_TTL: u64 = 900;
const GIT_TOKEN_MAX_TTL: u64 = 86_400;

/// The service token Claude sessions reach the platform with, and the one header they send it
/// in, as `{"cf-access-client-id": ..., "cf-access-client-secret": ...}`.
const CLAUDE_SERVICE_TOKEN: &str = "pigeon-claude";
const ACCESS_TOKEN_HEADER: &str = "cf-access-token";

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        (Method::Post, "/setup/platform") => match setup_platform(&env).await {
            Ok(served) => Response::from_json(&served),
            Err(e) => Response::error(format!("setup failed: {e}"), 500),
        },
        (Method::Post, "/setup/site") => match setup_site(&env).await {
            Ok(served) => Response::from_json(&served),
            Err(e) => Response::error(format!("setup failed: {e}"), 500),
        },
        (Method::Post, "/setup/source") => json(setup_source(&env).await),
        (Method::Post, "/source/token") => json(source_token(&env, &req).await),
        (Method::Get, "/source/tokens") => json(source_tokens(&env).await),
        (Method::Delete, path) if path.starts_with("/source/tokens/") => {
            let id = &path["/source/tokens/".len()..];
            json(source_revoke(&env, id).await)
        }
        (Method::Post, "/source/mirror") => json(source_mirror(&env).await),
        (Method::Post, "/mcp") => mcp_post(req, &env).await,
        (_, "/mcp") => Response::error("the MCP endpoint takes POST only", 405),
        _ => Response::error("not found", 404),
    }
}

/// Artifacts push events: a push to the source's branch or a tag mirrors it to GitHub, once per
/// batch. A failed mirror fails the batch, so the queue retries it; nothing waits on it.
#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> Result<()> {
    let pushed = batch.messages()?.iter().any(|m| mirrors(m.body()));
    if !pushed {
        return Ok(());
    }
    match source_mirror(&env).await {
        Ok(report) => {
            console_log!(
                "mirror: {}",
                serde_json::to_string(&report).unwrap_or_default()
            );
            Ok(())
        }
        Err(e) => {
            console_error!("mirror failed: {e}");
            Err(e)
        }
    }
}

/// Whether push event `event` moved the source's branch or a tag of the source repository.
fn mirrors(event: &Value) -> bool {
    let p = event.get("payload").unwrap_or(event);
    let r#ref = p["ref"].as_str().unwrap_or_default();
    // Artifacts names the repository in the event's `source`; older shapes put it in the payload.
    let ours = ["repoName", "repo_name", "repo", "repository"]
        .iter()
        .find_map(|k| {
            event["source"][*k]
                .as_str()
                .or_else(|| p[*k].as_str())
                .or_else(|| p[*k]["name"].as_str())
        })
        .is_none_or(|repo| repo == SOURCE_REPO || repo.ends_with(&format!("/{SOURCE_REPO}")));
    ours && (r#ref == format!("refs/heads/{SOURCE_BRANCH}") || r#ref.starts_with("refs/tags/"))
}

/// Brings the GitHub copy up to the Artifacts source; does nothing when it is current.
async fn source_mirror(env: &Env) -> Result<Report> {
    let token = secrets_store::read(&env.secret_store(MIRROR_TOKEN)?)
        .await?
        .ok_or_else(|| {
            Error::RustError(format!("`{MIRROR_TOKEN}` is not set in the Secrets Store"))
        })?;
    let source = Source {
        namespace: SOURCE_NAMESPACE.into(),
        repo: SOURCE_REPO.into(),
        branches: vec![SOURCE_BRANCH.into()],
    };
    let copy = Copy {
        url: MIRROR_URL.into(),
        token,
    };
    mirror::mirror(&account(env).await?, &source, &copy).await
}

/// The account this Worker acts on, with the parent token read from its binding.
async fn account(env: &Env) -> Result<Account> {
    let parent = secrets_store::read(&env.secret_store(PARENT_TOKEN)?)
        .await?
        .ok_or_else(|| {
            Error::RustError(format!("`{PARENT_TOKEN}` is not set in the Secrets Store"))
        })?;
    Ok(Account {
        id: env.var("CF_ACCOUNT_ID")?.to_string(),
        parent,
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
    })
}

/// Serves the platform hostname from this Worker, behind Access; idempotent.
async fn setup_platform(env: &Env) -> Result<Served> {
    let spec = Spec {
        zone: env.var("ZONE")?.to_string(),
        host: env.var("PLATFORM_HOST")?.to_string(),
        worker: WORKER.into(),
        protection: Protection {
            app: "pigeon-platform".into(),
            service_token: "pigeon-platform".into(),
            client_id_secret: "PLATFORM_ACCESS_CLIENT_ID".into(),
            client_secret_secret: "PLATFORM_ACCESS_CLIENT_SECRET".into(),
            guests: vec![CLAUDE_SERVICE_TOKEN.into()],
            token_header: Some(ACCESS_TOKEN_HEADER.into()),
        },
    };
    subdomain::serve(&account(env).await?, &spec).await
}

/// Serves the zone's apex from the site Worker, public; idempotent. Removes the Access setup the
/// apex had, if it is still there.
async fn setup_site(env: &Env) -> Result<domain::Served> {
    let retired = Protection {
        app: "pigeon".into(),
        service_token: "pigeon-site".into(),
        client_id_secret: "SITE_ACCESS_CLIENT_ID".into(),
        client_secret_secret: "SITE_ACCESS_CLIENT_SECRET".into(),
        guests: Vec::new(),
        token_header: None,
    };
    let zone = env.var("ZONE")?.to_string();
    domain::serve(&account(env).await?, &zone, SITE_WORKER, Some(&retired)).await
}

/// `result` as a JSON response, or its error as a 500.
fn json<T: Serialize>(result: Result<T>) -> Result<Response> {
    match result {
        Ok(v) => Response::from_json(&v),
        Err(e) => Response::error(format!("failed: {e}"), 500),
    }
}

/// What [`setup_source`] did.
#[derive(Serialize)]
struct SourceSetup {
    repo: artifacts::Ensured,
    events: queues::Ensured,
    subscription: queues::Subscribed,
}

/// Has the source repository exist in Artifacts, imported from GitHub when created, the queue
/// its push events go to, consumed by this Worker, and the subscription that sends them there;
/// idempotent.
async fn setup_source(env: &Env) -> Result<SourceSetup> {
    let account = account(env).await?;
    let spec = artifacts::Spec {
        namespace: SOURCE_NAMESPACE.into(),
        name: SOURCE_REPO.into(),
        default_branch: SOURCE_BRANCH.into(),
        description: Some("pigeon-security/pigeon".into()),
        import_from: Some(SOURCE_GITHUB.into()),
    };
    let repo = artifacts::ensure(&account, &spec).await?;
    let events = queues::Spec {
        queue: SOURCE_EVENTS.into(),
        worker: WORKER.into(),
        batch_size: 10,
        max_wait_ms: 5_000,
        max_retries: 5,
    };
    let events = queues::ensure(&account, &events).await?;
    let subscription = queues::Subscription {
        name: format!("{SOURCE_NAMESPACE}-{SOURCE_REPO}-pushed"),
        queue_id: events.queue_id.clone(),
        sources: artifacts::events_source(SOURCE_NAMESPACE, SOURCE_REPO),
        events: vec!["pushed".into()],
        source_permissions: artifacts::EVENTS_PERMISSIONS.map(String::from).to_vec(),
    };
    let subscription = queues::subscribe(&account, &subscription).await?;
    Ok(SourceSetup {
        repo,
        events,
        subscription,
    })
}

/// A git token handed to the caller, which is behind Access.
#[derive(Serialize)]
struct Handed {
    id: Option<String>,
    token: String,
    access: Access,
    ttl_secs: u64,
    remote: Option<String>,
}

/// Mints a git token for the source repository: `?access=read|write` (read by default) and
/// `?ttl=<seconds>` (at most [`GIT_TOKEN_MAX_TTL`]).
async fn source_token(env: &Env, req: &Request) -> Result<Handed> {
    let url = req.url()?;
    let mut access = None;
    let mut ttl = None;
    for (k, v) in url.query_pairs() {
        match (k.as_ref(), v.as_ref()) {
            ("access", a) => access = Some(a.to_string()),
            ("ttl", s) => {
                ttl = Some(
                    s.parse()
                        .map_err(|_| Error::RustError(format!("ttl `{s}` is not a number")))?,
                );
            }
            (k, v) => return Err(Error::RustError(format!("unknown parameter `{k}={v}`"))),
        }
    }
    mint_source_token(env, access.as_deref(), ttl).await
}

/// Mints a git token for the source repository: `access` is `read` (the default) or `write`,
/// living `ttl` seconds (15 minutes by default, 24 hours at most).
async fn mint_source_token(env: &Env, access: Option<&str>, ttl: Option<u64>) -> Result<Handed> {
    let access = match access.unwrap_or("read") {
        "read" => Access::Read,
        "write" => Access::Write,
        other => {
            return Err(Error::RustError(format!(
                "access `{other}` is not read or write"
            )));
        }
    };
    let ttl = ttl.unwrap_or(GIT_TOKEN_TTL).min(GIT_TOKEN_MAX_TTL);
    let account = account(env).await?;
    let minted = artifacts::mint(&account, SOURCE_NAMESPACE, SOURCE_REPO, access, ttl).await?;
    let remote = artifacts::repo(&account, SOURCE_NAMESPACE, SOURCE_REPO)
        .await?
        .map(|r| r.remote);
    Ok(Handed {
        id: minted.id,
        token: minted.value,
        access: minted.access,
        ttl_secs: minted.ttl_secs,
        remote,
    })
}

/// The source repository's git tokens, without their values.
async fn source_tokens(env: &Env) -> Result<Vec<artifacts::Listed>> {
    artifacts::tokens(&account(env).await?, SOURCE_NAMESPACE, SOURCE_REPO).await
}

/// Revokes git token `id` of the source repository.
async fn source_revoke(env: &Env, id: &str) -> Result<serde_json::Value> {
    artifacts::revoke(&account(env).await?, SOURCE_NAMESPACE, SOURCE_REPO, id).await?;
    Ok(serde_json::json!({ "revoked": id }))
}

/// The MCP server's tools: the platform's operations.
fn mcp_tools() -> Vec<mcp::Tool> {
    vec![
        mcp::Tool {
            name: "source_token",
            description: "Mint a git token for the pigeon-security/pigeon Artifacts repository. \
                          Git sends it as `Authorization: Bearer <token>`. Answers the token, \
                          its id and the repository's remote.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "access": { "type": "string", "enum": ["read", "write"], "default": "read" },
                    "ttl": { "type": "integer", "description": "Seconds; 900 by default, 86400 at most." }
                }
            }),
        },
        mcp::Tool::bare(
            "source_tokens",
            "List the repository's git tokens, live and past, without their values.",
        ),
        mcp::Tool {
            name: "source_revoke",
            description: "Revoke one of the repository's git tokens by id.",
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        },
        mcp::Tool::bare(
            "source_mirror",
            "Bring the read-only GitHub copy up to the Artifacts repository now; it also runs on \
             every push.",
        ),
        mcp::Tool::bare(
            "setup_source",
            "Make sure the Artifacts repository, its push-event queue and subscription exist.",
        ),
    ]
}

/// One MCP message, answered as JSON; a notification gets 202 and no body.
async fn mcp_post(mut req: Request, env: &Env) -> Result<Response> {
    let message: Value = match req.json().await {
        Ok(v) => v,
        Err(e) => return Response::error(format!("not JSON: {e}"), 400),
    };
    let server = mcp::Server {
        name: "pigeon-platform",
        version: env!("CARGO_PKG_VERSION"),
        instructions: "pigeon's platform. Source code lives in Cloudflare Artifacts; GitHub is \
                       a read-only copy. To push, mint a write token with source_token and send \
                       it to git as `Authorization: Bearer <token>`.",
    };
    let tools = mcp_tools();
    let call = async |name: &str, args: &Value| -> Result<Value> {
        let value = match name {
            "source_token" => {
                let ttl = args["ttl"].as_u64();
                to_value(mint_source_token(env, args["access"].as_str(), ttl).await?)
            }
            "source_tokens" => to_value(source_tokens(env).await?),
            "source_revoke" => {
                let id = args["id"]
                    .as_str()
                    .ok_or_else(|| Error::RustError("`id` is required".into()))?;
                source_revoke(env, id).await?
            }
            "source_mirror" => to_value(source_mirror(env).await?),
            "setup_source" => to_value(setup_source(env).await?),
            other => return Err(Error::RustError(format!("no tool `{other}`"))),
        };
        Ok(value)
    };
    match mcp::answer(&server, &tools, &message, call).await {
        Some(answer) => Response::from_json(&answer),
        None => Ok(Response::empty()?.with_status(202)),
    }
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

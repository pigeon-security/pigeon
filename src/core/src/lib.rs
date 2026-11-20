//! The pigeon Worker: its entry point and bindings. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-core builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use pigeon_capabilities::subdomain::{self, Served, Spec};
use pigeon_capabilities::zero_trust::Protection;
use pigeon_capabilities::{Account, secrets_store};
use worker::{Context, Env, Error, Method, Request, Response, Result, event};

/// The Secrets Store binding holding the parent token.
const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-core";

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        (Method::Post, "/setup/platform") => match setup_platform(&env).await {
            Ok(served) => Response::from_json(&served),
            Err(e) => Response::error(format!("setup failed: {e}"), 500),
        },
        _ => Response::error("not found", 404),
    }
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
        zone: env.var("PLATFORM_ZONE")?.to_string(),
        host: env.var("PLATFORM_HOST")?.to_string(),
        worker: WORKER.into(),
        protection: Protection {
            app: "pigeon-platform".into(),
            service_token: "pigeon-platform".into(),
            client_id_secret: "PLATFORM_ACCESS_CLIENT_ID".into(),
            client_secret_secret: "PLATFORM_ACCESS_CLIENT_SECRET".into(),
        },
    };
    subdomain::serve(&account(env).await?, &spec).await
}

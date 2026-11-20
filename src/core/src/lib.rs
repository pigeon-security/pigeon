//! The pigeon Worker: its entry point and bindings. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-core builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use pigeon_capabilities::tokens::{self, MintRequest, Minted};
use pigeon_capabilities::{domain, secrets_store, subdomain, zero_trust};
use serde_json::{Value, json};
use worker::{Context, Env, Error, Method, Request, Response, Result, event};

/// The Secrets Store binding holding the parent token.
const PARENT_TOKEN: &str = "CF_PARENT_TOKEN";

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-core";

/// The platform's Access objects and the Secrets Store names of its service token.
const PLATFORM_APP: &str = "pigeon-platform";
const PLATFORM_SERVICE_TOKEN: &str = "pigeon-platform";
const PLATFORM_CLIENT_ID: &str = "PLATFORM_ACCESS_CLIENT_ID";
const PLATFORM_CLIENT_SECRET: &str = "PLATFORM_ACCESS_CLIENT_SECRET";

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    match (req.method(), req.path().as_str()) {
        (Method::Get, "/health") => Response::ok("ok"),
        (Method::Post, "/setup/platform") => match setup_platform(&env).await {
            Ok(report) => Response::from_json(&report),
            Err(e) => Response::error(format!("setup failed: {e}"), 500),
        },
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
        .ok_or_else(|| {
            Error::RustError(format!("`{PARENT_TOKEN}` is not set in the Secrets Store"))
        })
}

/// Mints `request` from the parent token.
pub async fn mint(env: &Env, request: &MintRequest) -> Result<Minted> {
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

/// Makes the platform hostname exist behind Access; idempotent. In order, so the hostname is
/// never served before Access holds it:
/// 1. the zone is in the account and active;
/// 2. the service token exists, with its client ID and secret in the Secrets Store;
/// 3. the Access application on the hostname lets that token in;
/// 4. the hostname is served by this Worker.
async fn setup_platform(env: &Env) -> Result<Value> {
    let account = account(env)?;
    let zone_name = env.var("PLATFORM_ZONE")?.to_string();
    let host = env.var("PLATFORM_HOST")?.to_string();

    let t = mint(env, &MintRequest::new("domain", domain::policies())).await?;
    let zone = domain::zone(&t.value, &account, &zone_name).await;
    revoke(env, &t.id).await?;
    let zone = zone?;
    if !zone.is_active() {
        return Err(Error::RustError(format!(
            "zone `{zone_name}` is {}, not active",
            zone.status
        )));
    }

    let t = mint(env, &MintRequest::new("zero-trust", zero_trust::policies())).await?;
    let access = platform_access(env, &t.value, &account, &host).await;
    revoke(env, &t.id).await?;
    let (app_id, client_id) = access?;

    let t = mint(env, &MintRequest::new("subdomain", subdomain::policies())).await?;
    let domain_id = subdomain::attach_worker(&t.value, &account, &zone.id, &host, WORKER).await;
    revoke(env, &t.id).await?;
    let domain_id = domain_id?;

    Ok(json!({
        "zone": { "name": zone.name, "id": zone.id },
        "host": host,
        "access_app": app_id,
        "service_token": { "client_id": client_id, "secrets": [PLATFORM_CLIENT_ID, PLATFORM_CLIENT_SECRET] },
        "custom_domain": domain_id,
    }))
}

/// Steps 2 and 3 of [`setup_platform`], on a Zero Trust token: the app's ID and the service
/// token's client ID.
async fn platform_access(
    env: &Env,
    token: &str,
    account: &str,
    host: &str,
) -> Result<(String, String)> {
    let service = zero_trust::service_token(token, account, PLATFORM_SERVICE_TOKEN).await?;
    let writer = secrets_writer(env).await?;
    let stored = store_service_token(&writer, token, account, &service).await;
    writer.finish().await?;
    stored?;
    let policy =
        zero_trust::service_token_policy(token, account, PLATFORM_APP, &service.id).await?;
    let app = zero_trust::self_hosted_app(token, account, PLATFORM_APP, host, &policy).await?;
    Ok((app, service.client_id))
}

/// Keeps the service token's client ID and secret in the Secrets Store. An existing token whose
/// secret the store lacks is rotated, since its secret can't be read back.
async fn store_service_token(
    writer: &secrets_store::Writer,
    token: &str,
    account: &str,
    service: &zero_trust::ServiceToken,
) -> Result<()> {
    let secret = match &service.client_secret {
        Some(s) => s.clone(),
        None => {
            let have = writer.list().await?;
            if have.iter().any(|s| s.name == PLATFORM_CLIENT_SECRET) {
                return writer
                    .put(
                        PLATFORM_CLIENT_ID,
                        &service.client_id,
                        "platform service token",
                    )
                    .await;
            }
            zero_trust::rotate_service_token(token, account, &service.id).await?
        }
    };
    writer
        .put(
            PLATFORM_CLIENT_ID,
            &service.client_id,
            "platform service token",
        )
        .await?;
    writer
        .put(PLATFORM_CLIENT_SECRET, &secret, "platform service token")
        .await
}

//! pigeon-model-gateway: the models Worker (docs/plans/workers-split.md, phase 3). It serves
//! the models capability ([`pigeon_models::ModelGateway`]): requests to model providers
//! through one Cloudflare AI Gateway, their usage, and async model jobs, whose Durable Object
//! class core (pigeon-platform) still hosts, reached over the `MODEL_JOBS` binding.
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. It holds no parent
//! token: each Cloudflare call acts on a short-lived token core mints for the call in hand,
//! asked over its `PLATFORM` binding. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-model-gateway builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use pigeon_contract::{Answer, FailureKind};
use pigeon_core::secrets_store;
use pigeon_models::{Dialect, ModelGateway, Models, Provider};
use worker::{Context, Env, Request, Response, Result, event};

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";
/// The platform's hostname, as core's settings name it.
const HOST: &str = "platform.pigeon.dev";
/// The proxies' hostname, as core's settings name it.
const PROXY_HOST: &str = "proxy.pigeon.dev";

/// The models capability as this platform declares it: one gateway, one provider.
fn gateway() -> ModelGateway {
    ModelGateway {
        models: Models {
            gateway: "pigeon".into(),
            providers: vec![Provider {
                slug: "anthropic",
                alias: "default",
                dialect: Dialect::Anthropic,
                default_model: "claude-haiku-5-5",
                key_help: "A Claude Platform API key, from the Claude Console's API keys page.",
            }],
            secrets_page: format!("https://{HOST}/access#credentials"),
            host: HOST.into(),
            proxy_host: PROXY_HOST.into(),
        },
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let key = match env.secret_store(CALLER_KEY) {
        Ok(binding) => secrets_store::read(&binding).await?,
        Err(_) => None,
    };
    let Some(key) = key else {
        let answer = Answer::error(
            FailureKind::Failed,
            "pigeon-model-gateway has no caller key: CALLER_KEY is not set",
        );
        return Ok(Response::from_json(&answer)?.with_status(503));
    };
    pigeon_contract::serve(req, &env, &gateway(), key.as_bytes()).await
}

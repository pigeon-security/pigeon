//! pigeon-model-gateway: the models Worker (docs/plans/workers-split.md, phase 3). It serves
//! the models capability ([`pigeon_models::ModelGateway`]): requests to model providers
//! through one Cloudflare AI Gateway, their usage, and async model jobs, whose Durable Object
//! class ([`ModelJobs`]) it hosts: the class moved here from pigeon-platform with
//! `transferred_classes`, its objects and their rows with it (docs/plans/workers-split.md, T18).
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. It holds no parent
//! token: each Cloudflare call acts on a short-lived token core mints for the call in hand,
//! asked over its `PLATFORM` binding. Built for `wasm32-unknown-unknown` only.
//!
//! It deploys on its own: a landing that changes only this crate deploys only this Worker.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-model-gateway builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use pigeon_contract::{Answer, FailureKind};
use pigeon_core::ai_gateway::GatewayKind;
use pigeon_core::secrets_store;
use pigeon_models::runner::{self, Token};
use pigeon_models::{Billing, Dialect, Gateway, ModelGateway, Models, Provider, Route};
use serde_json::json;
use worker::{Context, Env, Request, Response, Result, event};
// The `durable_object` macro's expansion names `wasm_bindgen` at the crate root.
use worker::wasm_bindgen;

/// This Worker's script name: the service it signs as when no one is calling.
const WORKER: &str = "pigeon-model-gateway";
/// The AI Gateway's ID, as core's settings name it.
const AI_GATEWAY: &str = "pigeon";
/// The BYOK gateway for Anthropic keys.
const BYOK_GATEWAY: &str = "pigeon-anthropic";
/// When `pigeon` starts to bill credits; its logs before then were paid with stored keys. Far in
/// the future until the person has made the switch.
pub const CREDITS_SINCE: &str = "9999-12-31T00:00:00Z";

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";
/// The platform's hostname, as core's settings name it.
const HOST: &str = "platform.pigeon.dev";
/// The proxies' hostname, as core's settings name it.
const PROXY_HOST: &str = "proxy.pigeon.dev";

/// The models capability as this platform declares it: two gateways, three providers.
fn gateway() -> ModelGateway {
    ModelGateway {
        models: Models {
            gateways: vec![
                Gateway {
                    id: AI_GATEWAY.into(),
                    kind: GatewayKind::Credits,
                },
                Gateway {
                    id: BYOK_GATEWAY.into(),
                    kind: GatewayKind::Byok,
                },
            ],
            providers: vec![
                // The anthropic provider moves to `pigeon-anthropic` only after the person sets
                // that gateway's key slot (docs/plans/model-providers-cloudflare.md, decision 3).
                // Until then it stays on `pigeon` with its stored key, as before.
                Provider {
                    name: "anthropic",
                    gateway: AI_GATEWAY,
                    route: Route::Byok,
                    slug: "anthropic",
                    alias: "default",
                    dialect: Dialect::Anthropic,
                    default_model: "claude-haiku-5-5",
                    key_help: "A Claude Platform API key, from the Claude Console's API keys page.",
                    billing: Some(Billing::AnthropicAdmin),
                },
                Provider {
                    name: "cf-anthropic",
                    gateway: AI_GATEWAY,
                    route: Route::Credits,
                    slug: "anthropic",
                    alias: "default",
                    dialect: Dialect::Anthropic,
                    default_model: "claude-haiku-5-5",
                    key_help: "",
                    billing: None,
                },
                Provider {
                    name: "cf-openai",
                    gateway: AI_GATEWAY,
                    route: Route::Credits,
                    slug: "openai",
                    alias: "default",
                    dialect: Dialect::OpenAi,
                    default_model: "gpt-6.2-sol",
                    key_help: "",
                    billing: None,
                },
            ],
            staged: vec![(BYOK_GATEWAY, "anthropic", "default")],
            credits_since: CREDITS_SINCE.into(),
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

/// Async model jobs: the index and each job's own object. A job's model call acts on an
/// `ai_gateway` token core mints for it, asked as this Worker: the gateway holds no parent token.
#[worker::durable_object]
pub struct ModelJobs {
    inner: runner::ModelJobs,
}

impl worker::DurableObject for ModelJobs {
    fn new(state: worker::State, env: Env) -> Self {
        let settings = runner::Settings {
            gateway: AI_GATEWAY.into(),
            token: Token::Scoped(itself()),
        };
        let inner =
            runner::ModelJobs::new(state, env, settings).expect("the model jobs' migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.inner.alarm().await
    }
}

/// The caller this Worker signs its own `credential` requests as: itself, a service, acting for
/// no one, as pigeon-proxy does.
fn itself() -> serde_json::Value {
    json!({
        "user": { "id": format!("worker:{WORKER}"), "kind": "service", "name": WORKER, "created_at": 0 },
        "acting_for": null,
    })
}

//! pigeon-model-gateway: the models Worker (docs/plans/workers-split.md, phase 3). It serves
//! the models capability ([`pigeon_models::ModelGateway`]): requests to model providers
//! through Cloudflare AI Gateways, their usage, and async model jobs, whose Durable Object
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
use pigeon_models::platform_gateways::{BYOK_GATEWAY, CREDITS_GATEWAY, GATEWAYS, OPENAI_GATEWAY};
use pigeon_models::runner::{self, Token};
use pigeon_models::{Billing, Dialect, ModelGateway, ModelIds, Models, Provider, Route};
use serde_json::json;
use worker::{Context, Env, Request, Response, Result, event};
// The `durable_object` macro's expansion names `wasm_bindgen` at the crate root.
use worker::wasm_bindgen;

/// This Worker's script name: the service it signs as when no one is calling.
const WORKER: &str = "pigeon-model-gateway";

/// The platform's hostname, as core's settings name it.
const HOST: &str = "platform.pigeon.dev";
/// The proxies' hostname, as core's settings name it.
const PROXY_HOST: &str = "proxy.pigeon.dev";

/// The models capability as this platform declares it: four gateways, five providers.
fn gateway() -> ModelGateway {
    ModelGateway {
        models: Models {
            gateways: GATEWAYS.iter().map(|g| g.gateway()).collect(),
            providers: vec![
                // Anthropic BYOK goes through `pigeon-anthropic` and its key slot
                // `pigeon-anthropic_anthropic_default` (docs/plans/model-providers-cloudflare.md,
                // decision 3).
                Provider {
                    name: "anthropic",
                    gateway: BYOK_GATEWAY,
                    route: Route::Byok,
                    slug: "anthropic",
                    alias: "default",
                    dialect: Dialect::Anthropic,
                    ids: ModelIds::Claude,
                    default_model: "claude-haiku-5-5",
                    key_help: "A Claude Platform API key, from the Claude Console's API keys page.",
                    billing: Some(Billing::AnthropicAdmin),
                },
                Provider {
                    name: "cf-anthropic",
                    gateway: CREDITS_GATEWAY,
                    route: Route::Credits,
                    slug: "anthropic",
                    alias: "default",
                    dialect: Dialect::Anthropic,
                    ids: ModelIds::Claude,
                    default_model: "claude-haiku-5-5",
                    key_help: "",
                    billing: None,
                },
                Provider {
                    name: "cf-openai",
                    gateway: CREDITS_GATEWAY,
                    route: Route::Credits,
                    slug: "openai",
                    alias: "default",
                    dialect: Dialect::OpenAi,
                    ids: ModelIds::Plain,
                    default_model: "gpt-6.2-sol",
                    key_help: "",
                    billing: None,
                },
                // Workers AI: the REST API, paid with credits through `pigeon-cloudflare`. Each
                // call uses a token core mints for it and revokes after. It holds no gateway key.
                Provider {
                    name: "workers-ai",
                    gateway: CREDITS_GATEWAY,
                    route: Route::Credits,
                    slug: "workers-ai",
                    alias: "default",
                    dialect: Dialect::OpenAi,
                    ids: ModelIds::WorkersAi,
                    default_model: "@cf/zai-org/glm-5.3",
                    key_help: "",
                    billing: None,
                },
                // OpenAI on the person's own key, through `pigeon-openai`. Latent until its key
                // slot `pigeon-openai_openai_default` is set (docs/plans/model-providers.md,
                // Part 4, decision 4). Tiers mirror `cf-openai`.
                Provider {
                    name: "openai",
                    gateway: OPENAI_GATEWAY,
                    route: Route::Byok,
                    slug: "openai",
                    alias: "default",
                    dialect: Dialect::OpenAi,
                    ids: ModelIds::Plain,
                    default_model: "gpt-6.2-sol",
                    key_help: "An OpenAI API Platform key, from the OpenAI dashboard's API keys page.",
                    billing: None,
                },
            ],
            staged: Vec::new(),
            secrets_page: format!("https://{HOST}/secrets"),
            host: HOST.into(),
            proxy_host: PROXY_HOST.into(),
        },
    }
}

/// Names this Worker (`model-gateway` in ci/workers.toml) on every request it signs to core, before any
/// fetch or Durable Object code runs, so core checks it with CALLER_KEY_MODEL_GATEWAY first
/// (docs/plans/caller-keys.md, step 2). worker-build also exposes it as an entrypoint method of
/// this name, reachable only over a service binding; calling it again changes nothing.
#[event(start)]
fn identify_worker() {
    pigeon_contract::identify("model-gateway");
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // Core's calls are checked with this Worker's own key alone; with no own key it serves none
    // (docs/plans/caller-keys.md, steps 3 and 4).
    let keys = match pigeon_contract::keys::read(&env).await {
        Ok(keys) => keys,
        Err(e) => {
            let answer = Answer::error(FailureKind::Failed, format!("pigeon-model-gateway: {e}"));
            return Ok(Response::from_json(&answer)?.with_status(503));
        }
    };
    pigeon_contract::serve(req, &env, &gateway(), &keys).await
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
            gateway: BYOK_GATEWAY.into(),
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

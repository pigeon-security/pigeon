//! pigeon-ai-agents: the agents Worker (`docs/plans/ai-agents.md`). It serves the agents capability
//! ([`pigeon_ai_agents::service::Agents`]) and hosts its two Durable Object classes: `AiAgentRuns`
//! (the run index and one object per run, whose alarm drives the run) and `Sandbox` (one per
//! run, holding its container).
//!
//! A capability Worker: it has no public surface, and answers only core over a Service Binding,
//! through [`pigeon_contract::serve`], which checks the caller core signed. It holds no parent
//! token and no provider key: a run reaches models, CI and the policy only as its caller,
//! through core's `dispatch` over the `PLATFORM` binding. Built for `wasm32-unknown-unknown`
//! only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-ai-agents builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use pigeon_ai_agents::service::Agents;
use pigeon_contract::{Answer, FailureKind};
use worker::{Context, Env, Request, Response, Result, event, wasm_bindgen};

/// Names this Worker (`ai-agents` in ci/workers.toml) on every request it signs to core, before any
/// fetch or Durable Object code runs, so core checks it with CALLER_KEY_AI_AGENTS first
/// (docs/plans/caller-keys.md, step 2). worker-build also exposes it as an entrypoint method of
/// this name, reachable only over a service binding; calling it again changes nothing.
#[event(start)]
fn identify_worker() {
    pigeon_contract::identify("ai-agents");
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // Core's calls are checked with this Worker's own key, then the shared one; with no own
    // key it serves none (docs/plans/caller-keys.md, step 3).
    let keys = match pigeon_contract::keys::read(&env).await {
        Ok(keys) => keys,
        Err(e) => {
            let answer = Answer::error(FailureKind::Failed, format!("pigeon-ai-agents: {e}"));
            return Ok(Response::from_json(&answer)?.with_status(503));
        }
    };
    pigeon_contract::serve(req, &env, &Agents, &keys).await
}

/// The runs: the index and one object per run.
#[worker::durable_object]
pub struct AiAgentRuns {
    inner: pigeon_ai_agents::object::AiAgentRuns,
}

impl worker::DurableObject for AiAgentRuns {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_ai_agents::object::AiAgentRuns::new(state, env)
            .expect("the AI agent runs' migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }

    async fn alarm(&self) -> Result<Response> {
        self.inner.alarm().await
    }
}

/// A run's sandbox: one object per run, holding its container.
#[worker::durable_object]
pub struct Sandbox {
    inner: pigeon_ai_agents::sandbox::Sandbox,
}

impl worker::DurableObject for Sandbox {
    fn new(state: worker::State, env: Env) -> Self {
        let inner = pigeon_ai_agents::sandbox::Sandbox::new(state, env)
            .expect("the sandbox's migrations apply");
        Self { inner }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        self.inner.fetch(req).await
    }
}

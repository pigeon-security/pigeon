//! pigeon-ci-gateway: the CI Worker (docs/plans/workers-split.md, phase 4). It serves CI's tools
//! and its `/ci/` routes ([`pigeon_ci::CiGateway`]) behind the contract, through
//! [`pigeon_contract::serve`], which checks the caller core signed. A capability Worker: it has
//! no public surface and answers only core over a Service Binding. It never holds the parent
//! token: it asks core for a scoped credential for the call in hand over its `PLATFORM`
//! binding, or as itself when no one is calling. It hosts the scheduler's class
//! ([`CiScheduler`]), moved here from pigeon-platform with `transferred_classes`, its jobs and
//! alarm with it (docs/plans/workers-split.md, phase 4). The runners, which hold the job
//! containers, are still core's Durable Objects, reached by binding.
//! Built for `wasm32-unknown-unknown` only.
//!
//! It deploys on its own: a landing that changes only this crate deploys only this Worker.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-ci-gateway builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use pigeon_ci::{Accounts, CiGateway};
use pigeon_contract::{Answer, FailureKind};
use pigeon_core::secrets_store;
use worker::{Context, Env, Request, Response, Result, event};
// The `durable_object` macro's expansion names `wasm_bindgen` at the crate root.
use worker::wasm_bindgen;

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";

/// Names this Worker (`ci-gateway` in ci/workers.toml) on every request it signs to core, before any
/// fetch or Durable Object code runs, so core checks it with CALLER_KEY_CI_GATEWAY first
/// (docs/plans/caller-keys.md, step 2). worker-build also exposes it as an entrypoint method of
/// this name, reachable only over a service binding; calling it again changes nothing.
#[event(start)]
fn identify_worker() {
    pigeon_contract::identify("ci-gateway");
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
            "pigeon-ci-gateway has no caller key: CALLER_KEY is not set",
        );
        return Ok(Response::from_json(&answer)?.with_status(503));
    };
    let key = key.into_bytes();
    let gateway = CiGateway::new(pigeon_ci::pigeon::settings(Accounts::Scoped), key.clone());
    pigeon_contract::serve(req, &env, &gateway, &key).await
}

/// The CI scheduler's class: `pigeon_ci::scheduler::Scheduler` with pigeon's connectors and
/// scoped accounts. Every CI job's row, and the alarm that dispatches and follows them.
#[worker::durable_object]
pub struct CiScheduler {
    inner: pigeon_ci::scheduler::Scheduler,
}

impl worker::DurableObject for CiScheduler {
    fn new(state: worker::State, env: Env) -> Self {
        let settings = pigeon_ci::pigeon::settings(Accounts::Scoped);
        let inner = pigeon_ci::scheduler::Scheduler::new(state, env, settings)
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

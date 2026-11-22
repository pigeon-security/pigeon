//! pigeon-ci-gateway: the CI Worker (docs/plans/workers-split.md, phase 4). It serves CI's tools
//! and its `/ci/` routes ([`pigeon_ci::CiGateway`]) behind the contract, through
//! [`pigeon_contract::serve`], which checks the caller core signed. A capability Worker: it has
//! no public surface and answers only core over a Service Binding. It never holds the parent
//! token: it asks core for a scoped credential for the call in hand over its `PLATFORM`
//! binding. The scheduler and the runners are still core's Durable Objects, reached by binding.
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

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";

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

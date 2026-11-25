//! pigeon-cloud-gateway: the cloud Worker (docs/plans/cloud-gateway.md). It serves the cloud
//! capability ([`pigeon_cloud::CloudGateway`]): Scaleway's API keys, buckets and objects, with
//! every change a plan the person approves first. A capability Worker: it has no public
//! surface and answers only core over a Service Binding, through [`pigeon_contract::serve`],
//! which checks the caller core signed. It holds no Cloudflare parent token: each Secrets Store
//! write acts on a short-lived token core mints for the call in hand, asked over its
//! `PLATFORM` binding. Built for `wasm32-unknown-unknown` only.
//!
//! It deploys on its own: a landing that changes only this crate deploys only this Worker.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-cloud-gateway builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use pigeon_cloud::CloudGateway;
use pigeon_contract::{Answer, FailureKind};
use pigeon_core::secrets_store;
use worker::{Context, Env, Request, Response, Result, event};

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";

/// Names this Worker (`cloud-gateway` in ci/workers.toml) on every request it signs to core, before any
/// fetch or Durable Object code runs, so core checks it with CALLER_KEY_CLOUD_GATEWAY first
/// (docs/plans/caller-keys.md, step 2). worker-build also exposes it as an entrypoint method of
/// this name, reachable only over a service binding; calling it again changes nothing.
#[event(start)]
fn identify_worker() {
    pigeon_contract::identify("cloud-gateway");
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
            "pigeon-cloud-gateway has no caller key: CALLER_KEY is not set",
        );
        return Ok(Response::from_json(&answer)?.with_status(503));
    };
    pigeon_contract::serve(req, &env, &CloudGateway::new(), key.as_bytes()).await
}

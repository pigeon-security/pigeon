//! pigeon-dotest-a: a throwaway Worker for the Durable Object transfer rehearsal
//! (docs/plans/do-rehearsal.md; docs/plans/workers-split.md, T17). Its SQLite class `Probe`
//! moved to pigeon-dotest-b with `transferred_classes`, and this Worker no longer exports it
//! (the second rehearsal: does Cloudflare take the upload of a source that keeps the class's
//! migration and binding but not its export?). It still answers on its workers.dev URL what
//! the object holds, through its `PROBE` binding, which names pigeon-dotest-b. It holds no
//! secrets and only dummy data; every route asks for the header `X-Rehearsal: dotest`, a speed
//! bump, not security. Built for `wasm32-unknown-unknown` only.
//!
//! A cron trigger asks the object for its state every five minutes and logs the answer as one
//! `console.error` line, `dotest-state {...}`, so `observability_errors` for this Worker shows
//! what `GET /state` would answer without anyone reaching workers.dev.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-dotest-a builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use serde_json::{Value, json};
use worker::{Context, Env, Method, Request, Response, Result, event};

// Redeployed by a landing that touches only this crate (second rehearsal, fallback deploy).
/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-dotest-a";
/// The Durable Object binding.
const BINDING: &str = "PROBE";
/// The one object the rehearsal uses.
const OBJECT: &str = "probe";
/// The header every route asks for, and its value.
const HEADER: (&str, &str) = ("X-Rehearsal", "dotest");

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if req.headers().get(HEADER.0)?.as_deref() != Some(HEADER.1) {
        return Response::error("not found", 404);
    }
    match (req.method(), req.path().as_str()) {
        (Method::Post, "/seed") => {} // route: open
        (Method::Get, "/state") => {} // route: open
        _ => return Response::error("not found", 404),
    }
    ask(&env, req).await
}

/// Every five minutes (`[triggers]`): asks the object `/tick` through this Worker's binding and
/// logs the answer, which is what `GET /state` answers, as one error line, so
/// `observability_errors` lists it (the same state each time folds into one line with a count).
#[event(scheduled)]
async fn scheduled(_event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    let answer = async {
        let req = Request::new("https://probe/tick", Method::Get)?;
        let mut answer = ask(&env, req).await?;
        let status = answer.status_code();
        let text = answer.text().await?;
        Ok::<_, worker::Error>(if status == 200 {
            text
        } else {
            json!({ "worker": WORKER, "status": status, "body": text }).to_string()
        })
    }
    .await;
    match answer {
        Ok(state) => worker::console_error!("dotest-state {state}"),
        Err(e) => {
            let e = json!({ "worker": WORKER, "error": e.to_string() });
            worker::console_error!("dotest-state {e}")
        }
    }
}

/// Sends `req` to the object named [`OBJECT`] through [`BINDING`], and adds to its answer which
/// Worker asked and the id its binding gives that name.
async fn ask(env: &Env, req: Request) -> Result<Response> {
    let namespace = env.durable_object(BINDING)?;
    let id = namespace.id_from_name(OBJECT)?;
    let named = id.to_string();
    let mut answer = id.get_stub()?.fetch_with_request(req).await?;
    if answer.status_code() != 200 {
        return Ok(answer);
    }
    let mut object: Value = answer.json().await?;
    object["worker"] = json!(WORKER);
    object["id_from_name"] = json!(named);
    Response::from_json(&object)
}

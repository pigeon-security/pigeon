//! Core's side of the Workers split (docs/plans/workers-split.md): the capability Workers' tools
//! merged with this Worker's own, the two ops a capability Worker asks core (`credential` and
//! `dispatch`, at `https://core/<op>` over its `PLATFORM` binding), and two tools:
//! `workers_setup`, which makes the key that signs callers, and `manifest_refresh`.
//!
//! Core stays the front door. A capability Worker's call to `dispatch` runs through the same
//! [`crate::dispatch`] as `/mcp`, gate included, as the caller core signed for it. A
//! `credential` is only for a signed caller, and only for the call in hand.

use pigeon_contract::{Answer, CORE_OPS, Credential, CredentialRequest, FailureKind};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result};

use crate::remote::{self, Binding, CALLER_KEY};
use crate::users::Caller;
use crate::{Call, Capability, Context, Footprint, Slot, unknown};

/// The host a capability Worker's requests to core carry, over its `PLATFORM` binding. A
/// request from the internet never has it: core is reached only on its own hostnames.
pub const CORE_HOST: &str = "core";

/// How long a linked account's access token is handed out for, at most, in milliseconds: the
/// call in hand, not the token's own life.
const LINKED_USE_MS: i64 = 10 * 60_000;

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now_ms() -> i64 {
    worker::Date::now().as_millis() as i64
}

/// The local capabilities with the remote ones behind `bindings` added after them. A remote
/// capability whose name, a tool or a route is already taken locally is left out, so a
/// capability Worker can never shadow core.
pub async fn all(
    env: &Env,
    local: Vec<Box<dyn Capability>>,
    bindings: &[Binding],
) -> Vec<Box<dyn Capability>> {
    if bindings.is_empty() {
        return local;
    }
    let mut out = local;
    for r in remote::capabilities(env, bindings).await {
        let tools: Vec<&str> = r.tools().iter().map(|t| t.name).collect();
        let taken = out.iter().any(|c| {
            c.name() == r.name()
                || c.tools().iter().any(|t| tools.contains(&t.name))
                || c.routes().iter().any(|x| r.routes().contains(x))
        });
        if taken {
            worker::console_error!(
                "split: remote capability {} clashes with a local one; left out",
                r.name()
            );
            continue;
        }
        out.push(r);
    }
    out
}

/// The caller key, from the Secrets Store binding [`CALLER_KEY`].
pub async fn caller_key(env: &Env) -> Result<Vec<u8>> {
    let binding = env
        .secret_store(CALLER_KEY)
        .map_err(|_| bad("CALLER_KEY is not bound: run workers_setup, then land its binding"))?;
    let key = secrets_store::read(&binding)
        .await?
        .ok_or_else(|| bad("CALLER_KEY is not set in the Secrets Store: run workers_setup"))?;
    Ok(key.into_bytes())
}

/// Serves one op a capability Worker asks core: `credential` or `dispatch`, with the caller
/// core signed for it. Every answer is JSON, as [`pigeon_contract::serve`] answers.
pub async fn serve(mut req: Request, cx: &Context<'_>) -> Result<Response> {
    let op = req.path().trim_start_matches('/').to_string();
    let reply = |a: Answer, status: u16| -> Result<Response> {
        Ok(Response::from_json(&a)?.with_status(status))
    };
    if !CORE_OPS.contains(&op.as_str()) {
        return reply(
            Answer::error(FailureKind::UnknownOp, format!("unknown op {op}")),
            200,
        );
    }
    let header = req.headers().get(pigeon_contract::HEADER)?;
    let body = req.bytes().await?;
    let key = caller_key(cx.env).await?;
    let signed = match pigeon_contract::verify(&key, header.as_deref(), &op, &body, now_ms()) {
        Ok(s) => s,
        Err(why) => {
            return reply(
                Answer::error(FailureKind::Unauthorized, why.to_string()),
                401,
            );
        }
    };
    let Ok(caller) = serde_json::from_value::<Caller>(signed.caller.clone()) else {
        return reply(
            Answer::error(
                FailureKind::Unauthorized,
                "core acts only for a signed caller, and this request has none",
            ),
            401,
        );
    };
    let cx = Context {
        caller: Some(&caller),
        caller_note: None,
        ..*cx
    };
    let answer = match op.as_str() {
        "dispatch" => {
            let asked: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400);
                }
            };
            let tool = asked["tool"].as_str().unwrap_or_default();
            let args = asked.get("args").cloned().unwrap_or_else(|| json!({}));
            crate::dispatch(&cx, tool, &args).await
        }
        _ => match serde_json::from_slice::<CredentialRequest>(&body) {
            Ok(r) => credential(&cx, &r).await,
            Err(e) => return reply(Answer::error(FailureKind::BadRequest, e.to_string()), 400),
        },
    };
    match answer {
        Ok(v) => reply(Answer::Ok(v), 200),
        Err(e) => reply(Answer::error(FailureKind::Failed, e.to_string()), 200),
    }
}

/// A short-lived credential for the call in hand. `linked`: the instance's linked-account
/// access token, refreshed first when due, with the account it acts on.
async fn credential(cx: &Context<'_>, r: &CredentialRequest) -> Result<Value> {
    match r.kind.as_str() {
        "linked" => {
            let instance = r
                .instance
                .as_deref()
                .ok_or_else(|| bad("a linked credential names its instance"))?;
            let host = cx.env.var("PLATFORM_HOST")?.to_string();
            let account = crate::linked::account_for(cx.env, &host, instance).await?;
            let token = account
                .bearer
                .clone()
                .ok_or_else(|| bad("the linked account came back without its access token"))?;
            Ok(serde_json::to_value(Credential {
                token,
                account_id: account.id.clone(),
                expires_at: now_ms() + LINKED_USE_MS,
            })?)
        }
        "github" => {
            let instance = r
                .instance
                .as_deref()
                .ok_or_else(|| bad("a github credential names its instance"))?;
            // GitHub's own expiry (an hour) is later; the token is used for one call, as a
            // linked one is.
            let (token, _) = crate::github::installation_token(cx.env, instance).await?;
            Ok(serde_json::to_value(Credential {
                token,
                account_id: String::new(),
                expires_at: now_ms() + LINKED_USE_MS,
            })?)
        }
        other => Err(bad(&format!("no credential of kind `{other}`"))),
    }
}

/// `workers_setup` and `manifest_refresh`.
pub struct Split {
    /// The capability Workers bound to this one.
    pub bindings: &'static [Binding],
}

impl Split {
    /// Makes [`CALLER_KEY`] in the Secrets Store when it is not there, readable by Workers. An
    /// existing key is kept: replacing it would refuse every call in flight.
    async fn setup(&self, cx: &Context<'_>) -> Result<Value> {
        let writer = Writer::open(cx.account).await?;
        let made = async {
            if writer.list().await?.iter().any(|s| s.name == CALLER_KEY) {
                return Ok(false);
            }
            let key = pigeon_core::webauthn::random()?;
            writer
                .put_scoped(
                    CALLER_KEY,
                    &key,
                    "signs the caller core forwards to capability Workers, by workers_setup",
                    &[secrets_store::WORKERS],
                )
                .await?;
            Ok(true)
        }
        .await;
        let made = writer.finish(made).await?;
        Ok(json!({
            "caller_key": if made { "made" } else { "already there; kept" },
            "next": "bind CALLER_KEY in each platform Worker's wrangler.toml (a binding to a \
                     missing secret fails the deploy, so it lands after this)",
        }))
    }
}

impl Capability for Split {
    fn name(&self) -> &'static str {
        "split"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "workers_setup",
                "Make the key that signs the caller core forwards to its capability Workers \
                 (CALLER_KEY, in the Secrets Store), when it is not there; an existing key is \
                 kept. Run once before the first capability Worker is bound.",
            ),
            Tool::bare(
                "manifest_refresh",
                "Ask every capability Worker for its tools again now, rather than within a \
                 minute: after a capability Worker deployed. Answers each Worker's capabilities \
                 and tools, or why it did not answer.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "workers_setup" => self.setup(cx).await,
                "manifest_refresh" => {
                    remote::forget();
                    let found = remote::capabilities(cx.env, self.bindings).await;
                    let answered: Vec<Value> = found
                        .iter()
                        .map(|c| {
                            json!({
                                "capability": c.name(),
                                "tools": c.tools().iter().map(|t| t.name).collect::<Vec<_>>(),
                            })
                        })
                        .collect();
                    let unreachable: Vec<Value> = remote::unreachable()
                        .into_iter()
                        .map(|(worker, why)| json!({ "worker": worker, "why": why }))
                        .collect();
                    Ok(json!({ "capabilities": answered, "unreachable": unreachable }))
                }
                other => Err(unknown(other)),
            }
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        Vec::new()
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            workers: self
                .bindings
                .iter()
                .map(|b| crate::WorkerFootprint {
                    name: b.worker.to_string(),
                    routes: Vec::new(),
                })
                .collect(),
            ..Footprint::default()
        }
    }
}

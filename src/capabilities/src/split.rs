//! Core's side of the Workers split (docs/plans/workers-split.md): the capability Workers' tools
//! merged with this Worker's own, the ops a capability Worker asks core (at
//! `https://core/<op>` over its `PLATFORM` binding), and two tools: `workers_setup`, which makes
//! the key that signs callers, and `manifest_refresh`.
//!
//! The ops: `credential` and `dispatch` for a signed caller; and the proxy Worker's checks,
//! `verify` (who holds a proxy credential), `grant` (a CI job token's grant) and `deploy_grant`
//! (the caller a landing's deploy job token stands for), signed with `CALLER_KEY` but for no
//! caller, since each carries the credential it asks about.
//!
//! Core stays the front door. A capability Worker's call to `dispatch` runs through the same
//! [`crate::dispatch`] as `/mcp`, gate included, as the caller core signed for it. A
//! `credential` is only for a signed caller, and only for the call in hand.

use pigeon_contract::{
    Answer, CALLERLESS_OPS, CORE_OPS, Credential, CredentialRequest, FailureKind, GrantRequest,
    VerifyRequest,
};
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
/// core signed for it; or one of the proxy Worker's checks, signed for no caller. Every answer
/// is JSON, as [`pigeon_contract::serve`] answers.
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
    if CALLERLESS_OPS.contains(&op.as_str()) {
        let answer = match proxy_check(cx.env, &op, &body).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                return reply(
                    Answer::error(FailureKind::BadRequest, "the request does not parse"),
                    400,
                );
            }
            Err(e) => return reply(Answer::error(FailureKind::Failed, e.to_string()), 200),
        };
        return reply(Answer::Ok(answer), 200);
    }
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

/// One of the proxy Worker's checks, `op` with `body`, answered as core's own proxies answer
/// it: the holder of a proxy credential, a job token's grant for a repository, or the caller a
/// landing's deploy job token stands for; null for none. `None` when the body does not parse.
async fn proxy_check(env: &Env, op: &str, body: &[u8]) -> Result<Option<Value>> {
    Ok(Some(match op {
        "verify" => {
            let Ok(r) = serde_json::from_slice::<VerifyRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::proxy::verify(env, &r.credential).await?)?
        }
        "grant" => {
            let Ok(r) = serde_json::from_slice::<GrantRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::proxy::job_grant(env, &r.token, &r.repo).await?)?
        }
        "deploy_grant" => {
            let Ok(r) = serde_json::from_slice::<GrantRequest>(body) else {
                return Ok(None);
            };
            serde_json::to_value(crate::land::deploy_caller(env, &r.token, &r.repo).await?)?
        }
        _ => return Ok(None),
    }))
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
        "scoped" => scoped(cx, &r.scopes).await,
        other => Err(bad(&format!("no credential of kind `{other}`"))),
    }
}

/// How long a scoped token minted for a capability Worker lives, in seconds.
const SCOPED_TTL_SECS: u64 = 900;

/// The permissions each purpose a capability Worker may ask for brings: a fixed table, so a
/// capability Worker never names a permission itself (docs/plans/workers-split.md).
pub fn purpose_permissions(purpose: &str) -> Option<&'static [&'static str]> {
    Some(match purpose {
        // Model calls, usage and logs, and the gateway's setup.
        "ai_gateway" => &[
            "AI Gateway Read",
            "AI Gateway Run",
            "AI Gateway Write",
            "Secrets Store Write",
        ],
        // Git tokens for the source repository: reading a job's config, pushing notes; and the
        // proxy Worker's `/git/`, a git token per request, revoked after.
        "artifacts" => &["Artifacts Read", "Artifacts Write"],
        // A capability's own secrets, set on the secrets page.
        "secrets_store" => &["Secrets Store Write"],
        // The MCP server portal, synced after a setup.
        "mcp_portal" => &["MCP Portals Write"],
        // The proxy Worker's `/cf/`: every call `wrangler deploy` of a platform Worker makes
        // (reading and uploading the Worker, with its Secrets Store bindings), the queues it
        // looks up and its container image and applications; the union of what the deploy
        // proxy's allowlist names (`pigeon_proxies::cf_proxy::ALLOWED`).
        "cf_deploy" => &[
            "Workers Scripts Read",
            "Workers Scripts Write",
            "Secrets Store Write",
            "Queues Read",
            "Workers Containers Read",
            "Workers Containers Write",
        ],
        _ => return None,
    })
}

/// A token minted from the parent token for `purposes`, living [`SCOPED_TTL_SECS`]: the
/// capability Worker uses it as an `Account`'s bearer, and it expires on its own.
async fn scoped(cx: &Context<'_>, purposes: &[String]) -> Result<Value> {
    if purposes.is_empty() {
        return Err(bad("a scoped credential names its purposes"));
    }
    let mut permissions: Vec<&str> = Vec::new();
    for p in purposes {
        let more =
            purpose_permissions(p).ok_or_else(|| bad(&format!("no credential purpose `{p}`")))?;
        for m in more {
            if !permissions.contains(m) {
                permissions.push(m);
            }
        }
    }
    let request = pigeon_core::tokens::MintRequest {
        purpose: format!("worker-{}", purposes.join("-")),
        policies: vec![pigeon_core::tokens::Policy::new(
            pigeon_core::tokens::Scope::Account,
            &permissions,
        )],
        ttl_secs: SCOPED_TTL_SECS,
    };
    let minted = pigeon_core::tokens::mint(&cx.account.parent, &cx.account.id, &request).await?;
    Ok(serde_json::to_value(Credential {
        token: minted.value,
        account_id: cx.account.id.clone(),
        expires_at: now_ms() + (SCOPED_TTL_SECS as i64) * 1000,
    })?)
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

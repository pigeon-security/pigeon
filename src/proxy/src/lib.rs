//! pigeon-proxy: the proxy Worker (docs/plans/workers-split.md, phase 6). It serves the git
//! proxy (`/git/`) and the Cloudflare API proxy (`/cf/`) on the proxy host, with the same code
//! core serves them with ([`pigeon_proxies`]), and `/health`; anything else, on any host, is not
//! found.
//!
//! It never holds the parent token. Everything that needs core's state or power it asks core
//! for over its `PLATFORM` binding, signing each request with `CALLER_KEY`: a scoped credential
//! per purpose (`credential`, kept until a minute before it expires), who holds a proxy
//! credential (`verify`, kept a minute, keyed by the credential's hash), a CI job token's grant
//! (`grant`) and the caller a landing's deploy job stands for (`deploy_grant`), these two asked
//! on every request. Built for `wasm32-unknown-unknown` only.

#[cfg(not(target_arch = "wasm32"))]
compile_error!("pigeon-proxy builds for wasm32 only: cargo build --target wasm32-unknown-unknown");

use std::cell::RefCell;

use pigeon_contract::{Answer, Credential, CredentialRequest, GrantRequest, VerifyRequest};
use pigeon_core::{Account, secrets_store, webauthn};
use pigeon_proxies::cache::{Credentials, Verified};
use pigeon_proxies::cf_proxy::CfProxy;
use pigeon_proxies::git_proxy::GitProxy;
use pigeon_proxies::{Grant, Host, Pending, ProxyCaller, Purpose};
use serde_json::{Value, json};
use worker::{Context, Env, Error, Method, Request, Response, Result, event};

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-proxy";
/// The Secrets Store binding of the key core signs callers with, which this Worker signs its
/// requests to core with.
const CALLER_KEY: &str = "CALLER_KEY";
/// The service binding to core.
const PLATFORM: &str = "PLATFORM";
/// Where core answers its ops over the binding.
const CORE: &str = "https://core";

/// Core's script, which the deploy proxy deploys besides the Workers in the map.
const CORE_SCRIPT: &str = "pigeon-platform";
/// The source repository, as core's settings name it.
const NAMESPACE: &str = "pigeon-security";
const REPO: &str = "pigeon";
/// The branch that moves only by a landing.
const BRANCH: &str = "main";
/// The prefix of the branches sessions push, without its slash.
const SESSION_PREFIX: &str = "claude";

std::thread_local! {
    /// The scoped credentials core minted for this isolate.
    static CREDENTIALS: RefCell<Credentials> = RefCell::new(Credentials::new());
    /// The proxy credentials core verified for this isolate.
    static VERIFIED: RefCell<Verified> = RefCell::new(Verified::new());
}

/// The time in milliseconds since the epoch.
fn now_ms() -> i64 {
    i64::try_from(worker::Date::now().as_millis()).unwrap_or(i64::MAX)
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The git proxy, as core's settings configure it.
fn git_proxy(host: &str) -> GitProxy {
    GitProxy {
        host: host.into(),
        namespace: NAMESPACE.into(),
        repo: REPO.into(),
        protected: format!("refs/heads/{BRANCH}"),
        prefix: format!("refs/heads/{SESSION_PREFIX}/"),
    }
}

/// The deploy proxy, as core's settings configure it: core's queues and container classes from
/// its wrangler.toml, every other platform Worker from the map, both built in.
fn cf_proxy(host: &str) -> CfProxy {
    CfProxy::platform(
        host,
        CORE_SCRIPT,
        include_str!("../../platform/wrangler.toml"),
        include_str!("../../../ci/workers.toml"),
        &format!("{NAMESPACE}/{REPO}"),
    )
}

/// Core, as the proxies' [`Host`], reached over the `PLATFORM` binding.
struct Core<'a> {
    env: &'a Env,
    key: Vec<u8>,
    account_id: String,
    secrets_store: String,
}

impl Core<'_> {
    /// Asks core `op` with `body`, signed for `caller` (null for none); the op's value, or why
    /// core refused.
    async fn ask(&self, op: &str, body: Value, caller: &Value) -> Result<Value> {
        let fetcher = self.env.service(PLATFORM)?;
        let nonce = webauthn::random()?;
        let answer =
            pigeon_contract::ask(&fetcher, CORE, op, &body, Some((&self.key, caller)), &nonce)
                .await?;
        match answer {
            Answer::Ok(value) => Ok(value),
            Answer::Error(f) => Err(bad(format!(
                "core refused `{op}` ({:?}): {}",
                f.kind, f.message
            ))),
        }
    }

    /// The caller this Worker signs its `credential` requests as: itself, a service, acting for
    /// no one. Core's `credential` op takes a signed caller; this names the Worker plainly
    /// rather than borrowing a person.
    fn itself() -> Value {
        json!({
            "user": { "id": format!("worker:{WORKER}"), "kind": "service", "name": WORKER, "created_at": 0 },
            "acting_for": null,
        })
    }
}

impl Host for Core<'_> {
    fn account_id(&self) -> String {
        self.account_id.clone()
    }

    fn verify<'a>(&'a self, presented: &'a str) -> Pending<'a, Option<ProxyCaller>> {
        Box::pin(async move {
            let now = now_ms();
            if let Some(kept) = VERIFIED.with(|v| v.borrow().get(presented, now)) {
                return Ok(Some(kept));
            }
            let body = serde_json::to_value(VerifyRequest {
                credential: presented.to_string(),
            })?;
            let found = self.ask("verify", body, &Value::Null).await?;
            if found.is_null() {
                return Ok(None);
            }
            let caller: ProxyCaller = serde_json::from_value(found)?;
            VERIFIED.with(|v| v.borrow_mut().put(presented, caller.clone(), now));
            Ok(Some(caller))
        })
    }

    fn job_grant<'a>(&'a self, presented: &'a str, repo: &'a str) -> Pending<'a, Option<Grant>> {
        Box::pin(async move {
            let body = serde_json::to_value(GrantRequest {
                token: presented.to_string(),
                repo: repo.to_string(),
            })?;
            Ok(serde_json::from_value(
                self.ask("grant", body, &Value::Null).await?,
            )?)
        })
    }

    fn deploy_grant<'a>(
        &'a self,
        presented: &'a str,
        repo: &'a str,
    ) -> Pending<'a, Option<ProxyCaller>> {
        Box::pin(async move {
            let body = serde_json::to_value(GrantRequest {
                token: presented.to_string(),
                repo: repo.to_string(),
            })?;
            Ok(serde_json::from_value(
                self.ask("deploy_grant", body, &Value::Null).await?,
            )?)
        })
    }

    fn account<'a>(&'a self, purpose: Purpose) -> Pending<'a, Account> {
        Box::pin(async move {
            let kept = CREDENTIALS.with(|c| c.borrow().get(purpose, now_ms()).cloned());
            let credential = match kept {
                Some(c) => c,
                None => {
                    let body = serde_json::to_value(CredentialRequest {
                        kind: "scoped".into(),
                        instance: None,
                        scopes: vec![purpose.scoped().into()],
                    })?;
                    let minted: Credential = serde_json::from_value(
                        self.ask("credential", body, &Self::itself()).await?,
                    )?;
                    CREDENTIALS.with(|c| c.borrow_mut().put(purpose, minted.clone()));
                    minted
                }
            };
            Ok(Account {
                id: self.account_id.clone(),
                parent: String::new(),
                secrets_store: self.secrets_store.clone(),
                bearer: Some(credential.token),
            })
        })
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let proxy_host = env.var("PROXY_HOST")?.to_string();
    let host = req.url()?.host_str().unwrap_or_default().to_string();
    let path = req.path();
    let git = path.starts_with("/git/");
    let cf = path.starts_with("/cf/");
    if !host.eq_ignore_ascii_case(&proxy_host) || !(git || cf || path == "/health") {
        return Response::error("not found", 404);
    }
    if path == "/health" {
        return match req.method() {
            Method::Get => Response::ok("ok"),
            _ => Response::error("not found", 404),
        };
    }
    let key = match env.secret_store(CALLER_KEY) {
        Ok(binding) => secrets_store::read(&binding).await?,
        Err(_) => None,
    };
    let Some(key) = key else {
        worker::console_error!("pigeon-proxy has no caller key: CALLER_KEY is not set");
        return Response::error("the proxy is not set up; see the proxy's logs", 503);
    };
    let core = Core {
        env: &env,
        key: key.into_bytes(),
        account_id: env.var("CF_ACCOUNT_ID")?.to_string(),
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
    };
    if git {
        git_proxy(&proxy_host).serve(&core, Some(&ctx), req).await
    } else {
        cf_proxy(&proxy_host).serve(&core, req).await
    }
}

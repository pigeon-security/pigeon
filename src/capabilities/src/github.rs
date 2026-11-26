//! The GitHub App: pigeon reads an instance's repository and hears its pushes as one App
//! (`GITHUB_APP_ID` and `GITHUB_APP_CLIENT_ID` in the Worker's vars name it; its private key and
//! webhook secret are Secrets Store secrets the person sets in the console's Access › Secrets).
//! The key never leaves core: core mints short-lived installation tokens, narrowed to one
//! approved instance's repository and read access, through [`installation_token_for`]; the sites
//! Worker gets one only through the instance-scoped `sites_scope` callback.
//! `/github/hook` checks each delivery's `X-Hub-Signature-256` before reading it, accepts pushes
//! only from the installation an instance's record names, and records the latest push per
//! instance. A push to `main` is kept as a pending deploy, bound to the approved publishing
//! record; the platform's minute cron runs it ([`run_pending`]) through the sites Worker's
//! private `site_deploy_queued`, acting for the person who linked the instance, and tries again
//! a deploy a redeploy cut off. `github_status` shows how the latest went.

use pigeon_core::github_app as app;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store;
use serde_json::{Value, json};
use worker::{Env, Error, Method, Request, Response, Result};

use crate::expiry::Expiry;
use crate::instances::{
    self,
    record::{Instance, RepoRef},
};
use crate::users::directory;
use crate::{Call, Capability, Context, Expiring, Serve, Slot};

/// The App's private key, the `.pem` GitHub generated.
pub const PRIVATE_KEY: &str = "GITHUB_APP_PRIVATE_KEY";
/// The App's webhook secret.
pub const WEBHOOK_SECRET: &str = "GITHUB_APP_WEBHOOK_SECRET";
/// How long a recorded push is kept.
pub(crate) const PUSH_KEPT_MS: i64 = 90 * 24 * 3_600_000;

/// The GitHub App capability.
pub struct Github;

/// The App's numeric id and client id, from the Worker's vars.
pub fn ids(env: &Env) -> Result<(u64, String)> {
    let id = env
        .var("GITHUB_APP_ID")?
        .to_string()
        .parse()
        .map_err(|_| bad("GITHUB_APP_ID is not a number"))?;
    Ok((id, env.var("GITHUB_APP_CLIENT_ID")?.to_string()))
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// A secret through its binding; `None` until it is set and bound.
async fn secret(env: &Env, name: &str) -> Option<String> {
    let binding = env.secret_store(name).ok()?;
    secrets_store::read(&binding).await.ok().flatten()
}

/// A JWT for the App, signed with `pem`.
async fn jwt(pem: &str, client_id: &str) -> Result<String> {
    let der = app::private_key_der(pem)
        .ok_or_else(|| bad("that is not a PEM private key (BEGIN RSA PRIVATE KEY)"))?;
    app::sign_jwt(&der, client_id, worker::Date::now().as_millis() / 1000).await
}

/// An installation token for one repository from an already selected approved record: at most
/// an hour, that repository only, with `permissions`. Answers the token and its expiry (ISO 8601).
pub(crate) async fn installation_token_for(
    env: &Env,
    full_name: &str,
    installation: u64,
    permissions: &[(&str, &str)],
) -> Result<(String, String)> {
    let (_, client_id) = ids(env)?;
    let pem = secret(env, PRIVATE_KEY)
        .await
        .ok_or_else(|| bad("the GitHub App's private key is not set up yet"))?;
    let jwt = jwt(&pem, &client_id).await?;
    let body = app::token_request(&[full_name], permissions);
    let t = app::installation_token(&jwt, installation, &body).await?;
    Ok((t.token, t.expires_at))
}

/// The ref whose pushes deploy.
pub const DEPLOY_REF: &str = "refs/heads/main";
/// The capability Worker that deploys sites, as core binds it.
pub(crate) const SITES_BINDING: crate::remote::Binding = crate::remote::Binding {
    binding: "SITES",
    worker: "pigeon-sites",
};

/// The Directory key of `instance`'s latest push-started deploy and how it went.
pub fn deploy_key(instance: &str) -> String {
    format!("github:deploy:{instance}")
}

/// How long a deploy that started may run before it counts as cut off and is tried again.
const LEASE_MS: f64 = 3.0 * 60_000.0;
/// How many times a deploy is tried before it is left failed.
const MAX_ATTEMPTS: u64 = 5;

/// Changes a queue row only if no webhook or other cron has replaced that row.
async fn replace(env: &Env, key: &str, previous: &str, value: &Value) -> Result<bool> {
    Ok(directory(
        env,
        "setting.replace",
        json!({
            "key": key, "previous": previous, "value": value.to_string(), "ttl_ms": PUSH_KEPT_MS
        }),
    )
    .await?
    .as_bool()
    .unwrap_or(false))
}

/// Whether a kept deploy should run now: pending, or started longer ago than its lease (a
/// redeploy or a limit cut it off). An expired final attempt is due for terminal cleanup. Pure.
pub fn due(kept: &Value, now_ms: f64) -> bool {
    let attempts = kept["attempts"].as_u64().unwrap_or(0);
    match kept["state"].as_str() {
        Some("pending") => attempts < MAX_ATTEMPTS,
        Some("running") => now_ms - kept["started_ms"].as_f64().unwrap_or(0.0) > LEASE_MS,
        _ => false,
    }
}

/// The approved snapshot a webhook can bind. No job is queued for a disabled publisher.
pub fn publishing_binding(instance: &Instance) -> Option<String> {
    let record = serde_json::to_value(instance).ok()?;
    pigeon_core::site_publish::site(&record).ok()?;
    record["account"]["account_id"].as_str()?;
    Some(pigeon_core::oauth::hash(&record.to_string()))
}

/// A queued push can start only while the approved publishing snapshot still matches.
pub fn allows_queued_publish(instance: &Instance, queued: &Value) -> bool {
    let revision = queued["commit"].as_str().unwrap_or_default();
    revision.len() == 40
        && revision.bytes().all(|b| b.is_ascii_hexdigit())
        && queued["binding"]
            .as_str()
            .is_some_and(|binding| publishing_binding(instance).as_deref() == Some(binding))
        && matches!(&instance.repo,
            Some(RepoRef::Github { full_name, installation: Some(installation) })
                if queued["installation"].as_u64() == Some(*installation)
                    && queued["repo"].as_str().is_some_and(|repo| repo.eq_ignore_ascii_case(full_name))
        )
}

/// A scheduled publish cannot obtain a passkey. Evaluate the same tool policy as MCP.
async fn publish_allowed(env: &Env, actor: &str) -> Result<bool> {
    use pigeon_core::policy::{Decision, Kind, Vars};
    if !crate::approvals::asked() {
        return Ok(true);
    }
    let request = crate::policy::request(actor, Kind::CallTool, "site_deploy");
    let answer = crate::policy::decide(
        &Vars {
            session_prefix: String::new(),
            main: String::new(),
        },
        &request,
    );
    if answer.decision == Decision::Allow && answer.needs.is_none() {
        Ok(true)
    } else {
        crate::policy::record(env, "github", &request, &answer, None).await?;
        Ok(false)
    }
}

async fn run_publish(
    env: &Env,
    instance: &Instance,
    commit: &str,
    binding: &Value,
) -> Result<Value> {
    let person = deployer(env, &instance.id, &instance.account).await?;
    let actor = person["user"]["name"].as_str().unwrap_or_default();
    if !publish_allowed(env, actor).await? {
        return Ok(json!({ "deployed": false, "problems": ["policy requires manual publishing"] }));
    }
    crate::remote::call_owned_instance(
        env.clone(),
        &SITES_BINDING,
        "sites",
        "site_deploy_queued",
        json!({ "instance": instance.id, "rev": commit, "publishing_binding": binding }),
        person,
        instance,
    )
    .await
}

/// Recheck the approved grant before acquiring a lease for a due publication.
pub fn prepare_publish(
    kept: &mut Value,
    current: Option<Instance>,
    now_ms: f64,
) -> Option<Instance> {
    if kept["attempts"].as_u64().unwrap_or(0) >= MAX_ATTEMPTS {
        kept["state"] = json!("failed");
        kept["error"] = json!("final publishing attempt expired before reporting completion");
        kept["finished_ms"] = json!(now_ms);
        return None;
    }
    let Some(current) = current.filter(|record| allows_queued_publish(record, kept)) else {
        kept["state"] = json!("failed");
        kept["error"] = json!(
            "publishing configuration changed or was never bound; a new GitHub push is required"
        );
        kept["finished_ms"] = json!(now_ms);
        return None;
    };
    kept["state"] = json!("running");
    kept["started_ms"] = json!(now_ms);
    kept["attempts"] = json!(kept["attempts"].as_u64().unwrap_or(0) + 1);
    Some(current)
}

/// Settle one claimed attempt. A provider error retries, while a rejected deployment is final.
pub fn finish_publish(kept: &mut Value, outcome: Result<Value>, done_ms: u64) {
    match outcome {
        Ok(result) => {
            kept["state"] = json!(if result["deployed"] == true {
                "done"
            } else {
                "failed"
            });
            kept["result"] = result;
        }
        Err(error) => {
            kept["state"] = json!(if kept["attempts"].as_u64().unwrap_or(0) >= MAX_ATTEMPTS {
                "failed"
            } else {
                "pending"
            });
            kept["error"] = json!(error.to_string());
        }
    }
    kept["finished_ms"] = json!(done_ms);
}

/// Runs every due push-started deploy, one instance at a time: from the platform's
/// minute cron. Each is marked running before it starts and done or failed after, so one cut
/// off is tried again once its lease runs out.
pub async fn run_pending(env: &Env) -> Result<Vec<Value>> {
    let mut ran = Vec::new();
    for i in instances::all(env).await? {
        let key = deploy_key(&i.id);
        let Value::String(previous) = directory(env, "setting.get", json!({ "key": key })).await?
        else {
            continue;
        };
        let mut kept = serde_json::from_str::<Value>(&previous).unwrap_or(Value::Null);
        let now = worker::Date::now().as_millis() as f64;
        if !due(&kept, now) {
            continue;
        }
        let current = instances::one(env, &i.id).await?;
        let Some(current) = prepare_publish(&mut kept, current, now) else {
            if replace(env, &key, &previous, &kept).await? {
                ran.push(json!({ "instance": i.id, "state": kept["state"] }));
            }
            continue;
        };
        let commit = kept["commit"].as_str().unwrap_or_default().to_string();
        if !replace(env, &key, &previous, &kept).await? {
            continue;
        }
        let claimed = kept.to_string();
        let outcome = run_publish(env, &current, &commit, &kept["binding"]).await;
        finish_publish(&mut kept, outcome, worker::Date::now().as_millis());
        if replace(env, &key, &claimed, &kept).await? {
            ran.push(json!({ "instance": i.id, "state": kept["state"] }));
        }
    }
    Ok(ran)
}

/// Who a push-started deploy acts for: the person who linked the instance's account. A webhook
/// has no caller of its own, and the sites Worker and core's credentials serve only a person's.
async fn deployer(
    env: &Env,
    instance: &str,
    account: &instances::record::AccountRef,
) -> Result<Value> {
    let link = crate::linked::link(env, instance)
        .await?
        .ok_or_else(|| bad(&format!("instance {instance} is not linked")))?;
    if !matches!(account, instances::record::AccountRef::Linked { account_id: Some(id) } if id == &link.account_id)
    {
        return Err(bad(
            "the linked account does not match the approved publishing account",
        ));
    }
    let listed = directory(env, "users.list", json!({})).await?;
    let mut matching = listed["users"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|user| user["kind"] == "standard" && user["name"] == link.linked_by.as_str());
    let person = matching
        .next()
        .ok_or_else(|| bad("the person who linked this account no longer exists"))?;
    if matching.next().is_some() {
        return Err(bad("the linked account owner's name is ambiguous"));
    }
    Ok(json!({ "user": person, "acting_for": null }))
}

/// The Directory key of `instance`'s latest push.
pub fn push_key(instance: &str) -> String {
    format!("github:push:{instance}")
}

/// The instance whose record names `repo` (any case) and `installation`, if one does.
pub fn instance_for<'a>(
    records: &'a [instances::record::Instance],
    repo: &str,
    installation: u64,
) -> Option<&'a instances::record::Instance> {
    let mut matches = records.iter().filter(|i| {
        matches!(&i.repo, Some(RepoRef::Github { full_name, installation: Some(n) })
            if full_name.eq_ignore_ascii_case(repo) && *n == installation)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}

fn answer(status: u16, body: Value) -> Result<Response> {
    Ok(Response::from_json(&body)?.with_status(status))
}

impl Github {
    async fn hook(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let Some(key) = secret(cx.env, WEBHOOK_SECRET).await else {
            return answer(
                503,
                json!({ "error": "the webhook secret is not set up yet" }),
            );
        };
        let signature = req.headers().get("X-Hub-Signature-256")?;
        let event = req.headers().get("X-GitHub-Event")?;
        let body = req.bytes().await?;
        if !app::verify_webhook(key.as_bytes(), &body, signature.as_deref()) {
            return answer(401, json!({ "error": "the signature does not match" }));
        }
        if event.as_deref() == Some("ping") {
            return answer(200, json!({ "ok": "pong" }));
        }
        let push = match app::read_push(event.as_deref(), &body) {
            Ok(Some(p)) => p,
            Ok(None) => return answer(202, json!({ "ignored": event })),
            Err(why) => return answer(400, json!({ "error": why })),
        };
        let records = instances::all(cx.env).await?;
        let Some(instance) = instance_for(&records, &push.repo, push.installation) else {
            return answer(
                202,
                json!({ "ignored": "no unique instance names this repository and installation" }),
            );
        };
        let at = worker::Date::now().as_millis();
        let kept = json!({
            "commit": push.after, "ref": push.git_ref, "deleted": push.deleted,
            "message": push.head_message, "at_ms": at,
        });
        let binding = (push.git_ref == DEPLOY_REF && !push.deleted)
            .then(|| publishing_binding(instance))
            .flatten();
        // The cron must still see this exact approved target when it starts the deploy.
        let pending = binding.as_ref().map(|binding| {
            json!({
                "state": "pending", "commit": push.after, "at_ms": at, "attempts": 0,
                "repo": push.repo, "installation": push.installation, "binding": binding,
            })
        });
        let recorded = directory(
            cx.env,
            "github.delivery",
            json!({
                "instance": instance.id, "fingerprint": app::delivery_fingerprint(&body),
                "push": kept.to_string(), "pending": pending.map(|value| value.to_string()),
            }),
        )
        .await?;
        match recorded.as_str() {
            Some("recorded") => {}
            Some("duplicate") => return answer(202, json!({ "duplicate": true })),
            Some("full") => {
                return answer(
                    503,
                    json!({ "error": "webhook replay cache is full; retry later" }),
                );
            }
            _ => return Err(bad("unexpected result recording the webhook delivery")),
        }
        answer(
            202,
            json!({
                "recorded": instance.id,
                "commit": push.after,
                "deploy": if binding.is_some() { "queued: runs within a minute" } else { "not publishable for this ref and profile" },
            }),
        )
    }

    /// `github_status`: the App, and each GitHub instance's installation and latest push.
    async fn status(&self, cx: &Context<'_>) -> Result<Value> {
        let mut out = Vec::new();
        for i in instances::all(cx.env).await? {
            let Some(RepoRef::Github {
                full_name,
                installation,
            }) = &i.repo
            else {
                continue;
            };
            let push =
                match directory(cx.env, "setting.get", json!({ "key": push_key(&i.id) })).await? {
                    Value::String(t) => serde_json::from_str(&t).unwrap_or(Value::Null),
                    _ => Value::Null,
                };
            let deploy = match directory(cx.env, "setting.get", json!({ "key": deploy_key(&i.id) }))
                .await?
            {
                Value::String(t) => serde_json::from_str(&t).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            out.push(json!({ "instance": i.id, "repo": full_name,
                "installation": installation, "latest_push": push, "latest_deploy": deploy }));
        }
        Ok(json!({
            "app_id": ids(cx.env)?.0,
            "key_set": secret(cx.env, PRIVATE_KEY).await.is_some(),
            "webhook_secret_set": secret(cx.env, WEBHOOK_SECRET).await.is_some(),
            "instances": out,
        }))
    }
}

impl Capability for Github {
    fn name(&self) -> &'static str {
        "github"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "github",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn footprint(&self) -> crate::Footprint {
        crate::Footprint {
            externals: vec![crate::External::new(
                "GitHub App",
                "app",
                "reads instances' repositories; webhooks to /github/hook",
            )],
            ..crate::Footprint::default()
        }
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "github_status",
            description: "The GitHub App: whether its key and webhook secret are set, and each \
                          instance's repository, installation and latest push. Never a secret.",
            input_schema: json!({ "type": "object", "properties": {} }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "github_status" => self.status(cx).await,
                other => Err(crate::unknown(other)),
            }
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/github/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(async move {
            if req.method() == Method::Post && req.path() == "/github/hook" {
                return self.hook(cx, req).await;
            }
            Response::error("not found", 404)
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        vec![
            Slot {
                name: PRIVATE_KEY.into(),
                purpose: "The GitHub App's private key (the App's settings, Private keys, \
                          Generate a private key). Paste the whole .pem file, then delete the \
                          download. pigeon checks it signs in as the App before keeping it."
                    .into(),
                scopes: &[secrets_store::WORKERS],
            },
            Slot {
                name: WEBHOOK_SECRET.into(),
                purpose: "The GitHub App's webhook secret: a long random value, entered both \
                          here and in the App's settings (Webhook secret), with the webhook URL \
                          https://platform.pigeon.dev/github/hook and the Push event."
                    .into(),
                scopes: &[secrets_store::WORKERS],
            },
        ]
    }

    fn check_secret<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            if name == WEBHOOK_SECRET {
                if value.trim().len() < 20 {
                    return Err(bad("use a random value of at least 20 characters"));
                }
                return Ok(Value::String("long enough".into()));
            }
            if name != PRIVATE_KEY {
                return Ok(Value::String("not checked".into()));
            }
            let (app_id, client_id) = ids(cx.env)?;
            let jwt = jwt(value, &client_id).await?;
            let found = app::app(&jwt).await?;
            if found["id"].as_u64() != Some(app_id) {
                return Err(bad(&format!(
                    "this key signs in as App {}, not App {app_id}",
                    found["id"]
                )));
            }
            Ok(Value::String(format!(
                "signs in as App {app_id} ({})",
                found["slug"].as_str().unwrap_or("")
            )))
        })
    }

    fn secret_expiry<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str) -> Expiring<'a> {
        Box::pin(async { Ok(Expiry::Never) })
    }
}

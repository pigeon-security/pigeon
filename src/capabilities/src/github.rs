//! The GitHub App: pigeon reads an instance's repository and hears its pushes as one App
//! (`GITHUB_APP_ID` and `GITHUB_APP_CLIENT_ID` in the Worker's vars name it; its private key and webhook secret are Secrets Store secrets the
//! person sets on the Access tab). The key never leaves core: other Workers get a short-lived
//! installation token, narrowed to one instance's repository, through [`installation_token`].
//! `/github/hook` checks each delivery's `X-Hub-Signature-256` before reading it, accepts pushes
//! only from the installation an instance's record names, and records the latest push per
//! instance. A push to `main` is kept as a pending deploy; the platform's minute cron runs it
//! ([`run_pending`]) through the sites Worker's `site_deploy`, acting for the person who linked
//! the instance, and tries again a deploy a redeploy cut off. `github_status` shows how the
//! latest went.

use pigeon_core::github_app as app;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store;
use serde_json::{Value, json};
use worker::{Env, Error, Method, Request, Response, Result};

use crate::expiry::Expiry;
use crate::instances::{self, record::RepoRef};
use crate::users::directory;
use crate::{Call, Capability, Context, Expiring, Serve, Slot};

/// The App's private key, the `.pem` GitHub generated.
pub const PRIVATE_KEY: &str = "GITHUB_APP_PRIVATE_KEY";
/// The App's webhook secret.
pub const WEBHOOK_SECRET: &str = "GITHUB_APP_WEBHOOK_SECRET";
/// What an installation token may do: read the repository, and record deployments.
pub const PERMISSIONS: [(&str, &str); 2] = [("contents", "read"), ("deployments", "write")];
/// How long a recorded push is kept.
const PUSH_KEPT_MS: i64 = 90 * 24 * 3_600_000;

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

/// An installation token for `instance`'s repository: at most an hour, that repository only,
/// with [`PERMISSIONS`]. Answers the token and its expiry (ISO 8601).
pub async fn installation_token(env: &Env, instance: &str) -> Result<(String, String)> {
    let (_, client_id) = ids(env)?;
    let record = instances::one(env, instance)
        .await?
        .ok_or_else(|| bad(&format!("there is no instance {instance}")))?;
    let Some(RepoRef::Github {
        full_name,
        installation: Some(installation),
    }) = &record.repo
    else {
        return Err(bad(&format!(
            "instance {instance} names no GitHub repository with an App installation"
        )));
    };
    let pem = secret(env, PRIVATE_KEY)
        .await
        .ok_or_else(|| bad("the GitHub App's private key is not set up yet"))?;
    let jwt = jwt(&pem, &client_id).await?;
    let body = app::token_request(&[full_name.as_str()], &PERMISSIONS);
    let t = app::installation_token(&jwt, *installation, &body).await?;
    Ok((t.token, t.expires_at))
}

/// The ref whose pushes deploy.
pub const DEPLOY_REF: &str = "refs/heads/main";
/// The capability Worker that deploys sites, as core binds it.
const SITES_BINDING: &str = "SITES";

/// The Directory key of `instance`'s latest push-started deploy and how it went.
pub fn deploy_key(instance: &str) -> String {
    format!("github:deploy:{instance}")
}

/// How long a deploy that started may run before it counts as cut off and is tried again.
const LEASE_MS: f64 = 3.0 * 60_000.0;
/// How many times a deploy is tried before it is left failed.
const MAX_ATTEMPTS: u64 = 5;

async fn keep(env: &Env, key: &str, value: &Value) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": value.to_string(), "ttl_ms": PUSH_KEPT_MS }),
    )
    .await?;
    Ok(())
}

/// Whether a kept deploy should run now: pending, or started longer ago than its lease (a
/// redeploy or a limit cut it off), and not tried too often. Pure.
pub fn due(kept: &Value, now_ms: f64) -> bool {
    let attempts = kept["attempts"].as_u64().unwrap_or(0);
    if attempts >= MAX_ATTEMPTS {
        return false;
    }
    match kept["state"].as_str() {
        Some("pending") => true,
        Some("running") => now_ms - kept["started_ms"].as_f64().unwrap_or(0.0) > LEASE_MS,
        _ => false,
    }
}

/// Runs every due push-started deploy, one instance at a time: from the platform's
/// minute cron. Each is marked running before it starts and done or failed after, so one cut
/// off is tried again once its lease runs out.
pub async fn run_pending(env: &Env) -> Result<Vec<Value>> {
    let mut ran = Vec::new();
    for i in instances::all(env).await? {
        let key = deploy_key(&i.id);
        let mut kept = match directory(env, "setting.get", json!({ "key": key })).await? {
            Value::String(t) => serde_json::from_str::<Value>(&t).unwrap_or(Value::Null),
            _ => continue,
        };
        let now = worker::Date::now().as_millis() as f64;
        if !due(&kept, now) {
            continue;
        }
        let commit = kept["commit"].as_str().unwrap_or_default().to_string();
        kept["state"] = json!("running");
        kept["started_ms"] = json!(now);
        kept["attempts"] = json!(kept["attempts"].as_u64().unwrap_or(0) + 1);
        keep(env, &key, &kept).await?;
        let outcome = match deployer(env, &i.id).await {
            Ok(person) => {
                crate::remote::call_owned(
                    env.clone(),
                    SITES_BINDING,
                    "sites",
                    "site_deploy",
                    json!({ "instance": i.id, "rev": commit }),
                    person,
                )
                .await
            }
            Err(e) => Err(e),
        };
        let done_ms = worker::Date::now().as_millis();
        match outcome {
            Ok(v) if v["deployed"] == true => {
                kept["state"] = json!("done");
                kept["result"] = v;
            }
            Ok(v) => {
                // The content has problems: trying again gives the same answer.
                kept["state"] = json!("failed");
                kept["result"] = v;
            }
            Err(e) => {
                kept["state"] = json!(if kept["attempts"].as_u64().unwrap_or(0) >= MAX_ATTEMPTS {
                    "failed"
                } else {
                    "pending"
                });
                kept["error"] = json!(e.to_string());
            }
        }
        kept["finished_ms"] = json!(done_ms);
        keep(env, &key, &kept).await?;
        ran.push(json!({ "instance": i.id, "state": kept["state"] }));
    }
    Ok(ran)
}

/// Who a push-started deploy acts for: the person who linked the instance's account. A webhook
/// has no caller of its own, and the sites Worker and core's credentials serve only a person's.
async fn deployer(env: &Env, instance: &str) -> Result<Value> {
    let link = crate::linked::link(env, instance)
        .await?
        .ok_or_else(|| bad(&format!("instance {instance} is not linked")))?;
    let listed = directory(env, "users.list", json!({})).await?;
    let person = listed["users"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|u| u["kind"] == "standard" && u["name"] == link.linked_by.as_str())
        .cloned()
        .ok_or_else(|| bad(&format!("no person named {} linked it", link.linked_by)))?;
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
    records.iter().find(|i| {
        matches!(&i.repo, Some(RepoRef::Github { full_name, installation: Some(n) })
            if full_name.eq_ignore_ascii_case(repo) && *n == installation)
    })
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
                json!({ "ignored": "no instance names this repository and installation" }),
            );
        };
        let at = worker::Date::now().as_millis();
        let kept = json!({
            "commit": push.after, "ref": push.git_ref, "deleted": push.deleted,
            "message": push.head_message, "at_ms": at,
        });
        directory(
            cx.env,
            "setting.put",
            json!({ "key": push_key(&instance.id), "value": kept.to_string(), "ttl_ms": PUSH_KEPT_MS }),
        )
        .await?;
        let deploying = push.git_ref == DEPLOY_REF && !push.deleted;
        if deploying {
            // Kept, not run here: the minute's cron runs it (run_pending), so a deploy survives
            // the webhook's time limit and a redeploy of this Worker.
            let pending = json!({
                "state": "pending", "commit": push.after, "at_ms": at, "attempts": 0,
            });
            keep(cx.env, &deploy_key(&instance.id), &pending).await?;
        }
        answer(
            202,
            json!({
                "recorded": instance.id,
                "commit": push.after,
                "deploy": if deploying { "queued: runs within a minute" } else { "not for this ref" },
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

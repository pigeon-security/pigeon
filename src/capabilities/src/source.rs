//! The source repository in Cloudflare Artifacts: the repository itself (imported from a git
//! remote when created), the queue its push events go to with the hosting Worker as consumer,
//! and short-lived git tokens on demand.

use pigeon_core::artifacts::{self, Access};
use pigeon_core::mcp::Tool;
use pigeon_core::queues;
use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::{Call, Capability, Context, Footprint, arg, to_value, unknown};

/// The source repository and where its push events go.
pub struct Source {
    pub namespace: String,
    pub repo: String,
    pub default_branch: String,
    pub description: String,
    /// The git remote imported when the repository is created.
    pub import_from: Option<String>,
    /// The queue its push events go to, consumed by the hosting Worker.
    pub queue: String,
}

/// How long a git token lives unless asked otherwise, and at most.
const TOKEN_TTL: u64 = 900;
const TOKEN_MAX_TTL: u64 = 86_400;

/// What `setup_source` did.
#[derive(Serialize)]
struct Setup {
    repo: artifacts::Ensured,
    events: queues::Ensured,
    subscription: queues::Subscribed,
}

/// A git token handed to the caller, which is behind Access.
#[derive(Serialize)]
struct Handed {
    id: Option<String>,
    token: String,
    access: Access,
    ttl_secs: u64,
    remote: Option<String>,
}

impl Source {
    async fn setup(&self, cx: &Context<'_>) -> Result<Setup> {
        let spec = artifacts::Spec {
            namespace: self.namespace.clone(),
            name: self.repo.clone(),
            default_branch: self.default_branch.clone(),
            description: Some(self.description.clone()),
            import_from: self.import_from.clone(),
        };
        let repo = artifacts::ensure(cx.account, &spec).await?;
        let events = queues::Spec {
            queue: self.queue.clone(),
            worker: cx.hosted.worker.to_string(),
            batch_size: 10,
            max_wait_ms: 5_000,
            max_retries: 5,
        };
        let events = queues::ensure(cx.account, &events).await?;
        let subscription = queues::Subscription {
            name: format!("{}-{}-pushed", self.namespace, self.repo),
            queue_id: events.queue_id.clone(),
            sources: artifacts::events_source(&self.namespace, &self.repo),
            events: vec!["pushed".into()],
            source_permissions: artifacts::EVENTS_PERMISSIONS.map(String::from).to_vec(),
        };
        let subscription = queues::subscribe(cx.account, &subscription).await?;
        Ok(Setup {
            repo,
            events,
            subscription,
        })
    }

    async fn token(&self, cx: &Context<'_>, args: &Value) -> Result<Handed> {
        let access = match args["access"].as_str().unwrap_or("read") {
            "read" => Access::Read,
            "write" => Access::Write,
            other => {
                return Err(Error::RustError(format!(
                    "access `{other}` is not read or write"
                )));
            }
        };
        let ttl = args["ttl"].as_u64().unwrap_or(TOKEN_TTL).min(TOKEN_MAX_TTL);
        let minted = artifacts::mint(cx.account, &self.namespace, &self.repo, access, ttl).await?;
        let remote = artifacts::repo(cx.account, &self.namespace, &self.repo)
            .await?
            .map(|r| r.remote);
        Ok(Handed {
            id: minted.id,
            token: minted.value,
            access: minted.access,
            ttl_secs: minted.ttl_secs,
            remote,
        })
    }
}

impl Capability for Source {
    fn name(&self) -> &'static str {
        "source"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "source",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "source_token",
                description: "Mint a git token for the source repository. Git sends it as \
                              `Authorization: Bearer <token>`. Answers the token, its id and the \
                              repository's remote.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "access": { "type": "string", "enum": ["read", "write"], "default": "read" },
                        "ttl": { "type": "integer", "description": "Seconds; 900 by default, 86400 at most." }
                    }
                }),
            },
            Tool::bare(
                "source_tokens",
                "List the source repository's git tokens, live and past, without their values.",
            ),
            Tool {
                name: "source_revoke",
                description: "Revoke one of the source repository's git tokens by id.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool::bare(
                "setup_source",
                "Make sure the source repository, its push-event queue and subscription exist.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "source_token" => to_value(self.token(cx, args).await?),
                "source_tokens" => {
                    to_value(artifacts::tokens(cx.account, &self.namespace, &self.repo).await?)
                }
                "source_revoke" => {
                    let id = arg(args, "id")?;
                    artifacts::revoke(cx.account, &self.namespace, &self.repo, id).await?;
                    json!({ "revoked": id })
                }
                "setup_source" => to_value(self.setup(cx).await?),
                other => return Err(unknown(other)),
            })
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            repos: vec![format!("{}/{}", self.namespace, self.repo)],
            queues: vec![self.queue.clone()],
            ..Footprint::default()
        }
    }
}

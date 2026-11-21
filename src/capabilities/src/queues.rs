//! Cloudflare Queues: has a queue exist and a Worker consume it. Both are found by name first,
//! so running it again changes nothing.

use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// A queue and the Worker that consumes it.
pub struct Spec {
    pub queue: String,
    /// The consuming Worker's name.
    pub worker: String,
    /// At most this many messages per batch.
    pub batch_size: u32,
    /// How long a batch waits to fill, in milliseconds.
    pub max_wait_ms: u32,
    /// How many times a failed batch is retried before its messages are dropped.
    pub max_retries: u32,
}

/// What [`ensure`] did.
#[derive(Serialize)]
pub struct Ensured {
    pub queue_id: String,
    pub queue_created: bool,
    pub consumer_created: bool,
}

fn policies() -> Vec<Policy> {
    vec![Policy::new(Scope::Account, &["Queues Write"])]
}

/// Has queue `spec.queue` exist, consumed by Worker `spec.worker`; idempotent. A consumer that
/// exists keeps its settings.
pub async fn ensure(account: &Account, spec: &Spec) -> Result<Ensured> {
    let token = Scoped::open(account, "queues", policies()).await?;
    let ensured = ensure_with(token.value(), &account.id, spec).await;
    token.finish(ensured).await
}

async fn ensure_with(token: &str, account: &str, spec: &Spec) -> Result<Ensured> {
    let queues = format!("/accounts/{account}/queues");
    let listed = call(token, Method::Get, &format!("{queues}?per_page=100"), None).await?;
    let existing = rows(&listed)
        .into_iter()
        .find(|q| q["queue_name"].as_str() == Some(&spec.queue))
        .and_then(|q| q["queue_id"].as_str().map(String::from));
    let (queue_id, queue_created) = match existing {
        Some(id) => (id, false),
        None => {
            let body = json!({ "queue_name": spec.queue });
            let made = call(token, Method::Post, &queues, Some(&body)).await?;
            let id = made["queue_id"].as_str().map(String::from).ok_or_else(|| {
                Error::RustError(format!("queue `{}` came back without its id", spec.queue))
            })?;
            (id, true)
        }
    };
    let consumers = format!("{queues}/{queue_id}/consumers");
    let listed = call(token, Method::Get, &consumers, None).await?;
    let consumed = rows(&listed).iter().any(|c| {
        c["script_name"].as_str() == Some(&spec.worker)
            || c["script"].as_str() == Some(&spec.worker)
            || c["service"].as_str() == Some(&spec.worker)
    });
    if !consumed {
        let body = json!({
            "type": "worker",
            "script_name": spec.worker,
            "settings": {
                "batch_size": spec.batch_size,
                "max_wait_time_ms": spec.max_wait_ms,
                "max_retries": spec.max_retries,
            },
        });
        call(token, Method::Post, &consumers, Some(&body)).await?;
    }
    Ok(Ensured {
        queue_id,
        queue_created,
        consumer_created: !consumed,
    })
}

fn rows(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// An event subscription: events from a Cloudflare service delivered to a queue.
pub struct Subscription {
    /// Its name; an existing subscription of that name is kept as it is.
    pub name: String,
    pub queue_id: String,
    /// The service and what in it, such as `{"type": "artifacts.repo", ...}`. When the service
    /// rejects the first form, the next is tried, for fields the docs name two ways.
    pub sources: Vec<Value>,
    /// Such as `["pushed"]`.
    pub events: Vec<String>,
}

/// What [`subscribe`] did.
#[derive(Serialize)]
pub struct Subscribed {
    pub id: Option<String>,
    pub created: bool,
}

/// Has `subscription` exist; idempotent by name.
pub async fn subscribe(account: &Account, subscription: &Subscription) -> Result<Subscribed> {
    let token = Scoped::open(account, "queues", policies()).await?;
    let subscribed = subscribe_with(token.value(), &account.id, subscription).await;
    token.finish(subscribed).await
}

async fn subscribe_with(token: &str, account: &str, s: &Subscription) -> Result<Subscribed> {
    let path = format!("/accounts/{account}/event_subscriptions/subscriptions");
    // Listing finds one made before; if the account cannot list, creation goes ahead.
    if let Ok(listed) = call(token, Method::Get, &format!("{path}?per_page=100"), None).await {
        if let Some(found) = rows(&listed)
            .into_iter()
            .find(|x| x["name"].as_str() == Some(&s.name))
        {
            return Ok(Subscribed {
                id: found["id"].as_str().map(String::from),
                created: false,
            });
        }
    }
    let mut refused = Vec::new();
    for source in &s.sources {
        let body = json!({
            "name": s.name,
            "enabled": true,
            "source": source,
            "destination": { "type": "queues.queue", "queue_id": s.queue_id },
            "events": s.events,
        });
        match call(token, Method::Post, &path, Some(&body)).await {
            Ok(made) => {
                return Ok(Subscribed {
                    id: made["id"].as_str().map(String::from),
                    created: true,
                });
            }
            Err(e) if e.to_string().contains(": 400:") => refused.push(e.to_string()),
            Err(e) => return Err(e),
        }
    }
    Err(Error::RustError(format!(
        "no source form was accepted: {}",
        refused.join("; ")
    )))
}

//! The Buildkite connector's setup: observe what is in Buildkite (and the Cloudflare MCP server
//! that gives sessions Buildkite's MCP), plan with `pigeon_core::buildkite_setup`, and with
//! `apply` make it match. It creates and updates only; it never deletes, and it never touches
//! a resource pigeon has not recorded as its own. See docs/plans/buildkite-mcp.md.

use pigeon_core::buildkite::Org;
use pigeon_core::buildkite_setup::{
    self as plan_mod, Declared, Kind, Owned, Seen, Step, hash, missing_scopes, plan, wants,
};
use pigeon_core::{Account, mcp_portal, webauthn};
use serde_json::{Value, json};
use worker::Result;

use super::{API_TOKEN, Buildkite, secret};
use crate::ci::scheduler::account;
use crate::ci::{Cx, Held, SetupRun};

/// What pigeon declares for Buildkite, from the connector's settings.
pub fn declared(bk: &Buildkite, host: &str) -> Declared {
    let mut keys: Vec<&str> = bk.queues.iter().map(|q| q.key).collect();
    keys.dedup();
    let mut queues: Vec<(String, String)> = Vec::new();
    for q in &bk.queues {
        if !queues.iter().any(|(k, _)| k == q.key) {
            queues.push((q.key.to_string(), q.shape.to_string()));
        }
    }
    Declared {
        org: bk.org.clone(),
        cluster: bk.cluster.clone(),
        pipeline: bk.pipeline.clone(),
        queues,
        repository: bk.repository.clone(),
        steps: super::steps_yaml(&keys, host),
        hook_url: format!("https://{host}/ci/hooks/{}", super::NAME),
        host: host.to_string(),
        portal: bk.portal.clone(),
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

/// Buildkite's id of a created or listed resource.
fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .or_else(|| v["uuid"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// What is there now, normalised for the plan; secrets replaced by their hash.
async fn observe(
    bk: &Buildkite,
    d: &Declared,
    org: &Org<'_>,
    account: Option<&Account>,
    held: &Held,
) -> Result<Vec<Seen>> {
    let mut seen = Vec::new();
    let owned_id = |k: Kind| {
        held.owned
            .iter()
            .find(|o| o.kind == k)
            .map(|o| o.id.clone())
    };

    for c in org.clusters().await? {
        seen.push(Seen {
            kind: Kind::Cluster,
            key: str_of(&c, "name"),
            id: id_of(&c),
            description: str_of(&c, "description"),
            body: c,
        });
    }
    // The queues of pigeon's cluster, or failing that of the one with its name (so a conflict
    // there is seen too).
    let cluster = owned_id(Kind::Cluster)
        .filter(|id| seen.iter().any(|s| s.kind == Kind::Cluster && &s.id == id))
        .or_else(|| {
            seen.iter()
                .find(|s| s.kind == Kind::Cluster && s.key == d.cluster)
                .map(|s| s.id.clone())
        });
    if let Some(cluster) = cluster {
        for q in org.queues(&cluster).await? {
            seen.push(Seen {
                kind: Kind::Queue,
                key: str_of(&q, "key"),
                id: id_of(&q),
                description: str_of(&q, "description"),
                body: q,
            });
        }
    }
    if let Some(p) = org.pipeline(&bk.pipeline).await? {
        seen.push(Seen {
            kind: Kind::Pipeline,
            key: bk.pipeline.clone(),
            id: id_of(&p),
            description: str_of(&p, "description"),
            body: p,
        });
    }
    let hook_owner = owned_id(Kind::Service);
    let mut hooks: Vec<Value> = org
        .services()
        .await?
        .into_iter()
        .filter(|s| s["settings"]["url"].as_str() == Some(d.hook_url.as_str()))
        .collect();
    // pigeon's own first, when there are several.
    hooks.sort_by_key(|s| Some(id_of(s)) != hook_owner);
    if let Some(mut s) = hooks.into_iter().next() {
        if let Some(t) = s["settings"]["token"].as_str() {
            s["settings"]["token"] = json!(hash(t));
        }
        seen.push(Seen {
            kind: Kind::Service,
            key: "hook".into(),
            id: id_of(&s),
            description: str_of(&s, "description"),
            body: s,
        });
    }
    if let (Some(a), Some((id, _))) = (account, &d.portal)
        && let Some(s) = mcp_portal::servers(a)
            .await?
            .into_iter()
            .find(|s| s["id"].as_str() == Some(id.as_str()))
    {
        seen.push(Seen {
            kind: Kind::PortalServer,
            key: id.clone(),
            id: id.clone(),
            description: str_of(&s, "description"),
            body: s,
        });
    }
    Ok(seen)
}

/// Runs the setup; see the module's doc.
pub async fn run(bk: &Buildkite, cx: &Cx<'_>, held: &Held, apply: bool) -> Result<SetupRun> {
    let say = |report: Value| {
        Ok(SetupRun {
            report,
            ..SetupRun::default()
        })
    };
    if bk.org.is_empty() {
        return say(json!({ "off": "no Buildkite organization is set (BUILDKITE_ORG is empty)" }));
    }
    let Some(token) = secret(cx, API_TOKEN).await else {
        return say(json!({
            "blocked": "BUILDKITE_API_TOKEN is not set: set it on the secrets page (secrets_request)",
        }));
    };
    let d = declared(bk, cx.host);
    let scopes = pigeon_core::buildkite::scopes(&token).await?;
    let missing = missing_scopes(&scopes);
    let org = Org {
        token: &token,
        org: &bk.org,
    };
    let mut d = d;
    let mut notes: Vec<String> = Vec::new();
    let acct = match &d.portal {
        Some(_) => match account(cx.env, &bk.parent).await {
            Ok(a) => Some(a),
            Err(e) => {
                notes.push(format!("the portal server is skipped: {e}"));
                d.portal = None;
                None
            }
        },
        None => None,
    };
    // Without the setup's scopes, observing would be refused: plan from nothing, which blocks
    // every step on the missing scopes.
    let seen = if missing.is_empty() {
        observe(bk, &d, &org, acct.as_ref(), held).await?
    } else {
        Vec::new()
    };
    let key = match &held.hook_key {
        Some(k) => k.clone(),
        None => webauthn::random()?,
    };
    let w = wants(&d, &hash(&key), &hash(&token));
    let steps = plan(&w, &seen, &held.owned, &missing);
    let mut report = json!({
        "organization": bk.org,
        "steps": steps,
        "missing_scopes": missing,
        "notes": notes,
    });
    if !apply {
        return say(report);
    }

    let mut run = SetupRun::default();
    let mut cluster = held
        .owned
        .iter()
        .find(|o| o.kind == Kind::Cluster)
        .map(|o| o.id.clone())
        .unwrap_or_default();
    let mut pipeline = held
        .owned
        .iter()
        .find(|o| o.kind == Kind::Pipeline)
        .map(|o| o.id.clone())
        .unwrap_or_default();
    let mut done: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    // A cluster or pipeline that fails stops the run, since what follows needs it; a queue, the
    // webhook or the portal server is a leaf, so the others are still tried (a plan that
    // refuses one hosted shape still gets the rest).
    let parent = |k: Kind| matches!(k, Kind::Cluster | Kind::Pipeline);
    for (step, want) in steps.iter().zip(&w) {
        let (create, id) = match step {
            Step::Create { .. } => (true, String::new()),
            Step::Update { id, .. } => (false, id.clone()),
            _ => continue,
        };
        let ids = [
            ("cluster", cluster.as_str()),
            ("pipeline", pipeline.as_str()),
            ("hook_key", key.as_str()),
            ("token", token.as_str()),
        ];
        let body = plan_mod::fill(if create { &want.create } else { &want.update }, &ids);
        let made: Result<Value> = match (want.kind, create) {
            (Kind::Cluster, true) => org.create_cluster(&body).await,
            (Kind::Cluster, false) => org.update_cluster(&id, &body).await,
            (Kind::Queue, true) => org.create_queue(&cluster, &body).await,
            (Kind::Queue, false) => org.update_queue(&cluster, &id, &body).await,
            (Kind::Pipeline, true) => org.create_pipeline(&body).await,
            (Kind::Pipeline, false) => org.update_pipeline(&want.key, &body).await,
            (Kind::Service, true) => org.create_service(&body).await,
            (Kind::Service, false) => org.update_service(&id, &body).await,
            (Kind::PortalServer, c) => match &acct {
                Some(a) if c => mcp_portal::create_server(a, &body).await,
                Some(a) => mcp_portal::update_server(a, &id, &body).await,
                None => Err(worker::Error::RustError("no Cloudflare account".into())),
            },
        };
        let made = match made {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("{} {}: {e}", want.kind.name(), want.key));
                if parent(want.kind) {
                    failed.push("the steps after it were not tried".into());
                    break;
                }
                continue;
            }
        };
        let id = if create {
            match want.kind {
                Kind::PortalServer => want.key.clone(),
                _ => id_of(&made),
            }
        } else {
            id
        };
        if id.is_empty() {
            failed.push(format!(
                "{} {}: the answer names no id",
                want.kind.name(),
                want.key
            ));
            if parent(want.kind) {
                failed.push("the steps after it were not tried".into());
                break;
            }
            continue;
        }
        match want.kind {
            Kind::Cluster => cluster = id.clone(),
            Kind::Pipeline => pipeline = id.clone(),
            // The service now holds `key`: keep it (new, or the one it already had).
            Kind::Service => run.hook_key = Some(key.clone()),
            _ => {}
        }
        run.record.push(Owned {
            kind: want.kind,
            key: want.key.clone(),
            id: id.clone(),
            spec_hash: want.secret.clone(),
        });
        done.push(format!(
            "{} {} {}",
            if create { "created" } else { "updated" },
            want.kind.name(),
            want.key
        ));
    }
    report["done"] = json!(done);
    if !failed.is_empty() {
        report["error"] = json!(failed.join("; "));
    }
    run.report = report;
    Ok(run)
}

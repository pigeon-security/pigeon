//! Buildkite setup, planned: what pigeon needs in Buildkite (a cluster, hosted queues, one
//! pipeline, the webhook to the CI gateway) and the Cloudflare MCP server that gives sessions
//! Buildkite's MCP, compared with what is there. Pure: the capability observes, this module
//! decides, the capability applies. See docs/plans/buildkite-mcp.md.
//!
//! The rule that keeps the setup from touching what is not its own: a resource is changed only
//! when pigeon's record holds its id **and** its description carries [`MARKER`]. Anything else
//! with the same name is a conflict, never adopted, changed or deleted. Nothing is ever deleted:
//! a recorded resource no longer declared is reported as orphaned.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What every resource pigeon makes says in its description.
pub const MARKER: &str = "managed by pigeon";

/// The API token's scopes the connector uses at runtime.
pub const RUNTIME_SCOPES: [&str; 2] = ["read_builds", "write_builds"];
/// The scopes the setup needs on top.
pub const SETUP_SCOPES: [&str; 6] = [
    "read_clusters",
    "write_clusters",
    "read_pipelines",
    "write_pipelines",
    "read_notification_services",
    "write_notification_services",
];
/// The scopes Buildkite's MCP server uses for sessions on top: reading logs, artifacts, test
/// runs, agents and who the token is.
pub const MCP_SCOPES: [&str; 6] = [
    "read_build_logs",
    "read_artifacts",
    "read_suites",
    "read_agents",
    "read_user",
    "read_organizations",
];

/// The webhook's events.
pub const HOOK_EVENTS: [&str; 3] = ["build.running", "build.finished", "ping"];

/// A kind of resource the setup manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Cluster,
    Queue,
    Pipeline,
    Service,
    PortalServer,
}

impl Kind {
    /// Its name in records, markers and steps.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Cluster => "cluster",
            Kind::Queue => "queue",
            Kind::Pipeline => "pipeline",
            Kind::Service => "service",
            Kind::PortalServer => "portal_server",
        }
    }

    /// The kind named `name`.
    pub fn parse(name: &str) -> Option<Kind> {
        [
            Kind::Cluster,
            Kind::Queue,
            Kind::Pipeline,
            Kind::Service,
            Kind::PortalServer,
        ]
        .into_iter()
        .find(|k| k.name() == name)
    }
}

/// What pigeon declares, from settings.
#[derive(Debug, Clone)]
pub struct Declared {
    /// The organization's slug; empty leaves Buildkite off.
    pub org: String,
    /// The cluster's name.
    pub cluster: String,
    /// The pipeline's slug, also its name.
    pub pipeline: String,
    /// The hosted queues: key and instance shape, such as `LINUX_AMD64_2X4`.
    pub queues: Vec<(String, String)>,
    /// The pipeline's repository, never cloned (every step skips checkout).
    pub repository: String,
    /// The pipeline's steps YAML.
    pub steps: String,
    /// The CI gateway's webhook URL.
    pub hook_url: String,
    /// The platform's host, named in markers.
    pub host: String,
    /// The Cloudflare MCP server for Buildkite's MCP: its server ID and URL. `None` makes none.
    pub portal: Option<(String, String)>,
}

/// One resource as wanted: its create and update bodies (which may hold `${cluster}` and
/// `${pipeline}`, the ids the applier fills in), what its observed form must show, at JSON
/// pointers, and the kind it belongs to.
#[derive(Debug, Clone)]
pub struct Want {
    pub kind: Kind,
    pub key: String,
    pub create: Value,
    pub update: Value,
    pub expect: Vec<(String, Value)>,
    /// A secret it carries that its observed form never shows, as a hash: compared with the
    /// hash recorded when it was last sent.
    pub secret: Option<String>,
    pub needs: Option<Kind>,
}

/// One resource as observed, normalised by the capability: `key` is its name, queue key, slug,
/// `hook` for the service whose URL is the webhook's, or the server ID; secrets in `body` are
/// replaced by their [`hash`].
#[derive(Debug, Clone)]
pub struct Seen {
    pub kind: Kind,
    pub key: String,
    pub id: String,
    pub description: String,
    pub body: Value,
}

/// One row of pigeon's record of what it made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub kind: Kind,
    pub key: String,
    pub id: String,
    pub spec_hash: Option<String>,
}

/// What the setup would do with one resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Create {
        kind: Kind,
        key: String,
    },
    Update {
        kind: Kind,
        key: String,
        id: String,
        fields: Vec<String>,
    },
    Ok {
        kind: Kind,
        key: String,
        id: String,
    },
    Conflict {
        kind: Kind,
        key: String,
        id: String,
        why: String,
    },
    Orphaned {
        kind: Kind,
        key: String,
        id: String,
    },
    Blocked {
        kind: Kind,
        key: String,
        why: String,
    },
}

impl Step {
    pub fn kind(&self) -> Kind {
        match self {
            Step::Create { kind, .. }
            | Step::Update { kind, .. }
            | Step::Ok { kind, .. }
            | Step::Conflict { kind, .. }
            | Step::Orphaned { kind, .. }
            | Step::Blocked { kind, .. } => *kind,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Step::Create { key, .. }
            | Step::Update { key, .. }
            | Step::Ok { key, .. }
            | Step::Conflict { key, .. }
            | Step::Orphaned { key, .. }
            | Step::Blocked { key, .. } => key,
        }
    }

    /// Whether the resource is, or will be, as wanted.
    pub fn goes_ahead(&self) -> bool {
        matches!(
            self,
            Step::Create { .. } | Step::Update { .. } | Step::Ok { .. }
        )
    }
}

/// SHA-256 of `secret`, hex: how secrets are compared and recorded.
pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The description of a resource pigeon makes.
pub fn description(kind: Kind, key: &str, host: &str) -> String {
    format!(
        "{MARKER} ({host}), pigeon:{}:{key}; changes here are overwritten",
        kind.name()
    )
}

/// The scopes the setup and the connector need that `held` lacks.
pub fn missing_scopes(held: &[String]) -> Vec<String> {
    RUNTIME_SCOPES
        .iter()
        .chain(SETUP_SCOPES.iter())
        .filter(|s| !held.iter().any(|h| h == *s))
        .map(|s| s.to_string())
        .collect()
}

/// The resources `d` wants, parents first. `hook_key` is the hash of the webhook key;
/// `token` the hash of the API token, which the portal server carries.
pub fn wants(d: &Declared, hook_key: &str, token: &str) -> Vec<Want> {
    let mut out = Vec::new();
    let desc = |kind, key: &str| description(kind, key, &d.host);

    let cluster_desc = desc(Kind::Cluster, &d.cluster);
    let cluster = json!({ "name": d.cluster, "description": cluster_desc, "emoji": ":pigeon:" });
    out.push(Want {
        kind: Kind::Cluster,
        key: d.cluster.clone(),
        create: cluster.clone(),
        update: cluster,
        expect: vec![
            ("/name".into(), json!(d.cluster)),
            ("/description".into(), json!(cluster_desc)),
        ],
        secret: None,
        needs: None,
    });

    for (key, shape) in &d.queues {
        let qd = desc(Kind::Queue, key);
        out.push(Want {
            kind: Kind::Queue,
            key: key.clone(),
            create: json!({
                "key": key, "description": qd,
                "hostedAgents": { "instanceShape": shape },
            }),
            update: json!({ "description": qd, "hostedAgents": { "instanceShape": shape } }),
            expect: vec![
                ("/key".into(), json!(key)),
                ("/description".into(), json!(qd)),
                ("/hosted_agents/instance_shape/name".into(), json!(shape)),
            ],
            secret: None,
            needs: Some(Kind::Cluster),
        });
    }

    let pd = desc(Kind::Pipeline, &d.pipeline);
    let settings = json!({
        "trigger_mode": "none",
        "build_branches": false,
        "build_pull_requests": false,
        "build_tags": false,
        "publish_commit_status": false,
    });
    let pipeline = json!({
        "name": d.pipeline,
        "description": pd,
        "cluster_id": "${cluster}",
        "repository": d.repository,
        "configuration": d.steps,
        "default_branch": "main",
        "skip_queued_branch_builds": false,
        "cancel_running_branch_builds": false,
        "provider_settings": settings,
    });
    let mut create = pipeline.clone();
    create["slug"] = json!(d.pipeline);
    out.push(Want {
        kind: Kind::Pipeline,
        key: d.pipeline.clone(),
        create,
        update: pipeline,
        expect: vec![
            ("/name".into(), json!(d.pipeline)),
            ("/description".into(), json!(pd)),
            ("/cluster_id".into(), json!("${cluster}")),
            ("/configuration".into(), json!(d.steps)),
            ("/default_branch".into(), json!("main")),
            ("/provider/settings/trigger_mode".into(), json!("none")),
        ],
        secret: None,
        needs: Some(Kind::Cluster),
    });

    let sd = desc(Kind::Service, "hook");
    let service = json!({
        "provider": "webhook",
        "description": sd,
        "scope": "some_projects",
        "scope_uuids": ["${pipeline}"],
        "settings": {
            "url": d.hook_url,
            "token": "${hook_key}",
            "token_mode": "signature",
            "events": HOOK_EVENTS,
            "tls_verify": true,
        },
    });
    out.push(Want {
        kind: Kind::Service,
        key: "hook".into(),
        create: service.clone(),
        update: service,
        expect: vec![
            ("/description".into(), json!(sd)),
            ("/settings/url".into(), json!(d.hook_url)),
            ("/settings/token".into(), json!(hook_key)),
            ("/settings/token_mode".into(), json!("signature")),
            ("/settings/events".into(), json!(HOOK_EVENTS)),
        ],
        secret: None,
        needs: Some(Kind::Pipeline),
    });

    if let Some((id, url)) = &d.portal {
        let psd = desc(Kind::PortalServer, id);
        out.push(Want {
            kind: Kind::PortalServer,
            key: id.clone(),
            create: json!({
                "id": id, "name": "Buildkite", "hostname": url, "auth_type": "bearer",
                "auth_credentials": "${token}", "description": psd,
            }),
            update: json!({
                "name": "Buildkite", "hostname": url, "auth_type": "bearer",
                "auth_credentials": "${token}", "description": psd,
            }),
            expect: vec![
                ("/hostname".into(), json!(url)),
                ("/auth_type".into(), json!("bearer")),
                ("/description".into(), json!(psd)),
            ],
            secret: Some(token.to_string()),
            needs: None,
        });
    }
    out
}

/// `v` with `${cluster}` and `${pipeline}` replaced by the ids in `ids`, wherever a string is
/// exactly one of them (at any depth).
pub fn fill(v: &Value, ids: &[(&str, &str)]) -> Value {
    match v {
        Value::String(s) => ids
            .iter()
            .find(|(name, _)| s.strip_prefix("${").and_then(|r| r.strip_suffix('}')) == Some(name))
            .map(|(_, id)| json!(id))
            .unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(|x| fill(x, ids)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, x)| (k.clone(), fill(x, ids))).collect())
        }
        other => other.clone(),
    }
}

/// What the setup would do: one step per wanted resource, in order, then one per recorded
/// resource no longer wanted. `missing` is [`missing_scopes`] of the token.
pub fn plan(wants: &[Want], seen: &[Seen], owned: &[Owned], missing: &[String]) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    for w in wants {
        let step = step(w, seen, owned, missing, &steps);
        steps.push(step);
    }
    for o in owned {
        if !wants.iter().any(|w| w.kind == o.kind && w.key == o.key) {
            steps.push(Step::Orphaned {
                kind: o.kind,
                key: o.key.clone(),
                id: o.id.clone(),
            });
        }
    }
    steps
}

fn step(w: &Want, seen: &[Seen], owned: &[Owned], missing: &[String], before: &[Step]) -> Step {
    let blocked = |why: String| Step::Blocked {
        kind: w.kind,
        key: w.key.clone(),
        why,
    };
    if !missing.is_empty() {
        return blocked(format!("the API token lacks {}", missing.join(", ")));
    }
    if let Some(parent) = w.needs {
        match before.iter().find(|s| s.kind() == parent) {
            Some(s) if s.goes_ahead() => {}
            _ => return blocked(format!("its {} is not set up", parent.name())),
        }
    }
    let Some(s) = seen.iter().find(|s| s.kind == w.kind && s.key == w.key) else {
        return Step::Create {
            kind: w.kind,
            key: w.key.clone(),
        };
    };
    let mine = owned.iter().find(|o| o.kind == w.kind && o.key == w.key);
    let conflict = |why: &str| Step::Conflict {
        kind: w.kind,
        key: w.key.clone(),
        id: s.id.clone(),
        why: format!(
            "a Buildkite {} named {} {why}: rename or delete it, then deploy again",
            w.kind.name(),
            w.key
        ),
    };
    let Some(mine) = mine else {
        return conflict("exists and is not pigeon's");
    };
    if mine.id != s.id {
        return conflict("is not the one pigeon made");
    }
    if !s.description.contains(MARKER) {
        return conflict("has lost pigeon's marker");
    }
    // The ids `${…}` stands for, from what is already set up (a parent being created means
    // this resource is created too, so it never gets here).
    let ids: Vec<(&str, &str)> = [Kind::Cluster, Kind::Pipeline]
        .into_iter()
        .filter_map(|k| {
            let o = owned
                .iter()
                .find(|o| o.kind == k && seen.iter().any(|s| s.kind == k && s.id == o.id))?;
            Some((k.name(), o.id.as_str()))
        })
        .collect();
    let mut fields: Vec<String> = w
        .expect
        .iter()
        .filter(|(path, want)| s.body.pointer(path) != Some(&fill(want, &ids)))
        .map(|(path, _)| path.trim_start_matches('/').replace('/', "."))
        .collect();
    if let Some(secret) = &w.secret
        && mine.spec_hash.as_deref() != Some(secret.as_str())
    {
        fields.push("credentials".into());
    }
    if fields.is_empty() {
        Step::Ok {
            kind: w.kind,
            key: w.key.clone(),
            id: s.id.clone(),
        }
    } else {
        Step::Update {
            kind: w.kind,
            key: w.key.clone(),
            id: s.id.clone(),
            fields,
        }
    }
}

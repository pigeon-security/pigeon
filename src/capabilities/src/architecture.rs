//! pigeon's architecture, drawn in the design system from two sources, so nothing on it is
//! written by hand and the same inputs always give the same page:
//! - the repository's code: the building blocks in `src/core/src` and the capabilities in
//!   `src/capabilities/src` (each module's `//!` summary, interface, the modules it uses and
//!   the permission names in `Policy::new(...)` and `*PERMISSIONS*` consts), and the hosting
//!   Worker's `wrangler.toml`, `(Method::X, "/path")` routes and `#[event(...)]` handlers;
//! - the capabilities the Worker hosts: their MCP tools and their footprints, the hostnames,
//!   Workers and resources each brings.

mod inventory;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use pigeon_core::design::{Page, escape};
use pigeon_core::mcp::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Call, Capability, Context, Footprint, Hosted, to_value, unknown};

/// The architecture as a capability: its model, on demand.
pub struct Architecture;

impl Capability for Architecture {
    fn name(&self) -> &'static str {
        "architecture"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        vec![Component::began("architecture", Kind::Capability)]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "architecture_describe",
                description: "The platform's architecture as data: building blocks, capabilities, \
                          Workers, hostnames and resources, read from the sources this Worker \
                          was built with. With public true, only the public view and its \
                          drawing.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "public": {
                            "type": "boolean",
                            "description": "Answer the public view: commit, digest, model, drawing."
                        },
                        "instance": {
                            "type": "string",
                            "description": "Include the tool names exposed by this approved instance's scoped MCP endpoint."
                        },
                        "svg": {
                            "type": "boolean",
                            "description": "Include the diagram as an SVG image in drawing, instead of public HTML or a model alone."
                        }
                    }
                }),
            },
            Tool::bare(
                "mcp_conformance",
                "Checks every tool this server lists against the schema rules ChatGPT and other \
             MCP clients enforce. Answers the violations as {tool, path, rule}, the counts, the \
             tool count and the size of the tools/list answer in bytes. Read only.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool == "mcp_conformance" {
                // The tools exactly as `/mcp` lists them: every hosted capability's, annotated.
                let mut tools: Vec<Tool> = cx
                    .hosted
                    .capabilities
                    .iter()
                    .flat_map(|c| c.tools())
                    .collect();
                crate::approvals::annotate(&mut tools);
                return Ok(pigeon_core::mcp::conformance(&tools));
            }
            if tool != "architecture_describe" {
                return Err(unknown(tool));
            }
            let built = built(cx.hosted);
            let svg = args["svg"].as_bool() == Some(true);
            if args.get("public").and_then(Value::as_bool) == Some(true) {
                return Ok(serde_json::json!({
                    "commit": built.public_model["commit"],
                    "digest": built.digest,
                    "model": built.public_model,
                    "drawing": if svg { &built.public_svg } else { &built.public_drawing },
                }));
            }
            let mut model = built.model.clone();
            if let Some(id) = args["instance"].as_str() {
                let record = crate::instances::one(cx.env, id)
                    .await?
                    .ok_or_else(|| worker::Error::RustError("unknown instance".into()))?;
                let scope = crate::instances::tools::Scope::new(&record)?;
                let mut tools: Vec<Tool> = cx
                    .hosted
                    .capabilities
                    .iter()
                    .flat_map(|c| c.tools())
                    .collect();
                scope.restrict_tools(&mut tools);
                model["instance_tools"] = serde_json::json!({
                    "instance": id,
                    "tools": tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>(),
                });
            }
            if svg {
                model["drawing"] = Value::String(built.drawing.clone());
            }
            Ok(model)
        })
    }
}

/// The hosting Worker's architecture in every form it is shown in, built once per isolate: its
/// sources and capabilities are fixed when it is built, so every deploy draws it anew and
/// nothing else has to.
pub struct Built {
    /// The model, as `architecture_describe` answers it and `/architecture.json` serves it.
    pub model: Value,
    /// The SVG for the signed-in console.
    drawing: String,
    /// The SVG drawn only from the public model.
    public_svg: String,
    /// The whole page, as the site serves it.
    pub page: String,
    /// The public view of the model: what the apex serves and `architecture_describe` answers
    /// to `public: true`.
    pub public_model: Value,
    /// The drawing of the public view.
    pub public_drawing: String,
    pub digest: String,
    /// The commit it was built from, cut to 12 characters; empty for a local build.
    pub commit: String,
}

/// The architecture of the Worker `hosted` describes, built on first use and again whenever the
/// capabilities it hosts change (a capability Worker's manifest arrived late, or changed), so a
/// failed manifest never leaves its tools missing for the life of the isolate.
pub fn built(hosted: &Hosted) -> &'static Built {
    static BUILT: Mutex<Option<(String, &'static Built)>> = Mutex::new(None);
    let signature = signature(hosted);
    let mut kept = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((was, built)) = kept.as_ref()
        && *was == signature
    {
        return built;
    }
    let sources: Vec<Source> = hosted
        .sources
        .iter()
        .map(|(path, text)| Source { path, text })
        .collect();
    let model = describe(&sources, hosted);
    let shown = public(&model);
    let drawing = diagram(&model);
    let public_svg = diagram(&shown);
    let html = render_around(&model, &drawing);
    // Leaked: a build is kept for the rest of the isolate, and a new one is made only when the
    // capabilities change.
    let built: &'static Built = Box::leak(Box::new(Built {
        model: to_value(&model),
        page: page_around(&html),
        public_model: to_value(&shown),
        public_drawing: render_around(&shown, &public_svg),
        drawing,
        public_svg,
        digest: model.digest.clone(),
        commit: model.commit.chars().take(12).collect(),
    }));
    *kept = Some((signature, built));
    built
}

/// What the model depends on besides the sources: each capability manifest and its Worker.
fn signature(hosted: &Hosted) -> String {
    let mut out = hosted.worker.to_string();
    for c in hosted.capabilities {
        out.push('|');
        out.push_str(c.name());
        out.push('@');
        out.push_str(c.worker().unwrap_or_default());
        for t in c.tools() {
            out.push(',');
            out.push_str(t.name);
        }
        for route in c.routes() {
            out.push_str(";route=");
            out.push_str(route);
        }
        for version in c.versions() {
            out.push_str(";version=");
            out.push_str(&version.name);
            out.push('=');
            out.push_str(&version.version);
        }
    }
    out
}

/// One source file, by its path from the repository root.
pub struct Source<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

/// The platform, as its code and capabilities describe it.
#[derive(Serialize, Clone)]
pub struct Model {
    /// The capabilities, then the building blocks.
    pub parts: Vec<Part>,
    /// Source modules and their live hosting, for the authenticated inventory.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inventory: Vec<inventory::Module>,
    pub workers: Vec<Worker>,
    pub hostnames: Vec<Hostname>,
    /// How the Workers reach each other: service bindings, and Durable Objects another Worker
    /// hosts.
    pub links: Vec<Link>,
    /// Services outside the platform's Workers, from the capabilities' footprints.
    pub externals: Vec<Dependency>,
    pub resources: Resources,
    /// The versions the hosted capabilities declare, by name, without changelogs.
    pub versions: Vec<VersionRow>,
    /// How many files it was read from, and a digest of them.
    pub files: usize,
    pub digest: String,
    /// The commit it was built from (`PIGEON_COMMIT` at build time); empty for a local build.
    pub commit: String,
}

/// One declared component's version, as the architecture shows it.
#[derive(Serialize, Clone)]
pub struct VersionRow {
    /// The component's name, such as `instances.site_worker`.
    pub name: String,
    /// Its kind, such as `capability`.
    pub kind: String,
    /// The newest version this build serves.
    pub version: String,
    /// The Worker that serves it.
    pub worker: String,
}

/// A capability or a building block.
#[derive(Serialize, Clone)]
pub struct Part {
    pub name: String,
    /// A capability, rather than a building block.
    pub capability: bool,
    pub summary: String,
    /// A capability's MCP tools; a block's public functions and types.
    pub interface: Vec<String>,
    /// The blocks and capabilities it is built from.
    pub calls: Vec<String>,
    pub permissions: Vec<String>,
    /// The hosting Worker's bindings and queues (from its wrangler.toml) its code names.
    pub bindings: Vec<String>,
}

/// A Worker of the platform.
#[derive(Serialize, Default, Clone)]
pub struct Worker {
    /// Its script name, such as `pigeon-sites`.
    pub name: String,
    /// Its name in ci/workers.toml, such as `sites`; none for a Worker the map does not list.
    pub lane: Option<String>,
    /// The CI job that deploys it.
    pub deploy_job: Option<String>,
    /// The steps that run after it deploys.
    pub after: Vec<String>,
    /// Where its facts were read from.
    pub source: String,
    pub workers_dev: bool,
    pub routes: Vec<String>,
    pub tools: Vec<String>,
    /// `fetch`, `queue`, `scheduled`.
    pub handlers: Vec<String>,
    pub secrets: Vec<String>,
    /// The Cron Triggers in its wrangler.toml.
    pub crons: Vec<String>,
    /// Its service bindings.
    pub services: Vec<Service>,
    /// Its Durable Object bindings, with the Worker that hosts each class.
    pub objects: Vec<WorkerObject>,
    /// Its container classes.
    pub containers: Vec<String>,
    /// The queues it produces to or consumes.
    pub queues: Vec<QueueUse>,
    /// The Worker whose `/mcp` serves these tools, when it is not this one.
    pub merged_into: Option<String>,
    /// The capabilities it runs.
    pub uses: Vec<String>,
    /// The capability that publishes it, for a Worker with no code of its own.
    pub published_by: Option<String>,
}

/// A Durable Object binding of a Worker.
#[derive(Serialize, Clone)]
pub struct WorkerObject {
    pub binding: String,
    pub class: String,
    /// The script of the Worker that hosts the class.
    pub owner: String,
}

/// A queue a Worker produces to or consumes.
#[derive(Serialize, Clone)]
pub struct QueueUse {
    pub queue: String,
    /// `produces` or `consumes`.
    pub role: String,
}

/// One Worker reaching another.
#[derive(Serialize, Clone)]
pub struct Link {
    pub from: String,
    pub to: String,
    /// The binding's name, and for an object its class.
    pub via: String,
    /// `service binding` or `Durable Object`.
    pub kind: String,
}

/// A service outside the platform's Workers.
#[derive(Serialize, Clone)]
pub struct Dependency {
    pub name: String,
    pub kind: String,
    pub detail: String,
    /// The capability that depends on it.
    pub by: String,
}

#[derive(Serialize, Clone)]
pub struct Hostname {
    pub host: String,
    pub worker: String,
    /// The capability that serves it.
    pub by: String,
    /// The service tokens Access admits; empty when public.
    pub access: Vec<String>,
    /// Set only in the public view, where `access` is cleared: the host is behind Access.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub behind_access: bool,
    /// Paths on it Access lets everyone through.
    pub public_paths: Vec<String>,
}

#[derive(Serialize, Default, Clone)]
pub struct Resources {
    pub zone: Option<String>,
    pub secrets: Vec<String>,
    pub queues: Vec<String>,
    pub repos: Vec<String>,
    pub mirrors: Vec<String>,
    /// Durable Objects, as `BINDING (Class)`, from wrangler.toml.
    pub objects: Vec<String>,
    /// Containers, as `Class (image key: image)`, from wrangler.toml.
    pub containers: Vec<String>,
    /// AI gateways, by ID.
    pub gateways: Vec<String>,
    /// Every permission a minted token can hold.
    pub permissions: Vec<String>,
    /// Bindings and queues wrangler.toml declares that no code names: drift to remove.
    pub unused: Vec<String>,
}

/// The part of the model fit for the public apex. It is an allow-list: each field is copied by
/// name, so a field added to the model later stays out until it is added here. It holds no
/// Access token names, permissions, repositories, mirrors, source paths or external details.
pub fn public(m: &Model) -> Model {
    let zone = m.resources.zone.clone();
    let on_zone = |host: &str| {
        zone.as_deref().is_some_and(|z| {
            host.eq_ignore_ascii_case(z)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", z.to_ascii_lowercase()))
        })
    };
    Model {
        parts: m
            .parts
            .iter()
            .map(|p| Part {
                name: p.name.clone(),
                capability: p.capability,
                summary: p.summary.clone(),
                interface: p.interface.clone(),
                calls: p.calls.clone(),
                permissions: Vec::new(),
                bindings: Vec::new(),
            })
            .collect(),
        workers: m
            .workers
            .iter()
            .map(|w| Worker {
                name: w.name.clone(),
                lane: w.lane.clone(),
                deploy_job: w.deploy_job.clone(),
                after: w.after.clone(),
                source: String::new(),
                workers_dev: w.workers_dev,
                routes: w.routes.clone(),
                tools: w.tools.clone(),
                handlers: w.handlers.clone(),
                secrets: w.secrets.clone(),
                crons: w.crons.clone(),
                services: w.services.clone(),
                objects: w.objects.clone(),
                containers: w.containers.clone(),
                queues: w.queues.clone(),
                merged_into: w.merged_into.clone(),
                uses: w.uses.clone(),
                published_by: w.published_by.clone(),
            })
            .collect(),
        hostnames: m
            .hostnames
            .iter()
            .filter(|h| on_zone(&h.host))
            .map(|h| Hostname {
                host: h.host.clone(),
                worker: h.worker.clone(),
                by: h.by.clone(),
                access: Vec::new(),
                behind_access: h.behind_access || !h.access.is_empty(),
                public_paths: h.public_paths.clone(),
            })
            .collect(),
        links: m.links.clone(),
        externals: m
            .externals
            .iter()
            .map(|e| Dependency {
                name: e.name.clone(),
                kind: e.kind.clone(),
                detail: String::new(),
                by: e.by.clone(),
            })
            .collect(),
        resources: Resources {
            zone: m.resources.zone.clone(),
            secrets: m.resources.secrets.clone(),
            queues: m.resources.queues.clone(),
            repos: Vec::new(),
            mirrors: Vec::new(),
            objects: m.resources.objects.clone(),
            containers: m.resources.containers.clone(),
            gateways: m.resources.gateways.clone(),
            permissions: Vec::new(),
            unused: Vec::new(),
        },
        inventory: Vec::new(),
        versions: m.versions.clone(),
        files: m.files,
        digest: m.digest.clone(),
        commit: m.commit.clone(),
    }
}

/// The model of `sources` and the capabilities `hosted` hosts.
pub fn describe(sources: &[Source], hosted: &Hosted) -> Model {
    let mut sorted: Vec<&Source> = sources.iter().collect();
    sorted.sort_by_key(|s| s.path);
    let blocks = parts(&sorted, "src/core/src/", false, &[]);
    let block_names: Vec<String> = blocks.iter().map(|b| b.name.clone()).collect();
    let mut caps = parts(&sorted, "src/capabilities/src/", true, &block_names);
    for part in &mut caps {
        if let Some(c) = hosted.capabilities.iter().find(|c| c.name() == part.name) {
            part.interface = c.tools().iter().map(|t| t.name.to_string()).collect();
        }
    }

    let footprints: Vec<(String, Footprint)> = hosted
        .capabilities
        .iter()
        .map(|c| (c.name().to_string(), c.footprint()))
        .collect();
    let mut resources = Resources::default();
    let mut hostnames = Vec::new();
    let mut workers = Vec::new();

    // The Workers: one for each entry of ci/workers.toml, drawn from its own wrangler.toml and
    // the Rust beside it. Core's embedded copy is what is read, so a card says where it came from.
    let mut links: Vec<Link> = Vec::new();
    let entries = entries(&sorted, hosted);
    for entry in &entries {
        let is_core = entry.script == hosted.worker;
        let toml = sorted
            .iter()
            .find(|s| !entry.dir.is_empty() && s.path == format!("{}wrangler.toml", entry.dir));
        let config = toml.map(|t| Wrangler::read(t.text)).unwrap_or_default();
        let rust: String = sorted
            .iter()
            .filter(|s| {
                !entry.dir.is_empty()
                    && s.path.starts_with(&format!("{}src/", entry.dir))
                    && s.path.ends_with(".rs")
            })
            .map(|s| code(s.text))
            .collect::<Vec<_>>()
            .join("\n");
        // Everything the Worker is built from, to tell whether a binding is used: its own crate,
        // the crates its map entry lists (the libraries it links, such as src/ci for core) and
        // the crates every Worker links. Its routes and handlers stay its own crate's.
        let built: String = sorted
            .iter()
            .filter(|s| s.path.ends_with(".rs") && built_from(entry, s.path))
            .map(|s| code(s.text))
            .collect::<Vec<_>>()
            .join("\n");
        // The capabilities this Worker runs: its own, or a remote capability's, which names it.
        let on: Vec<&Box<dyn Capability>> = hosted
            .capabilities
            .iter()
            .filter(|c| c.worker().unwrap_or(hosted.worker) == entry.script)
            .collect();
        if is_core && toml.is_some() {
            // wrangler.toml is the authority for bindings: each is drawn once, from there, and
            // attributed to the capabilities whose code names it.
            let names: Vec<String> = config
                .objects
                .iter()
                .map(|o| o.binding.clone())
                .chain(config.secrets.iter().cloned())
                .chain(config.producers.iter().cloned())
                .chain(config.consumers.iter().cloned())
                .collect();
            for part in &mut caps {
                part.bindings = named(&sorted, "src/capabilities/src/", &part.name, &names);
            }
            let own = strings(&built);
            resources.unused = names
                .iter()
                .filter(|n| !own.contains(n) && !caps.iter().any(|p| p.bindings.contains(n)))
                .cloned()
                .collect();
        }
        resources.secrets.extend(config.secrets.iter().cloned());
        resources.queues.extend(config.producers.iter().cloned());
        resources.queues.extend(config.consumers.iter().cloned());
        // A class is drawn once, under the Worker that hosts it: a binding with `script_name`
        // only reaches it.
        resources.objects.extend(
            config
                .objects
                .iter()
                .filter(|o| o.script.is_none())
                .map(|o| format!("{} ({})", o.binding, o.class)),
        );
        resources
            .containers
            .extend(config.containers.iter().flat_map(|c| {
                c.images
                    .iter()
                    .map(move |(key, image)| format!("{} ({key}: {})", c.class, image_name(image)))
            }));
        for s in &config.services {
            links.push(Link {
                from: entry.script.clone(),
                to: s.service.clone(),
                via: s.binding.clone(),
                kind: "service binding".into(),
            });
        }
        for o in &config.objects {
            if let Some(owner) = o.script.as_ref().filter(|s| **s != entry.script) {
                links.push(Link {
                    from: entry.script.clone(),
                    to: owner.clone(),
                    via: format!("{} ({})", o.binding, o.class),
                    kind: "Durable Object".into(),
                });
            }
        }
        let tools: Vec<String> = on
            .iter()
            .flat_map(|c| c.tools())
            .map(|t| t.name.to_string())
            .collect();
        workers.push(Worker {
            name: entry.script.clone(),
            lane: entry.lane.as_ref().map(|l| l.name.clone()),
            deploy_job: entry.lane.as_ref().map(|l| l.deploy_job.clone()),
            after: entry
                .lane
                .as_ref()
                .map(|l| l.after.iter().map(|a| after_name(*a).to_string()).collect())
                .unwrap_or_default(),
            source: if toml.is_some() {
                "wrangler.toml built into core".into()
            } else {
                "no wrangler.toml found: drawn from the map only".into()
            },
            workers_dev: config.workers_dev,
            // The Worker's own routes, then each of its capabilities' path prefixes.
            routes: routes(&rust)
                .into_iter()
                .chain(on.iter().flat_map(|c| c.routes()).map(|r| format!("* {r}")))
                .collect(),
            merged_into: (!is_core && !tools.is_empty()).then(|| hosted.worker.to_string()),
            tools,
            handlers: handlers(&rust),
            crons: config.crons.clone(),
            secrets: config.secrets.clone(),
            services: config.services.clone(),
            objects: config
                .objects
                .iter()
                .map(|o| WorkerObject {
                    binding: o.binding.clone(),
                    class: o.class.clone(),
                    owner: o.script.clone().unwrap_or_else(|| entry.script.clone()),
                })
                .collect(),
            containers: config.containers.iter().map(|c| c.class.clone()).collect(),
            queues: config
                .producers
                .iter()
                .map(|q| QueueUse {
                    queue: q.clone(),
                    role: "produces".into(),
                })
                .chain(config.consumers.iter().map(|q| QueueUse {
                    queue: q.clone(),
                    role: "consumes".into(),
                }))
                .collect(),
            uses: on.iter().map(|c| c.name().to_string()).collect(),
            published_by: None,
        });
    }
    let mut externals: Vec<Dependency> = Vec::new();

    for (by, f) in &footprints {
        for h in &f.hostnames {
            // Several capabilities can serve one hostname, such as the proxies' host.
            if let Some(seen) = hostnames
                .iter_mut()
                .find(|s: &&mut Hostname| s.host == h.host)
            {
                seen.by = format!("{}, {}", seen.by, h.by);
                seen.public_paths.extend(h.public_paths.iter().cloned());
                continue;
            }
            hostnames.push(Hostname {
                host: h.host.clone(),
                worker: if h.worker.is_empty() {
                    hosted.worker.to_string()
                } else {
                    h.worker.clone()
                },
                by: h.by.clone(),
                access: h.access.clone(),
                behind_access: false,
                public_paths: h.public_paths.clone(),
            });
        }
        for w in &f.workers {
            // A Worker the map already draws is not drawn again.
            if workers.iter().any(|x: &Worker| x.name == w.name) {
                continue;
            }
            workers.push(Worker {
                name: w.name.clone(),
                routes: w.routes.clone(),
                handlers: vec!["fetch".into()],
                published_by: Some(by.clone()),
                ..Worker::default()
            });
        }
        for e in &f.externals {
            if !externals.iter().any(|x| x.name == e.name) {
                externals.push(Dependency {
                    name: e.name.clone(),
                    kind: e.kind.clone(),
                    detail: e.detail.clone(),
                    by: by.clone(),
                });
            }
        }
        if f.zone.is_some() {
            resources.zone.clone_from(&f.zone);
        }
        resources.secrets.extend(f.secrets.iter().cloned());
        resources.queues.extend(f.queues.iter().cloned());
        resources.repos.extend(f.repos.iter().cloned());
        resources.mirrors.extend(f.mirrors.iter().cloned());
        resources.gateways.extend(f.gateways.iter().cloned());
    }
    for part in blocks.iter().chain(&caps) {
        resources
            .permissions
            .extend(part.permissions.iter().cloned());
    }
    for list in [
        &mut resources.secrets,
        &mut resources.queues,
        &mut resources.repos,
        &mut resources.mirrors,
        &mut resources.objects,
        &mut resources.containers,
        &mut resources.gateways,
        &mut resources.permissions,
    ] {
        list.sort();
        list.dedup();
    }
    hostnames.sort_by(|a, b| a.host.cmp(&b.host));
    externals.sort_by(|a, b| a.name.cmp(&b.name));

    let mut version_rows: Vec<VersionRow> = Vec::new();
    for c in hosted.capabilities {
        for v in c.versions() {
            if version_rows.iter().any(|r| r.name == v.name) {
                continue;
            }
            version_rows.push(VersionRow {
                name: v.name,
                kind: to_value(v.kind).as_str().unwrap_or_default().to_string(),
                version: v.version,
                worker: c.worker().unwrap_or(hosted.worker).to_string(),
            });
        }
    }
    version_rows.sort_by(|a, b| a.name.cmp(&b.name));

    let inventory = inventory::collect(&sorted, &caps, &blocks, &entries, hosted);
    let mut parts = caps;
    parts.extend(blocks);
    Model {
        parts,
        inventory,
        versions: version_rows,
        workers,
        hostnames,
        links,
        externals,
        resources,
        files: sorted.len(),
        digest: digest(&sorted),
        commit: option_env!("PIGEON_COMMIT").unwrap_or_default().to_string(),
    }
}

/// The crates every Worker links, so every Worker is built from them.
const SHARED_CRATES: [&str; 2] = ["src/core/src/", "src/contract/src/"];

/// Whether the Rust file at `path` is one `entry`'s Worker is built from: under its own crate,
/// matched by one of its map entry's paths, or in a crate every Worker links.
fn built_from(entry: &Entry, path: &str) -> bool {
    (!entry.dir.is_empty() && path.starts_with(&format!("{}src/", entry.dir)))
        || entry.lane.as_ref().is_some_and(|lane| {
            lane.paths
                .iter()
                .any(|p| pigeon_core::lanes::matches(p, path))
        })
        || SHARED_CRATES.iter().any(|c| path.starts_with(c))
}

/// A Worker to draw: its script, its entry in ci/workers.toml and the directory of its files.
struct Entry {
    script: String,
    lane: Option<pigeon_core::lanes::Worker>,
    /// With a trailing slash; empty when the Worker's files were not found.
    dir: String,
}

/// The path of the map of Workers among the sources.
const MAP_PATH: &str = "ci/workers.toml";

/// The Workers to draw: each entry of the map, then the hosting Worker if the map lacks it (or
/// is missing or unreadable), so the page never draws nothing.
fn entries(sorted: &[&Source], hosted: &Hosted) -> Vec<Entry> {
    let mut out: Vec<Entry> = sorted
        .iter()
        .find(|s| s.path == MAP_PATH)
        .and_then(|s| pigeon_core::lanes::parse(s.text).ok())
        .map(|map| {
            map.workers
                .into_iter()
                .map(|w| Entry {
                    script: w.script.clone(),
                    dir: format!("{}/", w.dir.trim_end_matches('/')),
                    lane: Some(w),
                })
                .collect()
        })
        .unwrap_or_default();
    if !out.iter().any(|e| e.script == hosted.worker) {
        let dir = sorted
            .iter()
            .find(|s| {
                s.path.ends_with("wrangler.toml")
                    && Wrangler::read(s.text).name.as_deref() == Some(hosted.worker)
            })
            .map(|s| s.path.trim_end_matches("wrangler.toml").to_string())
            .unwrap_or_default();
        out.insert(
            0,
            Entry {
                script: hosted.worker.to_string(),
                lane: None,
                dir,
            },
        );
    }
    out
}

/// A step that runs after a deploy, as the map spells it.
fn after_name(step: pigeon_core::lanes::After) -> &'static str {
    use pigeon_core::lanes::After;
    match step {
        After::Architecture => "architecture",
        After::PortalSync => "portal_sync",
        After::ConnectorsSetup => "connectors_setup",
    }
}

/// The architecture page around a drawing already rendered, such as the public one.
pub fn page_around(drawing: &str) -> String {
    Page {
        title: "pigeon architecture",
        description: "pigeon's architecture, read from its own source.",
        body: drawing,
    }
    .render()
}

// ---- parts ----

/// The modules of the crate whose sources are in `dir`, as declared in its `lib.rs`. A
/// capability's calls are the building blocks (`pigeon_core::`) and capabilities (`crate::`)
/// it uses; a block's are the other blocks it uses.
fn parts(sorted: &[&Source], dir: &str, capability: bool, blocks: &[String]) -> Vec<Part> {
    let lib = sorted
        .iter()
        .find(|s| s.path == format!("{dir}lib.rs"))
        .map_or("", |s| s.text);
    let modules: BTreeSet<String> = lib
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("pub mod ")
                .or_else(|| l.strip_prefix("mod "))
                .and_then(|m| m.strip_suffix(';'))
                .map(String::from)
        })
        .collect();
    let blocks: BTreeSet<String> = blocks.iter().cloned().collect();
    let mut out: Vec<Part> = sorted
        .iter()
        .filter_map(|s| {
            let name = s.path.strip_prefix(dir)?.strip_suffix(".rs")?;
            if name.contains('/') || !modules.contains(name) {
                return None;
            }
            let code = code(s.text);
            let mut calls: BTreeSet<String> = refs(&code, "crate::", &modules);
            if capability {
                calls.extend(refs(&code, "pigeon_core::", &blocks));
            }
            calls.remove(name);
            Some(Part {
                name: name.to_string(),
                capability,
                summary: summary(s.text),
                interface: interface(s.text),
                calls: calls.into_iter().collect(),
                permissions: permissions(&code),
                bindings: Vec::new(),
            })
        })
        .collect();
    // Those built on most come first; then by name.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    for p in &out {
        depth_of(&p.name, &out, &mut BTreeSet::new(), &mut depth);
    }
    out.sort_by(|a, b| {
        depth[&b.name]
            .cmp(&depth[&a.name])
            .then(a.name.cmp(&b.name))
    });
    out
}

/// Which of `names` capability `name`'s code (its module and any submodules under `dir`, and
/// its own crate under `src/{name}` when it has one) names as a string literal.
fn named(sorted: &[&Source], dir: &str, name: &str, names: &[String]) -> Vec<String> {
    let file = format!("{dir}{name}.rs");
    let sub = format!("{dir}{name}/");
    // A capability whose code is its own crate, `src/{name}` (such as `pigeon-ci`).
    let krate = format!("src/{name}/src/");
    let literals: BTreeSet<String> = sorted
        .iter()
        .filter(|s| s.path == file || s.path.starts_with(&sub) || s.path.starts_with(&krate))
        .flat_map(|s| strings(&code(s.text)))
        .collect();
    names
        .iter()
        .filter(|n| literals.contains(*n))
        .cloned()
        .collect()
}

/// An image reference without its registry, and its digest cut to 12 characters:
/// `pigeon-ci-base@sha256:02b7487373a8`.
fn image_name(image: &str) -> String {
    let last = image.rsplit('/').next().unwrap_or(image);
    match last.split_once("@sha256:") {
        Some((name, digest)) => format!("{name}@sha256:{}", &digest[..digest.len().min(12)]),
        None => last.to_string(),
    }
}

/// The longest chain of calls from `name`, each part counted once: a call back into the chain
/// (a cycle) adds nothing. Memoized in `memo`, so it is linear in the calls; an unmemoized walk
/// is exponential once capabilities call each other.
fn depth_of(
    name: &str,
    all: &[Part],
    chain: &mut BTreeSet<String>,
    memo: &mut BTreeMap<String, usize>,
) -> usize {
    if let Some(&d) = memo.get(name) {
        return d;
    }
    let Some(part) = all.iter().find(|c| c.name == name) else {
        return 0;
    };
    chain.insert(name.to_string());
    let mut d = 0;
    for call in &part.calls {
        if !chain.contains(call) {
            d = d.max(1 + depth_of(call, all, chain, memo));
        }
    }
    chain.remove(name);
    memo.insert(name.to_string(), d);
    d
}

/// The module's `//!` summary: its first paragraph, as one line.
fn summary(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(doc) = line.strip_prefix("//!") else {
            break;
        };
        let doc = doc.trim();
        if doc.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        out.push(doc);
    }
    // Rustdoc links and code marks read as plain words.
    out.join(" ").replace(['`', '[', ']'], "")
}

/// Top-level `pub fn`s and `pub const`s, and `Type::method` for `pub` methods.
fn interface(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut within: Option<String> = None;
    for line in text.lines() {
        if line.starts_with("impl") {
            within = impl_type(line);
            continue;
        }
        if line == "}" {
            within = None;
            continue;
        }
        if let Some(name) = fn_name(line) {
            out.push(name);
        } else if let Some(name) = line
            .strip_prefix("pub const ")
            .and_then(|r| r.split(':').next())
        {
            out.push(name.trim().to_string());
        } else if let (Some(ty), Some(rest)) = (&within, line.strip_prefix("    "))
            && let Some(name) = fn_name(rest)
        {
            out.push(format!("{ty}::{name}"));
        }
    }
    out
}

fn fn_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub async fn ")
        .or_else(|| line.strip_prefix("pub fn "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The type an `impl` line is for: `impl<'a> Writer<'a> {` is `Writer`.
fn impl_type(line: &str) -> Option<String> {
    let mut rest = line.strip_prefix("impl")?;
    if rest.starts_with('<') {
        let end = rest.find('>')?;
        rest = &rest[end + 1..];
    }
    if rest.contains(" for ") {
        return None;
    }
    let name: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The modules `text` reaches through `prefix`, such as `crate::`.
fn refs(text: &str, prefix: &str, modules: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for at in found(text, prefix) {
        let rest = &text[at + prefix.len()..];
        if let Some(group) = rest.strip_prefix('{') {
            let group = &group[..group.find('}').unwrap_or(group.len())];
            for item in group.split(',') {
                let first = item.trim().split("::").next().unwrap_or("").trim();
                if modules.contains(first) {
                    out.insert(first.to_string());
                }
            }
        } else {
            let first = ident(rest);
            if modules.contains(&first) {
                out.insert(first);
            }
        }
    }
    out
}

/// Permission names: in `Policy::new(...)`, and in consts named `*PERMISSION*`.
fn permissions(text: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for at in found(text, "Policy::new(") {
        let span = balanced(&text[at + "Policy::new".len()..]);
        out.extend(strings(span));
    }
    for at in found(text, "const ") {
        let rest = &text[at + "const ".len()..];
        let name = ident(rest);
        if name.contains("PERMISSION") {
            // The value: a list to its `]`, or a single string to its `;`.
            let value = &rest[rest.find('=').map_or(0, |i| i + 1)..];
            let value = if value.trim_start().starts_with('[') {
                matched(value, '[', ']')
            } else {
                &value[..value.find(';').unwrap_or(value.len())]
            };
            out.extend(strings(value));
        }
    }
    out.into_iter().collect()
}

// ---- Workers ----

/// What a Worker's `wrangler.toml` declares: the authority for its bindings, which the model
/// reads rather than any capability repeating them.
#[derive(Default, Serialize, Clone, Debug)]
pub struct Wrangler {
    /// Worker name, if the file sets one.
    pub name: Option<String>,
    /// Whether workers.dev is enabled; true when the file doesn't say.
    pub workers_dev: bool,
    /// Plain string variables from the `[vars]` table.
    pub vars: BTreeMap<String, String>,
    /// Binding names of each secrets store secret, in file order.
    pub secrets: Vec<String>,
    /// Durable object bindings, in file order.
    pub objects: Vec<Object>,
    /// Container definitions, in file order.
    pub containers: Vec<Container>,
    /// Queue names used as producers, in file order.
    pub producers: Vec<String>,
    /// Queue names used as consumers, in file order.
    pub consumers: Vec<String>,
    /// Service bindings, in file order.
    pub services: Vec<Service>,
    /// Cron Triggers, in file order.
    pub crons: Vec<String>,
}

/// A service binding: the name this Worker uses and the Worker it reaches.
#[derive(Serialize, Clone, Debug)]
pub struct Service {
    pub binding: String,
    /// The bound Worker's script name.
    pub service: String,
}

/// A durable object binding and the class behind it.
#[derive(Serialize, Clone, Debug)]
pub struct Object {
    /// Binding name used by the worker.
    pub binding: String,
    /// Name of the durable object class.
    pub class: String,
    /// The script that hosts the class, when it is not this Worker (`script_name`).
    pub script: Option<String>,
}

/// A container definition with its images.
#[derive(Serialize, Clone, Debug)]
pub struct Container {
    /// Name of the container class.
    pub class: String,
    /// Pairs of image key and image reference, sorted by key.
    pub images: Vec<(String, String)>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct RawWrangler {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_true")]
    workers_dev: bool,
    #[serde(default)]
    vars: BTreeMap<String, toml::Value>,
    #[serde(default)]
    secrets_store_secrets: Vec<RawSecret>,
    #[serde(default)]
    durable_objects: RawDurableObjects,
    #[serde(default)]
    containers: Vec<RawContainer>,
    #[serde(default)]
    queues: RawQueues,
    #[serde(default)]
    services: Vec<RawService>,
    #[serde(default)]
    triggers: RawTriggers,
}

#[derive(Deserialize)]
struct RawService {
    #[serde(default)]
    binding: Option<String>,
    #[serde(default)]
    service: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawTriggers {
    #[serde(default)]
    crons: Vec<String>,
}

#[derive(Deserialize)]
struct RawSecret {
    #[serde(default)]
    binding: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawDurableObjects {
    #[serde(default)]
    bindings: Vec<RawObject>,
}

#[derive(Deserialize)]
struct RawObject {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    class_name: Option<String>,
    #[serde(default)]
    script_name: Option<String>,
}

#[derive(Deserialize)]
struct RawContainer {
    #[serde(default)]
    class_name: String,
    #[serde(default)]
    images: BTreeMap<String, RawImage>,
}

#[derive(Deserialize)]
struct RawImage {
    #[serde(default)]
    image: String,
}

#[derive(Deserialize, Default)]
struct RawQueues {
    #[serde(default)]
    producers: Vec<RawQueue>,
    #[serde(default)]
    consumers: Vec<RawQueue>,
}

#[derive(Deserialize)]
struct RawQueue {
    #[serde(default)]
    queue: Option<String>,
}

impl Wrangler {
    /// Parses wrangler TOML text, falling back to defaults if it is invalid.
    pub fn read(text: &str) -> Self {
        let Ok(raw) = toml::from_str::<RawWrangler>(text) else {
            return Wrangler {
                workers_dev: true,
                ..Default::default()
            };
        };
        Wrangler {
            name: raw.name,
            workers_dev: raw.workers_dev,
            vars: raw
                .vars
                .into_iter()
                .filter_map(|(k, v)| match v {
                    toml::Value::String(s) => Some((k, s)),
                    _ => None,
                })
                .collect(),
            secrets: raw
                .secrets_store_secrets
                .into_iter()
                .filter_map(|s| s.binding)
                .collect(),
            objects: raw
                .durable_objects
                .bindings
                .into_iter()
                .filter_map(|o| match (o.name, o.class_name) {
                    (Some(binding), Some(class)) => Some(Object {
                        binding,
                        class,
                        script: o.script_name,
                    }),
                    _ => None,
                })
                .collect(),
            containers: raw
                .containers
                .into_iter()
                .map(|c| Container {
                    class: c.class_name,
                    images: c.images.into_iter().map(|(k, i)| (k, i.image)).collect(),
                })
                .collect(),
            producers: raw
                .queues
                .producers
                .into_iter()
                .filter_map(|q| q.queue)
                .collect(),
            consumers: raw
                .queues
                .consumers
                .into_iter()
                .filter_map(|q| q.queue)
                .collect(),
            services: raw
                .services
                .into_iter()
                .filter_map(|s| match (s.binding, s.service) {
                    (Some(binding), Some(service)) => Some(Service { binding, service }),
                    _ => None,
                })
                .collect(),
            crons: raw.triggers.crons,
        }
    }
}

/// `"METHOD /path"` for each `(Method::X, "/path")` arm; a `path.starts_with("/p/")` guard
/// is `/p/:id`.
fn routes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for at in found(text, "(Method::") {
        let rest = &text[at + "(Method::".len()..];
        let method = ident(rest).to_uppercase();
        let after = rest[method.len()..].trim_start_matches([',', ' ']);
        let path = if after.starts_with('"') {
            strings(after).into_iter().next()
        } else {
            let line = &after[..after.find('\n').unwrap_or(after.len())];
            line.find("starts_with(")
                .and_then(|i| strings(&line[i..]).into_iter().next())
                .map(|p| format!("{p}:id"))
        };
        if let Some(path) = path {
            out.push(format!("{method} {path}"));
        }
    }
    out
}

/// The Worker's event handlers `text` defines: `fetch`, `queue`, `scheduled`.
fn handlers(text: &str) -> Vec<String> {
    ["fetch", "queue", "scheduled"]
        .into_iter()
        .filter(|h| text.contains(&format!("#[event({h})]")))
        .map(String::from)
        .collect()
}

// ---- text helpers ----

/// Where `pattern` occurs in code: not inside a string literal, nor inside a longer name.
fn found(text: &str, pattern: &str) -> Vec<usize> {
    text.match_indices(pattern)
        .map(|(i, _)| i)
        .filter(|&i| {
            let before = text[..i].chars().next_back();
            let in_word = pattern.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                && before.is_some_and(|c| c.is_alphanumeric() || c == '_');
            !in_word && before != Some('"') && !in_string(&text[..i])
        })
        .collect()
}

/// Whether the end of `prefix` is inside a string literal, counting unescaped quotes on its
/// last line.
fn in_string(prefix: &str) -> bool {
    let line = &prefix[prefix.rfind('\n').map_or(0, |i| i + 1)..];
    let mut inside = false;
    let mut escaped = false;
    for c in line.chars() {
        match c {
            '\\' if inside => escaped = !escaped,
            '"' if !escaped => inside = !inside,
            _ => escaped = false,
        }
    }
    inside
}

/// `text` with its comment lines blanked, so docs that name a module do not count as code.
fn code(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.trim_start().starts_with("//") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The string literals in `text`, in order.
fn strings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        s.push(n);
                    }
                }
                '"' => break,
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

fn ident(text: &str) -> String {
    text.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// `text` from its first `(` to the matching `)`.
fn balanced(text: &str) -> &str {
    matched(text, '(', ')')
}

fn matched(text: &str, open: char, close: char) -> &str {
    let Some(start) = text.find(open) else {
        return "";
    };
    let mut depth = 0;
    for (i, c) in text[start..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return &text[start..=start + i];
            }
        }
    }
    &text[start..]
}

/// FNV-1a over every path and text, as 8 hex digits.
fn digest(sorted: &[&Source]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in sorted {
        for b in s.path.bytes().chain([0]).chain(s.text.bytes()).chain([0]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{:08x}", h >> 32)
}

// ---- rendering ----
//
// One diagram, top to bottom in the order a request travels: clients, the hostnames Access
// holds or leaves public, the Workers behind them, the account resources they share, and the
// copies outside Cloudflare. Definitions of the Workers and capabilities follow it.

/// The canvas width and its outer margin; columns, gaps and rows follow from the model.
const W: f64 = 1000.0;
const M: f64 = 32.0;
const GAP: f64 = 36.0;

// The CSS travels with the SVG so both the app and the apex can use it as an image.
const DIAGRAM_STYLE: &str = r#"
.diagram { font-family: Inter, system-ui, sans-serif; }
.dg-shadow { fill: #0B0B0E; }
.dg-card { fill: #1F1F25; stroke: #3B3942; stroke-width: 1.2; }
.dg-inner, .dg-chip { fill: #2A2A31; stroke: #3B3942; }
.dg-access, .dg-store { stroke: #C9A7EE; }
.dg-worker, .dg-chip.accent { stroke: #E07FA8; }
.dg-cf { stroke: #E9B587; }
.dg-vendor { stroke: #9FC6F5; }
.dg-zone { fill: #17171C; stroke: #3B3942; stroke-width: 1.5; }
.dg-zone-name { fill: #E9B587; font: 700 12px ui-monospace, monospace; letter-spacing: .1em; }
.dg-t { fill: #ECEAE6; font-weight: 700; font-size: 14px; }
.dg-t.dg-big { font-size: 18px; }
.dg-wname { fill: #E07FA8; }
.dg-vname { fill: #9FC6F5; }
.dg-s { fill: #B9B6BF; font-size: 12px; }
.dg-m, .dg-e, .dg-lbl { fill: #B9B6BF; font: 11px ui-monospace, monospace; }
.dg-e { font-weight: 600; letter-spacing: .06em; }
.dg-link { fill: none; stroke: #A3A0A8; stroke-width: 1.8; }
.dg-link.dashed { stroke-dasharray: 5 5; }
.dg-head { fill: #A3A0A8; }
"#;

/// An SVG drawn in order.
struct Svg(Vec<String>);

impl Svg {
    fn push(&mut self, s: String) {
        self.0.push(s);
    }

    /// A card with the hard offset shadow.
    fn card(&mut self, x: f64, y: f64, w: f64, h: f64, class: &str) {
        self.push(format!(
            r#"<rect class="dg-shadow" x="{:.0}" y="{:.0}" width="{w:.0}" height="{h:.0}" rx="22"/><rect class="dg-card {class}" x="{x:.0}" y="{y:.0}" width="{w:.0}" height="{h:.0}" rx="22"/>"#,
            x,
            y + 6.0
        ));
    }

    /// A card stacked on two more, for something there are many of.
    fn stack(&mut self, x: f64, y: f64, w: f64, h: f64) {
        for d in [10.0, 5.0] {
            self.push(format!(
                r#"<rect class="dg-card dg-inner" x="{:.0}" y="{:.0}" width="{:.0}" height="{h:.0}" rx="10"/>"#,
                x + d,
                y + d,
                w - d
            ));
        }
        self.push(format!(
            r#"<rect class="dg-card dg-inner" x="{x:.0}" y="{y:.0}" width="{:.0}" height="{h:.0}" rx="10"/>"#,
            w - 10.0
        ));
    }

    fn text(&mut self, x: f64, y: f64, class: &str, s: &str, anchor: &str) {
        self.push(format!(
            r#"<text class="{class}" x="{x:.0}" y="{y:.0}" text-anchor="{anchor}">{}</text>"#,
            escape(s)
        ));
    }

    /// A pill from `x`; answers its width.
    fn chip(&mut self, x: f64, y: f64, s: &str, accent: bool) -> f64 {
        let w = chip_width(s);
        let class = if accent { "dg-chip accent" } else { "dg-chip" };
        self.push(format!(
            r#"<rect class="{class}" x="{x:.0}" y="{y:.0}" width="{w:.0}" height="24" rx="12"/><text class="dg-m" x="{:.0}" y="{:.0}" text-anchor="middle">{}</text>"#,
            x + w / 2.0,
            y + 16.0,
            escape(s)
        ));
        w
    }

    /// A link along `d`, with an arrowhead unless `bare`.
    fn link(&mut self, d: &str, dashed: bool, bare: bool) {
        let class = if dashed { "dg-link dashed" } else { "dg-link" };
        let head = if bare {
            ""
        } else {
            r#" marker-end="url(#ah)""#
        };
        self.push(format!(r#"<path class="{class}" d="{d}"{head}/>"#));
    }

    fn label(&mut self, x: f64, y: f64, s: &str) {
        self.text(x, y, "dg-lbl", s, "start");
    }
}

/// `s` cut with an ellipsis to fit `width` at about `px` per character.
fn fit(s: &str, width: f64, px: f64) -> String {
    let max = (width / px).floor() as usize;
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn chip_width(s: &str) -> f64 {
    s.chars().count() as f64 * 6.9 + 20.0
}

/// The capabilities that only shape pages, and so reach no account resource.
const PAGE_ONLY: [&str; 3] = ["design", "architecture", "mcp"];

/// Whether Access guards the hostname, by its token names or, in the public view, its flag.
fn gated(h: &Hostname) -> bool {
    h.behind_access || !h.access.is_empty()
}

/// The Worker that hosts core; it is drawn alone on the first row of the Workers layer.
const CORE: &str = "pigeon-platform";
/// How many Workers share a row under core.
const PER_ROW: usize = 3;
/// The height of a Worker card.
const CARD_H: f64 = 220.0;
/// The gap between Worker cards in a row.
const CARD_GAP: f64 = 24.0;

impl Svg {
    /// A link along `d` with an arrowhead at either end.
    fn route(&mut self, d: &str, dashed: bool, start: bool, end: bool) {
        let class = if dashed { "dg-link dashed" } else { "dg-link" };
        let start = if start {
            r#" marker-start="url(#ah)""#
        } else {
            ""
        };
        let end = if end { r#" marker-end="url(#ah)""# } else { "" };
        self.push(format!(r#"<path class="{class}" d="{d}"{start}{end}/>"#));
    }

    /// Pills in a row from `x`, as many as fit before `right`, then a "+N" pill for the rest.
    fn chips_more(&mut self, x: f64, y: f64, right: f64, items: &[String], accent: bool) {
        let mut at = x;
        for (i, item) in items.iter().enumerate() {
            let left = items.len() - i;
            let more = format!("+{left}");
            let after = if left > 1 {
                8.0 + chip_width(&more)
            } else {
                0.0
            };
            if at + chip_width(item) + after > right {
                self.chip(at, y, &more, false);
                return;
            }
            at += self.chip(at, y, item, accent) + 8.0;
        }
    }

    /// One row of a Worker card: a small label, then pills.
    fn row(&mut self, x: f64, y: f64, right: f64, label: &str, items: &[String]) {
        if items.is_empty() {
            return;
        }
        self.text(x, y + 16.0, "dg-lbl", label, "start");
        self.chips_more(x + 92.0, y, right, items, false);
    }
}

/// The binding names of the links from `from` to `to` of one kind.
fn via(m: &Model, from: &str, to: &str, kind: &str) -> Vec<String> {
    m.links
        .iter()
        .filter(|l| l.from == from && l.to == to && l.kind == kind)
        .map(|l| l.via.clone())
        .collect()
}

/// A Worker's card at (`x`, `y`), `w` wide.
fn worker_card(s: &mut Svg, wk: &Worker, x: f64, y: f64, w: f64) {
    let right = x + w - 16.0;
    s.card(x, y, w, CARD_H, "dg-worker");
    s.text(
        x + 16.0,
        y + 30.0,
        "dg-t dg-big dg-wname",
        &fit(&wk.name, w - 32.0, 11.0),
        "start",
    );
    let lane = match (&wk.lane, &wk.deploy_job) {
        (Some(l), Some(j)) => format!("{l} · {j}"),
        (Some(l), None) => l.clone(),
        (None, Some(j)) => j.clone(),
        (None, None) => "not in the map".to_string(),
    };
    s.text(
        x + 16.0,
        y + 50.0,
        "dg-s",
        &fit(&lane, w - 32.0, 6.4),
        "start",
    );
    let mut flags: Vec<String> = wk.handlers.clone();
    if let Some(by) = &wk.published_by {
        flags.push(format!("published by {by}"));
    }
    s.chips_more(x + 16.0, y + 62.0, right, &flags, false);
    let tools = if wk.tools.is_empty() {
        "no MCP tools".to_string()
    } else {
        format!("{} MCP tools", wk.tools.len())
    };
    s.text(
        x + 16.0,
        y + 112.0,
        "dg-t",
        &fit(
            &format!("{tools} · {} routes", wk.routes.len()),
            w - 32.0,
            8.2,
        ),
        "start",
    );
    // The classes it hosts: the ones it owns, not the ones it only reaches.
    let hosted: Vec<String> = wk
        .objects
        .iter()
        .filter(|o| o.owner == wk.name)
        .map(|o| o.class.clone())
        .collect();
    s.row(x + 16.0, y + 124.0, right, "OBJECTS", &hosted);
    s.row(x + 16.0, y + 154.0, right, "CONTAINERS", &wk.containers);
    if let Some(first) = wk.routes.first() {
        let extra = wk.routes.len() - 1;
        let suffix = if extra == 0 {
            String::new()
        } else {
            format!(" +{extra}")
        };
        s.text(x + 16.0, y + 200.0, "dg-lbl", "ROUTES", "start");
        s.text(
            x + 108.0,
            y + 200.0,
            "dg-m",
            &format!(
                "{}{suffix}",
                fit(first, w - 124.0 - suffix.len() as f64 * 7.0, 7.0)
            ),
            "start",
        );
    }
}

fn diagram(m: &Model) -> String {
    let hosts = &m.hostnames;
    let n = hosts.len().max(1) as f64;
    let col_w = (W - 2.0 * M - (n - 1.0) * GAP) / n;
    let col_x = |i: usize| M + i as f64 * (col_w + GAP);
    let mut s = Svg(Vec::new());

    // Layer 1: clients, above the hostname each reaches.
    let y1 = 40.0;
    s.text(M, 26.0, "dg-e", "LAYER 1 · CLIENTS", "start");
    let group_h = 190.0;
    for (i, h) in hosts.iter().enumerate() {
        let x = col_x(i);
        s.card(x, y1, col_w, group_h, "dg-group");
        let (title, inner): (&str, Vec<(String, String)>) = if !gated(h) {
            ("Visitors", vec![("Browser".into(), "anyone".into())])
        } else if h.access.is_empty() {
            (
                "Access callers",
                vec![("Access caller".into(), "service token".into())],
            )
        } else {
            (
                "Access callers",
                h.access
                    .iter()
                    .map(|t| (t.clone(), "service token".into()))
                    .collect(),
            )
        };
        s.text(x + 16.0, y1 + 32.0, "dg-t dg-big", title, "start");
        let k = inner.len().max(1) as f64;
        let iw = ((col_w - 32.0 - (k - 1.0) * 12.0) / k).min(220.0);
        for (j, (name, kind)) in inner.iter().enumerate() {
            let ix = x + 16.0 + j as f64 * (iw + 12.0);
            s.card(ix, y1 + 50.0, iw, 90.0, "");
            s.text(
                ix + 16.0,
                y1 + 78.0,
                "dg-t",
                &fit(name, iw - 24.0, 8.2),
                "start",
            );
            s.chip(ix + 16.0, y1 + 94.0, &fit(kind, iw - 44.0, 6.9), false);
        }
        let tag = if gated(h) {
            format!("{} · Access service token", h.host)
        } else {
            format!("{} · in public", h.host)
        };
        s.chip(
            x + 16.0,
            y1 + 152.0,
            &fit(&tag, col_w - 32.0 - 20.0, 6.9),
            false,
        );
    }

    // The Cloudflare region holds layers 2 to 5; its rectangle is drawn once its height is known.
    let zone_top = y1 + group_h + 48.0;
    let zone_at = s.0.len();
    s.text(
        M + 4.0,
        zone_top + 26.0,
        "dg-zone-name",
        "CLOUDFLARE",
        "start",
    );

    // Layer 2: hostnames, each a Zero Trust application or public, and the Worker serving it.
    let y2 = zone_top + 64.0;
    s.text(M + 4.0, y2 - 12.0, "dg-e", "LAYER 2 · ZERO TRUST", "start");
    let host_h = 76.0;
    let y3 = y2 + host_h + 64.0;
    for (i, h) in hosts.iter().enumerate() {
        let x = col_x(i);
        let cx = x + col_w / 2.0;
        s.link(
            &format!("M{cx:.0} {:.0}V{y2:.0}", y1 + group_h),
            false,
            false,
        );
        s.label(cx + 10.0, y1 + group_h + 26.0, "HTTPS");
        s.card(
            x,
            y2,
            col_w,
            host_h,
            if gated(h) { "dg-access" } else { "" },
        );
        s.text(
            x + 16.0,
            y2 + 32.0,
            "dg-t dg-big",
            &fit(&h.host, col_w - 32.0, 11.0),
            "start",
        );
        let sub = if !gated(h) {
            "public".to_string()
        } else if h.access.is_empty() {
            "behind Access".to_string()
        } else {
            format!("Zero Trust · admits {}", h.access.join(", "))
        };
        let sub = if h.public_paths.is_empty() {
            sub
        } else {
            format!("{sub} · public: {}", h.public_paths.join(", "))
        };
        let sub = format!("{sub} · served by {}", h.worker);
        s.text(
            x + 16.0,
            y2 + 56.0,
            "dg-s",
            &fit(&sub, col_w - 32.0, 6.4),
            "start",
        );
        if h.worker == CORE {
            s.link(
                &format!("M{cx:.0} {:.0}V{y3:.0}", y2 + host_h),
                false,
                false,
            );
            s.label(cx + 10.0, y2 + host_h + 30.0, "serves");
        }
    }

    // Layer 3: core on its own row, then every other Worker in map order, three to a row.
    s.text(M + 4.0, y3 - 12.0, "dg-e", "LAYER 3 · WORKERS", "start");
    let core_i = m
        .workers
        .iter()
        .position(|w| w.name == CORE)
        .or(if m.workers.is_empty() { None } else { Some(0) });
    let others: Vec<&Worker> = m
        .workers
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != core_i)
        .map(|(_, w)| w)
        .collect();
    let core = core_i.map(|i| &m.workers[i]);
    if let Some(c) = core {
        worker_card(&mut s, c, M, y3, W - 2.0 * M);
    }
    let core_name = core.map_or("", |c| c.name.as_str());
    let core_bottom = y3 + CARD_H;
    let cw = (W - 2.0 * M - 2.0 * CARD_GAP) / PER_ROW as f64;
    let row0 = core_bottom + 76.0;
    let row_y = |r: usize| row0 + r as f64 * (CARD_H + 56.0);
    let pos = |i: usize| {
        (
            M + (i % PER_ROW) as f64 * (cw + CARD_GAP),
            row_y(i / PER_ROW),
        )
    };
    for (i, w) in others.iter().enumerate() {
        let (x, y) = pos(i);
        worker_card(&mut s, w, x, y, cw);
    }
    let rows = others.len().div_ceil(PER_ROW);
    let workers_bottom = if rows == 0 {
        core_bottom
    } else {
        row_y(rows - 1) + CARD_H
    };

    // Layer 4: the binding arrows, labelled with the binding name. A pair of Workers shares one
    // route per kind: solid for service bindings, dashed for Durable Objects used across Workers.
    if !others.is_empty() {
        s.text(
            M + 4.0,
            core_bottom + 14.0,
            "dg-e",
            "LAYER 4 · BINDINGS",
            "start",
        );
    }
    let mut channel_lines = [0usize; PER_ROW];
    for (i, w) in others.iter().enumerate() {
        let (x, y) = pos(i);
        let (r, c) = (i / PER_ROW, i % PER_ROW);
        let kinds = [("service binding", false), ("Durable Object", true)];
        let routes: Vec<(bool, Vec<String>, Vec<String>)> = kinds
            .iter()
            .map(|(kind, dashed)| {
                (
                    *dashed,
                    via(m, core_name, &w.name, kind),
                    via(m, &w.name, core_name, kind),
                )
            })
            .filter(|(_, fwd, back)| !fwd.is_empty() || !back.is_empty())
            .collect();
        let both = routes.len() > 1;
        for (dashed, fwd, back) in routes {
            // A service route has one label, such as "SITES ⇄ PLATFORM"; an object route one
            // line for each binding.
            let lines: Vec<String> = if dashed {
                fwd.iter().chain(&back).cloned().collect()
            } else if fwd.is_empty() || back.is_empty() {
                vec![
                    fwd.iter()
                        .chain(&back)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                ]
            } else {
                vec![format!("{} ⇄ {}", fwd.join(", "), back.join(", "))]
            };
            let off = match (both, dashed) {
                (false, _) => 0.0,
                (true, false) => -6.0,
                (true, true) => 6.0,
            };
            if r == 0 {
                let px = x + cw / 2.0 + off;
                s.route(
                    &format!("M{px:.0} {core_bottom:.0}V{y:.0}"),
                    dashed,
                    !back.is_empty(),
                    !fwd.is_empty(),
                );
                for (k, line) in lines.iter().enumerate() {
                    let ly = core_bottom + 34.0 + 14.0 * k as f64;
                    let shown = fit(line, cw / 2.0 - 16.0, 6.4);
                    if dashed {
                        s.text(px + 8.0, ly, "dg-lbl", &shown, "start");
                    } else {
                        s.text(px - 8.0, ly, "dg-lbl", &shown, "end");
                    }
                }
            } else {
                // Down the gap beside the column, then into the card's side.
                let gap = c.min(PER_ROW - 2);
                let gx = M + (gap + 1) as f64 * cw + gap as f64 * CARD_GAP + CARD_GAP / 2.0;
                let px = gx + off * 0.6;
                let edge = if c <= gap { x + cw } else { x };
                let my = y + CARD_H / 2.0;
                s.route(
                    &format!("M{px:.0} {core_bottom:.0}V{my:.0}H{edge:.0}"),
                    dashed,
                    !back.is_empty(),
                    !fwd.is_empty(),
                );
                for line in &lines {
                    let ly = row_y(0) + CARD_H + 18.0 + 14.0 * channel_lines[gap] as f64;
                    channel_lines[gap] += 1;
                    s.text(
                        px + 10.0,
                        ly,
                        "dg-lbl",
                        &fit(line, W - M - px - 10.0, 6.4),
                        "start",
                    );
                }
            }
        }
    }
    // Links between two Workers that are not core: a plain line, labelled at its middle.
    for l in m
        .links
        .iter()
        .filter(|l| l.from != core_name && l.to != core_name)
    {
        let a = others.iter().position(|w| w.name == l.from);
        let b = others.iter().position(|w| w.name == l.to);
        if let (Some(a), Some(b)) = (a, b) {
            let ((ax, ay), (bx, by)) = (pos(a), pos(b));
            let (fx, fy, tx, ty) = (ax + cw / 2.0, ay + CARD_H, bx + cw / 2.0, by);
            s.route(
                &format!("M{fx:.0} {fy:.0}L{tx:.0} {ty:.0}"),
                l.kind != "service binding",
                false,
                true,
            );
            s.label(
                (fx + tx) / 2.0 + 8.0,
                (fy + ty) / 2.0,
                &fit(&l.via, 200.0, 6.4),
            );
        }
    }

    // Layer 5: the account resources, shared on one bus by the Workers that reach them.
    let r = &m.resources;
    let mut cards: Vec<(&str, String, &str)> = Vec::new();
    if !r.secrets.is_empty() {
        cards.push((
            "Secrets Store",
            format!("{} secrets", r.secrets.len()),
            "dg-store",
        ));
    }
    if !r.permissions.is_empty() {
        cards.push((
            "API tokens",
            format!("{} permissions, minted", r.permissions.len()),
            "dg-cf",
        ));
    }
    if let Some(z) = &r.zone {
        cards.push(("Zone", z.clone(), "dg-cf"));
    }
    if !r.objects.is_empty() {
        let classes: Vec<&str> = r
            .objects
            .iter()
            .map(|o| {
                o.split_once('(')
                    .map_or(o.as_str(), |(_, rest)| rest.trim_end_matches(')'))
            })
            .collect();
        cards.push((
            "Durable Objects",
            format!("{}: {}", r.objects.len(), classes.join(", ")),
            "dg-store",
        ));
    }
    if !r.containers.is_empty() {
        let classes: Vec<&str> = r
            .containers
            .iter()
            .map(|c| c.split_once(" (").map_or(c.as_str(), |(name, _)| name))
            .collect();
        cards.push(("Containers", classes.join(", "), "dg-cf"));
    }
    for repo in &r.repos {
        cards.push(("Artifacts", repo.clone(), "dg-cf"));
    }
    if !r.queues.is_empty() {
        cards.push(("Queue", r.queues.join(", "), "dg-cf"));
    }
    if !r.gateways.is_empty() {
        cards.push(("AI Gateway", r.gateways.join(", "), "dg-cf"));
    }
    let bus_y = workers_bottom + 36.0;
    let y4 = bus_y + 62.0;
    let k = cards.len().max(1) as f64;
    let rgap = 20.0;
    let rw = (W - 2.0 * M - (k - 1.0) * rgap) / k;
    let rx = |j: usize| M + j as f64 * (rw + rgap);
    let reaches = |w: &Worker| w.uses.iter().any(|u| !PAGE_ONLY.contains(&u.as_str()));
    // Where each Worker reaches the bus from: the last row's cards from below, and core, when
    // it is not on the last row, down the margin.
    let mut users: Vec<f64> = Vec::new();
    if others.is_empty() {
        if core.is_some_and(reaches) {
            users.push(W / 2.0);
        }
    } else {
        let last = rows - 1;
        for (i, w) in others.iter().enumerate() {
            if i / PER_ROW == last && reaches(w) {
                users.push(pos(i).0 + cw / 2.0);
            }
        }
        if core.is_some_and(reaches) {
            let tx = M - 10.0;
            s.link(
                &format!("M{M:.0} {:.0}H{tx:.0}V{bus_y:.0}", y3 + CARD_H / 2.0 + 30.0),
                false,
                true,
            );
            users.push(tx);
        }
    }
    if !cards.is_empty() && !users.is_empty() {
        let centers: Vec<f64> = (0..cards.len()).map(|j| rx(j) + rw / 2.0).collect();
        let left = users
            .iter()
            .chain(&centers)
            .cloned()
            .fold(f64::MAX, f64::min);
        let right = users
            .iter()
            .chain(&centers)
            .cloned()
            .fold(f64::MIN, f64::max);
        for u in users.iter().filter(|u| **u > M) {
            s.link(
                &format!("M{u:.0} {workers_bottom:.0}V{bus_y:.0}"),
                false,
                true,
            );
        }
        s.link(&format!("M{left:.0} {bus_y:.0}H{right:.0}"), false, true);
        s.text(
            right,
            bus_y - 8.0,
            "dg-lbl",
            "bindings · minted tokens",
            "end",
        );
        for c in &centers {
            s.link(&format!("M{c:.0} {bus_y:.0}V{y4:.0}"), false, false);
        }
    }
    s.text(
        M + 4.0,
        bus_y - 14.0,
        "dg-e",
        "LAYER 5 · ACCOUNT RESOURCES",
        "start",
    );
    let res_h = 64.0;
    let mut artifacts_x = None;
    for (j, (title, sub, class)) in cards.iter().enumerate() {
        let x = rx(j);
        s.card(x, y4, rw, res_h, class);
        s.text(
            x + 14.0,
            y4 + 28.0,
            "dg-t",
            &fit(title, rw - 28.0, 8.2),
            "start",
        );
        s.text(
            x + 14.0,
            y4 + 48.0,
            "dg-s",
            &fit(sub, rw - 28.0, 6.4),
            "start",
        );
        if *title == "Artifacts" {
            artifacts_x = Some(x);
        }
        if *title == "Queue" && j > 0 && cards[j - 1].0 == "Artifacts" {
            let y = y4 + res_h / 2.0;
            s.link(
                &format!("M{:.0} {y:.0}H{:.0}", rx(j - 1) + rw, x - 2.0),
                true,
                false,
            );
        }
    }
    let zone_bottom = y4 + res_h + 28.0;
    s.0.insert(
        zone_at,
        format!(
            r#"<rect class="dg-zone" x="{:.0}" y="{zone_top:.0}" width="{:.0}" height="{:.0}" rx="18"/>"#,
            M - 18.0,
            W - 2.0 * (M - 18.0),
            zone_bottom - zone_top
        ),
    );

    // Outside Cloudflare: each mirror's read-only copy, fed from Artifacts.
    let mut bottom = zone_bottom;
    if let (Some(ax), false) = (artifacts_x, r.mirrors.is_empty()) {
        let vy = zone_bottom + 56.0;
        let vw = 300.0_f64.min(W - 2.0 * M);
        for (j, url) in r.mirrors.iter().enumerate() {
            let x = (ax + j as f64 * (vw + GAP)).min(W - M - vw);
            let from = ax + rw / 2.0;
            let to = x + vw / 2.0;
            s.link(
                &format!(
                    "M{from:.0} {:.0}V{:.0}H{to:.0}V{vy:.0}",
                    y4 + res_h,
                    vy - 24.0
                ),
                false,
                false,
            );
            s.label(
                from + 10.0,
                zone_bottom + 22.0,
                "mirror · fast-forward only",
            );
            let vendor = url
                .split("//")
                .nth(1)
                .and_then(|h| h.split('/').next())
                .unwrap_or(url);
            s.card(x, vy, vw, 178.0, "dg-vendor");
            s.text(x + 16.0, vy + 28.0, "dg-t dg-vname", vendor, "start");
            s.card(x + 16.0, vy + 44.0, vw - 32.0, 46.0, "dg-inner");
            s.text(x + vw / 2.0, vy + 64.0, "dg-t", "git smart HTTP", "middle");
            s.text(
                x + vw / 2.0,
                vy + 81.0,
                "dg-m",
                "branches forward, tags added",
                "middle",
            );
            s.link(
                &format!("M{:.0} {:.0}V{:.0}", x + vw / 2.0, vy + 90.0, vy + 112.0),
                false,
                false,
            );
            s.label(x + vw / 2.0 + 8.0, vy + 105.0, "into");
            let path = url
                .split("//")
                .nth(1)
                .and_then(|h| h.split_once('/'))
                .map_or(url.as_str(), |(_, p)| p);
            s.stack(x + 16.0, vy + 114.0, vw - 32.0, 48.0);
            s.text(x + 32.0, vy + 134.0, "dg-t", "read-only copy", "start");
            s.text(x + 32.0, vy + 152.0, "dg-m", path, "start");
        }
        bottom = vy + 178.0;
    }

    // Layer 6: the external services, outside the Cloudflare region, four to a row.
    if !m.externals.is_empty() {
        let top = bottom + 40.0;
        s.text(M, top, "dg-e", "LAYER 6 · EXTERNAL SERVICES", "start");
        let per = 4usize;
        let ew = (W - 2.0 * M - (per - 1) as f64 * 20.0) / per as f64;
        let eh = 72.0;
        for (i, e) in m.externals.iter().enumerate() {
            let x = M + (i % per) as f64 * (ew + 20.0);
            let y = top + 14.0 + (i / per) as f64 * (eh + 20.0);
            s.card(x, y, ew, eh, "dg-vendor");
            s.text(
                x + 16.0,
                y + 28.0,
                "dg-t dg-vname",
                &fit(&e.name, ew - 32.0, 8.2),
                "start",
            );
            s.text(
                x + 16.0,
                y + 50.0,
                "dg-s",
                &fit(&e.kind, ew - 32.0, 6.4),
                "start",
            );
        }
        bottom = top + 14.0 + m.externals.len().div_ceil(per) as f64 * (eh + 20.0) - 20.0;
    }
    let height = bottom + 24.0;

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" class="diagram" viewBox="0 0 {W:.0} {height:.0}" role="img" aria-label="Pigeon architecture: routes, Workers, Durable Objects, containers and services"><style>{DIAGRAM_STYLE}</style><defs><marker id="ah" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse"><path class="dg-head" d="M0 0L10 5L0 10z"/></marker></defs>{}</svg>"#,
        s.0.concat()
    )
}

fn pills(items: &[String], class: &str) -> String {
    if items.is_empty() {
        return String::new();
    }
    let inner: String = items
        .iter()
        .map(|i| format!(r#"<span class="pill{class}">{}</span>"#, escape(i)))
        .collect();
    format!(r#"<div class="pills">{inner}</div>"#)
}

/// The architecture view around `drawing`, with definitions below it.
fn render_around(m: &Model, drawing: &str) -> String {
    let tool_count: usize = m.workers.iter().map(|w| w.tools.len()).sum();
    // Who uses each capability: Workers and other capabilities.
    let used_by = |name: &str| -> Vec<String> {
        m.workers
            .iter()
            .filter(|w| w.uses.iter().any(|u| u == name))
            .map(|w| w.name.clone())
            .chain(
                m.parts
                    .iter()
                    .filter(|c| c.calls.iter().any(|d| d == name))
                    .map(|c| c.name.clone()),
            )
            .collect()
    };
    let def = |c: &Part| {
        let mut body = pills(&c.interface, " accent");
        for (what, items) in [
            ("calls", c.calls.clone()),
            ("used by", used_by(&c.name)),
            ("token", c.permissions.clone()),
        ] {
            if !items.is_empty() {
                body.push_str(&format!(
                    r#"<span class="eyebrow">{what}</span>{}"#,
                    pills(&items, "")
                ));
            }
        }
        format!(
            r#"<article class="card"><div class="card-h"><h3>{}</h3></div><p>{}</p>{body}</article>"#,
            escape(&c.name),
            escape(&c.summary)
        )
    };
    let public: String = m.parts.iter().filter(|c| c.capability).map(def).collect();
    let internal: String = m.parts.iter().filter(|c| !c.capability).map(def).collect();
    let workers: String = m
        .workers
        .iter()
        .map(|w| {
            let routes: String = w
                .routes
                .iter()
                .map(|r| format!("<li><code>{}</code></li>", escape(r)))
                .collect();
            let mut body = format!(r#"<ul class="kv">{routes}</ul>"#);
            let services: Vec<String> = w
                .services
                .iter()
                .map(|s| format!("{} → {}", s.binding, s.service))
                .collect();
            let objects: Vec<String> = w
                .objects
                .iter()
                .map(|o| {
                    if o.owner == w.name {
                        format!("{} ({})", o.binding, o.class)
                    } else {
                        format!("{} ({}) on {}", o.binding, o.class, o.owner)
                    }
                })
                .collect();
            let queues: Vec<String> = w
                .queues
                .iter()
                .map(|q| format!("{} {}", q.role, q.queue))
                .collect();
            let lane: Vec<String> = w
                .lane
                .iter()
                .map(|l| format!("lane {l}"))
                .chain(w.deploy_job.iter().map(|j| format!("deploy {j}")))
                .chain(w.after.iter().map(|a| format!("after: {a}")))
                .collect();
            let merged: Vec<String> = w.merged_into.iter().cloned().collect();
            for (what, items) in [
                ("Deploy", &lane),
                ("MCP tools", &w.tools),
                ("Tools merged into", &merged),
                ("uses", &w.uses),
                ("Service bindings", &services),
                ("Durable Objects", &objects),
                ("Containers", &w.containers),
                ("Queues", &queues),
                ("Cron Triggers", &w.crons),
                ("Secrets Store bindings", &w.secrets),
                ("Read from", &(!w.source.is_empty()).then(|| w.source.clone()).into_iter().collect()),
            ] {
                if !items.is_empty() {
                    body.push_str(&format!(
                        r#"<span class="eyebrow">{what}</span>{}"#,
                        pills(items, if what == "uses" { " accent" } else { "" })
                    ));
                }
            }
            format!(
                r#"<article class="card kind-worker"><div class="card-h"><span class="dot"></span><h3>{}</h3></div>{body}</article>"#,
                escape(&w.name)
            )
        })
        .collect();
    let r = &m.resources;
    let resources: String = [
        ("Secrets Store", &r.secrets),
        ("API token permissions", &r.permissions),
        ("Queues", &r.queues),
        ("Artifacts", &r.repos),
        ("Mirrors", &r.mirrors),
        ("Durable Objects", &r.objects),
        ("Containers", &r.containers),
        ("AI gateways", &r.gateways),
        ("Declared but unused", &r.unused),
    ]
    .iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(title, items)| {
        format!(
            r#"<article class="card kind-api"><div class="card-h"><span class="dot"></span><h3>{title}</h3></div>{}</article>"#,
            pills(items, "")
        )
    })
    .collect();
    let externals: String = m
        .externals
        .iter()
        .map(|e| {
            let detail = if e.detail.is_empty() {
                String::new()
            } else {
                format!(": {}", escape(&e.detail))
            };
            format!(
                r#"<article class="card kind-api"><div class="card-h"><span class="dot"></span><h3>{}</h3></div><p>{}{detail} (by {})</p></article>"#,
                escape(&e.name),
                escape(&e.kind),
                escape(&e.by)
            )
        })
        .collect();
    let inventory: String = m.inventory.iter().map(|module| {
        let facts: String = [
            ("Hosted by", &module.hosted_by),
            ("Built into", &module.built_into),
            ("Tools", &module.tools),
            ("Routes", &module.routes),
            ("Versions", &module.versions),
            ("Code references", &module.dependencies),
        ].into_iter().filter(|(_, items)| !items.is_empty()).map(|(label, items)| {
            format!(r#"<span class="eyebrow">{label}</span>{}"#, pills(items, ""))
        }).collect();
        format!(
            r#"<article class="card"><div class="card-h"><h3>{}</h3></div><p>{} · <code>{}</code></p>{facts}</article>"#,
            escape(&module.name), module.scope.label(), escape(&module.source)
        )
    }).collect();
    let section = |eyebrow: &str, title: &str, cards: &str| {
        if cards.is_empty() {
            String::new()
        } else {
            format!(
                r#"<section class="layer" style="margin-top:28px"><span class="eyebrow">{eyebrow}</span><h2>{title}</h2><div class="cards">{cards}</div></section>"#
            )
        }
    };
    format!(
        r#"<main>
<header>
  <span class="eyebrow">pigeon · architecture</span>
  <h1>pigeon, layer by layer</h1>
  <p>Read from the platform's source, deployment configuration and registered Worker manifests. The diagram follows the order a request travels.</p>
  <div class="pills"><span class="pill">{} Workers</span><span class="pill">{} capabilities</span><span class="pill">{} core blocks</span><span class="pill">{tool_count} MCP tools</span><span class="pill">digest {}</span></div>
</header>
<section aria-label="Diagram" style="margin-top:24px"><div class="dg-canvas" tabindex="0" role="region" aria-label="Platform diagram">{drawing}</div></section>
{}
{}
{}
{}
{}
{}
</main>
<footer>Generated from {} source files{} · digest {}</footer>"#,
        m.workers.len(),
        m.parts.iter().filter(|c| c.capability).count(),
        m.parts.iter().filter(|c| !c.capability).count(),
        m.digest,
        section("Definitions", "Workers", &workers),
        section("Definitions", "Capabilities", &public),
        section("Foundation", "Core building blocks", &internal),
        section("Definitions", "Account resources", &resources),
        section("Definitions", "External services", &externals),
        section("Inventory", "Source modules and scope", &inventory),
        m.files,
        if m.commit.is_empty() {
            String::new()
        } else {
            format!(" at commit {}", escape(&m.commit[..m.commit.len().min(12)]))
        },
        m.digest,
    )
}

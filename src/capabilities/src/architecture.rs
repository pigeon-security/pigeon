//! pigeon's architecture, drawn in the design system from two sources, so nothing on it is
//! written by hand and the same inputs always give the same page:
//! - the repository's code: the building blocks in `src/core/src` and the capabilities in
//!   `src/capabilities/src` (each module's `//!` summary, interface, the modules it uses and
//!   the permission names in `Policy::new(...)` and `*PERMISSIONS*` consts), and the hosting
//!   Worker's `wrangler.toml`, `(Method::X, "/path")` routes and `#[event(...)]` handlers;
//! - the capabilities the Worker hosts: their MCP tools and their footprints, the hostnames,
//!   Workers and resources each brings.

mod check;
mod draw;
mod inventory;
mod layout;
mod scan;

pub use check::{Finding, check};
pub use draw::{Drawing, Theme, draw, render};
pub use layout::{Layout, Mode, Width, layout};
pub use scan::{ROUTE_KINDS, RouteKind, ToolCall};

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
        vec![Component::began("architecture", Kind::Capability).entry(
            "1.1.0",
            "The layered map, laid out from the model; `architecture_draw`, \
                 `architecture_layout` and `architecture_check`.",
        )]
    }

    fn sections(&self) -> Vec<crate::console::sections::Section> {
        vec![crate::console::sections::Section {
            area: "platform",
            id: "architecture",
            title: "Architecture",
            description: "Building blocks, capabilities, Workers, hostnames and resources.",
            order: 10,
        }]
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
                            "description": "Describe this approved instance: on home, the platform's architecture with the tool names its scoped MCP endpoint exposes; on any other, that instance's own (its Worker, hostnames, bucket, enabled capabilities and scoped tools)."
                        },
                        "svg": {
                            "type": "boolean",
                            "description": "Include the diagram as an SVG image in drawing, instead of public HTML or a model alone."
                        }
                    }
                }),
            },
            Tool {
                name: "architecture_draw",
                description: "Draws the architecture of the platform or one instance as an SVG \
                              laid out from its model: hostnames, the account's Workers and \
                              their bindings, the services each calls, and in full mode the MCP \
                              tool layer. Answers {svg, title, text}; text is the same drawing \
                              as plain text. Read only.",
                input_schema: draw_schema(true),
            },
            Tool {
                name: "architecture_layout",
                description: "The architecture drawing's computed layout as data, without the \
                              SVG: its rows, the cards with x, y, w and h, and the lines with \
                              their kind and label. Read only.",
                input_schema: draw_schema(false),
            },
            Tool {
                name: "architecture_check",
                description: "Checks the model the architecture drawing depends on: services \
                              without a place or a calling capability, a `through` naming no \
                              known service, Workers with no hostname and no binding, tool calls \
                              to Workers that host no such tool, and routes with no declared \
                              kind. Answers {findings, count}. Read only.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "instance": instance_property() }
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
            match tool {
                "architecture_draw" | "architecture_layout" => {
                    let (mode, width, theme) = drawing_args(args)?;
                    let (model, _) = model_for(cx, args).await?;
                    return Ok(if tool == "architecture_draw" {
                        to_value(draw(&model, mode, width, theme))
                    } else {
                        to_value(layout(&model, mode, width))
                    });
                }
                "architecture_check" => {
                    let (model, _) = model_for(cx, args).await?;
                    let findings = check(&model);
                    return Ok(serde_json::json!({
                        "count": findings.len(),
                        "findings": findings,
                    }));
                }
                "architecture_describe" => {}
                _ => return Err(unknown(tool)),
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
            if args["instance"].is_string() {
                let (own, instance_tools) = model_for(cx, args).await?;
                // An instance other than home answers its own architecture, never the
                // platform's (docs/plans/capability-scoping.md, M1). Home's is the platform's.
                if own.instance.is_some() {
                    let mut answer = to_value(&own);
                    answer["instance_tools"] = instance_tools.unwrap_or_default();
                    if svg {
                        answer["drawing"] = Value::String(diagram(&own));
                    }
                    return Ok(answer);
                }
                model["instance_tools"] = instance_tools.unwrap_or_default();
            }
            if svg {
                model["drawing"] = Value::String(built.drawing.clone());
            }
            Ok(model)
        })
    }
}

/// The `instance` argument the architecture tools share.
fn instance_property() -> Value {
    serde_json::json!({
        "type": "string",
        "description": "The approved instance to draw: home draws the platform; any other its own Worker, hostnames, bucket and capabilities. Without it, the platform."
    })
}

/// The input schema of `architecture_draw` (with `theme`) and `architecture_layout`.
fn draw_schema(theme: bool) -> Value {
    let mut properties = serde_json::json!({
        "instance": instance_property(),
        "mode": {
            "type": "string",
            "enum": ["compact", "full"],
            "description": "compact: names, hostnames, bindings and service lines only, as pigeon.dev shows it; full: counts, details, route checks and the MCP tool layer, as the console shows it."
        },
        "width": {
            "type": "string",
            "enum": ["wide", "narrow"],
            "description": "wide (the default): rows side by side; narrow: stacked for a phone."
        }
    });
    if theme {
        properties["theme"] = serde_json::json!({
            "type": "string",
            "enum": ["auto", "light", "dark"],
            "description": "auto (the default) follows the viewer's preference."
        });
    }
    serde_json::json!({ "type": "object", "properties": properties, "required": ["mode"] })
}

/// The mode, width and theme `args` ask for.
fn drawing_args(args: &Value) -> worker::Result<(Mode, Width, Theme)> {
    let bad = |what: &str| worker::Error::RustError(format!("unknown {what}"));
    let mode = match args["mode"].as_str() {
        Some("compact") => Mode::Compact,
        Some("full") => Mode::Full,
        _ => return Err(bad("mode: use compact or full")),
    };
    let width = match args["width"].as_str().unwrap_or("wide") {
        "wide" => Width::Wide,
        "narrow" => Width::Narrow,
        _ => return Err(bad("width: use wide or narrow")),
    };
    let theme = Theme::parse(args["theme"].as_str().unwrap_or("auto"))
        .ok_or_else(|| bad("theme: use auto, light or dark"))?;
    Ok((mode, width, theme))
}

/// The model `args.instance` names, and the tools its scoped endpoint exposes: home's is the
/// platform's, any other instance's its own ([`instance_model`]). Without an instance, the
/// platform's and none.
async fn model_for(cx: &Context<'_>, args: &Value) -> worker::Result<(Model, Option<Value>)> {
    let built = built(cx.hosted);
    let Some(id) = args["instance"].as_str() else {
        return Ok((built.described.clone(), None));
    };
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
    let names: Vec<String> = tools.into_iter().map(|t| t.name.to_string()).collect();
    let instance_tools = serde_json::json!({ "instance": id, "tools": names });
    // An instance other than home answers its own architecture, never the platform's
    // (docs/plans/capability-scoping.md, M1). Home's is the platform's.
    if crate::catalogue::is_home(&record) {
        return Ok((built.described.clone(), Some(instance_tools)));
    }
    let enabled = crate::catalogue::enabled(&record, &scope, cx.hosted);
    let own = instance_model(&record, &enabled, &names, &built.model);
    Ok((own, Some(instance_tools)))
}

/// The hosting Worker's architecture in every form it is shown in, built once per isolate: its
/// sources and capabilities are fixed when it is built, so every deploy draws it anew and
/// nothing else has to.
pub struct Built {
    /// The model, as `architecture_describe` answers it and `/architecture.json` serves it.
    pub model: Value,
    /// The SVG for the signed-in console.
    drawing: String,
    /// The SVG drawn only from the public model: what the apex's architecture page shows,
    /// compact and wide.
    pub public_svg: String,
    /// The same, stacked for a phone.
    pub public_svg_narrow: String,
    /// The model itself, for drawing it in other forms.
    pub described: Model,
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
    let drawable = public_drawable(&model);
    let public_svg = draw(&drawable, Mode::Compact, Width::Wide, Theme::Auto).svg;
    let public_svg_narrow = draw(&drawable, Mode::Compact, Width::Narrow, Theme::Auto).svg;
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
        public_svg_narrow,
        digest: model.digest.clone(),
        commit: model.commit.chars().take(12).collect(),
        described: model,
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
#[derive(Serialize, Clone, Default)]
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
    /// Set only on one instance's own architecture ([`instance_model`]): the instance it
    /// describes. Absent from the platform's, so `/architecture.json` is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<InstanceRef>,
    /// How each route checks its caller, as declared beside it. Not in the public view.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub route_kinds: Vec<RouteKind>,
    /// Which Worker calls which other Worker's MCP tools. Not in the public view.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

/// The instance an instance's own architecture describes.
#[derive(Serialize, Clone)]
pub struct InstanceRef {
    pub id: String,
    pub title: String,
    pub home: bool,
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
#[derive(Serialize, Clone, Default)]
pub struct Dependency {
    pub name: String,
    pub kind: String,
    pub detail: String,
    /// The capability that depends on it.
    pub by: String,
    /// Where it runs: `account`, `outside` or `container`; empty when undeclared.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub place: String,
    /// The service it is reached through, by name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub through: String,
    /// What the platform sends it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub out: String,
    /// What it sends the platform.
    #[serde(rename = "in", skip_serializing_if = "String::is_empty")]
    pub inbound: String,
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
    /// R2 buckets: set only on one instance's own architecture (its sites' bucket), so the
    /// platform's model and `/architecture.json` are unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<String>,
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
                place: String::new(),
                through: String::new(),
                out: String::new(),
                inbound: String::new(),
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
            buckets: Vec::new(),
        },
        inventory: Vec::new(),
        versions: m.versions.clone(),
        files: m.files,
        digest: m.digest.clone(),
        commit: m.commit.clone(),
        instance: None,
        route_kinds: Vec::new(),
        tool_calls: Vec::new(),
    }
}

/// The public view as the apex's drawing draws it: [`public`], with where each service runs
/// and what it is reached through, so services sit in the right row. Never served as JSON:
/// `/architecture.json` stays [`public`]'s.
pub fn public_drawable(m: &Model) -> Model {
    let mut shown = public(m);
    for e in &mut shown.externals {
        if let Some(full) = m.externals.iter().find(|x| x.name == e.name) {
            e.place.clone_from(&full.place);
            e.through.clone_from(&full.through);
        }
    }
    shown
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
                    place: e.place.clone(),
                    through: e.through.clone(),
                    out: e.out.clone(),
                    inbound: e.inbound.clone(),
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
    let route_kinds = route_kinds(&sorted, &entries, &workers, hosted);
    let tool_calls = tool_calls(&sorted, &entries, &workers, hosted);
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
        instance: None,
        route_kinds,
        tool_calls,
    }
}

/// The Worker a source file runs in: the Worker running the capability its module is, else the
/// Worker whose own crate or lane holds it (one other than core when several do), else core.
fn worker_of_file(path: &str, entries: &[Entry], workers: &[Worker], hosted: &Hosted) -> String {
    if let Some(module) = scan::module_of(path)
        && let Some(c) = hosted.capabilities.iter().find(|c| c.name() == module)
    {
        return c.worker().unwrap_or(hosted.worker).to_string();
    }
    if let Some(e) = entries
        .iter()
        .find(|e| !e.dir.is_empty() && path.starts_with(&format!("{}src/", e.dir)))
    {
        return e.script.clone();
    }
    let lanes: Vec<&Entry> = entries
        .iter()
        .filter(|e| {
            e.lane
                .as_ref()
                .is_some_and(|l| l.paths.iter().any(|p| pigeon_core::lanes::matches(p, path)))
        })
        .collect();
    lanes
        .iter()
        .find(|e| e.script != hosted.worker)
        .or(lanes.first())
        .map(|e| e.script.clone())
        .filter(|w| workers.iter().any(|x| x.name == *w))
        .unwrap_or_else(|| hosted.worker.to_string())
}

/// Each declared route ([`scan::declared`]), with the capability and Worker that serve it: the
/// capability it names, or whose module it is in, or whose route prefix covers it.
fn route_kinds(
    sorted: &[&Source],
    entries: &[Entry],
    workers: &[Worker],
    hosted: &Hosted,
) -> Vec<RouteKind> {
    // Each capability's route prefixes, longest first.
    let mut prefixes: Vec<(&str, &dyn Capability)> = hosted
        .capabilities
        .iter()
        .flat_map(|c| c.routes().into_iter().map(move |r| (r, c.as_ref())))
        .collect();
    prefixes.sort_by_key(|(r, _)| std::cmp::Reverse(r.len()));
    let worker_of = |c: &dyn Capability| c.worker().unwrap_or(hosted.worker).to_string();
    scan::declared(sorted)
        .into_iter()
        .map(|d| {
            let named = [
                d.pinned.as_str(),
                scan::module_of(&d.file).unwrap_or_default(),
            ]
            .into_iter()
            .filter(|n| !n.is_empty())
            .find_map(|n| hosted.capabilities.iter().find(|c| c.name() == n))
            .map(|c| c.as_ref());
            let covering = prefixes
                .iter()
                .find(|(r, _)| d.path.starts_with(r))
                .map(|(_, c)| *c);
            let by = named.or(covering);
            let worker = match by {
                Some(c) => worker_of(c),
                None => worker_of_file(&d.file, entries, workers, hosted),
            };
            RouteKind {
                method: d.method,
                path: d.path,
                kind: d.kind,
                by: by.map(|c| c.name().to_string()).unwrap_or_default(),
                worker,
                at: format!("{}:{}", d.file, d.line),
            }
        })
        .collect()
}

/// Which Worker calls which other Worker's MCP tools: each literal tool at a `dispatch(...)`
/// call site, from the Worker its file runs in, and each read the console's ops allow-list
/// makes, from the Worker running the console. A tool's host is the Worker that lists it, else
/// the Worker listing most tools of its prefix (such as `models_`).
fn tool_calls(
    sorted: &[&Source],
    entries: &[Entry],
    workers: &[Worker],
    hosted: &Hosted,
) -> Vec<ToolCall> {
    let sites = scan::dispatch_sites(sorted);
    let mut from: Vec<(String, &scan::Site)> = sites
        .iter()
        .map(|s| (worker_of_file(&s.file, entries, workers, hosted), s))
        .collect();
    // The console's reads: `app_*` are its own, not tools.
    let console = hosted.capabilities.iter().find(|c| c.name() == "console");
    let reads: Vec<scan::Site> = console
        .map(|_| {
            crate::console::ops::READS
                .iter()
                .filter(|(n, _)| !n.starts_with("app_"))
                .map(|(n, _)| scan::Site {
                    tool: (*n).to_string(),
                    file: "src/capabilities/src/console/web/ops.rs".into(),
                    line: 0,
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(c) = console {
        let w = c.worker().unwrap_or(hosted.worker).to_string();
        from.extend(reads.iter().map(|s| (w.clone(), s)));
    }
    scan::group(&from, |tool| tool_host(workers, tool))
}

/// The Worker hosting `tool`, else the one hosting most tools of its prefix.
fn tool_host(workers: &[Worker], tool: &str) -> Option<String> {
    if let Some(w) = workers.iter().find(|w| w.tools.iter().any(|t| t == tool)) {
        return Some(w.name.clone());
    }
    let prefix = &tool[..=tool.find('_')?];
    workers
        .iter()
        .map(|w| (w, w.tools.iter().filter(|t| t.starts_with(prefix)).count()))
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(w, _)| w.name.clone())
}

/// The architecture of one instance other than home (docs/plans/capability-scoping.md, M1):
/// what the platform runs for it, read from its approved record alone. Its Worker (with its
/// routes, workers.dev preview and the tools its scoped MCP endpoint offers), the hostnames
/// that Worker serves, its sites' bucket and zone, and the capabilities enabled on it as its
/// parts, each with the summary the platform's own `platform` model gives that name. The commit
/// and digest are the platform's: the code the instance's capabilities run. Nothing of the
/// platform's own Workers, routes or resources is in it. `platform_zone` is the zone the
/// instance's platform subdomains sit in.
pub fn instance_model(
    record: &crate::instances::record::Instance,
    enabled: &[&str],
    tools: &[String],
    platform: &Value,
) -> Model {
    use crate::instances::record::{RepoRef, Target, worker_for};
    let worker = record
        .worker
        .clone()
        .or_else(|| worker_for(record).ok().flatten())
        .unwrap_or_else(|| record.id.clone());
    let platform_zone = platform["resources"]["zone"].as_str();
    let mut routes = Vec::new();
    let mut hostnames = Vec::new();
    let mut workers_dev = false;
    for target in record.sites.iter().flat_map(|s| s.targets.iter()) {
        let host = match target {
            Target::Preview => {
                workers_dev = true;
                continue;
            }
            Target::Zone {
                hostname, route, ..
            } => {
                routes.push(route.clone());
                hostname.clone()
            }
            Target::Domain { hostname } => hostname.clone(),
            Target::Subdomain { label } => match platform_zone {
                Some(zone) => crate::instances::record::subdomain_host(label, zone),
                None => continue,
            },
        };
        if !hostnames.iter().any(|h: &Hostname| h.host == host) {
            hostnames.push(Hostname {
                host,
                worker: worker.clone(),
                by: "instances".into(),
                access: Vec::new(),
                behind_access: false,
                public_paths: Vec::new(),
            });
        }
    }
    hostnames.sort_by(|a, b| a.host.cmp(&b.host));
    routes.sort();
    routes.dedup();
    let summary = |name: &str| {
        platform["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["name"] == name)
            .and_then(|p| p["summary"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let parts: Vec<Part> = enabled
        .iter()
        .map(|name| Part {
            name: (*name).to_string(),
            capability: true,
            summary: summary(name),
            interface: Vec::new(),
            calls: Vec::new(),
            permissions: Vec::new(),
            bindings: Vec::new(),
        })
        .collect();
    let mut buckets: Vec<String> = record.sites.iter().map(|s| s.bucket.clone()).collect();
    buckets.sort();
    buckets.dedup();
    let repos = match &record.repo {
        Some(RepoRef::Artifacts { namespace, repo }) => vec![format!("{namespace}/{repo}")],
        Some(RepoRef::Github { full_name, .. }) => vec![full_name.clone()],
        None => Vec::new(),
    };
    let mut tools = tools.to_vec();
    tools.sort();
    tools.dedup();
    let text = |key: &str| platform[key].as_str().unwrap_or_default().to_string();
    Model {
        parts,
        inventory: Vec::new(),
        workers: vec![Worker {
            name: worker,
            source: "the instance record".into(),
            workers_dev,
            routes,
            tools,
            handlers: if record.sites.is_empty() {
                Vec::new()
            } else {
                vec!["fetch".into()]
            },
            uses: enabled.iter().map(|n| (*n).to_string()).collect(),
            ..Worker::default()
        }],
        hostnames,
        links: Vec::new(),
        externals: Vec::new(),
        resources: Resources {
            zone: record.zones.first().cloned(),
            repos,
            buckets,
            ..Resources::default()
        },
        versions: Vec::new(),
        files: 0,
        digest: text("digest"),
        commit: text("commit"),
        instance: Some(InstanceRef {
            id: record.id.clone(),
            title: record.title.clone(),
            home: false,
        }),
        route_kinds: Vec::new(),
        tool_calls: Vec::new(),
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
// The drawing is laid out from the model ([`layout`]) and painted ([`draw`]): the layered map,
// rows in the order a request travels. Definitions of the Workers and capabilities follow it.

/// The map of `m` as an SVG: the platform's, or one instance's own ([`instance_model`]), in full
/// and wide, light or dark as the viewer prefers.
pub fn diagram(m: &Model) -> String {
    draw(m, Mode::Full, Width::Wide, Theme::Auto).svg
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

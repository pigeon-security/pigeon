//! pigeon's architecture, drawn in the design system from two sources, so nothing on it is
//! written by hand and the same inputs always give the same page:
//! - the repository's code: the building blocks in `src/core/src` and the capabilities in
//!   `src/capabilities/src` (each module's `//!` summary, interface, the modules it uses and
//!   the permission names in `Policy::new(...)` and `*PERMISSIONS*` consts), and the hosting
//!   Worker's `wrangler.toml`, `(Method::X, "/path")` routes and `#[event(...)]` handlers;
//! - the capabilities the Worker hosts: their MCP tools and their footprints, the hostnames,
//!   Workers and resources each brings.

use std::collections::{BTreeMap, BTreeSet};

use pigeon_core::design::{Page, escape};
use pigeon_core::mcp::Tool;
use serde::Serialize;
use serde_json::Value;

use crate::console::sections::{Section, Tab};
use crate::users::User;
use crate::{Call, Capability, Context, Footprint, Hosted, to_value, unknown};

/// The architecture as a capability: its model, on demand.
pub struct Architecture;

impl Capability for Architecture {
    fn name(&self) -> &'static str {
        "architecture"
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Architecture,
            id: "architecture",
            title: "Architecture",
            description: "Drawn from the code and the capabilities' footprints.",
            order: 1,
            cost: None,
        }]
    }

    /// The drawing, as HTML the console places as it is.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let sources: Vec<Source> = cx
                .hosted
                .sources
                .iter()
                .map(|(path, text)| Source { path, text })
                .collect();
            Ok(serde_json::json!({ "html": render(&describe(&sources, cx.hosted)) }))
        })
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool::bare(
            "architecture_describe",
            "The platform's architecture as data: building blocks, capabilities, Workers, \
             hostnames and resources, read from the sources this Worker was built with.",
        )]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != "architecture_describe" {
                return Err(unknown(tool));
            }
            let sources: Vec<Source> = cx
                .hosted
                .sources
                .iter()
                .map(|(path, text)| Source { path, text })
                .collect();
            Ok(to_value(describe(&sources, cx.hosted)))
        })
    }
}

/// One source file, by its path from the repository root.
pub struct Source<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

/// The platform, as its code and capabilities describe it.
#[derive(Serialize)]
pub struct Model {
    /// The capabilities, then the building blocks.
    pub parts: Vec<Part>,
    pub workers: Vec<Worker>,
    pub hostnames: Vec<Hostname>,
    pub resources: Resources,
    /// How many files it was read from, and a digest of them.
    pub files: usize,
    pub digest: String,
}

/// A capability or a building block.
#[derive(Serialize)]
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
}

#[derive(Serialize)]
pub struct Worker {
    pub name: String,
    pub workers_dev: bool,
    pub routes: Vec<String>,
    pub tools: Vec<String>,
    /// `fetch`, `queue`, `scheduled`.
    pub handlers: Vec<String>,
    pub secrets: Vec<String>,
    /// The capabilities it hosts.
    pub uses: Vec<String>,
    /// The capability that publishes it, for a Worker with no code of its own.
    pub published_by: Option<String>,
}

#[derive(Serialize)]
pub struct Hostname {
    pub host: String,
    pub worker: String,
    /// The capability that serves it.
    pub by: String,
    /// The service tokens Access admits; empty when public.
    pub access: Vec<String>,
    /// Paths on it Access lets everyone through.
    pub public_paths: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct Resources {
    pub zone: Option<String>,
    pub secrets: Vec<String>,
    pub queues: Vec<String>,
    pub repos: Vec<String>,
    pub mirrors: Vec<String>,
    /// Durable Objects, by binding.
    pub objects: Vec<String>,
    /// AI gateways, by ID.
    pub gateways: Vec<String>,
    /// Every permission a minted token can hold.
    pub permissions: Vec<String>,
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

    // The hosting Worker: its wrangler.toml and the Rust beside it.
    if let Some(toml) = sorted.iter().find(|s| {
        s.path.ends_with("wrangler.toml")
            && Toml::read(s.text).name.as_deref() == Some(hosted.worker)
    }) {
        let dir = toml.path.trim_end_matches("wrangler.toml");
        let config = Toml::read(toml.text);
        let rust: String = sorted
            .iter()
            .filter(|s| s.path.starts_with(&format!("{dir}src/")) && s.path.ends_with(".rs"))
            .map(|s| code(s.text))
            .collect::<Vec<_>>()
            .join("\n");
        resources.secrets.extend(config.secrets.iter().cloned());
        workers.push(Worker {
            name: hosted.worker.to_string(),
            workers_dev: config.workers_dev,
            routes: routes(&rust),
            tools: hosted
                .capabilities
                .iter()
                .flat_map(|c| c.tools())
                .map(|t| t.name.to_string())
                .collect(),
            handlers: handlers(&rust),
            secrets: config.secrets,
            uses: hosted
                .capabilities
                .iter()
                .map(|c| c.name().to_string())
                .collect(),
            published_by: None,
        });
    }

    for (by, f) in &footprints {
        for h in &f.hostnames {
            hostnames.push(Hostname {
                host: h.host.clone(),
                worker: h.worker.clone(),
                by: h.by.clone(),
                access: h.access.clone(),
                public_paths: h.public_paths.clone(),
            });
        }
        for w in &f.workers {
            workers.push(Worker {
                name: w.name.clone(),
                workers_dev: false,
                routes: w.routes.clone(),
                tools: Vec::new(),
                handlers: vec!["fetch".into()],
                secrets: Vec::new(),
                uses: Vec::new(),
                published_by: Some(by.clone()),
            });
        }
        if f.zone.is_some() {
            resources.zone.clone_from(&f.zone);
        }
        resources.secrets.extend(f.secrets.iter().cloned());
        resources.queues.extend(f.queues.iter().cloned());
        resources.repos.extend(f.repos.iter().cloned());
        resources.mirrors.extend(f.mirrors.iter().cloned());
        resources.objects.extend(f.objects.iter().cloned());
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
        &mut resources.gateways,
        &mut resources.permissions,
    ] {
        list.sort();
        list.dedup();
    }
    hostnames.sort_by(|a, b| a.host.cmp(&b.host));

    let mut parts = caps;
    parts.extend(blocks);
    Model {
        parts,
        workers,
        hostnames,
        resources,
        files: sorted.len(),
        digest: digest(&sorted),
    }
}

/// The architecture page of `model`.
pub fn page(model: &Model) -> String {
    Page {
        title: "pigeon architecture",
        description: "pigeon's architecture, read from its own source.",
        body: &render(model),
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
            })
        })
        .collect();
    // Those built on most come first; then by name.
    let depth: BTreeMap<String, usize> = out
        .iter()
        .map(|p| (p.name.clone(), depth_of(&p.name, &out, 0)))
        .collect();
    out.sort_by(|a, b| {
        depth[&b.name]
            .cmp(&depth[&a.name])
            .then(a.name.cmp(&b.name))
    });
    out
}

fn depth_of(name: &str, all: &[Part], seen: usize) -> usize {
    if seen > all.len() {
        return 0;
    }
    all.iter().find(|c| c.name == name).map_or(0, |c| {
        c.calls
            .iter()
            .map(|d| 1 + depth_of(d, all, seen + 1))
            .max()
            .unwrap_or(0)
    })
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
        } else if let (Some(ty), Some(rest)) = (&within, line.strip_prefix("    ")) {
            if let Some(name) = fn_name(rest) {
                out.push(format!("{ty}::{name}"));
            }
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

#[derive(Default)]
struct Toml {
    name: Option<String>,
    workers_dev: bool,
    vars: BTreeMap<String, String>,
    secrets: Vec<String>,
}

impl Toml {
    fn read(text: &str) -> Self {
        let mut t = Self {
            workers_dev: true,
            ..Self::default()
        };
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                section = line.trim_matches(|c| c == '[' || c == ']').to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let quoted = strings(value).into_iter().next();
            match (section.as_str(), key) {
                ("", "name") => t.name = quoted,
                ("", "workers_dev") => t.workers_dev = value == "true",
                ("vars", k) => {
                    if let Some(v) = quoted {
                        t.vars.insert(k.to_string(), v);
                    }
                }
                ("secrets_store_secrets", "binding") => t.secrets.extend(quoted),
                _ => {}
            }
        }
        t
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

/// The names of the `mcp::Tool`s defined.

fn handlers(text: &str) -> Vec<String> {
    ["fetch", "queue", "scheduled"]
        .into_iter()
        .filter(|h| text.contains(&format!("#[event({h})]")))
        .map(String::from)
        .collect()
}

/// The capabilities `text` uses through `pigeon_capabilities::`.

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

/// An SVG drawn in order.
struct Svg(Vec<String>);

impl Svg {
    fn push(&mut self, s: String) {
        self.0.push(s);
    }

    /// A card with the hard offset shadow.
    fn card(&mut self, x: f64, y: f64, w: f64, h: f64, class: &str) {
        self.push(format!(
            r#"<rect class="dg-shadow" x="{:.0}" y="{:.0}" width="{w:.0}" height="{h:.0}" rx="14"/><rect class="dg-card {class}" x="{x:.0}" y="{y:.0}" width="{w:.0}" height="{h:.0}" rx="14"/>"#,
            x + 3.0,
            y + 3.0
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

    /// Pills in a row from `x`, as many as fit before `right`.
    fn chips(&mut self, x: f64, y: f64, right: f64, items: &[String], accent: bool) {
        let mut at = x;
        for item in items {
            if at + chip_width(item) > right {
                break;
            }
            at += self.chip(at, y, item, accent) + 8.0;
        }
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

/// One column: a hostname and the Worker that serves it, or a Worker that serves none.
struct Column<'a> {
    host: Option<&'a Hostname>,
    worker: Option<&'a Worker>,
}

fn columns(m: &Model) -> Vec<Column<'_>> {
    let mut out: Vec<Column> = m
        .hostnames
        .iter()
        .map(|h| Column {
            host: Some(h),
            worker: m.workers.iter().find(|w| w.name == h.worker),
        })
        .collect();
    for w in &m.workers {
        if !m.hostnames.iter().any(|h| h.worker == w.name) {
            out.push(Column {
                host: None,
                worker: Some(w),
            });
        }
    }
    out
}

/// The capabilities that only shape pages, and so reach no account resource.
const PAGE_ONLY: [&str; 3] = ["design", "architecture", "mcp"];

fn diagram(m: &Model) -> String {
    let cols = columns(m);
    let n = cols.len().max(1) as f64;
    let col_w = (W - 2.0 * M - (n - 1.0) * GAP) / n;
    let col_x = |i: usize| M + i as f64 * (col_w + GAP);
    let mut s = Svg(Vec::new());

    // Layer 1: clients, above the hostname each reaches.
    let y1 = 40.0;
    s.text(M, 26.0, "dg-e", "LAYER 1 · CLIENTS", "start");
    let group_h = 190.0;
    for (i, c) in cols.iter().enumerate() {
        let Some(h) = c.host else { continue };
        let x = col_x(i);
        s.card(x, y1, col_w, group_h, "dg-group");
        let (title, inner): (&str, Vec<(String, String)>) = if h.access.is_empty() {
            ("Visitors", vec![("Browser".into(), "anyone".into())])
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
            s.text(ix + 16.0, y1 + 78.0, "dg-t", name, "start");
            s.chip(ix + 16.0, y1 + 94.0, kind, false);
        }
        let via = if h.access.is_empty() {
            format!("{} · in public", h.host)
        } else {
            format!("{} · Access service token", h.host)
        };
        s.chip(x + 16.0, y1 + 152.0, &via, false);
    }

    // The Cloudflare region holds layers 2 to 4; its rectangle is drawn once its height is known.
    let zone_top = y1 + group_h + 48.0;
    let zone_at = s.0.len();
    s.text(
        M + 4.0,
        zone_top + 26.0,
        "dg-zone-name",
        "CLOUDFLARE",
        "start",
    );

    // Layer 2: hostnames, each a Zero Trust application or public.
    let y2 = zone_top + 64.0;
    s.text(M + 4.0, y2 - 12.0, "dg-e", "LAYER 2 · ZERO TRUST", "start");
    let host_h = 76.0;
    for (i, c) in cols.iter().enumerate() {
        let Some(h) = c.host else { continue };
        let x = col_x(i);
        let cx = x + col_w / 2.0;
        s.link(
            &format!("M{cx:.0} {:.0}V{y2:.0}", y1 + group_h),
            false,
            false,
        );
        s.label(cx + 10.0, y1 + group_h + 26.0, "HTTPS");
        let class = if h.access.is_empty() { "" } else { "dg-access" };
        s.card(x, y2, col_w, host_h, class);
        s.text(x + 16.0, y2 + 32.0, "dg-t dg-big", &h.host, "start");
        let sub = if h.access.is_empty() {
            "public · no Access".to_string()
        } else {
            format!("Zero Trust application · admits {}", h.access.join(", "))
        };
        let sub = if h.public_paths.is_empty() {
            sub
        } else {
            format!("{sub} · public: {}", h.public_paths.join(", "))
        };
        s.text(x + 16.0, y2 + 56.0, "dg-s", &sub, "start");
        let cw = chip_width(&h.by);
        s.chip(x + col_w - 16.0 - cw, y2 + 14.0, &h.by, true);
    }

    // Layer 3: the Worker under each hostname.
    let y3 = y2 + host_h + 64.0;
    s.text(M + 4.0, y3 - 12.0, "dg-e", "LAYER 3 · WORKERS", "start");
    let worker_h = 164.0;
    for (i, c) in cols.iter().enumerate() {
        let Some(w) = c.worker else { continue };
        let x = col_x(i);
        let cx = x + col_w / 2.0;
        if c.host.is_some() {
            s.link(
                &format!("M{cx:.0} {:.0}V{y3:.0}", y2 + host_h),
                false,
                false,
            );
            s.label(cx + 10.0, y2 + host_h + 30.0, "serves");
        }
        s.card(x, y3, col_w, worker_h, "dg-worker");
        s.text(
            x + 16.0,
            y3 + 32.0,
            "dg-t dg-big dg-wname",
            &w.name,
            "start",
        );
        let mut flags: Vec<String> = w.handlers.clone();
        flags.push(match (&w.published_by, w.workers_dev) {
            (Some(by), _) => format!("published by {by}"),
            (None, true) => "workers.dev on".into(),
            (None, false) => "workers.dev off".into(),
        });
        s.chips(x + 16.0, y3 + 46.0, x + col_w - 16.0, &flags, false);
        s.stack(x + 16.0, y3 + 84.0, col_w - 32.0, 62.0);
        let routes = format!("{} routes", w.routes.len());
        s.text(x + 32.0, y3 + 108.0, "dg-t", &routes, "start");
        let tools = if w.tools.is_empty() {
            "no MCP tools".to_string()
        } else {
            format!("{} MCP tools on POST /mcp", w.tools.len())
        };
        s.text(x + 32.0, y3 + 128.0, "dg-s", &tools, "start");
    }

    // Layer 4: the account resources, shared on one bus by the Workers that reach them.
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
    for o in &r.objects {
        cards.push(("Durable Object", o.clone(), "dg-store"));
    }
    for repo in &r.repos {
        cards.push(("Artifacts", repo.clone(), "dg-cf"));
    }
    let consumers: Vec<&str> = m
        .workers
        .iter()
        .filter(|w| w.handlers.iter().any(|h| h == "queue"))
        .map(|w| w.name.as_str())
        .collect();
    for q in &r.queues {
        cards.push(("Queue", q.clone(), "dg-cf"));
    }
    let bus_y = y3 + worker_h + 36.0;
    let y4 = bus_y + 62.0;
    let k = cards.len().max(1) as f64;
    let rgap = 20.0;
    let rw = (W - 2.0 * M - (k - 1.0) * rgap) / k;
    let rx = |j: usize| M + j as f64 * (rw + rgap);
    let users: Vec<f64> = cols
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            c.worker
                .is_some_and(|w| w.uses.iter().any(|u| !PAGE_ONLY.contains(&u.as_str())))
        })
        .map(|(i, _)| col_x(i) + col_w / 2.0)
        .collect();
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
        for u in &users {
            s.link(
                &format!("M{u:.0} {:.0}V{bus_y:.0}", y3 + worker_h),
                false,
                true,
            );
        }
        s.link(&format!("M{left:.0} {bus_y:.0}H{right:.0}"), false, true);
        s.text(
            users[0] - 10.0,
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
        "LAYER 4 · ACCOUNT RESOURCES",
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
        if *title == "Queue" {
            let qx = x + rw / 2.0 + 28.0;
            for (i, c) in cols.iter().enumerate() {
                let consumes = c
                    .worker
                    .is_some_and(|w| consumers.contains(&w.name.as_str()));
                if consumes && qx > col_x(i) && qx < col_x(i) + col_w {
                    s.link(
                        &format!("M{qx:.0} {y4:.0}V{:.0}", y3 + worker_h + 2.0),
                        true,
                        false,
                    );
                    s.label(qx + 8.0, bus_y + 24.0, "consumes");
                }
            }
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
    let mut height = zone_bottom + 24.0;
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
        height = vy + 178.0 + 24.0;
    }

    format!(
        r#"<div class="dg-canvas"><svg class="diagram" viewBox="0 0 {W:.0} {height:.0}" role="img" aria-label="pigeon's architecture, from clients through hostnames and Workers to account resources and mirrors"><defs><marker id="ah" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse"><path class="dg-head" d="M0 0L10 5L0 10z"/></marker></defs>{}</svg></div>"#,
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

/// The drawing alone, without a page around it: for the architecture page and the console.
pub fn render(m: &Model) -> String {
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
            for (what, items) in [
                ("MCP tools", &w.tools),
                ("uses", &w.uses),
                ("Secrets Store bindings", &w.secrets),
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
  <p>Read from the repository's own source each time the site is built, and drawn top to bottom in the order a request travels. Nothing here is written by hand.</p>
  <div class="pills"><span class="pill">{} Workers</span><span class="pill">{} capabilities</span><span class="pill">{} core blocks</span><span class="pill">{tool_count} MCP tools</span><span class="pill">digest {}</span></div>
</header>
<section aria-label="Diagram" style="margin-top:24px">{}</section>
{}
{}
{}
{}
</main>
<footer>Generated from {} source files · digest {}</footer>"#,
        m.workers.len(),
        m.parts.iter().filter(|c| c.capability).count(),
        m.parts.iter().filter(|c| !c.capability).count(),
        m.digest,
        diagram(m),
        section("Definitions", "Workers", &workers),
        section("Definitions", "Capabilities", &public),
        section("Foundation", "Core building blocks", &internal),
        section("Definitions", "Account resources", &resources),
        m.files,
        m.digest,
    )
}

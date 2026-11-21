//! The platform's architecture, read from its own source and drawn in the design system. The
//! same sources always give the same model and page; nothing is written by hand.
//!
//! What it reads, by convention:
//! - capabilities: each module of `src/capabilities/src`, `pub mod` or internal in `lib.rs`;
//!   its `//!` summary, top-level `pub fn`s and `pub` methods, `crate::` references to other
//!   modules, and the permission names in `Policy::new(...)` and `*PERMISSION*` consts;
//! - Workers: each `wrangler.toml` and the Rust beside it; name, `workers_dev`, vars, Secrets
//!   Store bindings, `(Method::X, "/path")` routes, `mcp::Tool` names, `#[event(...)]`
//!   handlers and the capabilities it uses;
//! - hostnames: a Worker using `subdomain` serves its `*_HOST` var behind Access; one calling
//!   `domain::serve` serves the `ZONE` apex from the Worker named there, in public;
//! - resources: Secrets Store names, `queue:`, the Artifacts `namespace:`/`name:` and the
//!   mirror `url:` named in the Worker's code.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::design::{Page, escape};

/// One source file, by its path from the repository root.
pub struct Source<'a> {
    pub path: &'a str,
    pub text: &'a str,
}

/// The platform, as its source describes it.
#[derive(Serialize)]
pub struct Model {
    pub capabilities: Vec<Capability>,
    pub workers: Vec<Worker>,
    pub hostnames: Vec<Hostname>,
    pub resources: Resources,
    /// How many files it was read from, and a digest of them.
    pub files: usize,
    pub digest: String,
}

#[derive(Serialize)]
pub struct Capability {
    pub name: String,
    /// Exported from the crate; internal modules serve capabilities only.
    pub public: bool,
    pub summary: String,
    pub interface: Vec<String>,
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
    pub uses: Vec<String>,
}

#[derive(Serialize)]
pub struct Hostname {
    pub host: String,
    pub worker: String,
    /// The capability that serves it.
    pub by: String,
    /// The service tokens Access admits; empty when public.
    pub access: Vec<String>,
}

#[derive(Serialize, Default)]
pub struct Resources {
    pub zone: Option<String>,
    pub secrets: Vec<String>,
    pub queues: Vec<String>,
    pub repos: Vec<String>,
    pub mirrors: Vec<String>,
    /// Every permission a minted token can hold.
    pub permissions: Vec<String>,
}

/// The model of `sources`.
pub fn describe(sources: &[Source]) -> Model {
    let mut sorted: Vec<&Source> = sources.iter().collect();
    sorted.sort_by_key(|s| s.path);
    let capabilities = capabilities(&sorted);
    let names: BTreeSet<String> = capabilities.iter().map(|c| c.name.clone()).collect();
    let mut workers = Vec::new();
    let mut hostnames = Vec::new();
    let mut resources = Resources::default();
    let toml: Vec<&&Source> = sorted
        .iter()
        .filter(|s| s.path.ends_with("wrangler.toml"))
        .collect();
    let worker_names: Vec<String> = toml
        .iter()
        .filter_map(|t| Toml::read(t.text).name)
        .collect();
    for t in &toml {
        let dir = t.path.trim_end_matches("wrangler.toml");
        let config = Toml::read(t.text);
        let rust: String = sorted
            .iter()
            .filter(|s| s.path.starts_with(&format!("{dir}src/")) && s.path.ends_with(".rs"))
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .join("\n");
        let consts = str_consts(&rust);
        let name = config.name.clone().unwrap_or_else(|| dir.to_string());
        let uses = uses(&rust, &names);
        let tokens = access_tokens(&rust, &consts);
        if uses.iter().any(|u| u == "subdomain") {
            for (var, host) in &config.vars {
                if var.ends_with("_HOST") {
                    hostnames.push(Hostname {
                        host: host.clone(),
                        worker: name.clone(),
                        by: "subdomain".into(),
                        access: tokens.clone(),
                    });
                }
            }
        }
        if let (Some(apex), Some(served)) = (config.vars.get("ZONE"), apex_worker(&rust, &consts)) {
            if worker_names.contains(&served) {
                hostnames.push(Hostname {
                    host: apex.clone(),
                    worker: served,
                    by: "domain".into(),
                    access: Vec::new(),
                });
            }
        }
        if let Some(zone) = config.vars.get("ZONE") {
            resources.zone = Some(zone.clone());
        }
        resources.secrets.extend(config.secrets.iter().cloned());
        resources.secrets.extend(field_values(
            &rust,
            "client_id_secret",
            &consts,
            "protection: Protection {",
        ));
        resources.secrets.extend(field_values(
            &rust,
            "client_secret_secret",
            &consts,
            "protection: Protection {",
        ));
        resources
            .queues
            .extend(field_values(&rust, "queue", &consts, "queues::Spec {"));
        let namespaces = field_values(&rust, "namespace", &consts, "artifacts::Spec {");
        let repos = field_values(&rust, "name", &consts, "artifacts::Spec {");
        for (ns, repo) in namespaces.iter().zip(&repos) {
            resources.repos.push(format!("{ns}/{repo}"));
        }
        resources
            .mirrors
            .extend(field_values(&rust, "url", &consts, "Copy {"));
        resources.permissions.extend(permissions(&rust));
        workers.push(Worker {
            name,
            workers_dev: config.workers_dev,
            routes: routes(&rust),
            tools: tools(&rust),
            handlers: handlers(&rust),
            secrets: config.secrets,
            uses,
        });
    }
    for c in &capabilities {
        resources.permissions.extend(c.permissions.iter().cloned());
    }
    for list in [
        &mut resources.secrets,
        &mut resources.queues,
        &mut resources.repos,
        &mut resources.mirrors,
        &mut resources.permissions,
    ] {
        list.sort();
        list.dedup();
    }
    hostnames.sort_by(|a, b| a.host.cmp(&b.host));
    Model {
        capabilities,
        workers,
        hostnames,
        resources,
        files: sorted.len(),
        digest: digest(&sorted),
    }
}

/// The architecture page of `sources`.
pub fn page(sources: &[Source]) -> String {
    let model = describe(sources);
    Page {
        title: "pigeon architecture",
        description: "pigeon's architecture, read from its own source.",
        body: &render(&model),
    }
    .render()
}

// ---- capabilities ----

fn capabilities(sorted: &[&Source]) -> Vec<Capability> {
    const DIR: &str = "src/capabilities/src/";
    let lib = sorted
        .iter()
        .find(|s| s.path == format!("{DIR}lib.rs"))
        .map_or("", |s| s.text);
    let mut public = BTreeSet::new();
    let mut modules = BTreeSet::new();
    for line in lib.lines() {
        let line = line.trim();
        if let Some(m) = line
            .strip_prefix("pub mod ")
            .and_then(|m| m.strip_suffix(';'))
        {
            public.insert(m.to_string());
            modules.insert(m.to_string());
        } else if let Some(m) = line.strip_prefix("mod ").and_then(|m| m.strip_suffix(';')) {
            modules.insert(m.to_string());
        }
    }
    let mut out: Vec<Capability> = sorted
        .iter()
        .filter_map(|s| {
            let name = s.path.strip_prefix(DIR)?.strip_suffix(".rs")?;
            if name.contains('/') || !modules.contains(name) {
                return None;
            }
            let code = code(s.text);
            let mut calls: Vec<String> = crate_refs(&code, &modules)
                .into_iter()
                .filter(|c| c != name)
                .collect();
            calls.sort();
            Some(Capability {
                name: name.to_string(),
                public: public.contains(name),
                summary: summary(s.text),
                interface: interface(s.text),
                calls,
                permissions: permissions(&code),
            })
        })
        .collect();
    // Those that call most come first; then by name.
    let depth: BTreeMap<String, usize> = out
        .iter()
        .map(|c| (c.name.clone(), depth_of(&c.name, &out, 0)))
        .collect();
    out.sort_by(|a, b| {
        depth[&b.name]
            .cmp(&depth[&a.name])
            .then(a.name.cmp(&b.name))
    });
    out
}

fn depth_of(name: &str, all: &[Capability], seen: usize) -> usize {
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

/// The modules `text` reaches through `crate::`.
fn crate_refs(text: &str, modules: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for at in found(text, "crate::") {
        let rest = &text[at + "crate::".len()..];
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
fn tools(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for at in found(text, "mcp::Tool") {
        let rest = &text[at + "mcp::Tool".len()..];
        let name = if let Some(r) = rest.strip_prefix("::bare(") {
            strings(r).into_iter().next()
        } else if rest.trim_start().starts_with('{') {
            rest.find("name:")
                .and_then(|i| strings(&rest[i..]).into_iter().next())
        } else {
            None
        };
        out.extend(name);
    }
    out
}

fn handlers(text: &str) -> Vec<String> {
    ["fetch", "queue", "scheduled"]
        .into_iter()
        .filter(|h| text.contains(&format!("#[event({h})]")))
        .map(String::from)
        .collect()
}

/// The capabilities `text` uses through `pigeon_capabilities::`.
fn uses(text: &str, names: &BTreeSet<String>) -> Vec<String> {
    let mut out = BTreeSet::new();
    for at in found(text, "pigeon_capabilities::") {
        let rest = &text[at + "pigeon_capabilities::".len()..];
        if let Some(group) = rest.strip_prefix('{') {
            let group = &group[..group.find('}').unwrap_or(group.len())];
            for item in group.split(',') {
                let first = item.trim().split("::").next().unwrap_or("").trim();
                if names.contains(first) {
                    out.insert(first.to_string());
                }
            }
        } else {
            let first = ident(rest);
            if names.contains(&first) {
                out.insert(first);
            }
        }
    }
    out.into_iter().collect()
}

/// The service tokens a `protection: Protection { ... }` lets in: its `service_token` and its
/// `guests`.
fn access_tokens(text: &str, consts: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = BTreeSet::new();
    out.extend(field_values(
        text,
        "service_token",
        consts,
        "protection: Protection {",
    ));
    for at in found(text, "protection: Protection {") {
        let block = balanced_braces(&text[at..]);
        if let Some(i) = block.find("guests:") {
            let list = &block[i..];
            let list = &list[..list.find(']').unwrap_or(list.len())];
            out.extend(strings(list));
            for word in list.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
                if let Some(v) = consts.get(word) {
                    out.insert(v.clone());
                }
            }
        }
    }
    out.into_iter().collect()
}

/// The Worker `domain::serve(account, zone, WORKER, ...)` names, resolved through consts.
fn apex_worker(text: &str, consts: &BTreeMap<String, String>) -> Option<String> {
    let at = found(text, "domain::serve(").into_iter().next()?;
    let args = balanced(&text[at + "domain::serve".len()..]);
    let third = args.trim_matches(['(', ')']).split(',').nth(2)?.trim();
    strings(third)
        .into_iter()
        .next()
        .or_else(|| consts.get(third).cloned())
}

/// The values of `field:` inside each block that starts with `opener`, as literals or consts.
fn field_values(
    text: &str,
    field: &str,
    consts: &BTreeMap<String, String>,
    opener: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for at in found(text, opener) {
        let block = balanced_braces(&text[at..]);
        let key = format!("{field}:");
        let Some(i) = block
            .match_indices(&key)
            .map(|(i, _)| i)
            .find(|&i| i == 0 || !block[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
        else {
            continue;
        };
        let value = block[i + key.len()..].trim_start();
        let value = &value[..value.find(['\n', ',']).unwrap_or(value.len())];
        if let Some(s) = strings(value).into_iter().next() {
            out.push(s);
        } else if let Some(v) = consts.get(&ident(value)) {
            out.push(v.clone());
        }
    }
    out
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

/// `const NAME: &str = "value";`, by name.
fn str_consts(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches("pub ");
        let Some(rest) = line.strip_prefix("const ") else {
            continue;
        };
        if !rest.contains(": &str =") {
            continue;
        }
        if let Some(v) = strings(rest).into_iter().next() {
            out.insert(ident(rest), v);
        }
    }
    out
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

/// `text` from its first `{` to the matching `}`.
fn balanced_braces(text: &str) -> &str {
    matched(text, '{', '}')
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

fn list(items: &[String]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let inner: String = items
        .iter()
        .map(|i| format!("<li><code>{}</code></li>", escape(i)))
        .collect();
    format!(r#"<ul class="kv">{inner}</ul>"#)
}

fn layer(n: usize, title: &str, cards: &str) -> String {
    format!(
        r#"<section class="layer"><span class="eyebrow">Layer {n} · {}</span><div class="cards">{cards}</div></section>"#,
        escape(title)
    )
}

fn arrow(label: &str) -> String {
    if label.is_empty() {
        r#"<div class="arrow" aria-hidden="true"></div>"#.into()
    } else {
        format!(r#"<div class="arrow"><span>{}</span></div>"#, escape(label))
    }
}

fn render(m: &Model) -> String {
    let tokens: Vec<String> = m
        .hostnames
        .iter()
        .flat_map(|h| h.access.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let public: Vec<&Hostname> = m.hostnames.iter().filter(|h| h.access.is_empty()).collect();
    let tool_count: usize = m.workers.iter().map(|w| w.tools.len()).sum();

    let mut clients = String::new();
    if !tokens.is_empty() {
        clients.push_str(&format!(
            r#"<article class="card"><div class="card-h"><span class="dot"></span><h3>Service-token callers</h3></div><p>Through Cloudflare Access, with a service token.</p>{}</article>"#,
            pills(&tokens, "")
        ));
    }
    if !public.is_empty() {
        let hosts: Vec<String> = public.iter().map(|h| h.host.clone()).collect();
        clients.push_str(&format!(
            r#"<article class="card"><div class="card-h"><span class="dot"></span><h3>Visitors</h3></div><p>Anyone, in public.</p>{}</article>"#,
            pills(&hosts, "")
        ));
    }

    let hostnames: String = m
        .hostnames
        .iter()
        .map(|h| {
            let (kind, access) = if h.access.is_empty() {
                ("", "public, no Access".to_string())
            } else {
                (" kind-access", format!("Access admits {}", h.access.join(", ")))
            };
            format!(
                r#"<article class="card{kind}"><div class="card-h"><span class="dot"></span><h3>{}</h3><span class="pill accent">{}</span></div><ul class="kv"><li>{}</li><li>serves {}</li></ul></article>"#,
                escape(&h.host),
                escape(&h.by),
                escape(&access),
                escape(&h.worker)
            )
        })
        .collect();

    let workers: String = m
        .workers
        .iter()
        .map(|w| {
            let mut body = list(&w.routes);
            if !w.tools.is_empty() {
                body.push_str(&format!(
                    r#"<span class="eyebrow">MCP tools</span>{}"#,
                    pills(&w.tools, "")
                ));
            }
            let mut notes = vec![format!("handles {}", w.handlers.join(", "))];
            notes.push(if w.workers_dev {
                "workers.dev on".into()
            } else {
                "workers.dev off".into()
            });
            format!(
                r#"<article class="card kind-worker"><div class="card-h"><span class="dot"></span><h3>{}</h3></div>{body}<span class="eyebrow">uses</span>{}<p>{}</p></article>"#,
                escape(&w.name),
                pills(&w.uses, " accent"),
                escape(&notes.join(" · "))
            )
        })
        .collect();

    let card = |c: &Capability| {
        let calls = if c.calls.is_empty() {
            String::new()
        } else {
            let inner: String = c
                .calls
                .iter()
                .map(|d| format!(r#"<span class="pill">{}</span>"#, escape(d)))
                .collect();
            format!(r#"<div class="chain"><span>calls</span>{inner}</div>"#)
        };
        let perms = if c.permissions.is_empty() {
            String::new()
        } else {
            format!(
                r#"<span class="eyebrow">token</span>{}"#,
                pills(&c.permissions, "")
            )
        };
        format!(
            r#"<article class="card"><div class="card-h"><h3>{}</h3></div>{}{calls}<p>{}</p>{perms}</article>"#,
            escape(&c.name),
            pills(&c.interface, " accent"),
            escape(&c.summary)
        )
    };
    let public_caps: String = m
        .capabilities
        .iter()
        .filter(|c| c.public)
        .map(card)
        .collect();
    let internal: String = m
        .capabilities
        .iter()
        .filter(|c| !c.public)
        .map(card)
        .collect();

    let r = &m.resources;
    let mut resources = String::new();
    if !r.secrets.is_empty() {
        resources.push_str(&format!(
            r#"<article class="card kind-store"><div class="card-h"><span class="dot"></span><h3>Secrets Store</h3></div>{}</article>"#,
            list(&r.secrets)
        ));
    }
    if !r.permissions.is_empty() {
        resources.push_str(&format!(
            r#"<article class="card kind-api"><div class="card-h"><span class="dot"></span><h3>API tokens</h3></div><p>Minted from the parent token, one per call, revoked after; together they hold:</p>{}</article>"#,
            pills(&r.permissions, "")
        ));
    }
    if let Some(zone) = &r.zone {
        let hosts: Vec<String> = m.hostnames.iter().map(|h| h.host.clone()).collect();
        resources.push_str(&format!(
            r#"<article class="card kind-api"><div class="card-h"><span class="dot"></span><h3>Zone {}</h3></div><p>custom domains</p>{}</article>"#,
            escape(zone),
            list(&hosts)
        ));
    }
    for (title, items) in [("Artifacts", &r.repos), ("Queues", &r.queues)] {
        if !items.is_empty() {
            resources.push_str(&format!(
                r#"<article class="card kind-api"><div class="card-h"><span class="dot"></span><h3>{title}</h3></div>{}</article>"#,
                list(items)
            ));
        }
    }

    let mirror = if r.mirrors.is_empty() {
        String::new()
    } else {
        let copies: String = r
            .mirrors
            .iter()
            .map(|u| {
                format!(
                    r#"<article class="card"><div class="card-h"><h3>{}</h3></div><p>read-only copy, mirrored from Artifacts</p></article>"#,
                    escape(u)
                )
            })
            .collect();
        format!(
            r#"{}<section class="region github" aria-label="Mirror"><span class="region-name">Mirror</span><div class="cards">{copies}</div></section>"#,
            arrow("mirror")
        )
    };

    let mut n = 0;
    let mut next = || {
        n += 1;
        n
    };
    let internal_layer = if internal.is_empty() {
        String::new()
    } else {
        format!(
            "{}{}",
            arrow(""),
            layer(0, "Internal modules", &internal).replace("Layer 0 · ", "")
        )
    };
    format!(
        r#"<main>
<header>
  <span class="eyebrow">pigeon · architecture</span>
  <h1>What's built</h1>
  <p>Read from the repository's own source each time the site is built: capabilities, Workers, hostnames and the resources they name. Nothing here is written by hand.</p>
  <div class="pills"><span class="pill">{} Workers</span><span class="pill">{} capabilities</span><span class="pill">{tool_count} MCP tools</span><span class="pill">digest {}</span></div>
</header>
{}
{}
{}
<section class="region cloudflare" aria-label="Cloudflare">
  <span class="region-name">Cloudflare</span>
  {}
  {}
  {}
  {}
  {}
  {}
  {}
  {}
</section>
{mirror}
<section class="layer" aria-label="Legend" style="margin-top:28px">
  <ul class="legend">
    <li><span class="dot kind-access"></span>Behind Access</li>
    <li><span class="dot"></span>Public or client</li>
    <li><span class="dot kind-worker"></span>Worker</li>
    <li><span class="dot kind-store"></span>Secrets</li>
    <li><span class="dot kind-api"></span>Cloudflare API object</li>
    <li><span class="pill accent">name</span>entry point</li>
  </ul>
</section>
</main>
<footer>Generated from {} source files · digest {}</footer>"#,
        m.workers.len(),
        m.capabilities.iter().filter(|c| c.public).count(),
        m.digest,
        arrow(""),
        layer(next(), "Clients", &clients),
        arrow("HTTPS"),
        layer(next(), "Hostnames", &hostnames),
        arrow(""),
        layer(next(), "Workers", &workers),
        arrow("calls"),
        layer(next(), "Capabilities", &public_caps),
        internal_layer,
        arrow("Cloudflare API · minted tokens"),
        layer(next(), "Account resources", &resources),
        m.files,
        m.digest,
    )
}

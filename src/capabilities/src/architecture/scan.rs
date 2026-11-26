//! What the drawing reads from the sources besides the Workers' configuration: how each route
//! checks its caller, as declared beside it, and which Worker calls which other Worker's MCP
//! tools. Pure: it reads the source texts it is given.
//!
//! A route declares its check with a comment on the line that names its path,
//! `(Method::Get, "/health") => ... // route: open`, or alone on the line after it (the first
//! line of the arm's block). A route served by a capability other than the one whose module it
//! is in names that capability: `// route: open @site`.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{Source, balanced, code, ident, strings};

/// How a route checks its caller, from least to most guarded.
pub const ROUTE_KINDS: [&str; 8] = [
    "open", "redirect", "sign-in", "session", "OAuth", "token", "job", "webhook",
];

/// Route kinds that need no credential.
pub const UNGUARDED: [&str; 2] = ["open", "redirect"];

/// One route and the check it declares.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RouteKind {
    /// `GET`, `POST`, ..., or `*` for any.
    pub method: String,
    /// The path, or a path prefix ending in `/`.
    pub path: String,
    /// One of [`ROUTE_KINDS`], as declared.
    pub kind: String,
    /// The capability that serves it, when it is a capability's.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// The Worker that serves it.
    pub worker: String,
    /// Where it is declared, as `path:line`.
    pub at: String,
}

/// A route declaration before it is attributed to a Worker: its route, kind, the capability it
/// names with `@`, and where it is.
pub struct Declared {
    pub method: String,
    pub path: String,
    pub kind: String,
    pub pinned: String,
    pub file: String,
    pub line: usize,
}

/// Every `// route: <kind>` declaration in `sorted`.
pub fn declared(sorted: &[&Source]) -> Vec<Declared> {
    let mut out = Vec::new();
    for s in sorted.iter().filter(|s| s.path.ends_with(".rs")) {
        let lines: Vec<&str> = s.text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(at) = line.find("// route: ") else {
                continue;
            };
            let (mut head, note) = line.split_at(at);
            if head.trim().is_empty() {
                // On a line of its own, it declares the route the line above names, as
                // rustfmt leaves a comment that follows an arm's `{`.
                let above = lines[..i].iter().rev().find(|l| !l.trim().is_empty());
                match above {
                    Some(l) if !l.trim_start().starts_with("//") => head = l,
                    _ => continue,
                }
            } else if head.trim_start().starts_with("//") {
                continue;
            }
            let mut words = note["// route: ".len()..].split_whitespace();
            let Some(kind) = words.next() else { continue };
            let pinned = words
                .find_map(|w| w.strip_prefix('@'))
                .unwrap_or_default()
                .to_string();
            let Some(path) = strings(head).into_iter().find(|p| p.starts_with('/')) else {
                continue;
            };
            let method = head
                .find("Method::")
                .map(|m| ident(&head[m + "Method::".len()..]).to_uppercase())
                .unwrap_or_else(|| "*".into());
            out.push(Declared {
                method,
                path,
                kind: kind.to_string(),
                pinned,
                file: s.path.to_string(),
                line: i + 1,
            });
        }
    }
    out
}

/// The capability module a file of the capabilities crate belongs to, such as `console` for
/// `src/capabilities/src/console/web/ops.rs`.
pub fn module_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("src/capabilities/src/")?;
    let first = rest.split('/').next()?;
    Some(first.trim_end_matches(".rs"))
}

/// Calls from one Worker to another's MCP tools, counted at `dispatch(...)` call sites whose
/// tool is a literal, and from the console's ops allow-list.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The calling Worker.
    pub from: String,
    /// The Worker hosting the tools; empty when no Worker hosts them.
    pub to: String,
    /// The tools called, sorted.
    pub tools: Vec<String>,
    /// How many call sites name them.
    pub sites: usize,
    /// The first call site, as `path:line`.
    pub at: String,
}

/// One literal tool named at a `dispatch(...)` call: the tool, its file and line.
pub struct Site {
    pub tool: String,
    pub file: String,
    pub line: usize,
}

/// Every `dispatch(...)` call in `sorted` whose tool argument is a string literal.
pub fn dispatch_sites(sorted: &[&Source]) -> Vec<Site> {
    let mut out = Vec::new();
    for s in sorted.iter().filter(|s| s.path.ends_with(".rs")) {
        let text = code(s.text);
        for at in super::found(&text, "dispatch(") {
            let call = balanced(&text[at..]);
            let inner = call.trim_start_matches('(').trim_end_matches(')');
            let Some(tool) = top_level_args(inner).into_iter().find_map(|a| {
                let a = a.trim();
                (a.starts_with('"') && a.ends_with('"') && a.len() > 2)
                    .then(|| a[1..a.len() - 1].to_string())
            }) else {
                continue;
            };
            if !tool
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                continue;
            }
            out.push(Site {
                tool,
                file: s.path.to_string(),
                line: text[..at].matches('\n').count() + 1,
            });
        }
    }
    out
}

/// `text` split at the commas outside brackets and strings.
fn top_level_args(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start, mut inside, mut escaped) = (0i32, 0usize, false, false);
    for (i, c) in text.char_indices() {
        if inside {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => inside = false,
                _ => escaped = false,
            }
            continue;
        }
        match c {
            '"' => inside = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// The calls of `sites` (each attributed to its caller by `caller`) grouped by caller and host.
/// `host` names the Worker hosting a tool, or none. Calls a Worker makes to itself are left out.
pub fn group(sites: &[(String, &Site)], host: impl Fn(&str) -> Option<String>) -> Vec<ToolCall> {
    let mut by: BTreeMap<(String, String), (Vec<String>, usize, String)> = BTreeMap::new();
    for (from, site) in sites {
        let to = host(&site.tool).unwrap_or_default();
        if *from == to {
            continue;
        }
        let entry = by
            .entry((from.clone(), to))
            .or_insert_with(|| (Vec::new(), 0, format!("{}:{}", site.file, site.line)));
        entry.0.push(site.tool.clone());
        entry.1 += 1;
    }
    by.into_iter()
        .map(|((from, to), (mut tools, sites, at))| {
            tools.sort();
            tools.dedup();
            ToolCall {
                from,
                to,
                tools,
                sites,
                at,
            }
        })
        .collect()
}

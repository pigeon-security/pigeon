//! The architecture drawing's layout (Direction A, the layered map), computed from the model
//! alone: every row, card, line and label, with its place. Pure: no I/O, no SVG. The renderer
//! ([`super::draw`]) only paints what this decides, and `architecture_layout` answers it as
//! JSON, so agents and tests can check the drawing without reading SVG.
//!
//! Rows, top to bottom, each only when it has something in it:
//! 1. HOSTNAMES, one column each, grouped by the Worker serving them.
//! 2. CLOUDFLARE ACCOUNT · zone, a dashed region holding the edge Workers (each under the
//!    hostnames it serves), the SERVICE BINDINGS bus from the hub Worker with the other Workers
//!    hanging off it, and the CLOUDFLARE SERVICES in the account.
//! 3. OUTSIDE THE ACCOUNT: third parties, each under the Worker whose capability calls it.
//! 4. CALLED BY <hub>: the hub's own third parties, fed down the left gutter.
//!
//! The hub is read from the model ([`hub`]): nothing names pigeon.dev's own Workers.

use std::collections::BTreeMap;

use serde::Serialize;

use super::scan::{ROUTE_KINDS, UNGUARDED};
use super::{Dependency, Hostname, Model, Worker};

/// How much a drawing shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Names, hostnames, bindings and their labels, the bus and service lines; no tool layer
    /// and no route counts. What pigeon.dev shows.
    Compact,
    /// Everything: counts, pills, flows, keys, details, route checks and the MCP tool layer.
    Full,
}

/// The canvas it is laid out for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Width {
    /// Rows side by side, at least 1000 units wide.
    Wide,
    /// A phone: every row stacked, 360 units wide.
    Narrow,
}

/// A drawing's layout: its size, rows, region, cards, lines and labels.
#[derive(Serialize, Clone, Debug)]
pub struct Layout {
    pub mode: Mode,
    pub width: Width,
    pub w: f64,
    pub h: f64,
    /// The Worker the others bind to; none when there are no Workers.
    pub hub: Option<String>,
    /// The short description a screen reader reads.
    pub title: String,
    /// The rows drawn, top to bottom.
    pub rows: Vec<Row>,
    /// The dashed account region, when there is anything in the account.
    pub region: Option<Rect>,
    pub cards: Vec<Card>,
    pub edges: Vec<Edge>,
    /// Text, pills and tags, each with its place.
    pub marks: Vec<Mark>,
}

/// A row of the drawing.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Row {
    /// `hostnames`, `edge`, `bindings`, `services`, `outside` or `called`.
    pub id: &'static str,
    pub label: String,
    pub y: f64,
    pub h: f64,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A hostname, Worker or service, as a box.
#[derive(Serialize, Clone, Debug)]
pub struct Card {
    /// `host:<name>`, `worker:<name>` or `service:<name>`.
    pub id: String,
    /// `hostname`, `worker` or `service`.
    pub kind: &'static str,
    /// The row it is in.
    pub row: &'static str,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// The hub Worker: the one accent.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hub: bool,
    /// A hostname: whether Cloudflare Access is in front of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<bool>,
    /// A service: where it runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
}

/// A line between cards.
#[derive(Serialize, Clone, Debug)]
pub struct Edge {
    /// `host` (hostname to Worker), `link` (edge Worker to edge Worker), `bus` (the binding
    /// bus), `binding`, `object` (a Durable Object used across Workers), `service`, `through`,
    /// `dispatch` (the tool bus), `tool` or `arc` (a tool call between two non-hub Workers).
    pub kind: &'static str,
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The SVG path.
    pub d: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dashed: bool,
    /// Arrowheads at its start and end.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub start: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub end: bool,
    /// `base`, or `tool` for the MCP tool layer.
    pub layer: &'static str,
}

/// A piece of text: plain, a pill, a tool tag, an eyebrow or a line's label.
#[derive(Serialize, Clone, Debug)]
pub struct Mark {
    /// `text`, `pill`, `tag` (a tool-layer pill), `label` (a line's label, on a background),
    /// `eyebrow`, `sep` (a rule inside a card, `w` long).
    pub kind: &'static str,
    pub x: f64,
    pub y: f64,
    /// The width of a pill, tag, label or rule.
    pub w: f64,
    pub text: String,
    /// Its style: `wn`, `hn`, `en`, `meta`, `pl`, `pl-open`, `pl-info`, `pl-acc`, `fo`, `fi`, `lb`,
    /// `dim`, `eb`, `eb-acc`.
    pub class: &'static str,
    /// `start`, `middle` or `end`.
    pub anchor: &'static str,
    pub layer: &'static str,
}

// ---- metrics ----

const WIDE: f64 = 1000.0;
const NARROW: f64 = 360.0;
/// The frame's padding and the gap between columns.
const P: f64 = 24.0;
const G: f64 = 16.0;
/// The narrowest column of hostnames or Workers before the canvas widens.
const MIN_COL: f64 = 176.0;
/// The left gutter the hub's own services are fed down.
const GUTTER: f64 = 15.0;

/// About how wide `s` is in a monospace face of `px`.
pub fn mono_w(s: &str, px: f64) -> f64 {
    s.chars().count() as f64 * px * 0.6
}

/// `s` cut with an ellipsis to fit `width` in a monospace face of `px`.
pub fn fit(s: &str, width: f64, px: f64) -> String {
    let max = (width / (px * 0.6)).floor().max(1.0) as usize;
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// `s` wrapped at words to lines `width` wide in a face `per` units a character.
fn wrap(s: &str, width: f64, per: f64) -> Vec<String> {
    let max = (width / per).floor().max(4.0) as usize;
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let next = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if next.chars().count() <= max || cur.is_empty() {
            cur = next;
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .map(|l| {
            if l.chars().count() > max {
                let kept: String = l.chars().take(max - 1).collect();
                format!("{kept}…")
            } else {
                l
            }
        })
        .collect()
}

fn pill_w(s: &str) -> f64 {
    mono_w(s, 10.5) + 12.0
}

fn tag_w(s: &str) -> f64 {
    mono_w(s, 10.0) + 10.0
}

fn label_w(s: &str) -> f64 {
    mono_w(s, 10.0) + 10.0
}

// ---- what the model says, read for the drawing ----

/// The Worker the others bind to: the one others' tools merge into, else the one with most
/// links, else the one serving most hostnames, else the first.
pub fn hub(m: &Model) -> Option<&Worker> {
    let score = |w: &Worker| {
        let merged = m
            .workers
            .iter()
            .filter(|x| x.merged_into.as_deref() == Some(w.name.as_str()))
            .count();
        let links = m
            .links
            .iter()
            .filter(|l| l.from == w.name || l.to == w.name)
            .count();
        let hosts = m.hostnames.iter().filter(|h| h.worker == w.name).count();
        (merged, links, hosts)
    };
    let best = m.workers.iter().map(score).max()?;
    m.workers.iter().find(|w| score(w) == best)
}

/// Whether Access stands in front of a hostname.
pub fn behind_access(h: &Hostname) -> bool {
    h.behind_access || !h.access.is_empty()
}

/// The binding names of the links from `from` to `to` of `kind`.
fn via<'a>(m: &'a Model, from: &str, to: &str, kind: &str) -> Vec<&'a str> {
    m.links
        .iter()
        .filter(|l| l.from == from && l.to == to && l.kind == kind)
        .map(|l| l.via.as_str())
        .collect()
}

/// How a Worker and the hub bind each other: `X ⇄ PLATFORM`, `X →` or `← PLATFORM`.
fn binding_label(m: &Model, hub: &str, name: &str) -> String {
    let down = via(m, hub, name, "service binding");
    let up = via(m, name, hub, "service binding");
    match (down.first(), up.first()) {
        (Some(d), Some(u)) => format!("{d} ⇄ {u}"),
        (Some(d), None) => format!("{d} →"),
        (None, Some(u)) => format!("← {u}"),
        (None, None) => String::new(),
    }
}

/// The Durable Object classes one of the two Workers uses on the other's.
fn shared_objects(m: &Model, hub: &str, name: &str) -> Vec<String> {
    let mut out: Vec<String> = via(m, hub, name, "Durable Object")
        .into_iter()
        .chain(via(m, name, hub, "Durable Object"))
        .map(|v| class_of(v).to_string())
        .collect();
    out.dedup();
    out
}

/// `CiRunner` from `CI_RUNNER (CiRunner)`.
fn class_of(via: &str) -> &str {
    via.split_once('(')
        .map(|(_, c)| c.trim_end_matches(')'))
        .unwrap_or(via)
}

/// A Worker's short name, such as `core` for the Worker whose lane is `core`.
fn short(w: &Worker) -> &str {
    w.lane.as_deref().unwrap_or(&w.name)
}

/// The Worker whose capability calls an external: a Worker running its `by`, other than the hub
/// when one is; the hub when none runs it.
pub fn caller_of<'a>(m: &'a Model, hub: Option<&'a str>, e: &Dependency) -> Option<&'a str> {
    let runs: Vec<&str> = m
        .workers
        .iter()
        .filter(|w| !e.by.is_empty() && w.uses.contains(&e.by))
        .map(|w| w.name.as_str())
        .collect();
    runs.iter()
        .find(|w| Some(**w) != hub)
        .or(runs.first())
        .copied()
        .or(hub)
}

/// The services drawn: the model's, without container classes (drawn as pills on Workers), and
/// each R2 bucket of an instance as a service in the account, called by the hub.
fn services(m: &Model) -> Vec<Dependency> {
    let mut out: Vec<Dependency> = m
        .externals
        .iter()
        .filter(|e| e.place != "container")
        .cloned()
        .collect();
    for b in &m.resources.buckets {
        out.push(Dependency {
            name: b.clone(),
            kind: "R2 bucket".into(),
            detail: String::new(),
            by: String::new(),
            place: "account".into(),
            out: "site files".into(),
            ..Dependency::default()
        });
    }
    out
}

/// A service's place, `outside` when undeclared.
fn place(e: &Dependency) -> &str {
    match e.place.as_str() {
        "account" => "account",
        _ => "outside",
    }
}

/// The MCP tools a Worker hosts, as one tag: `models_* · 12`, the tool itself when it is one,
/// or `90 tools: users_* 10, land_* 8, … +N more`.
pub fn hosted_label(tools: &[String]) -> Option<String> {
    let mut tools: Vec<&str> = tools.iter().map(String::as_str).collect();
    tools.sort_unstable();
    tools.dedup();
    match tools.len() {
        0 => return None,
        1 => return Some(tools[0].to_string()),
        _ => {}
    }
    if let Some(pre) = common_prefix(&tools) {
        return Some(format!("{pre}* · {}", tools.len()));
    }
    let mut groups: BTreeMap<&str, usize> = BTreeMap::new();
    for t in &tools {
        *groups.entry(t.split('_').next().unwrap_or(t)).or_default() += 1;
    }
    let mut top: Vec<(&str, usize)> = groups.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let shown: Vec<String> = top
        .iter()
        .take(3)
        .map(|(g, n)| format!("{g}_* {n}"))
        .collect();
    let more = top.len().saturating_sub(3);
    Some(if more == 0 {
        format!("{} tools: {}", tools.len(), shown.join(", "))
    } else {
        format!(
            "{} tools: {}, … +{more} more",
            tools.len(),
            shown.join(", ")
        )
    })
}

/// The prefix every name shares, up to and with its last `_`.
fn common_prefix(names: &[&str]) -> Option<String> {
    let first = names.first()?;
    let mut len = first.len();
    for n in names {
        len = len.min(
            first
                .bytes()
                .zip(n.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let pre = &first[..len];
    let cut = pre.rfind('_')?;
    Some(pre[..=cut].to_string())
}

/// A tool call's label: `models_* · 7`, the tool when it is one, or `N tools`.
fn call_label(tools: &[String]) -> String {
    let names: Vec<&str> = tools.iter().map(String::as_str).collect();
    match (names.len(), common_prefix(&names)) {
        (1, _) => names[0].to_string(),
        (n, Some(pre)) => format!("{pre}* · {n}"),
        (n, None) => format!("{n} tools"),
    }
}

/// Each hostname's routes counted by their check, in [`ROUTE_KINDS`] order: a route counts on
/// the hostnames its capability names, else on its Worker's (or, for a Worker with none, the
/// hub's) hostnames that no other routed capability holds alone.
pub fn route_counts(m: &Model, hub: Option<&str>) -> Vec<Vec<(String, usize)>> {
    let mut counts: Vec<BTreeMap<String, usize>> = vec![BTreeMap::new(); m.hostnames.len()];
    for (r, on) in m.route_kinds.iter().zip(route_hosts(m, hub)) {
        for i in on {
            *counts[i].entry(r.kind.clone()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|c| {
            let mut out: Vec<(String, usize)> = ROUTE_KINDS
                .iter()
                .filter_map(|k| c.get(*k).map(|n| ((*k).to_string(), *n)))
                .collect();
            out.extend(
                c.iter()
                    .filter(|(k, _)| !ROUTE_KINDS.contains(&k.as_str()))
                    .map(|(k, n)| (k.clone(), *n)),
            );
            out
        })
        .collect()
}

/// The hostnames (by index) each of the model's declared routes counts on.
fn route_hosts(m: &Model, hub: Option<&str>) -> Vec<Vec<usize>> {
    let routed: Vec<&str> = m
        .route_kinds
        .iter()
        .filter(|r| !r.by.is_empty())
        .map(|r| r.by.as_str())
        .collect();
    let by_of = |h: &Hostname| -> Vec<String> {
        h.by.split(',')
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .collect()
    };
    let held = |h: &Hostname| {
        let by = by_of(h);
        !by.is_empty() && by.iter().all(|b| routed.contains(&b.as_str()))
    };
    let mut all = Vec::new();
    for r in &m.route_kinds {
        let mut on: Vec<usize> = Vec::new();
        if !r.by.is_empty() {
            on = (0..m.hostnames.len())
                .filter(|i| by_of(&m.hostnames[*i]).contains(&r.by))
                .collect();
        }
        if on.is_empty() {
            for worker in [Some(r.worker.as_str()), hub].into_iter().flatten() {
                on = (0..m.hostnames.len())
                    .filter(|i| m.hostnames[*i].worker == worker && !held(&m.hostnames[*i]))
                    .collect();
                if !on.is_empty() {
                    break;
                }
            }
        }
        all.push(on);
    }
    all
}

/// The hostnames whose routes include `POST /mcp`: the MCP endpoint.
fn serves_mcp(m: &Model, hub: Option<&str>) -> Vec<bool> {
    let mut out = vec![false; m.hostnames.len()];
    for (r, on) in m.route_kinds.iter().zip(route_hosts(m, hub)) {
        if r.path == "/mcp" && r.method == "POST" {
            for i in on {
                out[i] = true;
            }
        }
    }
    out
}

/// The pills of a Worker card: classes it hosts, its containers and the containers it reaches
/// on another Worker.
fn worker_pills(m: &Model, w: &Worker, hub: bool) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let own = if hub { "pl-acc" } else { "pl" };
    for o in w.objects.iter().filter(|o| o.owner == w.name) {
        if !w.containers.contains(&o.class) {
            out.push((format!("◇ {}", o.class), own));
        }
    }
    for c in &w.containers {
        out.push((format!("⧉ {c}"), own));
    }
    for o in w.objects.iter().filter(|o| o.owner != w.name) {
        if let Some(owner) = m.workers.iter().find(|x| x.name == o.owner)
            && owner.containers.contains(&o.class)
        {
            let pill = format!("⧉ {} on {}", o.class, short(owner));
            if !out.iter().any(|(p, _)| *p == pill) {
                out.push((pill, "pl"));
            }
        }
    }
    out
}

/// Pills laid in rows `width` wide; a row that overflows ends in `+N`.
fn pill_rows(items: &[(String, &'static str)], width: f64) -> Vec<Vec<(String, &'static str)>> {
    let mut rows: Vec<Vec<(String, &'static str)>> = Vec::new();
    let mut cur: Vec<(String, &'static str)> = Vec::new();
    let mut at = 0.0;
    for (t, c) in items {
        let w = pill_w(t) + 6.0;
        if !cur.is_empty() && at + w > width {
            rows.push(std::mem::take(&mut cur));
            at = 0.0;
        }
        cur.push((t.clone(), *c));
        at += w;
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// The detail of a service, one item a line: `a, b, c` splits at commas, except that the bare
/// words after a labelled item (`hosted queues a, b`) stay with it.
fn detail_items(detail: &str) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    for part in detail.split(", ") {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match items.last_mut() {
            Some(last) if !part.contains(' ') && last.contains(' ') && !last.contains("://") => {
                last.push_str(", ");
                last.push_str(part);
            }
            _ => items.push(part.to_string()),
        }
    }
    items
}

// ---- the builder ----

struct Build<'a> {
    m: &'a Model,
    full: bool,
    w: f64,
    hub: Option<&'a str>,
    rows: Vec<Row>,
    cards: Vec<Card>,
    edges: Vec<Edge>,
    marks: Vec<Mark>,
    /// The tool layer is drawn: full mode, and something to show.
    tools: bool,
}

impl<'a> Build<'a> {
    fn text(&mut self, x: f64, y: f64, class: &'static str, text: &str, anchor: &'static str) {
        self.marks.push(Mark {
            kind: "text",
            x,
            y,
            w: 0.0,
            text: text.to_string(),
            class,
            anchor,
            layer: "base",
        });
    }

    fn eyebrow(&mut self, x: f64, y: f64, text: &str, class: &'static str) {
        self.marks.push(Mark {
            kind: "eyebrow",
            x,
            y,
            w: mono_w(text, 10.5) + 12.0,
            text: text.to_string(),
            class,
            anchor: "start",
            layer: "base",
        });
    }

    /// A pill at `x`; answers its width.
    fn pill(&mut self, x: f64, y: f64, text: &str, class: &'static str) -> f64 {
        let w = pill_w(text);
        self.marks.push(Mark {
            kind: "pill",
            x,
            y,
            w,
            text: text.to_string(),
            class,
            anchor: "start",
            layer: "base",
        });
        w
    }

    /// A tool tag whose baseline is `y`, placed by `anchor` at `x`.
    fn tag(&mut self, x: f64, y: f64, text: &str, anchor: &'static str) {
        let w = tag_w(text);
        let left = match anchor {
            "middle" => x - w / 2.0,
            "end" => x - w,
            _ => x,
        };
        self.marks.push(Mark {
            kind: "tag",
            x: left,
            y,
            w,
            text: text.to_string(),
            class: "tl",
            anchor: "start",
            layer: "tool",
        });
    }

    /// A line's label on a background, centred at `x`.
    fn label(&mut self, x: f64, y: f64, text: &str, class: &'static str) {
        self.marks.push(Mark {
            kind: "label",
            x,
            y,
            w: label_w(text),
            text: text.to_string(),
            class,
            anchor: "middle",
            layer: "base",
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn edge(
        &mut self,
        kind: &'static str,
        from: &str,
        to: &str,
        label: Option<String>,
        d: String,
        dashed: bool,
        start: bool,
        end: bool,
    ) {
        let layer = if matches!(kind, "dispatch" | "tool" | "arc") {
            "tool"
        } else {
            "base"
        };
        self.edges.push(Edge {
            kind,
            from: from.to_string(),
            to: to.to_string(),
            label,
            d,
            dashed,
            start,
            end,
            layer,
        });
    }

    fn card(&mut self, card: Card) -> Rect {
        let r = Rect {
            x: card.x,
            y: card.y,
            w: card.w,
            h: card.h,
        };
        self.cards.push(card);
        r
    }

    fn row(&mut self, id: &'static str, label: &str, y: f64, h: f64) {
        self.rows.push(Row {
            id,
            label: label.to_string(),
            y,
            h,
        });
    }

    fn worker(&self, name: &str) -> Option<&'a Worker> {
        self.m.workers.iter().find(|w| w.name == name)
    }

    /// The height a Worker card needs at `w` wide.
    fn worker_h(&self, wk: &Worker, w: f64) -> f64 {
        if !self.full {
            return 40.0;
        }
        let hub = self.hub == Some(wk.name.as_str());
        let rows = pill_rows(&worker_pills(self.m, wk, hub), w - 24.0).len() as f64;
        let tag = if self.tools && !hub && !wk.tools.is_empty() {
            22.0
        } else {
            0.0
        };
        52.0 + rows * 24.0 + tag
    }

    /// A Worker's card, `h` tall.
    fn worker_card(
        &mut self,
        wk: &Worker,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        row: &'static str,
    ) -> Rect {
        let hub = self.hub == Some(wk.name.as_str());
        let r = self.card(Card {
            id: format!("worker:{}", wk.name),
            kind: "worker",
            row,
            name: wk.name.clone(),
            x,
            y,
            w,
            h,
            hub,
            access: None,
            place: None,
        });
        let name_class = if hub { "wn-acc" } else { "wn" };
        if !self.full {
            self.text(
                x + 12.0,
                y + h / 2.0 + 4.5,
                name_class,
                &fit(&wk.name, w - 24.0, 12.5),
                "start",
            );
            return r;
        }
        self.text(
            x + 12.0,
            y + 22.0,
            name_class,
            &fit(&wk.name, w - 24.0, 12.5),
            "start",
        );
        let mut tools: Vec<&String> = wk.tools.iter().collect();
        tools.sort();
        tools.dedup();
        let mut meta = vec![format!(
            "{} tool{}",
            tools.len(),
            if tools.len() == 1 { "" } else { "s" }
        )];
        if !wk.handlers.is_empty() {
            meta.push(format!(
                "{} handler{}",
                wk.handlers.len(),
                if wk.handlers.len() == 1 { "" } else { "s" },
            ));
        }
        if hub && !wk.handlers.is_empty() {
            meta.push(wk.handlers.join(", "));
        }
        let meta = meta.join(" · ");
        self.text(
            x + 12.0,
            y + 40.0,
            "meta",
            &fit(&meta, w - 24.0, 11.0 * 0.93),
            "start",
        );
        let mut yy = y + 50.0;
        for prow in pill_rows(&worker_pills(self.m, wk, hub), w - 24.0) {
            let mut at = x + 12.0;
            for (t, c) in prow {
                at += self.pill(at, yy, &t, c) + 6.0;
            }
            yy += 24.0;
        }
        if self.tools
            && let Some(lab) = hosted_label(&wk.tools)
        {
            if hub {
                let room = (w - 24.0 - mono_w(&wk.name, 12.5) - 16.0).max(60.0);
                self.tag(x + w - 10.0, y + 22.0, &fit(&lab, room - 10.0, 10.0), "end");
            } else {
                self.tag(x + w - 10.0, y + h - 9.0, &fit(&lab, w - 30.0, 10.0), "end");
            }
        }
        r
    }

    /// The height a service card needs at `w` wide.
    fn service_h(&self, e: &Dependency, w: f64, key: bool) -> f64 {
        if !self.full {
            return 34.0;
        }
        let (flows, keys, detail) = self.service_lines(e, w, key);
        let mut h = 46.0;
        if !flows.is_empty() || !keys.is_empty() || !detail.is_empty() {
            h += 8.0;
        }
        h += 14.0 * flows.len() as f64 + 13.0 * (keys.len() + detail.len()) as f64;
        if !flows.is_empty() && (!keys.is_empty() || !detail.is_empty()) {
            h += 6.0;
        }
        h
    }

    /// A service's flow lines (arrow, text, class), key lines and detail lines.
    #[allow(clippy::type_complexity)]
    fn service_lines(
        &self,
        e: &Dependency,
        w: f64,
        key: bool,
    ) -> (
        Vec<(&'static str, String, &'static str)>,
        Vec<String>,
        Vec<String>,
    ) {
        let inner = w - 24.0;
        let mut flows = Vec::new();
        for (arrow, text, class) in [("→", &e.out, "fo"), ("←", &e.inbound, "fi")] {
            if text.is_empty() {
                continue;
            }
            for (i, l) in wrap(text, inner - 14.0, 10.5 * 0.56)
                .into_iter()
                .enumerate()
            {
                flows.push((if i == 0 { arrow } else { " " }, l, class));
            }
        }
        let keys = if key {
            let k = keys_of(self.m, &e.by);
            if k.is_empty() {
                Vec::new()
            } else {
                wrap(&format!("key {}", k.join(", ")), inner, 6.0)
            }
        } else {
            Vec::new()
        };
        let mut detail = Vec::new();
        for item in detail_items(&e.detail) {
            for (i, l) in wrap(&item, inner - 12.0, 6.0).into_iter().enumerate() {
                detail.push(if i == 0 { l } else { format!("  {l}") });
            }
        }
        (flows, keys, detail)
    }

    /// A service's card at (`x`, `y`), `w` wide; answers its height.
    fn service_card(
        &mut self,
        e: &Dependency,
        x: f64,
        y: f64,
        w: f64,
        row: &'static str,
        key: bool,
    ) -> f64 {
        let h = self.service_h(e, w, key);
        self.card(Card {
            id: format!("service:{}", e.name),
            kind: "service",
            row,
            name: e.name.clone(),
            x,
            y,
            w,
            h,
            hub: false,
            access: None,
            place: Some(place(e).to_string()),
        });
        if !self.full {
            self.text(
                x + 12.0,
                y + 21.5,
                "en",
                &fit(&e.name, w - 24.0, 12.0 * 0.95),
                "start",
            );
            return h;
        }
        self.text(
            x + 12.0,
            y + 19.0,
            "en",
            &fit(&e.name, w - 24.0, 12.0 * 0.95),
            "start",
        );
        let kind = fit(&e.kind, w * 0.55, 10.5);
        let pw = self.pill(x + 12.0, y + 26.0, &kind, "pl");
        if !e.by.is_empty() {
            let via = format!("via {}", e.by);
            self.text(
                x + 18.0 + pw,
                y + 39.0,
                "meta",
                &fit(&via, w - pw - 30.0, 11.0 * 0.93),
                "start",
            );
        }
        let (flows, keys, detail) = self.service_lines(e, w, key);
        let mut yy = y + 48.0;
        for (arrow, l, class) in &flows {
            yy += 14.0;
            self.text(x + 12.0, yy - 2.0, class, arrow, "start");
            self.text(x + 26.0, yy - 2.0, class, l, "start");
        }
        if !flows.is_empty() && (!keys.is_empty() || !detail.is_empty()) {
            self.marks.push(Mark {
                kind: "sep",
                x: x + 12.0,
                y: yy + 5.0,
                w: w - 24.0,
                text: String::new(),
                class: "sep",
                anchor: "start",
                layer: "base",
            });
            yy += 6.0;
        }
        for l in &keys {
            yy += 13.0;
            self.text(x + 12.0, yy, "dim", l, "start");
        }
        for l in &detail {
            yy += 13.0;
            self.text(x + 12.0, yy, "lb", l, "start");
        }
        h
    }

    /// A fork: a spine down from `top` at `x`, with a tick into each card at the heights `ys`;
    /// a service that also sends to the platform gets a tick back.
    fn fork(&mut self, from: &str, x: f64, top: f64, ticks: &[(f64, String, bool)]) {
        let Some(bottom) = ticks.iter().map(|t| t.0).reduce(f64::max) else {
            return;
        };
        self.edge(
            "service",
            from,
            from,
            None,
            format!("M{x:.1},{top:.1} V{bottom:.1}"),
            false,
            false,
            false,
        );
        for (y, to, both) in ticks {
            self.edge(
                "service",
                from,
                to,
                None,
                format!("M{x:.1},{y:.1} H{:.1}", x + 13.0),
                false,
                false,
                true,
            );
            if *both {
                self.edge(
                    "service",
                    to,
                    from,
                    None,
                    format!("M{:.1},{:.1} H{:.1}", x + 13.0, y + 6.0, x + 1.0),
                    false,
                    false,
                    true,
                );
            }
        }
    }
}

/// Keys a capability uses: its bindings that are Secrets Store secrets, other than caller keys.
fn keys_of(m: &Model, by: &str) -> Vec<String> {
    m.parts
        .iter()
        .find(|p| p.name == by)
        .map(|p| {
            p.bindings
                .iter()
                .filter(|b| m.resources.secrets.contains(b) && !b.starts_with("CALLER_KEY"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The layout of `m`'s drawing in `mode`, `width` wide.
pub fn layout(m: &Model, mode: Mode, width: Width) -> Layout {
    let hub = hub(m).map(|w| w.name.as_str());
    let full = mode == Mode::Full;
    let tools = full
        && (m.workers.iter().any(|w| !w.tools.is_empty())
            || m.tool_calls.iter().any(|c| !c.to.is_empty()));
    let mut b = Build {
        m,
        full,
        w: if width == Width::Wide { WIDE } else { NARROW },
        hub,
        rows: Vec::new(),
        cards: Vec::new(),
        edges: Vec::new(),
        marks: Vec::new(),
        tools,
    };
    let (region, h) = match width {
        Width::Wide => wide(&mut b),
        Width::Narrow => narrow(&mut b),
    };
    let services = services(m);
    let outside = services.iter().filter(|e| place(e) == "outside").count();
    let who = m
        .instance
        .as_ref()
        .map(|i| i.title.clone())
        .unwrap_or_else(|| "pigeon".into());
    let title = format!(
        "{who} architecture: {} hostname{}, {} Worker{}, {} service{} in the account, {} outside",
        m.hostnames.len(),
        if m.hostnames.len() == 1 { "" } else { "s" },
        m.workers.len(),
        if m.workers.len() == 1 { "" } else { "s" },
        services.len() - outside,
        if services.len() - outside == 1 {
            ""
        } else {
            "s"
        },
        outside,
    );
    Layout {
        mode,
        width,
        w: b.w,
        h,
        hub: hub.map(String::from),
        title,
        rows: b.rows,
        region,
        cards: b.cards,
        edges: b.edges,
        marks: b.marks,
    }
}

/// Hostnames in the order drawn: grouped by the Worker serving them, in the order each Worker
/// first appears, so a Worker spans adjacent columns.
fn host_order(m: &Model) -> Vec<usize> {
    let mut order: Vec<&str> = Vec::new();
    for h in &m.hostnames {
        if !order.contains(&h.worker.as_str()) {
            order.push(&h.worker);
        }
    }
    order
        .iter()
        .flat_map(|w| (0..m.hostnames.len()).filter(move |i| m.hostnames[*i].worker == *w))
        .collect()
}

/// The status pill of a hostname.
fn host_status(h: &Hostname) -> (&'static str, &'static str) {
    if behind_access(h) {
        ("Access", "pl-info")
    } else {
        ("no Access", "pl")
    }
}

fn chip_class(kind: &str) -> &'static str {
    if UNGUARDED.contains(&kind) {
        "pl-open"
    } else {
        "pl"
    }
}

/// The wide layout; answers the region and the height.
fn wide(b: &mut Build) -> (Option<Rect>, f64) {
    let m = b.m;
    let hub = b.hub;
    let order = host_order(m);
    let counts = route_counts(m, hub);
    let mcp = serves_mcp(m, hub);
    let n = order.len();
    let edge_names: Vec<&str> = {
        let mut v: Vec<&str> = Vec::new();
        for i in &order {
            let w = m.hostnames[*i].worker.as_str();
            if !v.contains(&w) && m.workers.iter().any(|x| x.name == w) {
                v.push(w);
            }
        }
        v
    };
    let inner: Vec<&Worker> = m
        .workers
        .iter()
        .filter(|w| !edge_names.contains(&w.name.as_str()) && Some(w.name.as_str()) != hub)
        .collect();
    let k = inner.len();
    // The gap before each column of hostnames: narrow within one Worker's, wide enough for a
    // binding's label between two Workers that bind each other.
    let gaps: Vec<f64> = (0..n)
        .map(|c| {
            if c == 0 {
                return 0.0;
            }
            let (a, z) = (
                &m.hostnames[order[c - 1]].worker,
                &m.hostnames[order[c]].worker,
            );
            if a == z {
                return G;
            }
            let label = match hub {
                Some(h) if h == a => binding_label(m, h, z),
                Some(h) if h == z => binding_label(m, h, a),
                _ => String::new(),
            };
            if label.is_empty() {
                32.0
            } else {
                (label_w(&label) + 28.0).max(48.0)
            }
        })
        .collect();
    let gap_sum: f64 = gaps.iter().sum();
    // Widen the canvas so no column is narrower than MIN_COL.
    let need = |cols: usize, gaps: f64| 2.0 * P + cols as f64 * MIN_COL + gaps;
    b.w =
        b.w.max(need(n, gap_sum))
            .max(need(k, k.saturating_sub(1) as f64 * 12.0));
    let w_all = b.w;
    let mut y = 28.0;

    // 1. Hostnames.
    let hw = if n == 0 {
        0.0
    } else {
        (w_all - 2.0 * P - gap_sum) / n as f64
    };
    let hx = |c: usize| P + c as f64 * hw + gaps[..=c].iter().sum::<f64>();
    let chip_rows: Vec<Vec<Vec<(String, &'static str)>>> = order
        .iter()
        .map(|i| {
            if !b.full {
                return Vec::new();
            }
            let chips: Vec<(String, &'static str)> = counts[*i]
                .iter()
                .map(|(k, c)| (format!("{k} {c}"), chip_class(k)))
                .collect();
            pill_rows(&chips, hw - 24.0)
        })
        .collect();
    let mut host_bottom = y;
    if n > 0 {
        b.eyebrow(P, y, "HOSTNAMES", "eb");
        y += 12.0;
        let most = chip_rows.iter().map(Vec::len).max().unwrap_or(0);
        let host_h = if !b.full {
            36.0
        } else if most == 0 {
            48.0
        } else {
            56.0 + 22.0 * most as f64
        };
        b.row("hostnames", "HOSTNAMES", y, host_h);
        for (c, i) in order.iter().enumerate() {
            let h = &m.hostnames[*i];
            let x = hx(c);
            b.card(Card {
                id: format!("host:{}", h.host),
                kind: "hostname",
                row: "hostnames",
                name: h.host.clone(),
                x,
                y,
                w: hw,
                h: host_h,
                hub: false,
                access: Some(behind_access(h)),
                place: None,
            });
            if !b.full {
                b.text(
                    x + 12.0,
                    y + 22.5,
                    "hn",
                    &fit(&h.host, hw - 24.0, 13.0),
                    "start",
                );
                continue;
            }
            let tag = if b.tools && mcp[*i] {
                Some("/mcp · every tool")
            } else {
                None
            };
            b.text(
                x + 12.0,
                y + 21.0,
                "hn",
                &fit(&h.host, hw - 24.0, 13.0),
                "start",
            );
            let (st, cls) = host_status(h);
            let pw = b.pill(x + 12.0, y + 28.0, st, cls);
            let room = hw - pw - 32.0 - tag.map_or(0.0, |t| tag_w(t) + 8.0);
            if let Some(t) = tag {
                b.tag(x + hw - 10.0, y + 41.0, t, "end");
            }
            if !chip_rows[c].is_empty() && room > 40.0 {
                b.text(
                    x + 20.0 + pw,
                    y + 41.0,
                    "meta",
                    &fit("routes by check:", room, 11.0 * 0.93),
                    "start",
                );
            }
            {
                for (r, prow) in chip_rows[c].iter().enumerate() {
                    let mut at = x + 12.0;
                    for (t, cl) in prow {
                        at += b.pill(at, y + 52.0 + r as f64 * 22.0, t, cl) + 6.0;
                    }
                }
            }
        }
        host_bottom = y + host_h;
        y = host_bottom;
    }

    if m.workers.is_empty() {
        return (None, y + 24.0);
    }

    // 2. The account region.
    let region_top = if n > 0 { host_bottom + 40.0 } else { y + 12.0 };
    y = region_top + 26.0;
    let mut pos: BTreeMap<String, Rect> = BTreeMap::new();

    // The edge tier: Workers serving hostnames, under their columns; the hub on its own when it
    // serves none.
    let mut tier_h = 0.0f64;
    let mut spans: Vec<(&str, f64, f64)> = Vec::new();
    for name in &edge_names {
        let cols: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, i)| m.hostnames[**i].worker == *name)
            .map(|(c, _)| c)
            .collect();
        let (Some(first), Some(last)) = (cols.first(), cols.last()) else {
            continue;
        };
        let x0 = hx(*first);
        let x1 = hx(*last) + hw;
        spans.push((name, x0, x1 - x0));
        if let Some(wk) = b.worker(name) {
            tier_h = tier_h.max(b.worker_h(wk, x1 - x0));
        }
    }
    let hub_alone = hub.filter(|h| !edge_names.contains(h));
    if let Some(name) = hub_alone {
        let w = (w_all - 2.0 * P).min(520.0_f64.max((w_all - 2.0 * P) / 2.0));
        spans.insert(0, (name, P, w));
        if let Some(wk) = b.worker(name) {
            tier_h = tier_h.max(b.worker_h(wk, w));
        }
    }
    if !spans.is_empty() {
        b.row("edge", "", y, tier_h);
    }
    for (name, x, w) in &spans {
        let Some(wk) = b.worker(name) else { continue };
        let r = b.worker_card(wk, *x, y, *w, tier_h, "edge");
        pos.insert((*name).to_string(), r);
    }
    // Hostnames into the Worker serving them.
    for (c, i) in order.iter().enumerate() {
        let h = &m.hostnames[*i];
        if let Some(r) = pos.get(&h.worker) {
            let cx = hx(c) + hw / 2.0;
            b.edge(
                "host",
                &format!("host:{}", h.host),
                &format!("worker:{}", h.worker),
                None,
                format!("M{cx:.1},{host_bottom:.1} V{:.1}", r.y - 1.0),
                false,
                false,
                true,
            );
        }
    }
    // Edge Workers bound to the hub: a short labelled arrow between the cards.
    if let Some(hn) = hub
        && let Some(a) = pos.get(hn).copied()
    {
        for name in edge_names.iter().filter(|n| Some(**n) != hub) {
            let Some(o) = pos.get(*name).copied() else {
                continue;
            };
            let label = binding_label(m, hn, name);
            if label.is_empty() {
                continue;
            }
            let ly = a.y + (tier_h / 2.0).min(30.0);
            let (x0, x1) = if o.x > a.x {
                (a.x + a.w, o.x)
            } else {
                (o.x + o.w, a.x)
            };
            let down = !via(m, hn, name, "service binding").is_empty();
            let up = !via(m, name, hn, "service binding").is_empty();
            // Drawn from the bound Worker's side towards the one that binds it.
            let towards_hub = o.x > a.x;
            let (start, end) = match (down, up) {
                (true, true) => (true, true),
                (false, true) => (towards_hub, !towards_hub),
                _ => (!towards_hub, towards_hub),
            };
            b.edge(
                "link",
                &format!("worker:{name}"),
                &format!("worker:{hn}"),
                Some(label.clone()),
                format!("M{:.1},{ly:.1} H{:.1}", x0 + 1.0, x1 - 1.0),
                false,
                start,
                end,
            );
            b.label(
                (x0 + x1) / 2.0,
                ly - 6.0,
                &fit(&label, (x1 - x0 - 6.0).max(40.0), 10.0),
                "lb",
            );
        }
    }
    let tier_bottom = y + tier_h;
    y = tier_bottom;

    // The SERVICE BINDINGS bus and the Workers hanging off it.
    let iw = if k == 0 {
        0.0
    } else {
        (w_all - 2.0 * P - (k - 1) as f64 * 12.0) / k as f64
    };
    let ix = |i: usize| P + i as f64 * (iw + 12.0);
    let hub_rect = hub.and_then(|h| pos.get(h).copied());
    let calls: Vec<&super::ToolCall> = if b.tools {
        m.tool_calls.iter().filter(|c| !c.to.is_empty()).collect()
    } else {
        Vec::new()
    };
    let inner_names: Vec<&str> = inner.iter().map(|w| w.name.as_str()).collect();
    let hub_calls: Vec<&&super::ToolCall> = calls
        .iter()
        .filter(|c| {
            hub.is_some_and(|h| {
                (c.from == h && inner_names.contains(&c.to.as_str()))
                    || (c.to == h && inner_names.contains(&c.from.as_str()))
            })
        })
        .collect();
    let peer: Vec<&&super::ToolCall> = calls
        .iter()
        .filter(|c| inner_names.contains(&c.from.as_str()) && inner_names.contains(&c.to.as_str()))
        .collect();
    if k > 0 {
        let y_bus = tier_bottom + if hub_calls.is_empty() { 30.0 } else { 40.0 };
        let yi = y_bus + if hub_calls.is_empty() { 54.0 } else { 92.0 };
        let card_h = inner.iter().map(|w| b.worker_h(w, iw)).fold(0.0, f64::max);
        let bound: Vec<(usize, &Worker, String, Vec<String>)> = inner
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let hn = hub?;
                let label = binding_label(m, hn, &w.name);
                let objs = shared_objects(m, hn, &w.name);
                (!label.is_empty() || !objs.is_empty()).then_some((i, *w, label, objs))
            })
            .collect();
        if let (Some(hr), false) = (hub_rect, bound.is_empty()) {
            let hn = hub.unwrap_or_default();
            b.row("bindings", "SERVICE BINDINGS", y_bus, yi - y_bus);
            let cx = hr.x + hr.w / 2.0;
            let xs: Vec<f64> = bound.iter().map(|(i, ..)| ix(*i) + iw / 2.0).collect();
            let lo = xs.iter().copied().fold(cx, f64::min);
            let hi = xs.iter().copied().fold(cx, f64::max);
            b.edge(
                "bus",
                &format!("worker:{hn}"),
                &format!("worker:{hn}"),
                None,
                format!(
                    "M{cx:.1},{:.1} V{y_bus:.1} M{lo:.1},{y_bus:.1} H{hi:.1}",
                    hr.y + hr.h
                ),
                false,
                false,
                false,
            );
            for (i, w, label, objs) in &bound {
                let cx = ix(*i) + iw / 2.0;
                let to = format!("worker:{}", w.name);
                let from = format!("worker:{hn}");
                if objs.is_empty() {
                    b.edge(
                        "binding",
                        &from,
                        &to,
                        Some(label.clone()),
                        format!("M{cx:.1},{y_bus:.1} V{:.1}", yi - 1.0),
                        false,
                        false,
                        true,
                    );
                } else {
                    if !label.is_empty() {
                        b.edge(
                            "binding",
                            &from,
                            &to,
                            Some(label.clone()),
                            format!("M{:.1},{y_bus:.1} V{:.1}", cx - 4.0, yi - 1.0),
                            false,
                            false,
                            true,
                        );
                    }
                    let t = format!("◇ {}", objs.join(", "));
                    b.edge(
                        "object",
                        &from,
                        &to,
                        Some(t),
                        format!("M{:.1},{:.1} V{:.1}", cx + 4.0, y_bus + 4.0, yi - 1.0),
                        true,
                        false,
                        true,
                    );
                }
                if !label.is_empty() {
                    b.label(cx, y_bus + 21.0, &fit(label, iw - 8.0, 10.0), "lb");
                }
                if !objs.is_empty() {
                    let t = fit(&format!("◇ {}", objs.join(", ")), iw - 8.0, 10.0);
                    b.label(cx, y_bus + 39.0, &t, "dim");
                }
            }
            b.eyebrow(P, y_bus - 8.0, "SERVICE BINDINGS", "eb");
        }
        b.row("inner", "", yi, card_h);
        for (i, w) in inner.iter().enumerate() {
            let r = b.worker_card(w, ix(i), yi, iw, card_h, "inner");
            pos.insert(w.name.clone(), r);
        }
        y = yi + card_h;

        // The tool layer: calls to and from the hub through a dispatch bus under it, calls
        // between two other Workers as arcs beneath their cards.
        if let (Some(hr), false) = (hub_rect, hub_calls.is_empty()) {
            let hn = hub.unwrap_or_default();
            let yt = hr.y + hr.h + 14.0;
            let mut xs: Vec<f64> = Vec::new();
            for c in &hub_calls {
                let down = c.from == hn;
                let other = if down { &c.to } else { &c.from };
                let Some(r) = pos.get(other.as_str()).copied() else {
                    continue;
                };
                let lx = if down { r.x + r.w - 16.0 } else { r.x + 16.0 };
                xs.push(lx);
                let d = if down {
                    format!("M{lx:.1},{yt:.1} V{:.1}", r.y - 2.0)
                } else {
                    format!("M{lx:.1},{:.1} V{:.1}", r.y - 2.0, yt + 1.0)
                };
                let label = call_label(&c.tools);
                b.edge(
                    "tool",
                    &format!("worker:{}", c.from),
                    &format!("worker:{}", c.to),
                    Some(label.clone()),
                    d,
                    false,
                    false,
                    true,
                );
                let ly = if down { r.y - 11.0 } else { r.y - 30.0 };
                let text = format!("{} {}", if down { "↓" } else { "↑" }, label);
                let text = fit(&text, r.w / 2.0 + 30.0, 10.0);
                if down {
                    b.tag(lx - 5.0, ly, &text, "end");
                } else {
                    b.tag(lx + 5.0, ly, &text, "start");
                }
            }
            if !xs.is_empty() {
                let hx0 = hr.x + hr.w / 2.0 + 40.0;
                let lo = xs.iter().copied().fold(hx0, f64::min);
                let hi = xs.iter().copied().fold(hx0, f64::max);
                b.edge(
                    "dispatch",
                    &format!("worker:{hn}"),
                    &format!("worker:{hn}"),
                    Some("dispatch".into()),
                    format!(
                        "M{lo:.1},{yt:.1} H{hi:.1} M{hx0:.1},{:.1} V{yt:.1}",
                        hr.y + hr.h
                    ),
                    false,
                    false,
                    false,
                );
                b.marks.push(Mark {
                    kind: "text",
                    x: hx0 + 6.0,
                    y: yt - 3.0,
                    w: 0.0,
                    text: "dispatch".into(),
                    class: "tl-t",
                    anchor: "start",
                    layer: "tool",
                });
            }
        }
        if !peer.is_empty() {
            let base = y;
            let mut sorted = peer.clone();
            sorted.sort_by(|a, c| {
                let d = |x: &super::ToolCall| {
                    let (f, t) = (pos[x.from.as_str()], pos[x.to.as_str()]);
                    (f.x - t.x).abs() as i64
                };
                d(a).cmp(&d(c))
            });
            for (i, c) in sorted.iter().enumerate() {
                let (f, t) = (pos[c.from.as_str()], pos[c.to.as_str()]);
                let ax = f.x + f.w * 0.3;
                let bx = t.x + t.w * 0.7;
                let dy = 30.0 + 22.0 * i as f64;
                let label = call_label(&c.tools);
                b.edge(
                    "arc",
                    &format!("worker:{}", c.from),
                    &format!("worker:{}", c.to),
                    Some(label.clone()),
                    format!(
                        "M{ax:.1},{:.1} C{ax:.1},{:.1} {bx:.1},{:.1} {bx:.1},{:.1}",
                        base + 2.0,
                        base + dy + 6.0,
                        base + dy + 6.0,
                        base + 3.0
                    ),
                    false,
                    false,
                    true,
                );
                b.tag((ax + bx) / 2.0, base + dy + 3.0, &label, "middle");
            }
            y = base + 26.0 + 22.0 * sorted.len() as f64;
        }
    }

    // Services: each under the Worker whose capability calls it.
    let all = services(m);
    let named: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
    let hangs = |e: &Dependency| !e.through.is_empty() && named.contains(&e.through.as_str());
    let mut groups: BTreeMap<&str, Vec<&Dependency>> = BTreeMap::new();
    for e in all.iter().filter(|e| !hangs(e)) {
        let caller = if e.by.is_empty() {
            hub
        } else {
            caller_of(m, hub, e)
        };
        if let Some(c) = caller {
            groups.entry(c).or_default().push(e);
        }
    }
    let column = |name: &str| inner_names.iter().position(|n| *n == name);
    // The Workers whose services are fed down the gutter: the hub and the other edge Workers.
    let fed: Vec<&str> = groups
        .keys()
        .copied()
        .filter(|n| column(n).is_none())
        .collect();
    let mut svc_pos: BTreeMap<String, Rect> = BTreeMap::new();
    let mut keyed: Vec<String> = Vec::new();
    let mut key_once = |by: &str| {
        if keyed.iter().any(|k| k == by) {
            false
        } else {
            keyed.push(by.to_string());
            true
        }
    };

    // CLOUDFLARE SERVICES, inside the region.
    let account_of = |name: &str| -> Vec<&Dependency> {
        groups
            .get(name)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|e| place(e) == "account")
                    .collect()
            })
            .unwrap_or_default()
    };
    let any_account = groups.keys().any(|n| !account_of(n).is_empty());
    let mut region_bottom = y + 22.0;
    if any_account {
        let yc = y + 34.0;
        b.eyebrow(P, yc - 8.0, "CLOUDFLARE SERVICES", "eb");
        let mut deepest = yc;
        // Each inner Worker's own, under it.
        for (i, name) in inner_names.iter().enumerate() {
            let es = account_of(name);
            if es.is_empty() {
                continue;
            }
            let x = ix(i);
            let mut yy = yc;
            let mut ticks = Vec::new();
            for e in es {
                let key = key_once(&e.by);
                let h = b.service_card(e, x + 14.0, yy, iw - 14.0, "services", key);
                ticks.push((
                    yy + 20.0,
                    format!("service:{}", e.name),
                    !e.inbound.is_empty(),
                ));
                svc_pos.insert(
                    e.name.clone(),
                    Rect {
                        x: x + 14.0,
                        y: yy,
                        w: iw - 14.0,
                        h,
                    },
                );
                yy += h + 10.0;
            }
            let top = pos.get(*name).map_or(yc, |r| r.y + r.h);
            b.fork(&format!("worker:{name}"), x + 4.0, top, &ticks);
            deepest = deepest.max(yy - 10.0);
        }
        // The hub's and the other edge Workers', in the free columns, fed down the gutter.
        let free: Vec<usize> = (0..k)
            .filter(|i| account_of(inner_names[*i]).is_empty())
            .collect();
        let fed_account: Vec<(&str, &Dependency)> = fed
            .iter()
            .flat_map(|n| account_of(n).into_iter().map(move |e| (*n, e)))
            .collect();
        if !fed_account.is_empty() {
            let cols = if k == 0 {
                fed_account.len().clamp(3, 4)
            } else {
                k
            };
            let cw = (w_all - 2.0 * P - (cols - 1) as f64 * 12.0) / cols as f64;
            let cx = |i: usize| P + i as f64 * (cw + 12.0);
            let mut slots: Vec<(f64, f64)> = if k == 0 {
                Vec::new()
            } else {
                free.iter().map(|i| (ix(*i), yc)).collect()
            };
            let mut line_y = if slots.is_empty() { yc } else { deepest + 14.0 };
            let mut ticks: BTreeMap<&str, Vec<(f64, String, bool)>> = BTreeMap::new();
            let mut row_h = 0.0f64;
            for (j, (owner, e)) in fed_account.iter().enumerate() {
                if slots.is_empty() {
                    if j > 0 {
                        line_y += row_h + 12.0;
                        row_h = 0.0;
                    }
                    slots = (0..cols).map(|c| (cx(c), line_y)).collect();
                }
                let (x, sy) = slots.remove(0);
                let w = if k == 0 { cw } else { iw };
                let key = key_once(&e.by);
                let h = b.service_card(e, x + 14.0, sy, w - 14.0, "services", key);
                row_h = row_h.max(h);
                svc_pos.insert(
                    e.name.clone(),
                    Rect {
                        x: x + 14.0,
                        y: sy,
                        w: w - 14.0,
                        h,
                    },
                );
                ticks.entry(owner).or_default().push((
                    sy + 20.0,
                    format!("service:{}", e.name),
                    !e.inbound.is_empty(),
                ));
                deepest = deepest.max(sy + h);
                // The feed: along the gutter, then right into the card.
                b.edge(
                    "service",
                    &format!("worker:{owner}"),
                    &format!("service:{}", e.name),
                    None,
                    format!("M{GUTTER:.1},{:.1} H{:.1}", sy + 20.0, x + 13.0),
                    false,
                    false,
                    true,
                );
            }
            for (owner, t) in ticks {
                let Some(r) = pos.get(owner).copied() else {
                    continue;
                };
                let bottom = t.iter().map(|x| x.0).fold(0.0, f64::max);
                b.edge(
                    "service",
                    &format!("worker:{owner}"),
                    &format!("worker:{owner}"),
                    None,
                    format!(
                        "M{:.1},{:.1} H{GUTTER:.1} V{bottom:.1}",
                        r.x,
                        r.y + r.h - 14.0
                    ),
                    false,
                    false,
                    false,
                );
            }
        }
        b.row("services", "CLOUDFLARE SERVICES", yc, deepest - yc);
        region_bottom = deepest + 22.0;
    }
    let region = Rect {
        x: 8.0,
        y: region_top,
        w: w_all - 16.0,
        h: region_bottom - region_top,
    };
    let zone = m.resources.zone.clone().unwrap_or_default();
    let lab = if zone.is_empty() {
        "CLOUDFLARE ACCOUNT".to_string()
    } else {
        format!("CLOUDFLARE ACCOUNT · {}", zone.to_uppercase())
    };
    b.eyebrow(P, region_top + 4.0, &lab, "eb-acc");

    // 3. OUTSIDE THE ACCOUNT: each inner Worker's third parties under it.
    let outside_of = |name: &str| -> Vec<&Dependency> {
        groups
            .get(name)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|e| place(e) == "outside")
                    .collect()
            })
            .unwrap_or_default()
    };
    let hanging: Vec<&Dependency> = all.iter().filter(|e| hangs(e)).collect();
    let y0 = region_bottom + 56.0;
    let mut deepest = region_bottom;
    let mut outside_drawn = false;
    for (i, name) in inner_names.iter().enumerate() {
        let es = outside_of(name);
        if es.is_empty() {
            continue;
        }
        outside_drawn = true;
        let x = ix(i);
        let mut yy = y0;
        let mut ticks = Vec::new();
        for e in es {
            let key = key_once(&e.by);
            let h = b.service_card(e, x + 14.0, yy, iw - 14.0, "outside", key);
            ticks.push((
                yy + 20.0,
                format!("service:{}", e.name),
                !e.inbound.is_empty(),
            ));
            svc_pos.insert(
                e.name.clone(),
                Rect {
                    x: x + 14.0,
                    y: yy,
                    w: iw - 14.0,
                    h,
                },
            );
            yy += h + 12.0;
        }
        let top = pos.get(*name).map_or(y0, |r| r.y + r.h);
        b.fork(&format!("worker:{name}"), x + 4.0, top, &ticks);
        deepest = deepest.max(yy - 12.0);
    }
    // A service reached through another: under it, from it.
    let mut through_rows: BTreeMap<String, f64> = BTreeMap::new();
    for e in &hanging {
        let Some(t) = svc_pos.get(&e.through).copied() else {
            continue;
        };
        outside_drawn = true;
        let top = through_rows
            .get(&e.through)
            .copied()
            .unwrap_or_else(|| (t.y + t.h + 40.0).max(y0 + 8.0));
        let key = key_once(&e.by);
        let h = b.service_card(e, t.x, top, t.w, "outside", key);
        through_rows.insert(e.through.clone(), top + h + 12.0);
        svc_pos.insert(
            e.name.clone(),
            Rect {
                x: t.x,
                y: top,
                w: t.w,
                h,
            },
        );
        let cx = t.x + t.w / 2.0;
        b.edge(
            "through",
            &format!("service:{}", e.through),
            &format!("service:{}", e.name),
            Some(e.kind.clone()),
            format!("M{cx:.1},{:.1} V{:.1}", t.y + t.h, top - 1.0),
            false,
            false,
            true,
        );
        b.label(cx + 8.0 + label_w(&e.kind) / 2.0, top - 8.0, &e.kind, "lb");
        if b.tools
            && let Some(server) = backticked(&e.detail)
        {
            b.tag(cx - 6.0, top - 8.0, &format!("{server}_*"), "end");
        }
        deepest = deepest.max(top + h);
    }
    // The portal's tag: the servers it serves, into /mcp.
    if b.tools {
        for e in &all {
            if e.kind != "portal" {
                continue;
            }
            let Some(servers) = e.detail.split("servers ").nth(1) else {
                continue;
            };
            if let Some(r) = svc_pos.get(&e.name).copied() {
                let t = fit(&format!("serves {servers} → /mcp"), r.w / 2.0 + 40.0, 10.0);
                b.tag(r.x + r.w / 2.0 + 6.0, r.y + r.h + 15.0, &t, "start");
            }
        }
    }

    // 4. CALLED BY <Worker>: the hub's and other edge Workers' third parties, down the gutter.
    let mut bottom = deepest;
    for owner in &fed {
        let es = outside_of(owner);
        if es.is_empty() {
            continue;
        }
        let Some(r) = pos.get(*owner).copied() else {
            continue;
        };
        let yr = deepest + 40.0;
        let label = format!("CALLED BY {}", owner.to_uppercase());
        b.eyebrow(P, yr - 8.0, &label, "eb");
        let k2 = es.len();
        let ew = (w_all - 2.0 * P - (k2 - 1) as f64 * 12.0) / k2 as f64;
        let mut hmax = 0.0f64;
        let last_cx = P + (k2 - 1) as f64 * (ew + 12.0) + ew / 2.0;
        b.edge(
            "service",
            &format!("worker:{owner}"),
            &format!("worker:{owner}"),
            None,
            format!(
                "M{:.1},{:.1} H{GUTTER:.1} V{:.1} H{last_cx:.1}",
                r.x,
                r.y + r.h - 14.0,
                yr + 6.0
            ),
            false,
            false,
            false,
        );
        for (j, e) in es.iter().enumerate() {
            let x = P + j as f64 * (ew + 12.0);
            let key = key_once(&e.by);
            b.edge(
                "service",
                &format!("worker:{owner}"),
                &format!("service:{}", e.name),
                None,
                format!("M{:.1},{:.1} V{:.1}", x + ew / 2.0, yr + 6.0, yr + 21.0),
                false,
                !e.inbound.is_empty(),
                true,
            );
            let h = b.service_card(e, x, yr + 22.0, ew, "called", key);
            svc_pos.insert(
                e.name.clone(),
                Rect {
                    x,
                    y: yr + 22.0,
                    w: ew,
                    h,
                },
            );
            hmax = hmax.max(h);
        }
        b.row("called", &label, yr + 22.0, hmax);
        deepest = yr + 22.0 + hmax;
        bottom = deepest;
    }
    if outside_drawn {
        b.eyebrow(P, region_bottom + 26.0, "OUTSIDE THE ACCOUNT", "eb");
        let first_called = b.rows.iter().find(|r| r.id == "called").map(|r| r.y - 62.0);
        let end = first_called.unwrap_or(bottom);
        b.row("outside", "OUTSIDE THE ACCOUNT", y0, (end - y0).max(0.0));
    }
    // Rows in drawing order.
    b.rows.sort_by(|a, c| a.y.total_cmp(&c.y));
    (Some(region), bottom + 24.0)
}

/// The word between backticks in `s`, such as a portal server's id.
fn backticked(s: &str) -> Option<&str> {
    let start = s.find('`')? + 1;
    let len = s[start..].find('`')?;
    Some(&s[start..start + len]).filter(|w| !w.is_empty())
}

/// The narrow layout: every row stacked, the Workers hanging off a rail from the hub.
fn narrow(b: &mut Build) -> (Option<Rect>, f64) {
    let m = b.m;
    let hub = b.hub;
    let w_all = b.w;
    let full_w = w_all - 2.0 * P + 16.0;
    let left = P - 8.0;
    let counts = route_counts(m, hub);
    let mcp = serves_mcp(m, hub);
    let mut y = 26.0;

    // Hostnames, one under another.
    let order = host_order(m);
    if !order.is_empty() {
        b.eyebrow(left, y, "HOSTNAMES", "eb");
        y += 10.0;
        let top = y;
        for i in &order {
            let h = &m.hostnames[*i];
            let chips: Vec<(String, &'static str)> = if b.full {
                counts[*i]
                    .iter()
                    .map(|(k, c)| (format!("{k} {c}"), chip_class(k)))
                    .collect()
            } else {
                Vec::new()
            };
            let rows = pill_rows(&chips, full_w - 24.0);
            let hh = if b.full {
                46.0 + 22.0 * rows.len() as f64
            } else {
                40.0
            };
            b.card(Card {
                id: format!("host:{}", h.host),
                kind: "hostname",
                row: "hostnames",
                name: h.host.clone(),
                x: left,
                y,
                w: full_w,
                h: hh,
                hub: false,
                access: Some(behind_access(h)),
                place: None,
            });
            let (st, cls) = host_status(h);
            let st_w = pill_w(st);
            b.text(
                left + 12.0,
                y + 19.0,
                "hn",
                &fit(
                    &h.host,
                    full_w - 36.0 - if b.full { st_w } else { 0.0 },
                    13.0,
                ),
                "start",
            );
            if b.full {
                b.pill(left + full_w - 12.0 - st_w, y + 6.0, st, cls);
            }
            let mut sub = format!("→ {}", h.worker);
            if b.tools && mcp[*i] {
                sub.push_str("  /mcp");
            }
            b.text(
                left + 12.0,
                y + 34.0,
                "meta",
                &fit(&sub, full_w - 24.0, 11.0 * 0.93),
                "start",
            );
            for (r, prow) in rows.iter().enumerate() {
                let mut at = left + 12.0;
                for (t, cl) in prow {
                    at += b.pill(at, y + 42.0 + r as f64 * 22.0, t, cl) + 6.0;
                }
            }
            y += hh + 8.0;
        }
        b.row("hostnames", "HOSTNAMES", top, y - 8.0 - top);
        y += 16.0;
    }

    let all = services(m);
    if m.workers.is_empty() {
        return (None, y + 8.0);
    }

    // The account: the hub, the rail, each Worker and its services in the account.
    let region_top = y;
    y += 26.0;
    let mut pos: BTreeMap<String, Rect> = BTreeMap::new();
    let named: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
    let hangs = |e: &Dependency| !e.through.is_empty() && named.contains(&e.through.as_str());
    let mut groups: BTreeMap<&str, Vec<&Dependency>> = BTreeMap::new();
    for e in all.iter().filter(|e| !hangs(e)) {
        let caller = if e.by.is_empty() {
            hub
        } else {
            caller_of(m, hub, e)
        };
        if let Some(c) = caller {
            groups.entry(c).or_default().push(e);
        }
    }
    let mut keyed: Vec<String> = Vec::new();
    let mut key_once = |by: &str| {
        if keyed.iter().any(|k| k == by) {
            false
        } else {
            keyed.push(by.to_string());
            true
        }
    };
    let account_of = |name: &str| -> Vec<&Dependency> {
        groups
            .get(name)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|e| place(e) == "account")
                    .collect()
            })
            .unwrap_or_default()
    };
    if let Some(hn) = hub
        && let Some(wk) = b.worker(hn)
    {
        let h = b.worker_h(wk, full_w);
        let r = b.worker_card(wk, left, y, full_w, h, "edge");
        pos.insert(hn.to_string(), r);
        b.row("edge", "", y, h);
        y += h;
    }
    let others: Vec<&Worker> = m
        .workers
        .iter()
        .filter(|w| Some(w.name.as_str()) != hub)
        .collect();
    let rail = left + 14.0;
    let rail_top = y;
    let mut last_mid = None;
    // The hub's services in the account first, on the rail.
    let mut account_services = |b: &mut Build, owner: &str, x: f64, y: &mut f64, w: f64| {
        for e in account_of(owner) {
            let key = key_once(&e.by);
            let h = b.service_card(e, x + 14.0, *y, w - 14.0, "services", key);
            b.edge(
                "service",
                &format!("worker:{owner}"),
                &format!("service:{}", e.name),
                None,
                format!(
                    "M{:.1},{:.1} V{:.1} H{:.1}",
                    x + 4.0,
                    *y - 8.0,
                    *y + 20.0,
                    x + 13.0
                ),
                false,
                false,
                true,
            );
            *y += h + 10.0;
        }
    };
    if hub.is_some() {
        y += 12.0;
        let hn = hub.unwrap_or_default().to_string();
        let before = y;
        account_services(b, &hn, left, &mut y, full_w);
        if y > before {
            b.row("services", "CLOUDFLARE SERVICES", before, y - before);
        }
    }
    if !others.is_empty() {
        if let Some(hn) = hub
            && others
                .iter()
                .any(|w| !binding_label(m, hn, &w.name).is_empty())
        {
            b.eyebrow(left, y + 4.0, "SERVICE BINDINGS", "eb");
            y += 14.0;
        }
        for w in &others {
            let label = hub
                .map(|hn| binding_label(m, hn, &w.name))
                .unwrap_or_default();
            let objs = hub
                .map(|hn| shared_objects(m, hn, &w.name))
                .unwrap_or_default();
            let mut lab = label.clone();
            if !objs.is_empty() {
                lab = format!("{lab}  ◇ {}", objs.join(", "));
            }
            if !lab.trim().is_empty() {
                b.text(
                    rail + 16.0,
                    y + 11.0,
                    "lb",
                    &fit(lab.trim(), w_all - rail - 40.0, 10.0),
                    "start",
                );
            }
            let cy = y + 18.0;
            let cw = left + full_w - rail - 16.0;
            let h = b.worker_h(w, cw);
            let r = b.worker_card(w, rail + 16.0, cy, cw, h, "inner");
            pos.insert(w.name.clone(), r);
            let mid = cy + 22.0;
            if hub.is_some() && !label.is_empty() {
                b.edge(
                    "binding",
                    &format!("worker:{}", hub.unwrap_or_default()),
                    &format!("worker:{}", w.name),
                    Some(label.clone()),
                    format!("M{rail:.1},{mid:.1} H{:.1}", rail + 15.0),
                    !objs.is_empty() && label.is_empty(),
                    false,
                    true,
                );
                last_mid = Some(mid);
            }
            y = cy + h + 10.0;
            if b.tools {
                let mut tags: Vec<String> = Vec::new();
                for c in m.tool_calls.iter().filter(|c| !c.to.is_empty()) {
                    if c.from == w.name {
                        let to = b.worker(&c.to).map_or(c.to.as_str(), short);
                        tags.push(format!("→ {to}  {}", call_label(&c.tools)));
                    } else if c.to == w.name && Some(c.from.as_str()) == hub {
                        let from = b.worker(&c.from).map_or(c.from.as_str(), short);
                        tags.push(format!("← {from}  {}", call_label(&c.tools)));
                    }
                }
                for t in tags {
                    b.tag(rail + 16.0, y + 6.0, &fit(&t, cw - 10.0, 10.0), "start");
                    y += 19.0;
                }
            }
            let before = y;
            account_services(b, &w.name, rail + 16.0, &mut y, cw);
            if y > before {
                b.row("services", "CLOUDFLARE SERVICES", before, y - before);
            }
            y += 4.0;
        }
        if let Some(mid) = last_mid {
            b.edge(
                "bus",
                &format!("worker:{}", hub.unwrap_or_default()),
                &format!("worker:{}", hub.unwrap_or_default()),
                None,
                format!("M{rail:.1},{rail_top:.1} V{mid:.1}"),
                false,
                false,
                false,
            );
        }
    }
    let region = Rect {
        x: 8.0,
        y: region_top,
        w: w_all - 16.0,
        h: y + 10.0 - region_top,
    };
    let zone = m.resources.zone.clone().unwrap_or_default();
    let lab = if zone.is_empty() {
        "CLOUDFLARE ACCOUNT".to_string()
    } else {
        format!("CLOUDFLARE ACCOUNT · {}", zone.to_uppercase())
    };
    b.eyebrow(left, region_top + 4.0, &lab, "eb-acc");
    y += 34.0;

    // Outside the account: each Worker's third parties, the hub's last.
    let mut owners: Vec<&str> = others.iter().map(|w| w.name.as_str()).collect();
    if let Some(hn) = hub {
        owners.push(hn);
    }
    let hanging: Vec<&Dependency> = all.iter().filter(|e| hangs(e)).collect();
    let outside_top = y;
    let mut any = false;
    for owner in owners {
        let es: Vec<&Dependency> = groups
            .get(owner)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|e| place(e) == "outside")
                    .collect()
            })
            .unwrap_or_default();
        let through: Vec<&Dependency> = hanging
            .iter()
            .copied()
            .filter(|e| {
                all.iter().find(|x| x.name == e.through).and_then(|t| {
                    if t.by.is_empty() {
                        hub
                    } else {
                        caller_of(m, hub, t)
                    }
                }) == Some(owner)
            })
            .collect();
        if es.is_empty() && through.is_empty() {
            continue;
        }
        if !any {
            b.eyebrow(left, y, "OUTSIDE THE ACCOUNT", "eb");
            y += 14.0;
            any = true;
        }
        let title = format!("CALLED BY {}", owner.to_uppercase());
        b.text(left, y + 8.0, "dim", &title, "start");
        y += 16.0;
        let x = left + 16.0;
        let w = full_w - 16.0;
        for e in es.iter().chain(through.iter()) {
            let indent = if hangs(e) { 16.0 } else { 0.0 };
            if hangs(e) {
                b.text(
                    x + indent,
                    y + 9.0,
                    "lb",
                    &fit(
                        &format!("↳ {}, through {}", e.kind, e.through),
                        w - indent,
                        10.0,
                    ),
                    "start",
                );
                y += 14.0;
            }
            let key = key_once(&e.by);
            let h = b.service_card(e, x + indent, y, w - indent, "outside", key);
            b.edge(
                if hangs(e) { "through" } else { "service" },
                &format!("worker:{owner}"),
                &format!("service:{}", e.name),
                None,
                format!(
                    "M{:.1},{:.1} V{:.1} H{:.1}",
                    x - 8.0 + indent,
                    y - 6.0,
                    y + 20.0,
                    x + indent - 1.0
                ),
                true,
                false,
                true,
            );
            y += h + 8.0;
        }
        y += 6.0;
    }
    if any {
        b.row(
            "outside",
            "OUTSIDE THE ACCOUNT",
            outside_top,
            y - outside_top,
        );
    } else {
        y -= 24.0;
    }
    (Some(region), y + 8.0)
}

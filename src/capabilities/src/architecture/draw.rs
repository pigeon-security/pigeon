//! The architecture drawing as SVG: it paints a [`Layout`] and decides nothing about where
//! things go. The SVG carries its own style, in the console's tokens, so it renders the same as
//! an `<img>` (which sees no page style) and inline. Its marker and pattern ids are its own,
//! prefixed per drawing, so several drawings on one page never take each other's arrowheads.

use pigeon_core::design::escape;
use serde::Serialize;

use super::Model;
use super::layout::{Layout, Mark, Mode, Width, layout};

/// The colours a drawing uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Light, and dark when the viewer prefers it.
    Auto,
    Light,
    Dark,
}

impl Theme {
    /// `auto`, `light` or `dark`; anything else is none.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// A drawing: its SVG, its title and the same content as plain text.
#[derive(Serialize, Clone, Debug)]
pub struct Drawing {
    pub svg: String,
    pub title: String,
    pub text: String,
}

const LIGHT: &str = "--bg:#FFFFFF;--sunk:#F6F5F7;--surface:#FFFFFF;--raised:#FAF9FB;--line:#E4E2E8;\
--line-strong:#CFCCD6;--ink:#1D1B21;--ink-2:#47434F;--muted:#6E6A76;--accent:#B03A6A;\
--accent-soft:#FBEAF1;--accent-line:#E9B7CC;--info:#2C5E9E;--info-soft:#E7EEF8;--dot:#D5D1DB;\
--tool:#4F46C8;--tool-soft:#ECEBFB;";
const DARK: &str = "--bg:#141418;--sunk:#101013;--surface:#18181D;--raised:#1D1D23;--line:#2A2A31;\
--line-strong:#3B3B44;--ink:#ECEAE6;--ink-2:#C3C0C8;--muted:#98949F;--accent:#E07FA8;\
--accent-soft:#2B1821;--accent-line:#6A3550;--info:#8DB6F0;--info-soft:#15202F;--dot:#34333D;\
--tool:#A39DF6;--tool-soft:#1F1D3A;";

/// The drawing's classes, in the tokens.
const STYLE: &str = r#"
.dg{font-family:Inter,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
.bg{fill:var(--sunk)}.dot{fill:var(--dot)}.head{fill:var(--muted)}.thead{fill:var(--tool)}
.region{fill:none;stroke:var(--line-strong);stroke-width:1;stroke-dasharray:4 4}
.card{fill:var(--surface);stroke:var(--line-strong);stroke-width:1}
.card.hub{stroke:var(--accent-line);stroke-width:1.5}.card.cf{fill:var(--raised)}
.wn,.hn,.wn-acc{fill:var(--ink);font:600 12.5px "JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,monospace}
.hn{font-size:13px}.wn-acc{fill:var(--accent)}
.en{fill:var(--ink);font-weight:600;font-size:12px}
.meta{fill:var(--muted);font-size:11px}
.eb{fill:var(--muted);font-weight:500;font-size:10.5px;letter-spacing:.08em}.eb-acc{fill:var(--accent);font-weight:500;font-size:10.5px;letter-spacing:.08em}
.ebg{fill:var(--sunk)}
.pl-b{fill:var(--raised);stroke:var(--line)}.pl-acc-b{fill:var(--accent-soft);stroke:var(--accent-line)}
.pl-info-b{fill:var(--info-soft);stroke:none}.pl-open-b{fill:none;stroke:var(--line-strong);stroke-dasharray:2 2}
.pl-t,.pl-acc-t,.pl-info-t,.pl-open-t,.lb,.dim,.tl-t{font:10.5px "JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,monospace}
.pl-t{fill:var(--ink-2)}.pl-acc-t{fill:var(--accent)}.pl-info-t{fill:var(--info);font-weight:500}.pl-open-t{fill:var(--muted)}
.lb,.dim,.tl-t{font-size:10px}.lb{fill:var(--ink-2)}.dim{fill:var(--muted)}
.lbg{fill:var(--sunk)}
.fo,.fi{font-size:10.5px}.fo{fill:var(--ink-2)}.fi{fill:var(--info)}
.sep{stroke:var(--line);stroke-width:1;fill:none}
.ln{fill:none;stroke:var(--muted);stroke-width:1}.ln.dash{stroke-dasharray:3 3}
.tl{fill:none;stroke:var(--tool);stroke-width:1.5}.tl-b{fill:var(--tool-soft);stroke:var(--tool);stroke-width:.8}.tl-t{fill:var(--tool);font-weight:500}
"#;

/// The style for `theme`: its tokens, then the classes.
fn style(theme: Theme) -> String {
    let tokens = match theme {
        Theme::Light => format!("svg{{{LIGHT}}}"),
        Theme::Dark => format!("svg{{{DARK}}}"),
        Theme::Auto => {
            format!("svg{{{LIGHT}}}@media (prefers-color-scheme:dark){{svg{{{DARK}}}}}")
        }
    };
    format!("{tokens}{STYLE}")
}

/// A short id prefix for one drawing, from what makes it differ from another.
fn prefix(l: &Layout, theme: Theme, salt: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let key = format!(
        "{:?}|{:?}|{:?}|{}|{:.0}|{:.0}|{}|{salt}",
        l.mode,
        l.width,
        theme,
        l.title,
        l.w,
        l.h,
        l.cards.len()
    );
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("dg{:08x}", h >> 32)
}

/// The drawing of `m` in `mode`, `width` wide, in `theme`. `salt` tells apart two drawings of
/// the same model, such as one per instance.
pub fn draw(m: &Model, mode: Mode, width: Width, theme: Theme) -> Drawing {
    let salt = m
        .instance
        .as_ref()
        .map(|i| i.id.as_str())
        .unwrap_or("platform");
    let l = layout(m, mode, width);
    let svg = render(&l, theme, salt);
    Drawing {
        svg,
        title: l.title.clone(),
        text: text(&l),
    }
}

/// `l` painted as SVG.
pub fn render(l: &Layout, theme: Theme, salt: &str) -> String {
    let id = prefix(l, theme, salt);
    let mut base: Vec<String> = Vec::new();
    let mut tool: Vec<String> = Vec::new();
    if let Some(r) = &l.region {
        base.push(format!(
            r#"<rect class="region" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="8"/>"#,
            r.x, r.y, r.w, r.h
        ));
    }
    for e in &l.edges {
        let tl = e.layer == "tool";
        let class = match (tl, e.dashed) {
            (true, _) => "tl",
            (false, true) => "ln dash",
            (false, false) => "ln",
        };
        let marker = if tl { "th" } else { "ah" };
        let start = if e.start {
            format!(r#" marker-start="url(#{id}-{marker})""#)
        } else {
            String::new()
        };
        let end = if e.end {
            format!(r#" marker-end="url(#{id}-{marker})""#)
        } else {
            String::new()
        };
        let out = if tl { &mut tool } else { &mut base };
        out.push(format!(
            r#"<path class="{class}" d="{}"{start}{end}/>"#,
            e.d
        ));
    }
    for c in &l.cards {
        let mut class = String::from("card");
        if c.hub {
            class.push_str(" hub");
        }
        if c.place.as_deref() == Some("account") {
            class.push_str(" cf");
        }
        base.push(format!(
            r#"<rect class="{class}" x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" rx="6"/>"#,
            c.x, c.y, c.w, c.h
        ));
    }
    for mk in &l.marks {
        let out = if mk.layer == "tool" {
            &mut tool
        } else {
            &mut base
        };
        out.push(mark(mk));
    }
    let tool = if tool.is_empty() {
        String::new()
    } else {
        format!(r#"<g class="tools">{}</g>"#, tool.concat())
    };
    let title = escape(&l.title);
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" class="dg" viewBox="0 0 {w:.0} {h:.0}" width="{w:.0}" height="{h:.0}" role="img" aria-labelledby="{id}-t"><title id="{id}-t">{title}</title><style>{style}</style><defs><pattern id="{id}-dots" width="16" height="16" patternUnits="userSpaceOnUse"><circle class="dot" cx="1.5" cy="1.5" r="1.1"/></pattern><marker id="{id}-ah" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="head" d="M0,0 L8,4 L0,8 z"/></marker><marker id="{id}-th" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="thead" d="M0,0 L8,4 L0,8 z"/></marker></defs><rect class="bg" width="{w:.0}" height="{h:.0}"/><rect width="{w:.0}" height="{h:.0}" fill="url(#{id}-dots)"/>{body}{tool}</svg>"#,
        w = l.w,
        h = l.h,
        style = style(theme),
        body = base.concat(),
    )
}

/// One mark as SVG.
fn mark(m: &Mark) -> String {
    let t = escape(&m.text);
    match m.kind {
        "pill" => format!(
            r#"<rect class="{c}-b" x="{x:.1}" y="{y:.1}" width="{w:.1}" height="18" rx="3"/><text class="{c}-t" x="{tx:.1}" y="{ty:.1}">{t}</text>"#,
            c = m.class,
            x = m.x,
            y = m.y,
            w = m.w,
            tx = m.x + 6.0,
            ty = m.y + 12.5
        ),
        "tag" => format!(
            r#"<rect class="tl-b" x="{x:.1}" y="{y:.1}" width="{w:.1}" height="15" rx="3"/><text class="tl-t" x="{tx:.1}" y="{ty:.1}">{t}</text>"#,
            x = m.x,
            y = m.y - 11.0,
            w = m.w,
            tx = m.x + 5.0,
            ty = m.y
        ),
        "label" => format!(
            r#"<rect class="lbg" x="{x:.1}" y="{y:.1}" width="{w:.1}" height="15" rx="3"/><text class="{c}" x="{tx:.1}" y="{ty:.1}" text-anchor="middle">{t}</text>"#,
            c = m.class,
            x = m.x - m.w / 2.0,
            y = m.y - 11.0,
            w = m.w,
            tx = m.x,
            ty = m.y
        ),
        "eyebrow" => format!(
            r#"<rect class="ebg" x="{x:.1}" y="{y:.1}" width="{w:.1}" height="16" rx="3"/><text class="{c}" x="{tx:.1}" y="{ty:.1}">{t}</text>"#,
            c = m.class,
            x = m.x - 6.0,
            y = m.y - 12.0,
            w = m.w,
            tx = m.x,
            ty = m.y
        ),
        "sep" => format!(
            r#"<path class="sep" d="M{:.1},{:.1} H{:.1}"/>"#,
            m.x,
            m.y,
            m.x + m.w
        ),
        _ => {
            let anchor = if m.anchor == "start" {
                String::new()
            } else {
                format!(r#" text-anchor="{}""#, m.anchor)
            };
            format!(
                r#"<text class="{}" x="{:.1}" y="{:.1}"{anchor}>{t}</text>"#,
                m.class, m.x, m.y
            )
        }
    }
}

/// The drawing as plain text: each row and what is in it, then the labelled lines.
pub fn text(l: &Layout) -> String {
    let mut out = vec![l.title.clone()];
    for row in &l.rows {
        let cards: Vec<String> = l
            .cards
            .iter()
            .filter(|c| c.row == row.id)
            .map(|c| match (c.access, c.hub) {
                (Some(true), _) => format!("{} (Access)", c.name),
                (Some(false), _) => format!("{} (no Access)", c.name),
                (_, true) => format!("{} (hub)", c.name),
                _ => c.name.clone(),
            })
            .collect();
        let name = if row.label.is_empty() {
            "Workers"
        } else {
            &row.label
        };
        if !cards.is_empty() {
            out.push(format!("{name}: {}", cards.join(", ")));
        }
    }
    let mut lines: Vec<String> = l
        .edges
        .iter()
        .filter_map(|e| {
            let label = e.label.as_ref()?;
            let name = |id: &str| id.split_once(':').map_or(id, |(_, n)| n).to_string();
            Some(format!(
                "{} → {}: {label} ({})",
                name(&e.from),
                name(&e.to),
                e.kind
            ))
        })
        .collect();
    lines.dedup();
    out.extend(lines);
    out.join("\n")
}

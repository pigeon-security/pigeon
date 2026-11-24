//! The pages of the pigeon.dev apex: the home page and the architecture page, as pure HTML,
//! and the routing of a method and path to one of them. No Cloudflare calls.

use crate::{design::escape, git::base64};

/// The Content-Security-Policy of the apex pages: inline styles and the logo, nothing else.
pub const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// What a request for the apex is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `/`, the home page.
    Home,
    /// `/architecture`, the drawing.
    Architecture,
    /// `/architecture.json`, the public model.
    Json,
    /// `/logo.png`, the logo.
    Logo,
    /// No such page.
    NotFound,
    /// A known path, asked with a method other than GET or HEAD.
    MethodNotAllowed,
}

/// The route for `method` and `path`. Paths are matched exactly: no trailing slash.
pub fn route(method: &str, path: &str) -> Route {
    let known = match path {
        "/" => Route::Home,
        "/architecture" => Route::Architecture,
        "/architecture.json" => Route::Json,
        "/logo.png" => Route::Logo,
        _ => return Route::NotFound,
    };
    if method == "GET" || method == "HEAD" {
        known
    } else {
        Route::MethodNotAllowed
    }
}

const CSS: &str = r#"
:root {
  color-scheme: dark;
  --bg: #141418;
  --card: #1F1F25;
  --shadow: #0B0B0E;
  --ink: #ECEAE6;
  --muted: #B9B6BF;
  --pink: #E07FA8;
  --line: #2C2C33;
  --gutter: clamp(16px, 5vw, 56px);
}
* { box-sizing: border-box; }
body {
  margin: 0;
  min-height: 100vh;
  display: flex;
  flex-direction: column;
  background: var(--bg) radial-gradient(circle, rgba(255,255,255,.07) 1px, transparent 1px) 0 0 / 22px 22px;
  color: var(--ink);
  font: 16px/1.5 Inter, system-ui, sans-serif;
}
.bar { border-bottom: 1px solid var(--line); background: var(--bg); }
.bar-inner, main, footer { width: min(100% - var(--gutter) * 2, 1120px); margin-inline: auto; }
.bar-inner { display: flex; align-items: center; flex-wrap: wrap; gap: 8px 24px; padding-block: 12px; }
.bar a { text-decoration: none; }
.brand { display: flex; align-items: center; gap: 10px; color: var(--ink); font-size: 20px; font-weight: 700; }
.logo { display: grid; place-items: center; width: 32px; height: 32px; border-radius: 9px; background: var(--pink); color: #1A0F14; }
.bar nav { display: flex; gap: 4px; flex-wrap: wrap; }
.tab, .console { display: inline-flex; align-items: center; min-height: 40px; padding: 0 12px; border-radius: 12px; font-size: 14px; font-weight: 600; }
.tab { color: var(--muted); }
.tab:hover, .tab[aria-current=page] { color: var(--pink); background: var(--card); }
.console { margin-left: auto; gap: 6px; background: var(--pink); color: #1A0F14; }
.console:hover { background: #F2A3C4; }
:focus-visible { outline: 2px solid #F2A3C4; outline-offset: 3px; }
main { padding-block: 48px; flex: 1 0 auto; min-width: 0; }
.label, footer { color: var(--muted); font: 13px/1.5 ui-monospace, monospace; }
.label { margin: 0 0 10px; text-transform: uppercase; letter-spacing: .08em; }
h1 { margin: 0; font: 700 clamp(36px, 7vw, 56px)/1.06 "Inter Tight", Inter, system-ui, sans-serif; letter-spacing: -.035em; }
.diagram-frame { margin-top: 24px; padding: 8px; overflow-x: auto; border: 1px solid var(--line); border-radius: 22px; background: var(--card); box-shadow: 0 6px 0 var(--shadow); }
.diagram-frame img { display: block; width: 100%; min-width: 840px; height: auto; }
footer { margin-top: auto; padding-block: 14px 20px; border-top: 1px solid var(--line); text-align: right; overflow-wrap: anywhere; }
@media (max-width: 500px) {
  .bar nav { order: 1; flex: 1 0 100%; }
}
"#;

/// Architecture provenance supplied by the deployed platform, not the sites build.
#[derive(Clone, Copy)]
pub struct Build<'a> {
    pub commit: &'a str,
    pub digest: &'a str,
}

fn page(title: &str, architecture: bool, content: &str, build: Option<Build<'_>>) -> String {
    let current = " aria-current=\"page\"";
    let (overview, diagram) = if architecture {
        ("", current)
    } else {
        (current, "")
    };
    let stamp = build.map_or_else(
        || "Deployment metadata unavailable".to_string(),
        |b| {
            let short: String = b.commit.chars().take(12).collect();
            let commit = if short.is_empty() {
                "Local build".into()
            } else {
                escape(&short)
            };
            format!("{commit} · digest {}", escape(b.digest))
        },
    );
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="theme-color" content="#141418"><title>{title}</title>
<style>{CSS}</style></head><body>
<header class="bar"><div class="bar-inner">
<a class="brand" href="/"><span class="logo" aria-hidden="true">P</span>pigeon</a>
<nav aria-label="Site"><a class="tab" href="/"{overview}>Overview</a><a class="tab" href="/architecture"{diagram}>Architecture</a></nav>
<a class="console" href="https://platform.pigeon.dev/app">Console <span aria-hidden="true">↗</span></a>
</div></header><main>{content}</main><footer>{stamp}</footer></body></html>"##
    )
}

/// The overview. It still renders when the platform cannot supply deployment metadata.
pub fn home_html(build: Option<Build<'_>>) -> String {
    page(
        "pigeon",
        false,
        "<p class=\"label\">Coming soon</p><h1>Find the pattern. Act with confidence.</h1>",
        build,
    )
}

/// The architecture page uses the platform's public SVG in image mode, never as inline markup.
pub fn architecture_html(svg: &str, build: Build<'_>) -> String {
    let image = base64(svg.as_bytes());
    page(
        "pigeon architecture",
        true,
        &format!(
            r#"<p class="label">Architecture</p><h1>Diagram</h1><div class="diagram-frame" role="region" aria-label="Platform diagram" tabindex="0"><img src="data:image/svg+xml;base64,{image}" alt="Platform architecture: routes, Workers, Durable Objects, containers and services"></div>"#
        ),
        Some(build),
    )
}

/// The architecture page when core cannot supply the diagram.
pub fn unavailable_html(build: Option<Build<'_>>) -> String {
    page(
        "pigeon architecture",
        true,
        "<p class=\"label\">Architecture</p><h1>Diagram</h1><p>The architecture is not available right now. Try again soon.</p>",
        build,
    )
}

//! The pages of the pigeon.dev apex: the home page and the architecture page, as pure HTML, the
//! images they use, and the routing of a method and path to one of them. No Cloudflare calls.
//! Both Workers that can own the apex (pigeon-sites and the platform) serve from here.

use crate::{design::escape, git::base64};

/// The Content-Security-Policy of the apex pages: inline styles and images from the apex itself
/// or `data:`, nothing else.
pub const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Where the console is.
pub const CONSOLE: &str = "https://platform.pigeon.dev/app";

/// What a request for the apex is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// `/`, the home page.
    Home,
    /// `/architecture`, the drawing.
    Architecture,
    /// `/architecture.json`, the public model.
    Json,
    /// `/logo.png`, the iOS app icon (pigeon-sites only).
    Logo,
    /// One of the pages' images.
    Asset(Asset),
    /// No such page.
    NotFound,
    /// A known path, asked with a method other than GET or HEAD.
    MethodNotAllowed,
}

/// An image the apex pages use, built into the Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asset {
    /// `/mark/head.webp`: the mark's artwork, drawn on its ground by CSS.
    MarkHead,
    /// `/favicon-32.png`.
    Favicon,
    /// `/apple-touch-icon.png`, 180px.
    AppleTouch,
}

impl Asset {
    /// Every asset.
    pub const ALL: [Asset; 3] = [Asset::MarkHead, Asset::Favicon, Asset::AppleTouch];

    /// The path it is served at.
    pub fn path(self) -> &'static str {
        match self {
            Asset::MarkHead => "/mark/head.webp",
            Asset::Favicon => "/favicon-32.png",
            Asset::AppleTouch => "/apple-touch-icon.png",
        }
    }

    /// Its `Content-Type`.
    pub fn content_type(self) -> &'static str {
        match self {
            Asset::MarkHead => "image/webp",
            Asset::Favicon | Asset::AppleTouch => "image/png",
        }
    }

    /// Its bytes.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Asset::MarkHead => include_bytes!("../assets/mark-head.webp"),
            Asset::Favicon => include_bytes!("../assets/favicon-32.png"),
            Asset::AppleTouch => include_bytes!("../assets/apple-touch-180.png"),
        }
    }
}

/// The `Cache-Control` of an image: a day.
pub const ASSET_CACHE: &str = "public, max-age=86400";

/// The route for `method` and `path`. Paths are matched exactly: no trailing slash.
pub fn route(method: &str, path: &str) -> Route {
    let known = match path {
        "/" => Route::Home,
        "/architecture" => Route::Architecture,
        "/architecture.json" => Route::Json,
        "/logo.png" => Route::Logo,
        _ => match Asset::ALL.into_iter().find(|a| a.path() == path) {
            Some(a) => Route::Asset(a),
            None => return Route::NotFound,
        },
    };
    if method == "GET" || method == "HEAD" {
        known
    } else {
        Route::MethodNotAllowed
    }
}

const CSS: &str = include_str!("home.css");

/// Architecture provenance supplied by the deployed platform, not the sites build.
#[derive(Clone, Copy)]
pub struct Build<'a> {
    pub commit: &'a str,
    pub digest: &'a str,
}

/// The footer's build stamp: the commit, cut to 12 characters, and the digest.
fn stamp(build: Option<Build<'_>>) -> String {
    build.map_or_else(
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
    )
}

fn page(title: &str, architecture: bool, content: &str, build: Option<Build<'_>>) -> String {
    let current = " aria-current=\"page\"";
    let (overview, diagram) = if architecture {
        ("", current)
    } else {
        (current, "")
    };
    let stamp = stamp(build);
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="theme-color" content="#FFFFFF" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#141418" media="(prefers-color-scheme: dark)">
<meta name="description" content="pigeon is a private workspace for making sense of money records.">
<title>{title}</title>
<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32.png">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<style>{CSS}</style></head><body>
<a class="skip" href="#main">Skip to content</a>
<header class="top"><div class="wrap">
<a class="brand" href="/">pigeon</a>
<nav class="nav" aria-label="Site"><a href="/"{overview}>Overview</a><a href="/architecture"{diagram}>Architecture</a></nav>
<span class="spacer"></span>
<a class="btn btn-primary" href="{CONSOLE}">Open console <span aria-hidden="true">↗</span></a>
</div></header>
<main id="main"><div class="wrap">{content}</div></main>
<footer class="foot"><div class="wrap">
<span>{stamp}</span>
<nav aria-label="Footer"><a href="/architecture">Architecture</a><a href="/architecture.json">architecture.json</a><a href="{CONSOLE}">Console</a></nav>
</div></footer>
</body></html>"##
    )
}

/// The home page's content, between the top bar and the footer. The example record's figures
/// are invented, and the panel says so.
const HOME: &str = r##"<div class="hero">
<div class="hero-copy">
<span class="mark" aria-hidden="true"></span>
<span><span class="pill accent">Invite only</span></span>
<h1>Find the pattern. Act with confidence.</h1>
<p class="lede">pigeon is a private workspace for making sense of money records. Bring in statements, receipts and messages you already have. pigeon lines them up by date, shows what repeats, and keeps a record you can rely on.</p>
<div class="actions">
<a class="btn btn-primary btn-lg" href="https://platform.pigeon.dev/app">Open console <span aria-hidden="true">↗</span></a>
<a class="btn btn-ghost btn-lg" href="#how">How it works</a>
</div>
<p class="fine">Sign in with a passkey. pigeon is in a small private test; there is no public sign-up yet.</p>
</div>
<figure class="panel record" aria-label="Example of a pattern pigeon finds">
<div class="record-head">
<span class="t">Transfers after each deposit</span>
<span class="pill info">Pattern</span>
<span class="tag">Example</span>
</div>
<dl class="record-sum">
<div><dt>Times</dt><dd>7</dd></div>
<div><dt>Since</dt><dd>Mar 2026</dd></div>
<div><dt>Total</dt><dd>$2,940.00</dd></div>
</dl>
<div class="tbl-wrap">
<table class="tbl">
<thead><tr><th>Date</th><th>What</th><th class="hide-sm">Source</th><th class="num">Amount</th></tr></thead>
<tbody>
<tr><td class="mono">2026-09-01</td><td>Paycheck in<span class="sub">Checking ··4417</span></td><td class="hide-sm"><span class="tag">statement</span></td><td class="num">+$1,850.00</td></tr>
<tr class="hit"><td class="mono">2026-09-01</td><td>Transfer out<span class="sub">to ··0932, same day</span></td><td class="hide-sm"><span class="tag">statement</span></td><td class="num">−$420.00</td></tr>
<tr><td class="mono">2026-10-01</td><td>Paycheck in<span class="sub">Checking ··4417</span></td><td class="hide-sm"><span class="tag">statement</span></td><td class="num">+$1,850.00</td></tr>
<tr class="hit"><td class="mono">2026-10-02</td><td>Transfer out<span class="sub">to ··0932, next day</span></td><td class="hide-sm"><span class="tag">statement</span></td><td class="num">−$420.00</td></tr>
<tr><td class="mono">2026-10-02</td><td>Message mentions “the usual”<span class="sub">linked to the transfer above</span></td><td class="hide-sm"><span class="tag">email</span></td><td class="num muted">—</td></tr>
</tbody>
</table>
</div>
<div class="record-foot">
<span>Each row links to its original file, unchanged.</span>
<span class="mono">5 of 21 rows</span>
</div>
</figure>
</div>
<section class="block" id="how" aria-labelledby="how-h">
<header><span class="label">How it works</span><h2 id="how-h">Three steps, at your pace</h2></header>
<ol class="steps">
<li><h3>Bring it in</h3><p>Add bank statements, card exports, receipts and emails. pigeon keeps each original exactly as it arrived, with the date it was added.</p></li>
<li><h3>See what repeats</h3><p>pigeon puts everything on one timeline and points out what recurs: the same payee, the same day, the same amount. You decide what matters.</p></li>
<li><h3>Keep a clear record</h3><p>Save what you find as a dated record with its sources attached, ready when you choose to share it with someone you trust.</p></li>
</ol>
</section>
<section class="block" id="private" aria-labelledby="priv-h">
<header><span class="label">Privacy</span><h2 id="priv-h">Yours, and only yours</h2></header>
<div class="tbl-wrap">
<table class="kv">
<tbody>
<tr><th>Sign-in</th><td>Passkeys on your own device. No passwords to guess or reset.</td><td class="st"><span class="pill ok">Live</span></td></tr>
<tr><th>Approvals</th><td>Anything that changes or shares your data waits for your passkey on that exact action.</td><td class="st"><span class="pill ok">Live</span></td></tr>
<tr><th>Your own space</th><td>Each person gets a separate instance. Nothing is pooled with anyone else’s.</td><td class="st"><span class="pill ok">Live</span></td></tr>
<tr><th>Originals</th><td>Files are stored by their content and never edited, so a record can always be traced back.</td><td class="st"><span class="pill warn">In testing</span></td></tr>
<tr><th>Email in</th><td>A private address per person for forwarding messages and receipts.</td><td class="st"><span class="pill">Planned</span></td></tr>
</tbody>
</table>
</div>
</section>
<section class="block" id="hood" aria-labelledby="hood-h">
<header><span class="label">Under the hood</span><h2 id="hood-h">Built in the open</h2></header>
<div class="stack">
<p class="hood">pigeon runs on Cloudflare Workers. Its architecture page is drawn from the deployed code on every release.</p>
<p class="hood"><a class="btn" href="/architecture">View architecture</a> <a class="btn btn-ghost" href="/architecture.json"><span class="mono">architecture.json</span></a></p>
</div>
</section>"##;

/// The overview. It still renders when the platform cannot supply deployment metadata.
pub fn home_html(build: Option<Build<'_>>) -> String {
    page("pigeon", false, HOME, build)
}

/// The heading of the architecture page.
const ARCHITECTURE_HEAD: &str =
    r#"<div class="page-head"><span class="label">Architecture</span><h1>Diagram</h1></div>"#;

/// The architecture page uses the platform's public SVG in image mode, never as inline markup.
pub fn architecture_html(svg: &str, build: Build<'_>) -> String {
    let image = base64(svg.as_bytes());
    page(
        "pigeon architecture",
        true,
        &format!(
            r#"{ARCHITECTURE_HEAD}<div class="figure" role="region" aria-label="Platform diagram" tabindex="0"><img src="data:image/svg+xml;base64,{image}" alt="Platform architecture: routes, Workers, Durable Objects, containers and services"></div>"#
        ),
        Some(build),
    )
}

/// The architecture page when core cannot supply the diagram.
pub fn unavailable_html(build: Option<Build<'_>>) -> String {
    page(
        "pigeon architecture",
        true,
        &format!(
            r#"{ARCHITECTURE_HEAD}<p class="notice">The architecture is not available right now. Try again soon.</p>"#
        ),
        build,
    )
}

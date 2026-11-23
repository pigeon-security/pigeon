//! The pages of the pigeon.dev apex: the home page and the architecture page, as pure HTML,
//! and the routing of a method and path to one of them. No Cloudflare calls.

use crate::design::{Page, escape};

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

/// Small CSS for the top bar and the page body, in the design tokens.
const CSS: &str = "<style>\
.bar{display:flex;align-items:center;gap:var(--space-gap);padding:12px var(--space-page);}\
.brand{display:flex;align-items:center;gap:10px;color:var(--ink);text-decoration:none;font:var(--type-heading);}\
.brand img{width:32px;height:32px;border-radius:8px;}\
.bar nav{display:flex;gap:8px;margin-left:var(--space-gap);}\
.bar nav a{color:var(--ink2);text-decoration:none;padding:6px 12px;border-radius:var(--radius-pill);}\
.bar nav a[aria-current=page]{color:var(--ink);background:var(--chip);}\
.bar .button{margin-left:auto;text-decoration:none;padding:8px 16px;}\
.page{padding:0 var(--space-page);}\
footer{padding:var(--space-page);}\
</style>";

/// The top bar: logo and name, the Architecture tab, and the Console button.
fn bar(current: bool) -> String {
    let cur = if current {
        " aria-current=\"page\""
    } else {
        ""
    };
    format!(
        "<header class=\"bar\"><a class=\"brand\" href=\"/\"><img src=\"/logo.png\" width=\"32\" \
         height=\"32\" alt=\"pigeon\"><span>pigeon</span></a>\
         <nav><a href=\"/architecture\"{cur}>Architecture</a></nav>\
         <a class=\"button\" href=\"https://platform.pigeon.dev\">Console</a></header>"
    )
}

/// A whole page with the top bar and `content`.
fn page(title: &str, current: bool, content: &str) -> String {
    let body = format!("{CSS}{}\n{content}", bar(current));
    Page {
        title,
        description: "pigeon: a platform on Cloudflare Workers.",
        body: &body,
    }
    .render()
}

/// The home page: the top bar on the dot grid, and one short line.
pub fn home_html() -> String {
    page(
        "pigeon",
        false,
        "<main class=\"page\"><p>A platform on Cloudflare Workers.</p></main>",
    )
}

/// The architecture page: the top bar, `drawing` (core's trusted, already escaped HTML, which
/// brings its own `<main>`, inserted as given) and a footer with the first 12 characters of
/// `commit`, escaped.
pub fn architecture_html(drawing: &str, commit: &str) -> String {
    let short: String = commit.chars().take(12).collect();
    page(
        "pigeon architecture",
        true,
        &format!(
            "{drawing}\n<footer><small>commit <code>{}</code></small></footer>",
            escape(&short)
        ),
    )
}

/// The architecture page when core cannot answer: the same top bar and one line.
pub fn unavailable_html() -> String {
    page(
        "pigeon architecture",
        true,
        "<main class=\"page\"><p>The architecture is not available right now. Try again soon.</p></main>",
    )
}

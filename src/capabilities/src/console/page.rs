//! The app's HTML shell (`/app`), its response headers and the redirects from old console
//! bookmarks. The shell holds no content of its own, because the app's scripts build
//! everything; it carries no inline script and no inline event handler, so the policy can forbid
//! both. The console's static assets are served with the same care.

use pigeon_core::design::escape;
use worker::{Headers, Response, Result};

/// The page's policy: its own scripts, styles, images and calls; inline styles for the design
/// system; images also from `data:` URLs, for profile pictures.
pub const CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The app's Credentials page, under Access: where secrets and proxy credentials are listed and
/// set. A fragment naming a secret, `#NAME`, scrolls to it.
pub const CREDENTIALS: &str = "/app?tab=management&subtab=credentials";

/// Routes from old console bookmarks into the app.
pub fn app_redirect(path: &str) -> Option<&'static str> {
    match path.trim_end_matches('/') {
        "" => Some("/app"),
        "/approvals" => Some("/app?tab=approvals"),
        "/instances" => Some("/app?tab=instances"),
        "/architecture" => Some("/app?tab=architecture"),
        "/access" | "/settings" => Some("/app?tab=management"),
        "/cost" | "/gateways" => Some("/app?tab=cost"),
        _ => None,
    }
}

// The asset list serves both the page tags and the exact paths they load.
macro_rules! app_assets {
    ($($view:literal),+ $(,)?) => {
        const APP_ASSETS: &[(&str, &str, &str)] = &[
            $((concat!("/app/", $view, ".css"), "text/css; charset=utf-8",
                include_str!(concat!("web/", $view, ".css"))),)+
            $((concat!("/app/", $view, ".js"), "text/javascript; charset=utf-8",
                include_str!(concat!("web/", $view, ".js"))),)+
            crate::site_passkeys::console::ASSET,
        ];
    };
}
app_assets!(
    "legacy-shell",
    "sessions",
    "cost",
    "settings",
    "connections",
    "architecture",
    "management"
);

/// Where the redesigned console's shell is served while the views are ported, beside the current
/// console at `/app`: its pages are `/app/next/<instance>/<area>/<view>`.
///
/// At cutover (docs/plans/console-redesign.md, "Decommission checklist"), [`next_shell`] moves
/// to `/app` with `data-base="/app"`, `/app/next/*` redirects there in one hop, and
/// [`app_shell`], `APP_ASSETS` and `web/legacy-shell.{js,css}` are deleted.
pub const NEXT_BASE: &str = "/app/next";

/// The redesigned shell's assets, in load order: one stylesheet and its scripts. Its fixtures
/// (`web/fixtures/`, `web/fixtures.html`) are never among them, so fixture mode cannot run on a
/// deployed host.
pub const NEXT_ASSETS: &[(&str, &str, &str)] = &[
    (
        "/app/next/shell.css",
        "text/css; charset=utf-8",
        include_str!("web/shell.css"),
    ),
    (
        "/app/next/shell.js",
        "text/javascript; charset=utf-8",
        include_str!("web/shell.js"),
    ),
    (
        "/app/next/views.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views.js"),
    ),
    (
        "/app/next/views/data.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/data.js"),
    ),
    (
        "/app/next/views/sessions.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/sessions.js"),
    ),
    (
        "/app/next/views/site.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/site.js"),
    ),
    (
        "/app/next/views/capabilities.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/capabilities.js"),
    ),
];

/// Whether `path` is one of the redesigned shell's pages, `/app/next` or below it, rather than
/// an asset: no segment holds a dot.
pub fn is_next_page(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == NEXT_BASE
        || path
            .strip_prefix(NEXT_BASE)
            .is_some_and(|rest| rest.starts_with('/') && !rest.contains('.'))
}

/// The redesigned shell, with the signed-in person's display name if the session is fresh. It
/// never carries `data-fixtures`, so `?fixtures=1` does nothing on a deployed host.
pub fn next_shell(site_name: &str, person: Option<&str>) -> String {
    let mut assets = String::from("<link rel=\"icon\" href=\"/console/icon.png\">\n");
    for (path, kind, _) in NEXT_ASSETS {
        if kind.starts_with("text/css") {
            assets.push_str(&format!("<link rel=\"stylesheet\" href=\"{path}\">\n"));
        }
    }
    assets.push_str("<script src=\"/auth/passkey.js\" defer></script>\n");
    for (path, kind, _) in NEXT_ASSETS {
        if kind.starts_with("text/javascript") {
            assets.push_str(&format!("<script src=\"{path}\" defer></script>\n"));
        }
    }
    let person = person.map_or_else(String::new, |name| {
        format!(" data-person=\"{}\"", escape(name))
    });
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{site} · Console</title>\n{assets}</head><body>\n\
         <div id=\"pigeon-shell\" data-site=\"{site}\" data-base=\"{NEXT_BASE}\"{person}></div>\n\
         <noscript><p>The console needs JavaScript.</p></noscript>\n\
         </body></html>",
        site = escape(site_name),
    )
}

/// The app shell, with the signed-in person's display name if the session is fresh.
pub(super) fn app_shell(site_name: &str, person: Option<&str>) -> String {
    let mut assets = String::new();
    for (path, _, _) in APP_ASSETS
        .iter()
        .filter(|(path, _, _)| path.ends_with(".css"))
    {
        assets.push_str(&format!("<link rel=\"stylesheet\" href=\"{path}\">\n"));
    }
    assets.insert_str(
        0,
        "<link rel=\"stylesheet\" href=\"/console/console.css\">\n",
    );
    assets.push_str("<link rel=\"icon\" href=\"/console/icon.png\">\n");
    assets.push_str("<script src=\"/auth/passkey.js\" defer></script>\n");
    for (path, _, _) in APP_ASSETS
        .iter()
        .filter(|(path, _, _)| path.ends_with(".js"))
    {
        assets.push_str(&format!("<script src=\"{path}\" defer></script>\n"));
    }
    for panel in ["approvals", "instances", "keys", "users"] {
        assets.push_str(&format!(
            "<script src=\"/console/{panel}.js\" defer></script>\n"
        ));
    }
    let person = person.map_or_else(String::new, |name| {
        format!(" data-person=\"{}\"", escape(name))
    });
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta name=\"theme-color\" content=\"#141418\">\n\
         <title>{site} · App</title>\n{assets}</head><body>\n\
         <div id=\"pigeon-app\" data-site=\"{site}\"{person}></div>\n\
         <noscript><p>The app needs JavaScript.</p></noscript>\n\
         </body></html>",
        site = escape(site_name),
    )
}

/// The body and Content-Type of the app asset at `path`: only those registered in the page.
pub fn app_asset(path: &str) -> Option<(&'static str, &'static str)> {
    APP_ASSETS
        .iter()
        .chain(NEXT_ASSETS)
        .find(|(url, _, _)| *url == path)
        .map(|(_, kind, body)| (*body, *kind))
}

/// The shell as a response, under the console's policy and the pages' other headers.
pub fn html(page: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Content-Security-Policy", CSP)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::ok(page)?.with_headers(headers))
}

/// One of the console's scripts or its stylesheet as a response.
pub fn asset(body: &str, content_type: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", content_type)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::ok(body)?.with_headers(headers))
}

/// A redirect to `location`, a path on this host; a browser keeps the request's fragment, so
/// `/secrets#NAME` lands on [`CREDENTIALS`]`#NAME`.
pub fn redirect(location: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Location", location)?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::empty()?.with_status(302).with_headers(headers))
}

/// The console's icon as a response, cached for an hour.
pub fn image(body: &[u8], content_type: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", content_type)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Cache-Control", "public, max-age=3600")?;
    Ok(Response::from_bytes(body.to_vec())?.with_headers(headers))
}

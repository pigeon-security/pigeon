//! The console's HTML shell (`/app`), its assets, its response headers and the redirects from
//! old console bookmarks. The shell holds no content of its own, because its scripts build
//! everything; it carries no inline script and no inline event handler, so the policy can forbid
//! both. The console's static assets are served with the same care.

use pigeon_core::design::escape;
use worker::{Headers, Response, Result};

/// The page's policy: its own scripts, styles, images and calls; inline styles for the design
/// system; images also from `data:` URLs, for profile pictures.
pub const CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Where the console is served: its pages are `/app/<instance>/<area>/<view>`.
pub const BASE: &str = "/app";

/// The shell's assets, in load order: one stylesheet and its scripts. Its fixtures
/// (`web/fixtures/`, `web/fixtures.html`) are never among them, so fixture mode cannot run on a
/// deployed host.
pub const ASSETS: &[(&str, &str, &str)] = &[
    (
        "/app/shell.css",
        "text/css; charset=utf-8",
        include_str!("web/shell.css"),
    ),
    (
        "/app/shell.js",
        "text/javascript; charset=utf-8",
        include_str!("web/shell.js"),
    ),
    (
        "/app/views.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views.js"),
    ),
    (
        "/app/views/data.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/data.js"),
    ),
    (
        "/app/views/sessions.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/sessions.js"),
    ),
    (
        "/app/views/site.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/site.js"),
    ),
    (
        "/app/views/capabilities.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/capabilities.js"),
    ),
    (
        "/app/views/access.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/access.js"),
    ),
    (
        "/app/views/platform.js",
        "text/javascript; charset=utf-8",
        include_str!("web/views/platform.js"),
    ),
];

/// The body and Content-Type of the shell's asset at `path`: only those it loads.
pub fn asset_at(path: &str) -> Option<(&'static str, &'static str)> {
    ASSETS
        .iter()
        .find(|(url, _, _)| *url == path)
        .map(|(_, kind, body)| (*body, *kind))
}

/// Whether `path` is one of the console's pages, `/app` or below it, rather than an asset or
/// its op endpoint: no segment holds a dot.
pub fn is_page(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path == BASE
        || path
            .strip_prefix(BASE)
            .is_some_and(|rest| rest.starts_with('/') && !rest.contains('.') && rest != "/op")
}

/// Where an old console bookmark goes, in one hop (C7).
#[derive(Debug, PartialEq, Eq)]
pub enum Bookmark {
    /// The console's root, which opens the home instance's first area.
    Root,
    /// An area, and a view of it when the bookmark named one, of `instance`, or of the home
    /// instance when it names none.
    Area {
        instance: Option<String>,
        area: &'static str,
        view: Option<&'static str>,
    },
}

/// An old bookmark's path and the area and view it now opens. `/` opens the console's root.
pub const BOOKMARKS: &[(&str, &str, Option<&str>)] = &[
    ("/approvals", "inbox", None),
    ("/instances", "instances", Some("list")),
    ("/architecture", "platform", Some("architecture")),
    ("/access", "access", None),
    ("/settings", "access", None),
    ("/secrets", "access", Some("secrets")),
    ("/tokens", "access", Some("tokens")),
    ("/cost", "cost", None),
    ("/gateways", "cost", None),
];

/// The first console's `/app?tab=&subtab=` links: [tab, subtab or none, area, view or none].
/// The first match wins; a tab's row without a subtab matches any subtab not listed.
pub const OLD_TABS: &[(&str, Option<&str>, &str, Option<&str>)] = &[
    ("approvals", None, "inbox", None),
    ("instances", None, "instances", Some("list")),
    ("architecture", None, "platform", Some("architecture")),
    ("management", Some("credentials"), "access", Some("secrets")),
    ("management", Some("people"), "access", Some("people")),
    ("management", Some("delegation"), "access", Some("apps")),
    ("management", None, "access", Some("secrets")),
    ("cost", Some("7d"), "cost", Some("7d")),
    ("cost", Some("today"), "cost", Some("today")),
    ("cost", Some("all"), "cost", Some("all")),
    ("cost", None, "cost", None),
    ("sessions", Some("main"), "sessions", Some("live")),
    ("sessions", Some("compare"), "sessions", Some("compare")),
    ("sessions", None, "sessions", None),
    ("settings", Some("models"), "platform", Some("models")),
    (
        "settings",
        Some("connections"),
        "platform",
        Some("endpoints"),
    ),
    ("settings", Some("people"), "access", Some("people")),
    ("settings", Some("access"), "access", Some("secrets")),
    ("settings", Some("capabilities"), "capabilities", None),
    ("settings", Some("site"), "site", Some("sites")),
    ("settings", None, "overview", None),
    ("home", None, "overview", None),
];

/// Whether `id` may be carried from an old link into a console URL as an instance id.
fn instance_like(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'/')
}

/// Where the old bookmark at `path` with `query` goes; none for anything else, including the
/// console's own pages.
pub fn bookmark(path: &str, query: Option<&str>) -> Option<Bookmark> {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Some(Bookmark::Root);
    }
    if let Some((_, area, view)) = BOOKMARKS.iter().find(|(p, ..)| *p == path) {
        return Some(Bookmark::Area {
            instance: None,
            area,
            view: *view,
        });
    }
    if path != BASE {
        return None;
    }
    let parsed = worker::Url::parse(&format!("http://console/?{}", query?)).ok()?;
    let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    let get = |k: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    };
    let instance = get("instance")
        .filter(|i| instance_like(i))
        .map(str::to_string);
    let (area, view) = match get("tab") {
        Some(tab) => {
            let sub = get("subtab");
            OLD_TABS
                .iter()
                .find(|(t, s, ..)| *t == tab && s.is_some() && *s == sub)
                .or_else(|| OLD_TABS.iter().find(|(t, s, ..)| *t == tab && s.is_none()))
                .map_or(("overview", None), |(.., a, v)| (*a, *v))
        }
        None if instance.is_some() => ("overview", None),
        None => return None,
    };
    Some(Bookmark::Area {
        instance,
        area,
        view,
    })
}

/// The console's URL for `area`, and `view` when given, of instance `instance`. Each segment is
/// encoded as the shell's `encodeURIComponent` does, so an instance id such as
/// `pigeon-security/platform` stays one segment.
pub fn url(instance: &str, area: &str, view: Option<&str>) -> String {
    let (instance, area) = (segment(instance), segment(area));
    match view {
        Some(v) => format!("{BASE}/{instance}/{area}/{}", segment(v)),
        None => format!("{BASE}/{instance}/{area}"),
    }
}

/// `s` percent-encoded as one URL path segment, keeping what `encodeURIComponent` keeps.
pub fn segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The shell, with the signed-in person's display name if the session is fresh. It never
/// carries `data-fixtures`, so `?fixtures=1` does nothing on a deployed host.
pub fn shell(site_name: &str, person: Option<&str>) -> String {
    let mut assets = String::from("<link rel=\"icon\" href=\"/console/icon.png\">\n");
    for (path, kind, _) in ASSETS {
        if kind.starts_with("text/css") {
            assets.push_str(&format!("<link rel=\"stylesheet\" href=\"{path}\">\n"));
        }
    }
    assets.push_str("<script src=\"/auth/passkey.js\" defer></script>\n");
    for (path, kind, _) in ASSETS {
        if kind.starts_with("text/javascript") {
            assets.push_str(&format!("<script src=\"{path}\" defer></script>\n"));
        }
    }
    let person = person.map_or_else(String::new, |name| {
        format!(" data-person=\"{}\"", escape(name))
    });
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n\
         <title>{site} · Console</title>\n{assets}</head><body>\n\
         <div id=\"pigeon-shell\" data-site=\"{site}\" data-base=\"{BASE}\"{person}></div>\n\
         <noscript><p>The console needs JavaScript.</p></noscript>\n\
         </body></html>",
        site = escape(site_name),
    )
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
/// `/secrets#NAME` lands on Access › Secrets with `#NAME`.
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

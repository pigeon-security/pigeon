//! The console page's HTML shell and its response headers. The shell holds no content of its
//! own, because the console's scripts build everything; it carries no inline script and no
//! inline event handler, so the policy can forbid both. The console's static assets are served
//! with the same care.

use pigeon_core::design::{Page, escape};
use worker::{Headers, Response, Result};

/// The page's policy: its own scripts, styles, images and calls; inline styles for the design
/// system; images also from `data:` URLs, for profile pictures.
pub const CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The scripts the page loads, in order; `passkey.js` first, since the others use its helpers.
pub const SCRIPTS: [&str; 8] = [
    "/auth/passkey.js",
    "/console/app.js",
    "/console/chart.js",
    "/console/platform.js",
    "/console/keys.js",
    "/console/users.js",
    "/console/approvals.js",
    "/console/instances.js",
];

/// The console's stylesheet, on top of the design system's.
pub const STYLESHEET: &str = "/console/console.css";

/// The page shell for `site_name`, such as `pigeon`: its stylesheet, an empty `#console` the
/// scripts fill, a note for browsers without JavaScript, and the scripts, deferred.
pub fn shell(site_name: &str) -> String {
    let site = escape(site_name);
    let title = format!("{site_name} console");
    let mut body = format!("<link rel=\"stylesheet\" href=\"{STYLESHEET}\">\n");
    body.push_str(&format!(
        "<div id=\"console\" data-site=\"{site}\"></div>\n"
    ));
    body.push_str("<noscript><p>The console needs JavaScript to run.</p></noscript>\n");
    for script in SCRIPTS {
        body.push_str(&format!("<script src=\"{script}\" defer></script>\n"));
    }
    Page {
        title: &title,
        description: "The platform's console",
        body: &body,
    }
    .render()
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
/// `/secrets#NAME` lands on `/access#NAME`.
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

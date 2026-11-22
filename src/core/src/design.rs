//! pigeon's design system for web deployments: tokens, base styles and components as CSS, and a
//! page shell that inlines them. Pure: no Cloudflare calls, no token.
//!
//! Components, by class: `eyebrow`, `card` (`card-h`, `.accent`), `pill` (`pills`, `.accent`),
//! `dot` with `kind-worker|access|store|api`, `region` (`.cloudflare`, `.github`,
//! `region-name`), `layer` and `cards`, `arrow` (label in a `<span>`), `kv`, `chain`, `legend`.

/// The tokens as Swift source for the iOS app.
pub mod swift;

/// Colours, radii, spacing and type, dark first, light by `prefers-color-scheme` or
/// `[data-theme]`.
pub const TOKENS: &str = include_str!("design/tokens.css");

/// Plain elements: page, type, links, code.
pub const BASE: &str = include_str!("design/base.css");

/// The components, by class.
pub const COMPONENTS: &str = include_str!("design/components.css");

/// The page background, for `theme-color`.
pub const BACKGROUND: &str = "#14121A";

/// The whole stylesheet: tokens, base, components.
pub fn stylesheet() -> String {
    [TOKENS, BASE, COMPONENTS].concat()
}

/// The Content-Security-Policy a page from [`Page::render`] needs: inline styles, nothing else.
pub const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// A page: its title, description and body HTML, which the caller escapes.
pub struct Page<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub body: &'a str,
}

impl Page<'_> {
    /// The page as a whole HTML document, its stylesheet inline.
    pub fn render(&self) -> String {
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\n\
             <meta name=\"theme-color\" content=\"{BACKGROUND}\">\n\
             <title>{}</title>\n<meta name=\"description\" content=\"{}\">\n\
             <style>{}</style>\n</head>\n<body>\n{}\n</body>\n</html>\n",
            escape(self.title),
            escape(self.description),
            stylesheet(),
            self.body,
        )
    }
}

/// `s` escaped for HTML text and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

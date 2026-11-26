//! The pages, as Rust functions: the frame (head, top bar, footer), the home page from
//! [`Index`], the plain pages from Markdown, and the 404 page. Every text is escaped, every link
//! was checked when the content was parsed, and nothing here writes a script or an event
//! handler.

use pulldown_cmark::{Event, Options, Parser, Tag, html};

use super::content::{Action, Example, Hero, Index, Page, Pill, Section};
use super::site::Settings;
use super::{CSS_PATH, esc};

/// What every page reads.
pub struct Site<'a> {
    /// The settings.
    pub settings: &'a Settings,
    /// The deployed commit, short, for the footer.
    pub commit: &'a str,
}

/// The arrow after a link that leaves the site.
const OUT: &str = r#" <span aria-hidden="true">↗</span>"#;

fn outward(link: &str) -> bool {
    link.starts_with("https://") || link.starts_with("http://")
}

/// The frame around `content`. `current` is the site path the page answers, for the top bar's
/// `aria-current`.
fn frame(
    site: &Site<'_>,
    title: &str,
    description: &str,
    current: Option<&str>,
    content: &str,
) -> String {
    let s = site.settings;
    let nav: String = s
        .nav
        .iter()
        .map(|l| {
            let here = if current == Some(l.link.as_str()) {
                " aria-current=\"page\""
            } else {
                ""
            };
            format!("<a href=\"{}\"{here}>{}</a>", esc(&l.link), esc(&l.title))
        })
        .collect();
    let links: String = s
        .links
        .iter()
        .map(|l| format!("<a href=\"{}\">{}</a>", esc(&l.link), esc(&l.title)))
        .collect();
    let console = format!(
        "<a class=\"btn btn-primary\" href=\"{}\">{}{}</a>",
        esc(&s.console_url),
        esc(&s.console_label),
        if outward(&s.console_url) { OUT } else { "" }
    );
    let stamp = if site.commit.is_empty() {
        "Local build".to_string()
    } else {
        esc(site.commit)
    };
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="theme-color" content="#FFFFFF" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#141418" media="(prefers-color-scheme: dark)">
<meta name="description" content="{description}">
<title>{title}</title>
<link rel="icon" type="image/png" sizes="32x32" href="/favicon-32.png">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<link rel="stylesheet" href="/{CSS_PATH}"></head><body>
<a class="skip" href="#main">Skip to content</a>
<header class="top"><div class="wrap">
<a class="brand" href="/">{brand}</a>
<nav class="nav" aria-label="Site">{nav}</nav>
<span class="spacer"></span>
{console}
</div></header>
<main id="main"><div class="wrap">{content}</div></main>
<footer class="foot"><div class="wrap">
<span>{stamp}</span>
<nav aria-label="Footer">{links}</nav>
</div></footer>
</body></html>"##,
        description = esc(description),
        title = esc(title),
        brand = esc(&s.title),
    )
}

fn pill(p: &Pill) -> String {
    let class = match p.tone.as_deref() {
        Some(t) => format!("pill {t}"),
        None => "pill".into(),
    };
    format!("<span class=\"{class}\">{}</span>", esc(&p.text))
}

fn button(a: &Action, large: bool) -> String {
    let mut class = String::from("btn");
    if let Some(s) = &a.style {
        class.push_str(" btn-");
        class.push_str(s);
    }
    if large {
        class.push_str(" btn-lg");
    }
    let text = if a.mono {
        format!("<span class=\"mono\">{}</span>", esc(&a.title))
    } else {
        esc(&a.title)
    };
    let out = if outward(&a.link) { OUT } else { "" };
    format!(
        "<a class=\"{class}\" href=\"{}\">{text}{out}</a>",
        esc(&a.link)
    )
}

fn hero(h: &Hero) -> String {
    let mut out = String::from(
        "<div class=\"hero-copy\">\n<span class=\"mark\" aria-hidden=\"true\"></span>\n",
    );
    if let Some(p) = &h.pill {
        out.push_str(&format!("<span>{}</span>\n", pill(p)));
    }
    out.push_str(&format!(
        "<h1>{}</h1>\n<p class=\"lede\">{}</p>\n",
        esc(&h.title),
        esc(&h.lede)
    ));
    if !h.actions.is_empty() {
        out.push_str("<div class=\"actions\">\n");
        for a in &h.actions {
            out.push_str(&button(a, true));
            out.push('\n');
        }
        out.push_str("</div>\n");
    }
    if let Some(f) = &h.fine {
        out.push_str(&format!("<p class=\"fine\">{}</p>\n", esc(f)));
    }
    out.push_str("</div>\n");
    out
}

fn example(e: &Example) -> String {
    let mut out = format!(
        "<figure class=\"panel record\" aria-label=\"{}\">\n<div class=\"record-head\">\n<span class=\"t\">{}</span>\n",
        esc(&e.label),
        esc(&e.title)
    );
    if let Some(p) = &e.pill {
        out.push_str(&pill(p));
        out.push('\n');
    }
    if let Some(t) = &e.tag {
        out.push_str(&format!("<span class=\"tag\">{}</span>\n", esc(t)));
    }
    out.push_str("</div>\n");
    if !e.facts.is_empty() {
        out.push_str("<dl class=\"record-sum\">\n");
        for f in &e.facts {
            out.push_str(&format!(
                "<div><dt>{}</dt><dd>{}</dd></div>\n",
                esc(&f.term),
                esc(&f.value)
            ));
        }
        out.push_str("</dl>\n");
    }
    let [date, what, source, amount] = &e.columns;
    out.push_str(&format!(
        "<div class=\"tbl-wrap\">\n<table class=\"tbl\">\n<thead><tr><th>{}</th><th>{}</th><th class=\"hide-sm\">{}</th><th class=\"num\">{}</th></tr></thead>\n<tbody>\n",
        esc(date),
        esc(what),
        esc(source),
        esc(amount)
    ));
    for r in &e.rows {
        let open = if r.highlight {
            "<tr class=\"hit\">"
        } else {
            "<tr>"
        };
        let detail = r
            .detail
            .as_ref()
            .map(|d| format!("<span class=\"sub\">{}</span>", esc(d)))
            .unwrap_or_default();
        let amount = match &r.amount {
            Some(a) => format!("<td class=\"num\">{}</td>", esc(a)),
            None => "<td class=\"num muted\">—</td>".into(),
        };
        out.push_str(&format!(
            "{open}<td class=\"mono\">{}</td><td>{}{detail}</td><td class=\"hide-sm\"><span class=\"tag\">{}</span></td>{amount}</tr>\n",
            esc(&r.date),
            esc(&r.what),
            esc(&r.source)
        ));
    }
    out.push_str("</tbody>\n</table>\n</div>\n");
    if e.foot.is_some() || e.count.is_some() {
        out.push_str("<div class=\"record-foot\">\n");
        if let Some(f) = &e.foot {
            out.push_str(&format!("<span>{}</span>\n", esc(f)));
        }
        if let Some(c) = &e.count {
            out.push_str(&format!("<span class=\"mono\">{}</span>\n", esc(c)));
        }
        out.push_str("</div>\n");
    }
    out.push_str("</figure>\n");
    out
}

fn section(s: &Section) -> String {
    let h = esc(&s.heading());
    let mut out = format!(
        "<section class=\"block\" id=\"{}\" aria-labelledby=\"{h}\">\n<header><span class=\"label\">{}</span><h2 id=\"{h}\">{}</h2></header>\n",
        esc(&s.id),
        esc(&s.label),
        esc(&s.title)
    );
    if !s.steps.is_empty() {
        out.push_str("<ol class=\"steps\">\n");
        for step in &s.steps {
            out.push_str(&format!(
                "<li><h3>{}</h3><p>{}</p></li>\n",
                esc(&step.title),
                esc(&step.text)
            ));
        }
        out.push_str("</ol>\n");
    } else if !s.rows.is_empty() {
        out.push_str("<div class=\"tbl-wrap\">\n<table class=\"kv\">\n<tbody>\n");
        for r in &s.rows {
            out.push_str(&format!(
                "<tr><th>{}</th><td>{}</td><td class=\"st\">{}</td></tr>\n",
                esc(&r.name),
                esc(&r.text),
                pill(&r.status)
            ));
        }
        out.push_str("</tbody>\n</table>\n</div>\n");
    } else {
        out.push_str("<div class=\"stack\">\n");
        if let Some(t) = &s.text {
            out.push_str(&format!("<p class=\"hood\">{}</p>\n", esc(t)));
        }
        if !s.links.is_empty() {
            let links: Vec<String> = s.links.iter().map(|a| button(a, false)).collect();
            out.push_str(&format!("<p class=\"hood\">{}</p>\n", links.join(" ")));
        }
        out.push_str("</div>\n");
    }
    out.push_str("</section>");
    out
}

/// The home page.
pub fn index(site: &Site<'_>, index: &Index) -> String {
    let mut content = String::from("<div class=\"hero\">\n");
    content.push_str(&hero(&index.hero));
    if let Some(e) = &index.example {
        content.push_str(&example(e));
    }
    content.push_str("</div>");
    for s in &index.sections {
        content.push('\n');
        content.push_str(&section(s));
    }
    let s = site.settings;
    frame(site, &s.title, &s.description, Some("/"), &content)
}

fn head(label: &str, title: &str) -> String {
    format!(
        "<div class=\"page-head\"><span class=\"label\">{}</span><h1>{}</h1></div>",
        esc(label),
        esc(title)
    )
}

/// A page from `content/pages/`.
pub fn page(site: &Site<'_>, page: &Page) -> String {
    let s = site.settings;
    let content = format!(
        "{}<div class=\"stack prose\">{}</div>",
        head(&s.title, &page.title),
        markdown(&page.body)
    );
    let path = format!("/{}/", page.slug);
    frame(
        site,
        &format!("{} · {}", page.title, s.title),
        page.description.as_deref().unwrap_or(&s.description),
        Some(&path),
        &content,
    )
}

/// The 404 page.
pub fn not_found(site: &Site<'_>) -> String {
    let s = site.settings;
    let content = format!(
        "{}<p class=\"notice\">There is nothing at this address. <a href=\"/\">Go to the overview</a>.</p>",
        head("Not found", "Page not found")
    );
    frame(
        site,
        &format!("Not found · {}", s.title),
        &s.description,
        None,
        &content,
    )
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// Markdown as HTML, with raw HTML shown as text: a page can never carry markup of its own.
pub fn markdown(text: &str) -> String {
    let events = Parser::new_ext(text, options()).map(|e| match e {
        Event::Html(t) | Event::InlineHtml(t) => Event::Text(t),
        e => e,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// Every link and image destination in `text`, for checking.
pub fn links(text: &str) -> Vec<String> {
    Parser::new_ext(text, options())
        .filter_map(|e| match e {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                Some(dest_url.to_string())
            }
            _ => None,
        })
        .collect()
}

//! Markdown as Zola 0.23.6 renders it, with highlighting off: pulldown-cmark with tables,
//! footnotes, strikethrough, task lists and heading attributes, no smart punctuation. Headings get
//! an `id`, slugified, with `-1`, `-2` for repeats; fenced code is `<pre><code data-lang="x">`;
//! images are `<img src alt />`. Raw HTML passes through.
//!
//! One departure, so a page works on every host it is served from: a link to `#x` becomes
//! `{path}#x` (root-relative), where Zola writes the absolute permalink.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd, html};
use pulldown_cmark_escape::{escape_href, escape_html};

/// Lowercase ASCII letters and digits, every other run as one `-`, none at the ends: Zola's
/// slugs for heading ids and tags. (Zola also transliterates non-ASCII letters; this drops them.)
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c);
            dash = false;
        } else {
            dash = true;
        }
    }
    out
}

fn options() -> Options {
    let mut o = Options::empty();
    o.insert(Options::ENABLE_TABLES);
    o.insert(Options::ENABLE_FOOTNOTES);
    o.insert(Options::ENABLE_STRIKETHROUGH);
    o.insert(Options::ENABLE_TASKLISTS);
    o.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    o
}

fn escaped(s: &str) -> String {
    let mut out = String::new();
    let _ = escape_html(&mut out, s);
    out
}

fn href(s: &str) -> String {
    let mut out = String::new();
    let _ = escape_href(&mut out, s);
    out
}

/// Renders a page's Markdown body. `path` is the page's site path, for `#anchor` links.
pub fn render(text: &str, path: &str) -> String {
    let events: Vec<Event> = Parser::new_ext(text, options()).collect();
    let mut anchors: Vec<String> = Vec::new();
    let mut out: Vec<Event> = Vec::with_capacity(events.len());
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::Heading {
                level, id, classes, ..
            }) => {
                let mut j = i + 1;
                let mut text = String::new();
                while !matches!(events[j], Event::End(TagEnd::Heading(_))) {
                    if let Event::Text(t) | Event::Code(t) = &events[j] {
                        text.push_str(t);
                    }
                    j += 1;
                }
                let id = match id {
                    Some(id) => id.to_string(),
                    None => {
                        let base = slugify(&text);
                        let mut slug = base.clone();
                        let mut n = 1;
                        while anchors.contains(&slug) {
                            slug = format!("{base}-{n}");
                            n += 1;
                        }
                        slug
                    }
                };
                anchors.push(id.clone());
                let n = *level as u8;
                let class = if classes.is_empty() {
                    String::new()
                } else {
                    let c: Vec<String> = classes.iter().map(|c| escaped(c)).collect();
                    format!(" class=\"{}\"", c.join(" "))
                };
                out.push(Event::Html(CowStr::from(format!(
                    "<h{n} id=\"{}\"{class}>",
                    escaped(&id)
                ))));
                out.extend(events[i + 1..j].iter().cloned());
                out.push(Event::Html(CowStr::from(format!("</h{n}>\n"))));
                i = j + 1;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split([' ', ',', '{'])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                let mut j = i + 1;
                let mut code = String::new();
                while !matches!(events[j], Event::End(TagEnd::CodeBlock)) {
                    if let Event::Text(t) = &events[j] {
                        code.push_str(t);
                    }
                    j += 1;
                }
                let attr = if lang.is_empty() || lang == "plain" || lang == "text" || lang == "txt"
                {
                    String::new()
                } else {
                    format!(" data-lang=\"{lang}\"")
                };
                out.push(Event::Html(CowStr::from(format!(
                    "<pre><code{attr}>{}</code></pre>\n",
                    escaped(&code)
                ))));
                i = j + 1;
            }
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                let mut j = i + 1;
                let mut alt = String::new();
                let mut depth = 0;
                loop {
                    match &events[j] {
                        Event::End(TagEnd::Image) if depth == 0 => break,
                        Event::Start(Tag::Image { .. }) => depth += 1,
                        Event::End(TagEnd::Image) => depth -= 1,
                        Event::Text(t) | Event::Code(t) => alt.push_str(t),
                        _ => {}
                    }
                    j += 1;
                }
                let title = if title.is_empty() {
                    String::new()
                } else {
                    format!(" title=\"{}\"", escaped(title))
                };
                out.push(Event::Html(CowStr::from(format!(
                    "<img src=\"{}\" alt=\"{}\"{title} />",
                    href(dest_url),
                    escaped(&alt)
                ))));
                i = j + 1;
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) if dest_url.starts_with('#') && dest_url.len() > 1 => {
                out.push(Event::Start(Tag::Link {
                    link_type: *link_type,
                    dest_url: CowStr::from(format!("{path}{dest_url}")),
                    title: title.clone(),
                    id: id.clone(),
                }));
                i += 1;
            }
            e => {
                out.push(e.clone());
                i += 1;
            }
        }
    }
    let mut s = String::new();
    html::push_html(&mut s, out.into_iter());
    s
}

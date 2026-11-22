//! The feed, sitemap, robots.txt and manifest of `blog@1`, as Zola writes them for the upstream
//! theme (`templates/posts_feed.xml`, `templates/robots.txt`, Zola's built-in sitemap, the
//! scaffold's `manifest.json`). One departure: the feed's generator names pigeon.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Date, Site, esc};

/// `/posts/posts_feed.xml`: Atom, every post, newest first.
pub fn atom(site: &Site) -> String {
    let s = &site.settings;
    let feed_url = format!("{}/posts/posts_feed.xml", site.base_url);
    let latest = site
        .posts
        .iter()
        .filter_map(|p| p.updated.as_ref().or(p.date.as_ref()))
        .max_by_key(|d| d.instant())
        .map_or_else(|| "1970-01-01T00:00:00+00:00".to_string(), Date::iso);
    let mut o = String::new();
    o.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    o.push_str("<feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"en\">\n");
    o.push_str(&format!("    <title>{}</title>", esc(&s.title)));
    if !s.description.is_empty() {
        o.push_str(&format!(
            "\n    <subtitle>{}</subtitle>",
            esc(&s.description)
        ));
    }
    o.push_str(&format!(
        "\n    <link rel=\"self\" type=\"application/atom+xml\" href=\"{feed_url}\"/>\n    <link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n    <generator uri=\"https://pigeon.dev/\">pigeon blog@1</generator>\n    <updated>{}</updated>\n    <id>{feed_url}</id>",
        site.base_url,
        esc(&latest)
    ));
    for p in &site.posts {
        let permalink = format!("{}{}", site.base_url, p.path);
        let published = p.date.as_ref().map(Date::iso).unwrap_or_default();
        let updated = p
            .updated
            .as_ref()
            .map_or_else(|| published.clone(), Date::iso);
        o.push_str(&format!(
            "\n    <entry xml:lang=\"en\">\n        <title>{}</title>\n        <published>{}</published>\n        <updated>{}</updated>\n        <link rel=\"alternate\" type=\"text/html\" href=\"{permalink}\"/>\n        <id>{permalink}</id>",
            esc(&p.title),
            esc(&published),
            esc(&updated)
        ));
        if let Some(d) = p.description.as_ref().filter(|d| !d.is_empty()) {
            o.push_str(&format!("\n        <summary>{}</summary>", esc(d)));
        }
        o.push_str("\n    </entry>");
    }
    o.push_str("\n</feed>\n");
    o
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `/sitemap.xml`: every post and page (with `lastmod`), the home page and the tags, by URL.
pub fn sitemap(site: &Site) -> String {
    let mut entries: BTreeMap<String, Option<String>> = BTreeMap::new();
    for e in site.posts.iter().chain(&site.pages) {
        let lastmod = e.updated.as_ref().or(e.date.as_ref()).map(Date::lastmod);
        entries.insert(format!("{}{}", site.base_url, e.path), lastmod);
    }
    entries.insert(format!("{}/", site.base_url), None);
    entries.insert(format!("{}/tags/", site.base_url), None);
    for t in &site.tags {
        entries.insert(format!("{}/tags/{}/", site.base_url, t.slug), None);
    }
    let mut o = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">",
    );
    for (loc, lastmod) in &entries {
        o.push_str(&format!(
            "\n    <url>\n        <loc>{}</loc>",
            escape_xml(loc)
        ));
        if let Some(m) = lastmod {
            o.push_str(&format!("\n        <lastmod>{}</lastmod>", esc(m)));
        }
        o.push_str("\n    </url>");
    }
    o.push_str("\n</urlset>\n");
    o
}

/// `/robots.txt`.
pub fn robots(site: &Site) -> String {
    format!(
        "User-agent: *\nAllow: /\n\nSitemap: {}/sitemap.xml\n",
        site.base_url
    )
}

/// `/manifest.json`, its keys in order.
pub fn manifest(site: &Site) -> String {
    let s = &site.settings;
    let mut m = Map::new();
    m.insert("start_url".into(), "/".into());
    m.insert("display".into(), "standalone".into());
    m.insert("theme_color".into(), s.theme_color.light.clone().into());
    m.insert(
        "background_color".into(),
        s.theme_color.light.clone().into(),
    );
    m.insert("name".into(), s.title.clone().into());
    m.insert("description".into(), s.description.clone().into());
    if let Some(short) = &s.short_name {
        m.insert("short_name".into(), short.clone().into());
    }
    if !s.icons.is_empty() {
        let icons = s
            .icons
            .iter()
            .map(|i| {
                let mut icon = Map::new();
                icon.insert("src".into(), i.src.clone().into());
                icon.insert("sizes".into(), i.sizes.clone().into());
                icon.insert("type".into(), i.kind.clone().into());
                Value::Object(icon)
            })
            .collect();
        m.insert("icons".into(), Value::Array(icons));
    }
    let mut out = serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default();
    out.push('\n');
    out
}

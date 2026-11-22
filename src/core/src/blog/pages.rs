//! The HTML pages of `blog@1`, transliterated from upstream `templates/` (`base.html`,
//! `header.html`, `index.html`, `post.html`, `page.html`, `tags/list.html`, `tags/single.html`)
//! and Zola's built-in alias and 404 pages. Text is escaped as Tera escapes it ([`esc`]).
//!
//! One departure: links to the site's own pages are root-relative (`/posts/x/`), where the tag
//! templates and the alias page used Zola's absolute permalinks, so a page works on every host
//! that serves it. Canonical, Open Graph and JSON-LD URLs stay absolute.

use super::{Date, Site, Tag, content::Entry, esc, json, markdown};

/// The blocks a page fills in `base.html`.
struct Blocks<'a> {
    title: String,
    description: String,
    canonical: String,
    og_type: &'a str,
    og_url: String,
    og_title: String,
    og_description: String,
    og_image: String,
    og_image_extra: bool,
    twitter_title: String,
    twitter_description: String,
    twitter_image: String,
    body_class: &'a str,
    extra_head: String,
    content: String,
    is_home: bool,
}

impl<'a> Blocks<'a> {
    /// base.html's defaults, with a page's title, description and canonical URL. Every block
    /// holds escaped text.
    fn new(
        site: &Site,
        title: &str,
        description: &str,
        canonical: &str,
        body_class: &'a str,
    ) -> Self {
        let preview = esc(&format!(
            "{}{}",
            site.base_url,
            site.settings.social_preview.as_deref().unwrap_or("")
        ));
        let (title, description, canonical) = (esc(title), esc(description), esc(canonical));
        Blocks {
            title: title.clone(),
            description: description.clone(),
            canonical: canonical.clone(),
            og_type: "website",
            og_url: canonical,
            og_title: title.clone(),
            og_description: description.clone(),
            og_image: preview.clone(),
            og_image_extra: true,
            twitter_title: title,
            twitter_description: description,
            twitter_image: preview,
            body_class,
            extra_head: String::new(),
            content: String::new(),
            is_home: false,
        }
    }
}

/// base.html with header.html.
fn shell(site: &Site, b: &Blocks) -> String {
    let s = &site.settings;
    let f = &site.features;
    let title = esc(&s.title);
    let mut o = String::with_capacity(8192 + b.content.len());
    o.push_str("<!DOCTYPE html>\n<html lang=\"en-US\" dir=\"auto\">\n<head>\n");
    o.push_str("  <meta charset=\"utf-8\">\n");
    o.push_str("  <meta name=\"viewport\" content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\n");
    o.push_str(&format!("  <title>{}</title>\n", b.title));
    o.push_str("  <meta name=\"color-scheme\" content=\"light dark\">\n");
    o.push_str(&format!(
        "  <meta name=\"description\" content=\"{}\">\n\n",
        b.description
    ));
    o.push_str(&format!(
        "  <link rel=\"canonical\" href=\"{}\">\n\n",
        b.canonical
    ));
    o.push_str("  <link rel=\"icon\" href=\"/assets/images/site/favicon-32x32.png\" sizes=\"32x32\" type=\"image/png\">\n");
    o.push_str("  <link rel=\"icon\" href=\"/assets/images/site/favicon-16x16.png\" sizes=\"16x16\" type=\"image/png\">\n");
    o.push_str(
        "  <link rel=\"apple-touch-icon\" href=\"/assets/images/site/apple-touch-icon.png\">\n",
    );
    o.push_str("  <link rel=\"manifest\" href=\"/manifest.json\">\n\n");
    o.push_str(&format!(
        "  <meta property=\"og:site_name\" content=\"{title}\">\n"
    ));
    o.push_str(&format!(
        "  <meta property=\"og:type\" content=\"{}\">\n",
        b.og_type
    ));
    o.push_str(&format!(
        "  <meta property=\"og:url\" content=\"{}\">\n",
        b.og_url
    ));
    o.push_str(&format!(
        "  <meta property=\"og:title\" content=\"{}\">\n",
        b.og_title
    ));
    o.push_str(&format!(
        "  <meta property=\"og:description\" content=\"{}\">\n",
        b.og_description
    ));
    o.push_str(&format!(
        "  <meta property=\"og:image\" content=\"{}\">\n",
        b.og_image
    ));
    if b.og_image_extra {
        o.push_str(&format!(
            "  \n  <meta property=\"og:image:width\" content=\"{}\">\n  <meta property=\"og:image:height\" content=\"{}\">\n  \n\n",
            s.og_image_size.width, s.og_image_size.height
        ));
    } else {
        o.push_str("  \n\n");
    }
    o.push_str("  <meta name=\"twitter:card\" content=\"summary_large_image\">\n");
    o.push_str(&format!(
        "  <meta name=\"twitter:title\" content=\"{}\">\n",
        b.twitter_title
    ));
    o.push_str(&format!(
        "  <meta name=\"twitter:description\" content=\"{}\">\n",
        b.twitter_description
    ));
    o.push_str(&format!(
        "  <meta name=\"twitter:image\" content=\"{}\">",
        b.twitter_image
    ));
    if f.feed {
        o.push_str(&format!(
            "\n  <link rel=\"alternate\" type=\"application/atom+xml\" title=\"{title}\" href=\"/posts/posts_feed.xml\" />"
        ));
    }
    o.push_str("\n\n  <link rel=\"stylesheet\" href=\"/assets/css/main.css\">\n");
    o.push_str("  <link rel=\"stylesheet\" href=\"/assets/css/tokens.css\">\n");
    o.push_str("  <link rel=\"stylesheet\" href=\"/assets/css/pigeon.css\">");
    if f.math {
        o.push_str("\n  <link rel=\"stylesheet\" href=\"https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/katex.min.css\" integrity=\"sha384-5TcZemv2l/9On385z///+d7MSYlvIEw9FuZTIdZ14vJLqWphw7e7ZPuOiCHJcFCP\" crossorigin=\"anonymous\">");
    }
    if f.fonts {
        o.push_str(
            "\n\n  <link rel=\"preconnect\" href=\"https://fonts.bunny.net\" crossorigin>\n",
        );
        o.push_str(&format!(
            "  <link rel=\"stylesheet\" href=\"https://fonts.bunny.net/css?family={}&display=swap\">",
            esc(&s.look.font_family)
        ));
    }
    o.push_str(&format!(
        "\n\n  <meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"{}\">\n",
        esc(&s.theme_color.light)
    ));
    o.push_str(&format!(
        "  <meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"{}\">\n\n",
        esc(&s.theme_color.dark)
    ));
    o.push_str(&format!("  {}\n</head>\n", b.extra_head));
    o.push_str(&format!(
        "<body class=\"site-view {} color-theme-{} font-theme-{}\">\n",
        b.body_class,
        esc(&s.look.color),
        esc(&s.look.font)
    ));
    o.push_str("  <a class=\"skip-link\" href=\"#main\">Skip to content</a>");
    if let Some(banner) = s
        .banner
        .as_ref()
        .filter(|b| f.banner && !b.message.is_empty())
    {
        o.push_str("\n  <aside class=\"site-banner\" aria-label=\"Site notice\">\n    <p>");
        o.push_str(&esc(&banner.message));
        if let Some(link) = banner.link.as_ref().filter(|l| !l.is_empty()) {
            o.push_str(&format!(
                " <a href=\"{}\">{}</a>",
                esc(link),
                esc(banner.link_text.as_deref().unwrap_or(""))
            ));
        }
        o.push_str("</p>\n  </aside>");
    }
    o.push_str("\n\n  ");
    o.push_str(&header(site, b.is_home));
    o.push_str("\n\n  <main id=\"main\" class=\"site-main\">\n    ");
    o.push_str(&b.content);
    o.push_str("\n  </main>\n\n  <footer class=\"footer\">\n    ");
    o.push_str(&format!("{title} {}", site.commit_year));
    if f.commit {
        let sha = if site.commit.is_empty() {
            "dev"
        } else {
            &site.commit
        };
        o.push_str(&format!(" * {}", esc(sha)));
    }
    if f.changelog {
        o.push_str(" * <a href=\"/changelog\">Changelog</a>");
    }
    o.push_str("\n  </footer>");
    if f.math {
        o.push_str("\n  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/katex.min.js\" integrity=\"sha384-cMkvdD8LoxVzGF/RPUKAcvmm49FQ0oxwDF3BGKtDXcEc+T1b2N+teh/OJfpU0jr6\" crossorigin=\"anonymous\"></script>\n");
        o.push_str("  <script defer src=\"https://cdn.jsdelivr.net/npm/katex@0.16.22/dist/contrib/auto-render.min.js\" integrity=\"sha384-hCXGrW6PitJEwbkoStFjeJxv+fSOOQKOPbJxSfM6G5sWZjAyWhXiTIIAmQqnlLlh\" crossorigin=\"anonymous\"\n");
        o.push_str("    onload=\"renderMathInElement(document.body, {delimiters: [\n");
        o.push_str("      {left: '\\\\[', right: '\\\\]', display: true},\n");
        o.push_str("      {left: '\\\\(', right: '\\\\)', display: false}\n");
        o.push_str("    ]});\"></script>");
    }
    o.push_str("\n</body>\n</html>\n");
    o
}

/// header.html.
fn header(site: &Site, is_home: bool) -> String {
    let s = &site.settings;
    let title = esc(&s.title);
    let name = if is_home {
        format!("<h1 class=\"m-0\"><a class=\"p-name u-url\" href=\"/\">{title}</a></h1>")
    } else {
        format!("<a class=\"p-name u-url\" href=\"/\">{title}</a>")
    };
    let mut nav = String::new();
    for item in &s.nav {
        nav.push_str(&format!(
            "\n          <li><a class=\"site-nav-link\" href=\"{}\">{}</a></li>\n        ",
            esc(&item.link),
            esc(&item.title)
        ));
    }
    format!(
        "<header class=\"site-header h-card\">\n  <a class=\"site-header-avatar-link\" title=\"{title}\" href=\"/\">\n    <img alt=\"{title}\" class=\"avatar u-photo\" aria-hidden=\"true\" src=\"{}\" width=\"72\" height=\"72\" />\n  </a>\n  <div>\n    <div class=\"site-title m-0\">{name}</div>\n    <nav class=\"site-nav\">\n      <ul>\n        {nav}\n      </ul>\n    </nav>\n  </div>\n</header>\n\n<hr class=\"site-hr-below-site-header mt-L\">",
        esc(s.avatar.as_deref().unwrap_or(""))
    )
}

fn iso(d: Option<&Date>) -> String {
    d.map(Date::iso).unwrap_or_default()
}

/// The year-grouped list of posts on the home and tag pages.
fn post_list(posts: &[&Entry]) -> String {
    let mut o = String::new();
    let mut year = String::new();
    for post in posts {
        let y = post
            .date
            .as_ref()
            .map(|d| d.year().to_string())
            .unwrap_or_default();
        if y != year {
            year = y;
            o.push_str(&format!(
                "\n        \n          \n          <li class=\"post-list-year-h2\">{}</li>\n        \n        ",
                esc(&year)
            ));
        } else {
            o.push_str("\n        \n        ");
        }
        let when = esc(&iso(post.date.as_ref()));
        o.push_str(&format!(
            "<li class=\"h-entry\">\n          <time class=\"dt-published\" datetime=\"{when}\" title=\"{when}\">\n            {}\n          </time>\n          <span class=\"t-post post-link\"><a class=\"p-name u-url\" href=\"{}\">{}</a></span>\n        </li>\n      ",
            esc(&post.date.as_ref().map(Date::month_day).unwrap_or_default()),
            esc(&post.path),
            esc(&post.title)
        ));
    }
    o
}

/// The home page: the intro, then every post by year.
pub fn index(site: &Site) -> String {
    let s = &site.settings;
    let home = format!("{}/", site.base_url);
    let title = format!("{} — {}", s.title, s.description);
    let mut b = Blocks::new(
        site,
        &title,
        &s.description,
        &home,
        "site-page-body site-home-page",
    );
    b.is_home = true;
    let person = format!("{}/#person", site.base_url);
    let alternate = if s.alternate_names.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = s.alternate_names.iter().map(|n| json(n)).collect();
        format!("\n  \"alternateName\": [{}],", names.join(","))
    };
    let same_as: Vec<String> = s
        .nav
        .iter()
        .filter(|i| i.link.starts_with("http"))
        .map(|i| json(&i.link))
        .collect();
    b.extra_head = format!(
        "\n<script type=\"application/ld+json\">\n{{\n  \"@context\": \"https://schema.org\",\n  \"@type\": \"WebSite\",\n  \"name\": {},\n  \"url\": {},\n  \"description\": {},\n  \"author\": {{ \"@id\": {} }}\n}}\n</script>\n<script type=\"application/ld+json\">\n{{\n  \"@context\": \"https://schema.org\",\n  \"@type\": \"Person\",\n  \"@id\": {},\n  \"name\": {},\n  \"url\": {},\n  \"image\": {},{alternate}\n  \"sameAs\": [{}]\n}}\n</script>\n",
        json(&s.title),
        json(&home),
        json(&s.description),
        json(&person),
        json(&person),
        json(s.author()),
        json(&home),
        json(&format!(
            "{}{}",
            site.base_url,
            s.avatar.as_deref().unwrap_or("")
        )),
        same_as.join(", ")
    );
    let intro = s
        .intro
        .as_deref()
        .filter(|i| !i.is_empty())
        .map(|i| markdown::render(i, "/"))
        .unwrap_or_default();
    let posts: Vec<&Entry> = site.posts.iter().collect();
    b.content = format!(
        "\n<div class=\"site-page zoomable\">\n  <div class=\"trix-content e-content\">\n    {intro}\n\n    <ul class=\"t-list-of-posts site-list-of-posts h-feed\">\n      \n      \n      {}\n    </ul>\n  </div>\n</div>\n",
        post_list(&posts)
    );
    shell(site, &b)
}

/// The byline's author, linked to the author page when there is one.
fn byline(site: &Site) -> String {
    let name = esc(site.settings.author());
    match &site.settings.author_page {
        Some(page) => format!(
            "<a class=\"p-author h-card\" href=\"{}\">{name}</a>",
            esc(page)
        ),
        None => format!("<span class=\"p-author h-card\">{name}</span>"),
    }
}

/// A post.
pub fn post(site: &Site, e: &Entry) -> String {
    let s = &site.settings;
    let permalink = format!("{}{}", site.base_url, e.path);
    let title = format!("{} - {}", e.title, s.title);
    let description = e
        .description
        .clone()
        .unwrap_or_else(|| s.description.clone());
    let mut b = Blocks::new(site, &title, &description, &permalink, "site-post-body");
    b.og_type = "article";
    let image = match &e.og_image {
        Some(img) => format!("{}{img}", site.base_url),
        None => format!(
            "{}{}",
            site.base_url,
            s.social_preview.as_deref().unwrap_or("")
        ),
    };
    b.og_image = esc(&image);
    b.twitter_image = esc(&image);
    b.og_image_extra = false;
    let published = iso(e.date.as_ref());
    let modified = iso(e.updated.as_ref().or(e.date.as_ref()));
    let author = json(s.author());
    let home = json(&format!("{}/", site.base_url));
    b.extra_head = format!(
        "\n\n\n\n\n<meta property=\"article:published_time\" content=\"{}\">\n<meta property=\"article:modified_time\" content=\"{}\">\n<script type=\"application/ld+json\">\n{{\n  \"@context\": \"https://schema.org\",\n  \"@type\": \"BlogPosting\",\n  \"headline\": {},\n  \"description\": {},\n  \"datePublished\": \"{}\",\n  \"dateModified\": \"{}\",\n  \"url\": {},\n  \"mainEntityOfPage\": {},\n  \"image\": {},\n  \"author\": {{ \"@type\": \"Person\", \"name\": {author}, \"url\": {home} }},\n  \"publisher\": {{ \"@type\": \"Person\", \"name\": {author}, \"url\": {home} }}\n}}\n</script>\n",
        esc(&published),
        esc(&modified),
        json(&e.title),
        json(&description),
        esc(&published),
        esc(&modified),
        json(&permalink),
        json(&permalink),
        json(&image)
    );
    let mut footer = String::new();
    if site.features.tags && !e.tags.is_empty() {
        let links: Vec<String> = e
            .tags
            .iter()
            .map(|t| {
                format!(
                    "<a href=\"{}\">#{}</a>",
                    esc(&format!("/tags/{}/", markdown::slugify(t))),
                    esc(t)
                )
            })
            .collect();
        footer.push_str(&format!(
            "\n    <div class=\"mt-3XS site-post-tags\">\n      {}\n    </div>",
            links.join(" ")
        ));
    }
    if site.features.feed {
        footer.push_str(&format!(
            "\n\n    <div class=\"mt-3XS link-subscribe-to-feed\">\n      <a target=\"_blank\" href=\"/posts/posts_feed.xml\">{}</a>\n    </div>",
            esc(&s.labels.subscribe)
        ));
    }
    b.content = format!(
        "\n<article class=\"site-post h-entry zoomable\">\n  <header class=\"site-post-header\">\n    <h1 class=\"p-name\">{}</h1>\n    <div class=\"site-post-byline\">By {}</div>\n  </header>\n\n  <div class=\"trix-content e-content\">\n    {}\n  </div>\n\n  <footer class=\"site-meta\">\n    <div class=\"published-at\">\n      <time class=\"dt-published\" datetime=\"{p}\" title=\"{p}\">\n        {}\n      </time>\n    </div>{footer}\n\n    <div class=\"mt-3XS site-link-back-to-blog-posts\">\n      <a href=\"/\">{}</a>\n    </div>\n  </footer>\n</article>\n",
        esc(&e.title),
        byline(site),
        e.html,
        esc(&e.date.as_ref().map(Date::long).unwrap_or_default()),
        esc(&s.labels.back),
        p = esc(&published),
    );
    shell(site, &b)
}

/// A page.
pub fn page(site: &Site, e: &Entry) -> String {
    let s = &site.settings;
    let permalink = format!("{}{}", site.base_url, e.path);
    let title = format!("{} - {}", e.title, s.title);
    let description = e
        .description
        .clone()
        .unwrap_or_else(|| s.description.clone());
    let mut b = Blocks::new(site, &title, &description, &permalink, "site-page-body");
    b.content = format!(
        "\n<article class=\"site-page zoomable\">\n  <header class=\"site-page-header\">\n    <h1>{}</h1>\n  </header>\n\n  <div class=\"trix-content e-content\">\n    {}\n  </div>\n\n  <footer class=\"site-meta\">\n    <div class=\"mt-3XS site-link-back-to-blog-posts\">\n      <a href=\"/\">{}</a>\n    </div>\n  </footer>\n</article>\n",
        esc(&e.title),
        e.html,
        esc(&s.labels.back)
    );
    shell(site, &b)
}

/// The list of tags.
pub fn tags(site: &Site) -> String {
    let s = &site.settings;
    let url = format!("{}/tags/", site.base_url);
    let title = format!("Tags - {}", s.title);
    let mut b = Blocks::new(
        site,
        &title,
        "Browse posts by topic.",
        &url,
        "site-page-body",
    );
    let mut items = String::new();
    for t in &site.tags {
        items.push_str(&format!(
            "\n        <li><a href=\"{}\">{}</a> ({})</li>\n      ",
            esc(&format!("/tags/{}/", t.slug)),
            esc(&t.name),
            t.posts.len()
        ));
    }
    b.content = format!(
        "\n<div class=\"site-page zoomable\">\n  <div class=\"trix-content e-content\">\n    <h1>Tags</h1>\n    <ul class=\"site-list-of-tags\">\n      {items}\n    </ul>\n  </div>\n</div>\n"
    );
    shell(site, &b)
}

/// One tag's posts.
pub fn tag(site: &Site, t: &Tag) -> String {
    let s = &site.settings;
    let url = format!("{}/tags/{}/", site.base_url, t.slug);
    let title = format!("{} - {}", t.name, s.title);
    // Upstream writes the quotes unescaped, which ends the attribute early; here they are escaped.
    let description = format!("Posts tagged \"{}\".", t.name);
    let mut b = Blocks::new(site, &title, &description, &url, "site-page-body");
    let posts: Vec<&Entry> = t.posts.iter().filter_map(|i| site.posts.get(*i)).collect();
    b.content = format!(
        "\n<div class=\"site-page zoomable\">\n  <div class=\"trix-content e-content\">\n    <h1>Tag: {}</h1>\n    <ul class=\"t-list-of-posts site-list-of-posts h-feed\">\n      \n      {}\n    </ul>\n    <p class=\"mt-3XS\"><a href=\"/tags/\">All tags</a></p>\n  </div>\n</div>\n",
        esc(&t.name),
        post_list(&posts)
    );
    shell(site, &b)
}

/// Zola's redirect page for an alias, pointing at `target`, a root-relative path.
pub fn alias(target: &str) -> String {
    format!(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<title>Redirect</title>\n<script>\n  const target = \"{target}\";\n  const hash = window.location.hash || \"\";\n  window.location.replace(target + hash);\n</script>\n<noscript>\n  <meta http-equiv=\"refresh\" content=\"0; url={target}\">\n</noscript>\n<p><a href=\"{target}\">Click here</a> to be redirected.</p>\n"
    )
}

/// Zola's built-in 404 page.
pub fn not_found() -> String {
    "<!doctype html>\n<title>404 Not Found</title>\n<h1>404 Not Found</h1>\n".to_string()
}

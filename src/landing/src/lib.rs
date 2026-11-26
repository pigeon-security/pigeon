//! The `landing` template and its renderer, a peer of `blog` (docs/plans/platform-self-hosting.md,
//! "Landing page as the `landing@1` template"): a repository (`.pigeon/site.toml`,
//! `content/index.toml`, `content/pages/*.md`, `static/`) in, a static landing site out.
//!
//! Its own crate (`pigeon-landing`); it uses only `serde`, `toml`, `sha2` and `pulldown-cmark`,
//! and builds natively for the smoke suite.
//!
//! Pure: no I/O, no clock, no environment. The same [`Input`] always renders the same bytes.
//!
//! **No scripts, ever.** The pages carry no `<script>` and no `on*=` attribute, Markdown's raw
//! HTML is shown as text, and every link must be a site path, a fragment or an `http(s)` or
//! `mailto` URL. That is what lets a landing release share an origin with the console
//! (docs/plans/platform-self-hosting.md).
//!
//! The stylesheet is the apex's (`src/core/src/home.css`, the console's tokens and components),
//! embedded at compile time, so [`template_digest`] covers it: a change to it is a new template
//! version (the smoke suite ties the digest to the version).

pub mod content;
pub mod pages;
pub mod repo;
pub mod site;

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

/// The newest template a consumer can name in `site.toml`.
pub const TEMPLATE: &str = "landing@1";

/// The newest exact version this build carries. A deploy records the one it used.
pub const VERSION: &str = "1.1.0";

/// The stylesheet, served at `/assets/pigeon.css`: the apex's, one source for both.
pub const CSS: &str = include_str!("../../core/src/home.css");

/// The images the stylesheet and the pages name: site path, bytes, content type. The mark's
/// artwork is at `/mark/head.webp` because the stylesheet names it there.
pub const ASSETS: [(&str, &[u8], &str); 3] = [
    (
        "mark/head.webp",
        include_bytes!("../../core/assets/mark-head.webp"),
        "image/webp",
    ),
    (
        "favicon-32.png",
        include_bytes!("../../core/assets/favicon-32.png"),
        "image/png",
    ),
    (
        "apple-touch-icon.png",
        include_bytes!("../../core/assets/apple-touch-180.png"),
        "image/png",
    ),
];

/// Where the stylesheet is served, without the leading slash.
pub const CSS_PATH: &str = "assets/pigeon.css";

/// One template version this build carries.
#[derive(Clone, Copy, Debug)]
pub struct Template {
    /// What `site.toml` names, such as `landing@1`.
    pub name: &'static str,
    /// The major version the name stands for.
    pub major: u64,
    /// The exact version, `MAJOR.MINOR.PATCH`.
    pub version: &'static str,
    /// The stylesheet this version serves at [`CSS_PATH`].
    pub css: &'static str,
    /// The images this version serves: site path, bytes, content type.
    pub assets: &'static [(&'static str, &'static [u8], &'static str)],
}

/// Every template version this build carries, newest first.
pub const TEMPLATES: &[Template] = &[Template {
    name: TEMPLATE,
    major: 1,
    version: VERSION,
    css: CSS,
    assets: &ASSETS,
}];

/// One line of the template's changelog.
#[derive(Clone, Copy, Debug)]
pub struct Change {
    /// The version the change came in.
    pub version: &'static str,
    /// Whether an instance or a consumer repository must change something.
    pub breaking: bool,
    /// What changed, in a line.
    pub summary: &'static str,
    /// The work a breaking change needs; set when `breaking` is true.
    pub migration: Option<&'static str>,
}

/// The template's changelog, newest first.
pub const CHANGELOG: &[Change] = &[
    Change {
        version: "1.1.0",
        breaking: false,
        summary: "A dotted ground behind the hero, fading out before the first section (the `--dot` token).",
        migration: None,
    },
    Change {
        version: "1.0.0",
        breaking: false,
        summary: "The state when versions began (docs/plans/versioning.md).",
        migration: None,
    },
];

/// What a deploy hands the renderer.
#[derive(Clone, Debug, Default)]
pub struct Input {
    /// `.pigeon/site.toml`.
    pub site_toml: String,
    /// `content/index.toml`, the home page, if the repository has one.
    pub index: Option<String>,
    /// `content/pages/*.md`: file name and text.
    pub pages: Vec<(String, String)>,
    /// Every path under `static/`, relative to it.
    pub statics: BTreeSet<String>,
    /// The deployed commit, short, for the footer.
    pub commit: String,
}

/// A rendered site.
#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    /// Generated files by site path without a leading slash (`index.html`, `about/index.html`,
    /// `assets/pigeon.css`): bytes and content type.
    pub files: BTreeMap<String, (Vec<u8>, &'static str)>,
    /// Paths under `static/` to serve as they are, at the same path.
    pub statics: Vec<String>,
    /// The selected template's name.
    pub template: &'static str,
    /// The selected template's exact version.
    pub version: &'static str,
    /// The template's digest: sha256 over its version and embedded files.
    pub digest: String,
}

/// Something wrong with the consumer's repository, and where.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    /// The repository path, such as `content/index.toml`.
    pub file: String,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

/// Renders the consumer's repository, or lists everything wrong with it.
pub fn render(input: &Input) -> Result<Output, Vec<Problem>> {
    render_pinned(input, &|_| true)
}

/// Renders with the newest carried template of the major `site.toml` names whose version
/// `allows` accepts, or lists everything wrong.
pub fn render_pinned(input: &Input, allows: &dyn Fn(&str) -> bool) -> Result<Output, Vec<Problem>> {
    let mut problems = Vec::new();
    let settings = site::parse(&input.site_toml, &mut problems);
    let index = match &input.index {
        Some(text) => content::parse_index(text, &mut problems),
        None => {
            problems.push(Problem {
                file: content::INDEX.into(),
                message: "the home page is missing: add content/index.toml".into(),
            });
            None
        }
    };
    let pages = content::parse_pages(&input.pages, &mut problems);
    let Some((settings, major)) = settings else {
        problems.sort();
        return Err(problems);
    };
    let chosen = TEMPLATES
        .iter()
        .find(|t| t.major == major && allows(t.version));
    if chosen.is_none() {
        let carried: Vec<&str> = TEMPLATES
            .iter()
            .filter(|t| t.major == major)
            .map(|t| t.version)
            .collect();
        problems.push(Problem {
            file: site::FILE.into(),
            message: if carried.is_empty() {
                format!(
                    "this pigeon carries no landing@{major}; name {TEMPLATE} in `template`"
                )
            } else {
                format!(
                    "this pigeon carries landing@{major} versions {}; the instance's pin allows none of them, so either the pin or `template` must move",
                    carried.join(", ")
                )
            },
        });
    }
    let (Some(index), Some(t)) = (index, chosen) else {
        problems.sort();
        problems.dedup();
        return Err(problems);
    };
    let site = pages::Site {
        settings: &settings,
        commit: &input.commit,
    };
    let mut files: BTreeMap<String, (Vec<u8>, &'static str)> = BTreeMap::new();
    let html = "text/html; charset=utf-8";
    files.insert(
        "index.html".into(),
        (pages::index(&site, &index).into_bytes(), html),
    );
    files.insert(
        "404.html".into(),
        (pages::not_found(&site).into_bytes(), html),
    );
    for page in &pages {
        let path = format!("{}/index.html", page.slug);
        if files.contains_key(&path) || page.slug == "assets" || page.slug == "mark" {
            problems.push(Problem {
                file: page.file.clone(),
                message: format!(
                    "/{}/ belongs to the template; choose another name",
                    page.slug
                ),
            });
            continue;
        }
        files.insert(path, (pages::page(&site, page).into_bytes(), html));
    }
    files.insert(
        CSS_PATH.into(),
        (t.css.as_bytes().to_vec(), "text/css; charset=utf-8"),
    );
    for (path, bytes, kind) in t.assets {
        files.insert((*path).to_string(), (bytes.to_vec(), kind));
    }
    for path in files.keys() {
        if input.statics.contains(path) {
            problems.push(Problem {
                file: format!("static/{path}"),
                message: format!("/{path} is generated by {}; remove this file", t.name),
            });
        }
    }
    if !problems.is_empty() {
        problems.sort();
        problems.dedup();
        return Err(problems);
    }
    Ok(Output {
        files,
        statics: input.statics.iter().cloned().collect(),
        template: t.name,
        version: t.version,
        digest: template_digest(t),
    })
}

/// The newest template's digest: sha256 over its name, version and embedded files.
pub fn digest() -> String {
    TEMPLATES.first().map(template_digest).unwrap_or_default()
}

/// A template's digest: sha256 over its name, version, stylesheet and images.
pub fn template_digest(t: &Template) -> String {
    let mut h = Sha256::new();
    h.update(t.name.as_bytes());
    h.update(t.version.as_bytes());
    h.update(CSS_PATH.as_bytes());
    h.update((t.css.len() as u64).to_be_bytes());
    h.update(t.css.as_bytes());
    for (path, bytes, kind) in t.assets {
        h.update(path.as_bytes());
        h.update(kind.as_bytes());
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    hex(h)
}

/// Fingerprint the renderer sources separately from the selected template's assets.
pub fn renderer_digest() -> String {
    let mut h = Sha256::new();
    for source in [
        include_str!("lib.rs"),
        include_str!("site.rs"),
        include_str!("content.rs"),
        include_str!("pages.rs"),
        include_str!("repo.rs"),
    ] {
        h.update((source.len() as u64).to_be_bytes());
        h.update(source.as_bytes());
    }
    hex(h)
}

fn hex(h: Sha256) -> String {
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Escapes text for HTML text and attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Whether `link` is one the template may write into an `href`: a site path, a fragment, or an
/// `http(s)` or `mailto` URL. Anything else (`javascript:`, `data:`, a bare word) is refused.
pub fn safe_link(link: &str) -> bool {
    if link.is_empty() || link.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let lower = link.to_ascii_lowercase();
    (link.starts_with('/') && !link.starts_with("//"))
        || link.starts_with('#')
        || lower.starts_with("https://")
        || lower.starts_with("http://")
        || lower.starts_with("mailto:")
}

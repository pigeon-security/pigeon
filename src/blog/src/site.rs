//! `.pigeon/site.toml`: which template, which features, and the site's settings. Unknown keys
//! are refused; every setting but `title` and `description` has the template's default.

use serde::Deserialize;

use super::{Problem, TEMPLATE, TEMPLATES};

pub(crate) const FILE: &str = ".pigeon/site.toml";

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    template: String,
    #[serde(default)]
    features: Vec<String>,
    site: Settings,
}

/// `[site]`, with the template's defaults.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// The site's name: titles, the header, the manifest.
    pub title: String,
    /// Meta descriptions, JSON-LD, the feed's subtitle, the manifest.
    pub description: String,
    /// The manifest's short name.
    #[serde(default)]
    pub short_name: Option<String>,
    /// Markdown above the home page's list of posts.
    #[serde(default)]
    pub intro: Option<String>,
    /// The header's image, a site path.
    #[serde(default)]
    pub avatar: Option<String>,
    /// The default Open Graph image, a site path.
    #[serde(default)]
    pub social_preview: Option<String>,
    /// Where the byline links, a site path.
    #[serde(default)]
    pub author_page: Option<String>,
    /// The byline's and JSON-LD's name; the title when unset.
    #[serde(default)]
    pub author_name: Option<String>,
    /// The JSON-LD person's other names.
    #[serde(default)]
    pub alternate_names: Vec<String>,
    /// The header's links, in order. External ones are also JSON-LD `sameAs`.
    #[serde(default)]
    pub nav: Vec<Link>,
    /// The manifest's icons.
    #[serde(default)]
    pub icons: Vec<Icon>,
    /// The colour and font themes.
    #[serde(default)]
    pub look: Look,
    /// The post footer's labels.
    #[serde(default)]
    pub labels: Labels,
    /// A one-line notice above the header, with the `banner` feature.
    #[serde(default)]
    pub banner: Option<Banner>,
    /// The `theme-color` metas; light is also the manifest's colours.
    #[serde(default)]
    pub theme_color: ThemeColor,
    /// The Open Graph image's size.
    #[serde(default)]
    pub og_image_size: Size,
}

impl Settings {
    /// The author's name: `author_name`, else the title.
    pub fn author(&self) -> &str {
        self.author_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.title)
    }
}

/// A header link.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// Its text.
    pub title: String,
    /// A site path or an `https://` URL.
    pub link: String,
}

/// A manifest icon.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Icon {
    /// A site path.
    pub src: String,
    /// Such as `192x192`.
    pub sizes: String,
    /// Such as `image/png`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// The `color-theme-*` and `font-theme-*` classes and the Bunny Fonts family.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Look {
    /// `pigeon` (the default) or `gameboy`.
    pub color: String,
    /// `ibm-plex` by default.
    pub font: String,
    /// The Bunny Fonts family, with the `fonts` feature.
    pub font_family: String,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            color: "pigeon".into(),
            font: "ibm-plex".into(),
            font_family: "ibm-plex-sans:400,400i,500,500i,600,600i,700,700i".into(),
        }
    }
}

/// The post footer's labels.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Labels {
    /// The link back to the home page.
    pub back: String,
    /// The link to the feed.
    pub subscribe: String,
}

impl Default for Labels {
    fn default() -> Self {
        Labels {
            back: "Back to all posts".into(),
            subscribe: "Subscribe via RSS".into(),
        }
    }
}

/// The banner.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Banner {
    /// Its text.
    pub message: String,
    /// An optional link after it.
    #[serde(default)]
    pub link: Option<String>,
    /// The link's text.
    #[serde(default)]
    pub link_text: Option<String>,
}

/// The `theme-color` metas.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeColor {
    /// Light; also the manifest's theme and background colour.
    pub light: String,
    /// Dark.
    pub dark: String,
}

impl Default for ThemeColor {
    fn default() -> Self {
        ThemeColor {
            light: "rgb(228, 233, 226)".into(),
            dark: "rgb(25, 28, 26)".into(),
        }
    }
}

/// A size in pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Size {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Default for Size {
    fn default() -> Self {
        Size {
            width: 1188,
            height: 670,
        }
    }
}

/// The template's features, each on or off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    /// The Atom feed's links (default on).
    pub feed: bool,
    /// A post's tag links (default off).
    pub tags: bool,
    /// The footer's link to `/changelog` (default off).
    pub changelog: bool,
    /// KaTeX, pinned with SRI (default off).
    pub math: bool,
    /// Bunny Fonts for `look.font_family` (default on).
    pub fonts: bool,
    /// The banner (default off).
    pub banner: bool,
    /// The commit in the footer (default on).
    pub commit: bool,
}

impl Default for Features {
    fn default() -> Self {
        Features {
            feed: true,
            tags: false,
            changelog: false,
            math: false,
            fonts: true,
            banner: false,
            commit: true,
        }
    }
}

const FEATURES: [&str; 7] = [
    "feed",
    "tags",
    "changelog",
    "math",
    "fonts",
    "banner",
    "commit",
];

impl Features {
    fn flag(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "feed" => &mut self.feed,
            "tags" => &mut self.tags,
            "changelog" => &mut self.changelog,
            "math" => &mut self.math,
            "fonts" => &mut self.fonts,
            "banner" => &mut self.banner,
            "commit" => &mut self.commit,
            _ => return None,
        })
    }
}

/// Parses `site.toml`, adding a problem for everything wrong with it. Answers the settings
/// whenever the file parses, so the rest of the site is still checked.
pub fn parse(text: &str, problems: &mut Vec<Problem>) -> Option<(Settings, Features)> {
    parse_major(text, problems).map(|(s, f, _)| (s, f))
}

/// Like [`parse`], and also answers the major of `blog@N` when the file names a carried one.
pub fn parse_major(
    text: &str,
    problems: &mut Vec<Problem>,
) -> Option<(Settings, Features, Option<u64>)> {
    let mut problem = |message: String| {
        problems.push(Problem {
            file: FILE.into(),
            message,
        });
    };
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            problem(e.to_string().trim().replace('\n', " "));
            return None;
        }
    };
    let mut problem = |message: String| {
        problems.push(Problem {
            file: FILE.into(),
            message,
        });
    };
    let major = file
        .template
        .strip_prefix("blog@")
        .and_then(|n| n.parse::<u64>().ok())
        .filter(|n| TEMPLATES.iter().any(|t| t.major == *n));
    if major.is_none() {
        let carried: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
        problem(format!(
            "template is `{}`; this pigeon carries {}",
            file.template,
            carried.join(", ")
        ));
    }
    let mut features = Features::default();
    for f in &file.features {
        let (name, on) = match f.strip_prefix("no-") {
            Some(name) => (name, false),
            None => (f.as_str(), true),
        };
        match features.flag(name) {
            Some(flag) => *flag = on,
            None => problem(format!(
                "features: `{f}` is not a feature of {TEMPLATE}; known: {} (or `no-` before one)",
                FEATURES.join(", ")
            )),
        }
    }
    let s = &file.site;
    for (what, value) in [("title", &s.title), ("description", &s.description)] {
        if value.trim().is_empty() {
            problem(format!("site.{what} is empty"));
        }
    }
    for (what, path) in [
        ("avatar", &s.avatar),
        ("social_preview", &s.social_preview),
        ("author_page", &s.author_page),
    ] {
        if let Some(p) = path
            && !p.starts_with('/')
        {
            problem(format!(
                "site.{what} `{p}` must be a site path starting with /"
            ));
        }
    }
    for item in &s.nav {
        let ok = item.link.starts_with('/')
            || item.link.starts_with("https://")
            || item.link.starts_with("http://");
        if !ok {
            problem(format!(
                "site.nav `{}`: link `{}` must start with /, https:// or http://",
                item.title, item.link
            ));
        }
    }
    for icon in &s.icons {
        if !icon.src.starts_with('/') {
            problem(format!("site.icons: src `{}` must start with /", icon.src));
        }
    }
    for (what, v) in [("look.color", &s.look.color), ("look.font", &s.look.font)] {
        if v.is_empty()
            || !v
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            problem(format!(
                "site.{what} `{v}` must be lowercase letters, digits and dashes"
            ));
        }
    }
    if features.banner && s.banner.is_none() {
        problem("the banner feature is on, but [site.banner] is missing".into());
    }
    Some((file.site, features, major))
}

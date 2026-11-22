//! Posts and pages: their names, their TOML `+++` front matter and their Markdown.
//!
//! A post is `content/posts/YYYY-MM-DD-slug.md`, served at `/posts/slug/`; its date prefix must
//! equal its `date`. A page is `content/pages/name.md`, served at `/pages/name/`.

use serde::Deserialize;
use toml::value::{Datetime, Offset};

use super::{Date, Problem, markdown};

/// Which kind of entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `content/posts/`.
    Post,
    /// `content/pages/`.
    Page,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Post => "posts",
            Kind::Page => "pages",
        }
    }
}

/// A post or a page, parsed and rendered.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Post or page.
    pub kind: Kind,
    /// Its repository path, such as `content/posts/2025-01-02-hello.md`.
    pub file: String,
    /// Its slug, such as `hello`.
    pub slug: String,
    /// Its site path, such as `/posts/hello/`.
    pub path: String,
    /// `title`.
    pub title: String,
    /// `description`.
    pub description: Option<String>,
    /// `date`: required on a post.
    pub date: Option<Date>,
    /// `updated`.
    pub updated: Option<Date>,
    /// `aliases`: old URLs that redirect here, such as `pages/names`.
    pub aliases: Vec<String>,
    /// `[taxonomies] tags`.
    pub tags: Vec<String>,
    /// `[extra] og_image`: a site path.
    pub og_image: Option<String>,
    /// The body as HTML.
    pub html: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    date: Option<When>,
    #[serde(default)]
    updated: Option<When>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    taxonomies: Taxonomies,
    #[serde(default)]
    extra: Extra,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum When {
    Datetime(Datetime),
    Text(String),
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Taxonomies {
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Extra {
    #[serde(default)]
    og_image: Option<String>,
}

/// Parses every file of one kind, adding a problem for each that is wrong.
pub fn parse_all(
    kind: Kind,
    files: &[(String, String)],
    problems: &mut Vec<Problem>,
) -> Vec<Entry> {
    files
        .iter()
        .filter_map(|(name, text)| match parse(kind, name, text) {
            Ok(e) => Some(e),
            Err(mut found) => {
                problems.append(&mut found);
                None
            }
        })
        .collect()
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Parses one post or page.
pub fn parse(kind: Kind, name: &str, text: &str) -> Result<Entry, Vec<Problem>> {
    let file = format!("content/{}/{name}", kind.dir());
    let mut problems = Vec::new();
    let mut problem = |message: String| {
        problems.push(Problem {
            file: file.clone(),
            message,
        });
    };
    let Some(stem) = name.strip_suffix(".md") else {
        problem(format!("only .md files belong in content/{}/", kind.dir()));
        return Err(problems);
    };
    let (prefix, slug) = match kind {
        Kind::Post => {
            let dated = stem.len() > 11
                && stem.as_bytes()[..10].iter().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        *b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                })
                && stem.as_bytes()[10] == b'-';
            if !dated || !is_slug(&stem[11..]) {
                problem("is not named YYYY-MM-DD-slug.md (the slug in lowercase letters, digits and dashes)".into());
                return Err(problems);
            }
            (Some(&stem[..10]), &stem[11..])
        }
        Kind::Page => {
            if !is_slug(stem) {
                problem("is not named slug.md (lowercase letters, digits and dashes)".into());
                return Err(problems);
            }
            (None, stem)
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let split = text
        .strip_prefix("+++")
        .and_then(|rest| rest.split_once("\n+++"));
    let Some((front, body)) = split else {
        problem("has no TOML front matter: start it with +++ and end it with a line of +++".into());
        return Err(problems);
    };
    let body = body.split_once('\n').map_or("", |(_, b)| b);
    let front: Front = match toml::from_str(front) {
        Ok(f) => f,
        Err(e) => {
            problem(format!(
                "front matter: {}",
                e.to_string().trim().replace('\n', " ")
            ));
            return Err(problems);
        }
    };
    let mut date_of = |what: &str, w: Option<When>| -> Option<Date> {
        let dt = match w? {
            When::Datetime(dt) => dt,
            When::Text(s) => match s.parse::<Datetime>() {
                Ok(dt) => dt,
                Err(_) => {
                    problem(format!(
                        "{what} `{s}` is not a date such as 2025-01-02 or 2025-01-02T03:04:05Z"
                    ));
                    return None;
                }
            },
        };
        let Some(d) = dt.date else {
            problem(format!("{what} has a time but no date"));
            return None;
        };
        let t = dt.time;
        let offset_min = match dt.offset {
            Some(Offset::Custom { minutes }) => minutes,
            _ => 0,
        };
        Some(Date {
            ymdhms: (
                d.year,
                d.month,
                d.day,
                t.map_or(0, |t| t.hour),
                t.map_or(0, |t| t.minute),
                t.map_or(0, |t| t.second),
            ),
            offset_min,
            has_time: t.is_some(),
        })
    };
    let date = date_of("date", front.date);
    let updated = date_of("updated", front.updated);
    if front.title.trim().is_empty() {
        problem("title is empty".into());
    }
    if let Some(prefix) = prefix {
        match &date {
            None => problem("a post needs a date".into()),
            Some(d) if d.ymd() != prefix => problem(format!(
                "is prefixed {prefix}, but its date is {}: make them the same",
                d.ymd()
            )),
            Some(_) => {}
        }
    }
    for a in &front.aliases {
        let ok = !a.trim_matches('/').is_empty()
            && a.trim_matches('/').split('/').all(|seg| {
                !seg.is_empty()
                    && seg
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            });
        if !ok {
            problem(format!(
                "alias `{a}` must be a site path such as pages/old-name"
            ));
        }
    }
    if let Some(img) = &front.extra.og_image
        && !img.starts_with('/')
    {
        problem(format!(
            "og_image `{img}` must be a site path starting with /"
        ));
    }
    for t in &front.taxonomies.tags {
        if markdown::slugify(t).is_empty() {
            problem(format!("tag `{t}` has no letters or digits"));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Entry {
        kind,
        file,
        slug: slug.to_string(),
        path: format!("/{}/{slug}/", kind.dir()),
        title: front.title,
        description: front.description,
        date,
        updated,
        aliases: front.aliases,
        tags: front.taxonomies.tags,
        og_image: front.extra.og_image,
        html: markdown::render(body, &format!("/{}/{slug}/", kind.dir())),
    })
}

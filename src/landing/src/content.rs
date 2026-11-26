//! The content: `content/index.toml`, the home page as structured blocks (the hero, an example
//! panel, and sections of steps, rows or text with links), and `content/pages/*.md`, plain
//! pages with TOML `+++` front matter. Unknown keys are refused, and every link is checked
//! ([`crate::safe_link`]).

use serde::Deserialize;

use super::{Problem, safe_link};

pub(crate) const INDEX: &str = "content/index.toml";

/// The home page.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// The block at the top: the mark, a pill, the heading, the lede and the actions.
    pub hero: Hero,
    /// The panel beside the hero, if any: an example record.
    #[serde(default)]
    pub example: Option<Example>,
    /// The sections below the hero, in order.
    #[serde(default)]
    pub sections: Vec<Section>,
}

/// A small coloured label.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pill {
    /// The text.
    pub text: String,
    /// `ok`, `warn`, `info` or `accent`; neutral when unset.
    #[serde(default)]
    pub tone: Option<String>,
}

/// A button.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    /// The text.
    pub title: String,
    /// Where it goes.
    pub link: String,
    /// `primary` or `ghost`; a plain button when unset.
    #[serde(default)]
    pub style: Option<String>,
    /// Whether the text is set in the monospace face.
    #[serde(default)]
    pub mono: bool,
}

/// The hero.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hero {
    /// Above the heading.
    #[serde(default)]
    pub pill: Option<Pill>,
    /// The page's `<h1>`.
    pub title: String,
    /// The paragraph under it.
    pub lede: String,
    /// The buttons, in order.
    #[serde(default)]
    pub actions: Vec<Action>,
    /// The small print under the buttons.
    #[serde(default)]
    pub fine: Option<String>,
}

/// The example panel: a titled record with a summary, a table and a footer. Its figures are
/// illustrations, and its `tag` should say so.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Example {
    /// The panel's accessible name.
    pub label: String,
    /// The record's title.
    pub title: String,
    /// Beside the title.
    #[serde(default)]
    pub pill: Option<Pill>,
    /// The small tag at the right of the title, such as `Example`.
    #[serde(default)]
    pub tag: Option<String>,
    /// The summary figures.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// The table's four column headings: date, what, source, amount.
    pub columns: [String; 4],
    /// The table's rows.
    #[serde(default)]
    pub rows: Vec<Row>,
    /// The footer's note, at the left.
    #[serde(default)]
    pub foot: Option<String>,
    /// The footer's count, at the right.
    #[serde(default)]
    pub count: Option<String>,
}

/// One summary figure.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// Its name.
    pub term: String,
    /// Its value.
    pub value: String,
}

/// One row of the example table.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// The date, as shown.
    pub date: String,
    /// What it is.
    pub what: String,
    /// A second, muted line under `what`.
    #[serde(default)]
    pub detail: Option<String>,
    /// Where it came from, shown as a tag; hidden on narrow screens.
    pub source: String,
    /// The amount, as shown; a muted dash when unset.
    #[serde(default)]
    pub amount: Option<String>,
    /// Whether the row is highlighted as part of the pattern.
    #[serde(default)]
    pub highlight: bool,
}

/// A section: a label and heading at the left, and one body at the right: `steps`, `rows`, or
/// `text` with `links`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// The section's fragment, such as `how` for `#how`.
    pub id: String,
    /// The heading's id; `{id}-h` when unset.
    #[serde(default)]
    pub heading_id: Option<String>,
    /// The small label above the heading.
    pub label: String,
    /// The heading.
    pub title: String,
    /// Numbered steps.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Rows of a name, a sentence and a status.
    #[serde(default)]
    pub rows: Vec<Status>,
    /// A paragraph.
    #[serde(default)]
    pub text: Option<String>,
    /// Buttons, on one line under `text`.
    #[serde(default)]
    pub links: Vec<Action>,
}

impl Section {
    /// The heading's id.
    pub fn heading(&self) -> String {
        self.heading_id
            .clone()
            .unwrap_or_else(|| format!("{}-h", self.id))
    }
}

/// One numbered step.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Its heading.
    pub title: String,
    /// Its paragraph.
    pub text: String,
}

/// One row of a status table.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// What it is about.
    pub name: String,
    /// The sentence.
    pub text: String,
    /// Its state, at the right.
    pub status: Pill,
}

const TONES: [&str; 4] = ["ok", "warn", "info", "accent"];
const STYLES: [&str; 2] = ["primary", "ghost"];

fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Parses `content/index.toml`, or records its problems.
pub fn parse_index(text: &str, problems: &mut Vec<Problem>) -> Option<Index> {
    let mut found = Vec::new();
    let index: Index = match toml::from_str(text) {
        Ok(i) => i,
        Err(e) => {
            problems.push(Problem {
                file: INDEX.into(),
                message: e.message().to_string(),
            });
            return None;
        }
    };
    let pill = |p: &Option<Pill>, found: &mut Vec<String>| {
        if let Some(t) = p.as_ref().and_then(|p| p.tone.as_deref())
            && !TONES.contains(&t)
        {
            found.push(format!("tone `{t}`: use one of {}", TONES.join(", ")));
        }
    };
    let action = |a: &Action, found: &mut Vec<String>| {
        if !safe_link(&a.link) {
            found.push(format!(
                "`{}` links to `{}`: use a site path, a #fragment, or an http(s) or mailto URL",
                a.title, a.link
            ));
        }
        if let Some(s) = a.style.as_deref()
            && !STYLES.contains(&s)
        {
            found.push(format!("style `{s}`: use one of {}", STYLES.join(", ")));
        }
    };
    pill(&index.hero.pill, &mut found);
    for a in &index.hero.actions {
        action(a, &mut found);
    }
    if let Some(e) = &index.example {
        pill(&e.pill, &mut found);
    }
    let mut ids = std::collections::BTreeSet::new();
    for s in &index.sections {
        if !is_id(&s.id) || !is_id(&s.heading()) {
            found.push(format!(
                "section `{}`: ids are lowercase letters, digits and hyphens",
                s.id
            ));
        }
        if !ids.insert(s.id.clone()) || !ids.insert(s.heading()) {
            found.push(format!("section `{}`: its ids are used twice", s.id));
        }
        let bodies = [
            !s.steps.is_empty(),
            !s.rows.is_empty(),
            s.text.is_some() || !s.links.is_empty(),
        ];
        if bodies.iter().filter(|b| **b).count() != 1 {
            found.push(format!(
                "section `{}` needs exactly one body: steps, rows, or text with links",
                s.id
            ));
        }
        for r in &s.rows {
            pill(&Some(r.status.clone()), &mut found);
        }
        for a in &s.links {
            action(a, &mut found);
        }
    }
    if found.is_empty() {
        Some(index)
    } else {
        problems.extend(found.into_iter().map(|message| Problem {
            file: INDEX.into(),
            message,
        }));
        None
    }
}

/// One page from `content/pages/`.
#[derive(Clone, Debug)]
pub struct Page {
    /// The repository path.
    pub file: String,
    /// Its URL segment: `/{slug}/`.
    pub slug: String,
    /// Its heading and title.
    pub title: String,
    /// Its meta description; the site's when unset.
    pub description: Option<String>,
    /// Its Markdown.
    pub body: String,
}

/// The front matter of a page.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Front {
    title: String,
    #[serde(default)]
    description: Option<String>,
}

/// Parses `content/pages/*.md`, by file name, or records their problems.
pub fn parse_pages(files: &[(String, String)], problems: &mut Vec<Problem>) -> Vec<Page> {
    let mut out = Vec::new();
    for (name, text) in files {
        let file = format!("content/pages/{name}");
        let mut problem = |message: String| {
            problems.push(Problem {
                file: file.clone(),
                message,
            })
        };
        let Some(slug) = name.strip_suffix(".md").filter(|s| is_id(s)) else {
            problem("name pages lowercase-words.md".into());
            continue;
        };
        let Some((front, body)) = text
            .strip_prefix("+++")
            .and_then(|rest| rest.split_once("\n+++"))
        else {
            problem(
                "has no TOML front matter: start it with +++ and end it with a line of +++".into(),
            );
            continue;
        };
        let front: Front = match toml::from_str(front) {
            Ok(f) => f,
            Err(e) => {
                problem(e.message().to_string());
                continue;
            }
        };
        let body = body.strip_prefix('\n').unwrap_or(body);
        let body = body.strip_prefix('\n').unwrap_or(body).to_string();
        for link in crate::pages::links(&body) {
            if !safe_link(&link) {
                problem(format!(
                    "links to `{link}`: use a site path, a #fragment, or an http(s) or mailto URL"
                ));
            }
        }
        out.push(Page {
            file: file.clone(),
            slug: slug.to_string(),
            title: front.title,
            description: front.description,
            body,
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

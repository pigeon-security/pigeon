//! `.pigeon/site.toml` for `landing`: the template, the site's name and description, the top
//! bar's links and console button, and the footer's links. Unknown keys are refused.

use serde::Deserialize;

use super::{Problem, TEMPLATE, safe_link};

pub(crate) const FILE: &str = ".pigeon/site.toml";

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    template: String,
    site: Written,
    #[serde(default)]
    nav: Vec<Link>,
    #[serde(default)]
    links: Vec<Link>,
}

/// The settings, with the template's defaults filled in.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The site's name: the brand, the home page's `<title>`, the other pages' title suffix.
    pub title: String,
    /// The meta description.
    pub description: String,
    /// The top bar's console button, at the right.
    pub console_url: String,
    /// The console button's text.
    pub console_label: String,
    /// The top bar's links, in order.
    pub nav: Vec<Link>,
    /// The footer's links, in order.
    pub links: Vec<Link>,
}

/// `[site]` as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    title: String,
    description: String,
    console_url: String,
    #[serde(default)]
    console_label: Option<String>,
}

/// A link: its text and where it goes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// The text.
    pub title: String,
    /// A site path, a fragment, or an `http(s)` or `mailto` URL.
    pub link: String,
}

/// Parses `site.toml`: the settings and the major version its `template` names, or `None`
/// with the problems recorded.
pub fn parse(text: &str, problems: &mut Vec<Problem>) -> Option<(Settings, u64)> {
    let mut problem = |message: String| {
        problems.push(Problem {
            file: FILE.into(),
            message,
        })
    };
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            problem(e.message().to_string());
            return None;
        }
    };
    let major = match file.template.split_once('@') {
        Some(("landing", v)) => v.parse::<u64>().ok(),
        _ => None,
    };
    let Some(major) = major else {
        problem(format!(
            "`template = \"{}\"` is not a landing template; name {TEMPLATE}",
            file.template
        ));
        return None;
    };
    let mut ok = true;
    for (what, l) in file
        .nav
        .iter()
        .map(|l| ("nav", l))
        .chain(file.links.iter().map(|l| ("links", l)))
    {
        if !safe_link(&l.link) {
            problem(format!(
                "{what} `{}` links to `{}`: use a site path, a #fragment, or an http(s) or mailto URL",
                l.title, l.link
            ));
            ok = false;
        }
    }
    if !safe_link(&file.site.console_url) {
        problem(format!(
            "console_url `{}`: use a site path or an http(s) URL",
            file.site.console_url
        ));
        ok = false;
    }
    if file.site.title.trim().is_empty() {
        problem("`title` is empty".into());
        ok = false;
    }
    ok.then(|| {
        (
            Settings {
                title: file.site.title,
                description: file.site.description,
                console_url: file.site.console_url,
                console_label: file
                    .site
                    .console_label
                    .unwrap_or_else(|| "Open console".into()),
                nav: file.nav,
                links: file.links,
            },
            major,
        )
    })
}

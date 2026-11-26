//! Template dispatch: which template renders a repository, chosen by the prefix of
//! `.pigeon/site.toml`'s `template` (`blog@1` is [`Template::Blog`], `landing@1` is
//! [`Template::Landing`]), and one rendering path for both, used by `site_render` and
//! `site_deploy`. Each template is its own crate ([`pigeon_blog`], [`pigeon_landing`]) with its
//! own versions, pin and renderer digest.
//!
//! Anything that does not name `landing@…` goes to the blog renderer exactly as before, so a
//! blog site, and every problem the blog renderer reports, is unchanged.
//!
//! Pure, and free of `crate::` paths, so the smoke suite compiles this file natively.

use std::collections::{BTreeMap, BTreeSet};

/// A template this build can render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// `blog@N`: [`pigeon_blog`].
    Blog,
    /// `landing@N`: [`pigeon_landing`].
    Landing,
}

impl Template {
    /// The template's name before the `@`, which is also its version component and the key of
    /// its pin in an instance's record.
    pub fn component(self) -> &'static str {
        match self {
            Template::Blog => "blog",
            Template::Landing => "landing",
        }
    }

    /// The template `site_toml` names. Only a `template` starting `landing@` is the landing;
    /// everything else, an unreadable file included, is the blog, which reports its own
    /// problems.
    pub fn of(site_toml: &str) -> Template {
        let named = toml::from_str::<toml::Table>(site_toml)
            .ok()
            .and_then(|t| t.get("template")?.as_str().map(str::to_string));
        match named.as_deref().and_then(|n| n.split_once('@')) {
            Some(("landing", _)) => Template::Landing,
            _ => Template::Blog,
        }
    }

    /// The template's renderer digest: sha256 over its renderer's sources.
    pub fn renderer_digest(self) -> String {
        match self {
            Template::Blog => pigeon_blog::renderer_digest(),
            Template::Landing => pigeon_landing::renderer_digest(),
        }
    }
}

/// A repository's files the renderers read, whichever template it names.
#[derive(Clone, Debug, Default)]
pub struct Parts {
    /// `.pigeon/site.toml`.
    pub site_toml: String,
    /// `content/posts/*.md` (blog): file name and text.
    pub posts: Vec<(String, String)>,
    /// `content/pages/*.md`: file name and text.
    pub pages: Vec<(String, String)>,
    /// `content/index.toml` (landing).
    pub index: Option<String>,
    /// Every path under `static/`, relative to it.
    pub statics: BTreeSet<String>,
    /// Where the site is canonically served, without a trailing slash.
    pub base_url: String,
    /// The deployed commit, short, for the footer.
    pub commit: String,
    /// The commit's year, for the footer.
    pub commit_year: u16,
}

/// A rendered site, whichever template rendered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// Which template.
    pub kind: Template,
    /// Generated files by site path without a leading slash: bytes and content type.
    pub files: BTreeMap<String, (Vec<u8>, &'static str)>,
    /// Paths under `static/` to serve as they are, at the same path.
    pub statics: Vec<String>,
    /// The selected template's name, such as `blog@1`.
    pub template: &'static str,
    /// The selected template's exact version.
    pub version: &'static str,
    /// The template's digest.
    pub digest: String,
    /// The renderer's digest.
    pub renderer_digest: String,
}

/// Something wrong with the repository, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The repository path.
    pub file: String,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

/// Renders `parts` with the template its `site.toml` names, at the newest carried version of
/// that template's major that `allows(template, version)` accepts.
pub fn render(
    parts: &Parts,
    allows: &dyn Fn(Template, &str) -> bool,
) -> Result<Rendered, Vec<Problem>> {
    let kind = Template::of(&parts.site_toml);
    match kind {
        Template::Blog => {
            let input = pigeon_blog::Input {
                site_toml: parts.site_toml.clone(),
                posts: parts.posts.clone(),
                pages: parts.pages.clone(),
                statics: parts.statics.clone(),
                base_url: parts.base_url.clone(),
                commit: parts.commit.clone(),
                commit_year: parts.commit_year,
            };
            pigeon_blog::render_pinned(&input, &|v| allows(kind, v))
                .map(|o| Rendered {
                    kind,
                    files: o.files,
                    statics: o.statics,
                    template: o.template,
                    version: o.version,
                    digest: o.digest,
                    renderer_digest: kind.renderer_digest(),
                })
                .map_err(|ps| {
                    ps.into_iter()
                        .map(|p| Problem {
                            file: p.file,
                            message: p.message,
                        })
                        .collect()
                })
        }
        Template::Landing => {
            let input = pigeon_landing::Input {
                site_toml: parts.site_toml.clone(),
                index: parts.index.clone(),
                pages: parts.pages.clone(),
                statics: parts.statics.clone(),
                commit: parts.commit.clone(),
            };
            pigeon_landing::render_pinned(&input, &|v| allows(kind, v))
                .map(|o| Rendered {
                    kind,
                    files: o.files,
                    statics: o.statics,
                    template: o.template,
                    version: o.version,
                    digest: o.digest,
                    renderer_digest: kind.renderer_digest(),
                })
                .map_err(|ps| {
                    ps.into_iter()
                        .map(|p| Problem {
                            file: p.file,
                            message: p.message,
                        })
                        .collect()
                })
        }
    }
}

/// A repository's files sorted by the template its `.pigeon/site.toml` names.
#[derive(Debug, Default)]
pub struct Split {
    /// Whether the repository has a `.pigeon/site.toml`.
    pub has_site_toml: bool,
    /// What the renderer reads, without the deploy's fields (base URL, commit, year).
    pub parts: Parts,
    /// The static files with their bytes, for the deploy to copy as they are.
    pub statics: Vec<(String, Vec<u8>)>,
}

/// A repository's files (paths relative to its root) sorted by the template its
/// `.pigeon/site.toml` names. An error when a file the template reads is not UTF-8 text.
pub fn split(files: Vec<(String, Vec<u8>)>) -> Result<Split, String> {
    let named = files
        .iter()
        .find(|(p, _)| p == ".pigeon/site.toml")
        .and_then(|(_, b)| std::str::from_utf8(b).ok())
        .map_or(Template::Blog, Template::of);
    Ok(match named {
        Template::Blog => {
            let s = pigeon_blog::repo::split(files)?;
            let parts = Parts {
                site_toml: s.site_toml.clone().unwrap_or_default(),
                posts: s.posts,
                pages: s.pages,
                statics: s.statics.iter().map(|(p, _)| p.clone()).collect(),
                ..Parts::default()
            };
            Split {
                has_site_toml: s.site_toml.is_some(),
                parts,
                statics: s.statics,
            }
        }
        Template::Landing => {
            let s = pigeon_landing::repo::split(files)?;
            let parts = Parts {
                site_toml: s.site_toml.clone().unwrap_or_default(),
                index: s.index,
                pages: s.pages,
                statics: s.statics.iter().map(|(p, _)| p.clone()).collect(),
                ..Parts::default()
            };
            Split {
                has_site_toml: s.site_toml.is_some(),
                parts,
                statics: s.statics,
            }
        }
    })
}

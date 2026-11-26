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

/// A release rendered from a content root, ready to stage: each file's site path, bytes and
/// content type (the template's files, then `static/`'s), and what rendered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub files: Vec<(String, Vec<u8>, String)>,
    pub template: &'static str,
    pub version: &'static str,
    pub digest: String,
    pub renderer_digest: String,
}

/// Renders a content root's `files` (paths relative to it) at `commit` as a release of the
/// landing template alone, at the newest carried version `allows` accepts: the home instance's
/// site (docs/plans/platform-self-hosting.md, L3). Anything else, a blog among it, is a problem,
/// never a release.
pub fn landing_release(
    files: Vec<(String, Vec<u8>)>,
    commit: &str,
    allows: &dyn Fn(&str) -> bool,
) -> Result<Release, Vec<String>> {
    let s = split(files).map_err(|e| vec![e])?;
    if !s.has_site_toml {
        return Err(vec![
            ".pigeon/site.toml: the content root has none; the site is built from it".into(),
        ]);
    }
    if Template::of(&s.parts.site_toml) != Template::Landing {
        return Err(vec![format!(
            ".pigeon/site.toml: only {} is published here; set template = \"{}\"",
            pigeon_landing::TEMPLATE,
            pigeon_landing::TEMPLATE
        )]);
    }
    let parts = Parts {
        commit: commit.chars().take(7).collect(),
        ..s.parts
    };
    let out = render(&parts, &|template, v| {
        template == Template::Landing && allows(v)
    })
    .map_err(|ps| ps.iter().map(ToString::to_string).collect::<Vec<_>>())?;
    let mut files: Vec<(String, Vec<u8>, String)> = out
        .files
        .into_iter()
        .map(|(path, (bytes, ctype))| (path, bytes, ctype.to_string()))
        .collect();
    for (path, bytes) in s.statics {
        let ctype = pigeon_core::r2::content_type(&path).to_string();
        files.push((path, bytes, ctype));
    }
    Ok(Release {
        files,
        template: out.template,
        version: out.version,
        digest: out.digest,
        renderer_digest: out.renderer_digest,
    })
}

/// The private call core makes to render the home instance's release: never in the manifest,
/// so no person or agent calls it, and signed with the instance's context.
pub const BUILD_TOOL: &str = "site_build";

/// [`BUILD_TOOL`]'s answer to `args` (`files`: `[path, base64url]` pairs relative to the content
/// root, `commit`, and the `landing` pin when the record holds one): `{"built": true, …, files:
/// [[path, base64url, content type]]}`, or `{"built": false, "problems": […]}`. Nothing is read
/// or written; an argument that is not as described is an error.
pub fn site_build(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    use pigeon_core::versions::{Requirement, Version};
    use pigeon_core::webauthn::{b64url, unb64url};
    let commit = args["commit"]
        .as_str()
        .filter(|c| !c.is_empty() && c.len() <= 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or("`commit` must be the commit's id")?;
    let pin = match &args["pin"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(p) => {
            Some(Requirement::parse(p).map_err(|e| format!("the landing pin `{p}`: {e}"))?)
        }
        _ => return Err("`pin` must be a version requirement".into()),
    };
    let mut files = Vec::new();
    for pair in args["files"].as_array().ok_or("`files` is required")? {
        let (Some(path), Some(body)) = (pair[0].as_str(), pair[1].as_str()) else {
            return Err("each file is a [path, base64url] pair".into());
        };
        let bytes = unb64url(body).map_err(|e| format!("{path}: {e}"))?;
        files.push((path.to_string(), bytes));
    }
    let allows = |v: &str| {
        pin.as_ref()
            .is_none_or(|r| Version::parse(v).is_ok_and(|v| r.matches(&v)))
    };
    Ok(match landing_release(files, commit, &allows) {
        Ok(r) => serde_json::json!({
            "built": true,
            "template": r.template,
            "template_version": r.version,
            "template_digest": r.digest,
            "renderer_digest": r.renderer_digest,
            "files": r
                .files
                .iter()
                .map(|(p, b, c)| serde_json::json!([p, b64url(b), c]))
                .collect::<Vec<_>>(),
        }),
        Err(problems) => serde_json::json!({ "built": false, "problems": problems }),
    })
}

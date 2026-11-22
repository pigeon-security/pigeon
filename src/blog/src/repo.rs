//! A consumer repository's files sorted into what the renderer reads: `.pigeon/site.toml`,
//! `content/posts/*.md`, `content/pages/*.md`, and everything under `static/` (kept with its
//! bytes, for the deploy to copy as it is). Everything else is ignored. Pure.

/// The repository's files the renderer reads, split by role.
#[derive(Debug, Default)]
pub struct Split {
    /// `.pigeon/site.toml`, if the repository has one.
    pub site_toml: Option<String>,
    /// `content/posts/*.md`: file name and text.
    pub posts: Vec<(String, String)>,
    /// `content/pages/*.md`: file name and text.
    pub pages: Vec<(String, String)>,
    /// Paths under `static/`, relative to it, with their bytes.
    pub statics: Vec<(String, Vec<u8>)>,
}

/// Sorts `(path, bytes)` files, paths relative to the repository root.
pub fn split(files: Vec<(String, Vec<u8>)>) -> Result<Split, String> {
    let mut out = Split::default();
    for (path, bytes) in files {
        let text =
            |b: Vec<u8>| String::from_utf8(b).map_err(|_| format!("{path} is not UTF-8 text"));
        if path == ".pigeon/site.toml" {
            out.site_toml = Some(text(bytes)?);
        } else if let Some(name) = path.strip_prefix("content/posts/") {
            if !name.contains('/') && name.ends_with(".md") {
                out.posts.push((name.to_string(), text(bytes)?));
            }
        } else if let Some(name) = path.strip_prefix("content/pages/") {
            if !name.contains('/') && name.ends_with(".md") {
                out.pages.push((name.to_string(), text(bytes)?));
            }
        } else if let Some(rest) = path.strip_prefix("static/")
            && !rest.is_empty()
        {
            out.statics.push((rest.to_string(), bytes));
        }
    }
    Ok(out)
}

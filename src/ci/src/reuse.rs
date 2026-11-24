//! Reusing a green job for the same commit: which runner image a job ran on, and the one
//! decision of whether an earlier job may stand in for a new one.
//!
//! Nothing here is clever on purpose. A job is reused only when it succeeded, is of the same
//! repo, the same full commit id, the same job name, and recorded the same runner image. A job
//! with no image recorded is never reused.

/// The wrangler.toml the base image's pinned digest is read from, compiled in.
const WRANGLER: &str = include_str!("../../platform/wrangler.toml");

/// The image string under `[containers.images.base]` in `wrangler_toml`, when it is pinned by a
/// `@sha256:` digest; `None` when the section or the key is missing, or the digest is.
pub fn base_image(wrangler_toml: &str) -> Option<String> {
    let doc: toml::Table = toml::from_str(wrangler_toml).ok()?;
    let image = doc
        .get("containers")?
        .as_array()?
        .iter()
        .find_map(|c| c.get("images")?.get("base")?.get("image")?.as_str())
        .or_else(|| {
            doc.get("containers")?
                .as_table()?
                .get("images")?
                .get("base")?
                .get("image")?
                .as_str()
        })?;
    let (_, digest) = image.split_once("@sha256:")?;
    let ok = digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| image.to_string())
}

/// The image the containers connector runs jobs on now, as pinned in the platform's
/// wrangler.toml; `None` when none is pinned, which turns reuse off.
pub fn runner_image() -> Option<String> {
    base_image(WRANGLER)
}

/// A job as kept, with only what the reuse decision reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub repo: String,
    pub commit: String,
    pub name: String,
    pub state: String,
    /// The runner image recorded at dispatch; `None` for a job that recorded none.
    pub image: Option<String>,
}

/// What a new job would be: the job a candidate must match exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub repo: String,
    pub commit: String,
    pub name: String,
    pub image: String,
}

/// Whether `s` is a full lowercase commit id.
fn full_lowercase_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The first of `candidates` (callers pass them newest first) that may stand in for `want`:
/// `succeeded`, same repo, commit, job name and image. Answers `None` when `want.commit` is
/// not a full lowercase commit id or `want.image` is empty.
pub fn reusable<'a>(candidates: &'a [Candidate], want: &Want) -> Option<&'a Candidate> {
    if !full_lowercase_sha(&want.commit) || want.image.is_empty() {
        return None;
    }
    candidates.iter().find(|c| {
        c.state == "succeeded"
            && c.repo == want.repo
            && c.commit == want.commit
            && c.name == want.name
            && c.image.as_deref() == Some(want.image.as_str())
    })
}

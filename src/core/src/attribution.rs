//! Who pigeon's git commits name. The person is the author of every commit pigeon writes
//! itself, so GitHub attributes it to their user; the part of pigeon that wrote it is the
//! committer. This is the one place these identities are spelled
//! (docs/plans/commit-attribution.md).

/// The person's GitHub user name, the author name of commits pigeon writes.
pub const PERSON_NAME: &str = "noisypigeon";
/// The person's public GitHub noreply address, which links a commit to their user.
pub const PERSON_EMAIL: &str = "167243387+noisypigeon@users.noreply.github.com";
/// The committer of the Lander's rebases and landing notes.
pub const LANDER: &str = "pigeon <land@pigeon.dev>";
/// The committer of the CI gateway's job notes.
pub const CI: &str = "pigeon <ci@pigeon.dev>";
/// The committer email of AI agent runs' commits.
pub const AI_AGENTS_EMAIL: &str = "ai-agents@pigeon.dev";

/// The author of commits pigeon writes: `name <email>`, as a commit header holds it.
pub fn author() -> String {
    format!("{PERSON_NAME} <{PERSON_EMAIL}>")
}

/// The committer name of AI agent run `run`'s commits.
pub fn ai_agent_name(run: &str) -> String {
    format!("pigeon AI agent run {run}")
}

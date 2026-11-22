//! Who a proxied request is for, shared by core (the Lander's deploy grants) and the proxy.

use serde::{Deserialize, Serialize};

/// How many characters of a credential's hash make its id.
pub const ID_LEN: usize = 12;

/// Who holds the proxy credential a request carries.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProxyCaller {
    /// The credential's id.
    pub id: String,
    pub label: String,
    /// The person who issued it.
    pub person: String,
    /// The one Worker script it may deploy, when it is a landing's deploy job; `None` for a
    /// proxy credential, which may deploy any platform Worker.
    #[serde(default)]
    pub script: Option<String>,
    /// The branches it may push, when it is an AI agent run's git grant
    /// (`docs/plans/ai-agents.md`): only `refs/heads/{prefix}…`, and nothing through `/cf/`.
    /// `None` for a proxy credential, which pushes as the policy allows.
    #[serde(default)]
    pub branch_prefix: Option<String>,
}

/// Whether every ref in `refs` is a branch under `prefix` (`refs/heads/{prefix}…`): what an AI
/// agent run's git grant may push; the refs outside it otherwise.
pub fn within_prefix<'a>(
    refs: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Result<(), Vec<String>> {
    let full = format!("refs/heads/{prefix}");
    let outside: Vec<String> = refs
        .into_iter()
        .filter(|r| !r.starts_with(&full) || r.contains(".."))
        .map(String::from)
        .collect();
    if outside.is_empty() {
        Ok(())
    } else {
        Err(outside)
    }
}

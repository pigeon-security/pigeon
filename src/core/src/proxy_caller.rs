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
    /// The branches it may push, when it is git-only: an AI agent run's git grant
    /// (`docs/plans/ai-agents.md`), only `refs/heads/{prefix}` and `refs/heads/{prefix}-…`; or
    /// an app's session credential (`session_credential`, docs/plans/agent-git.md), whose
    /// prefix is the app's branch namespace ending in `/`, such as `codex/`, and pushes only
    /// branches under it (see [`within_prefix`]). Either reaches nothing through `/cf/`. `None`
    /// for a proxy credential, which pushes as the policy allows.
    #[serde(default)]
    pub branch_prefix: Option<String>,
}

/// Whether every ref in `refs` is inside `prefix`; the refs outside it otherwise. A `prefix`
/// ending in `/` is a namespace (an app's session credential): a branch under it, such as
/// `refs/heads/codex/x` for `codex/`, never the namespace itself. Any other `prefix` is an AI
/// agent run's: the branch `prefix` or a branch `{prefix}-…` (`refs/heads/{prefix}`,
/// `refs/heads/{prefix}-…`), and another run's branch is outside even when its name starts
/// with `prefix` (run `ab`, branch `claude/ai-agent-abc`).
pub fn within_prefix<'a>(
    refs: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Result<(), Vec<String>> {
    let full = format!("refs/heads/{prefix}");
    let namespace = prefix.ends_with('/') && prefix.len() > 1;
    let inside = |r: &str| {
        r.strip_prefix(&full).is_some_and(|rest| {
            if namespace {
                !rest.is_empty() && !rest.starts_with('/')
            } else {
                rest.is_empty() || rest.starts_with('-')
            }
        }) && !r.contains("..")
    };
    let outside: Vec<String> = refs
        .into_iter()
        .filter(|r| !inside(r))
        .map(String::from)
        .collect();
    if outside.is_empty() {
        Ok(())
    } else {
        Err(outside)
    }
}

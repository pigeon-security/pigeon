//! The git note the gateway writes on a commit under `refs/notes/ci`: one line per job's final
//! result. The notes commit holds every note the gateway has written, as a flat tree named by
//! commit id, so the ref is rebuilt from the scheduler's records each time: the gateway owns it.

use std::collections::HashSet;

use pigeon_core::gitobj::Object;
use serde::{Deserialize, Serialize};

/// The ref the notes live under.
pub const REF: &str = "refs/notes/ci";
/// The author and committer of notes commits.
pub const WHO: &str = "pigeon <ci@pigeon.dev>";

/// One final job result, as the note records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub job: String,
    pub state: String,
    pub connector: Option<String>,
    pub attempt: u32,
    /// What the script exited with, when its runner said.
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub url: Option<String>,
}

/// `s` with whitespace replaced, so a field never splits a line.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

fn or_dash(s: &Option<String>) -> String {
    s.as_deref().map_or_else(|| "-".to_string(), clean)
}

/// The note's text: one line per result, sorted by job,
/// `<job> <state> connector=<c> attempt=<n> exit=<code> duration=<ms>ms url=<u>`, `-` for what
/// is unknown.
pub fn text(lines: &[Line]) -> String {
    let mut sorted: Vec<&Line> = lines.iter().collect();
    sorted.sort_by(|a, b| a.job.cmp(&b.job));
    let mut out = String::new();
    for l in sorted {
        let duration = l
            .duration_ms
            .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms"));
        let exit = l
            .exit_code
            .map_or_else(|| "-".to_string(), |c| c.to_string());
        out.push_str(&format!(
            "{} {} connector={} attempt={} exit={} duration={} url={}\n",
            clean(&l.job),
            clean(&l.state),
            or_dash(&l.connector),
            l.attempt,
            exit,
            duration,
            or_dash(&l.url),
        ));
    }
    out
}

/// The objects of a notes commit over `notes` (`(commit id, text)`), with `parent` the ref's
/// current tip (`None` for the first): the blobs, the tree, then the commit; and the commit's
/// id.
pub fn commit(
    notes: &[(String, String)],
    parent: Option<&str>,
    unix_secs: i64,
) -> Result<(Vec<Object>, String), String> {
    let mut objects: Vec<Object> = Vec::new();
    let mut seen_blobs: HashSet<String> = HashSet::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut entries: Vec<(String, String)> = Vec::new();
    for (cid, body) in notes {
        if cid.len() != 40 || !cid.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("not a commit id: {cid:?}"));
        }
        let cid = cid.to_ascii_lowercase();
        if !seen_ids.insert(cid.clone()) {
            return Err(format!("two notes for commit {cid}"));
        }
        let blob = Object::blob(body.as_bytes());
        let bid = blob.id();
        if seen_blobs.insert(bid.clone()) {
            objects.push(blob);
        }
        entries.push((cid, bid));
    }
    let tree = Object::tree(&entries)?;
    let tree_id = tree.id();
    objects.push(tree);
    let parents: Vec<String> = parent.iter().map(|p| p.to_string()).collect();
    let message = format!("ci: {} notes", notes.len());
    let c = Object::commit(&tree_id, &parents, WHO, unix_secs, &message)?;
    let id = c.id();
    objects.push(c);
    Ok((objects, id))
}

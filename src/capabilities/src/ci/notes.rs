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
    pub duration_secs: Option<u64>,
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
/// `<job> <state> connector=<c> attempt=<n> duration=<s>s url=<u>`, `-` for what is unknown.
pub fn text(lines: &[Line]) -> String {
    let mut sorted: Vec<&Line> = lines.iter().collect();
    sorted.sort_by(|a, b| a.job.cmp(&b.job));
    let mut out = String::new();
    for l in sorted {
        let duration = l
            .duration_secs
            .map_or_else(|| "-".to_string(), |s| format!("{s}s"));
        out.push_str(&format!(
            "{} {} connector={} attempt={} duration={} url={}\n",
            clean(&l.job),
            clean(&l.state),
            or_dash(&l.connector),
            l.attempt,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn line(job: &str, url: Option<&str>) -> Line {
        Line {
            job: job.to_string(),
            state: "succeeded".to_string(),
            connector: None,
            attempt: 1,
            duration_secs: None,
            url: url.map(|u| u.to_string()),
        }
    }

    fn two(t1: &str, t2: &str) -> Vec<(String, String)> {
        vec![
            ("a".repeat(40), t1.to_string()),
            ("b".repeat(40), t2.to_string()),
        ]
    }

    #[test]
    fn the_text_is_sorted_and_says_dash_for_the_unknown() {
        let mut b = line("build", None);
        b.connector = Some("containers".to_string());
        b.attempt = 2;
        b.duration_secs = Some(42);
        let t = text(&[b, line("alpha", None)]);
        assert_eq!(
            t,
            "alpha succeeded connector=- attempt=1 duration=- url=-\n\
             build succeeded connector=containers attempt=2 duration=42s url=-\n"
        );
        assert_eq!(text(&[]), "");
    }

    #[test]
    fn whitespace_never_splits_a_line() {
        let t = text(&[line("j", Some("https://x/a b\tc\nd"))]);
        assert!(t.ends_with("url=https://x/a_b_c_d\n"), "{t}");
        assert_eq!(t.lines().count(), 1);
    }

    #[test]
    fn equal_notes_share_one_blob() {
        let (objs, id) = commit(&two("x\n", "y\n"), None, 10).unwrap();
        assert_eq!(objs.len(), 4);
        assert_eq!(objs.last().map(|o| o.id()), Some(id));
        let (objs, _) = commit(&two("x\n", "x\n"), None, 10).unwrap();
        assert_eq!(objs.len(), 3);
    }

    #[test]
    fn the_tree_names_each_blob_by_its_commit() {
        let (objs, _) = commit(&two("x\n", "y\n"), None, 10).unwrap();
        let expected = Object::tree(&[
            ("a".repeat(40), objs[0].id()),
            ("b".repeat(40), objs[1].id()),
        ])
        .unwrap();
        assert_eq!(objs[objs.len() - 2].id(), expected.id());
    }

    #[test]
    fn the_commit_has_its_parent_and_lowercase_names() {
        let p = "c".repeat(40);
        let notes = vec![("A".repeat(40), "x\n".to_string())];
        let (objs, _) = commit(&notes, Some(&p), 10).unwrap();
        let body = String::from_utf8(objs[objs.len() - 1].body.clone()).unwrap();
        assert!(body.contains(&format!("parent {p}\n")), "{body}");
        assert!(body.ends_with("\nci: 1 notes\n"));
        let tree = &objs[objs.len() - 2].body;
        assert!(tree.windows(40).any(|w| w == "a".repeat(40).as_bytes()));
    }

    #[test]
    fn bad_and_duplicate_commit_ids_are_refused() {
        assert!(commit(&[("xyz".into(), "x".into())], None, 0).is_err());
        let dup = vec![
            ("a".repeat(40), "x".to_string()),
            ("A".repeat(40), "y".to_string()),
        ];
        assert!(commit(&dup, None, 0).is_err());
    }
}

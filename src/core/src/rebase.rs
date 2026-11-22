//! Path-level rebase for the Lander. A session branch's changed files are laid onto a main that
//! moved, one whole path at a time. There is no content merge here ([`crate::merge3`] does text
//! merges); a path that both sides changed is the caller's to refuse or merge.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::gitobj;
use crate::pack::{self, Entry, Objects};

/// The deepest tree nesting this module will walk.
const MAX_DEPTH: usize = 256;
/// The mode of a directory entry, without leading zeros.
const DIR_MODE: &str = "40000";
/// The mode of a gitlink entry.
const GITLINK_MODE: &str = "160000";

/// What happened at one file path between two commits: its new mode and blob id, or `None`
/// when it was deleted.
pub type Change = Option<(String, String)>;

/// A tree of pending changes, keyed by path component.
#[derive(Default)]
struct Node {
    /// The change at this exact path, if one was named.
    change: Option<Change>,
    /// Changes at paths below this one.
    children: BTreeMap<String, Node>,
}

/// Collects new tree objects, each once, in the order they were built.
struct Sink {
    out: Vec<gitobj::Object>,
    seen: HashSet<String>,
}

impl Sink {
    /// Keeps `object` unless one with the same id is already held; returns its id.
    fn put(&mut self, object: gitobj::Object) -> String {
        let id = object.id();
        if self.seen.insert(id.clone()) {
            self.out.push(object);
        }
        id
    }
}

/// One validated tree entry, ready to be sorted and encoded.
struct Row<'a> {
    key: Vec<u8>,
    mode: String,
    name: &'a str,
    raw: [u8; 20],
}

/// Every file path (blobs, symlinks and gitlinks, never directories) whose mode or id differs
/// between `from`'s tree and `to`'s tree, with what `to` holds there. Both commits and all their
/// trees must be in `objects`; blobs need not be. A directory replaced by a file, or the other
/// way round, yields the deletions of the old files and the additions of the new.
pub fn changes(
    objects: &Objects,
    from: &str,
    to: &str,
) -> Result<BTreeMap<String, Change>, String> {
    let old = commit_tree_of(objects, from)?;
    let new = commit_tree_of(objects, to)?;
    let mut out = BTreeMap::new();
    diff_trees(objects, "", Some(&old), Some(&new), 0, &mut out)?;
    Ok(out)
}

/// `onto`'s tree with `changes` applied: the new root tree's id and the tree objects that are
/// new (every rewritten tree on the way; untouched subtrees are reused by id). Directories left
/// empty are removed. Fails when a path's parent is a file in `onto`, or a change names a path
/// under a gitlink.
pub fn apply(
    objects: &Objects,
    onto: &str,
    changes: &BTreeMap<String, Change>,
) -> Result<(String, Vec<gitobj::Object>), String> {
    let base = commit_tree_of(objects, onto)?;
    let mut root = Node::default();
    for (path, change) in changes {
        insert(&mut root, path, change)?;
    }
    if changes.is_empty() {
        return Ok((base, Vec::new()));
    }
    let mut sink = Sink {
        out: Vec::new(),
        seen: HashSet::new(),
    };
    let id = match rebuild(objects, Some(&base), &root, "", 0, &mut sink)? {
        Some(id) => id,
        None => {
            let empty = tree(&[])?;
            sink.put(empty)
        }
    };
    Ok((id, sink.out))
}

/// A tree object from `entries` (mode, name, id), sorted as git sorts trees: by name bytes,
/// with a directory's name compared as if it ended in '/'.
pub fn tree(entries: &[(String, String, String)]) -> Result<gitobj::Object, String> {
    let mut names: HashSet<&str> = HashSet::new();
    let mut rows: Vec<Row> = Vec::with_capacity(entries.len());
    for (mode, name, id) in entries {
        check_name(name)?;
        if !names.insert(name.as_str()) {
            return Err(format!("duplicate name {name:?}"));
        }
        let mode = normalize_mode(mode)?;
        let raw = decode_id(id).map_err(|e| format!("{name:?}: {e}"))?;
        let mut key = name.as_bytes().to_vec();
        if mode == DIR_MODE {
            key.push(b'/');
        }
        rows.push(Row {
            key,
            mode,
            name: name.as_str(),
            raw,
        });
    }
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    let mut body = Vec::new();
    for row in &rows {
        body.extend_from_slice(row.mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(row.name.as_bytes());
        body.push(0);
        body.extend_from_slice(&row.raw);
    }
    Ok(gitobj::Object {
        kind: gitobj::Kind::Tree,
        body,
    })
}

/// The nearest common ancestor of `a` and `b` in `parents` (commit id to parent ids; a commit
/// missing from the map has no known parents), found by breadth-first search from `a` over
/// commits also reachable from `b`; `None` when none is found within the map.
pub fn merge_base(parents: &HashMap<String, Vec<String>>, a: &str, b: &str) -> Option<String> {
    let mut reach: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    reach.insert(b);
    queue.push_back(b);
    while let Some(c) = queue.pop_front() {
        if let Some(ps) = parents.get(c) {
            for p in ps {
                if reach.insert(p.as_str()) {
                    queue.push_back(p.as_str());
                }
            }
        }
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    seen.insert(a);
    queue.push_back(a);
    while let Some(c) = queue.pop_front() {
        if reach.contains(c) {
            return Some(c.to_owned());
        }
        if let Some(ps) = parents.get(c) {
            for p in ps {
                if seen.insert(p.as_str()) {
                    queue.push_back(p.as_str());
                }
            }
        }
    }
    None
}

/// The parents of each commit in `objects`, read from their bodies' `parent <id>` header lines.
pub fn parents(objects: &Objects) -> HashMap<String, Vec<String>> {
    let mut map = HashMap::new();
    for (id, (kind, body)) in &objects.by_id {
        if *kind != 1 {
            continue;
        }
        let mut list = Vec::new();
        for line in body.split(|b| *b == b'\n') {
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix(b"parent ")
                && let Ok(text) = std::str::from_utf8(rest)
            {
                list.push(text.to_owned());
            }
        }
        map.insert(id.clone(), list);
    }
    map
}

/// The rebased commit: `tree`, with `onto` as its only parent, carrying the original's author
/// line and message byte for byte, committed by `committer` at `unix_secs`. Other headers (a
/// signature among them) are dropped: the signature no longer holds.
pub fn commit(
    original_body: &[u8],
    tree: &str,
    onto: &str,
    committer: &str,
    unix_secs: i64,
) -> Result<gitobj::Object, String> {
    decode_id(tree).map_err(|e| format!("tree: {e}"))?;
    decode_id(onto).map_err(|e| format!("parent: {e}"))?;
    if committer.is_empty() || committer.contains(['\n', '\0']) {
        return Err("bad committer".to_owned());
    }
    let split = original_body.windows(2).position(|w| w == b"\n\n");
    let (head, message): (&[u8], &[u8]) = match split {
        Some(p) => (
            original_body.get(..p).unwrap_or_default(),
            original_body.get(p + 2..).unwrap_or_default(),
        ),
        None => (original_body, &[]),
    };
    let author = head
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"author "))
        .ok_or_else(|| "commit has no author line".to_owned())?;
    let mut body = Vec::new();
    body.extend_from_slice(format!("tree {tree}\nparent {onto}\nauthor ").as_bytes());
    body.extend_from_slice(author);
    body.extend_from_slice(format!("\ncommitter {committer} {unix_secs} +0000\n\n").as_bytes());
    body.extend_from_slice(message);
    Ok(gitobj::Object {
        kind: gitobj::Kind::Commit,
        body,
    })
}

/// The tree id of commit `id`, which must be in `objects`.
fn commit_tree_of(objects: &Objects, id: &str) -> Result<String, String> {
    let (kind, body) = objects
        .by_id
        .get(id)
        .ok_or_else(|| format!("commit {id} is missing"))?;
    if *kind != 1 {
        return Err(format!("object {id} is not a commit"));
    }
    pack::commit_tree(body)
}

/// Reads tree `id` into a map by entry name, checking names and duplicates.
fn read_tree(objects: &Objects, id: &str) -> Result<BTreeMap<String, Entry>, String> {
    let (kind, body) = objects
        .by_id
        .get(id)
        .ok_or_else(|| format!("tree {id} is missing"))?;
    if *kind != 2 {
        return Err(format!("object {id} is not a tree"));
    }
    let mut map = BTreeMap::new();
    for entry in pack::tree_entries(body)? {
        check_name(&entry.name)?;
        let name = entry.name.clone();
        if map.insert(name, entry).is_some() {
            return Err(format!("tree {id} has a duplicate name"));
        }
    }
    Ok(map)
}

/// Records the file-level differences between two optional trees under `prefix`.
fn diff_trees(
    objects: &Objects,
    prefix: &str,
    from: Option<&str>,
    to: Option<&str>,
    depth: usize,
    out: &mut BTreeMap<String, Change>,
) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err("tree is nested too deeply".to_owned());
    }
    let old = match from {
        Some(id) => read_tree(objects, id)?,
        None => BTreeMap::new(),
    };
    let new = match to {
        Some(id) => read_tree(objects, id)?,
        None => BTreeMap::new(),
    };
    let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for name in names {
        let path = join(prefix, name);
        let a = old.get(name);
        let b = new.get(name);
        let a_dir = a.filter(|e| is_dir(&e.mode));
        let b_dir = b.filter(|e| is_dir(&e.mode));
        let a_file = a.filter(|e| !is_dir(&e.mode));
        let b_file = b.filter(|e| !is_dir(&e.mode));
        if a_dir.is_some() || b_dir.is_some() {
            diff_trees(
                objects,
                &path,
                a_dir.map(|e| e.id.as_str()),
                b_dir.map(|e| e.id.as_str()),
                depth + 1,
                out,
            )?;
        }
        match (a_file, b_file) {
            (Some(x), Some(y)) => {
                if x.id != y.id || !same_mode(&x.mode, &y.mode) {
                    out.insert(path, Some((y.mode.clone(), y.id.clone())));
                }
            }
            (None, Some(y)) => {
                out.insert(path, Some((y.mode.clone(), y.id.clone())));
            }
            (Some(_), None) => {
                out.insert(path, None);
            }
            (None, None) => {}
        }
    }
    Ok(())
}

/// Adds one validated change to the change tree at `path`.
fn insert(root: &mut Node, path: &str, change: &Change) -> Result<(), String> {
    if let Some((mode, id)) = change {
        let mode = normalize_mode(mode).map_err(|e| format!("{path}: {e}"))?;
        if mode == DIR_MODE {
            return Err(format!("{path} is changed to a directory"));
        }
        decode_id(id).map_err(|e| format!("{path}: {e}"))?;
    }
    let mut node = root;
    for part in path.split('/') {
        check_name(part).map_err(|e| format!("path {path:?}: {e}"))?;
        node = node.children.entry(part.to_owned()).or_default();
    }
    node.change = Some(change.clone());
    Ok(())
}

/// Builds the tree for `base` with `node`'s changes applied; `None` when it ends up empty.
fn rebuild(
    objects: &Objects,
    base: Option<&str>,
    node: &Node,
    path: &str,
    depth: usize,
    sink: &mut Sink,
) -> Result<Option<String>, String> {
    if depth > MAX_DEPTH {
        return Err("tree is nested too deeply".to_owned());
    }
    let entries = match base {
        Some(id) => read_tree(objects, id)?,
        None => BTreeMap::new(),
    };
    let mut result: Vec<(String, String, String)> = Vec::new();
    for (name, entry) in &entries {
        match node.children.get(name) {
            None => result.push((entry.mode.clone(), name.clone(), entry.id.clone())),
            Some(child) => {
                let at = join(path, name);
                if let Some((mode, id)) = resolve(objects, Some(entry), child, &at, depth, sink)? {
                    result.push((mode, name.clone(), id));
                }
            }
        }
    }
    for (name, child) in &node.children {
        if entries.contains_key(name) {
            continue;
        }
        let at = join(path, name);
        if let Some((mode, id)) = resolve(objects, None, child, &at, depth, sink)? {
            result.push((mode, name.clone(), id));
        }
    }
    if result.is_empty() {
        return Ok(None);
    }
    let object = tree(&result)?;
    Ok(Some(sink.put(object)))
}

/// What one name holds after `node`'s changes: a (mode, id) pair, or nothing.
fn resolve(
    objects: &Objects,
    existing: Option<&Entry>,
    node: &Node,
    path: &str,
    depth: usize,
    sink: &mut Sink,
) -> Result<Option<(String, String)>, String> {
    let old_dir = existing.filter(|e| is_dir(&e.mode));
    let old_file = existing.filter(|e| !is_dir(&e.mode));
    if let Some(file) = old_file.filter(|_| node.change.is_none() && !node.children.is_empty()) {
        return Err(if file.mode.trim_start_matches('0') == GITLINK_MODE {
            format!("a change names a path under gitlink {path}")
        } else {
            format!("{path} is a file, so nothing can be changed under it")
        });
    }
    let base = old_dir.map(|e| e.id.as_str());
    let sub = if node.children.is_empty() {
        base.map(str::to_owned)
    } else {
        rebuild(objects, base, node, path, depth + 1, sink)?
    };
    let file = match &node.change {
        Some(Some((mode, id))) => Some((mode.clone(), id.clone())),
        Some(None) => None,
        None => old_file.map(|e| (e.mode.clone(), e.id.clone())),
    };
    match (file, sub) {
        (Some(_), Some(_)) => Err(format!("{path} would be both a file and a directory")),
        (Some(file), None) => Ok(Some(file)),
        (None, Some(id)) => Ok(Some((DIR_MODE.to_owned(), id))),
        (None, None) => Ok(None),
    }
}

/// Joins a directory path and a name with '/'.
fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Whether `mode` is the directory mode.
fn is_dir(mode: &str) -> bool {
    mode.trim_start_matches('0') == DIR_MODE
}

/// Whether two mode strings are equal ignoring leading zeros.
fn same_mode(a: &str, b: &str) -> bool {
    a.trim_start_matches('0') == b.trim_start_matches('0')
}

/// Checks that `name` is usable as one tree entry name.
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        Err("empty name".to_owned())
    } else if name.contains(['/', '\0']) {
        Err(format!("name {name:?} contains '/' or NUL"))
    } else if name == "." || name == ".." {
        Err(format!("name {name:?} is not allowed"))
    } else {
        Ok(())
    }
}

/// The mode without leading zeros, when it is one git writes in trees.
fn normalize_mode(mode: &str) -> Result<String, String> {
    let m = mode.trim_start_matches('0');
    match m {
        "40000" | "100644" | "100755" | "120000" | "160000" => Ok(m.to_owned()),
        _ => Err(format!("unknown mode {mode:?}")),
    }
}

/// A 40-character hex id as its 20 bytes.
fn decode_id(id: &str) -> Result<[u8; 20], String> {
    let bytes = crate::sign::unhex(id)
        .filter(|b| b.len() == 20)
        .ok_or_else(|| format!("not an object id: {id:?}"))?;
    let mut raw = [0u8; 20];
    raw.copy_from_slice(&bytes);
    Ok(raw)
}

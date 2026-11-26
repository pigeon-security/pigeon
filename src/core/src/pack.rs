//! Git packs, read far enough to know a push's history: the commits in a pack and their
//! parents, deltas resolved within the pack, and whether one commit descends from another.

use std::collections::{HashMap, HashSet, VecDeque};

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::inflate_flags::{
    TINFL_FLAG_PARSE_ZLIB_HEADER, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
};
use miniz_oxide::inflate::core::{DecompressorOxide, decompress};
use sha1::{Digest, Sha1};

/// How much a pack may inflate to, in all, deltas included.
pub const INFLATED_MAX: usize = 48 * 1024 * 1024;

/// The commits found in a pack.
pub struct Commits {
    /// Each commit's parents, in order, by commit ID.
    pub parents: HashMap<String, Vec<String>>,
    /// Deltas whose base is outside the pack (a thin pack), so what they are is unknown.
    pub unresolved: usize,
}

/// Where a delta's base is.
enum Base {
    None,
    /// At this offset in the pack.
    Offset(usize),
    /// The object with this ID.
    Id(String),
}

struct Object {
    start: usize,
    /// 1 to 4 once known (commit, tree, blob, tag); 6 or 7 while an unresolved delta.
    kind: u8,
    base: Base,
    data: Vec<u8>,
    id: Option<String>,
}

/// Every object in a pack, deltas resolved within it.
pub struct Objects {
    /// Each resolved object's kind (1 commit, 2 tree, 3 blob, 4 tag) and data, by ID.
    pub by_id: HashMap<String, (u8, Vec<u8>)>,
    /// Deltas whose base is outside the pack (a thin pack), so what they are is unknown.
    pub unresolved: usize,
}

/// The commits in `pack` with their parents. The pack is untrusted: anything malformed, or
/// inflating past [`INFLATED_MAX`], is an error.
pub fn commits(pack: &[u8]) -> Result<Commits, String> {
    let objects = objects(pack)?;
    let mut parents_of = HashMap::new();
    for (id, (kind, data)) in &objects.by_id {
        if *kind == 1 {
            parents_of.insert(id.clone(), parents(data)?);
        }
    }
    Ok(Commits {
        parents: parents_of,
        unresolved: objects.unresolved,
    })
}

/// Every object in `pack`, deltas resolved where their base is in the pack. The pack is
/// untrusted: anything malformed, or inflating past [`INFLATED_MAX`], is an error.
pub fn objects(pack: &[u8]) -> Result<Objects, String> {
    scan(pack, false, INFLATED_MAX)
}

/// How much all objects of a pack may inflate to when only commits are kept. This is
/// counted, never stored.
pub const SCAN_MAX: usize = 512 * 1024 * 1024;

/// The commits in `pack` with their parents, for a pack that carries trees and blobs too.
/// Trees, blobs and deltas of them are inflated to find the next object, then dropped, so
/// only commits count toward [`INFLATED_MAX`]. The inflated total of all objects may reach
/// [`SCAN_MAX`], and no single object may pass [`INFLATED_MAX`].
pub fn commit_parents(pack: &[u8]) -> Result<Commits, String> {
    commit_parents_with(pack, INFLATED_MAX)
}

/// [`commit_parents`] with the cap on what is kept given, so a test can use a small one.
pub fn commit_parents_with(pack: &[u8], kept_max: usize) -> Result<Commits, String> {
    let objects = scan(pack, true, kept_max)?;
    let mut parents_of = HashMap::new();
    for (id, (kind, data)) in &objects.by_id {
        if *kind == 1 {
            parents_of.insert(id.clone(), parents(data)?);
        }
    }
    Ok(Commits {
        parents: parents_of,
        unresolved: objects.unresolved,
    })
}

/// Reads `pack`, keeping every object, or with `only_commits` just commits and deltas on
/// kept objects. At most `max` bytes are kept.
fn scan(pack: &[u8], only_commits: bool, max: usize) -> Result<Objects, String> {
    if pack.len() < 32 || pack.get(..4) != Some(b"PACK".as_slice()) {
        return Err(bad("no pack header"));
    }
    let word = |at: usize| -> usize {
        pack.get(at..at + 4)
            .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    if !matches!(word(4), 2 | 3) {
        return Err(bad("unknown pack version"));
    }
    let count = word(8);
    // The objects, then a 20-byte trailer.
    let end = pack.len() - 20;
    let body = &pack[..end];
    let mut objects: Vec<Object> = Vec::new();
    let mut total = 0usize;
    let mut all = 0usize;
    let mut kept_starts: HashSet<usize> = HashSet::new();
    let mut kept_ids: HashSet<String> = HashSet::new();
    let mut at = 12;
    for _ in 0..count {
        if at >= end {
            return Err(bad("fewer objects than counted"));
        }
        let start = at;
        let (kind, size, next) = header(body, at)?;
        let (base, next) = match kind {
            1..=4 => (Base::None, next),
            6 => {
                let (back, next) = ofs(body, next)?;
                let base = start
                    .checked_sub(back)
                    .filter(|b| back > 0 && *b >= 12)
                    .ok_or_else(|| bad("a delta's base is out of range"))?;
                (Base::Offset(base), next)
            }
            7 => {
                let raw = body
                    .get(next..next + 20)
                    .ok_or_else(|| bad("truncated delta base"))?;
                (Base::Id(hex(raw)), next + 20)
            }
            _ => return Err(bad("unknown object type")),
        };
        // No single object may pass INFLATED_MAX; this is checked before anything is inflated.
        if size > INFLATED_MAX {
            return Err(bad("it inflates too large"));
        }
        let keep = !only_commits
            || kind == 1
            || match &base {
                Base::Offset(p) => kept_starts.contains(p),
                Base::Id(i) => kept_ids.contains(i),
                Base::None => false,
            };
        all = all
            .checked_add(size)
            .filter(|t| *t <= if only_commits { SCAN_MAX } else { max })
            .ok_or_else(|| bad("it inflates too large"))?;
        if keep {
            total = total
                .checked_add(size)
                .filter(|t| *t <= max)
                .ok_or_else(|| bad("it inflates too large"))?;
        }
        let (data, next) = inflate(body, next, size)?;
        if !keep {
            at = next;
            continue;
        }
        let id = if base_known(kind) {
            Some(id(kind, &data)?)
        } else {
            None
        };
        kept_starts.insert(start);
        if let Some(i) = &id {
            kept_ids.insert(i.clone());
        }
        objects.push(Object {
            start,
            kind,
            base,
            data,
            id,
        });
        at = next;
    }
    if at != end {
        return Err(bad("bytes left after the objects"));
    }
    let by_start: HashMap<usize, usize> = objects
        .iter()
        .enumerate()
        .map(|(i, o)| (o.start, i))
        .collect();
    let mut by_id: HashMap<String, usize> = objects
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.id.clone().map(|id| (id, i)))
        .collect();
    // Resolve deltas whose base is resolved, until a pass resolves none.
    loop {
        let mut progress = false;
        for i in 0..objects.len() {
            if base_known(objects[i].kind) {
                continue;
            }
            let j = match &objects[i].base {
                Base::Offset(p) => by_start.get(p).copied(),
                Base::Id(id) => by_id.get(id).copied(),
                Base::None => None,
            };
            let Some(j) = j.filter(|j| base_known(objects[*j].kind)) else {
                continue;
            };
            let data = apply_delta(&objects[j].data, &objects[i].data)?;
            total = total
                .checked_add(data.len())
                .filter(|t| *t <= max)
                .ok_or_else(|| bad("it inflates too large"))?;
            let kind = objects[j].kind;
            let oid = id(kind, &data)?;
            by_id.insert(oid.clone(), i);
            let o = &mut objects[i];
            o.kind = kind;
            o.data = data;
            o.id = Some(oid);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    let mut out = Objects {
        by_id: HashMap::new(),
        unresolved: 0,
    };
    for o in objects {
        match o.id {
            Some(id) if base_known(o.kind) => {
                out.by_id.insert(id, (o.kind, o.data));
            }
            _ => out.unresolved += 1,
        }
    }
    Ok(out)
}

/// What a walk from a new commit toward an old one found.
#[derive(Debug, PartialEq, Eq)]
pub enum Walk {
    /// The old commit is an ancestor of the new one, or is it.
    Forward,
    /// It is not.
    NotForward,
    /// These commits are needed to tell: they are neither known nor the old commit.
    Need(Vec<String>),
}

/// Whether `new` descends from `old`, walking parents through `known`. A commit not in
/// `known` is answered as needed, unless `last`: then it is taken to be one the old commit
/// already reaches, which is what an upstream answer that left out `old`'s history means.
pub fn walk(old: &str, new: &str, known: &HashMap<String, Vec<String>>, last: bool) -> Walk {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([new]);
    let mut need: Vec<String> = Vec::new();
    while let Some(c) = queue.pop_front() {
        if c == old {
            return Walk::Forward;
        }
        if !seen.insert(c) {
            continue;
        }
        match known.get(c) {
            Some(ps) => queue.extend(ps.iter().map(String::as_str)),
            None => need.push(c.to_string()),
        }
    }
    if last || need.is_empty() {
        Walk::NotForward
    } else {
        need.sort();
        Walk::Need(need)
    }
}

/// Whether an object of `kind` is resolved: not a delta.
fn base_known(kind: u8) -> bool {
    (1..=4).contains(&kind)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bad(what: &str) -> String {
    format!("the pack is malformed: {what}")
}

/// The byte at `at`, or an error naming `what` was cut short.
fn byte(data: &[u8], at: usize, what: &str) -> Result<u8, String> {
    data.get(at).copied().ok_or_else(|| bad(what))
}

/// An object's header at `at`: its type, inflated size, and where what follows starts.
fn header(data: &[u8], at: usize) -> Result<(u8, usize, usize), String> {
    let mut pos = at;
    let mut b = byte(data, pos, "truncated object header")?;
    pos += 1;
    let kind = (b >> 4) & 7;
    let mut size = (b & 0x0f) as u64;
    let mut shift: u32 = 4;
    while b & 0x80 != 0 {
        if shift > 57 {
            return Err(bad("object header too long"));
        }
        b = byte(data, pos, "truncated object header")?;
        pos += 1;
        size = size
            .checked_add(u64::from(b & 0x7f) << shift)
            .filter(|s| *s <= INFLATED_MAX as u64)
            .ok_or_else(|| bad("object too large"))?;
        shift += 7;
    }
    Ok((kind, size as usize, pos))
}

/// An offset delta's distance back to its base, and where what follows starts.
fn ofs(data: &[u8], at: usize) -> Result<(usize, usize), String> {
    let mut pos = at;
    let mut b = byte(data, pos, "truncated delta offset")?;
    pos += 1;
    let mut o = (b & 0x7f) as usize;
    while b & 0x80 != 0 {
        b = byte(data, pos, "truncated delta offset")?;
        pos += 1;
        o = o
            .checked_add(1)
            .and_then(|v| v.checked_mul(128))
            .map(|v| v | (b & 0x7f) as usize)
            .ok_or_else(|| bad("delta offset overflow"))?;
    }
    Ok((o, pos))
}

/// The zlib stream at `at` inflated, which must be exactly `size` bytes, and where it ends.
fn inflate(data: &[u8], at: usize, size: usize) -> Result<(Vec<u8>, usize), String> {
    let input = data
        .get(at..)
        .ok_or_else(|| bad("object offset out of range"))?;
    // One byte more than declared, so a stream that runs long is caught.
    let mut out = vec![0u8; size + 1];
    let mut state = DecompressorOxide::new();
    let flags = TINFL_FLAG_PARSE_ZLIB_HEADER | TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, consumed, written) = decompress(&mut state, input, &mut out, 0, flags);
    if status != TINFLStatus::Done || written != size {
        return Err(bad("an object does not inflate to its size"));
    }
    out.truncate(size);
    Ok((out, at + consumed))
}

/// A delta's size field at `at`: little-endian groups of 7 bits.
fn delta_size(d: &[u8], at: usize) -> Result<(usize, usize), String> {
    let mut pos = at;
    let mut size: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        if shift > 57 {
            return Err(bad("delta size too long"));
        }
        let b = byte(d, pos, "truncated delta size")?;
        pos += 1;
        size = size
            .checked_add(u64::from(b & 0x7f) << shift)
            .filter(|s| *s <= INFLATED_MAX as u64)
            .ok_or_else(|| bad("delta too large"))?;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    Ok((size as usize, pos))
}

/// `delta` applied to `base`.
fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    let (source, pos) = delta_size(delta, 0)?;
    if source != base.len() {
        return Err(bad("a delta's base is not the size it expects"));
    }
    let (target, mut pos) = delta_size(delta, pos)?;
    let mut out: Vec<u8> = Vec::with_capacity(target);
    while pos < delta.len() {
        let op = byte(delta, pos, "truncated delta")?;
        pos += 1;
        let chunk = if op & 0x80 != 0 {
            let mut off: usize = 0;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    off |= (byte(delta, pos, "truncated copy")? as usize) << (8 * i);
                    pos += 1;
                }
            }
            let mut len: usize = 0;
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    len |= (byte(delta, pos, "truncated copy")? as usize) << (8 * i);
                    pos += 1;
                }
            }
            if len == 0 {
                len = 0x10000;
            }
            let end = off.checked_add(len).ok_or_else(|| bad("copy overflow"))?;
            base.get(off..end)
                .ok_or_else(|| bad("a copy past the base"))?
        } else if op != 0 {
            let end = pos + op as usize;
            let chunk = delta.get(pos..end).ok_or_else(|| bad("truncated insert"))?;
            pos = end;
            chunk
        } else {
            return Err(bad("delta opcode 0"));
        };
        if out.len() + chunk.len() > target {
            return Err(bad("a delta runs past its size"));
        }
        out.extend_from_slice(chunk);
    }
    if out.len() != target {
        return Err(bad("a delta falls short of its size"));
    }
    Ok(out)
}

/// The ID of an object of `kind` holding `data`: SHA-1 of its header and data.
fn id(kind: u8, data: &[u8]) -> Result<String, String> {
    let name = match kind {
        1 => "commit",
        2 => "tree",
        3 => "blob",
        4 => "tag",
        _ => return Err(bad("unknown object type")),
    };
    let mut h = Sha1::new();
    h.update(format!("{name} {}\0", data.len()).as_bytes());
    h.update(data);
    Ok(hex(&h.finalize()))
}

/// A commit's parents, from its header.
fn parents(data: &[u8]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(b"parent ".as_slice()) {
            let parent = std::str::from_utf8(rest)
                .ok()
                .filter(|p| crate::git::is_id(p))
                .ok_or_else(|| bad("a commit's parent is not an object ID"))?;
            out.push(parent.to_string());
        }
    }
    Ok(out)
}

// ---- reading a file at a commit ----

/// One entry of a tree.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its mode in ASCII octal, such as `100644` or `40000`.
    pub mode: String,
    pub name: String,
    /// Its object's ID, 40 lowercase hex.
    pub id: String,
}

/// A tree object's entries.
pub fn tree_entries(data: &[u8]) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| bad("a tree entry has no mode"))?;
        let mode_bytes = rest
            .get(..sp)
            .ok_or_else(|| bad("a tree entry is truncated"))?;
        if mode_bytes.is_empty()
            || mode_bytes.len() > 6
            || !mode_bytes.iter().all(|b| (b'0'..=b'7').contains(b))
        {
            return Err(bad("a tree entry has a bad mode"));
        }
        let mode = String::from_utf8_lossy(mode_bytes).into_owned();
        rest = rest
            .get(sp + 1..)
            .ok_or_else(|| bad("a tree entry is truncated"))?;
        let nul = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| bad("a tree entry's name is unterminated"))?;
        let name = rest
            .get(..nul)
            .and_then(|n| std::str::from_utf8(n).ok())
            .ok_or_else(|| bad("a tree entry's name is not UTF-8"))?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(bad("a tree entry has a bad name"));
        }
        let name = name.to_string();
        rest = rest
            .get(nul + 1..)
            .ok_or_else(|| bad("a tree entry is truncated"))?;
        let id = rest
            .get(..20)
            .ok_or_else(|| bad("a tree entry's id is truncated"))?;
        let id = hex(id);
        rest = rest.get(20..).unwrap_or_default();
        out.push(Entry { mode, name, id });
    }
    Ok(out)
}

/// The ID of a commit's tree, from its first header line.
pub fn commit_tree(data: &[u8]) -> Result<String, String> {
    let end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
    let id = data
        .get(..end)
        .and_then(|line| line.strip_prefix(b"tree ".as_slice()))
        .and_then(|id| std::str::from_utf8(id).ok())
        .filter(|id| crate::git::is_id(id))
        .ok_or_else(|| bad("a commit does not start with its tree"))?;
    Ok(id.to_string())
}

/// The regular file at `path` (such as `.pigeon/ci.toml`) in `commit`'s tree, from `objects`;
/// `None` when there is no regular file there.
pub fn read_path(objects: &Objects, commit: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
    let parts: Vec<&str> = path.split('/').collect();
    if path.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err("not a file path".to_string());
    }
    let Some((1, commit_data)) = objects.by_id.get(commit) else {
        return Err("the commit is not in the pack".to_string());
    };
    let mut tree_id = commit_tree(commit_data)?;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        let Some((2, tree_data)) = objects.by_id.get(&tree_id) else {
            return Err("a tree is not in the pack".to_string());
        };
        let Some(entry) = tree_entries(tree_data)?
            .into_iter()
            .find(|e| e.name == *part)
        else {
            return Ok(None);
        };
        if i < last {
            if entry.mode != "40000" {
                return Ok(None);
            }
            tree_id = entry.id;
        } else {
            if entry.mode != "100644" && entry.mode != "100755" {
                return Ok(None);
            }
            return match objects.by_id.get(&entry.id) {
                Some((3, data)) => Ok(Some(data.clone())),
                _ => Err("a blob is not in the pack".to_string()),
            };
        }
    }
    Ok(None)
}

/// The ID of what `path` names in `commit`'s tree, from `objects`: a file's blob or a
/// directory's tree, `None` when nothing is there. Two commits with the same ID at a path hold
/// the same content there. Blobs need not be in `objects`; a tree on the way that is missing is
/// an error, never read as nothing.
pub fn path_id(objects: &Objects, commit: &str, path: &str) -> Result<Option<String>, String> {
    let parts: Vec<&str> = path.split('/').collect();
    if path.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err("not a path".to_string());
    }
    let Some((1, commit_data)) = objects.by_id.get(commit) else {
        return Err("the commit is not in the pack".to_string());
    };
    let mut id = commit_tree(commit_data)?;
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        let Some((2, tree_data)) = objects.by_id.get(&id) else {
            return Err("a tree is not in the pack".to_string());
        };
        let Some(entry) = tree_entries(tree_data)?
            .into_iter()
            .find(|e| e.name == *part)
        else {
            return Ok(None);
        };
        // A file where a directory was expected: nothing is at the path.
        if i < last && entry.mode != "40000" {
            return Ok(None);
        }
        id = entry.id;
    }
    Ok(Some(id))
}

// ---- reading a directory at a commit ----

/// How much of a directory [`read_tree`] reads before it refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLimits {
    /// The most regular files.
    pub max_files: usize,
    /// The most bytes, all files together.
    pub max_bytes: usize,
    /// The most directories deep below the root.
    pub max_depth: usize,
    /// The longest path, relative to the root, in bytes.
    pub max_path: usize,
}

/// Whether `path` is a relative directory path: parts split by `/`, none empty, `.` or `..`,
/// and no backslash or control character anywhere.
fn relative_dir(path: &str) -> bool {
    !path.is_empty()
        && !path.chars().any(|c| c == '\\' || c.is_control())
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Every regular file under directory `root` (the whole tree when `None`) of `commit`'s tree,
/// from `objects`: its path relative to `root` and its bytes, sorted by path. The tree is
/// untrusted: a root that is not a relative path or not a directory, an entry name holding a
/// backslash or control character, a symbolic link or submodule, a missing object, or a tree
/// past `limits`, is an error and nothing is answered.
pub fn read_tree(
    objects: &Objects,
    commit: &str,
    root: Option<&str>,
    limits: &TreeLimits,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let Some((1, commit_data)) = objects.by_id.get(commit) else {
        return Err("the commit is not in the pack".to_string());
    };
    let mut tree_id = commit_tree(commit_data)?;
    if let Some(root) = root {
        if !relative_dir(root) {
            return Err(format!("the directory `{root}` is not a relative path"));
        }
        for part in root.split('/') {
            let Some((2, data)) = objects.by_id.get(&tree_id) else {
                return Err("a tree is not in the pack".to_string());
            };
            match tree_entries(data)?.into_iter().find(|e| e.name == part) {
                Some(e) if e.mode == "40000" => tree_id = e.id,
                _ => return Err(format!("the commit has no directory `{root}`")),
            }
        }
    }
    let mut out = Vec::new();
    let mut bytes = 0usize;
    // (tree, its path relative to the root with a trailing slash or empty, its depth)
    let mut todo = vec![(tree_id, String::new(), 0usize)];
    while let Some((id, prefix, depth)) = todo.pop() {
        let Some((2, data)) = objects.by_id.get(&id) else {
            return Err("a tree is not in the pack".to_string());
        };
        for entry in tree_entries(data)? {
            if entry.name.chars().any(|c| c == '\\' || c.is_control()) {
                return Err(format!(
                    "`{prefix}{}` has a backslash or control character in its name",
                    entry.name.escape_debug()
                ));
            }
            let path = format!("{prefix}{}", entry.name);
            if path.len() > limits.max_path {
                return Err(format!("`{path}` is longer than {} bytes", limits.max_path));
            }
            match entry.mode.as_str() {
                "40000" => {
                    if depth + 1 > limits.max_depth {
                        return Err(format!(
                            "`{path}` is more than {} directories deep",
                            limits.max_depth
                        ));
                    }
                    todo.push((entry.id, format!("{path}/"), depth + 1));
                }
                "100644" | "100755" => {
                    let Some((3, blob)) = objects.by_id.get(&entry.id) else {
                        return Err(format!("the blob of `{path}` is not in the pack"));
                    };
                    if out.len() + 1 > limits.max_files {
                        return Err(format!(
                            "the directory holds more than {} files",
                            limits.max_files
                        ));
                    }
                    bytes = bytes.saturating_add(blob.len());
                    if bytes > limits.max_bytes {
                        return Err(format!(
                            "the directory holds more than {} bytes",
                            limits.max_bytes
                        ));
                    }
                    out.push((path, blob.clone()));
                }
                "120000" => return Err(format!("`{path}` is a symbolic link")),
                "160000" => return Err(format!("`{path}` is a submodule")),
                other => return Err(format!("`{path}` has an unknown mode {other}")),
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

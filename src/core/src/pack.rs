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
        total = total
            .checked_add(size)
            .filter(|t| *t <= INFLATED_MAX)
            .ok_or_else(|| bad("it inflates too large"))?;
        let (data, next) = inflate(body, next, size)?;
        let id = if base_known(kind) {
            Some(id(kind, &data)?)
        } else {
            None
        };
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
                .filter(|t| *t <= INFLATED_MAX)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A scratch repository, run with `git`.
    struct Repo(std::path::PathBuf);

    impl Repo {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("pigeon-pack-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let r = Self(dir);
            r.git(&["init", "-q", "-b", "main"]);
            r.git(&["config", "user.email", "t@example.com"]);
            r.git(&["config", "user.name", "t"]);
            r
        }

        fn git(&self, args: &[&str]) -> String {
            self.git_in(args, None)
        }

        fn git_in(&self, args: &[&str], input: Option<&str>) -> String {
            use std::io::Write;
            let mut child = Command::new("git")
                .args(args)
                .current_dir(&self.0)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            if let Some(i) = input {
                child.stdin.take().unwrap().write_all(i.as_bytes()).unwrap();
            }
            drop(child.stdin.take());
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// A commit changing `file` to `body`, on the current branch.
        fn commit(&self, file: &str, body: &str) -> String {
            std::fs::write(self.0.join(file), body).unwrap();
            self.git(&["add", file]);
            self.git(&["commit", "-q", "-m", file]);
            self.git(&["rev-parse", "HEAD"])
        }

        /// The pack `git pack-objects` makes for `new` less `old`'s history.
        fn pack(&self, new: &str, old: Option<&str>, extra: &[&str]) -> Vec<u8> {
            let mut revs = format!("{new}\n");
            if let Some(o) = old {
                revs.push_str(&format!("^{o}\n"));
            }
            let mut args = vec!["pack-objects", "--stdout", "--revs", "-q"];
            args.extend(extra);
            let out = Command::new("git")
                .args(&args)
                .current_dir(&self.0)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut c| {
                    use std::io::Write;
                    c.stdin.take().unwrap().write_all(revs.as_bytes())?;
                    c.wait_with_output()
                })
                .unwrap();
            assert!(out.status.success());
            out.stdout
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_the_commits_of_a_pack_git_made() {
        let r = Repo::new("plain");
        let a = r.commit("f", "one\n");
        let b = r.commit("f", &"two\n".repeat(200));
        let c = r.commit("f", &format!("{}three\n", "two\n".repeat(200)));
        for extra in [&[][..], &["--delta-base-offset"][..]] {
            let got = commits(&r.pack(&c, None, extra)).unwrap();
            assert_eq!(got.unresolved, 0);
            assert_eq!(got.parents.len(), 3);
            assert_eq!(got.parents[&c], std::slice::from_ref(&b));
            assert_eq!(got.parents[&b], std::slice::from_ref(&a));
            assert!(got.parents[&a].is_empty());
        }
    }

    #[test]
    fn a_thin_pack_counts_what_it_cannot_resolve() {
        let r = Repo::new("thin");
        let a = r.commit("f", &"line\n".repeat(500));
        let b = r.commit("f", &format!("{}more\n", "line\n".repeat(500)));
        let got = commits(&r.pack(&b, Some(&a), &["--thin"])).unwrap();
        assert_eq!(got.parents.len(), 1);
        assert_eq!(got.parents[&b], [a]);
        assert_eq!(
            got.unresolved, 1,
            "the blob is a delta against one outside the pack"
        );
    }

    #[test]
    fn an_empty_pack_has_no_commits() {
        let r = Repo::new("empty");
        let a = r.commit("f", "x");
        let got = commits(&r.pack(&a, Some(&a), &[])).unwrap();
        assert!(got.parents.is_empty());
    }

    #[test]
    fn malformed_packs_are_refused() {
        let r = Repo::new("bad");
        let a = r.commit("f", "x");
        let good = r.pack(&a, None, &[]);
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"PACK".to_vec(),
            good[..good.len() - 1].to_vec(),
        ];
        let mut extra = good.clone();
        extra.insert(30, 0);
        cases.push(extra);
        let mut count = good.clone();
        count[11] += 1;
        cases.push(count);
        let mut version = good.clone();
        version[7] = 9;
        cases.push(version);
        for (i, c) in cases.iter().enumerate() {
            assert!(commits(c).is_err(), "case {i}");
        }
        // Corrupt bytes anywhere must not panic.
        for i in 12..good.len() - 20 {
            let mut c = good.clone();
            c[i] ^= 0x5a;
            let _ = commits(&c);
        }
    }

    #[test]
    fn deltas_apply_and_refuse_bad_copies() {
        // source 3, target 5: copy 3 bytes from 0, insert "de".
        let delta = [3, 5, 0x90, 3, 2, b'd', b'e'];
        assert_eq!(apply_delta(b"abc", &delta).unwrap(), b"abcde");
        assert!(apply_delta(b"abcd", &delta).is_err());
        assert!(apply_delta(b"abc", &[3, 5, 0x91, 1, 3, 2, b'd', b'e']).is_err());
        assert!(apply_delta(b"abc", &[3, 1, 0]).is_err());
        assert!(apply_delta(b"abc", &[3, 1, 2, b'a', b'b']).is_err());
    }

    fn graph(edges: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        edges
            .iter()
            .map(|(c, ps)| (c.to_string(), ps.iter().map(|p| p.to_string()).collect()))
            .collect()
    }

    #[test]
    fn walks_find_forward_and_not() {
        // o <- a <- n ; x <- m(merge of a and x)
        let g = graph(&[("n", &["a"]), ("a", &["o"]), ("m", &["a", "x"])]);
        assert_eq!(walk("o", "n", &g, false), Walk::Forward);
        assert_eq!(walk("o", "o", &g, false), Walk::Forward);
        assert_eq!(walk("o", "m", &g, false), Walk::Forward);
        assert_eq!(walk("n", "a", &g, false), Walk::Need(vec!["o".into()]));
        assert_eq!(walk("n", "a", &g, true), Walk::NotForward);
        // A rewritten history: n2's parent is p, which upstream left out.
        let g = graph(&[("n2", &["p"])]);
        assert_eq!(walk("o", "n2", &g, false), Walk::Need(vec!["p".into()]));
        assert_eq!(walk("o", "n2", &g, true), Walk::NotForward);
        assert_eq!(
            walk("o", "n2", &HashMap::new(), false),
            Walk::Need(vec!["n2".into()])
        );
        let g = graph(&[("r", &[])]);
        assert_eq!(walk("o", "r", &g, false), Walk::NotForward);
    }

    #[test]
    fn walks_survive_cycles() {
        let g = graph(&[("a", &["b"]), ("b", &["a"])]);
        assert_eq!(walk("o", "a", &g, true), Walk::NotForward);
    }
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

#[cfg(test)]
mod path_tests {
    use super::*;

    fn id_of(kind_name: &str, data: &[u8]) -> String {
        let mut h = Sha1::new();
        h.update(format!("{kind_name} {}\0", data.len()).as_bytes());
        h.update(data);
        hex(&h.finalize())
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn tree(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (mode, name, id) in entries {
            out.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            out.extend_from_slice(&unhex(id));
        }
        out
    }

    fn setup() -> (Objects, String) {
        let a = b"[jobs]\n".to_vec();
        let a_id = id_of("blob", &a);
        let readme = b"hello\n".to_vec();
        let readme_id = id_of("blob", &readme);
        let link = b"ci.toml".to_vec();
        let link_id = id_of("blob", &link);
        let pigeon = tree(&[("100644", "ci.toml", &a_id), ("120000", "link", &link_id)]);
        let pigeon_id = id_of("tree", &pigeon);
        let root = tree(&[
            ("40000", ".pigeon", &pigeon_id),
            ("100644", "README", &readme_id),
        ]);
        let root_id = id_of("tree", &root);
        let commit = format!("tree {root_id}\nauthor x\n\nmsg").into_bytes();
        let commit_id = id_of("commit", &commit);
        let mut by_id = HashMap::new();
        by_id.insert(a_id, (3, a));
        by_id.insert(readme_id, (3, readme));
        by_id.insert(link_id, (3, link));
        by_id.insert(pigeon_id, (2, pigeon));
        by_id.insert(root_id, (2, root));
        by_id.insert(commit_id.clone(), (1, commit));
        (
            Objects {
                by_id,
                unresolved: 0,
            },
            commit_id,
        )
    }

    #[test]
    fn reads_regular_files_only() {
        let (o, c) = setup();
        assert_eq!(
            read_path(&o, &c, ".pigeon/ci.toml").unwrap(),
            Some(b"[jobs]\n".to_vec())
        );
        assert_eq!(
            read_path(&o, &c, "README").unwrap(),
            Some(b"hello\n".to_vec())
        );
        for none in [
            "nope",
            ".pigeon",
            ".pigeon/link",
            "README/x",
            ".pigeon/nope",
        ] {
            assert_eq!(read_path(&o, &c, none).unwrap(), None, "{none}");
        }
    }

    #[test]
    fn refuses_bad_paths_and_missing_objects() {
        let (o, c) = setup();
        for p in ["/a", "a/", "a//b", "../a", "./a", ""] {
            assert!(read_path(&o, &c, p).is_err(), "{p}");
        }
        assert!(read_path(&o, &"0".repeat(40), "README").is_err());
        let id = "ab".repeat(20);
        for t in [
            tree(&[("100644", "a/b", &id)]),
            tree(&[("100644", "", &id)]),
            tree(&[("9", "a", &id)]),
            tree(&[("100644", "..", &id)]),
            tree(&[("100644", "a", &id)])[..10].to_vec(),
        ] {
            assert!(tree_entries(&t).is_err());
        }
        // A tree whose subtree is missing from the pack.
        let mut by_id = o.by_id.clone();
        by_id.retain(|_, (k, d)| !(*k == 2 && d.starts_with(b"100644 ci.toml")));
        let gone = Objects {
            by_id,
            unresolved: 0,
        };
        assert!(read_path(&gone, &c, ".pigeon/ci.toml").is_err());
    }

    /// Reads a file at a commit from a pack `git pack-objects` made, deltas and all.
    #[test]
    fn reads_a_file_from_a_real_pack() {
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!("pigeon-read-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".pigeon")).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        let body = "[jobs.test]\nrun = \"cargo test\"\n".repeat(40);
        std::fs::write(dir.join(".pigeon/ci.toml"), &body).unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        let changed = format!("{body}# changed\n");
        std::fs::write(dir.join(".pigeon/ci.toml"), &changed).unwrap();
        git(&["commit", "-q", "-am", "two"]);
        let head = git(&["rev-parse", "HEAD"]);
        let mut child = Command::new("git")
            .args([
                "pack-objects",
                "--stdout",
                "--revs",
                "-q",
                "--delta-base-offset",
            ])
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(format!("{head}\n").as_bytes()).unwrap();
        }
        let pack = child.wait_with_output().unwrap().stdout;
        let objects = objects(&pack).unwrap();
        assert_eq!(objects.unresolved, 0);
        let got = read_path(&objects, &head, ".pigeon/ci.toml").unwrap();
        assert_eq!(got.as_deref(), Some(changed.as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

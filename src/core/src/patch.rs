//! A branch as one unified patch that `git apply --3way` takes. Pure: no I/O, so it runs
//! natively in tests. A break-glass session that cannot reach Artifacts gets a branch's code
//! this way. Files that must not travel in a patch (binary, submodules, symlinks, secrets, too
//! large) are left out and named in a `skipped` list, never dropped silently. A patch is never
//! cut short: over [`PATCH_MAX`] it is an error.

use serde::Serialize;

use crate::merge3::matching;

/// Lines of context around each change in a hunk.
pub const CONTEXT: usize = 3;
/// The largest file side (old or new) that is diffed, in bytes.
pub const FILE_MAX: usize = 256 * 1024;
/// The largest whole patch, in bytes (1 MiB). A bigger one is an error, not a cut-off patch.
pub const PATCH_MAX: usize = 1024 * 1024;
/// File-name patterns of secrets; such a file is never put in a patch. Matched on the last path
/// component, ignoring case. `*` stands for any text at the start or the end of a pattern.
pub const SECRET_NAMES: [&str; 10] = [
    ".env",
    ".env.*",
    ".dev.vars",
    ".dev.vars.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa*",
    "id_ed25519*",
];

/// A file left out of the patch and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    /// The file's path.
    pub path: String,
    /// A short reason.
    pub why: String,
}

/// One side of a changed file: its git mode, its blob id (empty when unknown) and its bytes.
#[derive(Debug, Clone, Copy)]
pub struct Side<'a> {
    /// The git mode, such as `100644`.
    pub mode: &'a str,
    /// The blob id; empty when unknown.
    pub id: &'a str,
    /// The file's bytes.
    pub data: &'a [u8],
}

/// A changed file: its path and what it was (`None`: it is new) and is (`None`: it is deleted).
#[derive(Debug, Clone, Copy)]
pub struct File<'a> {
    /// The file's path in the repository.
    pub path: &'a str,
    /// The file at the merge base; `None` when the branch adds it.
    pub old: Option<Side<'a>>,
    /// The file at the branch tip; `None` when the branch deletes it.
    pub new: Option<Side<'a>>,
}

/// The whole patch, the number of files in it and the files left out.
#[derive(Debug)]
pub struct Built {
    /// The patch text, ready for `git apply`.
    pub patch: String,
    /// How many files the patch holds.
    pub files: usize,
    /// The files left out, with why.
    pub skipped: Vec<Skipped>,
}

/// The git ref for a branch `name` given with or without `refs/heads/`; an error for a name that
/// is empty, holds `..`, a space or a control character, or starts with `-`.
pub fn branch_ref(name: &str) -> Result<String, String> {
    let short = name.strip_prefix("refs/heads/").unwrap_or(name);
    if short.is_empty() {
        return Err("the branch name is empty".into());
    }
    if short.starts_with('-') {
        return Err("a branch name cannot start with `-`".into());
    }
    if short.contains("..") {
        return Err("a branch name cannot hold `..`".into());
    }
    if short.chars().any(|c| c == ' ' || c.is_control()) {
        return Err("a branch name cannot hold a space or a control character".into());
    }
    Ok(format!("refs/heads/{short}"))
}

/// Whether `name` (a file name) matches one of [`SECRET_NAMES`].
fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_NAMES.iter().any(|p| {
        if let Some(rest) = p.strip_prefix('*') {
            name.ends_with(rest)
        } else if let Some(rest) = p.strip_suffix('*') {
            name.starts_with(rest)
        } else {
            name == *p
        }
    })
}

/// Why a file is left out judging by its path and modes alone (so its bytes need not be read),
/// or `None`.
pub fn precheck(path: &str, old_mode: Option<&str>, new_mode: Option<&str>) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if secret_name(name) {
        return Some("secret-shaped file name".into());
    }
    let modes = [old_mode, new_mode];
    if modes.contains(&Some("160000")) {
        return Some("submodule".into());
    }
    if modes.contains(&Some("120000")) {
        return Some("symlink".into());
    }
    if path
        .chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || !c.is_ascii())
    {
        return Some("path needs quoting".into());
    }
    None
}

/// The text of `data`, or `None` when it is not UTF-8 or holds a NUL byte.
fn text(data: &[u8]) -> Option<&str> {
    std::str::from_utf8(data).ok().filter(|t| !t.contains('\0'))
}

/// Whether `text` holds a PEM private key block.
fn has_private_key(text: &str) -> bool {
    text.lines()
        .any(|l| l.starts_with("-----BEGIN ") && l.contains("PRIVATE KEY-----"))
}

/// Whether `id` is a full 40-digit hex blob id.
fn full_id(id: &str) -> bool {
    id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The diff of one file, or `Err(why)` when it is left out. An empty answer means the file did
/// not change.
pub fn file_diff(file: &File<'_>) -> Result<String, String> {
    let old_mode = file.old.map(|s| s.mode);
    let new_mode = file.new.map(|s| s.mode);
    if let Some(why) = precheck(file.path, old_mode, new_mode) {
        return Err(why);
    }
    let sides = [file.old, file.new];
    if sides.iter().flatten().any(|s| s.data.len() > FILE_MAX) {
        return Err(format!("over {FILE_MAX} bytes"));
    }
    let mut texts = Vec::new();
    for side in sides {
        match side {
            Some(s) => texts.push(text(s.data).ok_or("binary file")?),
            None => texts.push(""),
        }
    }
    let (old_text, new_text) = (
        texts.first().copied().unwrap_or(""),
        texts.get(1).copied().unwrap_or(""),
    );
    if has_private_key(old_text) || has_private_key(new_text) {
        return Err("holds a private key".into());
    }
    let path = file.path;
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let same_mode = old_mode == new_mode;
    match (file.old, file.new) {
        (None, Some(n)) => out.push_str(&format!("new file mode {}\n", n.mode)),
        (Some(o), None) => out.push_str(&format!("deleted file mode {}\n", o.mode)),
        (Some(o), Some(n)) if !same_mode => {
            out.push_str(&format!("old mode {}\nnew mode {}\n", o.mode, n.mode));
        }
        _ => {}
    }
    let old_id = file.old.map_or("0".repeat(40), |s| s.id.to_string());
    let new_id = file.new.map_or("0".repeat(40), |s| s.id.to_string());
    if full_id(&old_id) && full_id(&new_id) {
        let mode = match (file.old, file.new) {
            (Some(o), Some(_)) if same_mode => format!(" {}", o.mode),
            _ => String::new(),
        };
        out.push_str(&format!("index {old_id}..{new_id}{mode}\n"));
    }
    if old_text == new_text && file.old.is_some() && file.new.is_some() {
        // A mode change alone has no hunks; no change at all has no diff.
        return Ok(if same_mode { String::new() } else { out });
    }
    let old_lines: Vec<&str> = old_text.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new_text.split_inclusive('\n').collect();
    let body = hunks(&old_lines, &new_lines).ok_or("too large to diff")?;
    if body.is_empty() {
        return Ok(out);
    }
    let tab = if path.contains(' ') { "\t" } else { "" };
    match file.old {
        Some(_) => out.push_str(&format!("--- a/{path}{tab}\n")),
        None => out.push_str("--- /dev/null\n"),
    }
    match file.new {
        Some(_) => out.push_str(&format!("+++ b/{path}{tab}\n")),
        None => out.push_str("+++ /dev/null\n"),
    }
    out.push_str(&body);
    Ok(out)
}

/// One line of an edit script.
#[derive(Clone, Copy, PartialEq)]
enum Op {
    Keep,
    Del,
    Add,
}

/// The hunks that turn `old` into `new`; `None` when the pair is too large to diff.
fn hunks(old: &[&str], new: &[&str]) -> Option<String> {
    let map = matching(old, new)?;
    let mut ops: Vec<(Op, &str)> = Vec::new();
    let mut j = 0;
    for (i, line) in old.iter().enumerate() {
        match map.get(i).copied().flatten() {
            Some(k) => {
                for added in new.get(j..k)? {
                    ops.push((Op::Add, added));
                }
                ops.push((Op::Keep, line));
                j = k + 1;
            }
            None => ops.push((Op::Del, line)),
        }
    }
    for added in new.get(j..)? {
        ops.push((Op::Add, added));
    }
    // Lines of each side before each op.
    let mut old_at = vec![0usize];
    let mut new_at = vec![0usize];
    for (op, _) in &ops {
        let (o, n) = (old_at.last().copied()?, new_at.last().copied()?);
        old_at.push(o + usize::from(*op != Op::Add));
        new_at.push(n + usize::from(*op != Op::Del));
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for (idx, (op, _)) in ops.iter().enumerate() {
        if *op == Op::Keep {
            continue;
        }
        let lo = idx.saturating_sub(CONTEXT);
        let hi = (idx + CONTEXT + 1).min(ops.len());
        match ranges.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => ranges.push((lo, hi)),
        }
    }
    let mut out = String::new();
    for (lo, hi) in ranges {
        let (o0, o1) = (*old_at.get(lo)?, *old_at.get(hi)?);
        let (n0, n1) = (*new_at.get(lo)?, *new_at.get(hi)?);
        let (oc, nc) = (o1 - o0, n1 - n0);
        let os = if oc == 0 { o0 } else { o0 + 1 };
        let ns = if nc == 0 { n0 } else { n0 + 1 };
        out.push_str(&format!("@@ -{os},{oc} +{ns},{nc} @@\n"));
        for (op, line) in ops.get(lo..hi)? {
            out.push(match op {
                Op::Keep => ' ',
                Op::Del => '-',
                Op::Add => '+',
            });
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    Some(out)
}

/// The patch for `files`, in sorted path order, with the files left out named. An error, naming
/// the size and [`PATCH_MAX`], when the patch is too big; it is never truncated.
pub fn assemble(files: &[File<'_>]) -> Result<Built, String> {
    let mut sorted: Vec<&File<'_>> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(b.path));
    let mut parts = Vec::new();
    let mut skipped = Vec::new();
    for file in sorted {
        match file_diff(file) {
            Ok(d) if d.is_empty() => {}
            Ok(d) => parts.push(d),
            Err(why) => skipped.push(Skipped {
                path: file.path.to_string(),
                why,
            }),
        }
    }
    let total: usize = parts.iter().map(String::len).sum();
    if total > PATCH_MAX {
        return Err(format!(
            "the patch is {total} bytes, over the bound of {PATCH_MAX} bytes"
        ));
    }
    Ok(Built {
        files: parts.len(),
        patch: parts.concat(),
        skipped,
    })
}

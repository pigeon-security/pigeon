//! Line-level three-way merge for text files, in the manner of `git merge-file` (diff3).
//!
//! The Lander uses this when it rebases a branch onto a main that moved and both sides changed
//! the same text file, such as `tests/smoke.rs`. A clean merge is taken. Anything ambiguous is
//! refused with a [`Conflict`]; this module never guesses.
//!
//! The algorithm:
//!
//! 1. Split the three texts into lines (a missing final newline is kept exactly) and intern the
//!    lines to integers.
//! 2. Diff the base against each side with Myers' O((N+M)D) algorithm in linear space (the
//!    middle-snake divide and conquer, on an explicit stack). Past [`MAX_EDITS`] edits the diff
//!    is refused as [`Conflict::TooLarge`].
//! 3. Slide every pure insertion and pure deletion as far down as it goes while the text stays
//!    equal, as git does (`xdl_change_compact`), so one insertion lands at the same base place on
//!    both sides. A replacement does not move.
//! 4. Walk both sides' hunks in base order, with git's diff3 rules: a hunk that neither overlaps
//!    nor touches a hunk of the other side is applied. Hunks that overlap or touch (two
//!    insertions at one point, changes on adjacent lines) form one region. A region both sides
//!    changed is taken once if both produce the same text, and is a conflict otherwise, with two
//!    exceptions for two insertions at one place: when one is the other with lines added before
//!    or after it, the longer is taken; and with [`Options::union_insertions`], two
//!    bracket-balanced insertions are joined, ours then theirs. Git conflicts in both cases.
//! 5. The duplicate guard, which git does not have: see [`DUP_RUN`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

/// The most edits (lines deleted plus lines inserted) one diff may hold. A bigger diff is
/// refused as [`Conflict::TooLarge`], which bounds the work and the memory in a Worker.
pub const MAX_EDITS: usize = 20_000;

/// The shortest run of identical, non-blank lines that two one-sided changes may not both
/// insert. If an ours-only change and a theirs-only change each insert such a run, at different
/// places, the merge is refused as [`Conflict::Duplicate`]. A silent duplicate compiles and
/// lands, while a refusal only costs a rebase.
pub const DUP_RUN: usize = 4;

/// Why a three-way merge was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Both sides changed the same place, in different ways. `line` is the 1-based base line
    /// where the two changes meet (for an insertion, the line it is inserted before).
    Overlap { line: usize },
    /// Both sides inserted the same run of [`DUP_RUN`] or more lines at different places, so the
    /// merge would hold it twice. `line` is the 1-based line, in the merged result, of the
    /// first such run inside the theirs-only change.
    Duplicate { line: usize },
    /// A diff holds more than [`MAX_EDITS`] edits.
    TooLarge,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Conflict::Overlap { line } => write!(f, "both change base line {line}"),
            Conflict::Duplicate { line } => write!(
                f,
                "both add the same {DUP_RUN} or more lines, which would appear twice (merged \
                 line {line})"
            ),
            Conflict::TooLarge => write!(f, "the change is too large to merge"),
        }
    }
}

/// How [`try_merge_with`] treats two different insertions at one base place, which git's diff3
/// calls a conflict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Join them, ours then theirs, when each closes every bracket it opens. For files where
    /// every insertion is a whole item, such as the smoke suite's area modules. The duplicate
    /// guard still refuses two insertions that share a run of lines.
    pub union_insertions: bool,
}

/// The three-way merge of `ours` and `theirs`, both edited from `base`; `None` when they
/// conflict. See [`try_merge`] for the reason.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    try_merge(base, ours, theirs).ok()
}

/// The three-way merge of `ours` and `theirs`, both edited from `base`, or the [`Conflict`]
/// that stops it, with the default [`Options`].
pub fn try_merge(base: &str, ours: &str, theirs: &str) -> Result<String, Conflict> {
    try_merge_with(base, ours, theirs, Options::default())
}

/// The three-way merge of `ours` and `theirs`, both edited from `base`, or the [`Conflict`]
/// that stops it.
///
/// Beyond git's diff3 rules, one case is joined always: two insertions at one base place where
/// one is the other with more lines before or after it (a branch built on another that has
/// landed since) give the longer one, once. With [`Options::union_insertions`], two other
/// insertions at one place are joined too.
pub fn try_merge_with(
    base: &str,
    ours: &str,
    theirs: &str,
    options: Options,
) -> Result<String, Conflict> {
    if ours == theirs || theirs == base {
        return Ok(ours.to_owned());
    }
    if ours == base {
        return Ok(theirs.to_owned());
    }
    let b: Vec<&str> = base.split_inclusive('\n').collect();
    let o: Vec<&str> = ours.split_inclusive('\n').collect();
    let t: Vec<&str> = theirs.split_inclusive('\n').collect();
    let mut ids: HashMap<&str, u32> = HashMap::new();
    let mut blank: Vec<bool> = Vec::new();
    let bi = intern(&b, &mut ids, &mut blank)?;
    let oi = intern(&o, &mut ids, &mut blank)?;
    let ti = intern(&t, &mut ids, &mut blank)?;
    let ho = hunks(&bi, &oi)?;
    let ht = hunks(&bi, &ti)?;

    let mut out = String::with_capacity(ours.len().max(theirs.len()));
    let mut out_lines = 0usize;
    let mut records: Vec<Record> = Vec::new();
    let (mut io, mut it) = (0usize, 0usize);
    let mut cursor = 0usize;
    loop {
        let start = match (ho.get(io), ht.get(it)) {
            (None, None) => break,
            (Some(x), None) => x.base.start,
            (None, Some(y)) => y.base.start,
            (Some(x), Some(y)) => x.base.start.min(y.base.start),
        };
        let (o_from, t_from) = (io, it);
        let mut hi = start;
        loop {
            if let Some(h) = ho.get(io)
                && h.base.start <= hi
            {
                hi = hi.max(h.base.end);
                io += 1;
            } else if let Some(h) = ht.get(it)
                && h.base.start <= hi
            {
                hi = hi.max(h.base.end);
                it += 1;
            } else {
                break;
            }
        }
        let o_hunks = ho.get(o_from..io).ok_or(Conflict::TooLarge)?;
        let t_hunks = ht.get(t_from..it).ok_or(Conflict::TooLarge)?;
        push_lines(
            &mut out,
            &mut out_lines,
            b.get(cursor..start).ok_or(Conflict::TooLarge)?,
        );
        cursor = hi;
        let base_text = b.get(start..hi).ok_or(Conflict::TooLarge)?;
        let (o_range, o_text) = side_text(&o, o_hunks, start, hi, base_text)?;
        let (t_range, t_text) = side_text(&t, t_hunks, start, hi, base_text)?;
        // Two insertions at one place: the region holds no base line.
        let inserts = start == hi;
        if o_text == t_text {
            push_lines(&mut out, &mut out_lines, o_text);
        } else if t_hunks.is_empty() || o_hunks.is_empty() {
            let (ours_side, range, text) = if t_hunks.is_empty() {
                (true, o_range, o_text)
            } else {
                (false, t_range, t_text)
            };
            take(
                &mut out,
                &mut out_lines,
                &mut records,
                ours_side,
                range,
                text,
            );
        } else if inserts
            && let Some((ours_side, range, extra_at)) = longer(&o_range, o_text, &t_range, t_text)
        {
            // One side inserted what the other did and more: the longer, once. The extra lines
            // are that side's own, so the guard checks them.
            let text = if ours_side { o_text } else { t_text };
            records.push(Record {
                ours: ours_side,
                out_line: out_lines + extra_at,
                range,
            });
            push_lines(&mut out, &mut out_lines, text);
        } else if inserts && options.union_insertions && balanced(o_text) && balanced(t_text) {
            take(
                &mut out,
                &mut out_lines,
                &mut records,
                true,
                o_range,
                o_text,
            );
            take(
                &mut out,
                &mut out_lines,
                &mut records,
                false,
                t_range,
                t_text,
            );
        } else {
            return Err(Conflict::Overlap { line: start + 1 });
        }
    }
    push_lines(
        &mut out,
        &mut out_lines,
        b.get(cursor..).ok_or(Conflict::TooLarge)?,
    );
    guard(&records, &oi, &ti, &blank)?;
    Ok(out)
}

/// One change taken from a single side: which side, the merged-result line count before it, and
/// the side's line range it inserted.
struct Record {
    ours: bool,
    out_line: usize,
    range: Range<usize>,
}

/// Appends `lines` to `out` and counts them in `count`.
fn push_lines(out: &mut String, count: &mut usize, lines: &[&str]) {
    for line in lines {
        out.push_str(line);
    }
    *count += lines.len();
}

/// Appends one side's change and records it for the duplicate guard.
fn take(
    out: &mut String,
    count: &mut usize,
    records: &mut Vec<Record>,
    ours: bool,
    range: Range<usize>,
    lines: &[&str],
) {
    records.push(Record {
        ours,
        out_line: *count,
        range,
    });
    push_lines(out, count, lines);
}

/// When one insertion is the other with lines added before or after it: which side is longer,
/// the side range of its extra lines, and where they start within its text.
fn longer(
    o_range: &Range<usize>,
    o_text: &[&str],
    t_range: &Range<usize>,
    t_text: &[&str],
) -> Option<(bool, Range<usize>, usize)> {
    let (ours, range, long, short) = if o_text.len() > t_text.len() {
        (true, o_range, o_text, t_text)
    } else {
        (false, t_range, t_text, o_text)
    };
    if short.is_empty() || long.len() == short.len() {
        None
    } else if long.starts_with(short) {
        Some((ours, range.start + short.len()..range.end, short.len()))
    } else if long.ends_with(short) {
        Some((ours, range.start..range.end - short.len(), 0))
    } else {
        None
    }
}

/// Whether `lines` close every `(`, `[` and `{` they open, in order. Brackets in strings and
/// comments count too, so this is a rough check; the Lander tests a merged result again.
fn balanced(lines: &[&str]) -> bool {
    let mut open: Vec<char> = Vec::new();
    for c in lines.iter().flat_map(|l| l.chars()) {
        let want = match c {
            '(' | '[' | '{' => {
                open.push(c);
                continue;
            }
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => continue,
        };
        if open.pop() != Some(want) {
            return false;
        }
    }
    open.is_empty()
}

/// A change: base lines `base` became `side` lines of one side.
#[derive(Debug, Clone)]
struct Hunk {
    base: Range<usize>,
    side: Range<usize>,
}

/// One side's lines, and their range, for the base range `lo..hi`, which holds `hunks` of that
/// side (or none, when the side keeps `base_text`).
fn side_text<'x, 'a>(
    lines: &'x [&'a str],
    hunks: &[Hunk],
    lo: usize,
    hi: usize,
    base_text: &'x [&'a str],
) -> Result<(Range<usize>, &'x [&'a str]), Conflict> {
    let (Some(first), Some(last)) = (hunks.first(), hunks.last()) else {
        return Ok((0..0, base_text));
    };
    let start = first
        .side
        .start
        .checked_sub(first.base.start.saturating_sub(lo))
        .ok_or(Conflict::TooLarge)?;
    let end = last.side.end + hi.saturating_sub(last.base.end);
    let text = lines.get(start..end).ok_or(Conflict::TooLarge)?;
    Ok((start..end, text))
}

/// The duplicate guard: refuses when a run of [`DUP_RUN`] non-blank lines was inserted by an
/// ours-only change and by a theirs-only change.
fn guard(records: &[Record], ours: &[u32], theirs: &[u32], blank: &[bool]) -> Result<(), Conflict> {
    let windows = |ids: &[u32], range: &Range<usize>| -> Vec<Option<[u32; DUP_RUN]>> {
        ids.get(range.clone())
            .unwrap_or(&[])
            .windows(DUP_RUN)
            .map(|w| {
                let solid = w
                    .iter()
                    .all(|id| !blank.get(*id as usize).copied().unwrap_or(true));
                if solid { w.try_into().ok() } else { None }
            })
            .collect()
    };
    let mut seen: HashSet<[u32; DUP_RUN]> = HashSet::new();
    for r in records.iter().filter(|r| r.ours) {
        seen.extend(windows(ours, &r.range).into_iter().flatten());
    }
    if seen.is_empty() {
        return Ok(());
    }
    for r in records.iter().filter(|r| !r.ours) {
        for (k, w) in windows(theirs, &r.range).into_iter().enumerate() {
            if w.is_some_and(|w| seen.contains(&w)) {
                return Err(Conflict::Duplicate {
                    line: r.out_line + k + 1,
                });
            }
        }
    }
    Ok(())
}

/// The hunks that turn `a` (the base) into `b`, with pure insertions and deletions slid down
/// ([`compact`]).
fn hunks(a: &[u32], b: &[u32]) -> Result<Vec<Hunk>, Conflict> {
    let (da, db) = diff(a, b)?;
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::new();
    loop {
        while i < a.len() && j < b.len() && !flag(&da, i) && !flag(&db, j) {
            i += 1;
            j += 1;
        }
        if i >= a.len() && j >= b.len() {
            return Ok(compact(a, b, out));
        }
        let (si, sj) = (i, j);
        while i < a.len() && flag(&da, i) {
            i += 1;
        }
        while j < b.len() && flag(&db, j) {
            j += 1;
        }
        if (si, sj) == (i, j) {
            // The flags disagree about the texts; cannot happen, and is refused if it does.
            return Err(Conflict::TooLarge);
        }
        out.push(Hunk {
            base: si..i,
            side: sj..j,
        });
    }
}

/// Whether line `i` is flagged as changed.
fn flag(flags: &[bool], i: usize) -> bool {
    flags.get(i).copied().unwrap_or(false)
}

/// Slides each pure insertion and pure deletion down, as git does (`xdl_change_compact`), while
/// the line leaving it equals the unchanged line entering it; a hunk that comes to touch the next
/// one is joined with it. A replacement never moves: sliding its deleted lines away from its
/// inserted lines would split one change into two, and the deletion could then match a deletion
/// by the other side and be taken once, losing a line.
fn compact(a: &[u32], b: &[u32], raw: Vec<Hunk>) -> Vec<Hunk> {
    let mut out: Vec<Hunk> = Vec::with_capacity(raw.len());
    let mut rest = raw.into_iter().peekable();
    while let Some(mut h) = rest.next() {
        loop {
            let next_base = rest.peek().map_or(a.len(), |n| n.base.start);
            if h.base.end >= next_base || h.side.end >= b.len() {
                break;
            }
            let slides = if h.base.is_empty() {
                b.get(h.side.start) == b.get(h.side.end)
            } else if h.side.is_empty() {
                a.get(h.base.start) == a.get(h.base.end)
            } else {
                false
            };
            if !slides {
                break;
            }
            h.base = h.base.start + 1..h.base.end + 1;
            h.side = h.side.start + 1..h.side.end + 1;
        }
        if let Some(n) = rest.peek_mut()
            && n.base.start == h.base.end
            && n.side.start == h.side.end
        {
            n.base.start = h.base.start;
            n.side.start = h.side.start;
            continue;
        }
        out.push(h);
    }
    out
}

/// Marks `range` of `flags` as changed.
fn mark(flags: &mut [bool], range: Range<usize>) {
    if let Some(s) = flags.get_mut(range) {
        s.fill(true);
    }
}

/// The lines of `a` deleted and the lines of `b` inserted by a minimal edit script, as flags.
/// Myers' diff in linear space: find the middle snake, then diff the two halves, on a stack.
fn diff(a: &[u32], b: &[u32]) -> Result<(Vec<bool>, Vec<bool>), Conflict> {
    let mut da = vec![false; a.len()];
    let mut db = vec![false; b.len()];
    let half = MAX_EDITS / 2 + 3;
    let mut scratch = Scratch {
        fwd: vec![0; 2 * half + 1],
        bwd: vec![0; 2 * half + 1],
        off: isize::try_from(half).map_err(|_| Conflict::TooLarge)?,
    };
    let mut stack = vec![(0, a.len(), 0, b.len())];
    while let Some((mut a0, mut a1, mut b0, mut b1)) = stack.pop() {
        while a0 < a1 && b0 < b1 && a.get(a0) == b.get(b0) {
            a0 += 1;
            b0 += 1;
        }
        while a0 < a1 && b0 < b1 && a.get(a1 - 1) == b.get(b1 - 1) {
            a1 -= 1;
            b1 -= 1;
        }
        if a0 == a1 {
            mark(&mut db, b0..b1);
            continue;
        }
        if b0 == b1 {
            mark(&mut da, a0..a1);
            continue;
        }
        let sa = a.get(a0..a1).ok_or(Conflict::TooLarge)?;
        let sb = b.get(b0..b1).ok_or(Conflict::TooLarge)?;
        let (x0, y0, x1, y1) = middle_snake(sa, sb, &mut scratch)?;
        if (x0, y0, x1, y1) == (0, 0, sa.len(), sb.len()) {
            return Err(Conflict::TooLarge);
        }
        stack.push((a0, a0 + x0, b0, b0 + y0));
        stack.push((a0 + x1, a1, b0 + y1, b1));
    }
    Ok((da, db))
}

/// The work arrays of the middle-snake search: furthest-reaching x per diagonal, forward and
/// backward, with `off` the index of diagonal zero.
struct Scratch {
    fwd: Vec<isize>,
    bwd: Vec<isize>,
    off: isize,
}

/// Reads `v` at a signed index; zero when out of range.
fn at(v: &[isize], i: isize) -> isize {
    usize::try_from(i)
        .ok()
        .and_then(|i| v.get(i))
        .copied()
        .unwrap_or(0)
}

/// Writes `x` into `v` at a signed index, if in range.
fn put(v: &mut [isize], i: isize, x: isize) {
    if let Some(c) = usize::try_from(i).ok().and_then(|i| v.get_mut(i)) {
        *c = x;
    }
}

/// The id of line `i` of `v`, counted from the front, or from the back when `rev`.
fn id_at(v: &[u32], i: isize, rev: bool) -> Option<u32> {
    let i = usize::try_from(i).ok()?;
    let i = if rev { v.len().checked_sub(i + 1)? } else { i };
    v.get(i).copied()
}

/// Converts a non-negative coordinate.
fn coord(x: isize) -> Result<usize, Conflict> {
    usize::try_from(x).map_err(|_| Conflict::TooLarge)
}

/// The middle snake of the shortest edit script of `a` into `b`, as `(x0, y0, x1, y1)`: it runs
/// from `(x0, y0)` to `(x1, y1)` along equal lines. [`Conflict::TooLarge`] when the script has
/// more than about [`MAX_EDITS`] edits.
fn middle_snake(
    a: &[u32],
    b: &[u32],
    s: &mut Scratch,
) -> Result<(usize, usize, usize, usize), Conflict> {
    let n = isize::try_from(a.len()).map_err(|_| Conflict::TooLarge)?;
    let m = isize::try_from(b.len()).map_err(|_| Conflict::TooLarge)?;
    let delta = n - m;
    let odd = delta & 1 != 0;
    let limit = isize::try_from(MAX_EDITS / 2 + 1).map_err(|_| Conflict::TooLarge)?;
    let dmax = ((n + m + 1) / 2).min(limit);
    let off = s.off;
    put(&mut s.fwd, off + 1, 0);
    put(&mut s.bwd, off + 1, 0);
    for d in 0..=dmax {
        let mut k = -d;
        while k <= d {
            let down = k == -d || (k != d && at(&s.fwd, off + k - 1) < at(&s.fwd, off + k + 1));
            let sx = if down {
                at(&s.fwd, off + k + 1)
            } else {
                at(&s.fwd, off + k - 1) + 1
            };
            let sy = sx - k;
            let (mut x, mut y) = (sx, sy);
            while x < n && y < m && id_at(a, x, false) == id_at(b, y, false) {
                x += 1;
                y += 1;
            }
            put(&mut s.fwd, off + k, x);
            if odd && (k - delta).abs() < d && x + at(&s.bwd, off + delta - k) >= n {
                return Ok((coord(sx)?, coord(sy)?, coord(x)?, coord(y)?));
            }
            k += 2;
        }
        let mut k = -d;
        while k <= d {
            let down = k == -d || (k != d && at(&s.bwd, off + k - 1) < at(&s.bwd, off + k + 1));
            let sx = if down {
                at(&s.bwd, off + k + 1)
            } else {
                at(&s.bwd, off + k - 1) + 1
            };
            let sy = sx - k;
            let (mut x, mut y) = (sx, sy);
            while x < n && y < m && id_at(a, x, true) == id_at(b, y, true) {
                x += 1;
                y += 1;
            }
            put(&mut s.bwd, off + k, x);
            if !odd && (delta - k).abs() <= d && at(&s.fwd, off + delta - k) + x >= n {
                return Ok((coord(n - x)?, coord(m - y)?, coord(n - sx)?, coord(m - sy)?));
            }
            k += 2;
        }
    }
    Err(Conflict::TooLarge)
}

/// Maps each line to an integer id, shared across calls through `ids`; `blank` records, per id,
/// whether the line is whitespace only.
fn intern<'a>(
    lines: &[&'a str],
    ids: &mut HashMap<&'a str, u32>,
    blank: &mut Vec<bool>,
) -> Result<Vec<u32>, Conflict> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let id = match ids.get(line) {
            Some(id) => *id,
            None => {
                let id = u32::try_from(blank.len()).map_err(|_| Conflict::TooLarge)?;
                ids.insert(line, id);
                blank.push(line.trim().is_empty());
                id
            }
        };
        out.push(id);
    }
    Ok(out)
}

/// Maps each line of `base` to the index of its partner in `other` in a minimal edit script,
/// if it has one; `None` when the pair is too large to diff. Used by the patch builder.
pub(crate) fn matching(base: &[&str], other: &[&str]) -> Option<Vec<Option<usize>>> {
    let mut ids = HashMap::new();
    let mut blank = Vec::new();
    let a = intern(base, &mut ids, &mut blank).ok()?;
    let b = intern(other, &mut ids, &mut blank).ok()?;
    let (deleted, added) = diff(&a, &b).ok()?;
    let mut kept_other = added
        .iter()
        .enumerate()
        .filter(|(_, inserted)| !**inserted)
        .map(|(j, _)| j);
    let mut map = Vec::with_capacity(base.len());
    for gone in &deleted {
        map.push(if *gone { None } else { kept_other.next() });
    }
    Some(map)
}

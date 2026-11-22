//! Line-level three-way merge for text files.
//!
//! The Lander uses this when it rebases a branch onto a main that moved and both sides changed
//! the same text file, such as `tests/smoke.rs`. A clean merge is taken. Anything ambiguous is
//! refused with `None`; this module never guesses.

/// The largest table the diff builds for one pair of texts, after their common prefix and
/// suffix: 16 million cells of two bytes, within a Worker's memory.
const MAX_CELLS: usize = 16_000_000;

/// The three-way merge of `ours` and `theirs`, both edited from `base`, line by line; `None`
/// when they conflict, or when the texts are too large to diff.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    if ours == theirs {
        return Some(ours.to_owned());
    }
    if ours == base {
        return Some(theirs.to_owned());
    }
    if theirs == base {
        return Some(ours.to_owned());
    }
    let b: Vec<&str> = base.split_inclusive('\n').collect();
    let o: Vec<&str> = ours.split_inclusive('\n').collect();
    let t: Vec<&str> = theirs.split_inclusive('\n').collect();
    let ours_map = matching(&b, &o)?;
    let theirs_map = matching(&b, &t)?;
    let mut out = String::with_capacity(ours.len().max(theirs.len()));
    let (mut pb, mut po, mut pt) = (0usize, 0usize, 0usize);
    for (bi, oi, ti) in stable_points(&ours_map, &theirs_map) {
        let chosen = resolve(b.get(pb..bi)?, o.get(po..oi)?, t.get(pt..ti)?)?;
        push_lines(&mut out, chosen);
        out.push_str(o.get(oi)?);
        pb = bi + 1;
        po = oi + 1;
        pt = ti + 1;
    }
    let chosen = resolve(b.get(pb..)?, o.get(po..)?, t.get(pt..)?)?;
    push_lines(&mut out, chosen);
    Some(out)
}

/// Appends every line in `lines` to `out`.
fn push_lines(out: &mut String, lines: &[&str]) {
    for line in lines {
        out.push_str(line);
    }
}

/// Picks the lines for one unstable chunk, or `None` on a conflict. Two different insertions at
/// the same place (an empty `base` chunk) are a conflict too.
fn resolve<'x, 'a>(
    base: &'x [&'a str],
    ours: &'x [&'a str],
    theirs: &'x [&'a str],
) -> Option<&'x [&'a str]> {
    if ours == theirs || theirs == base {
        Some(ours)
    } else if ours == base {
        Some(theirs)
    } else {
        None
    }
}

/// Base lines matched in both diffs, as `(base, ours, theirs)` indices in order.
fn stable_points(
    ours_map: &[Option<usize>],
    theirs_map: &[Option<usize>],
) -> Vec<(usize, usize, usize)> {
    let mut points = Vec::new();
    for (bi, (mo, mt)) in ours_map.iter().zip(theirs_map.iter()).enumerate() {
        if let (Some(oi), Some(ti)) = (mo, mt) {
            points.push((bi, *oi, *ti));
        }
    }
    points
}

/// Maps each base line to the index of its partner in `other` in a longest common
/// subsequence, if it has one; `None` when the pair is too large to diff.
fn matching(base: &[&str], other: &[&str]) -> Option<Vec<Option<usize>>> {
    let n = base.len();
    let m = other.len();
    let mut map: Vec<Option<usize>> = vec![None; n];
    let prefix = base
        .iter()
        .zip(other.iter())
        .take_while(|(x, y)| x == y)
        .count();
    let suffix = base
        .iter()
        .rev()
        .zip(other.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
        .min(n.min(m) - prefix);
    for k in 0..prefix {
        *map.get_mut(k)? = Some(k);
    }
    for k in 0..suffix {
        *map.get_mut(n - suffix + k)? = Some(m - suffix + k);
    }
    let a = base.get(prefix..n - suffix)?;
    let b = other.get(prefix..m - suffix)?;
    lcs_pairs(a, b, prefix, &mut map)?;
    Some(map)
}

/// Fills `map` with the longest common subsequence pairs of `a` and `b`, shifting indices by
/// `offset`; `None` when the table would be larger than [`MAX_CELLS`].
fn lcs_pairs(a: &[&str], b: &[&str], offset: usize, map: &mut [Option<usize>]) -> Option<()> {
    let width = b.len() + 1;
    let cells = (a.len() + 1).checked_mul(width)?;
    if cells > MAX_CELLS {
        return None;
    }
    // The cap bounds the shorter side at 4000 lines, so every cell fits in a u16.
    let mut table = vec![0u16; cells];
    for (i, x) in a.iter().enumerate().rev() {
        for (j, y) in b.iter().enumerate().rev() {
            let value = if x == y {
                cell(&table, width, i + 1, j + 1).saturating_add(1)
            } else {
                cell(&table, width, i + 1, j).max(cell(&table, width, i, j + 1))
            };
            *table.get_mut(i * width + j)? = value;
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        if x == y {
            *map.get_mut(offset + i)? = Some(offset + j);
            i += 1;
            j += 1;
        } else if cell(&table, width, i + 1, j) >= cell(&table, width, i, j + 1) {
            i += 1;
        } else {
            j += 1;
        }
    }
    Some(())
}

/// Reads one table cell, treating out-of-range reads as zero.
fn cell(table: &[u16], width: usize, i: usize, j: usize) -> u16 {
    table.get(i * width + j).copied().unwrap_or(0)
}

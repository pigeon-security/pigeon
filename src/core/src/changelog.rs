//! The changelog of main, from git history: one entry per commit, grouped by UTC date, newest
//! first, with the landing id when a land note names the commit. Pure: the I/O is the
//! `changelog` capability's (docs/plans/changelog.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pack::Objects;

/// The most characters a subject keeps.
const SUBJECT_MAX: usize = 100;
/// The state's format version.
pub const VERSION: u32 = 1;

/// One commit on main: its id, its committer time (UTC seconds) and its subject.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub sha: String,
    pub unix: i64,
    pub subject: String,
}

/// What a run keeps between runs: the last main commit it processed and every entry, newest
/// first.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub version: u32,
    pub main: String,
    pub entries: Vec<Entry>,
}

/// The commit `id`'s first parent, committer time and message, read from `objects`.
fn read_commit(objects: &Objects, id: &str) -> Result<(Option<String>, i64, String), String> {
    let (kind, body) = objects
        .by_id
        .get(id)
        .ok_or_else(|| format!("commit {id} is missing"))?;
    if *kind != 1 {
        return Err(format!("object {id} is not a commit"));
    }
    let text = String::from_utf8_lossy(body);
    let (head, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut parent = None;
    let mut unix = None;
    for line in head.lines() {
        if let Some(p) = line.strip_prefix("parent ")
            && parent.is_none()
        {
            parent = Some(p.trim().to_string());
        } else if let Some(c) = line.strip_prefix("committer ") {
            // `name <email> <secs> <zone>`: the time is the second field from the end.
            unix = c
                .rsplit(' ')
                .nth(1)
                .and_then(|secs| secs.parse::<i64>().ok());
        }
    }
    let unix = unix.ok_or_else(|| format!("commit {id} has no committer time"))?;
    Ok((parent, unix, subject(message)))
}

/// The entries from `tip` down its first parents to `stop` (left out) or the root, newest
/// first. An error when a commit is missing from `objects`, or `stop` is given and never met.
pub fn walk(objects: &Objects, tip: &str, stop: Option<&str>) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let mut at = Some(tip.to_string());
    while let Some(id) = at {
        if Some(id.as_str()) == stop {
            return Ok(out);
        }
        if out.len() > objects.by_id.len() {
            return Err("history loops".to_string());
        }
        let (parent, unix, subject) = read_commit(objects, &id)?;
        out.push(Entry {
            sha: id,
            unix,
            subject,
        });
        at = parent;
    }
    match stop {
        Some(s) => Err(format!("{s} is not on main's first-parent history")),
        None => Ok(out),
    }
}

/// The landing id in land note `note` (`landing <id> state=...`), if it is one.
pub fn landing_of(note: &str) -> Option<String> {
    let mut words = note.split_whitespace();
    match (words.next(), words.next()) {
        (Some("landing"), Some(id)) if !id.contains('=') => Some(id.to_string()),
        _ => None,
    }
}

/// `unix` (seconds) as a UTC date, `YYYY-MM-DD`.
pub fn date_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm), in i64 so no time overflows it.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// A message's subject: its first line trimmed, cut at 100 characters with `…`, or
/// `(no subject)`.
pub fn subject(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default().trim();
    if first.is_empty() {
        return "(no subject)".to_string();
    }
    if first.chars().count() > SUBJECT_MAX {
        let cut: String = first.chars().take(SUBJECT_MAX).collect();
        return format!("{}…", cut.trim_end());
    }
    first.to_string()
}

/// The changelog in Markdown: a section per UTC date, newest first, entries within a day in
/// the given order, each with its short id and its landing id from `landings` (by full id)
/// when there is one. `days` keeps only the newest that many sections.
pub fn render(
    entries: &[Entry],
    landings: &BTreeMap<String, String>,
    days: Option<usize>,
) -> String {
    let mut by_day: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
    for e in entries {
        by_day.entry(date_utc(e.unix)).or_default().push(e);
    }
    let mut out = String::from("# Changelog of main\n");
    for (day, list) in by_day.iter().rev().take(days.unwrap_or(usize::MAX)) {
        out.push_str(&format!("\n## {day}\n\n"));
        for e in list {
            let short: String = e.sha.chars().take(7).collect();
            match landings.get(&e.sha) {
                Some(l) => out.push_str(&format!("- {} (`{short}`, landing `{l}`)\n", e.subject)),
                None => out.push_str(&format!("- {} (`{short}`)\n", e.subject)),
            }
        }
    }
    out
}

/// `markdown` (as [`render`] makes it) cut to its newest `days` date sections.
pub fn newest_days(markdown: &str, days: usize) -> String {
    let mut out = String::new();
    let mut seen = 0;
    for line in markdown.split_inclusive('\n') {
        if line.starts_with("## ") {
            seen += 1;
            if seen > days {
                break;
            }
        }
        out.push_str(line);
    }
    out
}

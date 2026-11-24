//! Brief preflight: the repo paths a brief names, checked against the base commit when a run
//! starts (`docs/plans/ai-agents.md`, Phase 8a). Pure: the sandbox runs the check.

/// The most paths one brief has checked.
pub const MAX_PATHS: usize = 40;

/// File extensions that make a bare name (no `/`) look like a repo file.
const FILE_EXTENSIONS: &[&str] = &[
    "md", "rs", "toml", "json", "sh", "yml", "yaml", "txt", "lock", "ts", "js", "html", "css",
    "sql", "py", "swift",
];

/// Prose pairs written with a `/` that are not paths.
const PROSE: &[&str] = &[
    "and/or",
    "either/or",
    "yes/no",
    "true/false",
    "on/off",
    "input/output",
    "read/write",
    "he/she",
    "his/her",
    "w/o",
    "n/a",
    "pass/fail",
    "ok/err",
    "client/server",
    "start/stop",
    "before/after",
    "if/else",
    "open/closed",
    "request/response",
    "add/remove",
    "get/set",
];

/// Prefixes of branch and ref names, which are not paths.
const REF_PREFIXES: &[&str] = &[
    "claude/",
    "origin/",
    "refs/",
    "remotes/",
    "upstream/",
    "heads/",
];

/// Trims what surrounds a token in prose: quotes, brackets and sentence punctuation.
fn trim_prose(tok: &str) -> &str {
    let tok = tok.trim_start_matches(['(', '[', '<', '"', '\'', '*']);
    tok.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '>', '"', '\'', '*'])
        .trim_end_matches(',')
}

/// Drops `:line` and `:line:col` suffixes.
fn strip_line(tok: &str) -> &str {
    let mut tok = tok;
    for _ in 0..2 {
        match tok.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.chars().all(|c| c.is_ascii_digit() || c == '-') =>
            {
                tok = head;
            }
            _ => break,
        }
    }
    tok
}

/// The path `tok` names, or `None` when it is not one. `quoted` is true for a backticked token.
fn clean(tok: &str, quoted: bool) -> Option<String> {
    let tok = strip_line(trim_prose(tok));
    let tok = trim_prose(tok);
    let tok = tok.strip_suffix("/*").unwrap_or(tok);
    let tok = tok.strip_suffix('*').unwrap_or(tok);
    let tok = tok.trim_end_matches('/');
    if tok.is_empty() || tok.contains("://") || tok.starts_with('/') || tok.starts_with('~') {
        return None;
    }
    // Only plain path characters: no shell, glob or template characters, no spaces.
    if !tok
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return None;
    }
    let parts: Vec<&str> = tok.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    let lower = tok.to_ascii_lowercase();
    if REF_PREFIXES.iter().any(|p| lower.starts_with(p)) || PROSE.contains(&lower.as_str()) {
        return None;
    }
    // A version: `v1/v2`, `1.2/3`, or digits and dots only.
    let first = parts[0];
    if first.chars().all(|c| c.is_ascii_digit() || c == '.')
        || (first.starts_with('v') && first[1..].chars().all(|c| c.is_ascii_digit() || c == '.'))
            && first.len() > 1
    {
        return None;
    }
    if parts.len() == 1 {
        // A bare name must be backticked and end in a known file extension.
        let (stem, ext) = tok.rsplit_once('.')?;
        let known = FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str());
        return (quoted && known && !stem.is_empty()).then(|| tok.to_string());
    }
    Some(tok.to_string())
}

/// The repo paths `brief` names, in order, without repeats, at most [`MAX_PATHS`]. A candidate
/// is a backticked token, or a token with a `/` and no `://`. Branch names, URLs, absolute
/// paths, versions, prose like `and/or` and tokens with shell or template characters are left
/// out. A trailing `/`, `/*`, `*` and `:line` are stripped.
pub fn paths_in(brief: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut add = |p: Option<String>| {
        if let Some(p) = p
            && found.len() < MAX_PATHS
            && !found.contains(&p)
        {
            found.push(p);
        }
    };
    // Backticked spans first (one line each); the rest is read as plain words.
    let mut plain = String::new();
    for (i, piece) in brief.split('`').enumerate() {
        if i % 2 == 1 && !piece.contains('\n') {
            add(clean(piece, true));
            plain.push(' ');
        } else {
            plain.push_str(piece);
            if i % 2 == 1 {
                plain.push(' ');
            }
        }
    }
    for word in plain.split_whitespace() {
        if word.contains('/') {
            add(clean(word, false));
        }
    }
    found
}

/// The shell command that lists which of `paths` are absent from the checkout's `HEAD` (the
/// base, before the run commits). It prints `MISSING <path>` for each, and `NOGIT` with exit 3
/// when there is no commit to read. `paths` must come from [`paths_in`], so quoting is safe.
pub fn command(paths: &[String]) -> String {
    let list: Vec<String> = paths.iter().map(|p| format!("'{p}'")).collect();
    format!(
        "git rev-parse -q --verify HEAD >/dev/null 2>&1 || {{ echo NOGIT; exit 3; }}; \
         for p in {}; do git cat-file -e \"HEAD:$p\" 2>/dev/null || echo \"MISSING $p\"; done",
        list.join(" ")
    )
}

/// The paths [`command`] reported missing, from its exit code and output. An error when the
/// check did not run, so a broken check is never read as "all missing".
pub fn missing_from(exit: i64, output: &str, paths: &[String]) -> Result<Vec<String>, String> {
    if exit != 0 || output.lines().any(|l| l.trim() == "NOGIT") {
        return Err(format!("the path check exited {exit}"));
    }
    Ok(paths
        .iter()
        .filter(|p| {
            output.lines().any(|l| {
                l.strip_prefix("MISSING ")
                    .is_some_and(|m| m.trim() == p.as_str())
            })
        })
        .cloned()
        .collect())
}

/// The one-line `preflight` sentence for the start answer: `checked` paths, of which `missing`
/// are absent at `base`.
pub fn sentence(checked: usize, missing: &[String], base: &str) -> String {
    if checked == 0 {
        "preflight: the brief names no repo paths.".to_string()
    } else if missing.is_empty() {
        format!("preflight: all {checked} paths named in the brief exist at `{base}`.")
    } else {
        format!(
            "preflight: {} of {checked} paths named in the brief do not exist at `{base}`: {}.",
            missing.len(),
            missing.join(", ")
        )
    }
}

/// The note queued to the run before its first turn, or `None` when nothing is missing.
pub fn note_for(missing: &[String], base: &str) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "Preflight: these paths named in your brief do not exist at `{base}`: {}. The brief may \
         come from another repository. Look for the real paths in your checkout (list the \
         directories, grep for the names) instead of asking about them.",
        missing.join(", ")
    ))
}

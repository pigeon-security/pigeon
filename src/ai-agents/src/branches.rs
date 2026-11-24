//! The two agent tools `branch_restack` and `branch_combine`: their input checks, the
//! ownership rule, the git scripts they run in the checkout, and how the scripts' output is read.
//!
//! This module is pure. It does no I/O. The run's object fetches, runs the scripts through the
//! sandbox's `run` op and pushes with `commit_push {force}`; no sandbox op is added.
//!
//! Choices made here:
//! - Merge commits are skipped (`--no-merges`): their parents' commits are picked on their own,
//!   so the content arrives, and a cherry-pick of a merge needs `-m`.
//! - A commit named by two branches is picked once, at its first place in the list.
//! - The planner's own branch is read from the checkout's `HEAD`, so commits not pushed yet count.

use serde_json::{Map, Value};

/// The branch every restack and combine starts from. It is never moved.
pub const MAIN: &str = "main";

/// The prefix of every run's branch.
pub const PREFIX: &str = "claude/ai-agent-";

/// The most branches one `branch_combine` takes.
pub const MAX_BRANCHES: usize = 10;

/// Marks the start of a conflict in a script's output.
pub const CONFLICT_MARK: &str = "@@CONFLICT@@";

/// Marks the end of the conflicting paths in a script's output.
pub const END_MARK: &str = "@@END@@";

/// Marks the last line of a script that went through.
pub const DONE_MARK: &str = "@@DONE@@";

/// Whether `name` is a plain branch name that is safe in a shell word.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.contains("..")
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Checks a `branch_restack` input: an object with no keys.
pub fn check_restack(input: &Value) -> Result<(), String> {
    let obj = input
        .as_object()
        .ok_or("the input to `branch_restack` must be a JSON object; send {}")?;
    if let Some(k) = obj.keys().next() {
        return Err(format!(
            "`{k}` is not an input of `branch_restack`; its inputs are: none"
        ));
    }
    Ok(())
}

/// Checks a `branch_combine` input and reads the branch names in order, each once.
pub fn parse_combine(input: &Value) -> Result<Vec<String>, String> {
    let obj: &Map<String, Value> = input
        .as_object()
        .ok_or("the input to `branch_combine` must be a JSON object; send {\"branches\": [..]}")?;
    if let Some(k) = obj.keys().find(|k| k.as_str() != "branches") {
        return Err(format!(
            "`{k}` is not an input of `branch_combine`; its inputs are: branches"
        ));
    }
    let list = obj
        .get("branches")
        .and_then(Value::as_array)
        .ok_or("`branches` is required and must be an array of branch names")?;
    let mut out: Vec<String> = Vec::new();
    for v in list {
        let name = v
            .as_str()
            .ok_or("every entry of `branches` must be a string")?
            .trim();
        if !plain(name) {
            return Err(format!("`{name}` is not a branch name this tool takes"));
        }
        if !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err("`branches` must name at least one branch".into());
    }
    if out.len() > MAX_BRANCHES {
        return Err(format!(
            "`branches` names {} branches and the limit is {MAX_BRANCHES}",
            out.len()
        ));
    }
    Ok(out)
}

/// The branches a planner may combine: its own, then one per executor in the index's `children`
/// answer (rows with a `run` field).
pub fn owned(own: &str, children: &Value) -> Vec<String> {
    let mut out = vec![own.to_string()];
    for c in children.as_array().into_iter().flatten() {
        if let Some(run) = c["run"].as_str() {
            out.push(format!("{PREFIX}{run}"));
        }
    }
    out
}

/// Refuses a name that is `main` or is not in `owned`.
pub fn check_ownership(own: &str, children: &Value, names: &[String]) -> Result<(), String> {
    let ok = owned(own, children);
    for n in names {
        if n == MAIN || n == "origin/main" {
            return Err(
                "`main` is never moved or combined; name your own branch or an executor's".into(),
            );
        }
        if !ok.contains(n) {
            return Err(format!(
                "`{n}` is not your branch or one of your executors' branches; you may name: {}",
                ok.join(", ")
            ));
        }
    }
    Ok(())
}

/// The script for `branch_restack`: rebases the checkout onto `origin/main`. On a failure it
/// prints the conflicting paths between the marks, aborts the rebase and exits 1.
pub fn restack_script() -> String {
    format!(
        "if ! out=$(git rebase origin/{MAIN} 2>&1); then\n\
         echo {CONFLICT_MARK}\n\
         git diff --name-only --diff-filter=U\n\
         echo {END_MARK}\n\
         git rebase --abort >/dev/null 2>&1\n\
         echo \"$out\" | tail -n 5\n\
         exit 1\n\
         fi\n\
         echo {DONE_MARK}\n\
         echo \"$out\" | tail -n 3\n"
    )
}

/// The ref a branch is read from: the checkout's `HEAD` for the planner's own branch, else the
/// fetched `origin/<branch>`.
fn source(own: &str, branch: &str) -> String {
    if branch == own {
        "HEAD".into()
    } else {
        format!("origin/{branch}")
    }
}

/// The script that lists what `branch_combine` needs, one line each: `DIRTY` if the checkout
/// has uncommitted changes, `OWN <sha>` for each commit of `HEAD` not in main, and
/// `B <branch> <sha>` for each commit of each branch not in main, oldest first. A commit whose
/// change main already holds (`--cherry-pick`) is left out. The lines come from a shell loop,
/// not `sed`, because a branch name has `/` in it.
pub fn list_script(own: &str, names: &[String]) -> String {
    let mut s = format!(
        "st=$(git status --porcelain) || exit 2\n\
         [ -z \"$st\" ] || echo DIRTY\n\
         o=$(git rev-list --no-merges origin/{MAIN}..HEAD) || exit 2\n\
         for c in $o; do echo \"OWN $c\"; done\n"
    );
    for n in names {
        let src = source(own, n);
        s.push_str(&format!(
            "o=$(git rev-list --no-merges --topo-order --reverse --cherry-pick --right-only origin/{MAIN}...{src}) || {{ echo \"FAIL {n}\"; exit 2; }}\n\
             for c in $o; do echo \"B {n} $c\"; done\n"
        ));
    }
    s
}

/// What `list_script` printed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The checkout has uncommitted changes.
    pub dirty: bool,
    /// Commits of the checkout's `HEAD` that are not in main.
    pub own: Vec<String>,
    /// Each branch asked for, with its commits not in main, oldest first.
    pub branches: Vec<(String, Vec<String>)>,
}

/// Reads the output of `list_script` for `names`. A branch with no lines has no commits.
pub fn parse_listing(output: &str, names: &[String]) -> Listing {
    let mut l = Listing {
        branches: names.iter().map(|n| (n.clone(), Vec::new())).collect(),
        ..Listing::default()
    };
    for line in output.lines() {
        let line = line.trim();
        if line == "DIRTY" {
            l.dirty = true;
        } else if let Some(sha) = line.strip_prefix("OWN ") {
            l.own.push(sha.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("B ")
            && let Some((b, sha)) = rest.rsplit_once(' ')
            && let Some(entry) = l.branches.iter_mut().find(|(n, _)| n == b)
        {
            entry.1.push(sha.trim().to_string());
        }
    }
    l
}

/// Builds the cherry-pick plan: per branch, in the order given, the commits not already picked
/// for an earlier branch. Refuses a dirty checkout, and refuses when the planner's own branch
/// holds commits that no listed branch covers, unless the list names the planner's own branch.
pub fn plan(
    own: &str,
    names: &[String],
    listing: &Listing,
) -> Result<Vec<(String, Vec<String>)>, String> {
    if listing.dirty {
        return Err(
            "the checkout has uncommitted changes; commit_push or discard them first".into(),
        );
    }
    if !names.iter().any(|n| n == own) {
        let covered = |sha: &String| listing.branches.iter().any(|(_, c)| c.contains(sha));
        let lost: Vec<&String> = listing.own.iter().filter(|s| !covered(s)).collect();
        if !lost.is_empty() {
            return Err(format!(
                "your own branch holds {} commit(s) not in main and not in the list, and combining would drop them; add `{own}` to `branches`",
                lost.len()
            ));
        }
    }
    let mut seen: Vec<&String> = Vec::new();
    let mut out = Vec::new();
    for (b, shas) in &listing.branches {
        let fresh: Vec<String> = shas.iter().filter(|s| !seen.contains(s)).cloned().collect();
        seen.extend(shas.iter());
        out.push((b.clone(), fresh));
    }
    Ok(out)
}

/// The script that resets to `origin/main` and cherry-picks each branch's commits in order. On a
/// conflict it prints the branch and the paths, aborts, resets back to where it began and
/// exits 1. Only commit ids and checked branch names go into it.
pub fn combine_script(plan: &[(String, Vec<String>)]) -> String {
    let mut s = format!(
        "orig=$(git rev-parse HEAD) || exit 2\n\
         git reset -q --hard origin/{MAIN} || exit 2\n"
    );
    for (b, shas) in plan.iter().filter(|(_, s)| !s.is_empty()) {
        s.push_str(&format!(
            "if ! out=$(git cherry-pick {} 2>&1); then\n\
             echo \"{CONFLICT_MARK} {b}\"\n\
             git diff --name-only --diff-filter=U\n\
             echo {END_MARK}\n\
             git cherry-pick --abort >/dev/null 2>&1\n\
             git reset -q --hard \"$orig\"\n\
             echo \"$out\" | tail -n 5\n\
             exit 1\n\
             fi\n",
            shas.join(" ")
        ));
    }
    s.push_str(&format!("echo {DONE_MARK}\n"));
    s
}

/// A conflict read from a script's output.
#[derive(Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The branch being picked, for `branch_combine`.
    pub branch: Option<String>,
    /// The conflicting paths.
    pub paths: Vec<String>,
}

/// Reads a conflict from a script's output: the paths between the marks. `None` if there is no
/// mark.
pub fn parse_conflicts(output: &str) -> Option<Conflict> {
    let mut lines = output.lines();
    let head = lines.find_map(|l| l.trim().strip_prefix(CONFLICT_MARK).map(str::to_string))?;
    let branch = Some(head.trim().to_string()).filter(|b| !b.is_empty());
    let paths = lines
        .map(str::trim)
        .take_while(|l| *l != END_MARK)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    Some(Conflict { branch, paths })
}

/// The exit code in what the sandbox's `run` tool answers (`exit N after ...`).
pub fn parse_exit(answer: &str) -> Option<i64> {
    answer
        .lines()
        .next()?
        .strip_prefix("exit ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// What a conflict says to the model.
pub fn conflict_text(tool: &str, c: &Conflict) -> String {
    let on = c
        .branch
        .as_deref()
        .map(|b| format!(" while picking `{b}`"))
        .unwrap_or_default();
    format!(
        "{tool} stopped on a conflict{on}; nothing was pushed and the checkout is as it was. Conflicting files: {}",
        c.paths.join(", ")
    )
}

//! What a planner hands over when it ends, and the warning it gets at 75% of its budget.
//!
//! Everything here is pure. The run's Durable Object does the lookups (the index's `children`,
//! `land_list`, `ci_commit`) and passes the answers in. A lookup that failed is passed as
//! nothing, and the row says so.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::budget::{Budget, Over, Spent, WARN_PERCENT, money};

/// The heading of the machine-made section of a planner's report.
pub const HEADING: &str = "## Unlanded branches";

/// The most executor rows in the section.
const MAX_ROWS: usize = 40;

/// The user message a planner gets once, when `over` passes [`WARN_PERCENT`] of its budget.
pub fn warning_text(over: Over, budget: &Budget, spent: &Spent) -> String {
    let what = match over {
        Over::Sandbox => format!(
            "{} of {} sandbox minutes",
            spent.sandbox_ms / 60_000,
            budget.sandbox_ms / 60_000
        ),
        _ => format!(
            "{} of {} cost budget",
            money(spent.committed_micros()),
            money(budget.cost_micros)
        ),
    };
    format!(
        "Budget warning from pigeon: you have used {what}, over {WARN_PERCENT}%. Stop spawning \
         executors. Land or hand off what is green, then call `report`. Your report ends with an \
         automatic list of executor branches that are not landed."
    )
}

/// The last 40-character commit id in `text`, lowercase, if there is one.
///
/// An executor's report says its branch tip in words; this is how the tip is found.
pub fn tip_from_report(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .rfind(|w| w.len() == 40 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

/// The branch names a `land_list` answer shows as merged or landed. The answer is read as an
/// array, or an object holding one.
pub fn landed_branches(list: &Value) -> HashSet<String> {
    let rows = list.as_array().or_else(|| {
        list.as_object()
            .and_then(|o| o.values().find_map(Value::as_array))
    });
    rows.into_iter()
        .flatten()
        .filter(|r| matches!(r["state"].as_str(), Some("merged") | Some("landed")))
        .filter_map(|r| r["branch"].as_str())
        .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b).to_string())
        .collect()
}

/// A short CI status for a commit from a `ci_commit` answer: the state of its newest `test` job,
/// else of its newest job, else "none recorded".
pub fn ci_word(answer: &Value) -> String {
    let jobs: Vec<&Value> = answer["jobs"].as_array().into_iter().flatten().collect();
    let pick = jobs
        .iter()
        .find(|j| j["job"] == "test")
        .or_else(|| jobs.first());
    match pick.and_then(|j| j["state"].as_str()) {
        Some(state) => state.to_string(),
        None => "none recorded".to_string(),
    }
}

/// Makes text safe inside one table cell.
fn cell(text: &str) -> String {
    text.replace('|', "/").replace(['\n', '\r'], " ")
}

/// The executors to list: those of `children` whose branch is not in `landed`.
pub fn unlanded<'a>(children: &'a Value, landed: &HashSet<String>) -> Vec<&'a Value> {
    children
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["branch"].as_str().is_some_and(|b| !b.is_empty()))
        .filter(|c| !landed.contains(c["branch"].as_str().unwrap_or_default()))
        .take(MAX_ROWS)
        .collect()
}

/// The section "## Unlanded branches": one row per executor not landed, with its branch, tip,
/// end state and CI status. `ci` maps a tip to its status; a tip with no entry reads "unknown".
/// A child with no tip in its report shows "unknown" for both. Answers `None` when there are no
/// rows to show.
pub fn section(
    children: &Value,
    landed: &HashSet<String>,
    ci: &HashMap<String, String>,
) -> Option<String> {
    let rows = unlanded(children, landed);
    if rows.is_empty() {
        return None;
    }
    let mut out = format!(
        "{HEADING}\n\nMade by pigeon from the executors this run spawned. The tip is read from \
         the executor's report; CI is `unknown` when it was not looked up.\n\n\
         | Branch | Tip | State | CI |\n| --- | --- | --- | --- |\n"
    );
    for c in rows {
        let branch = c["branch"].as_str().unwrap_or_default();
        let tip = c["report"].as_str().and_then(tip_from_report);
        let ci_text = tip
            .as_ref()
            .and_then(|t| ci.get(t))
            .map(String::as_str)
            .unwrap_or("unknown");
        let state = match c["state"].as_str() {
            Some(s) if c["ended_at"].is_null() => format!("{s} (not ended)"),
            Some(s) => s.to_string(),
            None => "unknown".to_string(),
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            cell(branch),
            tip.as_deref()
                .map(|t| format!("`{t}`"))
                .unwrap_or_else(|| "unknown".into()),
            cell(&state),
            cell(ci_text),
        ));
    }
    Some(out)
}

/// The report with `section` after the model's own text. A report that already has the heading
/// is left as it is, so ending twice adds nothing.
pub fn append(report: &str, section: &str) -> String {
    if report.contains(HEADING) {
        return report.to_string();
    }
    let body = report.trim_end();
    if body.is_empty() {
        section.to_string()
    } else {
        format!("{body}\n\n{section}")
    }
}

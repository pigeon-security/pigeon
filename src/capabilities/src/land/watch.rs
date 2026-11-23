//! Events a landing sends to the AI agent runs that watch it: which state changes are worth
//! one, the text of each, and what to do when a run cannot take it. Pure, so it is testable.

use super::digest::{cut, one_line};
use super::state::State;

/// The most bytes an event's text takes.
pub const EVENT_BYTES: usize = 600;
/// How many alarms one event is tried in before it is dropped.
pub const EVENT_TRIES: i64 = 5;
/// How many events one alarm delivers at most.
pub const EVENT_BATCH: i64 = 10;

/// Whether a landing reaching `state` is told to its watchers: when it waits for a person, is
/// merged or landed, or has ended.
pub fn notable(state: State) -> bool {
    state.terminal()
        || matches!(
            state,
            State::AwaitingApproval | State::Merged | State::Landed
        )
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut_bytes(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The text of the event for a landing that is now in `state`: under [`EVENT_BYTES`].
pub fn event_text(id: &str, branch: &str, commit12: &str, state: &str, detail: &str) -> String {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let head = format!(
        "Landing {} ({} at {}) is now {}",
        cut(id, 40),
        cut(branch, 80),
        cut(commit12, 12),
        cut(state, 30)
    );
    let detail = one_line(detail, 200);
    let text = if detail.is_empty() {
        format!("{head}. land_status {{id, compact: true}} for more.")
    } else {
        format!("{head}: {detail}. land_status {{id, compact: true}} for more.")
    };
    cut_bytes(&text, EVENT_BYTES)
}

/// The text of the event for an incident on `worker` that included landing `id`.
pub fn incident_text(id: &str, worker: &str, summary: &str) -> String {
    let text = format!(
        "Incident on Worker {}: {}. Landing {} was included. land_status {{id: \"{}\", compact: true}} for more.",
        cut(worker, 60),
        one_line(summary, 250),
        cut(id, 40),
        cut(id, 40)
    );
    cut_bytes(&text, EVENT_BYTES)
}

/// Whether `why`, an error from `ai_agents_note`, says the run is gone for good.
pub fn run_gone(why: &str) -> bool {
    why.contains("is not a live gateway run") || why.contains("no run")
}

/// Whether an event that failed with `why` after `tries` earlier tries is dropped.
pub fn drop_event(why: &str, tries: i64) -> bool {
    run_gone(why) || tries + 1 >= EVENT_TRIES
}

//! A run's states and its next action. The run's Durable Object keeps what the run knows (its
//! messages, its model job in flight, the tool calls of its last turn, what it waits for) and
//! asks [`next`] what to do, once per alarm; it carries the action out, records it, and asks
//! again. Every action is either safe to repeat or marked started before its side effect, so a
//! run restarted by a deploy picks up where it was ([`Call::started`]).
//!
//! ```text
//! queued ─► preparing ─► running ⇄ waiting
//!                           └─► done | failed | stopped | over_budget
//! ```
//! Any live state may also end `stopped` (the kill switch) or `failed`.

use serde::{Deserialize, Serialize};

use crate::tools;

/// A run's state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting for room: too many runs, or too many sandboxes.
    Queued,
    /// Its sandbox is starting and its branch is being checked out.
    Preparing,
    /// Asking its model, or running the tools its model called.
    Running,
    /// Waiting for the person, an approval, its executors or the model provider.
    Waiting,
    /// It reported its work done.
    Done,
    /// It reported failure, or could not go on.
    Failed,
    /// The person or its planner stopped it.
    Stopped,
    /// It would have gone over its budget.
    OverBudget,
}

impl State {
    /// Its name, as kept and shown.
    pub fn name(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Preparing => "preparing",
            State::Running => "running",
            State::Waiting => "waiting",
            State::Done => "done",
            State::Failed => "failed",
            State::Stopped => "stopped",
            State::OverBudget => "over_budget",
        }
    }

    /// The state named `s`.
    pub fn named(s: &str) -> Option<State> {
        [
            State::Queued,
            State::Preparing,
            State::Running,
            State::Waiting,
            State::Done,
            State::Failed,
            State::Stopped,
            State::OverBudget,
        ]
        .into_iter()
        .find(|x| x.name() == s)
    }

    /// Whether it never changes again.
    pub fn terminal(self) -> bool {
        matches!(
            self,
            State::Done | State::Failed | State::Stopped | State::OverBudget
        )
    }
}

/// Whether a run may move from `from` to `to`: forward through its life, back and forth
/// between running and waiting, and to an end from any live state.
pub fn allowed(from: State, to: State) -> bool {
    use State::*;
    if from.terminal() {
        return false;
    }
    match to {
        Queued => false,
        Preparing => from == Queued,
        Running => matches!(from, Preparing | Waiting),
        Waiting => from == Running,
        Done => matches!(from, Running | Waiting),
        Failed | Stopped | OverBudget => true,
    }
}

/// What a waiting run waits for.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "for", rename_all = "snake_case")]
pub enum Wait {
    /// The person's passkey on a tool call: pigeon's approval `id`, for tool call `call`.
    Approval {
        /// The approval's id.
        id: String,
        /// The tool call waiting on it.
        call: String,
    },
    /// The person's answer to a question the run asked with `ask_person`.
    Answer {
        /// The tool call that asked.
        call: String,
        /// The question, as the person sees it.
        question: String,
        /// The answers offered, when the choice is a short list.
        #[serde(default)]
        options: Vec<String>,
        /// When it was asked (ms).
        #[serde(default)]
        since: i64,
    },
    /// Its executors: a planner's `wait_executors`, until one ends (ms since it began).
    Executors {
        /// When the wait began.
        #[serde(default)]
        since: i64,
    },
    /// The model provider or the gateway, failing since `since` (ms).
    Provider {
        /// When the failures began.
        since: i64,
    },
}

/// The model job of the run's turn in flight, as `models_status` last said.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Job {
    /// Queued or running at the gateway.
    Pending,
    /// Finished with an answer to read.
    Succeeded,
    /// Ended without one: failed, timed out, lost or cancelled.
    Ended,
}

/// One tool call of the last turn, in the order the model made them.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The model's id for the call (`tool_use.id`).
    pub id: String,
    /// The tool.
    pub name: String,
    /// Marked before its side effect: a call started but not done was interrupted.
    pub started: bool,
    /// Its result is recorded.
    pub done: bool,
}

/// What the run knows, as its object gives it to [`next`].
#[derive(Clone, Debug)]
pub struct View<'a> {
    /// Its state.
    pub state: State,
    /// The person or its planner asked it to stop.
    pub stop_asked: bool,
    /// Every run is paused (`ai_agents_pause`).
    pub paused: bool,
    /// Its turn in flight, and how long it has been in flight (ms).
    pub job: Option<(Job, i64)>,
    /// The tool calls of its last turn.
    pub calls: &'a [Call],
    /// How many turns it has taken.
    pub turns: u32,
    /// Why its model stopped last turn (`end_turn`, `tool_use`, `max_tokens`, ...).
    pub stop_reason: Option<&'a str>,
    /// Whether it called `report`, and with which state.
    pub reported: Option<State>,
    /// How many times it was reminded to report.
    pub nudges: u32,
    /// Whether the last message is the user's (a brief, results, a reminder, a summary): the
    /// model's turn is next.
    pub user_last: bool,
    /// What it waits for, when waiting.
    pub waiting: Option<&'a Wait>,
}

/// The next thing a run does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing: it has ended.
    Idle,
    /// Start its sandbox and check out its base; then it is running.
    Prepare,
    /// Ask its model for the next turn (a model job in the `ai-agents` lane).
    Turn,
    /// Ask the gateway how its turn is doing, after `after_ms`.
    Poll {
        /// How long to wait first.
        after_ms: i64,
    },
    /// Read its finished turn: its content goes into the messages, its tool calls into
    /// [`View::calls`].
    Read,
    /// The turn's job ended without an answer: retry it, or fall back ([`on_failure`]).
    Retry,
    /// Run tool call `index` of the last turn.
    Call {
        /// The call's place in [`View::calls`].
        index: usize,
    },
    /// Tool call `index` was interrupted and is not safe to repeat: answer the model that its
    /// effect is unknown.
    Interrupted {
        /// The call's place in [`View::calls`].
        index: usize,
    },
    /// Remind the model to report (it stopped without calling `report`), then take a turn.
    Nudge,
    /// Look again at what it waits for, after `after_ms`.
    Check {
        /// How long to wait first.
        after_ms: i64,
    },
    /// End in this state.
    End(State),
}

/// How often a model may stop without reporting before the run fails.
pub const MAX_NUDGES: u32 = 2;
/// How often a paused or waiting run looks again, in ms.
pub const CHECK_EVERY_MS: i64 = 15_000;
/// How often a planner looks at its executors, in ms.
pub const EXECUTORS_EVERY_MS: i64 = 30_000;
/// How long a run waits for the person's answer before it goes on without one, in ms: five
/// minutes, the person's choice. What it then assumes goes in its report and the inbox.
pub const ANSWER_PATIENCE_MS: i64 = 5 * 60_000;

/// What the model reads when its question went unanswered for [`ANSWER_PATIENCE_MS`].
pub fn unanswered(question: &str) -> String {
    format!(
        "No answer came within 5 minutes to: {question}\nChoose the conventional option and go \
         on. Say in one line what you chose, and list it in your report under \"Assumed:\", so \
         the person can see it and correct it."
    )
}
/// How long a run waits on a failing provider before it fails, in ms.
pub const PROVIDER_PATIENCE_MS: i64 = 60 * 60_000;
/// How often a run tries a failing provider again, in ms.
pub const PROVIDER_EVERY_MS: i64 = 5 * 60_000;

/// How long to wait before asking about a turn in flight for `elapsed` ms: often at first,
/// then less.
pub fn poll_after(elapsed: i64) -> i64 {
    match elapsed {
        e if e < 5_000 => 1_000,
        e if e < 30_000 => 2_000,
        _ => 5_000,
    }
}

/// The run's next action.
pub fn next(v: &View) -> Action {
    if v.state.terminal() {
        return Action::Idle;
    }
    if v.stop_asked {
        return Action::End(State::Stopped);
    }
    if v.paused {
        return Action::Check {
            after_ms: CHECK_EVERY_MS,
        };
    }
    match v.state {
        State::Queued | State::Preparing => return Action::Prepare,
        State::Waiting => {
            return match v.waiting {
                Some(Wait::Executors { .. }) => Action::Check {
                    after_ms: EXECUTORS_EVERY_MS,
                },
                Some(Wait::Provider { .. }) => Action::Check {
                    after_ms: PROVIDER_EVERY_MS,
                },
                Some(_) => Action::Check {
                    after_ms: CHECK_EVERY_MS,
                },
                // Waiting on nothing: back to work.
                None => Action::Turn,
            };
        }
        _ => {}
    }
    // Running.
    if let Some((job, elapsed)) = v.job {
        return match job {
            Job::Pending => Action::Poll {
                after_ms: poll_after(elapsed),
            },
            Job::Succeeded => Action::Read,
            Job::Ended => Action::Retry,
        };
    }
    if let Some((index, call)) = v.calls.iter().enumerate().find(|(_, c)| !c.done) {
        return if call.started && !tools::idempotent(&call.name) {
            Action::Interrupted { index }
        } else {
            Action::Call { index }
        };
    }
    if let Some(state) = v.reported {
        return Action::End(state);
    }
    if v.turns == 0 || !v.calls.is_empty() || v.user_last {
        // The first turn, the results of the last turn's calls, or a reminder: the model's turn.
        return Action::Turn;
    }
    // The model stopped without calling a tool and without reporting.
    match v.stop_reason {
        Some("max_tokens") | Some("pause_turn") => Action::Turn,
        _ if v.nudges >= MAX_NUDGES => Action::End(State::Failed),
        _ => Action::Nudge,
    }
}

/// What to do about a turn whose job ended without an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// Send the same turn again on the same model.
    Again,
    /// Send it on the next model of the role's list (`models()[index]`), thinking stripped.
    Fallback {
        /// The model's place in the role's list.
        index: usize,
    },
    /// Every model failed: wait for the provider, and tell the person.
    WaitProvider,
    /// Give up: the failure is not the provider's (a refused request, an over-long context).
    Fail,
}

/// Whether a job's failure is the provider's or the gateway's and worth trying again: overload,
/// rate limits, server errors, a lost runner or a timeout. A refused request (400s) is not.
pub fn transient(state: &str, error: &str) -> bool {
    if matches!(state, "lost" | "timed_out") {
        return true;
    }
    let e = error.to_ascii_lowercase();
    [
        "overloaded",
        "rate_limit",
        "answered 429",
        "answered 500",
        "answered 502",
        "answered 503",
        "answered 504",
        "answered 529",
        "api_error",
        "the stream ended before",
    ]
    .iter()
    .any(|p| e.contains(p))
}

/// What to do after a turn's job ended in `state` with `error`, the `failures`th failure in a
/// row on model `index` of `models`.
pub fn on_failure(
    state: &str,
    error: &str,
    failures: u32,
    index: usize,
    models: &[&str],
) -> Recovery {
    if state == "cancelled" || !transient(state, error) {
        return Recovery::Fail;
    }
    if failures < 2 {
        return Recovery::Again;
    }
    if index + 1 < models.len() {
        Recovery::Fallback { index: index + 1 }
    } else {
        Recovery::WaitProvider
    }
}

/// How many turns a fallback model is kept for before the first choice is tried again.
pub const FALLBACK_TURNS: u32 = 10;

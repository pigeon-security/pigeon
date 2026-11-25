//! The life of one CI job: its states and what moves it between them. A report from a runner or
//! connector, a passed deadline or a caller's cancel each come here and leave with what to do.
//! Pure: no I/O, no clock.

use serde::{Deserialize, Serialize};

/// The state of one CI job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting to be handed to a connector.
    Queued,
    /// Handed to a connector, not yet seen running.
    Dispatched,
    /// Seen running on a runner.
    Running,
    /// Finished and passed.
    Succeeded,
    /// Finished and did not pass.
    Failed,
    /// Cancelled by a caller or at the connector.
    Cancelled,
    /// Ran past its deadline.
    TimedOut,
    /// Could not be run, for want of working infrastructure.
    Error,
}

const ALL_STATES: [State; 8] = [
    State::Queued,
    State::Dispatched,
    State::Running,
    State::Succeeded,
    State::Failed,
    State::Cancelled,
    State::TimedOut,
    State::Error,
];

impl State {
    /// The state's name, such as `timed_out`.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Dispatched => "dispatched",
            State::Running => "running",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::TimedOut => "timed_out",
            State::Error => "error",
        }
    }

    /// The state named `s`.
    pub fn parse(s: &str) -> Option<State> {
        ALL_STATES.into_iter().find(|st| st.as_str() == s)
    }

    /// Whether the state is final.
    pub fn terminal(self) -> bool {
        matches!(
            self,
            State::Succeeded | State::Failed | State::Cancelled | State::TimedOut | State::Error
        )
    }
}

/// Whether a job may move from `from` to `to`, not counting a retry. A dispatched job may time
/// out: its deadline can pass before a runner starts it.
pub fn allowed(from: State, to: State) -> bool {
    use State::*;
    match from {
        Queued => matches!(to, Dispatched | Cancelled | Error),
        Dispatched => matches!(
            to,
            Running | Succeeded | Failed | TimedOut | Cancelled | Error
        ),
        Running => matches!(to, Succeeded | Failed | TimedOut | Cancelled | Error),
        Succeeded | Failed | Cancelled | TimedOut | Error => false,
    }
}

/// What a connector or runner saw of a job's run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    /// The run started.
    Started,
    /// The run finished with a result.
    Finished { passed: bool },
    /// The run was cancelled at the connector.
    Cancelled,
    /// The connector itself timed the run out.
    TimedOut,
    /// Dispatch was refused, the runner was lost, or it exited without a result.
    Infra { why: String },
}

/// What should happen to a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Move the job to this state.
    Move(State),
    /// Queue the job again, on the connector that lost it, as its next attempt.
    Retry,
    /// Leave the job as it is.
    Ignore,
}

/// The most attempts a job gets; an infrastructure failure on the last is an error.
pub const MAX_ATTEMPTS: u32 = 2;

/// What `report` does to a job in `state` on `attempt` (counted from 1).
pub fn on_report(state: State, attempt: u32, report: &Report) -> Next {
    // Late and duplicate reports never change a result; a job not yet dispatched has no run to
    // report on, so a report about it is not trusted.
    if state.terminal() || state == State::Queued {
        return Next::Ignore;
    }
    match report {
        Report::Started => match state {
            State::Dispatched => Next::Move(State::Running),
            _ => Next::Ignore,
        },
        Report::Finished { passed: true } => Next::Move(State::Succeeded),
        // A failure is a result, never retried.
        Report::Finished { passed: false } => Next::Move(State::Failed),
        Report::Cancelled => Next::Move(State::Cancelled),
        Report::TimedOut => Next::Move(State::TimedOut),
        Report::Infra { .. } => {
            if attempt < MAX_ATTEMPTS {
                Next::Retry
            } else {
                Next::Move(State::Error)
            }
        }
    }
}

/// What a job whose deadline has passed becomes, given the connector's last word on it
/// (`None`: nothing final known). There is no retry past a deadline.
pub fn on_deadline(state: State, last: Option<&Report>) -> Next {
    if state.terminal() || state == State::Queued {
        return Next::Ignore;
    }
    if let Some(r) = last
        && let Next::Move(to) = on_report(state, MAX_ATTEMPTS, r)
        && to.terminal()
    {
        return Next::Move(to);
    }
    Next::Move(State::TimedOut)
}

/// What a caller's cancel does.
pub fn on_cancel(state: State) -> Next {
    if state.terminal() {
        Next::Ignore
    } else {
        Next::Move(State::Cancelled)
    }
}

//! Async model jobs, the pure part: their states and the moves between them, how many run at
//! once, how long each may take and how long its result is kept, who may read it, and what a
//! notify says. The objects that run them are in `models/runner.rs`.
//!
//! ```text
//! queued ─► running ─► succeeded | failed | timed_out | lost
//!    └─────────────────► failed (could not start)
//! ```

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A job's state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    /// Its runner stopped without a result: restarted mid-stream, or silent past its deadline.
    Lost,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::TimedOut => "timed_out",
            State::Lost => "lost",
        }
    }

    pub fn named(s: &str) -> Option<State> {
        [
            State::Queued,
            State::Running,
            State::Succeeded,
            State::Failed,
            State::TimedOut,
            State::Lost,
        ]
        .into_iter()
        .find(|x| x.name() == s)
    }

    /// Whether it never changes again.
    pub fn terminal(self) -> bool {
        !matches!(self, State::Queued | State::Running)
    }
}

/// Whether a job may move from `from` to `to`: only forward, never out of a final state.
pub fn allowed(from: State, to: State) -> bool {
    match from {
        State::Queued => matches!(to, State::Running | State::Failed),
        State::Running => to.terminal(),
        _ => false,
    }
}

/// How many jobs run at once; more wait, queued.
pub const MAX_RUNNING: u32 = 4;
/// How many jobs may be queued or running at once; more are refused.
pub const MAX_OPEN: u32 = 50;
/// A job's timeout unless asked otherwise, and at most, in minutes. At most 14: a job streams in
/// an alarm, which has 15 minutes of wall time.
pub const DEFAULT_TIMEOUT_MIN: u64 = 10;
pub const MAX_TIMEOUT_MIN: u64 = 14;
/// How long past its deadline a silent running job is taken as lost.
pub const LOST_AFTER_MS: i64 = 2 * 60_000;
/// How long a result is kept at most, from submission.
pub const KEPT_MS: i64 = 24 * 3_600_000;
/// How long a result is kept once read, so a retried read still finds it.
pub const KEPT_AFTER_READ_MS: i64 = 10 * 60_000;
/// How often a running job reports its progress.
pub const PROGRESS_EVERY_MS: i64 = 15_000;
/// How many times a notify is tried, and the waits between.
pub const NOTIFY_WAITS_MS: [i64; 3] = [30_000, 120_000, 600_000];

/// A job's timeout in milliseconds, from `timeout_min`: 1 to [`MAX_TIMEOUT_MIN`].
pub fn timeout_ms(asked: &Value) -> Result<i64, String> {
    let min = match asked {
        Value::Null => DEFAULT_TIMEOUT_MIN,
        v => match v.as_u64() {
            Some(n) if (1..=MAX_TIMEOUT_MIN).contains(&n) => n,
            _ => return Err(format!("`timeout_min` must be from 1 to {MAX_TIMEOUT_MIN}")),
        },
    };
    Ok(min as i64 * 60_000)
}

/// Whether one more job may be submitted while `open` are queued or running.
pub fn may_submit(open: u32) -> Result<(), String> {
    if open >= MAX_OPEN {
        Err(format!(
            "{open} model jobs are queued or running, the most there may be; try again when \
             some have finished"
        ))
    } else {
        Ok(())
    }
}

/// How many queued jobs to start while `running` run.
pub fn to_start(running: u32, queued: u32) -> u32 {
    MAX_RUNNING.saturating_sub(running).min(queued)
}

/// When a job's row goes: [`KEPT_MS`] after submission, or [`KEPT_AFTER_READ_MS`] after its
/// result was first read, whichever is sooner.
pub fn expires_at(created_at: i64, read_at: Option<i64>) -> i64 {
    let kept = created_at + KEPT_MS;
    match read_at {
        Some(r) => kept.min(r + KEPT_AFTER_READ_MS),
        None => kept,
    }
}

/// Whether a running job whose deadline was `deadline_at` is lost by `now`.
pub fn lost(state: State, deadline_at: Option<i64>, now: i64) -> bool {
    state == State::Running && deadline_at.is_some_and(|d| now > d + LOST_AFTER_MS)
}

/// Whether `reader` may see a job sent by `caller` acting for `acting_for`: the sender, or the
/// person they acted for. A job without a sender is no one's.
pub fn may_read(caller: Option<&str>, acting_for: Option<&str>, reader: &str) -> bool {
    !reader.is_empty() && (caller == Some(reader) || acting_for == Some(reader))
}

/// When to try a notify again after `tries` failed ones; `None` once it has been tried enough.
pub fn notify_retry(tries: u32, now: i64) -> Option<i64> {
    NOTIFY_WAITS_MS
        .get(tries.saturating_sub(1) as usize)
        .map(|w| now + w)
}

/// What a notify says of a job that reached `state`: never its prompt or its answer.
#[allow(clippy::too_many_arguments)]
pub fn notify_body(
    job: &str,
    state: State,
    provider: &str,
    model: &str,
    task: Option<&str>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    at_ms: i64,
) -> Value {
    json!({
        "job": job,
        "kind": "model",
        "state": state.name(),
        "provider": provider,
        "model": model,
        "task": task,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "at": at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [State; 6] = [
        State::Queued,
        State::Running,
        State::Succeeded,
        State::Failed,
        State::TimedOut,
        State::Lost,
    ];

    #[test]
    fn moves_go_forward_only_and_final_states_stay() {
        for from in ALL {
            for to in ALL {
                let want = match (from, to) {
                    (State::Queued, State::Running | State::Failed) => true,
                    (State::Running, t) => t.terminal(),
                    _ => false,
                };
                assert_eq!(allowed(from, to), want, "{from:?} → {to:?}");
            }
        }
        assert!(!State::Queued.terminal() && !State::Running.terminal());
        for s in ALL {
            assert_eq!(State::named(s.name()), Some(s));
        }
        assert_eq!(State::named("done"), None);
    }

    #[test]
    fn four_run_at_once_and_fifty_wait_at_most() {
        assert_eq!(to_start(0, 10), 4);
        assert_eq!(to_start(3, 10), 1);
        assert_eq!(to_start(4, 10), 0);
        assert_eq!(to_start(9, 10), 0);
        assert_eq!(to_start(1, 1), 1);
        assert!(may_submit(49).is_ok());
        assert!(may_submit(50).is_err());
    }

    #[test]
    fn a_timeout_is_one_to_fourteen_minutes() {
        assert_eq!(timeout_ms(&Value::Null), Ok(10 * 60_000));
        assert_eq!(timeout_ms(&json!(14)), Ok(14 * 60_000));
        for bad in [json!(0), json!(15), json!(-1), json!("5"), json!(2.5)] {
            assert!(timeout_ms(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_result_is_kept_a_day_or_ten_minutes_after_it_is_read() {
        assert_eq!(expires_at(1_000, None), 1_000 + KEPT_MS);
        assert_eq!(expires_at(1_000, Some(5_000)), 5_000 + KEPT_AFTER_READ_MS);
        // Read just before the day is out: the day still ends it.
        let late = 1_000 + KEPT_MS - 1;
        assert_eq!(expires_at(1_000, Some(late)), 1_000 + KEPT_MS);
    }

    #[test]
    fn a_silent_running_job_is_lost_two_minutes_past_its_deadline() {
        assert!(!lost(State::Running, Some(100), 100 + LOST_AFTER_MS));
        assert!(lost(State::Running, Some(100), 101 + LOST_AFTER_MS));
        assert!(!lost(State::Queued, Some(100), i64::MAX));
        assert!(!lost(State::Running, None, i64::MAX));
    }

    #[test]
    fn only_the_sender_or_their_person_reads_a_job() {
        assert!(may_read(Some("s1"), Some("u1"), "s1"));
        assert!(may_read(Some("s1"), Some("u1"), "u1"));
        assert!(!may_read(Some("s1"), Some("u1"), "s2"));
        assert!(!may_read(None, None, ""));
        assert!(!may_read(Some("s1"), None, ""));
    }

    #[test]
    fn a_notify_is_tried_three_times_more() {
        assert_eq!(notify_retry(1, 0), Some(30_000));
        assert_eq!(notify_retry(3, 0), Some(600_000));
        assert_eq!(notify_retry(4, 0), None);
    }

    #[test]
    fn a_notify_never_carries_the_answer() {
        let b = notify_body(
            "j1",
            State::Succeeded,
            "anthropic",
            "m",
            Some("t"),
            Some(1),
            Some(2),
            3,
        );
        assert_eq!(b["state"], "succeeded");
        let keys: Vec<&String> = b.as_object().unwrap().keys().collect();
        for k in ["text", "prompt", "messages", "system", "result"] {
            assert!(!keys.iter().any(|x| *x == k), "{k}");
        }
    }
}

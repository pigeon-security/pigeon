//! A landing's states and the moves between them, which branches may land, which changes need a
//! person's approval whatever the setting, and the note a landing leaves. Pure.
//!
//! ```text
//! checking ─► testing ─► [awaiting_approval] ─► merging ─► deploying ─► landed
//!    │           │               │                 │            │
//!    ▼           ▼               ▼                 ▼            ▼
//! refused   test_failed       expired         merge_refused  deploy_failed
//! ```
//!
//! `checking` runs in the Lander's alarm after `land` has answered: a landing that fails a check
//! ends `refused`, with the reason. Any live state can become `error` (the gateway failed) or,
//! before `merging`, `cancelled`.

use serde::{Deserialize, Serialize};

/// A landing's state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Checking,
    Testing,
    AwaitingApproval,
    Merging,
    Deploying,
    Landed,
    Refused,
    TestFailed,
    Expired,
    MergeRefused,
    DeployFailed,
    Cancelled,
    Error,
}

impl State {
    pub const ALL: [State; 13] = [
        State::Checking,
        State::Testing,
        State::AwaitingApproval,
        State::Merging,
        State::Deploying,
        State::Landed,
        State::Refused,
        State::TestFailed,
        State::Expired,
        State::MergeRefused,
        State::DeployFailed,
        State::Cancelled,
        State::Error,
    ];

    pub fn name(self) -> &'static str {
        match self {
            State::Checking => "checking",
            State::Testing => "testing",
            State::AwaitingApproval => "awaiting_approval",
            State::Merging => "merging",
            State::Deploying => "deploying",
            State::Landed => "landed",
            State::Refused => "refused",
            State::TestFailed => "test_failed",
            State::Expired => "expired",
            State::MergeRefused => "merge_refused",
            State::DeployFailed => "deploy_failed",
            State::Cancelled => "cancelled",
            State::Error => "error",
        }
    }

    pub fn named(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|x| x.name() == s)
    }

    /// Whether it never changes again.
    pub fn terminal(self) -> bool {
        !matches!(
            self,
            State::Checking
                | State::Testing
                | State::AwaitingApproval
                | State::Merging
                | State::Deploying
        )
    }

    /// Whether `main` may still be untouched by it: it can be cancelled.
    pub fn cancellable(self) -> bool {
        matches!(
            self,
            State::Checking | State::Testing | State::AwaitingApproval
        )
    }
}

/// What the gateway saw of a landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Its checks passed.
    Checked,
    /// A check refused it.
    CheckRefused,
    /// Its CI job (test or deploy) ended: passed or not.
    JobEnded { passed: bool },
    /// A person approved it.
    Approved,
    /// Its approval was not given in time.
    ApprovalLapsed,
    /// `main` moved to its commit, or was there already.
    Merged,
    /// `main` could not be moved.
    MergeRefused,
    /// It was asked to stop.
    Cancel,
    /// The gateway itself failed.
    Failed,
}

/// The state a landing in `state` moves to on `seen`, `needs_approval` telling whether it waits
/// for a person after its tests; `None` when `seen` changes nothing.
pub fn next(state: State, needs_approval: bool, seen: &Seen) -> Option<State> {
    use State::*;
    let to = match (state, seen) {
        (s, Seen::Failed) if !s.terminal() => Error,
        (s, Seen::Cancel) if s.cancellable() => Cancelled,
        (Checking, Seen::Checked) => Testing,
        (Checking, Seen::CheckRefused) => Refused,
        (Testing, Seen::JobEnded { passed: true }) if needs_approval => AwaitingApproval,
        (Testing, Seen::JobEnded { passed: true }) => Merging,
        (Testing, Seen::JobEnded { passed: false }) => TestFailed,
        (AwaitingApproval, Seen::Approved) => Merging,
        (AwaitingApproval, Seen::ApprovalLapsed) => Expired,
        (Merging, Seen::Merged) => Deploying,
        (Merging, Seen::MergeRefused) => MergeRefused,
        (Deploying, Seen::JobEnded { passed: true }) => Landed,
        (Deploying, Seen::JobEnded { passed: false }) => DeployFailed,
        _ => return None,
    };
    Some(to)
}

/// Whether a landing may move from `from` to `to` at all: what [`next`] can answer.
pub fn allowed(from: State, to: State) -> bool {
    let seen = [
        Seen::Checked,
        Seen::CheckRefused,
        Seen::JobEnded { passed: true },
        Seen::JobEnded { passed: false },
        Seen::Approved,
        Seen::ApprovalLapsed,
        Seen::Merged,
        Seen::MergeRefused,
        Seen::Cancel,
        Seen::Failed,
    ];
    seen.iter()
        .any(|s| [true, false].iter().any(|a| next(from, *a, s) == Some(to)))
}

/// The prefix of the branches that may land.
pub const PREFIX: &str = "refs/heads/claude/";

/// `given` (`claude/x` or `refs/heads/claude/x`) as the full ref of a branch that may land.
pub fn branch_ref(given: &str) -> Result<String, String> {
    let given = given.trim();
    let full = if given.starts_with("refs/") {
        given.to_string()
    } else {
        format!("refs/heads/{given}")
    };
    let name = full
        .strip_prefix(PREFIX)
        .ok_or_else(|| format!("only branches under {PREFIX} land; not `{given}`"))?;
    let ok = !name.is_empty()
        && full.len() <= 255
        && name.split('/').all(|seg| {
            !seg.is_empty()
                && !seg.starts_with('.')
                && !seg.ends_with(".lock")
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        && !name.contains("..");
    if ok {
        Ok(full)
    } else {
        Err(format!("`{given}` is not a branch name a landing takes"))
    }
}

/// The paths whose content decides what the gate itself runs: the CI jobs and their scripts,
/// how cargo builds and tests (its config and toolchain), and the deploy tool's version and
/// where npm gets it. A landing that changes any of them needs a person's approval,
/// whatever the setting: tests catch mistakes, and a commit defines its own tests.
pub const GUARDED: [&str; 8] = [
    ".pigeon",
    "ci",
    ".cargo",
    "rust-toolchain.toml",
    ".npmrc",
    "src/platform/.npmrc",
    "src/platform/package.json",
    "src/platform/package-lock.json",
];

/// The guarded paths whose ID differs between `main` and the commit (`None` where nothing is).
pub fn guarded_changes(
    on_main: &[Option<String>],
    at_commit: &[Option<String>],
) -> Vec<&'static str> {
    GUARDED
        .iter()
        .zip(on_main.iter().zip(at_commit))
        .filter(|(_, (a, b))| a != b)
        .map(|(p, _)| *p)
        .collect()
}

/// Why a landing needs approval, or `None`: the setting, or a change to a guarded path.
pub fn approval_reason(setting: bool, guarded: &[&str]) -> Option<String> {
    match (setting, guarded) {
        (_, [_, ..]) => Some(format!(
            "it changes what the gate runs ({}), which a person always approves",
            guarded.join(", ")
        )),
        (true, []) => Some("the setting asks a person to approve every landing".into()),
        (false, []) => None,
    }
}

/// How long a landing waits for approval.
pub const APPROVAL_MS: i64 = 24 * 3_600_000;
/// How often a live landing is looked at.
pub const POLL_MS: i64 = 20_000;

/// The note a finished landing leaves on its commit, one line.
#[allow(clippy::too_many_arguments)]
pub fn note(
    id: &str,
    state: State,
    by: &str,
    test_job: Option<&str>,
    deploy_job: Option<&str>,
    approved_by: Option<&str>,
    at_iso: &str,
) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_whitespace() || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect()
    };
    let or_dash = |s: Option<&str>| s.map_or_else(|| "-".to_string(), clean);
    format!(
        "landing {} state={} by={} test={} deploy={} approved_by={} at={}\n",
        clean(id),
        state.name(),
        clean(by),
        or_dash(test_job),
        or_dash(deploy_job),
        or_dash(approved_by),
        clean(at_iso),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_happy_paths_with_and_without_approval() {
        let passed = Seen::JobEnded { passed: true };
        assert_eq!(
            next(State::Checking, false, &Seen::Checked),
            Some(State::Testing)
        );
        assert_eq!(next(State::Testing, false, &passed), Some(State::Merging));
        assert_eq!(
            next(State::Testing, true, &passed),
            Some(State::AwaitingApproval)
        );
        assert_eq!(
            next(State::AwaitingApproval, true, &Seen::Approved),
            Some(State::Merging)
        );
        assert_eq!(
            next(State::Merging, true, &Seen::Merged),
            Some(State::Deploying)
        );
        assert_eq!(next(State::Deploying, true, &passed), Some(State::Landed));
    }

    #[test]
    fn checking_comes_first_and_refuses_on_its_own() {
        assert_eq!(
            next(State::Checking, false, &Seen::Checked),
            Some(State::Testing)
        );
        assert_eq!(
            next(State::Checking, false, &Seen::CheckRefused),
            Some(State::Refused)
        );
        assert_eq!(
            next(State::Checking, false, &Seen::Cancel),
            Some(State::Cancelled)
        );
        assert_eq!(
            next(State::Checking, false, &Seen::Failed),
            Some(State::Error)
        );
        assert_eq!(
            next(State::Checking, false, &Seen::JobEnded { passed: true }),
            None
        );
        assert_eq!(next(State::Testing, false, &Seen::Checked), None);
        assert_eq!(next(State::Testing, false, &Seen::CheckRefused), None);
        assert!(!allowed(State::Checking, State::Merging));
        assert!(!allowed(State::Checking, State::Landed));
        assert!(!State::Checking.terminal());
        assert!(State::Refused.terminal());
    }

    #[test]
    fn each_step_fails_its_own_way() {
        let failed = Seen::JobEnded { passed: false };
        assert_eq!(
            next(State::Testing, false, &failed),
            Some(State::TestFailed)
        );
        assert_eq!(
            next(State::AwaitingApproval, true, &Seen::ApprovalLapsed),
            Some(State::Expired)
        );
        assert_eq!(
            next(State::Merging, false, &Seen::MergeRefused),
            Some(State::MergeRefused)
        );
        assert_eq!(
            next(State::Deploying, false, &failed),
            Some(State::DeployFailed)
        );
        for s in [
            State::Testing,
            State::AwaitingApproval,
            State::Merging,
            State::Deploying,
        ] {
            assert_eq!(next(s, false, &Seen::Failed), Some(State::Error));
        }
    }

    #[test]
    fn only_a_landing_that_has_not_touched_main_is_cancelled() {
        assert_eq!(
            next(State::Testing, false, &Seen::Cancel),
            Some(State::Cancelled)
        );
        assert_eq!(
            next(State::AwaitingApproval, true, &Seen::Cancel),
            Some(State::Cancelled)
        );
        assert_eq!(next(State::Merging, false, &Seen::Cancel), None);
        assert_eq!(next(State::Deploying, false, &Seen::Cancel), None);
    }

    #[test]
    fn nothing_moves_a_final_state_or_skips_a_step() {
        for s in State::ALL.into_iter().filter(|s| s.terminal()) {
            for to in State::ALL {
                assert!(!allowed(s, to), "{s:?} → {to:?}");
            }
        }
        // No way round the tests, the approval or the merge.
        assert!(!allowed(State::Testing, State::Deploying));
        assert!(!allowed(State::Testing, State::Landed));
        assert!(!allowed(State::AwaitingApproval, State::Deploying));
        assert!(!allowed(State::Merging, State::Landed));
        // An approval or a merge is no job result.
        assert_eq!(next(State::Testing, true, &Seen::Approved), None);
        assert_eq!(next(State::Testing, true, &Seen::Merged), None);
        assert_eq!(next(State::Deploying, true, &Seen::Merged), None);
        assert_eq!(
            next(State::Merging, true, &Seen::JobEnded { passed: true }),
            None
        );
        for s in State::ALL {
            assert_eq!(State::named(s.name()), Some(s));
        }
    }

    #[test]
    fn only_claude_branches_land() {
        assert_eq!(
            branch_ref("claude/friction"),
            Ok("refs/heads/claude/friction".into())
        );
        assert_eq!(
            branch_ref("refs/heads/claude/a/b-1.2_x"),
            Ok("refs/heads/claude/a/b-1.2_x".into())
        );
        for bad in [
            "main",
            "refs/heads/main",
            "claude",
            "claude/",
            "refs/heads/claude",
            "refs/tags/claude/x",
            "claudex/y",
            "claude/../main",
            "claude/a..b",
            "claude/.hidden",
            "claude/x.lock",
            "claude/a//b",
            "claude/a b",
            "claude/a~1",
        ] {
            assert!(branch_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_change_to_what_the_gate_runs_needs_a_person() {
        let mut main: Vec<Option<String>> = GUARDED.iter().map(|p| Some(p.to_string())).collect();
        main[7] = None;
        assert!(guarded_changes(&main, &main).is_empty());
        let mut commit = main.clone();
        commit[1] = Some("b2".into());
        commit[7] = Some("new".into());
        let changed = guarded_changes(&main, &commit);
        assert_eq!(changed, ["ci", "src/platform/package-lock.json"]);
        assert!(
            approval_reason(false, &changed)
                .unwrap()
                .contains("ci, src/platform")
        );
        assert!(approval_reason(true, &[]).is_some());
        assert_eq!(approval_reason(false, &[]), None);
    }

    #[test]
    fn the_note_is_one_line() {
        let n = note(
            "L1",
            State::Landed,
            "bot for ada",
            Some("j1"),
            None,
            None,
            "2026-11-21T10:00:00Z",
        );
        assert_eq!(
            n,
            "landing L1 state=landed by=bot_for_ada test=j1 deploy=- approved_by=- at=2026-11-21T10:00:00Z\n"
        );
        assert_eq!(
            note("x\ny", State::Error, "a", None, None, None, "t")
                .lines()
                .count(),
            1
        );
    }
}

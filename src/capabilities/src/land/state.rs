//! A landing's states and the moves between them, which branches may land, which changes the
//! policy keeps for the person's passkey or leaves to their delegate, and the note a landing
//! leaves. Pure.
//!
//! ```text
//! checking ─► testing ─► [awaiting_approval] ─► ready ─► merging ─► merged ─► deploying ─► landed
//!    │           │  ▲            │                │        │  │  │      │           │
//!    ▼           ▼  └── retest ──┼────────────────┼────────┘  │  │      ▼           ▼
//! refused   test_failed       expired          cancelled      │  │   landed      deploy_failed
//!                                                             │  ▼  (superseded)
//!                                              rebase_refused ◄┘ landed (docs only)
//! ```
//!
//! Many landings are live at once. Each is checked and tested on its own, outside any lock.
//! `ready` waits for the merge lock: one landing at a time is `merging`, which moves main by a
//! compare-and-swap, rebasing the branch onto main itself when main moved and the two changed
//! different paths. When main moved in paths the landing's tests cover, the rebased commit goes
//! back to `testing`. `merged` waits for the deploy lanes, one per Worker (`super::lanes`): it
//! is `deploying` once a deploy carrying it starts, and `landed` once every Worker it affects
//! deployed a commit at or after its merge (`landed` straight from `merged`, superseded, when
//! that happened without it ever being started); `deploy_failed` once a deploy carrying it
//! failed. A docs-only landing ends `landed` at the merge. Any live state can become `error`
//! (the gateway failed) or, before `merging`, `cancelled`.

use pigeon_core::policy::{Answer, Approval, Decision, Kind, Request};
use serde::{Deserialize, Serialize};

/// A landing's state.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Checking,
    Testing,
    AwaitingApproval,
    /// Tested (and approved where needed), waiting for the merge lock.
    Ready,
    Merging,
    /// In main, waiting for its Workers' deploy lanes.
    Merged,
    Deploying,
    Landed,
    Refused,
    TestFailed,
    Expired,
    MergeRefused,
    /// Main moved in a path the branch also changed, and the two could not be merged.
    RebaseRefused,
    DeployFailed,
    Cancelled,
    Error,
}

impl State {
    pub const ALL: [State; 16] = [
        State::Checking,
        State::Testing,
        State::AwaitingApproval,
        State::Ready,
        State::Merging,
        State::Merged,
        State::Deploying,
        State::Landed,
        State::Refused,
        State::TestFailed,
        State::Expired,
        State::MergeRefused,
        State::RebaseRefused,
        State::DeployFailed,
        State::Cancelled,
        State::Error,
    ];

    pub fn name(self) -> &'static str {
        match self {
            State::Checking => "checking",
            State::Testing => "testing",
            State::AwaitingApproval => "awaiting_approval",
            State::Ready => "ready",
            State::Merging => "merging",
            State::Merged => "merged",
            State::Deploying => "deploying",
            State::Landed => "landed",
            State::Refused => "refused",
            State::TestFailed => "test_failed",
            State::Expired => "expired",
            State::MergeRefused => "merge_refused",
            State::RebaseRefused => "rebase_refused",
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
                | State::Ready
                | State::Merging
                | State::Merged
                | State::Deploying
        )
    }

    /// Whether `main` may still be untouched by it: it can be cancelled.
    pub fn cancellable(self) -> bool {
        matches!(
            self,
            State::Checking | State::Testing | State::AwaitingApproval | State::Ready
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
    /// It took the lock or the lane it waited for: `ready` to `merging`, `merged` to
    /// `deploying`.
    Start,
    /// Main moved in paths its tests cover: its rebased commit is tested again.
    Retest,
    /// Main moved in a path it also changed, and the two could not be merged.
    RebaseRefused,
    /// A docs-only landing reached main: nothing to deploy.
    DocsMerged,
    /// A newer landing's deploy carried its commit.
    Superseded,
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
        (Testing, Seen::JobEnded { passed: true }) => Ready,
        (Testing, Seen::JobEnded { passed: false }) => TestFailed,
        (AwaitingApproval, Seen::Approved) => Ready,
        (AwaitingApproval, Seen::ApprovalLapsed) => Expired,
        (Ready, Seen::Start) => Merging,
        (Merging, Seen::Merged) => Merged,
        (Merging, Seen::DocsMerged) => Landed,
        (Merging, Seen::Retest) => Testing,
        (Merging, Seen::MergeRefused) => MergeRefused,
        (Merging, Seen::RebaseRefused) => RebaseRefused,
        (Merged, Seen::Start) => Deploying,
        (Merged, Seen::Superseded) => Landed,
        (Merged, Seen::JobEnded { passed: false }) => DeployFailed,
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
        Seen::Start,
        Seen::Retest,
        Seen::RebaseRefused,
        Seen::DocsMerged,
        Seen::Superseded,
        Seen::Cancel,
        Seen::Failed,
    ];
    seen.iter()
        .any(|s| [true, false].iter().any(|a| next(from, *a, s) == Some(to)))
}

/// The session prefix: what the policy's `@session/` stands for (`claude/`).
pub const PREFIX: &str = "refs/heads/claude/";

/// The prefixes of the branches that may land: the agents' namespaces
/// (`crate::source::push::NAMESPACES`), Claude's `claude/` and Codex's `codex/`. The policy
/// still decides each landing (`land-and-deploy`).
pub const PREFIXES: [&str; 2] = [PREFIX, "refs/heads/codex/"];

/// `given` (`claude/x`, `codex/x` or `refs/heads/claude/x`) as the full ref of a branch that
/// may land.
pub fn branch_ref(given: &str) -> Result<String, String> {
    let given = given.trim();
    let full = if given.starts_with("refs/") {
        given.to_string()
    } else {
        format!("refs/heads/{given}")
    };
    let name = PREFIXES
        .iter()
        .find_map(|p| full.strip_prefix(p))
        .ok_or_else(|| {
            format!(
                "only branches under {} land; not `{given}`",
                PREFIXES
                    .map(|p| p.trim_start_matches("refs/heads/"))
                    .join(" or ")
            )
        })?;
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

/// The paths whose content decides what the gate itself runs, and the policy over it: the
/// policy and the settings generated from it, the CI jobs and their scripts, how cargo builds
/// and tests (its config and toolchain), and the deploy tool's version and where npm gets it.
/// The policy says what a change to each needs ([`approvals`]): tests catch mistakes, and a
/// commit defines its own tests.
pub const GUARDED: [&str; 11] = [
    ".pigeon/policy.toml",
    ".claude/settings.json",
    ".pigeon/ci.toml",
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

/// The guarded paths among `changed`, the paths a landing changed: a guarded file itself, or
/// anything under a guarded directory.
pub fn guarded_in(changed: &[&str]) -> Vec<&'static str> {
    GUARDED
        .iter()
        .filter(|g| {
            changed.iter().any(|p| {
                p == *g
                    || p.strip_prefix(**g)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
        .copied()
        .collect()
}

/// The policy's request for a change to guarded path `path`: the policy and the settings
/// generated from it are `change_policy`; the rest `change_gate`, a directory as `dir/`.
pub fn guarded_request(path: &str) -> Request {
    let (kind, target) = match path {
        ".pigeon/policy.toml" | ".claude/settings.json" => (Kind::ChangePolicy, path.to_string()),
        ".pigeon" | "ci" | ".cargo" => (Kind::ChangeGate, format!("{path}/")),
        _ => (Kind::ChangeGate, path.to_string()),
    };
    crate::policy::request("lander", kind, &target)
}

/// What a landing's changes to guarded paths need, as the policy answers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Approvals {
    /// What waits for the person's passkey, each with why: a change whose rule asks for it,
    /// and one the policy denies or does not cover, which only the person can let through.
    pub passkey: Vec<String>,
    /// What the person's delegate approves, each with its rule.
    pub delegate: Vec<String>,
}

/// What `changed`, the guarded paths a landing changes, need, with `decide` answering each.
/// `.pigeon` itself counts only when neither file in it that the policy names changed: it then
/// holds another change.
pub fn approvals(changed: &[&str], mut decide: impl FnMut(&Request) -> Answer) -> Approvals {
    let named = changed
        .iter()
        .any(|p| matches!(*p, ".pigeon/policy.toml" | ".pigeon/ci.toml"));
    let mut out = Approvals::default();
    for path in changed {
        if *path == ".pigeon" && named {
            continue;
        }
        let req = guarded_request(path);
        let a = decide(&req);
        let rule = a.rule.as_deref().unwrap_or("no rule");
        match (a.decision, a.needs) {
            (Decision::Allow, _) => {}
            (Decision::Deny, Some(Approval::Delegate)) => {
                out.delegate.push(format!("{} ({rule})", req.target));
            }
            (Decision::Deny, _) => out
                .passkey
                .push(format!("{} ({rule}: {})", req.target, a.reason)),
        }
    }
    out
}

/// Why a landing waits for the person's passkey, or `None`: a change the policy keeps for the
/// person, or the setting.
pub fn approval_reason(setting: bool, passkey: &[String]) -> Option<String> {
    match (setting, passkey) {
        (_, [_, ..]) => Some(format!(
            "the policy keeps it for the person's passkey: {}",
            passkey.join("; ")
        )),
        (true, []) => Some("the setting asks a person to approve every landing".into()),
        (false, []) => None,
    }
}

/// Whether `path`, a file path, is under a guarded path ([`GUARDED`]): a guarded file itself, or
/// anything under a guarded directory.
pub fn is_guarded(path: &str) -> bool {
    !guarded_in(&[path]).is_empty()
}

/// One file of a landing's change to the guarded paths: its path, and its `(mode, blob id)`
/// before (on the commit it is laid onto) and after (at the landing's commit); `None` where no
/// file is.
pub type GatedChange = (String, Option<(String, String)>, Option<(String, String)>);

/// The digest an approval is bound to: SHA-256, hex, over `patch` (every guarded file the landing
/// changes, before and after), sorted by path, entries where before equals after dropped. Equal
/// digests mean the same change to the guarded paths, modes included.
pub fn gated_digest(patch: &[GatedChange]) -> String {
    use sha2::{Digest, Sha256};
    let mut rows: Vec<&GatedChange> = patch.iter().filter(|(_, a, b)| a != b).collect();
    rows.sort_by(|x, y| x.0.cmp(&y.0));
    let side = |c: &Option<(String, String)>| match c {
        Some((mode, id)) => format!("{mode} {id}"),
        None => "-".to_string(),
    };
    let mut h = Sha256::new();
    for (path, before, after) in rows {
        h.update(format!("{path}\0{}\0{}\n", side(before), side(after)).as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether an approval holds across the Lander's own rebase: one was given (`approved`), the
/// digest it was given for was recorded (`approved_digest`; none for an approval from before
/// digests), and the rebased commit's gated digest (`rebased`, `None` when it could not be
/// computed) is that one. Anything else voids it: an approval never moves to other content.
pub fn approval_holds(
    approved: bool,
    approved_digest: Option<&str>,
    rebased: Option<&str>,
) -> bool {
    approved && matches!((approved_digest, rebased), (Some(a), Some(r)) if a == r)
}

/// How many times a `test` job whose runner was lost is submitted again for the same commit.
pub const TEST_RETRIES: i64 = 1;

/// Whether a `test` job that ended in `job_state` is run once more at the same commit rather
/// than failing the landing: its runner was lost (`error`), and `retries` are left.
pub fn test_rerun(job_state: &str, retries: i64) -> bool {
    job_state == "error" && retries < TEST_RETRIES
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

//! The concurrency caps of the agents: how many runs may be live, how many executors one planner
//! may have, and how many slots planners leave to executors. Pure code: the index object reads
//! and stores the numbers, and decides nothing itself.

use serde_json::{Value, json};

use crate::Role;

/// Default for `max_live_runs`: live runs of every role together.
pub const DEFAULT_MAX_LIVE: u32 = 8;
/// Default for `max_executors_per_planner`.
pub const DEFAULT_PER_PLANNER: u32 = 3;
/// Default for `executor_reserve`: slots planners may not take.
pub const DEFAULT_RESERVE: u32 = 3;
/// The most `max_live_runs` may be set to.
pub const MAX_LIVE_LIMIT: u32 = 64;
/// The most `max_executors_per_planner` may be set to.
pub const PER_PLANNER_LIMIT: u32 = 16;

/// The names of the three settings, as the tool and the `flags` table spell them.
pub const KEYS: [&str; 3] = [
    "max_live_runs",
    "max_executors_per_planner",
    "executor_reserve",
];

/// The caps in force, already clamped (see [`StoredLimits::limits`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Live runs of every role together.
    pub max_live: u32,
    /// Live executors one planner may have.
    pub per_planner: u32,
    /// Live slots planners leave free for executors; always below `max_live`.
    pub reserve: u32,
}

impl Default for Limits {
    fn default() -> Self {
        StoredLimits::default().limits()
    }
}

/// The settings as stored: each is `None` when never set, which reads as its default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoredLimits {
    /// Stored `max_live_runs`.
    pub max_live: Option<u32>,
    /// Stored `max_executors_per_planner`.
    pub per_planner: Option<u32>,
    /// Stored `executor_reserve`.
    pub reserve: Option<u32>,
}

impl StoredLimits {
    /// The caps in force: defaults for what is not stored, then clamped so that at least one
    /// planner can always start (`reserve` below `max_live`, every cap at least 1).
    pub fn limits(&self) -> Limits {
        let max_live = self
            .max_live
            .unwrap_or(DEFAULT_MAX_LIVE)
            .clamp(1, MAX_LIVE_LIMIT);
        Limits {
            max_live,
            per_planner: self
                .per_planner
                .unwrap_or(DEFAULT_PER_PLANNER)
                .clamp(1, PER_PLANNER_LIMIT),
            reserve: self.reserve.unwrap_or(DEFAULT_RESERVE).min(max_live - 1),
        }
    }

    /// What `ai_agents_config` answers: the caps in force, and for each whether it is the
    /// default (`true`) or stored.
    pub fn answer(&self) -> Value {
        let l = self.limits();
        json!({
            "max_live_runs": l.max_live,
            "max_executors_per_planner": l.per_planner,
            "executor_reserve": l.reserve,
            "is_default": {
                "max_live_runs": self.max_live.is_none(),
                "max_executors_per_planner": self.per_planner.is_none(),
                "executor_reserve": self.reserve.is_none(),
            },
        })
    }
}

/// One whole number of an `ai_agents_config` call, within `lo..=hi`, or a clear refusal.
fn number(key: &str, v: &Value, lo: u32, hi: u32) -> Result<u32, String> {
    let n = v
        .as_u64()
        .ok_or_else(|| format!("`{key}` must be a whole number from {lo} to {hi}"))?;
    u32::try_from(n)
        .ok()
        .filter(|n| (lo..=hi).contains(n))
        .ok_or_else(|| format!("`{key}` must be from {lo} to {hi}, not {n}"))
}

/// Checks the arguments of an `ai_agents_config` call against what is stored and answers what
/// to store: the stored values with each one given replaced. No arguments change nothing.
/// `max_live_runs` is 1 to 64, `max_executors_per_planner` 1 to 16, `executor_reserve` 0 up to
/// one below `max_live_runs` (the new value, if given). Other keys are refused.
pub fn check_config(args: &Value, stored: &StoredLimits) -> Result<StoredLimits, String> {
    let mut next = *stored;
    let Some(map) = args.as_object() else {
        return if args.is_null() {
            Ok(next)
        } else {
            Err("give the settings as an object, or nothing to read them".into())
        };
    };
    if let Some(k) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!(
            "no setting `{k}`; the settings are {}",
            KEYS.join(", ")
        ));
    }
    if let Some(v) = map.get("max_live_runs") {
        next.max_live = Some(number("max_live_runs", v, 1, MAX_LIVE_LIMIT)?);
    }
    if let Some(v) = map.get("max_executors_per_planner") {
        next.per_planner = Some(number(
            "max_executors_per_planner",
            v,
            1,
            PER_PLANNER_LIMIT,
        )?);
    }
    if let Some(v) = map.get("executor_reserve") {
        let max_live = next.limits().max_live;
        let reserve = number("executor_reserve", v, 0, max_live - 1).map_err(|e| {
            format!(
                "{e} (it must stay below max_live_runs, now {max_live}, so a planner can start)"
            )
        })?;
        next.reserve = Some(reserve);
    }
    Ok(next)
}

/// The token gauge as the index takes it: `(live, soft_ceiling)` from a `tokens_status` answer
/// (or from the same two keys), `None` when either is missing. Pure.
pub fn gauge_of(status: &Value) -> Option<(u32, u32)> {
    let n = |k: &str| status[k].as_u64().and_then(|n| u32::try_from(n).ok());
    Some((
        n("account_total").or_else(|| n("live"))?,
        n("soft_ceiling")?,
    ))
}

/// Why a queued run may not take a live slot now, as the plain text shown in its `step`, or
/// `None` when it may start. This is the one place that composes the reason; a new reason is
/// added here. Arguments: the run's `role`; `live` runs of every role; `own_executors`, the live
/// executors of the run's planner (0 for a planner or a run with none); `paused`; `oldest`,
/// whether no older queued run of its kind that is allowed to start is ahead of it; and `tokens`,
/// the Cloudflare token gauge as `(live tokens, soft ceiling)`, or `None` when it is unknown
/// (an unknown gauge never holds a run).
pub fn queue_reason(
    role: Role,
    live: u32,
    own_executors: u32,
    limits: &Limits,
    paused: bool,
    oldest: bool,
    tokens: Option<(u32, u32)>,
) -> Option<String> {
    if paused {
        return Some("queued: runs are paused".into());
    }
    if role != Role::Planner && own_executors >= limits.per_planner {
        return Some(format!(
            "queued: this planner already has {own_executors} live executors (max_executors_per_planner)"
        ));
    }
    if live >= limits.max_live {
        return Some(format!(
            "queued: {live} of {} live runs (max_live_runs)",
            limits.max_live
        ));
    }
    if role == Role::Planner && live >= limits.max_live - limits.reserve {
        return Some(format!(
            "queued: planners leave {} slots for executors (executor_reserve)",
            limits.reserve
        ));
    }
    if let Some((live_tokens, ceiling)) = tokens
        && live_tokens >= ceiling
    {
        return Some(format!(
            "queued: {live_tokens} live Cloudflare tokens, at or above the soft ceiling of {ceiling} (waits until fewer than {ceiling})"
        ));
    }
    if !oldest {
        return Some("queued: older queued runs go first".into());
    }
    None
}

/// Whether a queued run of `role` may take a live slot while `live` runs of every role are live
/// and its planner has `own_executors` live executors; `oldest` is whether it is the oldest
/// queued run that its own planner's cap lets start (planners queue among planners, executors of
/// either kind among themselves). Every run needs a slot under `max_live`; a planner also leaves
/// `reserve` of them free; an executor needs room under its planner's `per_planner`.
pub fn may_start(role: Role, live: u32, own_executors: u32, limits: &Limits, oldest: bool) -> bool {
    queue_reason(role, live, own_executors, limits, false, oldest, None).is_none()
}

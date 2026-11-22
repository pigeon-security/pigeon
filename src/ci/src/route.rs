//! Where a job goes: the gateway's routing decision, pure. Each connector is matched against the
//! job's needs, dropped when it is not configured or unhealthy, and ranked: a pinned connector
//! alone, else the operator's order, ties to the shorter queue. A job waits for the first-ranked
//! connector while it is full, and past a wait limit goes to any ranked one with room. A job
//! nothing can run is refused at once, with every connector's reason.

use serde::{Deserialize, Serialize};

use super::class::{Class, Offered, pick};
use super::config::Needs;

/// A connector's state as last seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Availability {
    /// Its secrets are set.
    pub configured: bool,
    /// Its last check passed.
    pub healthy: bool,
    /// Free slots now; `None` when unknown or unlimited, which counts as room.
    pub free: Option<u32>,
    /// Jobs waiting at it.
    pub queued: u32,
}

/// A connector that might run the job.
pub struct Candidate<'a> {
    pub name: &'a str,
    /// Its place in the operator's order, best first; equal places tie.
    pub rank: u32,
    pub offers: &'a [Offered],
    pub availability: Availability,
}

/// What a job asks of routing.
pub struct Ask<'a> {
    pub needs: &'a Needs,
    pub timeout_min: u32,
    /// The connector the caller pinned, if any.
    pub pin: Option<&'a str>,
    /// Connectors that already failed this job with an infrastructure error.
    pub exclude: &'a [String],
}

/// A connector that can run the job, the class it would use, and whether it has room now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pick {
    pub connector: String,
    pub class: Class,
    pub ready: bool,
}

/// The connectors that can run the job, best first, and those that cannot, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Routed {
    pub ranked: Vec<Pick>,
    pub ruled_out: Vec<(String, String)>,
}

/// Match, availability and rank. `Err` is a refusal: every connector with why it cannot run
/// the job.
pub fn route(ask: &Ask, candidates: &[Candidate]) -> Result<Routed, Vec<(String, String)>> {
    if let Some(pin) = ask.pin
        && !candidates.iter().any(|c| c.name == pin)
    {
        return Err(vec![(pin.to_string(), "no such connector".to_string())]);
    }
    let mut ruled_out: Vec<(String, String)> = Vec::new();
    let mut kept: Vec<(Pick, u32, u32)> = Vec::new();
    for c in candidates {
        let name = c.name.to_string();
        if ask.exclude.iter().any(|e| e == c.name) {
            ruled_out.push((name, "already failed this job".to_string()));
            continue;
        }
        let offered = match pick(c.offers, ask.needs, ask.timeout_min) {
            Ok(o) => o,
            Err(reason) => {
                ruled_out.push((name, reason));
                continue;
            }
        };
        if ask.pin.is_some_and(|p| p != c.name) {
            ruled_out.push((name, "not the pinned connector".to_string()));
            continue;
        }
        let a = c.availability;
        if !a.configured {
            ruled_out.push((name, "not configured: its secrets are not set".to_string()));
            continue;
        }
        if !a.healthy {
            ruled_out.push((name, "unhealthy".to_string()));
            continue;
        }
        let ready = a.free != Some(0);
        kept.push((
            Pick {
                connector: name,
                class: offered.class,
                ready,
            },
            c.rank,
            a.queued,
        ));
    }
    if kept.is_empty() {
        return Err(ruled_out);
    }
    // The operator's order, ties to the shorter queue; the sort is stable. Cost and minute
    // limits per connector become later sort keys here.
    kept.sort_by_key(|(_, rank, queued)| (*rank, *queued));
    Ok(Routed {
        ranked: kept.into_iter().map(|(p, _, _)| p).collect(),
        ruled_out,
    })
}

/// Where a queued job goes now, if anywhere: the first-ranked connector when it has room;
/// once `fallback` (the wait limit passed), any connector with room.
pub fn choose(routed: &Routed, fallback: bool) -> Option<&Pick> {
    if fallback {
        routed.ranked.iter().find(|p| p.ready)
    } else {
        routed.ranked.first().filter(|p| p.ready)
    }
}

/// Whether a job queued at `queued_at_ms` has waited `wait_limit_ms` by `now_ms`.
pub fn fallback_due(queued_at_ms: i64, now_ms: i64, wait_limit_ms: i64) -> bool {
    now_ms.saturating_sub(queued_at_ms) >= wait_limit_ms
}

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
    if let Some(pin) = ask.pin {
        if !candidates.iter().any(|c| c.name == pin) {
            return Err(vec![(pin.to_string(), "no such connector".to_string())]);
        }
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

#[cfg(test)]
mod tests {
    use super::super::class::Size;
    use super::super::config::{Arch, Os};
    use super::*;

    fn offered() -> Offered {
        let class = Class {
            os: Os::Linux,
            arch: Arch::Amd64,
            size: Size::Small,
            docker: false,
            gpu: false,
        };
        Offered {
            class,
            cpu: 2,
            memory_gb: 8,
            disk_gb: 14,
            max_timeout_min: 360,
            network_none: false,
        }
    }

    fn ok() -> Availability {
        Availability {
            configured: true,
            healthy: true,
            free: None,
            queued: 0,
        }
    }

    fn full(queued: u32) -> Availability {
        Availability {
            free: Some(0),
            queued,
            ..ok()
        }
    }

    fn cand<'a>(name: &'a str, offers: &'a [Offered], availability: Availability) -> Candidate<'a> {
        Candidate {
            name,
            rank: u32::from(name.as_bytes()[0]),
            offers,
            availability,
        }
    }

    fn ask<'a>(needs: &'a Needs, pin: Option<&'a str>, exclude: &'a [String]) -> Ask<'a> {
        Ask {
            needs,
            timeout_min: 30,
            pin,
            exclude,
        }
    }

    fn names(r: &Routed) -> Vec<&str> {
        r.ranked.iter().map(|p| p.connector.as_str()).collect()
    }

    fn reason_of<'a>(list: &'a [(String, String)], name: &str) -> Option<&'a str> {
        list.iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r.as_str())
    }

    #[test]
    fn a_job_nothing_matches_is_refused_with_every_reason() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let a = Ask {
            timeout_min: 10_000,
            ..ask(&needs, None, &[])
        };
        let cs = [cand("a", &offers, ok()), cand("b", &offers, ok())];
        let err = route(&a, &cs).unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(reason_of(&err, "a").unwrap().contains("min"));
        assert!(reason_of(&err, "b").is_some());
        assert_eq!(route(&a, &[]).unwrap_err(), vec![]);
    }

    #[test]
    fn unconfigured_and_unhealthy_connectors_are_ruled_out() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let cs = [
            cand(
                "a",
                &offers,
                Availability {
                    configured: false,
                    ..ok()
                },
            ),
            cand(
                "b",
                &offers,
                Availability {
                    healthy: false,
                    ..ok()
                },
            ),
        ];
        let err = route(&ask(&needs, None, &[]), &cs).unwrap_err();
        assert_eq!(
            reason_of(&err, "a"),
            Some("not configured: its secrets are not set")
        );
        assert_eq!(reason_of(&err, "b"), Some("unhealthy"));
    }

    #[test]
    fn a_full_connector_stays_ranked_but_not_ready() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let r = route(&ask(&needs, None, &[]), &[cand("a", &offers, full(3))]).unwrap();
        assert_eq!(names(&r), vec!["a"]);
        assert!(!r.ranked[0].ready);
        assert_eq!(r.ranked[0].class, offered().class);
    }

    #[test]
    fn ready_connectors_keep_the_operators_order() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let cs = [
            cand("a", &offers, ok()),
            cand("b", &offers, ok()),
            cand("c", &offers, ok()),
        ];
        assert_eq!(
            names(&route(&ask(&needs, None, &[]), &cs).unwrap()),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn order_beats_room_and_ties_go_to_the_shorter_queue() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let cs = [cand("b", &offers, ok()), cand("a", &offers, full(0))];
        assert_eq!(
            names(&route(&ask(&needs, None, &[]), &cs).unwrap()),
            vec!["a", "b"]
        );
        let tied = |name, queued| Candidate {
            rank: 1,
            ..cand(name, &offers, Availability { queued, ..ok() })
        };
        let cs = [tied("a", 5), tied("b", 1), tied("c", 1)];
        assert_eq!(
            names(&route(&ask(&needs, None, &[]), &cs).unwrap()),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn a_pin_wins_and_a_bad_pin_is_refused() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let cs = [cand("a", &offers, ok()), cand("b", &offers, ok())];
        let r = route(&ask(&needs, Some("b"), &[]), &cs).unwrap();
        assert_eq!(names(&r), vec!["b"]);
        assert_eq!(
            reason_of(&r.ruled_out, "a"),
            Some("not the pinned connector")
        );
        assert_eq!(
            route(&ask(&needs, Some("zz"), &[]), &cs).unwrap_err(),
            vec![("zz".to_string(), "no such connector".to_string())]
        );
        let cs = [
            cand("a", &offers, ok()),
            cand(
                "b",
                &offers,
                Availability {
                    configured: false,
                    ..ok()
                },
            ),
        ];
        let err = route(&ask(&needs, Some("b"), &[]), &cs).unwrap_err();
        assert_eq!(
            reason_of(&err, "b"),
            Some("not configured: its secrets are not set")
        );
    }

    #[test]
    fn a_connector_that_failed_the_job_is_left_out() {
        let needs = Needs::default();
        let offers = vec![offered()];
        let cs = [cand("a", &offers, ok()), cand("b", &offers, ok())];
        let exclude = ["a".to_string()];
        let r = route(&ask(&needs, None, &exclude), &cs).unwrap();
        assert_eq!(names(&r), vec!["b"]);
        assert_eq!(
            reason_of(&r.ruled_out, "a"),
            Some("already failed this job")
        );
    }

    fn routed(ready: [bool; 2]) -> Routed {
        Routed {
            ranked: ["a", "b"]
                .iter()
                .zip(ready)
                .map(|(n, ready)| Pick {
                    connector: n.to_string(),
                    class: offered().class,
                    ready,
                })
                .collect(),
            ruled_out: vec![],
        }
    }

    #[test]
    fn a_queued_job_waits_for_the_first_until_the_fallback() {
        let r = routed([false, true]);
        assert!(choose(&r, false).is_none());
        assert_eq!(choose(&r, true).unwrap().connector, "b");
        let r = routed([true, true]);
        assert_eq!(choose(&r, false).unwrap().connector, "a");
        assert_eq!(choose(&r, true).unwrap().connector, "a");
        assert!(choose(&routed([false, false]), true).is_none());
    }

    #[test]
    fn the_fallback_is_due_at_the_wait_limit() {
        assert!(!fallback_due(0, 999, 1000));
        assert!(fallback_due(0, 1000, 1000));
        assert!(fallback_due(0, 5000, 1000));
    }
}

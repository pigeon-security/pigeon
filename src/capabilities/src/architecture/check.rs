//! What the drawing depends on that the model may lack: findings an agent or a test can act on
//! before the drawing shows something missing or misplaced.

use serde::Serialize;

use super::Model;
use super::scan::ROUTE_KINDS;

/// One thing the drawing cannot place or label as declared.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// `external_place`, `external_by`, `external_through`, `worker_unreached`, `tool_host`,
    /// `route_kind` or `route_unknown_kind`.
    pub rule: &'static str,
    /// What it is about: a service, Worker, tool call or route.
    pub subject: String,
    /// What is wrong, in a sentence.
    pub message: String,
}

fn finding(rule: &'static str, subject: impl Into<String>, message: String) -> Finding {
    Finding {
        rule,
        subject: subject.into(),
        message,
    }
}

/// Whether route `prefix` (a Worker's route: `METHOD /path` or `* /prefix`) is covered by a
/// declared route.
fn covered(m: &Model, worker: &str, route: &str) -> bool {
    let Some((method, path)) = route.split_once(' ') else {
        return true;
    };
    if !path.starts_with('/') {
        return true;
    }
    m.route_kinds.iter().any(|r| {
        let same_worker = r.worker == worker || method == "*";
        same_worker
            && if method == "*" {
                r.path.starts_with(path) || path.starts_with(&r.path)
            } else {
                r.path == path && (r.method == method || r.method == "*")
            }
    })
}

/// The findings about `m`, in a stable order.
pub fn check(m: &Model) -> Vec<Finding> {
    let mut out = Vec::new();
    let names: Vec<&str> = m.externals.iter().map(|e| e.name.as_str()).collect();
    for e in &m.externals {
        if !matches!(e.place.as_str(), "account" | "outside" | "container") {
            out.push(finding(
                "external_place",
                &e.name,
                format!(
                    "{} declares no place (account, outside or container); it is drawn outside the account.",
                    e.name
                ),
            ));
        }
        if e.by.is_empty() {
            out.push(finding(
                "external_by",
                &e.name,
                format!("{} names no capability that calls it.", e.name),
            ));
        } else if !m.workers.iter().any(|w| w.uses.contains(&e.by)) {
            out.push(finding(
                "external_by",
                &e.name,
                format!(
                    "{} is called by {}, which no Worker runs; it hangs from the hub.",
                    e.name, e.by
                ),
            ));
        }
        if !e.through.is_empty() && !names.contains(&e.through.as_str()) {
            out.push(finding(
                "external_through",
                &e.name,
                format!(
                    "{} is reached through {}, which is not a known service.",
                    e.name, e.through
                ),
            ));
        }
    }
    let hosted: Vec<&str> = m.hostnames.iter().map(|h| h.worker.as_str()).collect();
    if m.workers.len() > 1 {
        for w in &m.workers {
            let bound = m.links.iter().any(|l| l.from == w.name || l.to == w.name);
            if !bound && !hosted.contains(&w.name.as_str()) {
                out.push(finding(
                    "worker_unreached",
                    &w.name,
                    format!(
                        "{} serves no hostname and no Worker binds it or is bound by it.",
                        w.name
                    ),
                ));
            }
        }
    }
    for c in &m.tool_calls {
        let host = m.workers.iter().find(|w| w.name == c.to);
        for t in &c.tools {
            let hosts = host.is_some_and(|w| w.tools.contains(t));
            if !hosts {
                let message = match host {
                    Some(w) => format!("{} calls {t}, but {} hosts no such tool.", c.from, w.name),
                    None => format!("{} calls {t}, which no Worker hosts.", c.from),
                };
                out.push(finding("tool_host", format!("{} → {t}", c.from), message));
            }
        }
    }
    for r in &m.route_kinds {
        if !ROUTE_KINDS.contains(&r.kind.as_str()) {
            out.push(finding(
                "route_unknown_kind",
                format!("{} {}", r.method, r.path),
                format!(
                    "{} {} at {} declares kind {}, not one of {}.",
                    r.method,
                    r.path,
                    r.at,
                    r.kind,
                    ROUTE_KINDS.join(", ")
                ),
            ));
        }
    }
    // A Worker's routes each need a declared kind, when the model has route declarations at
    // all (an instance's own model lists Cloudflare route patterns instead).
    if !m.route_kinds.is_empty() {
        for w in &m.workers {
            for route in &w.routes {
                if !covered(m, &w.name, route) {
                    out.push(finding(
                        "route_kind",
                        format!("{} {route}", w.name),
                        format!(
                            "{route} on {} declares no kind; add `// route: <kind>` beside it.",
                            w.name
                        ),
                    ));
                }
            }
        }
    }
    out
}

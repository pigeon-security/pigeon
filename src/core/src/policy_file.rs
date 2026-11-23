//! The person's policy file, `.pigeon/policy.toml`, as built into this binary and read by
//! [`crate::policy`]. In pigeon-core so that core and the proxy Worker (`pigeon-proxy`) decide
//! every request by the same rules.

use std::sync::OnceLock;

use serde_json::json;

use crate::policy::{self, Answer, Kind, Policy, Request, Vars};

/// The policy's text, as built into this binary.
pub const TEXT: &str = include_str!("../../../.pigeon/policy.toml");

/// The policy built into this binary, parsed once; an error when the file does not parse,
/// which refuses everything it decides.
pub fn current() -> Result<&'static Policy, String> {
    static PARSED: OnceLock<Result<Policy, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| policy::parse(TEXT))
        .as_ref()
        .map_err(Clone::clone)
}

/// `req` decided by the policy built into this binary; when it does not parse, a deny naming
/// why.
pub fn decide(vars: &Vars, req: &Request) -> Answer {
    match current() {
        Ok(p) => p.evaluate(vars, req),
        Err(e) => Answer {
            decision: policy::Decision::Deny,
            rule: None,
            reason: format!("the policy does not parse: {e}"),
            needs: None,
        },
    }
}

/// A request with no approval.
pub fn request(actor: &str, action: Kind, target: &str) -> Request {
    Request {
        actor: actor.into(),
        action,
        target: target.into(),
        context: policy::Context::default(),
    }
}

/// `req` and its answer as one JSON audit line. Worker logs are kept (docs/plans/observability.md),
/// so it leaves out the actor, a person's or credential's name; the console's decision list keeps it.
pub fn audit_line(by: &str, req: &Request, a: &Answer) -> String {
    audit_line_for(by, req, a, None)
}

/// Like [`audit_line`], with a `"run"` key naming the AI agent run when `run` is `Some`.
pub fn audit_line_for(by: &str, req: &Request, a: &Answer, run: Option<&str>) -> String {
    let mut line = json!({
        "audit": "policy", "by": by, "action": req.action.name(),
        "target": req.target, "approval": req.context.approval.map(|x| x.name()),
        "decision": a.decision, "rule": a.rule, "reason": a.reason,
    });
    if let (Some(run), Some(o)) = (run, line.as_object_mut()) {
        o.insert("run".into(), json!(run));
    }
    line.to_string()
}

/// The run id as an audit label: kept when it is 1 to 32 ASCII letters and digits, else dropped.
/// A label for lines only; nothing is decided by it.
pub fn run_label(run: Option<&str>) -> Option<&str> {
    run.filter(|r| (1..=32).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric()))
}

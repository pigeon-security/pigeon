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

/// `req` and its answer as one JSON audit line.
pub fn audit_line(by: &str, req: &Request, a: &Answer) -> String {
    json!({
        "audit": "policy", "by": by, "actor": req.actor, "action": req.action.name(),
        "target": req.target, "approval": req.context.approval.map(|x| x.name()),
        "decision": a.decision, "rule": a.rule, "reason": a.reason,
    })
    .to_string()
}

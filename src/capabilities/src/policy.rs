//! The policy, as the platform uses it: `.pigeon/policy.toml`, built into the Worker, read by
//! [`pigeon_core::policy`]. `policy_check` answers one request as the person's delegate: the
//! service account acting for the standard user. The Lander takes from it which landings wait
//! for a passkey, and the git proxy which ref updates it lets through. Every decision is one
//! audit line in the Worker's log; `policy_check`'s and the Lander's are also kept, the latest
//! readable with [`recent`]. The console no longer lists them.

use std::sync::OnceLock;

use pigeon_core::mcp::Tool;
use pigeon_core::policy::{self, Answer, Kind, Policy as Rules, Request, Vars};
use serde_json::{Value, json};
use worker::{Env, Error, Result, console_log};

use crate::users::{Caller, directory};
use crate::{Call, Capability, Context, unknown};

/// The policy's text, as built into this Worker.
pub const TEXT: &str = include_str!("../../../.pigeon/policy.toml");

/// How many decisions are kept.
pub const LISTED: usize = 10;

/// The policy built into this Worker, parsed once; an error when the file does not parse,
/// which refuses everything it decides.
pub fn current() -> std::result::Result<&'static Rules, String> {
    static PARSED: OnceLock<std::result::Result<Rules, String>> = OnceLock::new();
    PARSED
        .get_or_init(|| policy::parse(TEXT))
        .as_ref()
        .map_err(Clone::clone)
}

/// `req` decided by the policy built into this Worker; when it does not parse, a deny naming why.
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

/// Writes `req`'s audit line and keeps the decision.
pub async fn record(env: &Env, by: &str, req: &Request, a: &Answer) -> Result<()> {
    console_log!("{}", audit_line(by, req, a));
    directory(
        env,
        "policy.log",
        json!({
            "actor": req.actor, "action": req.action.name(), "target": req.target,
            "decision": a.decision, "rule": a.rule, "reason": a.reason,
        }),
    )
    .await
    .map(|_| ())
}

/// The latest decisions kept, newest first.
pub async fn recent(env: &Env) -> Result<Value> {
    directory(env, "policy.recent", json!({ "limit": LISTED })).await
}

/// Who `policy_check` answers as: the service account and the person it acts for.
pub fn delegate(caller: &Caller) -> String {
    match &caller.acting_for {
        Some(person) => format!(
            "{}, the delegate of {}: the service account acting for the standard user",
            caller.user.name, person.name
        ),
        None => format!(
            "{}, a service account acting for no one: its answers are the policy's, with no \
             person behind them",
            caller.user.name
        ),
    }
}

/// The `policy` capability: `policy_check`.
pub struct Policy {
    /// The branch prefix sessions push under, such as `claude/`.
    pub session_prefix: String,
    /// The protected branch, such as `main`.
    pub main: String,
}

impl Policy {
    fn vars(&self) -> Vars {
        Vars {
            session_prefix: self.session_prefix.clone(),
            main: self.main.clone(),
        }
    }

    async fn check(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| Error::RustError("no caller".into()))?;
        let mut asked = args.clone();
        if let Some(o) = asked.as_object_mut() {
            o.insert("actor".into(), json!(caller.user.name));
        }
        let req: Request = serde_json::from_value(asked).map_err(|e| {
            let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.name()).collect();
            Error::RustError(format!(
                "not a request the policy reads ({e}); action is one of {}",
                kinds.join(", ")
            ))
        })?;
        let answer = decide(&self.vars(), &req);
        record(cx.env, "policy_check", &req, &answer).await?;
        Ok(json!({
            "decision": answer.decision, "rule": answer.rule, "reason": answer.reason,
            "needs": answer.needs, "as": delegate(caller),
        }))
    }
}

impl Capability for Policy {
    fn name(&self) -> &'static str {
        "policy"
    }

    fn tools(&self) -> Vec<Tool> {
        let kinds: Vec<Value> = Kind::ALL.iter().map(|k| json!(k.name())).collect();
        vec![Tool {
            name: "policy_check",
            description: "Ask the person's policy (.pigeon/policy.toml) whether an action is \
                          allowed, before acting and after any refusal. Answers as the \
                          person's delegate, the service account acting for them: `allow` is \
                          the person's standing instruction to go ahead; `deny` names the rule \
                          and why, or `not in policy`. Never propose new rules.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": kinds },
                    "target": { "type": "string", "description": "A branch name without refs/heads/ (such as `claude/x` or `main`), or a repository path (such as `ci/runner/test.sh`)." },
                    "context": {
                        "type": "object",
                        "properties": { "approval": { "type": "string", "enum": ["passkey", "delegate"] } }
                    }
                },
                "required": ["action"]
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "policy_check" => self.check(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }
}

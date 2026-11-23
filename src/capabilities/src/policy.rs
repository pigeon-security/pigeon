//! The policy, as the platform uses it: `.pigeon/policy.toml`, built into the Worker, read by
//! [`pigeon_core::policy`]. `policy_check` answers one request as the person's delegate: the
//! service account acting for the standard user. The Lander takes from it which landings wait
//! for a passkey, and the git proxy which ref updates it lets through. Every decision is one
//! audit line in the Worker's log; `policy_check`'s and the Lander's are also kept, the latest
//! readable with [`recent`]. The console no longer lists them.

use pigeon_core::mcp::Tool;
use pigeon_core::policy::{Answer, Kind, Request, Vars};
use serde_json::{Value, json};
use worker::{Env, Error, Result, console_log};

use crate::users::{Caller, directory};
use crate::{Call, Capability, Context, unknown};

/// The policy built into this Worker: its text, parsed once, deciding, and audit lines; in
/// pigeon-core so the proxy Worker decides by the same rules.
pub use pigeon_core::policy_file::{
    TEXT, audit_line, audit_line_for, current, decide, request, run_label,
};

/// How many decisions are kept.
pub const LISTED: usize = 10;

/// Writes `req`'s audit line and keeps the decision. `run` is the AI agent run, a label for the
/// line only; the kept decision does not carry it.
pub async fn record(
    env: &Env,
    by: &str,
    req: &Request,
    a: &Answer,
    run: Option<&str>,
) -> Result<()> {
    console_log!("{}", audit_line_for(by, req, a, run_label(run)));
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
        record(cx.env, "policy_check", &req, &answer, caller.run.as_deref()).await?;
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

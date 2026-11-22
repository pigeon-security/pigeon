//! Passkey approval of single tool calls (`docs/plans/tool-approvals.md`). [`gate`] asks the
//! person's policy about every call `dispatch` runs, as `call_tool` with the tool's name as
//! target. A call whose rule needs the passkey answers at once with a pending approval; the
//! person approves that exact call on the console's Approvals tab; the caller calls again with
//! the approval's id, and the call runs once. Every step is a policy audit line. The records,
//! digests and state changes are [`pigeon_core::approval`]'s; the Directory keeps them.

use pigeon_core::approval::{self, Kind as ApprovalKind, Record, State};
use pigeon_core::mcp::Tool;
use pigeon_core::policy::{Answer, Approval, Decision, Kind, Request, Vars};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::console::sections::{Section, Tab};
use crate::users::{Caller, User, directory};
use crate::{Call, Capability, Context, arg, unknown};

/// The argument a gated call carries its approval's id in, taken out before the tool runs.
pub const ARG: &str = "approval";

/// Who writes the audit lines of this module.
const BY: &str = "dispatch";

/// Tool targets name no branch, so the policy's branch placeholders are never expanded for them.
fn vars() -> Vars {
    Vars {
        session_prefix: String::new(),
        main: String::new(),
    }
}

/// Whether the policy built into this Worker has any `call_tool` rule. Until it does, tool calls
/// are not asked about, and run as before. A policy that does not parse is taken to have one,
/// so that every call is asked, and refused.
pub fn asked() -> bool {
    crate::policy::current()
        .map(|p| p.rules.iter().any(|r| r.actions.contains(&Kind::CallTool)))
        .unwrap_or(true)
}

/// The policy's answer to `actor` calling `tool`, with no approval.
fn answer(actor: &str, tool: &str) -> (Request, Answer) {
    let req = crate::policy::request(actor, Kind::CallTool, tool);
    let a = crate::policy::decide(&vars(), &req);
    (req, a)
}

/// Whether the policy wants the person's passkey for each call of `tool`.
pub fn gated(tool: &str) -> bool {
    asked() && answer("", tool).1.needs == Some(Approval::Passkey)
}

/// What [`gate`] lets run.
pub enum Gate {
    /// The call runs as asked; the policy allows it with no approval.
    Run(Value),
    /// The call runs with the person's approval, used by this call.
    Approved(Value, Box<Record>),
}

/// `args` without [`ARG`], and the approval id it held.
fn split(args: &Value) -> (Value, Option<String>) {
    let mut rest = args.clone();
    let id = rest
        .as_object_mut()
        .and_then(|o| o.remove(ARG))
        .and_then(|v| v.as_str().map(str::to_string));
    (rest, id)
}

/// One audit line and kept decision about `tool`, by `actor`.
#[allow(clippy::too_many_arguments)]
async fn audit(
    env: &Env,
    by: &str,
    actor: &str,
    tool: &str,
    approval: Option<Approval>,
    decision: Decision,
    rule: Option<String>,
    reason: String,
) -> Result<()> {
    let mut req = crate::policy::request(actor, Kind::CallTool, tool);
    req.context.approval = approval;
    let a = Answer {
        decision,
        rule,
        reason,
        needs: None,
    };
    crate::policy::record(env, by, &req, &a).await
}

/// The rule a tool call's approval was asked under, kept in its summary.
fn rule_of(r: &Record) -> Option<String> {
    r.summary["rule"].as_str().map(str::to_string)
}

fn actor_of(r: &Record) -> String {
    r.summary["caller"].as_str().unwrap_or_default().to_string()
}

/// Records each approval the Directory found past its time.
async fn expired(env: &Env, answered: &Value) -> Result<()> {
    let list: Vec<Record> = serde_json::from_value(answered["expired"].clone()).unwrap_or_default();
    for r in list.iter().filter(|r| r.kind == ApprovalKind::ToolCall) {
        audit(
            env,
            BY,
            &actor_of(r),
            &r.subject,
            None,
            Decision::Deny,
            rule_of(r),
            format!("approval {} expired", r.id),
        )
        .await?;
    }
    Ok(())
}

/// `ms` since the epoch as `HH:MM UTC`.
fn clock(ms: i64) -> String {
    let mins = ms.div_euclid(60_000).rem_euclid(24 * 60);
    format!("{:02}:{:02} UTC", mins / 60, mins % 60)
}

/// Decides whether `tool` may run for `cx`'s caller with `args`. A call the policy allows runs;
/// one it refuses outright is refused; one that needs the passkey is refused with a new (or the
/// same pending) approval to ask the person for, or runs once with an approved one.
pub async fn gate(cx: &Context<'_>, tool: &str, args: &Value) -> Result<Gate> {
    if !asked() {
        return Ok(Gate::Run(args.clone()));
    }
    let (rest, id) = split(args);
    let actor = cx
        .caller
        .map_or_else(|| "unknown".to_string(), |c| c.user.name.clone());
    let (req, a) = answer(&actor, tool);
    match (a.decision, a.needs) {
        (Decision::Allow, _) => Ok(Gate::Run(rest)),
        (Decision::Deny, None) => {
            crate::policy::record(cx.env, BY, &req, &a).await?;
            Err(Error::RustError(format!(
                "the person's policy refuses `{tool}`: {} ({})",
                a.reason,
                a.rule.as_deref().unwrap_or("no rule")
            )))
        }
        (Decision::Deny, Some(_)) => {
            let caller = cx.caller.ok_or_else(|| {
                Error::RustError(format!("`{tool}` needs a known caller to be approved"))
            })?;
            let digest = approval::call_digest(tool, &rest, &caller.user.id);
            match id {
                None => ask(cx.env, caller, tool, &rest, digest, a).await,
                Some(id) => use_approved(cx.env, caller, tool, rest, digest, &id, a).await,
            }
        }
    }
}

/// Asks for the person's approval of this call, answering why it does not run yet.
async fn ask(
    env: &Env,
    caller: &Caller,
    tool: &str,
    args: &Value,
    digest: String,
    a: Answer,
) -> Result<Gate> {
    let summary = json!({
        "tool": tool,
        "args": args,
        "caller": caller.user.name,
        "acting_for": caller.acting_for.as_ref().map(|p| p.name.clone()),
        "rule": a.rule,
    });
    let made = directory(
        env,
        "approval.create",
        json!({
            "kind": ApprovalKind::ToolCall.name(),
            "subject": tool,
            "digest": digest,
            "summary": summary,
            "payload": Value::Null,
            "caller_id": caller.user.id,
            "approver_id": caller.acting_for.as_ref().map(|p| p.id.clone()),
        }),
    )
    .await?;
    expired(env, &made).await?;
    let r: Record = serde_json::from_value(made["record"].clone())?;
    if made["existing"] != true {
        audit(
            env,
            BY,
            &caller.user.name,
            tool,
            None,
            Decision::Deny,
            a.rule.clone(),
            format!("needs passkey: approval {} pending", r.id),
        )
        .await?;
    }
    Err(Error::RustError(format!(
        "`{tool}` needs the person's passkey for this call: approval `{id}` waits on the \
         console's Approvals tab until {until}. Once approved, call `{tool}` again with the \
         same arguments and \"{ARG}\": \"{id}\"; `approvals_status` says where it stands.",
        id = r.id,
        until = clock(r.expires_ms),
    )))
}

/// Uses approval `id` for this call, once.
async fn use_approved(
    env: &Env,
    caller: &Caller,
    tool: &str,
    args: Value,
    digest: String,
    id: &str,
    a: Answer,
) -> Result<Gate> {
    let taken = directory(
        env,
        "approval.take",
        json!({ "id": id, "digest": digest, "caller_id": caller.user.id }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        audit(
            env,
            BY,
            &caller.user.name,
            tool,
            None,
            Decision::Deny,
            a.rule,
            format!("approval {id} refused: {why}"),
        )
        .await?;
        return Err(Error::RustError(format!("`{tool}` does not run: {why}")));
    }
    let r: Record = serde_json::from_value(taken["record"].clone())?;
    Ok(Gate::Approved(args, Box::new(r)))
}

/// Records how an approved call went: never its result.
pub async fn ran(env: &Env, r: &Record, ok: bool) -> Result<()> {
    audit(
        env,
        BY,
        &actor_of(r),
        &r.subject,
        Some(Approval::Passkey),
        Decision::Allow,
        rule_of(r),
        format!(
            "approval {} used: ran, {}",
            r.id,
            if ok { "ok" } else { "failed" }
        ),
    )
    .await
}

/// Adds the [`ARG`] argument to each tool the policy gates, saying how approval works.
pub fn annotate(tools: &mut [Tool]) {
    if !asked() {
        return;
    }
    for t in tools.iter_mut().filter(|t| gated(t.name)) {
        if let Some(props) = t.input_schema["properties"].as_object_mut() {
            props.insert(
                ARG.into(),
                json!({
                    "type": "string",
                    "description": "Each call of this tool needs the person's passkey. The first \
                                    call answers an approval id and runs nothing; once the person \
                                    approves it on the console, call again with the same \
                                    arguments and this id. It runs once."
                }),
            );
        } else {
            t.input_schema["properties"] = json!({ ARG: { "type": "string" } });
        }
    }
}

/// The open approvals `person` may decide, recording any that expired.
pub async fn open_for(env: &Env, person: &User) -> Result<Vec<Record>> {
    let listed = directory(env, "approval.list", json!({})).await?;
    expired(env, &listed).await?;
    let open: Vec<Record> = serde_json::from_value(listed["open"].clone())?;
    Ok(open
        .into_iter()
        .filter(|r| r.approver_id.as_deref().is_none_or(|p| p == person.id))
        .collect())
}

/// How many approvals wait for a person, for the Approvals tab's count.
pub async fn waiting(env: &Env) -> Result<usize> {
    let listed = directory(env, "approval.list", json!({})).await?;
    expired(env, &listed).await?;
    let open: Vec<Record> = serde_json::from_value(listed["open"].clone())?;
    Ok(open.iter().filter(|r| r.state == State::Pending).count())
}

/// The passkey purpose for approving `id`, from its kept digest; an error when there is no
/// such approval.
pub async fn purpose(env: &Env, id: &str) -> Result<String> {
    let r: Option<Record> =
        serde_json::from_value(directory(env, "approval.get", json!({ "id": id })).await?)?;
    let r =
        r.ok_or_else(|| Error::RustError(approval::Refusal::NotFound(id.into()).to_string()))?;
    Ok(approval::purpose(&r.id, &r.digest))
}

/// `person`, who passed a passkey bound to approval `id` and its digest, approves or denies it:
/// the console's write.
pub async fn decide(env: &Env, id: &str, approve: bool, person: &User) -> Result<Value> {
    let r: Option<Record> =
        serde_json::from_value(directory(env, "approval.get", json!({ "id": id })).await?)?;
    let r =
        r.ok_or_else(|| Error::RustError(approval::Refusal::NotFound(id.into()).to_string()))?;
    let decided = directory(
        env,
        "approval.decide",
        json!({ "id": id, "approve": approve, "digest": r.digest, "person_id": person.id }),
    )
    .await?;
    if let Some(why) = decided["refused"].as_str() {
        return Err(Error::RustError(why.to_string()));
    }
    let r: Record = serde_json::from_value(decided["record"].clone())?;
    if r.kind == ApprovalKind::ToolCall {
        let (decision, verb) = if approve {
            (Decision::Allow, "approved")
        } else {
            (Decision::Deny, "denied")
        };
        audit(
            env,
            "console",
            &actor_of(&r),
            &r.subject,
            approve.then_some(Approval::Passkey),
            decision,
            rule_of(&r),
            format!("approval {id} {verb} by {}", person.name),
        )
        .await?;
    }
    if r.kind == ApprovalKind::InstanceDraft {
        return Ok(json!({ "message": match r.state {
            State::Used => format!("Applied: instance `{}` has the approved record.", r.subject),
            State::Expired => format!(
                "Not applied: {}. Ask for a new proposal.",
                r.note.as_deref().unwrap_or("the draft expired")
            ),
            _ => format!("Denied: instance `{}` is unchanged.", r.subject),
        } }));
    }
    if r.kind == ApprovalKind::LinkedPlan {
        return Ok(json!({ "message": if approve {
            format!("Approved: the plan for `{}` is applied once, by the agent that asked, if the account still gives the same plan.", r.subject)
        } else {
            format!("Denied: nothing changes in `{}`'s account.", r.subject)
        } }));
    }
    Ok(json!({ "message": if approve {
        format!("Approved: `{}` runs once when its caller calls it again, within 10 minutes.", r.subject)
    } else {
        format!("Denied: `{}` does not run.", r.subject)
    } }))
}

/// The `approvals` capability: `approvals_status`, and the calls on the Approvals tab.
pub struct Approvals;

impl Approvals {
    async fn status(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let id = arg(args, "id")?;
        let caller = cx
            .caller
            .ok_or_else(|| Error::RustError("no caller".into()))?;
        let r: Option<Record> =
            serde_json::from_value(directory(cx.env, "approval.get", json!({ "id": id })).await?)?;
        // Another caller's approval is answered as none, so ids say nothing to anyone else.
        let r = r
            .filter(|r| r.caller_id == caller.user.id)
            .ok_or_else(|| Error::RustError(approval::Refusal::NotFound(id.into()).to_string()))?;
        let now = js_now();
        let state = if approval::is_stale(&r, now) {
            State::Expired
        } else {
            r.state
        };
        Ok(json!({
            "id": r.id, "tool": r.subject, "state": state.name(),
            "expires_ms": r.expires_ms, "decided_by_person": r.decided_by.is_some(),
        }))
    }
}

fn js_now() -> i64 {
    worker::js_sys::Date::now() as i64
}

impl Capability for Approvals {
    fn name(&self) -> &'static str {
        "approvals"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "approvals_status",
            description: "Where one of your tool calls' passkey approvals stands: pending, \
                          approved (call the tool again with it), denied, expired or used.",
            input_schema: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "approvals_status" => self.status(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Approvals,
            id: "approvals",
            title: "Waiting for you",
            description: "Only what cannot go ahead without your passkey.",
            order: 1,
            cost: None,
        }]
    }

    /// The tool calls waiting for this person's passkey.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let open = open_for(cx.env, person).await?;
            let calls: Vec<Value> = open
                .iter()
                .filter(|r| r.state == State::Pending && r.kind == ApprovalKind::ToolCall)
                .map(|r| {
                    json!({
                        "id": r.id, "tool": r.subject, "args": r.summary["args"],
                        "caller": r.summary["caller"], "acting_for": r.summary["acting_for"],
                        "created_ms": r.created_ms, "expires_ms": r.expires_ms,
                        "digest": r.digest.chars().take(12).collect::<String>(),
                    })
                })
                .collect();
            let drafts: Vec<Value> = open
                .iter()
                .filter(|r| r.state == State::Pending && r.kind == ApprovalKind::InstanceDraft)
                .map(|r| {
                    json!({
                        "id": r.id, "instance": r.subject, "summary": r.summary,
                        "created_ms": r.created_ms, "expires_ms": r.expires_ms,
                        "digest": r.digest.chars().take(12).collect::<String>(),
                    })
                })
                .collect();
            let plans: Vec<Value> = open
                .iter()
                .filter(|r| r.state == State::Pending && r.kind == ApprovalKind::LinkedPlan)
                .map(|r| {
                    json!({
                        "id": r.id, "instance": r.subject, "summary": r.summary,
                        "created_ms": r.created_ms, "expires_ms": r.expires_ms,
                        "digest": r.digest.chars().take(12).collect::<String>(),
                    })
                })
                .collect();
            Ok(json!({ "calls": calls, "drafts": drafts, "plans": plans }))
        })
    }
}

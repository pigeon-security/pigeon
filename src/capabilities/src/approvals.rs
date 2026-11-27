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

/// Why the policy refuses `actor` calling `tool` outright, if it does: asked before a console
/// write's passkey, so a call that could never run is not approved.
pub fn refused(actor: &str, tool: &str) -> Option<String> {
    if !asked() {
        return None;
    }
    let (_, a) = answer(actor, tool);
    (a.decision == Decision::Deny && a.needs.is_none()).then(|| {
        format!(
            "the person's policy refuses `{tool}`: {} ({})",
            a.reason,
            a.rule.as_deref().unwrap_or("no rule")
        )
    })
}

/// For a console write of `tool` with `args`, made by `person` with a passkey just checked for
/// exactly this call: when the policy gates the call on the passkey, records an approval of it,
/// approved by `person` at once, and answers its id for the call to use once (as [`gate`]
/// takes it). `None` when the call needs no approval.
pub async fn approve_own(
    env: &Env,
    person: &User,
    tool: &str,
    args: &Value,
) -> Result<Option<String>> {
    if !asked() {
        return Ok(None);
    }
    let (_, a) = answer(&person.name, tool);
    if a.decision == Decision::Allow || a.needs.is_none() {
        return Ok(None);
    }
    let digest = approval::call_digest(tool, args, &person.id);
    let made = directory(
        env,
        "approval.create",
        json!({
            "kind": ApprovalKind::ToolCall.name(),
            "subject": tool,
            "digest": digest,
            "summary": {
                "tool": tool, "args": args, "caller": person.name, "acting_for": person.name,
                "rule": a.rule, "via": "console",
            },
            "payload": Value::Null,
            "caller_id": person.id,
            "approver_id": person.id,
        }),
    )
    .await?;
    expired(env, &made).await?;
    let r: Record = serde_json::from_value(made["record"].clone())?;
    decide(env, &r.id, true, person).await?;
    Ok(Some(r.id))
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
    run: Option<&str>,
) -> Result<()> {
    let mut req = crate::policy::request(actor, Kind::CallTool, tool);
    req.context.approval = approval;
    let a = Answer {
        decision,
        rule,
        reason,
        needs: None,
    };
    crate::policy::record(env, by, &req, &a, run).await
}

/// The rule a tool call's approval was asked under, kept in its summary.
fn rule_of(r: &Record) -> Option<String> {
    r.summary["rule"].as_str().map(str::to_string)
}

fn actor_of(r: &Record) -> String {
    r.summary["caller"].as_str().unwrap_or_default().to_string()
}

/// The AI agent run an approval was asked under, kept in its summary.
fn run_of(r: &Record) -> Option<String> {
    r.summary["run"].as_str().map(str::to_string)
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
            run_of(r).as_deref(),
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
            crate::policy::record(
                cx.env,
                BY,
                &req,
                &a,
                cx.caller.and_then(|c| c.run.as_deref()),
            )
            .await?;
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
            let scope = cx.instance.map(|s| s.instance().to_string());
            match id {
                None => ask(cx.env, caller, tool, &rest, scope, digest, a).await,
                Some(id) => use_approved(cx.env, caller, tool, rest, digest, &id, a).await,
            }
        }
    }
}

/// Asks for the person's approval of this call, answering why it does not run yet.
#[allow(clippy::too_many_arguments)]
async fn ask(
    env: &Env,
    caller: &Caller,
    tool: &str,
    args: &Value,
    scope: Option<String>,
    digest: String,
    a: Answer,
) -> Result<Gate> {
    let mut summary = json!({
        "tool": tool,
        "args": args,
        "caller": caller.user.name,
        "acting_for": caller.acting_for.as_ref().map(|p| p.name.clone()),
        "rule": a.rule,
    });
    if let (Some(run), Some(o)) = (
        crate::policy::run_label(caller.run.as_deref()),
        summary.as_object_mut(),
    ) {
        o.insert("run".into(), json!(run));
    }
    let made = directory(
        env,
        "approval.create",
        json!({
            "kind": ApprovalKind::ToolCall.name(),
            "subject": tool,
            "digest": digest,
            "summary": summary,
            // What runs once the person approves: these arguments, through this instance
            // endpoint when the call came through one. The digest covers the arguments.
            "payload": if ANSWERS_SECRET.contains(&tool) {
                Value::Null
            } else {
                json!({ "args": args, "scope": scope })
            },
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
            caller.run.as_deref(),
        )
        .await?;
        if crate::apple::push::NOTIFY_APPROVALS
            && let Some(person) = &caller.acting_for
        {
            let body = format!("`{tool}` from {} waits for your passkey.", caller.user.name);
            let sent = crate::apple::push::notify(
                env,
                &person.id,
                "Approval waiting",
                &body,
                "pigeon://approvals",
                &format!("approval:{}", r.id),
            )
            .await;
            if let Err(e) = sent {
                worker::console_log!("approvals: push for approval {} failed: {e}", r.id);
            }
        }
    }
    if ANSWERS_SECRET.contains(&tool) {
        return Err(Error::RustError(format!(
            "`{tool}` needs the person's passkey for this call: approval `{id}` waits on the \
             console's Approvals tab until {until}. Once approved, call `{tool}` again with the \
             same arguments and \"{ARG}\": \"{id}\"; `approvals_status` says where it stands.",
            id = r.id,
            until = clock(r.expires_ms),
        )));
    }
    Err(Error::RustError(format!(
        "`{tool}` needs the person's passkey for this call: approval `{id}` waits on the \
         console's Approvals tab until {until}. Once the person approves it, the platform runs \
         this exact call once, as you; do not call it again. `approvals_status {{\"id\": \
         \"{id}\"}}` answers its result, and any approval it asks for next.",
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
            caller.run.as_deref(),
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
        run_of(r).as_deref(),
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
                                    approves it on the console, the platform runs that exact \
                                    call once, as you (approvals_status answers its result). \
                                    Calling again with this id answers the recorded result and \
                                    never runs it twice."
                }),
            );
        } else {
            t.input_schema["properties"] = json!({ ARG: { "type": "string" } });
        }
    }
}

/// The open approvals `person` may decide, recording any that expired: theirs to approve, about
/// an instance they are an operator or owner of (docs/plans/membership.md).
pub async fn open_for(env: &Env, person: &User) -> Result<Vec<Record>> {
    let listed = directory(env, "approval.list", json!({})).await?;
    expired(env, &listed).await?;
    let open: Vec<Record> = serde_json::from_value(listed["open"].clone())?;
    let access = crate::membership::access(env, &person.id).await?;
    Ok(open
        .into_iter()
        .filter(|r| r.approver_id.as_deref().is_none_or(|p| p == person.id))
        .filter(|r| {
            let instance = crate::instances::manifest::approval_instance(r);
            access
                .check(instance, crate::membership::Role::Operator)
                .is_ok()
        })
        .collect())
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
    // What it stands for runs now, as its caller, in the Approver's alarm; the console's
    // request does not wait for it (docs/plans/approvals-auto-run.md).
    let runs = approve && matches!(follow_up(&r), Ok(Some(_)));
    let queued = if runs && approval::runnable(&r, js_now()).is_ok() {
        match crate::approver::enqueue(env, &r.id).await {
            Ok(()) => true,
            Err(e) => {
                worker::console_error!("approval {} not handed to the runner: {e}", r.id);
                false
            }
        }
    } else {
        false
    };
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
            run_of(&r).as_deref(),
        )
        .await?;
    }
    if r.kind == ApprovalKind::InstanceDraft {
        return Ok(json!({ "queued": queued, "message": match r.state {
            State::Used if queued => format!(
                "Applied: instance `{}` has the approved record. Its plan is being made now; \
                 if there is something to do, it waits here for your approval.",
                r.subject
            ),
            State::Used => format!("Applied: instance `{}` has the approved record.", r.subject),
            State::Expired => format!(
                "Not applied: {}. Ask for a new proposal.",
                r.note.as_deref().unwrap_or("the draft expired")
            ),
            _ => format!("Denied: instance `{}` is unchanged.", r.subject),
        } }));
    }
    if queued {
        return Ok(json!({ "queued": true, "message": format!(
            "Approved: {} runs now, once, as the caller that asked. Its result shows here.",
            follow_up(&r).ok().flatten().map_or_else(|| r.subject.clone(), |f| format!("`{}`", f.tool))
        ) }));
    }
    if r.kind.name() == "observability_monitors" {
        return Ok(json!({ "message": if approve {
            "Approved: the monitors plan is applied once by the agent that asked."
        } else {
            "Denied: no notification policy is created."
        } }));
    }
    if r.kind.name() == "ingest_run" {
        return Ok(json!({ "message": if approve {
            format!("Approved: run {} starts once, when the agent that asked calls data_ingest_start.", r.subject)
        } else {
            "Denied: nothing starts.".to_string()
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
    /// `approvals_list`: what waits for the person the caller is, or acts for.
    async fn list(&self, cx: &Context<'_>) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| Error::RustError("no caller".into()))?;
        let person = if caller.user.kind == "standard" {
            &caller.user
        } else {
            caller
                .acting_for
                .as_ref()
                .filter(|u| u.kind == "standard")
                .ok_or_else(|| {
                    Error::RustError(
                        "approvals_list is for a person or an app acting for one".into(),
                    )
                })?
        };
        let open = open_for(cx.env, person).await?;
        let mut out = listing(&open);
        out["recent"] = Value::Array(recent_for(cx.env, person, &open).await?);
        out["landings"] = Value::Array(with_plain(crate::land::awaiting(cx.env).await?));
        Ok(out)
    }

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
        let mut out = json!({
            "id": r.id, "tool": r.subject, "kind": r.kind.name(), "state": state.name(),
            "expires_ms": r.expires_ms, "decided_by_person": r.decided_by.is_some(),
            "runs_on_approval": matches!(follow_up(&r), Ok(Some(_))),
        });
        if let Some(o) = &r.outcome {
            out["outcome"] = outcome_view(o);
            out["next"] = json!(o.next);
        }
        if let Some(after) = &r.after {
            out["after"] = json!(after);
        }
        Ok(out)
    }
}

fn js_now() -> i64 {
    worker::js_sys::Date::now() as i64
}

impl Capability for Approvals {
    fn name(&self) -> &'static str {
        "approvals"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "approvals",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "approvals_list",
                description: "What waits for the person's passkey: tool calls, instance drafts, \
                          linked plans and landings; and `recent`, what ran on approval in the \
                          last day, with its result and the next approval it asked for. For a \
                          person, or an app acting for one.",
                input_schema: json!({ "type": "object", "properties": {} }),
            },
            Tool {
                name: "approvals_status",
                description: "Where one of your approvals stands: pending, approved, denied, \
                          expired or used; and, once the person approved it, what the platform \
                          ran: `outcome` (running, done with its result, or failed with why) \
                          and `next`, the approval that run asked for, if any.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "approvals_status" => self.status(cx, args).await,
                "approvals_list" => self.list(cx).await,
                other => Err(unknown(other)),
            }
        })
    }
}

/// The pending records of each kind as `{calls, drafts, plans, other}`, the fields the console's
/// Approvals section and the `approvals_list` tool both answer. Other states are left out.
pub fn listing(open: &[Record]) -> Value {
    let pending = |kind: ApprovalKind| {
        open.iter()
            .filter(move |r| r.state == State::Pending && r.kind == kind)
    };
    let short = |r: &Record| r.digest.chars().take(12).collect::<String>();
    let calls: Vec<Value> = pending(ApprovalKind::ToolCall)
        .map(|r| {
            json!({
                "id": r.id, "tool": r.subject, "args": r.summary["args"],
                "caller": r.summary["caller"], "acting_for": r.summary["acting_for"],
                "run": r.summary["run"],
                "created_ms": r.created_ms, "expires_ms": r.expires_ms,
                "digest": short(r),
                "plain": plain_call(&r.subject, &r.summary["args"], &r.summary["acting_for"]),
                "after": r.after,
            })
        })
        .collect();
    let item = |r: &Record, plain: String| {
        json!({
            "id": r.id, "instance": r.subject, "summary": r.summary,
            "created_ms": r.created_ms, "expires_ms": r.expires_ms,
            "digest": short(r), "plain": plain, "after": r.after,
        })
    };
    let drafts: Vec<Value> = pending(ApprovalKind::InstanceDraft)
        .map(|r| item(r, format!("Save the changes to instance {}", r.subject)))
        .collect();
    let plans: Vec<Value> = pending(ApprovalKind::LinkedPlan)
        .map(|r| {
            item(
                r,
                if r.summary["retire"] == true {
                    format!("Retire {}: remove what pigeon made for it", r.subject)
                } else if r.summary["rename"] == true {
                    format!(
                        "Rename the home instance {} to {}",
                        r.subject,
                        r.summary["to"].as_str().unwrap_or_default()
                    )
                } else {
                    format!("Apply the plan for {}", r.subject)
                },
            )
        })
        .collect();
    // Any other kind (such as `observability_monitors`) is listed under `other`, so it can
    // be approved. The first three lists keep their fields.
    let other: Vec<Value> = open
        .iter()
        .filter(|r| {
            r.state == State::Pending
                && !matches!(
                    r.kind,
                    ApprovalKind::ToolCall | ApprovalKind::InstanceDraft | ApprovalKind::LinkedPlan
                )
        })
        .map(|r| {
            json!({
                "id": r.id, "kind": r.kind.name(), "title": r.subject, "summary": r.summary,
                "created_ms": r.created_ms, "expires_ms": r.expires_ms,
                "digest": short(r),
                "after": r.after,
                "plain": if r.subject.is_empty() {
                    format!("Approve {}", r.kind.name())
                } else {
                    r.subject.clone()
                },
            })
        })
        .collect();
    json!({ "calls": calls, "drafts": drafts, "plans": plans, "other": other })
}

/// One plain sentence saying what approving the tool call `tool` with `args` does, for the
/// caller named in `acting_for` (a name, or null).
pub fn plain_call(tool: &str, args: &Value, acting_for: &Value) -> String {
    let who = acting_for
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("the caller");
    let text = |k: &str| args[k].as_str().unwrap_or("?").to_string();
    match tool {
        "source_token" => format!("Give a source token to {who}"),
        "setup_platform" => "Set up the platform".to_string(),
        "users_remove" => format!("Remove user {}", text("id")),
        "users_admin" => match args["admin"].as_bool() {
            Some(true) => format!("Make user {} an admin", text("id")),
            _ => format!("Make user {} a member", text("id")),
        },
        "passkeys_remove" => format!("Remove passkey {}", text("id")),
        "instance_member_set" => match args["role"].as_str() {
            Some("none") => format!(
                "Remove {} from instance {}",
                text("person"),
                text("instance")
            ),
            _ => format!(
                "Make {} {} on instance {}",
                text("person"),
                text("role"),
                text("instance")
            ),
        },
        "oauth_revoke" => format!("Disconnect app {}", text("client_id")),
        _ => {
            let n = args.as_object().map_or(0, |o| o.len());
            format!("Run {tool} with {n} argument(s)")
        }
    }
}

/// One plain sentence for a landing awaiting approval: its branch and the first 7 characters of
/// its commit.
pub fn plain_landing(landing: &Value) -> String {
    let branch = landing["branch"].as_str().unwrap_or("?");
    let commit: String = landing["commit"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(7)
        .collect();
    if commit.is_empty() {
        format!("Land {branch}")
    } else {
        format!("Land {branch} at {commit}")
    }
}

/// `landings` with `plain` (and `created_ms`, from `created_at`) added to each item.
pub fn with_plain(landings: Vec<Value>) -> Vec<Value> {
    landings
        .into_iter()
        .map(|mut l| {
            l["plain"] = json!(plain_landing(&l));
            if l.get("created_ms").is_none() && l["created_at"].is_i64() {
                l["created_ms"] = l["created_at"].clone();
            }
            l
        })
        .collect()
}

// ---- What runs on approval (docs/plans/approvals-auto-run.md) ----

/// Gated tools whose answer is a live secret: their approval does not run on its own, since
/// the secret would wait at rest for its caller. The caller calls again with the id, as before.
pub const ANSWERS_SECRET: &[&str] = &["source_token", "session_credential", "oauth_issue_token"];

/// The tools a plan's approval runs, each taking the approval's id as `plan`.
pub const PLAN_TOOLS: &[&str] = &[
    "instance_apply",
    "instance_retire_apply",
    crate::instances::rename::TOOL,
    "apps_cleanup_apply",
    "portal_oauth_redirects_apply",
    "observability_monitors_apply",
    "data_ingest_start",
    "cloud_apply",
];

/// Tools renamed outright (docs/plans/record-reshape.md, docs/plans/data-ingest-names.md), old
/// name first. An approval of a call asked for under an old name runs nothing and says so: the
/// person signed for that name's digest, and the caller asks again under the new one.
pub const RENAMED: &[(&str, &str)] = &[
    ("ingest_plan", "data_ingest_plan"),
    ("ingest_start", "data_ingest_start"),
    ("ingest_status", "data_ingest_status"),
    ("ingest_report", "data_ingest_report"),
    ("ingest_stop", "data_ingest_stop"),
    ("email_status", "data_email_status"),
    ("email_list", "data_email_list"),
    ("email_get", "data_email_get"),
    ("email_mailboxes", "data_email_mailboxes"),
    ("email_address_set", "data_email_address_set"),
    ("email_forward_set", "data_email_forward_set"),
    ("storage_accounts", "data_read_accounts"),
    ("storage_list_buckets", "data_read_list_buckets"),
    ("storage_list_objects", "data_read_list_objects"),
    ("storage_get_object", "data_read_get_object"),
    ("storage_head_object", "data_read_head_object"),
];

/// The name `tool` was renamed to, when it is an old name. Pure.
pub fn renamed(tool: &str) -> Option<&'static str> {
    RENAMED
        .iter()
        .find(|(old, _)| *old == tool)
        .map(|(_, new)| *new)
}

/// The operation an approval stands for: a tool call, made as the approval's caller.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowUp {
    pub tool: String,
    pub args: Value,
    /// The instance endpoint the call came through, when it came through one.
    pub scope: Option<String>,
}

/// What runs once `r` is approved, if anything does. Pure. A tool call runs with the arguments
/// kept with it, only when they are exactly the ones whose digest the person signed for; a call
/// whose answer is a secret, a console write's own approval, and a record from before the
/// arguments were kept do not run on their own. A draft, once applied, has its instance
/// planned; a plan is applied by its tool; a kind not known here runs nothing.
pub fn follow_up(r: &Record) -> std::result::Result<Option<FollowUp>, String> {
    let plan = |tool: &str| {
        Ok(Some(FollowUp {
            tool: tool.to_string(),
            args: json!({ "plan": r.id }),
            scope: None,
        }))
    };
    match &r.kind {
        ApprovalKind::ToolCall => {
            if r.summary["via"] == "console" || ANSWERS_SECRET.contains(&r.subject.as_str()) {
                return Ok(None);
            }
            if let Some(to) = renamed(&r.subject) {
                return Err(format!(
                    "`{}` was renamed `{to}` after approval {} was asked for; nothing ran: call `{to}` for a new approval",
                    r.subject, r.id
                ));
            }
            let Some(args) = r.payload.get("args").filter(|a| a.is_object()) else {
                return Ok(None);
            };
            if approval::call_digest(&r.subject, args, &r.caller_id) != r.digest {
                return Err(format!(
                    "the arguments kept with approval {} are not the ones approved; nothing ran",
                    r.id
                ));
            }
            let mut run = args.clone();
            run[ARG] = json!(r.id);
            Ok(Some(FollowUp {
                tool: r.subject.clone(),
                args: run,
                scope: r.payload["scope"].as_str().map(str::to_string),
            }))
        }
        ApprovalKind::InstanceDraft => Ok(Some(FollowUp {
            tool: "instance_plan".into(),
            args: json!({ "instance": r.subject }),
            scope: None,
        })),
        ApprovalKind::LinkedPlan => {
            if r.payload["retire"] == true {
                plan("instance_retire_apply")
            } else if r.payload["rename"] == true {
                plan(crate::instances::rename::TOOL)
            } else {
                plan("instance_apply")
            }
        }
        ApprovalKind::Other(kind) => match kind.as_str() {
            crate::delegates::KIND => plan("apps_cleanup_apply"),
            crate::portal_oauth::KIND => plan("portal_oauth_redirects_apply"),
            "observability_monitors" => plan("observability_monitors_apply"),
            crate::ingest::KIND => plan("data_ingest_start"),
            "cloud_change" => plan("cloud_apply"),
            _ => Ok(None),
        },
    }
}

/// Whether the policy's answer `a` about a call of `tool` lets an approved run of it go on:
/// allowed, or gated on the passkey (the approval is that passkey). Refused outright, it does
/// not run, whatever was approved. Pure.
pub fn runs_under(a: &Answer, tool: &str) -> std::result::Result<(), String> {
    match (a.decision, a.needs) {
        (Decision::Allow, _) | (Decision::Deny, Some(Approval::Passkey)) => Ok(()),
        _ => Err(format!(
            "the person's policy now refuses `{tool}`: {} ({}); nothing ran",
            a.reason,
            a.rule.as_deref().unwrap_or("no rule")
        )),
    }
}

/// The policy asked again, at run time, about `actor` calling `tool`: the Approver's check
/// before an approved call runs (`dispatch`'s gate asks it once more).
pub fn recheck(actor: &str, tool: &str) -> std::result::Result<(), String> {
    if !asked() {
        return Ok(());
    }
    runs_under(&answer(actor, tool).1, tool)
}

/// The approval id a call of `tool` with `args` names, when the tool is one an approval runs.
fn recall_id<'a>(tool: &str, args: &'a Value) -> Option<&'a str> {
    if PLAN_TOOLS.contains(&tool) {
        args["plan"].as_str()
    } else if args.get(ARG).is_some() && gated(tool) {
        args[ARG].as_str()
    } else {
        None
    }
}

/// What a caller calling `tool` again with approval `r`, which the platform has claimed, is
/// answered: the recorded answer, or why there is none, so nothing runs twice. Pure. None lets
/// the call go on: not claimed, another caller's, another tool's, or a failed retire, which
/// calling again resumes.
pub fn recall_answer(
    r: &Record,
    tool: &str,
    caller_id: &str,
) -> Option<std::result::Result<Value, String>> {
    let o = r.outcome.as_ref()?;
    if o.tool != tool {
        return None;
    }
    if o.status == approval::RunStatus::Failed && tool == "instance_retire_apply" {
        return None;
    }
    approval::recall(r, caller_id)
}

/// For [`crate::dispatch`]: when `tool` is called with the id of an approval the platform
/// already ran (or runs), its recorded answer, so the old "call again" flow still works and
/// never runs anything twice. None for every other call.
pub async fn recalled(
    cx: &Context<'_>,
    tool: &str,
    args: &Value,
) -> Result<Option<std::result::Result<Value, String>>> {
    let (Some(id), Some(caller)) = (recall_id(tool, args), cx.caller) else {
        return Ok(None);
    };
    let got = directory(cx.env, "approval.get", json!({ "id": id })).await?;
    let Ok(Some(r)) = serde_json::from_value::<Option<Record>>(got) else {
        return Ok(None);
    };
    Ok(recall_answer(&r, tool, &caller.user.id))
}

/// One line saying what a run of `tool` that answered `answer` came to. Pure.
pub fn summarize(tool: &str, answer: &Value) -> String {
    if next_of(answer).is_some() {
        let steps = answer["plan"].as_array().map_or(0, Vec::len);
        let changes = answer["changes"].as_array().map_or(0, Vec::len);
        return match (steps, changes) {
            (0, 0) => format!("`{tool}` ran and asks for the next approval."),
            (0, n) => format!("`{tool}` ran and asks for the next approval: {n} change(s)."),
            (n, _) => {
                format!("`{tool}` ran and asks for the next approval: a plan of {n} step(s).")
            }
        };
    }
    let said = ["note", "message", "summary"]
        .iter()
        .find_map(|k| answer[*k].as_str())
        .map(str::to_string);
    match said {
        Some(s) => s,
        None if answer["problems"].is_array() => format!(
            "`{tool}` asked nothing: {}",
            answer["problems"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        ),
        None => format!("`{tool}` ran."),
    }
}

/// The approval a run's answer asks for next, if it names one. Pure.
pub fn next_of(answer: &Value) -> Option<String> {
    answer["approval"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// An outcome as `approvals_status` and the Approvals tab show it.
pub fn outcome_view(o: &approval::Outcome) -> Value {
    json!({
        "status": o.status.name(), "tool": o.tool, "summary": o.summary, "error": o.error,
        "next": o.next, "started_ms": o.started_ms, "finished_ms": o.finished_ms,
        "result": o.result,
    })
}

/// What ran on approval in the last day that `person` may see, as the Approvals tab lists it:
/// newest first, each with whether its next approval still waits (in `open`).
pub async fn recent_for(env: &Env, person: &User, open: &[Record]) -> Result<Vec<Value>> {
    let got = directory(env, "approval.recent", json!({})).await?;
    let ran: Vec<Record> = serde_json::from_value(got["ran"].clone()).unwrap_or_default();
    let access = crate::membership::access(env, &person.id).await?;
    Ok(ran
        .iter()
        .filter(|r| r.approver_id.as_deref().is_none_or(|p| p == person.id))
        .filter(|r| {
            let instance = crate::instances::manifest::approval_instance(r);
            access
                .check(instance, crate::membership::Role::Operator)
                .is_ok()
        })
        .filter_map(|r| recent_item(r, open))
        .collect())
}

/// One item of `recent`: the outcome without the answer, which stays with its caller's own
/// `approvals_status`; and whether the next approval still waits (in `open`). Pure.
pub fn recent_item(r: &Record, open: &[Record]) -> Option<Value> {
    let o = r.outcome.as_ref()?;
    let waiting = o
        .next
        .as_deref()
        .is_some_and(|n| open.iter().any(|p| p.id == n && p.state == State::Pending));
    let mut view = outcome_view(o);
    view["id"] = json!(r.id);
    view["kind"] = json!(r.kind.name());
    view["subject"] = json!(r.subject);
    view["instance"] = json!(crate::instances::manifest::approval_instance(r));
    view["next_waits"] = json!(waiting);
    view["decided_ms"] = json!(r.decided_ms);
    if let Some(m) = view.as_object_mut() {
        m.remove("result");
    }
    Some(view)
}

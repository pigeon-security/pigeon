//! The agents capability as its Worker serves it: the `agents_*` tools, behind the contract.
//! Core authenticates the caller and runs its gate before any of them; the caller who starts a
//! run (the person, or their delegate) is the caller the run acts as, with the run's id added,
//! so every model turn and pigeon call the run makes is theirs, through the same gate.

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, SectionRequest,
    SectionSpec, Service, Signed, ToolSpec,
};
use serde_json::{Value, json};
use worker::Env;

use crate::Role;
use crate::budget::Budget;
use crate::object::{self, INDEX};

/// The capability's name.
pub const NAME: &str = "ai-agents";
/// The Worker's script name.
pub const WORKER: &str = "pigeon-ai-agents";

/// Every tool the capability offers.
pub const TOOLS: [&str; 13] = [
    "ai_agents_start",
    "ai_agents_status",
    "ai_agents_stop",
    "ai_agents_pause",
    "ai_agents_resume",
    "ai_agents_inbox",
    "ai_agents_answer",
    "ai_agents_extend",
    "ai_agents_note",
    "ai_agents_register",
    "ai_agents_report",
    "ai_agents_handoff",
    "ai_agents_setup",
];

/// The console section's id; it sits on the Instances tab, with what else pigeon runs.
pub const SECTION: &str = "ai-agents";

/// The agents capability.
pub struct Agents;

/// The caller's user id, from the signed caller's JSON.
fn reader(caller: &Signed) -> String {
    // Runs are the person's: whoever stands for them (they, or a delegate acting for them)
    // sees and acts on them, whichever delegate a run acts as.
    object::person_of(&caller.caller)
        .unwrap_or_default()
        .to_string()
}

/// A run's id: 16 URL-safe characters.
pub fn run_id() -> worker::Result<String> {
    Ok(pigeon_core::webauthn::random()?
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect())
}

/// The arguments of `ai_agents_start`, checked: role, brief, base and budget. Pure.
pub fn start_args(args: &Value) -> Result<(Role, String, String, Budget), String> {
    let role = match args["role"].as_str().unwrap_or("executor") {
        "executor" => Role::Executor,
        "mechanical" => Role::Mechanical,
        "planner" => Role::Planner,
        other => {
            return Err(format!(
                "`role` must be `planner`, `executor` or `mechanical`, not `{other}`"
            ));
        }
    };
    let brief = args["brief"].as_str().map(str::trim).unwrap_or_default();
    if brief.is_empty() {
        return Err("`brief` is required: what the run is to build, standing alone".into());
    }
    if brief.len() > 64 * 1024 {
        return Err("`brief` is longer than 64 KiB; point at files instead of pasting them".into());
    }
    let base = args["base"].as_str().unwrap_or("main");
    let base_ok = !base.is_empty()
        && base.len() <= 200
        && !base.contains("..")
        && base
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    if !base_ok {
        return Err("`base` must be a branch or a commit".into());
    }
    let budget = Budget::asked(role, &args["budget"])?;
    Ok((role, brief.to_string(), base.to_string(), budget))
}

/// The branch run `run` pushes: one of the person's session branches.
pub fn branch(run: &str) -> String {
    format!("claude/ai-agent-{run}")
}

impl Agents {
    async fn start(&self, env: &Env, caller: &Signed, args: &Value) -> Result<Value, String> {
        if caller.caller["user"]["id"].as_str().is_none() {
            return Err("a run needs a caller to act as".into());
        }
        let (role, brief, base, budget) = start_args(args)?;
        let id = run_id().map_err(|e| e.to_string())?;
        // The run acts as its role's service user, linked to the person; as its starter until
        // that link is made. Marked with the run's id for the audit either way.
        let as_caller = object::run_caller(env, &caller.caller, role, &id).await;
        let paused = object::call(env, INDEX, "paused", &json!({}))
            .await
            .map_err(|e| e.to_string())?;
        if paused == true {
            return Err("every run is paused (ai_agents_pause); ai_agents_resume first".into());
        }
        object::call(
            env,
            INDEX,
            "create",
            &json!({
                "run": id,
                "role": role.name(),
                "runtime": "gateway",
                "caller": as_caller,
                "brief": brief,
                "base": base,
                "branch": branch(&id),
                "budget": budget,
            }),
        )
        .await
        .map_err(|e| e.to_string())
    }

    async fn call_tool(
        &self,
        env: &Env,
        caller: &Signed,
        req: &CallRequest,
    ) -> Result<Value, String> {
        let a = &req.args;
        let reader = reader(caller);
        let r = |v: worker::Result<Value>| v.map_err(|e| e.to_string());
        match req.tool.as_str() {
            "ai_agents_start" => self.start(env, caller, a).await,
            "ai_agents_status" => match a["run"].as_str() {
                Some(run) => {
                    r(
                        object::call(env, INDEX, "get", &json!({ "run": run, "reader": reader }))
                            .await,
                    )
                }
                None => r(object::call(
                    env,
                    INDEX,
                    "list",
                    &json!({ "reader": reader, "limit": a["limit"] }),
                )
                .await),
            },
            "ai_agents_stop" => {
                if a["run"].is_null() && a["all"] != true {
                    return Err("give `run`, or `all: true` to stop every live run of yours".into());
                }
                r(object::call(
                    env,
                    INDEX,
                    "stop",
                    &json!({ "run": a["run"], "all": a["all"], "reader": reader }),
                )
                .await)
            }
            "ai_agents_pause" | "ai_agents_resume" => {
                let on = req.tool == "ai_agents_pause";
                pause_args(&req.tool, a)?;
                r(object::call(env, INDEX, "pause", &json!({ "paused": on })).await)
                    .map(|_| pause_answer(on))
            }
            "ai_agents_inbox" => r(object::call(
                env,
                INDEX,
                "inbox",
                &json!({ "reader": reader, "since": a["since"].as_i64().unwrap_or(0) }),
            )
            .await),
            "ai_agents_answer" => {
                let (Some(run), Some(answer)) = (a["run"].as_str(), a["answer"].as_str()) else {
                    return Err("give `run` and `answer`".into());
                };
                r(object::call(
                    env,
                    INDEX,
                    "relay_answer",
                    &json!({ "run": run, "answer": answer, "reader": reader }),
                )
                .await)
            }
            "ai_agents_extend" => {
                let run = a["run"].as_str().ok_or("give `run`")?;
                r(object::call(
                    env,
                    INDEX,
                    "extend",
                    &json!({ "run": run, "add": extension(a), "reader": reader }),
                )
                .await)
            }
            "ai_agents_note" => {
                let run = a["run"].as_str().ok_or("give `run`")?;
                let text = note(a)?;
                r(object::call(
                    env,
                    INDEX,
                    "note",
                    &json!({ "run": run, "text": text, "reader": reader }),
                )
                .await)
            }
            "ai_agents_register" => {
                let session = a["session_id"].as_str().unwrap_or_default();
                if !session.starts_with("session_") {
                    return Err("`session_id` must be a Claude Code session's id".into());
                }
                let role = match a["role"].as_str().unwrap_or("executor") {
                    r @ ("planner" | "executor" | "mechanical") => r,
                    other => {
                        return Err(format!("`role` must be planner or executor, not `{other}`"));
                    }
                };
                let brief = a["brief"].as_str().unwrap_or_default();
                if brief.trim().is_empty() {
                    return Err("`brief` is required".into());
                }
                let id = run_id().map_err(|e| e.to_string())?;
                r(object::call(
                    env,
                    INDEX,
                    "create",
                    &json!({
                        "run": id,
                        "role": role,
                        "runtime": "claude_code",
                        "session_id": session,
                        "parent_run": a["parent_run"],
                        "caller": caller.caller,
                        "brief": brief,
                        "branch": a["branch"],
                        "budget": Value::Null,
                    }),
                )
                .await)
            }
            "ai_agents_setup" => {
                let mut out = serde_json::Map::new();
                for role in [Role::Planner, Role::Executor] {
                    let key = object::service_key(role);
                    let v = object::service_user(env, &caller.caller, key)
                        .await
                        .map_err(|e| e.to_string())?;
                    let linked = v["caller"]["acting_for"]["id"].as_str().is_some();
                    out.insert(
                        key.into(),
                        json!({
                            "name": v["caller"]["user"]["name"],
                            "linked": linked,
                            "approve": v["approve"],
                        }),
                    );
                }
                Ok(json!({
                    "service_users": out,
                    "note": "Runs act as these delegates once the person approves each link with \
                             their passkey; until then a run acts as the caller who started it.",
                }))
            }
            "ai_agents_handoff" => r(object::call(
                env,
                INDEX,
                "handoff",
                &json!({ "run": a["run"], "reader": reader }),
            )
            .await),
            "ai_agents_report" => r(object::call(
                env,
                INDEX,
                "report",
                &json!({
                    "run": a["run"],
                    "state": a["state"],
                    "report": a["report"],
                    "branch": a["branch"],
                    "step": a["step"],
                    "reader": reader,
                }),
            )
            .await),
            other => Err(format!("no tool `{other}`")),
        }
    }
}

impl Service for Agents {
    fn manifest(&self) -> Manifest {
        let tool = |name: &str, description: &str, input_schema: Value| ToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema,
        };
        Manifest {
            contract: CONTRACT,
            worker: WORKER.into(),
            commit: option_env!("PIGEON_COMMIT").unwrap_or_default().into(),
            capabilities: vec![CapabilityManifest {
                name: NAME.into(),
                tools: vec![
                    tool(
                        "ai_agents_start",
                        "Start an AI agent run inside pigeon (docs/plans/ai-agents.md), acting as you \
                         through pigeon's gate, with models through the model gateway. An \
                         executor builds `brief` in its own sandbox, pushes \
                         `claude/ai-agent-{run}` and runs CI; it never lands. A planner plans the \
                         work, spawns executors, reviews their branches and lands phase by \
                         phase. Answers the run at once; follow it with ai_agents_inbox and \
                         ai_agents_status. Budgets by default: executor $5, 60 turns, 90 min; \
                         planner $25, 200 turns, 8 h, 12 executors.",
                        json!({
                            "type": "object",
                            "properties": {
                                "role": { "type": "string", "enum": ["planner", "executor", "mechanical"], "description": "`executor` (Sonnet) by default; `mechanical` (Haiku) for moves, renames and docs; `planner` (Opus) to plan a piece of work, spawn executors, review their branches, run CI and land phase by phase." },
                                "brief": { "type": "string", "description": "What to build, standing alone: the run sees nothing else." },
                                "base": { "type": "string", "description": "The branch or commit to start from; main by default." },
                                "budget": { "type": "object", "description": "Any of turns, cost_usd, wall_min, sandbox_min; each at most its hard cap." }
                            },
                            "required": ["brief"]
                        }),
                    ),
                    tool(
                        "ai_agents_status",
                        "One run (with `run`): its state, current step, what it waits for, its \
                         spending against its budget, its branch and its report. Without \
                         `run`, your recent runs and whether runs are paused.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "limit": { "type": "integer", "description": "20 by default, 100 at most." }
                            }
                        }),
                    ),
                    tool(
                        "ai_agents_stop",
                        "Stop a run (and a planner's executors), or every live run of yours \
                         with `all: true`: its model job is cancelled and its sandbox \
                         destroyed. Its branch stays.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "all": { "type": "boolean" }
                            }
                        }),
                    ),
                    tool(
                        "ai_agents_pause",
                        "Pause EVERY run at once (it takes no run id): no run takes another step \
                         and none starts until ai_agents_resume. To stop one run, use \
                         ai_agents_stop with its `run`.",
                        json!({ "type": "object", "properties": {} }),
                    ),
                    tool(
                        "ai_agents_resume",
                        "Resume every run after ai_agents_pause.",
                        json!({ "type": "object", "properties": {} }),
                    ),
                    tool(
                        "ai_agents_inbox",
                        "What waits for the person (runs' questions and passkey waits) and \
                         the runs that ended since `since` (ms), with their reports. Read it \
                         when the person writes and after starting a run; relay questions \
                         and reports to them; pass `next_since` next time.",
                        json!({
                            "type": "object",
                            "properties": { "since": { "type": "integer" } }
                        }),
                    ),
                    tool(
                        "ai_agents_answer",
                        "Give a run the person's answer to the question it asked; it goes on.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "answer": { "type": "string" }
                            },
                            "required": ["run", "answer"]
                        }),
                    ),
                    tool(
                        "ai_agents_extend",
                        "Raise a live run's budget: each amount given is added to its limit, \
                         within the hard caps ($100, 500 turns, 24 h of wall time, 6 h of \
                         sandbox, 30 executors). Who extended it and by how much is kept in its \
                         status under `extended`. Answers the run's status.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "cost_usd": { "type": "number", "description": "Dollars to add." },
                                "turns": { "type": "integer" },
                                "wall_min": { "type": "integer" },
                                "sandbox_min": { "type": "integer" },
                                "executors": { "type": "integer", "description": "Planners only." }
                            },
                            "required": ["run"]
                        }),
                    ),
                    tool(
                        "ai_agents_note",
                        "Send a live run a short note from the person (at most 4000 bytes). It \
                         goes into the run's next turn as a message of its own, kept in its \
                         transcript; it does not answer a pending question (ai_agents_answer \
                         does).",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "text": { "type": "string" }
                            },
                            "required": ["run", "text"]
                        }),
                    ),
                    tool(
                        "ai_agents_register",
                        "Record a Claude Code session started for a task (when the person \
                         asked for the work in the Claude chat) as a run, so it is listed and \
                         reports like a gateway run. Answers the run's id; tell the session \
                         to call ai_agents_report with it at its end.",
                        json!({
                            "type": "object",
                            "properties": {
                                "session_id": { "type": "string" },
                                "role": { "type": "string", "enum": ["planner", "executor", "mechanical"] },
                                "brief": { "type": "string" },
                                "branch": { "type": "string" },
                                "parent_run": { "type": "string" }
                            },
                            "required": ["session_id", "brief"]
                        }),
                    ),
                    tool(
                        "ai_agents_report",
                        "From a Claude Code session registered with ai_agents_register: where \
                         it is (`running`, with a `step`) or how it ended (`done` or \
                         `failed`, with a `report`: what changed, the branch and its tip, \
                         CI, any landing).",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "state": { "type": "string", "enum": ["running", "done", "failed"] },
                                "report": { "type": "string" },
                                "branch": { "type": "string" },
                                "step": { "type": "string" }
                            },
                            "required": ["run", "state"]
                        }),
                    ),
                    tool(
                        "ai_agents_handoff",
                        "Move a gateway run to the Claude chat: it stops, and the answer is \
                         a packet (its brief, branch, what it did and spent, and a prompt) to \
                         start a Claude Code session with, then ai_agents_register it. For when \
                         the person asks, or the model gateway keeps failing.",
                        json!({
                            "type": "object",
                            "properties": { "run": { "type": "string" } },
                            "required": ["run"]
                        }),
                    ),
                    tool(
                        "ai_agents_setup",
                        "Make sure the AI agents' two service users exist (\"Pigeon planner\" \
                         and \"Pigeon executor\") and say whether each is linked to the \
                         person; for each that is not, a link to approve with their passkey. \
                         Runs act as these delegates once linked.",
                        json!({ "type": "object", "properties": {} }),
                    ),
                ],
                sections: vec![SectionSpec {
                    tab: "instances".into(),
                    id: SECTION.into(),
                    title: "AI Agents".into(),
                    description: "Planner and executor runs, through the model gateway or in \
                                  your Claude chat, and what they wait for from you."
                        .into(),
                    order: 50,
                    cost: None,
                }],
                // Its objects and container come from its wrangler.toml, which the architecture reads.
                footprint: Value::Null,
                ..CapabilityManifest::default()
            }],
        }
    }

    fn section<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: SectionRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move {
            if req.id != SECTION {
                return Answer::error(FailureKind::BadRequest, format!("no section `{}`", req.id));
            }
            let reader = reader(caller);
            match object::call(env, INDEX, "section", &json!({ "reader": reader })).await {
                Ok(data) => Answer::Ok(json!({ "html": crate::page::section(&data) })),
                Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
            }
        })
    }

    fn call<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: CallRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move {
            if !TOOLS.contains(&req.tool.as_str()) {
                return Answer::error(FailureKind::UnknownTool, format!("no tool `{}`", req.tool));
            }
            match self.call_tool(env, caller, &req).await {
                Ok(v) => Answer::Ok(v),
                Err(why) => Answer::error(FailureKind::Failed, why),
            }
        })
    }
}

/// Refuses arguments `ai_agents_pause` and `ai_agents_resume` do not take: both act on every run,
/// and a `run` given to them was once taken as a per-run pause.
pub fn pause_args(tool: &str, a: &Value) -> Result<(), String> {
    match a.as_object() {
        Some(o) if o.contains_key("run") => Err(format!(
            "{tool} acts on every run and takes no `run`; to stop one run, use ai_agents_stop with its `run`"
        )),
        _ => Ok(()),
    }
}

/// What `ai_agents_pause` (`paused`) and `ai_agents_resume` answer: that it is every run.
pub fn pause_answer(paused: bool) -> Value {
    json!({
        "paused": paused,
        "scope": "every run",
        "note": if paused {
            "every run is paused: none takes a step or starts until ai_agents_resume"
        } else {
            "every run is resumed"
        },
    })
}

/// The amounts an `ai_agents_extend` call adds: its arguments without `run`. `cost` is taken
/// as `cost_usd`.
pub fn extension(a: &Value) -> Value {
    let mut add = a.as_object().cloned().unwrap_or_default();
    add.remove("run");
    if let Some(c) = add.remove("cost") {
        add.entry("cost_usd").or_insert(c);
    }
    Value::Object(add)
}

/// The text of an `ai_agents_note` call, checked.
pub fn note(a: &Value) -> Result<String, String> {
    let text = a["text"].as_str().unwrap_or_default().trim();
    if text.is_empty() {
        return Err("give `text`".into());
    }
    if text.len() > crate::transcript::MAX_NOTE_BYTES {
        return Err(format!(
            "`text` is {} bytes; a note is at most {}",
            text.len(),
            crate::transcript::MAX_NOTE_BYTES
        ));
    }
    Ok(text.to_string())
}

//! The agents capability as its Worker serves it: the `agents_*` tools, behind the contract.
//! Core authenticates the caller and runs its gate before any of them; the caller who starts a
//! run (the person, or their delegate) is the caller the run acts as, with the run's id added,
//! so every model turn and pigeon call the run makes is theirs, through the same gate.

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, FailureKind, Manifest, SectionSpec, Service,
    Signed, ToolSpec,
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
pub const TOOLS: [&str; 17] = [
    "ai_agents_start",
    "ai_agents_estimate",
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
    "ai_agents_models",
    "ai_agents_config",
    "ai_agents_compare",
];

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

/// The arguments of `ai_agents_estimate`, checked: the role (executor by default), the brief and
/// the optional kind (any role may give one). Pure.
pub fn estimate_args(args: &Value) -> Result<(Role, String, Option<&'static str>), String> {
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
        return Err("`brief` is required: the brief the run would be started with".into());
    }
    if brief.len() > 64 * 1024 {
        return Err("`brief` is longer than 64 KiB".into());
    }
    let kind = match &args["kind"] {
        Value::Null => None,
        Value::String(s) => Some(crate::estimate::parse_kind(s).ok_or_else(|| {
            format!("`{s}` is not a kind: use integrate, docs, fix, design, build or live check")
        })?),
        _ => {
            return Err(
                "`kind` must be one of: integrate, docs, fix, design, build, live check".into(),
            );
        }
    };
    Ok((role, brief.to_string(), kind))
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
        let name = crate::tools::check_run_name(args.get("name"))?;
        let (model, child_models) = crate::models::start_models(role, args)?;
        let kind = crate::models::start_kind(role, args)?;
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
        // The brief's repo paths, checked against the base. A failed check is logged (never with
        // the brief) and left out; it never stops the start.
        let paths = crate::preflight::paths_in(&brief);
        let missing = if paths.is_empty() {
            None
        } else {
            let checked = crate::sandbox::call(
                env,
                &id,
                "preflight",
                &json!({
                    "run": id,
                    "role": role.name(),
                    "base": base,
                    "branch": branch(&id),
                    "caller": as_caller,
                    "paths": paths,
                }),
            )
            .await
            .map_err(|e| e.to_string())
            .and_then(|v| {
                crate::preflight::missing_from(
                    v["exit"].as_i64().unwrap_or(-1),
                    v["output"].as_str().unwrap_or_default(),
                    &paths,
                )
            });
            match checked {
                Ok(m) => Some(m),
                Err(e) => {
                    worker::console_error!("agents: the start preflight failed: {e}");
                    None
                }
            }
        };
        let note = missing
            .as_deref()
            .and_then(|m| crate::preflight::note_for(m, &base));
        // Only a suggestion: a failed estimate is logged (never with the brief) and left out.
        let estimate = match object::call(
            env,
            INDEX,
            "estimate",
            &json!({ "role": role.name(), "brief": brief, "kind": kind }),
        )
        .await
        {
            Ok(e) => Some(e),
            Err(e) => {
                worker::console_error!("agents: the start estimate failed: {e}");
                None
            }
        };
        let mut answer = object::call(
            env,
            INDEX,
            "create",
            &json!({
                "run": id,
                "role": role.name(),
                "runtime": "gateway",
                "caller": as_caller,
                "brief": brief,
                "name": name,
                "kind": kind,
                "base": base,
                "branch": branch(&id),
                "budget": budget,
                "estimate": estimate,
                "preflight_note": note,
                "model": model.map(|c| c.text()),
                "child_models": if child_models.is_empty() { Value::Null } else { child_models.to_json() },
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
        if let (Some(e), Some(obj)) = (estimate, answer.as_object_mut()) {
            obj.insert("suggested_budget".into(), e["suggested_budget"].clone());
            obj.insert("estimate".into(), e);
        } else if let Some(obj) = answer.as_object_mut() {
            obj.remove("estimate");
        }
        if let (Some(m), Some(obj)) = (&missing, answer.as_object_mut()) {
            obj.insert("missing_paths".into(), json!(m));
            obj.insert(
                "preflight".into(),
                json!(crate::preflight::sentence(paths.len(), m, &base)),
            );
        }
        Ok(answer)
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
            "ai_agents_estimate" => {
                let (role, brief, kind) = estimate_args(a)?;
                r(object::call(
                    env,
                    INDEX,
                    "estimate",
                    &json!({ "role": role.name(), "brief": brief, "kind": kind }),
                )
                .await)
            }
            "ai_agents_status" => {
                let opts = shape_args(a)?;
                let count = turns_arg(a)?;
                match a["run"].as_str() {
                    Some(run) => {
                        let mut v = r(object::call(
                            env,
                            INDEX,
                            "get",
                            &json!({ "run": run, "reader": reader }),
                        )
                        .await)?;
                        // Only a gateway run has an object with turn rows; a failure here
                        // leaves the status as it was.
                        if v["runtime"] == "gateway"
                            && let Ok(t) = object::call(
                                env,
                                &format!("run:{run}"),
                                "turns_recent",
                                &json!({ "turns": count }),
                            )
                            .await
                        {
                            add_turns(&mut v, &t, count);
                        }
                        Ok(shape(&v, &opts))
                    }
                    None => {
                        let mut v = r(object::call(
                            env,
                            INDEX,
                            "list",
                            &json!({
                                "reader": reader,
                                "limit": a["limit"],
                                "live": a["live"].as_bool().unwrap_or(false),
                            }),
                        )
                        .await)?;
                        shape_list(&mut v, "runs", &opts);
                        Ok(v)
                    }
                }
            }
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
            "ai_agents_inbox" => {
                let opts = shape_args(a)?;
                let mut v = r(object::call(
                    env,
                    INDEX,
                    "inbox",
                    &json!({ "reader": reader, "since": a["since"].as_i64().unwrap_or(0) }),
                )
                .await)?;
                shape_list(&mut v, "waiting", &opts);
                shape_list(&mut v, "ended", &opts);
                Ok(v)
            }
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
                let text = if a["event"].as_bool().unwrap_or(false) {
                    format!("{}{text}", crate::transcript::EVENT_MARKER)
                } else if text.starts_with(crate::transcript::EVENT_MARKER) {
                    return Err("`text` must not start with a control character".into());
                } else {
                    text
                };
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
                let runtime = match a.get("runtime") {
                    None => "claude_code",
                    Some(value) => match value.as_str() {
                        Some(runtime @ ("claude_code" | "codex_chat")) => runtime,
                        _ => return Err("`runtime` must be claude_code or codex_chat".into()),
                    },
                };
                if runtime == "claude_code" && !session.starts_with("session_") {
                    return Err("`session_id` must be a Claude Code session's id".into());
                }
                if runtime == "codex_chat" && session.trim().is_empty() {
                    return Err("`session_id` must identify the ChatGPT Codex session".into());
                }
                let role = match a["role"].as_str().unwrap_or("executor") {
                    r @ ("planner" | "executor" | "mechanical") => r,
                    other => {
                        return Err(format!(
                            "`role` must be planner, executor or mechanical, not `{other}`"
                        ));
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
                        "runtime": runtime,
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
            "ai_agents_models" => {
                let mut set = serde_json::Map::new();
                if !a["set"].is_null() {
                    for (slot, c) in crate::models::check_set(&a["set"])? {
                        set.insert(slot.name().into(), json!(c.text()));
                    }
                }
                r(object::call(env, INDEX, "models", &json!({ "set": set })).await)
            }
            "ai_agents_config" => {
                let set = a.as_object().filter(|o| !o.is_empty()).map(|_| a.clone());
                r(object::call(env, INDEX, "config", &json!({ "set": set })).await)
            }
            "ai_agents_compare" => {
                let days = crate::compare::days_arg(&a["days"])?;
                let role = crate::compare::role_arg(&a["role"])?;
                r(object::call(
                    env,
                    INDEX,
                    "compare",
                    &json!({ "days": days, "role": role.map(Role::name), "reader": reader }),
                )
                .await)
            }
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
                versions: crate::versions(),
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
                                "role": { "type": "string", "enum": ["planner", "executor", "mechanical"], "description": "`executor` by default; `mechanical` for moves, renames and docs; `planner` to plan a piece of work, spawn executors, review their branches, run CI and land phase by phase. Models by default (see ai_agents_models, which can store others): planner Claude Opus, executor Claude Sonnet, mechanical Claude Haiku. To choose, give `model` (`provider/model`, a `claude-…` name, or `anthropic` or `openai` (your key), `cf-anthropic`, `cf-openai` or `workers-ai` (Cloudflare credits; workers-ai has no small model, so name one: workers-ai/@cf/…) for that provider's model for the role), and for a planner `models` for its executors. E.g. a planner on OpenAI with executors on Claude: {role: \"planner\", model: \"cf-openai\", models: {executor: \"anthropic\"}}." },
                                "model": { "type": "string", "description": "This run's model: `provider/model` (cf-openai/gpt-6.2-sol), a `claude-…` name, or `anthropic`, `cf-anthropic`, `openai`, `cf-openai` or `workers-ai`. Else the stored default for its role." },
                                "models": { "type": "object", "description": "Planners only: what its executors use unless a spawn says otherwise.", "properties": { "executor": { "type": "string" }, "mechanical": { "type": "string" } } },
                                "kind": { "type": "string", "enum": ["integrate", "docs", "fix", "design", "build", "live check"], "description": "Planners only (refused for other roles): what the run is for. It is kept on the run, used by the cost estimate (the answer's `estimate` shows `kind` and `kind_source`) and shown in status, the listing and ai_agents_compare. With no `model`, the run uses the stored model for its kind (ai_agents_models slot `planner_<kind>`; `build` and `live check` have no slot) if one is set, else the stored planner default." },
                                "brief": { "type": "string", "description": "What to build, standing alone: the run sees nothing else." },
                                "name": { "type": "string", "minLength": 1, "maxLength": 60, "description": "A short name for the run, one line, 1 to 60 characters. Else it is made from the brief." },
                                "base": { "type": "string", "description": "The branch or commit to start from; main by default." },
                                "budget": { "type": "object", "additionalProperties": true, "description": "Any of turns, cost_usd, wall_min, sandbox_min; each at most its hard cap." }
                            },
                            "required": ["brief"]
                        }),
                    ),
                    tool(
                        "ai_agents_estimate",
                        "Estimates a run's cost from similar past runs (same role, brief size and kind; actual AI Gateway-based cost), with a range and a suggested budget. Suggests only.",
                        json!({
                            "type": "object",
                            "properties": {
                                "role": { "type": "string", "enum": ["planner", "executor", "mechanical"], "description": "`executor` by default." },
                                "brief": { "type": "string", "description": "The brief the run would be started with." },
                                "kind": { "type": "string", "enum": ["integrate", "docs", "fix", "design", "build", "live check"], "description": "Any role. The run's kind. With none, it is classified from the brief by keyword (first match: live check, integrate, fix, design, docs, else build; docs needs the brief's first sentence or `docs-only`). The answer shows `kind` and `kind_source` (`given` or `classified`)." }
                            },
                            "required": ["brief"]
                        }),
                    ),
                    tool(
                        "ai_agents_status",
                        "One run (with `run`): its state, current step, what it waits for, its \
                         spending against its budget, its branch and its report. Without \
                         `run`, your recent runs and whether runs are paused. Size options: \
                         `brief` false or \"line\", `report` \"short\" or false, `fields` (keys \
                         to keep; `run` always stays). With `run`, `turns_recent` lists the \
                         last `turns` turns (1 to 50, 10 by default), newest last, each \
                         `{n, job_id, model, at}`; `models_status {id}` with a listed job_id \
                         shows that job's endpoint (`chat` or `responses`). `job` is the job \
                         in flight.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "turns": { "type": "integer", "minimum": 1, "maximum": 50, "description": "With `run`: how many recent turns to list (10 by default, 50 at most)." },
                                "limit": { "type": "integer", "description": "20 by default, 100 at most." },
                                "live": { "type": "boolean", "description": "List only runs that have not ended." },
                                "brief": { "anyOf": [{ "type": "boolean" }, { "type": "string" }], "description": "true (default), false, or \"line\" (first line, 100 characters)." },
                                "report": { "anyOf": [{ "type": "string" }, { "type": "boolean" }], "description": "\"full\" (default), \"short\" (first line, 200 characters) or false." },
                                "fields": { "type": "array", "items": { "type": "string" }, "description": "Top-level keys to keep." }
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
                         and reports to them; pass `next_since` next time. Takes the size \
                         options of ai_agents_status (`brief`, `report`, `fields`).",
                        json!({
                            "type": "object",
                            "properties": {
                                "since": { "type": "integer" },
                                "brief": { "anyOf": [{ "type": "boolean" }, { "type": "string" }], "description": "true (default), false, or \"line\"." },
                                "report": { "anyOf": [{ "type": "string" }, { "type": "boolean" }], "description": "\"full\" (default), \"short\" or false." },
                                "fields": { "type": "array", "items": { "type": "string" } }
                            }
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
                         does). With `event: true` it reads as an event from pigeon, not the \
                         person.",
                        json!({
                            "type": "object",
                            "properties": {
                                "run": { "type": "string" },
                                "text": { "type": "string" },
                                "event": { "type": "boolean", "description": "Deliver as an event from pigeon." }
                            },
                            "required": ["run", "text"]
                        }),
                    ),
                    tool(
                        "ai_agents_register",
                        "Record a Claude Code or ChatGPT Codex session as a Pigeon run. \
                         The session runs in its own client and reports progress and results \
                         with ai_agents_report using the returned run id.",
                        json!({
                            "type": "object",
                            "properties": {
                                "session_id": { "type": "string" },
                                "runtime": { "type": "string", "enum": ["claude_code", "codex_chat"], "default": "claude_code" },
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
                        "From an external session registered with ai_agents_register: where \
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
                    tool(
                        "ai_agents_models",
                        "Show the models runs use, and set them. Each default (planner, executor, \
                         mechanical, summary for compaction) is marked `stored` or `built_in`; the \
                         known tiers of each provider are listed. With `set`, the entries given are \
                         checked and stored. A choice is `provider/model` (cf-openai/gpt-6.2-sol), a \
                         bare `claude-…` name (Anthropic), or a bare provider name for its model \
                         in the slot's tier. A run can override its default with `model` in \
                         ai_agents_start. Planner kinds (integrate, docs, fix, design) have slots \
                         `planner_integrate`, `planner_docs`, `planner_fix` and `planner_design`, \
                         marked `unset` until set; a planner started with that `kind` uses its \
                         slot, else the planner default.",
                        json!({
                            "type": "object",
                            "properties": {
                                "set": {
                                    "type": "object",
                                    "description": "Defaults to store, e.g. {executor: \"cf-openai\", summary: \"anthropic/claude-haiku-5-5\"}.",
                                    "properties": {
                                        "planner": { "type": "string" },
                                        "executor": { "type": "string" },
                                        "mechanical": { "type": "string" },
                                        "summary": { "type": "string" },
                                        "planner_integrate": { "type": "string" },
                                        "planner_docs": { "type": "string" },
                                        "planner_fix": { "type": "string" },
                                        "planner_design": { "type": "string" }
                                    }
                                }
                            }
                        }),
                    ),
                    tool(
                        "ai_agents_config",
                        "Show the concurrency caps, and set them. With no arguments, answers \
                         max_live_runs (live runs of every role, default 8, 1 to 64), \
                         max_executors_per_planner (default 3, 1 to 16) and executor_reserve \
                         (slots planners leave to executors, default 3, 0 up to max_live_runs - 1), \
                         with `is_default` saying which are not stored. With arguments, the ones \
                         given are checked and stored. A run beyond a cap stays queued, with the \
                         reason in its `step` in ai_agents_status, and starts as slots free.",
                        json!({
                            "type": "object",
                            "properties": {
                                "max_live_runs": { "type": "integer", "description": "1 to 64." },
                                "max_executors_per_planner": { "type": "integer", "description": "1 to 16." },
                                "executor_reserve": { "type": "integer", "description": "0 up to max_live_runs - 1." }
                            }
                        }),
                    ),
                    tool(
                        "ai_agents_compare",
                        "Compare ended runs (done, failed, over_budget, stopped) by role and model \
                         over the last `days`: runs, how each ended, cost in dollars (total, \
                         median, high = 90th percentile), median turns and median minutes. A run \
                         with no stored model counts as its role's built-in default. A planner's \
                         `kind` is part of the group.",
                        json!({
                            "type": "object",
                            "properties": {
                                "days": { "type": "integer", "description": "30 by default, 1 to 365." },
                                "role": { "type": "string", "enum": ["planner", "executor", "mechanical"] }
                            }
                        }),
                    ),
                ],
                // Sessions › Live and Sessions › Compare.
                sections: vec![
                    SectionSpec {
                        tab: "sessions".into(),
                        id: "live".into(),
                        title: "Live".into(),
                        description: "Runs going on now and what they wait for from you.".into(),
                        order: 10,
                        cost: None,
                        view: true,
                    },
                    SectionSpec {
                        tab: "sessions".into(),
                        id: "compare".into(),
                        title: "Compare".into(),
                        description: "Models compared by cost, turns and outcome per role.".into(),
                        order: 20,
                        cost: None,
                        view: true,
                    },
                ],
                // Its objects and container come from its wrangler.toml, which the architecture reads.
                footprint: Value::Null,
                ..CapabilityManifest::default()
            }],
        }
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

/// How `ai_agents_status` and `ai_agents_inbox` show a run's brief.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BriefShape {
    /// As stored (up to 300 characters).
    Full,
    /// Left out.
    Drop,
    /// Its first line, at most 100 characters.
    Line,
}

/// How `ai_agents_status` and `ai_agents_inbox` show a run's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportShape {
    /// As stored.
    Full,
    /// Its first line, at most 200 characters.
    Short,
    /// Left out.
    Drop,
}

/// The size options of `ai_agents_status` and `ai_agents_inbox`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// What to show of the brief.
    pub brief: BriefShape,
    /// What to show of the report.
    pub report: ReportShape,
    /// The top-level keys to keep (`run` is always kept); all when `None`.
    pub fields: Option<Vec<String>>,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            brief: BriefShape::Full,
            report: ReportShape::Full,
            fields: None,
        }
    }
}

/// Reads the size options `brief`, `report` and `fields` from a call's arguments.
pub fn shape_args(a: &Value) -> Result<Shape, String> {
    let brief = match &a["brief"] {
        Value::Null | Value::Bool(true) => BriefShape::Full,
        Value::Bool(false) => BriefShape::Drop,
        Value::String(s) if s == "line" => BriefShape::Line,
        _ => return Err("`brief` must be true, false or \"line\"".into()),
    };
    let report = match &a["report"] {
        Value::Null => ReportShape::Full,
        Value::Bool(false) => ReportShape::Drop,
        Value::String(s) if s == "full" => ReportShape::Full,
        Value::String(s) if s == "short" => ReportShape::Short,
        _ => return Err("`report` must be \"full\", \"short\" or false".into()),
    };
    let fields = match &a["fields"] {
        Value::Null => None,
        Value::Array(items) => {
            let mut keys = Vec::new();
            for item in items {
                keys.push(
                    item.as_str()
                        .ok_or("`fields` must be an array of strings")?
                        .to_string(),
                );
            }
            Some(keys)
        }
        _ => return Err("`fields` must be an array of strings".into()),
    };
    Ok(Shape {
        brief,
        report,
        fields,
    })
}

/// Reads `turns` (how many recent turns `ai_agents_status {run}` lists): 1 to 50, 10 when absent.
pub fn turns_arg(a: &Value) -> Result<usize, String> {
    match &a["turns"] {
        Value::Null => Ok(10),
        v => match v.as_i64() {
            Some(n) if (1..=50).contains(&n) => Ok(n as usize),
            _ => Err("`turns` must be a whole number from 1 to 50".into()),
        },
    }
}

/// The last `limit` of `rows` (each `{n, job_id, model, at}`, any order) as a list ordered by
/// `n`, newest last. A missing job id shows as null.
pub fn turns_recent(rows: &[Value], limit: usize) -> Value {
    let mut rows: Vec<&Value> = rows.iter().collect();
    rows.sort_by_key(|r| r["n"].as_i64().unwrap_or(0));
    let skip = rows.len().saturating_sub(limit);
    Value::Array(
        rows.into_iter()
            .skip(skip)
            .map(|r| {
                json!({
                    "n": r["n"],
                    "job_id": r["job_id"],
                    "model": r["model"],
                    "at": r["at"],
                })
            })
            .collect(),
    )
}

/// Puts `turns_recent` and, when a job is in flight and the status does not show it yet, `job`
/// into one run's status. `t` is the run object's `{turns, job}` answer.
fn add_turns(status: &mut Value, t: &Value, limit: usize) {
    let rows = t["turns"].as_array().cloned().unwrap_or_default();
    let Some(obj) = status.as_object_mut() else {
        return;
    };
    obj.insert("turns_recent".into(), turns_recent(&rows, limit));
    if let Some(job) = t["job"].as_str()
        && !obj.contains_key("job")
    {
        obj.insert("job".into(), json!(job));
    }
}

/// The first line of `text`, cut to at most `max` characters.
fn first_line(text: &str, max: usize) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(max)
        .collect()
}

/// Shapes one run's status by `opts`: the brief and report as asked, then only the wanted keys.
pub fn shape(run: &Value, opts: &Shape) -> Value {
    let Some(obj) = run.as_object() else {
        return run.clone();
    };
    let mut out = obj.clone();
    match opts.brief {
        BriefShape::Full => {}
        BriefShape::Drop => {
            out.remove("brief");
        }
        BriefShape::Line => {
            if let Some(b) = obj.get("brief").and_then(Value::as_str) {
                out.insert("brief".into(), json!(first_line(b, 100)));
            }
        }
    }
    match opts.report {
        ReportShape::Full => {}
        ReportShape::Drop => {
            out.remove("report");
        }
        ReportShape::Short => {
            if let Some(r) = obj.get("report").and_then(Value::as_str) {
                out.insert("report".into(), json!(first_line(r, 200)));
            }
        }
    }
    if let Some(keys) = &opts.fields {
        out.retain(|k, _| k == "run" || keys.iter().any(|f| f == k));
    }
    Value::Object(out)
}

/// Shapes the runs listed under `key` in an answer (`runs`, `waiting`, `ended`).
fn shape_list(answer: &mut Value, key: &str, opts: &Shape) {
    if let Some(list) = answer[key].as_array_mut() {
        for run in list.iter_mut() {
            *run = shape(run, opts);
        }
    }
}

//! Which tools an AI agent run is offered, where each call goes, and whether a call's input is
//! acceptable before it runs.
//!
//! This module is pure. It does no I/O and holds no state.

use crate::Role;
use serde_json::{Map, Value, json};

/// Where a tool call is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Runs in the run's container.
    Sandbox,
    /// Runs through pigeon's dispatch, as the run's caller.
    Pigeon,
    /// Handled by the run itself.
    Agent,
}

/// Pigeon tools an executor (or mechanical run) may call.
pub const EXECUTOR_PIGEON: [&str; 7] = [
    "policy_check",
    "ci_submit",
    "ci_status",
    "ci_commit",
    "land_status",
    "work_status",
    "land_list",
];

/// Pigeon tools a planner may call. Executors never land: the person's rule.
pub const PLANNER_PIGEON: [&str; 12] = [
    "policy_check",
    "ci_submit",
    "ci_status",
    "ci_commit",
    "land_status",
    "land",
    "land_cancel",
    "models_usage",
    "architecture_describe",
    "land_list",
    "land_watch",
    "work_status",
];

/// Files that only the person may change.
pub const GUARDED: [&str; 2] = [".pigeon/policy.toml", ".claude/settings.json"];

/// Tools that run in the container, in the order they are offered.
const SANDBOX: [&str; 8] = [
    "read_file",
    "write_file",
    "edit_file",
    "list",
    "grep",
    "run",
    "fetch",
    "commit_push",
];

/// Agent tools every role is offered.
const AGENT_COMMON: [&str; 2] = ["ask_person", "report"];

/// Agent tools only a planner is offered.
const AGENT_PLANNER: [&str; 6] = [
    "spawn_executor",
    "executor_status",
    "executor_result",
    "executor_stop",
    "wait_executors",
    "wait_events",
];

/// Largest `write_file` content, in bytes (1 MiB).
const MAX_CONTENT_BYTES: usize = 1024 * 1024;

/// Largest `run` command, in bytes (8 KiB).
const MAX_CMD_BYTES: usize = 8 * 1024;

/// The longest a `run` may take, in seconds: a run's step happens in an alarm, which has 15
/// minutes of wall time.
pub const MAX_RUN_S: u64 = 840;

/// Largest `grep` pattern, in bytes.
const MAX_PATTERN_BYTES: usize = 500;

/// Largest `commit_push` message, in bytes.
const MAX_MESSAGE_BYTES: usize = 2000;

/// Says where a tool call goes, or `None` if the role is not offered the tool.
///
/// Executor and Mechanical runs share one list. Planner agent tools are offered to planners
/// only.
pub fn kind(role: Role, name: &str) -> Option<Kind> {
    if SANDBOX.contains(&name) {
        return Some(Kind::Sandbox);
    }
    if AGENT_COMMON.contains(&name) {
        return Some(Kind::Agent);
    }
    if role == Role::Planner && AGENT_PLANNER.contains(&name) {
        return Some(Kind::Agent);
    }
    let pigeon: &[&str] = match role {
        Role::Planner => &PLANNER_PIGEON,
        Role::Executor | Role::Mechanical => &EXECUTOR_PIGEON,
    };
    if pigeon.contains(&name) {
        return Some(Kind::Pigeon);
    }
    None
}

/// Says whether a call is safe to repeat after an interruption.
pub fn idempotent(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "fetch"
            | "list"
            | "grep"
            | "policy_check"
            | "ci_status"
            | "ci_commit"
            | "land_status"
            | "land_list"
            | "work_status"
            | "models_usage"
            | "architecture_describe"
            | "executor_status"
            | "executor_result"
    )
}

/// Builds one Anthropic tool definition.
fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "input_schema": input_schema,
    })
}

/// Definitions for the sandbox tools, in offer order.
fn sandbox_definitions() -> Vec<Value> {
    vec![
        tool(
            "read_file",
            "Reads lines of a file in the checkout and numbers them. Returns at most 2000 lines per call; use from_line and to_line to page.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the checkout's root."},
                    "from_line": {"type": "integer", "minimum": 1, "description": "First line to read (1-based)."},
                    "to_line": {"type": "integer", "minimum": 1, "description": "Last line to read, not before from_line."}
                },
                "required": ["path"]
            }),
        ),
        tool(
            "write_file",
            "Creates a file or replaces a whole file with the given content. For small changes to an existing file, prefer edit_file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the checkout's root."},
                    "content": {"type": "string", "description": "The complete new content (at most 1 MiB)."}
                },
                "required": ["path", "content"]
            }),
        ),
        tool(
            "edit_file",
            "Replaces the one exact occurrence of `old` with `new` in a file. Fails unless `old` occurs exactly once, so include enough context to make it unique.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the checkout's root."},
                    "old": {"type": "string", "description": "The exact text to replace. Must be non-empty and occur once."},
                    "new": {"type": "string", "description": "The text to put in its place. Must differ from `old`."}
                },
                "required": ["path", "old", "new"]
            }),
        ),
        tool(
            "list",
            "Lists files and directories under a path. Defaults to the checkout's root and a depth of 2.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory path, relative to the checkout's root."},
                    "depth": {"type": "integer", "minimum": 1, "maximum": 4, "default": 2, "description": "How many levels to show."}
                }
            }),
        ),
        tool(
            "grep",
            "Finds lines matching a regular expression. Each match shows its file and line number. Returns at most 200 matches.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression to look for (at most 500 bytes)."},
                    "path": {"type": "string", "description": "File or directory to search, relative to the checkout's root."},
                    "glob": {"type": "string", "description": "Only search files whose names match this glob, such as `*.rs`."}
                },
                "required": ["pattern"]
            }),
        ),
        tool(
            "run",
            "Runs a shell command in the checkout's root. Answers its exit code and the end of its output. Builds and tests go here.",
            json!({
                "type": "object",
                "properties": {
                    "cmd": {"type": "string", "description": "The shell command (at most 8 KiB)."},
                    "timeout_s": {"type": "integer", "minimum": 1, "maximum": 840, "default": 600, "description": "Seconds before the command is stopped (14 minutes at most)."}
                },
                "required": ["cmd"]
            }),
        ),
        tool(
            "fetch",
            "Fetches a branch of the repository (an executor's branch, main) into the checkout as `origin/<branch>`, with your run's git grant, which `run` does not have (a `git fetch` there fails with 403). Answers the branch's commit. Then read it with `run`, such as `git diff main...origin/<branch>`.",
            json!({
                "type": "object",
                "properties": {
                    "ref": {"type": "string", "description": "The branch, such as `claude/ai-agent-r1`, or a full 40-character commit id."}
                },
                "required": ["ref"]
            }),
        ),
        tool(
            "commit_push",
            "Commits every change in the checkout, if there is any, and pushes HEAD to the run's branch, local commits included (a rebase, a cherry-pick). After you rewrite the branch (a rebase), set `force`: it pushes with --force-with-lease, to your run's branch only. Answers the pushed commit id.",
            json!({
                "type": "object",
                "properties": {
                    "message": {"type": "string", "description": "The commit message (at most 2000 bytes), used when there are changes to commit."},
                    "force": {"type": "boolean", "default": false, "description": "Push with --force-with-lease, for a branch you rewrote. The push fails if the branch moved since you last saw it."}
                },
                "required": ["message"]
            }),
        ),
    ]
}

/// Definitions for the AI agent tools the role is offered, in offer order.
fn agent_definitions(role: Role) -> Vec<Value> {
    let mut out = vec![
        tool(
            "ask_person",
            "Asks the person a question and waits for the answer. Use it for choices that change what is built, not for things you can look up. Give a headline, facts and the conventional option when you can.",
            json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question to ask."},
                    "headline": {"type": "string", "maxLength": 140, "description": "The question as ONE short sentence, one line."},
                    "facts": {
                        "type": "array",
                        "maxItems": 6,
                        "description": "Labelled supporting facts, such as Blocked, Against or Ready.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {"type": "string", "maxLength": 24},
                                "value": {"type": "string", "maxLength": 300},
                                "mono": {"type": "boolean", "description": "True for commits and file names."}
                            },
                            "required": ["label", "value"]
                        }
                    },
                    "conventional": {"type": "integer", "minimum": 0, "description": "0-based index into `options`: the option you take if no answer comes in 5 minutes."},
                    "options": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 2,
                        "maxItems": 6,
                        "description": "Answers to offer, if the choice is a short list."
                    }
                },
                "required": ["question"]
            }),
        ),
        tool(
            "report",
            "Ends the run. Say what changed, the branch and its tip, the CI result, and what is left.",
            json!({
                "type": "object",
                "properties": {
                    "state": {"type": "string", "enum": ["done", "failed"], "description": "`done` if the work is finished, `failed` if it is not."},
                    "summary": {"type": "string", "description": "What changed, the branch and its tip, CI, and what is left."}
                },
                "required": ["state", "summary"]
            }),
        ),
    ];
    if role == Role::Planner {
        out.push(tool(
            "spawn_executor",
            "Starts an executor run on a brief and answers its run id. The brief must stand alone, because the executor sees nothing else.",
            json!({
                "type": "object",
                "properties": {
                    "brief": {"type": "string", "description": "A complete task description for the executor."},
                    "name": {"type": "string", "minLength": 1, "maxLength": 60, "description": "A 2 to 5 word name for the executor, such as \"Cost tab fix\"."},
                    "kind": {"type": "string", "enum": ["code", "mechanical"], "default": "code", "description": "`code` for ordinary work, `mechanical` for simple repetitive edits."},
                    "model": {"type": "string", "description": "The executor's model: `provider/model` (cf-openai/gpt-6.2-sol), a `claude-…` name, or `anthropic`, `cf-anthropic` or `cf-openai` for that provider's model for its kind. Leave out to use what you were started with for executors, else the stored default."},
                    "base": {"type": "string", "description": "The branch or commit it starts from; main by default. Name another executor's branch to build on its work."},
                    "budget": {
                        "type": "object",
                        "description": "Limits for the executor. Leave out a field to use its default.",
                        "properties": {
                            "turns": {"type": "integer"},
                            "cost_usd": {"type": "number"},
                            "wall_min": {"type": "integer"},
                            "sandbox_min": {"type": "integer"}
                        }
                    }
                },
                "required": ["brief"]
            }),
        ));
        let run_schema = json!({
            "type": "object",
            "properties": {
                "run": {"type": "string", "description": "The run id that spawn_executor gave."}
            },
            "required": ["run"]
        });
        out.push(tool(
            "executor_status",
            "Answers how an executor run is going: its state and what it has used so far.",
            run_schema.clone(),
        ));
        out.push(tool(
            "executor_result",
            "Answers the final report of an executor run that has ended.",
            run_schema.clone(),
        ));
        out.push(tool(
            "executor_stop",
            "Stops an executor run that is still going.",
            run_schema,
        ));
        out.push(tool(
            "wait_executors",
            "Waits, without spending turns, until one of your executors ends (or all have \
             ended); answers which ended and how. Use it instead of asking executor_status \
             again and again.",
            json!({ "type": "object", "properties": {} }),
        ));
        out.push(tool(
            "wait_events",
            "Waits, without spending turns, until one of your executors ends, a note or \
             event arrives, or the minutes pass; answers which, with your executors' \
             state. Use it after `land` instead of polling land_status; landing state \
             changes arrive as events.",
            json!({
                "type": "object",
                "properties": {
                    "minutes": {"type": "integer", "minimum": 1, "maximum": 120, "description": "How long to wait at most; 30 by default."}
                }
            }),
        ));
    }
    out
}

/// Builds the tool definitions offered to a role.
///
/// The order is sandbox tools, agent tools, then the pigeon tools. Pigeon tools come from
/// `pigeon` as (name, description, input schema), in that order. Those the role is not offered
/// are left out.
pub fn definitions(role: Role, pigeon: &[(String, String, Value)]) -> Vec<Value> {
    let mut out = sandbox_definitions();
    out.extend(agent_definitions(role));
    for (name, description, schema) in pigeon {
        if kind(role, name) == Some(Kind::Pigeon) {
            out.push(tool(name, description, schema.clone()));
        }
    }
    out
}

/// Reads an optional string field. A field of another type is refused.
fn get_str<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("`{key}` must be a string; send it as text")),
    }
}

/// Reads a required string field, which may be empty.
fn need_str<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    get_str(obj, key)?.ok_or_else(|| format!("`{key}` is required; add it to the input"))
}

/// Reads a required, non-empty string field of at most `max` bytes.
fn need_text<'a>(obj: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, String> {
    let s = need_str(obj, key)?;
    if s.is_empty() {
        return Err(format!("`{key}` must not be empty; give it some text"));
    }
    if s.len() > max {
        return Err(format!(
            "`{key}` is {} bytes long and the limit is {max}; shorten it",
            s.len()
        ));
    }
    Ok(s)
}

/// Checks an optional run `name`: a string of 1 to 60 characters on one line once trimmed.
/// Answers the trimmed name, or `None` when it is absent or null.
pub fn check_run_name(v: Option<&Value>) -> Result<Option<String>, String> {
    let s = match v {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.trim(),
        Some(_) => return Err("`name` must be a string of 1 to 60 characters".to_string()),
    };
    let n = s.chars().count();
    if n == 0 || n > 60 {
        return Err(format!(
            "`name` is {n} characters long but must be 1 to 60; give a short name or leave it out"
        ));
    }
    if s.contains(['\n', '\r']) {
        return Err("`name` must be one line; remove the line breaks".to_string());
    }
    Ok(Some(s.to_string()))
}

/// Checks the optional `headline`, `facts` and `conventional` of an `ask_person` input.
fn check_structured_question(obj: &Map<String, Value>) -> Result<(), String> {
    match obj.get("headline") {
        None | Some(Value::Null) => {}
        Some(Value::String(h)) => {
            if h.chars().count() > 140 || h.contains(['\n', '\r']) {
                return Err(
                    "`headline` must be one line of at most 140 characters: the question as one short sentence"
                        .to_string(),
                );
            }
        }
        Some(_) => return Err("`headline` must be a string".to_string()),
    }
    match obj.get("facts") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            if items.len() > 6 {
                return Err(format!(
                    "`facts` has {} items but may have at most 6; keep the most useful",
                    items.len()
                ));
            }
            for (i, f) in items.iter().enumerate() {
                let Some(f) = f.as_object() else {
                    return Err(format!(
                        "`facts[{i}]` must be an object with `label` and `value`"
                    ));
                };
                for (key, max) in [("label", 24usize), ("value", 300)] {
                    match f.get(key) {
                        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= max => {}
                        _ => {
                            return Err(format!(
                                "`facts[{i}].{key}` must be a string of 1 to {max} characters"
                            ));
                        }
                    }
                }
                if !matches!(f.get("mono"), None | Some(Value::Null | Value::Bool(_))) {
                    return Err(format!("`facts[{i}].mono` must be true or false"));
                }
            }
        }
        Some(_) => return Err("`facts` must be an array of `{label, value}` objects".to_string()),
    }
    match obj.get("conventional") {
        None | Some(Value::Null) => Ok(()),
        Some(c) => {
            let Some(i) = c.as_u64() else {
                return Err(
                    "`conventional` must be a whole number: an index into `options`".to_string(),
                );
            };
            let n = obj
                .get("options")
                .and_then(Value::as_array)
                .map(Vec::len)
                .ok_or_else(|| {
                    "`conventional` needs `options`; add the list or leave `conventional` out"
                        .to_string()
                })?;
            if i as usize >= n {
                return Err(format!(
                    "`conventional` is {i} but `options` has {n} items; use 0 to {}",
                    n.saturating_sub(1)
                ));
            }
            Ok(())
        }
    }
}

/// Reads an optional whole number in `min..=max`.
fn get_int(obj: &Map<String, Value>, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if n >= min && n <= max => Ok(Some(n)),
            Some(n) => Err(format!(
                "`{key}` is {n} but must be between {min} and {max}; pick a value in that range"
            )),
            None => Err(format!(
                "`{key}` must be a whole number from {min} to {max}"
            )),
        },
    }
}

/// Splits a path into its meaningful segments, dropping empty and `.` ones.
fn segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// Checks a path is relative, safe, and not inside `.git`.
fn check_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("`path` must not be empty; give a path like `src/main.rs`".to_string());
    }
    if path.contains('\0') {
        return Err("`path` must not contain a NUL character; remove it".to_string());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(format!(
            "`path` must be relative to the checkout's root, so `{path}` is refused; drop the leading slash"
        ));
    }
    let parts = segments(path);
    if parts.contains(&"..") {
        return Err(format!(
            "`path` must stay inside the checkout, so `{path}` is refused; remove the `..` segments"
        ));
    }
    if parts
        .first()
        .is_some_and(|p| p.eq_ignore_ascii_case(".git"))
    {
        return Err(format!(
            "`path` must not point into `.git`, so `{path}` is refused; use commit_push for git work"
        ));
    }
    Ok(())
}

/// Says whether a path names one of the GUARDED files.
fn is_guarded(path: &str) -> bool {
    let norm = segments(path).join("/");
    GUARDED.iter().any(|g| g.eq_ignore_ascii_case(&norm))
}

/// Checks the `path` field, if present, and returns it.
///
/// When `required` is true, a missing path is refused.
fn path_field(obj: &Map<String, Value>, required: bool) -> Result<Option<&str>, String> {
    let path = get_str(obj, "path")?;
    match path {
        Some(p) => {
            check_path(p)?;
            Ok(Some(p))
        }
        None if required => Err("`path` is required; add it to the input".to_string()),
        None => Ok(None),
    }
}

/// Refuses a write to a guarded file.
fn refuse_guarded(path: &str) -> Result<(), String> {
    if is_guarded(path) {
        return Err(format!("only the person changes `{path}`; ask them"));
    }
    Ok(())
}

/// Checks one call's input before it runs.
///
/// The role must be offered the tool, and the input must be an object. Each built-in tool then
/// has its own rules. Pigeon tools get no further check here, because pigeon checks its own.
pub fn check(role: Role, name: &str, input: &Value) -> Result<(), String> {
    if kind(role, name).is_none() {
        return Err(format!(
            "`{name}` is not one of this run's tools; use only the tools you were offered"
        ));
    }
    let obj = input
        .as_object()
        .ok_or_else(|| format!("the input to `{name}` must be a JSON object; send named fields"))?;
    match name {
        "read_file" => {
            path_field(obj, true)?;
            let from = get_int(obj, "from_line", 1, u64::MAX)?;
            let to = get_int(obj, "to_line", 1, u64::MAX)?;
            if let (Some(f), Some(t)) = (from, to)
                && t < f
            {
                return Err(format!(
                    "`to_line` ({t}) comes before `from_line` ({f}); swap them or raise `to_line`"
                ));
            }
            Ok(())
        }
        "write_file" => {
            if let Some(p) = path_field(obj, true)? {
                refuse_guarded(p)?;
            }
            let content = need_str(obj, "content")?;
            if content.len() > MAX_CONTENT_BYTES {
                return Err(format!(
                    "`content` is {} bytes long and the limit is {MAX_CONTENT_BYTES}; split it across several files",
                    content.len()
                ));
            }
            Ok(())
        }
        "edit_file" => {
            if let Some(p) = path_field(obj, true)? {
                refuse_guarded(p)?;
            }
            let old = need_str(obj, "old")?;
            if old.is_empty() {
                return Err("`old` must not be empty; give the exact text to replace".to_string());
            }
            let new = need_str(obj, "new")?;
            if old == new {
                return Err(
                    "`old` and `new` are the same, so nothing would change; give a different `new`"
                        .to_string(),
                );
            }
            Ok(())
        }
        "list" => {
            path_field(obj, false)?;
            get_int(obj, "depth", 1, 4)?;
            Ok(())
        }
        "grep" => {
            path_field(obj, false)?;
            need_text(obj, "pattern", MAX_PATTERN_BYTES)?;
            get_str(obj, "glob")?;
            Ok(())
        }
        "run" => {
            need_text(obj, "cmd", MAX_CMD_BYTES)?;
            get_int(obj, "timeout_s", 1, MAX_RUN_S)?;
            Ok(())
        }
        "commit_push" => {
            need_text(obj, "message", MAX_MESSAGE_BYTES)?;
            match obj.get("force") {
                None | Some(Value::Null | Value::Bool(_)) => Ok(()),
                Some(_) => {
                    Err("`force` must be true or false; leave it out for a plain push".into())
                }
            }
        }
        "fetch" => {
            need_text(obj, "ref", 200)?;
            Ok(())
        }
        "ask_person" => {
            need_text(obj, "question", usize::MAX)?;
            check_structured_question(obj)?;
            match obj.get("options") {
                None => Ok(()),
                Some(Value::Array(items)) => {
                    if items.len() < 2 || items.len() > 6 {
                        return Err(format!(
                            "`options` has {} items but must have 2 to 6; adjust the list or leave it out",
                            items.len()
                        ));
                    }
                    if items.iter().any(|v| !v.is_string()) {
                        return Err("every item in `options` must be a string".to_string());
                    }
                    Ok(())
                }
                Some(_) => Err("`options` must be an array of 2 to 6 strings".to_string()),
            }
        }
        "report" => {
            let state = need_str(obj, "state")?;
            if state != "done" && state != "failed" {
                return Err(format!(
                    "`state` is `{state}` but must be `done` or `failed`; pick one"
                ));
            }
            need_text(obj, "summary", usize::MAX)?;
            Ok(())
        }
        "spawn_executor" => {
            need_text(obj, "brief", usize::MAX)?;
            check_run_name(obj.get("name"))?;
            if let Some(k) = get_str(obj, "kind")?
                && k != "code"
                && k != "mechanical"
            {
                return Err(format!(
                    "`kind` is `{k}` but must be `code` or `mechanical`; pick one"
                ));
            }
            let kind = if get_str(obj, "kind")? == Some("mechanical") {
                Role::Mechanical
            } else {
                Role::Executor
            };
            crate::models::spawn_model(kind, input)?;
            match obj.get("budget") {
                None | Some(Value::Object(_)) => Ok(()),
                Some(_) => Err(
                    "`budget` must be an object with fields like `turns` and `cost_usd`"
                        .to_string(),
                ),
            }
        }
        "executor_status" | "executor_result" | "executor_stop" => {
            need_text(obj, "run", usize::MAX)?;
            Ok(())
        }
        "wait_events" => {
            get_int(obj, "minutes", 1, 120)?;
            Ok(())
        }
        _ => Ok(()),
    }
}

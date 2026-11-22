//! What a run is told: its system prompt, its first message, the reminder to report, the pigeon
//! tools it is offered (as the model sees them; pigeon checks the arguments itself), and the
//! one-line step the console shows. Pure.

use serde_json::{Value, json};

use crate::Role;

/// The reminder a run gets when its model stops without calling a tool or `report`.
pub const NUDGE: &str = "You stopped without calling a tool. If the work is finished or cannot \
     go on, call `report`; otherwise keep working with your tools.";

/// The rules every run follows, whatever its role.
const RULES: &str = "\
Rules:
1. Read before you edit. Never change a file you have not read in this run.
2. Make the smallest change that does what the brief asks. Do not refactor beyond it.
3. After each change that compiles, run the narrowest check that covers it with `run`, then \
`commit_push`. Your branch is your checkpoint: push after every green step.
4. Tests live only in tests/smoke.rs. You may add a scenario there, prefixed with its Worker's \
name; the suite stays under 60 seconds per Worker. Never add a #[test] anywhere else.
5. Only the person changes .pigeon/policy.toml and .claude/settings.json. Never edit them.
6. You hold no tokens. Git goes through your run's grant, models through the gateway, \
Cloudflare only through tools. Never print, guess or ask for a secret.
7. A tool that answers that it needs the person's passkey makes the run wait for them; then \
it goes on by itself.
8. If the brief is ambiguous in a way that changes what you build, choose the conventional \
option and say so in your report.
9. Before you report done, run the repository's checks (`sh ci/runner/test.sh` for pigeon) or \
the part of them the brief names, then `ci_submit` the `test` job on your pushed commit and \
wait for it with `ci_status`.
10. End with `report`: what changed and why, the branch and its tip, the CI job and its state, \
what you did not do, and what a reviewer should read first.
11. Keep tool output small: prefer `grep` and line ranges to whole files, and read a long \
output's saved file only when you need more.
Style: plain English in comments and docs, short sentences, no marketing words, every public \
item documented in a line or two saying what it is for. Errors are returned, never panicked.";

/// The system prompt of a run of `role` working on `branch`.
pub fn system(role: Role, branch: &str) -> String {
    let who = match role {
        Role::Planner => {
            "You are a planner AI agent for pigeon, a Rust platform on Cloudflare Workers. You \
             plan a piece of work from the person's brief, split it into well-specified \
             executor briefs, review what each executor pushes, run CI and land phase by \
             phase. You never land a change to .pigeon/policy.toml or .claude/settings.json \
             without the person's passkey."
        }
        Role::Executor => {
            "You are an executor AI agent for pigeon, a Rust platform on Cloudflare Workers. You \
             build one well-specified piece of work from a brief, in your own sandbox \
             checkout, and push it to your branch. You never land: your planner or the \
             person does."
        }
        Role::Mechanical => {
            "You are an executor AI agent for pigeon doing a mechanical change (a move, a rename, \
             docs). Do exactly what the brief says, check it builds, and push it to your \
             branch. You never land."
        }
    };
    format!(
        "{who}\n\nYour checkout is the repository at your branch, `{branch}`. Your tools act \
         in it; `commit_push` pushes that branch and nothing else.\n\n{RULES}"
    )
}

/// The run's first message: its brief, and where it starts.
pub fn first_message(start: &Value) -> String {
    let brief = start["brief"].as_str().unwrap_or_default();
    let base = start["base"].as_str().unwrap_or("main");
    let branch = start["branch"].as_str().unwrap_or_default();
    format!(
        "Brief:\n\n{brief}\n\nYour branch `{branch}` starts from `{base}`. Begin by reading \
         what the brief points at."
    )
}

/// The pigeon tools a run may be offered, as `(name, description, input schema)`. Only those
/// its role allows are offered ([`crate::tools::definitions`]); pigeon checks the arguments of
/// every call itself, so these schemas are the model's guide, not the gate.
pub fn pigeon_tools() -> Vec<(String, String, Value)> {
    let t = |name: &str, description: &str, schema: Value| {
        (name.to_string(), description.to_string(), schema)
    };
    let id = json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] });
    vec![
        t(
            "policy_check",
            "Ask the person's policy whether an action is allowed before acting: push_branch, \
             land, add_test, change_gate and the like, with its target (a branch or a path).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "target": { "type": "string" }
                },
                "required": ["action"]
            }),
        ),
        t(
            "ci_submit",
            "Run a CI job from .pigeon/ci.toml (such as `test`) on a pushed commit; answers \
             the job's id.",
            json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "pigeon-security/pigeon" },
                    "commit": { "type": "string", "description": "A full 40-character commit id you pushed." },
                    "job": { "type": "string" },
                    "task": { "type": "string" }
                },
                "required": ["repo", "commit", "job"]
            }),
        ),
        t(
            "ci_status",
            "One CI job: its state, attempts and the end of its output.",
            id.clone(),
        ),
        t(
            "ci_commit",
            "The CI results recorded for a commit.",
            json!({
                "type": "object",
                "properties": { "repo": { "type": "string" }, "commit": { "type": "string" } },
                "required": ["repo", "commit"]
            }),
        ),
        t(
            "land_status",
            "The landing queue: every live landing and the last ones.",
            json!({ "type": "object", "properties": {} }),
        ),
        t(
            "land",
            "Land a session branch: test, merge and deploy through the gate.",
            json!({
                "type": "object",
                "properties": { "branch": { "type": "string" } },
                "required": ["branch"]
            }),
        ),
        t(
            "land_cancel",
            "Cancel a landing that has not merged.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string" } }
            }),
        ),
        t(
            "models_usage",
            "What model requests used over a window, in groups; `task_prefix` `ai-agent/` for \
             AI agent runs.",
            json!({
                "type": "object",
                "properties": {
                    "hours": { "type": "number" },
                    "task_prefix": { "type": "string" },
                    "by": { "type": "array", "items": { "type": "string" } }
                }
            }),
        ),
        t(
            "architecture_describe",
            "The platform's architecture as data: Workers, capabilities, tools, resources.",
            json!({ "type": "object", "properties": {} }),
        ),
    ]
}

/// The one line the console shows for a tool call in progress.
pub fn step_line(name: &str, input: &Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or_default();
    let line = match name {
        "run" => format!("run: {}", s("cmd")),
        "read_file" | "write_file" | "edit_file" => format!("{name}: {}", s("path")),
        "grep" => format!("grep: {}", s("pattern")),
        "list" => format!("list: {}", input["path"].as_str().unwrap_or(".")),
        "commit_push" => "commit and push".to_string(),
        "ci_submit" => format!("CI: {} on {}", s("job"), s("commit")),
        other => other.to_string(),
    };
    let line = line.replace('\n', " ");
    if line.chars().count() > 120 {
        let cut: String = line.chars().take(119).collect();
        format!("{cut}…")
    } else {
        line
    }
}

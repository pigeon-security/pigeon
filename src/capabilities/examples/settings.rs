//! Generates `.claude/settings.json` (Claude Code's project settings) from the allow rules of
//! `.pigeon/policy.toml`. The file is generated, never edited by hand. With `--check` it exits
//! 1 when the file differs from what the policy generates; the gate (`ci/runner/test.sh`) runs
//! that check. The provider-specific tool names live here, never in the policy.
//!
//! Run as:
//! `cargo run -q -p pigeon-capabilities --example settings --target x86_64-unknown-linux-gnu [-- --check]`

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use pigeon_core::policy::{self, Decision, Kind, Policy};
use serde_json::json;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// The names sessions see pigeon's MCP tools under, as (server, tool prefix): the claude.ai
/// connector `Pigeon MCP` (server `Pigeon_MCP`) to the Cloudflare MCP server portal on
/// `mcp.pigeon.dev`, which lists pigeon's tools under its server id `pigeon`, as
/// `pigeon_<tool>` (docs/plans/mcp-portal.md). The connector straight to
/// `platform.pigeon.dev/mcp` was removed at the cutover. Rules name each tool one by one: a
/// server-wide `mcp__<server>__*` would also allow every tool the policy does not name
/// (`call`, `source_token`) and the portal's own `portal_*` tools.
const PIGEON: [(&str, &str); 1] = [("Pigeon_MCP", "pigeon_")];

/// A tool of claude-code-remote's, or of pigeon's (under each of [`PIGEON`]).
enum Tool {
    Remote(&'static str),
    Pigeon(&'static str),
    Bash(&'static str),
}

/// The Claude Code tools an allowed action kind is carried out with.
fn tools(k: Kind) -> &'static [Tool] {
    match k {
        Kind::MessageSession => &[Tool::Remote("send_message")],
        Kind::StartSession => &[Tool::Remote("create_session")],
        Kind::ArchiveSession => &[Tool::Remote("archive_session")],
        Kind::Land => &[
            Tool::Pigeon("land"),
            Tool::Pigeon("land_status"),
            Tool::Pigeon("land_cancel"),
        ],
        // Git through the proxy, or over MCP: `branch_push` writes a bundle to the app's own
        // namespace, `session_credential` mints a git-only credential for it
        // (docs/plans/agent-git.md). Both are scoped to the calling app's namespace.
        Kind::PushBranch | Kind::RewriteBranch | Kind::DeleteBranch => &[
            Tool::Bash("git push https://proxy.pigeon.dev/*"),
            Tool::Pigeon("branch_push"),
            Tool::Pigeon("session_credential"),
        ],
        Kind::HandoffBranch
        | Kind::Deploy
        | Kind::MoveMain
        | Kind::ChangePolicy
        | Kind::ChangeGate
        | Kind::AddTest
        // A tool call is allowed by `dispatch` as it runs, never pre-approved: a rule for all
        // tools would otherwise pre-approve `call` and the passkey-gated tools.
        | Kind::CallTool => &[],
    }
}

/// `tool`'s permission rules, as the Claude Code docs write them: `mcp__<server>__<tool>`, and
/// `Bash(<command>)` with `*` matching any text.
fn rules(tool: &Tool) -> Vec<String> {
    match tool {
        Tool::Remote(t) => vec![format!("mcp__claude-code-remote__{t}")],
        Tool::Pigeon(t) => PIGEON
            .iter()
            .map(|(s, prefix)| format!("mcp__{s}__{prefix}{t}"))
            .collect(),
        Tool::Bash(c) => vec![format!("Bash({c})")],
    }
}

/// The allow list: `policy_check`, which only reads the policy, then each unconditional allow
/// rule's tools. Never `call`: it reaches every pigeon tool, the policy's or not.
fn allow_list(p: &Policy) -> Vec<String> {
    let mut allow: Vec<String> = Vec::new();
    let mut add = |tool: &Tool| {
        for r in rules(tool) {
            if !allow.contains(&r) {
                allow.push(r);
            }
        }
    };
    add(&Tool::Pigeon("policy_check"));
    for rule in &p.rules {
        if rule.decision != Decision::Allow || rule.approval.is_some() {
            continue;
        }
        for action in rule.actions.iter().copied() {
            for tool in tools(action) {
                add(tool);
            }
        }
    }
    allow
}

fn generate(p: &Policy) -> String {
    let value = json!({
        "$schema": "https://json.schemastore.org/claude-code-settings.json",
        "permissions": { "allow": allow_list(p) },
    });
    let mut out = serde_json::to_string_pretty(&value).expect("settings serialize");
    out.push('\n');
    out
}

fn run(check: bool) -> Result<ExitCode, String> {
    let root = Path::new(ROOT);
    let path = root.join(".pigeon/policy.toml");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let policy = policy::parse(&text)?;
    let generated = generate(&policy);
    let dir = root.join(".claude");
    let file = dir.join("settings.json");

    if check {
        let existing = fs::read_to_string(&file).unwrap_or_default();
        if existing == generated {
            println!(".claude/settings.json matches the policy");
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            ".claude/settings.json differs from what .pigeon/policy.toml generates: run \
             `cargo run -p pigeon-capabilities --example settings --target \
             x86_64-unknown-linux-gnu` and commit it"
        );
        return Ok(ExitCode::from(1));
    }

    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    fs::write(&file, &generated).map_err(|e| format!("{}: {e}", file.display()))?;
    println!(
        "wrote .claude/settings.json ({} tools)",
        allow_list(&policy).len()
    );
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = match args.as_slice() {
        [] => false,
        [a] if a == "--check" => true,
        _ => {
            eprintln!("usage: settings [--check]");
            return ExitCode::from(2);
        }
    };
    match run(check) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("settings: {e}");
            ExitCode::from(2)
        }
    }
}

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

/// The Claude Code tools an allowed action kind is carried out with.
fn tools(k: Kind) -> &'static [&'static str] {
    match k {
        Kind::MessageSession => &["mcp__claude-code-remote__send_message"],
        Kind::StartSession => &["mcp__claude-code-remote__create_session"],
        Kind::ArchiveSession => &["mcp__claude-code-remote__archive_session"],
        Kind::Land => &[
            "mcp__pigeon-platform__land",
            "mcp__pigeon-platform__land_status",
            "mcp__pigeon-platform__land_cancel",
        ],
        Kind::PushBranch | Kind::RewriteBranch | Kind::DeleteBranch => {
            &["Bash(git push https://proxy.pigeon.dev/*)"]
        }
        Kind::HandoffBranch
        | Kind::Deploy
        | Kind::MoveMain
        | Kind::ChangePolicy
        | Kind::ChangeGate
        | Kind::AddTest => &[],
    }
}

/// The allow list: `policy_check` and `call`, then each unconditional allow rule's tools.
fn allow_list(p: &Policy) -> Vec<String> {
    let mut allow: Vec<String> = Vec::new();
    let mut add = |tool: &str| {
        if !allow.iter().any(|t| t == tool) {
            allow.push(tool.to_string());
        }
    };
    add("mcp__pigeon-platform__policy_check");
    add("mcp__pigeon-platform__call");
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

//! Checks the calls a dry `wrangler deploy` made against the stand-in (`ci/deploy/standin.mjs`)
//! against the Cloudflare API proxy's allowlist, and fails naming each call (or body) the
//! proxy would refuse. Run natively by `ci/deploy/allowlist.sh`:
//!
//! ```text
//! allowlist <calls.txt> --account <id> --script <name> [--queue <name>]... [--namespace <id>]...
//! ```

use std::process::ExitCode;

use pigeon_proxies::cf_proxy::{Scope, unlisted};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(file) = args.first() else {
        eprintln!(
            "usage: allowlist <calls.txt> --account <id> --script <name> [--queue q]... [--namespace n]..."
        );
        return ExitCode::from(2);
    };
    let (mut account, mut script) = (String::new(), String::new());
    let (mut queues, mut namespaces) = (Vec::new(), Vec::new());
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let Some(value) = rest.next() else {
            eprintln!("{flag} needs a value");
            return ExitCode::from(2);
        };
        match flag.as_str() {
            "--account" => account = value.clone(),
            "--script" => script = value.clone(),
            "--queue" => queues.push(value.as_str()),
            "--namespace" => namespaces.push(value.as_str()),
            other => {
                eprintln!("unknown flag {other}");
                return ExitCode::from(2);
            }
        }
    }
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::from(2);
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        eprintln!("{file} holds no calls: the dry deploy made none");
        return ExitCode::from(1);
    }
    let scope = Scope {
        account: &account,
        script: &script,
        queues: &queues,
        namespaces: &namespaces,
    };
    let refused = unlisted(&lines, &scope);
    if refused.is_empty() {
        println!("all {} calls are on the allowlist", lines.len());
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "{} of {} calls are not on the allowlist:",
        refused.len(),
        lines.len()
    );
    for r in &refused {
        eprintln!("  {r}");
    }
    ExitCode::from(1)
}

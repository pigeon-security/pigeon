//! Runs the git proxy's push checks on a receive-pack request body read from stdin, as the
//! proxy does, so they can be tested against real git: the push rules, then, for an update
//! of the protected branch, the fast-forward check, with the repository's history read from
//! the bare repository named by `PIGEON_UPSTREAM` with `git pack-objects`, as Artifacts would
//! answer an upload-pack fetch. Prints `ok`, or writes the refusal git gets. Used by a local
//! stand-in server; not part of the Worker.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

use pigeon_capabilities::git_proxy::{Rule, check_all, not_forward};
use pigeon_core::{git, pack};

const PROTECTED: &str = "refs/heads/main";
const PREFIX: &str = "refs/heads/claude/";

/// The history the upstream repository's branches reach that `old` doesn't.
fn upstream(old: &str) -> Result<HashMap<String, Vec<String>>, String> {
    let repo = std::env::var("PIGEON_UPSTREAM").map_err(|_| "no PIGEON_UPSTREAM".to_string())?;
    let tips = Command::new("git")
        .args(["-C", &repo, "for-each-ref", "--format=%(objectname)"])
        .output()
        .map_err(|e| e.to_string())?;
    let mut revs: String = String::from_utf8_lossy(&tips.stdout)
        .lines()
        .filter(|t| *t != old)
        .map(|t| format!("{t}\n"))
        .collect();
    revs.push_str(&format!("^{old}\n"));
    let mut child = Command::new("git")
        .args(["-C", &repo, "pack-objects", "--stdout", "--revs", "-q"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(revs.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    Ok(pack::commits(&out.stdout)?.parents)
}

/// Runs a future that never waits.
fn now<F: std::future::Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match f.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("the check waited"),
    }
}

fn main() {
    let mut body = Vec::new();
    std::io::stdin().read_to_end(&mut body).unwrap();
    let c = match git::commands(&body) {
        Ok(Some(c)) => c,
        other => {
            eprintln!("unreadable ref updates: {other:?}");
            std::process::exit(2);
        }
    };
    eprintln!("caps: {:?}", c.caps);
    let refused = match check_all(&c.commands, PROTECTED, PREFIX) {
        Err(refused) => Some(refused),
        Ok(rules) if rules.contains(&Rule::FastForward) => {
            now(not_forward(&body, &c, &rules, PREFIX, |old: String| {
                std::future::ready(upstream(&old))
            }))
        }
        Ok(_) => None,
    };
    match refused {
        None => println!("ok"),
        Some(refused) => {
            eprintln!("refused: {refused:?}");
            let out = git::refusal(&c.commands, &c.caps, &refused);
            std::io::stdout().write_all(&out).unwrap();
        }
    }
}

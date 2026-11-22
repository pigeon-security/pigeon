//! Generates `ios/Pigeon/Design/Tokens.swift` from the web design tokens. The file is generated,
//! never edited by hand. With `--check` it exits 1 when the file differs; the smoke suite checks
//! the same drift.
//!
//! Run as:
//! `cargo run -q -p pigeon-capabilities --example design_swift --target x86_64-unknown-linux-gnu [-- --check]`

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn run(check: bool) -> Result<ExitCode, String> {
    let generated = pigeon_core::design::swift::generated()?;
    let file = Path::new(ROOT).join("ios/Pigeon/Design/Tokens.swift");

    if check {
        let existing = fs::read_to_string(&file).unwrap_or_default();
        if existing == generated {
            println!("ios/Pigeon/Design/Tokens.swift matches tokens.css");
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            "ios/Pigeon/Design/Tokens.swift differs from tokens.css: run `cargo run -p \
             pigeon-capabilities --example design_swift --target x86_64-unknown-linux-gnu` \
             and commit it"
        );
        return Ok(ExitCode::from(1));
    }

    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&file, &generated).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("wrote ios/Pigeon/Design/Tokens.swift");
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = match args.as_slice() {
        [] => false,
        [a] if a == "--check" => true,
        _ => {
            eprintln!("usage: design_swift [--check]");
            return ExitCode::from(2);
        }
    };
    match run(check) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("design_swift: {e}");
            ExitCode::from(2)
        }
    }
}

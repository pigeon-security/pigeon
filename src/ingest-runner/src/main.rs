//! `pigeon-ingest-runner`: runs the ingest job its environment names, then exits.
//! Exit codes: 0 done, 3 stopped, 1 error (one line on stderr, never a secret).

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use pigeon_ingest::manifest::RunState;

    if std::env::args().any(|a| a == "--version") {
        println!(
            "pigeon-ingest-runner {} {}",
            env!("CARGO_PKG_VERSION"),
            option_env!("PIGEON_COMMIT").unwrap_or("unknown")
        );
        return ExitCode::SUCCESS;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    if let Err(e) = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst)) {
        eprintln!("ingest runner: cannot watch for stop signals: {e}");
        return ExitCode::from(1);
    }
    match pigeon_ingest_runner::run_job(&|name| std::env::var(name).ok(), &stop) {
        Ok(RunState::Done) => ExitCode::SUCCESS,
        Ok(RunState::Stopped) => ExitCode::from(3),
        Ok(state) => {
            eprintln!("ingest runner: the run ended as {state:?}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("ingest runner: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}

//! Reads the environment, builds the source, destination and spool, and runs the job.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pigeon_core::s3::{Keys, parse_keys};
use pigeon_ingest::engine::{Clock, Dest, Run, Source, run};
use pigeon_ingest::managed_destination as managed;
use pigeon_ingest::manifest::{JobSpec, RunState, SourceSpec};
use pigeon_ingest::source_credentials as sources;

use crate::{DirSpool, LocalSource, R2Dest, S3Source};

/// The default spool budget: 50 GiB.
const DEFAULT_SPOOL_BYTES: u64 = 50 * 1024 * 1024 * 1024;

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        crate::client::now_ms() as i64
    }

    fn now_us(&self) -> u64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn pause(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// The default budget for files staged in memory: 3 GiB of a `standard-4`'s 12 GiB.
const DEFAULT_MEMORY_BYTES: u64 = 3 * 1024 * 1024 * 1024;
/// The largest file staged in memory: the managed destination's single-write limit.
const MEMORY_LIMIT: u64 = 16 * 1024 * 1024;

fn keys(env: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<Keys, String> {
    let value = env(name).ok_or_else(|| format!("{name} is not set"))?;
    parse_keys(&value).map_err(|e| format!("{name}: {e}"))
}

/// Runs the job named by the environment (`INGEST_JOB`, `SOURCE_KEYS`, `DEST_KEYS`, `SPOOL_DIR`,
/// `SPOOL_BYTES`; `env` looks a variable up) until it is done or `stop` is set. Answers the
/// state the run ended in. An error never holds a key.
pub fn run_job(
    env: &dyn Fn(&str) -> Option<String>,
    stop: &AtomicBool,
) -> Result<RunState, String> {
    let job_json = env("INGEST_JOB").ok_or("INGEST_JOB is not set")?;
    let job: JobSpec =
        serde_json::from_str(&job_json).map_err(|e| format!("INGEST_JOB is not a job: {e}"))?;
    if env("INGEST_REQUIRE_REVISION").as_deref() == Some("1")
        && option_env!("PIGEON_COMMIT") != Some(job.commit.as_str())
    {
        return Err("the runner binary does not match the approved ingest revision".into());
    }
    let source: Box<dyn Source> = match &job.source {
        SourceSpec::S3 {
            endpoint,
            region,
            bucket,
            prefix,
            ..
        } => match env(sources::RUN_ENV) {
            Some(raw) => {
                let source =
                    serde_json::from_str(&raw).map_err(|_| "invalid owning-account source")?;
                Box::new(
                    crate::managed_source::ManagedSource::new(source, endpoint, bucket)
                        .map_err(|error| error.to_string())?,
                )
            }
            None => Box::new(S3Source::new(
                endpoint,
                region,
                bucket,
                prefix,
                keys(env, "SOURCE_KEYS")?,
            )),
        },
        SourceSpec::Local { root, .. } => Box::new(LocalSource::new(PathBuf::from(root))),
    };
    let dest: Box<dyn Dest> = match env(managed::ENV) {
        Some(raw) => {
            let dest = serde_json::from_str(&raw).map_err(|_| "invalid managed destination")?;
            Box::new(crate::managed_dest::ManagedDest::new(dest).map_err(|e| e.to_string())?)
        }
        None => Box::new(R2Dest::new(
            &job.dest.endpoint,
            &job.dest.bucket,
            keys(env, "DEST_KEYS")?,
        )),
    };
    let budget = match env("SPOOL_BYTES") {
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map_err(|_| "SPOOL_BYTES is not a number".to_string())?,
        None => DEFAULT_SPOOL_BYTES,
    };
    let memory = match env("SPOOL_MEMORY_BYTES") {
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map_err(|_| "SPOOL_MEMORY_BYTES is not a number".to_string())?,
        None => DEFAULT_MEMORY_BYTES,
    };
    let dir = env("SPOOL_DIR").unwrap_or_else(|| "/spool".into());
    let spool = DirSpool::new(PathBuf::from(dir), budget)
        .map_err(|e| e.to_string())?
        .with_memory(MEMORY_LIMIT, memory);
    // Stop shortly before the approved run time ends, so the run checkpoints and ends `stopped`
    // (and can be continued) rather than being killed by its host.
    let stop_at = job.stop_at_ms(SystemClock.now_ms());
    let finished = AtomicBool::new(false);
    let report = std::thread::scope(|scope| {
        if let Some(at) = stop_at {
            let finished = &finished;
            scope.spawn(move || {
                while !finished.load(Ordering::SeqCst) {
                    if SystemClock.now_ms() >= at {
                        stop.store(true, Ordering::SeqCst);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
        }
        let report = run(&Run {
            job: &job,
            source: source.as_ref(),
            dest: dest.as_ref(),
            spool: &spool,
            clock: &SystemClock,
            stop,
        });
        finished.store(true, Ordering::SeqCst);
        report
    })
    .map_err(|e| e.to_string())?;
    Ok(report.state)
}

//! Run time from the file count, measured rates and continuations, shared by the three hosts'
//! plans (docs/plans/ingest-throughput.md).

use pigeon_core::r2::{self, Bucket};
use pigeon_ingest::estimate::{self, Estimate, Host, Measured, Throughput};
use pigeon_ingest::manifest::{self, Checkpoint, JobSpec, Resume, RunState};
use serde::Deserialize;
use serde_json::Value;
use worker::{Env, Result};

use super::{UNIQUE_SHARE, bad, now_ms};

/// How many earlier runs a plan reads for a measured rate.
const MEASURE_RUNS: usize = 3;
/// A run with no report whose status is older than this is taken as ended.
const STALE_MS: i64 = 15 * 60_000;

/// Worker threads of a run: IO-bound small objects on a 4-vCPU host.
pub(crate) const THREADS: u32 = 32;

/// Worker threads of a Cloudflare Container run: it reaches B2 and the Data Worker over the
/// internet, so each object waits on more round trips and more of them are kept in flight
/// (docs/plans/ingest-bytes.md). Memory staging is bounded by the runner's budget, not this.
pub(crate) const CLOUDFLARE_THREADS: u32 = 64;

/// Objects in flight assumed for `host` before a run there is measured. DigitalOcean and
/// Scaleway build the runner at the approved commit, so they run the parallel engine. A
/// Cloudflare Container runs the image pinned by hand, which may be an older serial runner, so
/// its estimate assumes one until a run measures it.
pub(crate) fn concurrency(host: Host) -> u32 {
    match host {
        Host::Cloudflare => 1,
        _ => THREADS,
    }
}

/// The `objects` argument: the source's whole object count, from the provider's console.
pub(crate) fn objects_arg(args: &Value) -> Result<Option<u64>> {
    match args.get("objects") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| bad("objects must be a positive whole number")),
    }
}

/// The estimate for a plan on `host`, preferring `measured`.
pub(crate) fn plan_estimate(
    host: Host,
    bytes: u64,
    objects: u64,
    extrapolated: bool,
    measured: Option<Measured>,
) -> Estimate {
    let throughput = Throughput {
        measured,
        extrapolated,
        ..Throughput::new(host, concurrency(host))
    };
    estimate::estimate_run(host, bytes, objects, UNIQUE_SHARE, &throughput)
}

#[derive(Deserialize)]
struct Counted {
    listed: u64,
    bytes_read: u64,
}

#[derive(Deserialize)]
struct Ended {
    state: RunState,
    counters: Counted,
    started_ms: i64,
    #[serde(default)]
    finished_ms: Option<i64>,
    #[serde(default)]
    updated_ms: Option<i64>,
    #[serde(default)]
    files_per_s: f64,
    #[serde(default)]
    bytes_per_s: f64,
}

/// The rate of a run from its `report.json`, else its `status.json`.
fn rate_of(run: &str, report: Option<&[u8]>, status: Option<&[u8]>) -> Option<Measured> {
    if let Some(r) = report.and_then(|b| serde_json::from_slice::<Ended>(b).ok()) {
        if r.files_per_s > 0.0 {
            let elapsed = r.finished_ms.unwrap_or(r.started_ms) - r.started_ms;
            return (elapsed >= 60_000 && r.files_per_s.is_finite()).then(|| Measured {
                run: run.to_string(),
                files_per_s: r.files_per_s,
                bytes_per_s: r.bytes_per_s,
            });
        }
        let end = r.finished_ms.unwrap_or(r.started_ms);
        return estimate::measure(
            run,
            r.counters.listed,
            r.counters.bytes_read,
            end - r.started_ms,
        );
    }
    let s = status.and_then(|b| serde_json::from_slice::<Ended>(b).ok())?;
    let end = s.updated_ms.unwrap_or(s.started_ms);
    estimate::measure(
        run,
        s.counters.listed,
        s.counters.bytes_read,
        end - s.started_ms,
    )
}

/// The newest measured rate of a run on `host` (`cloudflare`, `digitalocean`, `scaleway`) for
/// `instance`, read from the Data bucket. Anything unreadable is skipped.
pub(crate) async fn measured(
    env: &Env,
    account: &pigeon_core::Account,
    bucket: &str,
    instance: &str,
    host: &str,
) -> Option<Measured> {
    let mut records = super::runs(env, instance).await.ok()?;
    records.retain(|r| r.host == host);
    records.reverse();
    let bucket = Bucket {
        name: bucket.to_string(),
        jurisdiction: None,
    };
    for rec in records.into_iter().take(MEASURE_RUNS) {
        let report = r2::get(account, &bucket, &manifest::report_key(&rec.run))
            .await
            .ok()
            .flatten();
        let status = if report.is_none() {
            r2::get(account, &bucket, &manifest::status_key(&rec.run))
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        if let Some(m) = rate_of(&rec.run, report.as_deref(), status.as_deref()) {
            return Some(m);
        }
    }
    None
}

/// The run time for an estimate on `host`, in minutes, or the given override.
pub(crate) fn deadline(host: Host, estimate: &Estimate, given: Option<u64>) -> Result<u64> {
    estimate::run_minutes(host, estimate, given).map_err(bad)
}

/// A run to continue: its approved job and its checkpoint, read from the Data bucket. Refused for
/// a run of another instance, a finished run, one still running, or one with no checkpoint.
pub(crate) async fn continuation(
    account: &pigeon_core::Account,
    bucket: &str,
    instance: &str,
    prior: &str,
) -> Result<(JobSpec, Resume)> {
    let bucket = Bucket {
        name: bucket.to_string(),
        jurisdiction: None,
    };
    let get = |key: String| {
        let bucket = &bucket;
        async move { r2::get(account, bucket, &key).await }
    };
    let job: JobSpec = match get(manifest::job_key(prior)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => return Err(bad(format!("run `{prior}` has no job.json to continue"))),
    };
    if job.instance != instance || job.run != prior {
        return Err(bad(format!("run `{prior}` belongs to another instance")));
    }
    let ended = match get(manifest::report_key(prior)).await? {
        Some(bytes) => {
            let report: Ended = serde_json::from_slice(&bytes)?;
            if report.state == RunState::Done {
                return Err(bad(format!(
                    "run `{prior}` finished; there is nothing to continue"
                )));
            }
            true
        }
        None => false,
    };
    if !ended {
        let status = get(manifest::status_key(prior)).await?;
        let status: Option<Ended> = status.and_then(|b| serde_json::from_slice(&b).ok());
        let quiet = status.as_ref().is_none_or(|s| {
            s.state != RunState::Done && s.updated_ms.unwrap_or(s.started_ms) < now_ms() - STALE_MS
        });
        if !quiet {
            return Err(bad(format!(
                "run `{prior}` is still running or finished; continue it once it has stopped"
            )));
        }
    }
    let checkpoint: Checkpoint = match get(manifest::checkpoint_key(prior)).await? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => {
            return Err(bad(format!(
                "run `{prior}` wrote no checkpoint; plan a new run instead"
            )));
        }
    };
    if checkpoint.after_key.is_none() {
        return Err(bad(format!(
            "run `{prior}`'s checkpoint is empty; plan a new run instead"
        )));
    }
    let resume = Resume {
        run: prior.to_string(),
        checkpoint,
    };
    Ok((job, resume))
}

/// The `max_live_bytes` argument: the live cap for this run (default 10 GB), shown in the
/// approval the person signs.
pub(crate) fn live_cap(args: &Value) -> Result<u64> {
    use pigeon_ingest::managed_destination::{LIVE_CAP_BYTES, LIVE_CAP_MAX};
    match args.get("max_live_bytes") {
        None | Some(Value::Null) => Ok(LIVE_CAP_BYTES),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=LIVE_CAP_MAX).contains(n))
            .ok_or_else(|| bad(format!("max_live_bytes must be from 1 to {LIVE_CAP_MAX}"))),
    }
}

/// The shared live guard for a managed run, from a first listing page.
pub(crate) fn live(
    args: &Value,
    dry_run: bool,
    listing: &pigeon_core::s3::Listing,
    multipart: bool,
) -> Result<std::result::Result<pigeon_ingest::managed_destination::Live, String>> {
    let sampled = listing
        .objects
        .iter()
        .fold(0u64, |sum, o| sum.saturating_add(o.size));
    let largest = listing.objects.iter().map(|o| o.size).max().unwrap_or(0);
    Ok(pigeon_ingest::managed_destination::live_guard(
        dry_run,
        listing.next.is_some(),
        args["size_bytes"].as_u64(),
        sampled,
        largest,
        multipart,
        live_cap(args)?,
    ))
}

/// The plan line for the expected time.
pub(crate) fn time_line(estimate: &Estimate) -> String {
    format!(
        "Expected {:.1} to {:.1} hours ({}-bound, {:.1} files a second, {} confidence); the runner stops itself before its deadline and the run can then be continued.",
        estimate.hours_low,
        estimate.hours_high,
        estimate.bound,
        estimate.files_per_s,
        estimate.confidence
    )
}

/// The plan line for a continuation.
pub(crate) fn continue_line(resume: &Resume) -> String {
    format!(
        "Continue run {} after key {:?} ({} objects already listed); a new run id, the same source and prefix.",
        resume.run,
        resume.checkpoint.after_key.as_deref().unwrap_or(""),
        resume.checkpoint.counters.listed
    )
}

//! Scheduled resource cleanup and terminal audit for Scaleway ingest jobs.
use pigeon_core::r2::{self, Bucket};
use pigeon_ingest::{
    audit::{Cleanup, PipelineCounts, PipelineKind, PipelineOutcome, PipelineTerminal},
    manifest::{self, Report, RunState, Status},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Result};

use crate::{
    jobs_audit::{self, ExportStatus},
    users::directory,
};

use super::{RunRecord, bad, hosts, net, now_ms, run_key, secret_value, setting};

const MINUTE: i64 = 60_000;
const START_GRACE: i64 = 5 * MINUTE;
pub(super) const MAX_RUN: i64 = 24 * 60 * MINUTE;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lifecycle {
    account: String,
    project: String,
    compute_slot: String,
    audit_bucket: String,
    next_ms: i64,
    lease_until_ms: i64,
    deadline_ms: i64,
    audit_status: ExportStatus,
    terminal: Option<PipelineTerminal>,
}

impl Lifecycle {
    pub(super) fn new(
        account: String,
        project: String,
        compute_slot: String,
        audit_bucket: String,
        time: i64,
    ) -> Self {
        Self {
            account,
            project,
            compute_slot,
            audit_bucket,
            next_ms: time + START_GRACE,
            lease_until_ms: time + START_GRACE,
            deadline_ms: time + MAX_RUN,
            audit_status: ExportStatus::Pending,
            terminal: None,
        }
    }
}

fn terminal(run: &RunRecord, outcome: PipelineOutcome, report: Option<Report>) -> PipelineTerminal {
    let counts = report.map_or_else(PipelineCounts::default, |report| PipelineCounts {
        examined: report
            .counters
            .files
            .saturating_add(report.counters.members),
        stored: report.counters.uploaded,
        duplicates: report.counters.duplicates,
        failed: report.counters.failed,
        bytes_read: report.counters.bytes_read,
        bytes_written: report.counters.uploaded_bytes,
    });
    PipelineTerminal {
        run_id: run.run.clone(),
        started_ms: run.started_ms.max(0) as u64,
        finished_ms: now_ms().max(run.started_ms) as u64,
        outcome,
        cleanup: Some(Cleanup::Unconfirmed),
        counts,
        estimated_usd_micros: None,
    }
}

async fn replace(env: &Env, previous: &str, run: &RunRecord) -> Result<bool> {
    Ok(directory(
        env,
        "scaleway.ingest.replace",
        json!({
            "run": run.run,
            "previous": previous,
            "value": serde_json::to_string(run)?,
        }),
    )
    .await?
        == true)
}

/// Snapshot a cancellation request. The cron reaper owns actual deletion.
pub(super) async fn request_stop(env: &Env, run: &str) -> Result<bool> {
    let Some(prior) = setting(env, &run_key(run)).await? else {
        return Ok(false);
    };
    let mut run: RunRecord = serde_json::from_str(&prior)?;
    let cancelled = terminal(&run, PipelineOutcome::Cancelled, None);
    let Some(state) = run.scaleway.as_mut() else {
        return Ok(false);
    };
    if state.terminal.is_some() {
        return Ok(true);
    }
    if state.lease_until_ms > now_ms() {
        return Ok(false);
    }
    state.terminal = Some(cancelled);
    state.next_ms = now_ms();
    replace(env, &prior, &run).await
}

pub(super) fn mark_started(run: &mut RunRecord) {
    if let Some(state) = run.scaleway.as_mut() {
        state.lease_until_ms = 0;
    }
}

pub(super) fn mark_start_failed(run: &mut RunRecord) {
    let failure = terminal(run, PipelineOutcome::Failed, None);
    if let Some(state) = run.scaleway.as_mut() {
        state.terminal = Some(failure);
        state.next_ms = now_ms();
        state.lease_until_ms = 0;
    }
}

/// This target is resolved on both sides of approval, and a run never changes destinations.
pub(super) async fn audit_bucket(
    env: &Env,
    host: &str,
    record: &crate::instances::record::Instance,
) -> Result<String> {
    let bucket = jobs_audit::configured_bucket(record)
        .ok_or_else(|| bad("configure this instance's private Jobs audit bucket first"))?;
    let plan = jobs_audit::setup_plan(env, host, record).await?;
    if plan["action"] != "reuse" || plan["bucket"] != bucket {
        return Err(bad(
            "create and verify the approved private Jobs audit bucket first",
        ));
    }
    Ok(bucket.to_string())
}

pub(super) async fn provider_credentials(env: &Env, state: &Lifecycle) -> Result<String> {
    let value = secret_value(env, &state.compute_slot)
        .await
        .ok_or_else(|| bad("the saved Scaleway compute slot is unavailable"))?;
    let (project, secret) = hosts::scaleway_parts(&value).map_err(bad)?;
    if project != state.project {
        return Err(bad("the Scaleway compute project has changed"));
    }
    Ok(secret)
}

async fn runner_outcome(
    env: &Env,
    host: &str,
    run: &RunRecord,
    state: &Lifecycle,
) -> Result<Option<PipelineTerminal>> {
    let Some(destination) = run.destination.as_ref() else {
        return Ok(None);
    };
    if destination.endpoint != hosts::r2_endpoint(&state.account) {
        return Err(bad("the Scaleway job's R2 destination changed"));
    }
    let account =
        crate::linked::account_for_bound(env, host, &run.instance, &state.account).await?;
    let bucket = Bucket {
        name: destination.bucket.clone(),
        jurisdiction: None,
    };
    let report = r2::get(&account, &bucket, &manifest::report_key(&run.run)).await?;
    if let Some(report) = report {
        let report: Report = serde_json::from_slice(&report)?;
        if report.run != run.run {
            return Err(bad("the runner report does not match the Scaleway job"));
        }
        let outcome = match report.state {
            RunState::Done => PipelineOutcome::Succeeded,
            RunState::Failed => PipelineOutcome::Failed,
            RunState::Stopped => PipelineOutcome::Cancelled,
            _ => return Ok(None),
        };
        return Ok(Some(terminal(run, outcome, Some(report))));
    }
    let status = r2::get(&account, &bucket, &manifest::status_key(&run.run)).await?;
    if let Some(status) = status {
        let status: Status = serde_json::from_slice(&status)?;
        if status.run == run.run && status.state == RunState::Failed {
            return Ok(Some(terminal(run, PipelineOutcome::Failed, None)));
        }
    }
    Ok(None)
}

/// Request removal of the server and confirm its volumes are all gone.
async fn cleanup(run: &mut RunRecord, secret: &str, project: &str) -> Result<()> {
    run.vm_deleted = pigeon_cloud::scaleway_ingest::cleanup(
        secret,
        &hosts::zone(&run.region),
        project,
        &run.run,
        run.vm.as_deref(),
        &mut run.scaleway_volumes,
    )
    .await?;
    Ok(())
}

async fn reconcile(
    env: &Env,
    host: &str,
    run: &mut RunRecord,
    state: &mut Lifecycle,
) -> Result<()> {
    if state.terminal.is_none() {
        state.terminal = runner_outcome(env, host, run, state).await.ok().flatten();
        if state.terminal.is_none() && now_ms() >= state.deadline_ms {
            state.terminal = Some(terminal(run, PipelineOutcome::TimedOut, None));
        }
    }
    if run.vm.is_none() {
        let secret = provider_credentials(env, state).await?;
        let zone = hosts::zone(&run.region);
        if let Some(resources) =
            net::scw_find_created(&secret, &zone, &state.project, &run.run).await?
        {
            run.vm = Some(resources.server);
            run.scaleway_volumes = resources.volumes;
            state
                .terminal
                .get_or_insert_with(|| terminal(run, PipelineOutcome::Failed, None));
        } else if now_ms() < state.deadline_ms {
            return Ok(());
        }
    }
    if state.terminal.is_none() && !run.vm_deleted {
        let secret = provider_credentials(env, state).await?;
        let zone = hosts::zone(&run.region);
        let id = run
            .vm
            .as_deref()
            .ok_or_else(|| bad("missing Scaleway server"))?;
        if matches!(
            net::scw_state(&secret, &zone, id).await?.as_deref(),
            Some("stopped" | "stopped in place") | None
        ) {
            // A successful runner writes its report before its guest powers off.
            state.terminal = Some(terminal(run, PipelineOutcome::Failed, None));
        }
    }
    if state.terminal.is_some() && !run.vm_deleted {
        let secret = provider_credentials(env, state).await?;
        cleanup(run, &secret, &state.project).await?;
    }
    if run.vm_deleted
        && let Some(terminal) = state.terminal.as_mut()
    {
        terminal.cleanup = Some(Cleanup::Confirmed);
        state.audit_status = jobs_audit::export_pipeline(
            env,
            host,
            &run.instance,
            (&state.account, &state.audit_bucket),
            PipelineKind::ScalewayIngest,
            terminal,
        )
        .await;
        state.next_ms = if state.audit_status == ExportStatus::Stored {
            0
        } else {
            now_ms() + 5 * MINUTE
        };
    }
    Ok(())
}

/// Claim a small batch and reconcile it; a lease suppresses overlapping cron ticks.
pub async fn reap(env: &Env, host: &str) -> Result<()> {
    let due: Vec<String> =
        serde_json::from_value(directory(env, "scaleway.ingest.pending", Value::Null).await?)?;
    for prior in due {
        let mut run: RunRecord = serde_json::from_str(&prior)?;
        let Some(state) = run.scaleway.as_mut() else {
            continue;
        };
        state.lease_until_ms = now_ms() + 2 * MINUTE;
        state.next_ms = now_ms() + MINUTE;
        let claim = serde_json::to_string(&run)?;
        if !replace(env, &prior, &run).await? {
            continue;
        }
        let mut state = run
            .scaleway
            .take()
            .expect("the claimed job has its lifecycle");
        if reconcile(env, host, &mut run, &mut state).await.is_err() {
            state.next_ms = now_ms() + 5 * MINUTE;
        }
        state.lease_until_ms = 0;
        run.scaleway = Some(state);
        replace(env, &claim, &run).await?;
    }
    Ok(())
}

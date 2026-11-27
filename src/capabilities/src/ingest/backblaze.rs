//! The instance-owned Backblaze data-ingest adapter. The source and its credential are
//! independent of the backup connection and of legacy Storage accounts.

use pigeon_core::r2::{self, Bucket};
use pigeon_core::s3::{self, Endpoint};
use pigeon_ingest::archive::Limits;
use pigeon_ingest::audit::{
    Cleanup, PipelineCounts, PipelineKind, PipelineOutcome, PipelineTerminal,
};
use pigeon_ingest::backblaze::Source;
pub use pigeon_ingest::container_job::TERMINAL_PATH;
use pigeon_ingest::container_job::{self, Outcome, Terminal, TerminalNotice};
use pigeon_ingest::estimate::{self, Host};
use pigeon_ingest::manifest::{DestSpec, JobSpec, SourceSpec, job_key};
use pigeon_ingest::protocol::{self, StartRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Method, Request, Response, Result};

use crate::data_store;
use crate::instances::{self, record::Instance};
use crate::jobs_audit::{self, ExportStatus};
use crate::{Context, to_value, users::directory};

use super::{Ingest, KEPT_MS, KIND, PENDING_MS, RunRecord, UNIQUE_SHARE, bad, data_enabled};
use super::{
    execution_binding, hosts, index_key, keep, net, now_ms, runs_of, save_run, secret_value,
    setting, today, unready,
};

const ADAPTER: &str = "backblaze";
const MAX_CALLBACK_BYTES: usize = 8 * 1024;
const MICROS_PER_HOUR: u128 = 400_000;

pub(crate) fn source(record: &Instance) -> Option<&Source> {
    record.capabilities.get(super::NAME)?.backblaze.as_ref()
}

pub(crate) fn source_slot(instance: &str) -> String {
    if instance.bytes().all(|value| value.is_ascii_alphanumeric()) {
        format!("INGEST_B2_{}", instance.to_ascii_uppercase())
    } else {
        let hash = pigeon_core::oauth::hash(instance);
        format!(
            "INGEST_B2_HASH_{}",
            pigeon_core::sign::hex(hash.as_bytes())[..32].to_ascii_uppercase()
        )
    }
}

fn connection(record: &Instance) -> Result<&Source> {
    if !data_enabled(record) {
        return Err(bad("Enable Data and Data Ingest on this instance first."));
    }
    let binding = source(record)
        .ok_or_else(|| bad("Configure this instance's approved read-only Backblaze source."))?;
    binding.validate().map_err(bad)?;
    if data_store::settings(record)
        .and_then(|settings| settings.backup.as_ref())
        .is_some_and(|backup| backup.bucket.eq_ignore_ascii_case(&binding.bucket))
    {
        return Err(bad(
            "Backblaze ingest and backup must use separate buckets.",
        ));
    }
    Ok(binding)
}

async fn source_keys(record: &Instance, env: &Env) -> Result<String> {
    let name = source_slot(&record.id);
    let pair = secret_value(env, &name).await.ok_or_else(|| {
        bad(format!(
            "Bind the read-only Backblaze source key as {name}."
        ))
    })?;
    s3::parse_keys(&pair).map_err(|_| bad("the saved Backblaze source key is invalid"))?;
    Ok(pair)
}

async fn sample(source: &SourceSpec, pair: &str) -> Result<s3::Listing> {
    let SourceSpec::S3 {
        endpoint,
        region,
        bucket,
        prefix,
        ..
    } = source
    else {
        return Err(bad("Backblaze requires an S3 source"));
    };
    let endpoint = Endpoint {
        base: endpoint.clone(),
        region: region.clone(),
    };
    let key = s3::parse_keys(pair)?;
    let query = s3::list_objects_query(
        Some(prefix.as_str()).filter(|prefix| !prefix.is_empty()),
        None,
        1000,
        None,
    );
    let mut response = s3::send(
        Method::Get,
        &endpoint,
        &s3::object_path(bucket, ""),
        &query,
        &[],
        &key,
    )
    .await
    .map_err(|_| bad("The bound key could not list the approved Backblaze prefix."))?;
    s3::parse_list_objects(&response.text().await?)
        .map_err(|_| bad("Backblaze returned an unreadable object listing."))
}

pub(crate) async fn check_key(record: &Instance, pair: &str) -> Result<Value> {
    let approved = connection(record)?.bind(None).map_err(bad)?;
    let listing = sample(&approved, pair).await?;
    Ok(json!({ "instance": record.id, "verified": true, "sample_count": listing.objects.len() }))
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    account: String,
    audit_bucket: String,
    worker: String,
    image: String,
    max_minutes: u64,
}

struct Ready {
    execution: Execution,
    account: pigeon_core::Account,
    bucket: Bucket,
    source: SourceSpec,
    source_keys: String,
    dest_keys: String,
    worker_key: String,
    revision: String,
}

async fn ready(
    cap: &Ingest,
    record: &Instance,
    env: &Env,
    source: SourceSpec,
    max_minutes: u64,
) -> Result<Ready> {
    if data_store::inline(record) {
        return Err(bad(
            "Backblaze ingest needs Data deployed in its own Worker with an ingest container attached.",
        ));
    }
    let account = cap.checked_link(env, record).await?;
    let bucket = Bucket {
        name: data_store::store_name(record),
        jurisdiction: None,
    };
    let audit_bucket = jobs_audit::configured_bucket(record)
        .filter(|name| *name != bucket.name)
        .ok_or_else(|| {
            bad("Set up a separate private Jobs audit bucket before planning Backblaze ingest.")
        })?;
    for name in [&bucket.name, audit_bucket] {
        r2::require_private_bucket(
            &account,
            &Bucket {
                name: name.to_string(),
                jurisdiction: None,
            },
        )
        .await
        .map_err(|_| {
            bad(format!(
                "The expected private R2 bucket {name} could not be verified."
            ))
        })?;
    }
    let source_keys = source_keys(record, env).await?;
    let dest_slot = hosts::r2_slot(&record.id);
    let dest_keys = secret_value(env, &dest_slot).await.ok_or_else(|| {
        bad(format!(
            "Bind an object key scoped to {} as {dest_slot}.",
            bucket.name
        ))
    })?;
    let keys =
        s3::parse_keys(&dest_keys).map_err(|_| bad("the saved R2 destination key is invalid"))?;
    let data_endpoint = Endpoint {
        base: hosts::r2_endpoint(&account.id),
        region: "auto".into(),
    };
    s3::send(
        Method::Head,
        &data_endpoint,
        &s3::object_path(&bucket.name, ""),
        &[],
        &[],
        &keys,
    )
    .await
    .map_err(|_| bad("The bound R2 key cannot read the approved Data bucket."))?;
    let (worker, worker_key) = data_store::reach(env, record).await?;
    execution_binding(Host::Cloudflare, Some(&worker), None)?;
    let reply = net::data_post(&worker, container_job::READY_PATH, &worker_key, now_ms(), &json!({}))
        .await
        .map_err(|_| bad("The Data Worker has no reachable ingest container. Deploy its reviewed runtime configuration."))?;
    let reply: Value = serde_json::from_str(&reply)?;
    let revision = reply["image_revision"]
        .as_str()
        .filter(|revision| container_job::valid_revision(revision));
    let image = reply["image"]
        .as_str()
        .filter(|image| container_job::valid_image(image, &account.id));
    let (Some(revision), Some(image)) = (
        revision.filter(|_| reply["jobs_origin"] == format!("https://{}", cap.host_name())),
        image,
    ) else {
        return Err(bad(
            "The Data Worker needs a named ingest container image and the Jobs callback configured.",
        ));
    };
    Ok(Ready {
        execution: Execution {
            account: account.id.clone(),
            audit_bucket: audit_bucket.to_string(),
            worker,
            image: image.to_string(),
            max_minutes,
        },
        account,
        bucket,
        source,
        source_keys,
        dest_keys,
        worker_key,
        revision: revision.to_string(),
    })
}

#[derive(Serialize)]
struct Payload<'a> {
    adapter: &'static str,
    job: &'a JobSpec,
    execution: &'a Execution,
}

fn payload(job: &JobSpec, execution: &Execution) -> Value {
    to_value(Payload {
        adapter: ADAPTER,
        job,
        execution,
    })
}

pub(crate) async fn plan(
    cap: &Ingest,
    cx: &Context<'_>,
    record: &Instance,
    args: &Value,
) -> Result<Value> {
    let env = cx.env;
    let caller = cx
        .caller
        .ok_or_else(|| bad("planning needs a known caller"))?;
    let person = caller
        .acting_for
        .as_ref()
        .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
    if !args.as_object().is_some_and(|fields| {
        fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "instance" | "source" | "prefix" | "size_bytes" | "dry_run" | "fingerprints"
            )
        })
    }) {
        return Err(bad(
            "The Backblaze plan takes the approved source, prefix, size, dry-run and fingerprint options.",
        ));
    }
    let prefix = args
        .get("prefix")
        .map(|value| value.as_str().ok_or_else(|| bad("prefix must be text")))
        .transpose()?;
    let approved = connection(record)?.bind(prefix).map_err(bad)?;
    let creds = source_keys(record, env).await?;
    let listing = sample(&approved, &creds).await?;
    let count = listing.objects.len() as u64;
    let bytes = listing
        .objects
        .iter()
        .fold(0u64, |total, object| total.saturating_add(object.size));
    let sampled =
        json!({ "objects": count, "bytes": bytes, "first_page_only": listing.next.is_some() });
    if count == 0 {
        return Ok(unready(
            vec!["The approved prefix contains no objects.".into()],
            Value::Null,
            sampled,
        ));
    }
    let expected = args["size_bytes"].as_u64();
    if listing.next.is_some() && expected.is_none() {
        return Ok(unready(
            vec!["The sample is truncated. Provide size_bytes for the whole approved prefix, or choose a smaller prefix.".into()],
            Value::Null, sampled,
        ));
    }
    let total = expected.unwrap_or(bytes);
    if total < bytes {
        return Err(bad("size_bytes is below the sampled source size"));
    }
    let objects = ((u128::from(total) * u128::from(count)) / u128::from(bytes.max(1)))
        .max(u128::from(count))
        .min(u128::from(u64::MAX)) as u64;
    let estimate = estimate::estimate(Host::Cloudflare, total, objects, UNIQUE_SHARE);
    let max_minutes = (estimate.hours_high.mul_add(90.0, 5.0).ceil() as u64).clamp(5, 24 * 60);
    let resolved = match ready(cap, record, env, approved, max_minutes).await {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(unready(
                vec![error.to_string()],
                to_value(&estimate),
                sampled,
            ));
        }
    };
    let source = resolved.source;
    let SourceSpec::S3 { bucket, prefix, .. } = &source else {
        return Err(bad("Backblaze requires an S3 source"));
    };
    let nonce = pigeon_core::webauthn::random()?;
    let id = format!(
        "{}-b2-{}",
        today(),
        &pigeon_core::sign::hex(nonce.as_bytes())[..32]
    );
    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let lines = vec![
        format!("Read Backblaze bucket {bucket}, prefix {prefix:?}, with its saved read-only key."),
        format!(
            "Run one Cloudflare standard-4 container for at most {max_minutes} minutes, at runner revision {}.",
            resolved.revision
        ),
        format!(
            "{} in the private {} Data bucket; preserve archive containers, exact duplicates and portable manifests.",
            if dry_run {
                "Record decisions only"
            } else {
                "Write missing content-addressed payloads and provenance"
            },
            resolved.bucket.name
        ),
        format!(
            "Confirm container exit and store the terminal audit in {}.",
            resolved.execution.audit_bucket
        ),
    ];
    let job = JobSpec {
        run: id,
        instance: record.id.clone(),
        source,
        dest: DestSpec {
            endpoint: hosts::r2_endpoint(&resolved.account.id),
            bucket: resolved.bucket.name,
        },
        dry_run,
        keep_archives: true,
        fingerprints: args["fingerprints"].as_bool().unwrap_or(false),
        limits: Limits::default(),
        threads: 4,
        commit: resolved.revision,
    };
    Ingest::check_current(env, record).await?;
    let payload = payload(&job, &resolved.execution);
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest = pigeon_core::approval::payload_digest(&kind, &job.run, &payload);
    let made = directory(env, "approval.create", json!({
        "kind": kind.name(), "subject": job.run, "digest": digest,
        "summary": { "plan": lines, "estimate": estimate, "execution": resolved.execution },
        "payload": payload, "caller_id": caller.user.id, "approver_id": person.id, "pending_ms": PENDING_MS,
    })).await?;
    Ok(json!({
        "applies": true, "missing": [], "plan": lines, "sample": sampled, "estimate": estimate,
        "job": job, "approval": made["record"]["id"], "waits_until_ms": made["record"]["expires_ms"],
        "note": "Pigeon starts the Cloudflare container once after passkey approval. Check approvals_status for the result.",
    }))
}

#[derive(Deserialize, Serialize)]
struct ManagedRun {
    instance: String,
    execution: Execution,
    started_ms: i64,
    deadline_ms: i64,
    terminal: Option<Terminal>,
    audit_status: ExportStatus,
}

fn managed_key(run: &str) -> String {
    format!("data-ingest-backblaze:{run}")
}

pub(crate) fn managed(payload: &Value) -> bool {
    payload["adapter"] == ADAPTER
}

pub(crate) async fn start(
    cap: &Ingest,
    cx: &Context<'_>,
    args: &Value,
    approval: &str,
    approved: &Value,
) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| bad("starting needs a known caller"))?;
    let env = cx.env;
    let job: JobSpec = serde_json::from_value(approved["payload"]["job"].clone())?;
    let prior: Execution = serde_json::from_value(approved["payload"]["execution"].clone())?;
    if approved["subject"] != job.run
        || !container_job::valid_run(&job.run)
        || args
            .get("instance")
            .is_some_and(|instance| instance.as_str() != Some(&job.instance))
        || !(5..=24 * 60).contains(&prior.max_minutes)
    {
        return Err(bad("the approved Backblaze run or instance does not match"));
    }
    let person = caller
        .acting_for
        .as_ref()
        .ok_or_else(|| bad("starting needs a linked person"))?;
    crate::membership::require(
        env,
        person,
        Some(&job.instance),
        crate::membership::Role::Operator,
    )
    .await?;
    let record = instances::one(env, &job.instance)
        .await?
        .ok_or_else(|| bad("the approved instance no longer exists"))?;
    let SourceSpec::S3 {
        account: source_account,
        bucket: source_bucket,
        prefix,
        region,
        ..
    } = &job.source
    else {
        return Err(bad("the approved Backblaze run has no source bucket"));
    };
    let source = connection(&record)?.bind(Some(prefix)).map_err(bad)?;
    let resolved = ready(cap, &record, env, source, prior.max_minutes).await?;
    if resolved.execution != prior
        || resolved.revision != job.commit
        || resolved.source != job.source
        || resolved.bucket.name != job.dest.bucket
        || hosts::r2_endpoint(&resolved.account.id) != job.dest.endpoint
    {
        return Err(bad(
            "the approved Backblaze source, Data destination, audit target or runner changed; plan again",
        ));
    }
    // Confirm the freshly read key still works before spending the approval.
    sample(&job.source, &resolved.source_keys).await?;
    Ingest::check_current(env, &record).await?;
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest =
        pigeon_core::approval::payload_digest(&kind, &job.run, &payload(&job, &resolved.execution));
    let taken = directory(
        env,
        "approval.take",
        json!({
            "id": approval, "digest": digest, "caller_id": caller.user.id
        }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(bad(format!(
            "the approved Backblaze run does not start: {why}"
        )));
    }
    Ingest::check_current(env, &record).await?;
    r2::put(
        &resolved.account,
        &resolved.bucket,
        &job_key(&job.run),
        "application/json",
        serde_json::to_vec(&job)?,
    )
    .await?;
    let started_ms = now_ms();
    let deadline_ms = started_ms + (prior.max_minutes as i64 * 60_000);
    let record = RunRecord {
        run: job.run.clone(),
        instance: job.instance.clone(),
        host: "cloudflare".into(),
        source: source_account.clone(),
        bucket: source_bucket.clone(),
        region: region.clone(),
        vm: None,
        scaleway_volumes: Vec::new(),
        scaleway: None,
        vm_deleted: false,
        destination: Some(job.dest.clone()),
        data_worker: Some(prior.worker.clone()),
        approval: approval.to_string(),
        commit: job.commit.clone(),
        dry_run: job.dry_run,
        started_ms,
    };
    save_run(env, &record).await?;
    let mut runs = runs_of(env, &job.instance).await?;
    runs.push(job.run.clone());
    keep(
        env,
        &index_key(&job.instance),
        &serde_json::to_string(&runs)?,
    )
    .await?;
    let managed = ManagedRun {
        instance: job.instance.clone(),
        execution: prior.clone(),
        started_ms,
        deadline_ms,
        terminal: None,
        audit_status: ExportStatus::Pending,
    };
    keep(
        env,
        &managed_key(&job.run),
        &serde_json::to_string(&managed)?,
    )
    .await?;
    let body = serde_json::to_value(container_job::Start {
        request: StartRequest {
            job: job.clone(),
            source_keys: resolved.source_keys,
            dest_keys: resolved.dest_keys,
        },
        image_revision: Some(job.commit.clone()),
        image: Some(prior.image.clone()),
        deadline_ms: Some(deadline_ms),
        report_to_jobs: true,
    })?;
    match net::data_post(
        &prior.worker,
        protocol::START_PATH,
        &resolved.worker_key,
        now_ms(),
        &body,
    )
    .await
    {
        Ok(_) => Ok(json!({ "run": job.run, "host": "cloudflare", "state": "starting" })),
        Err(_) => Ok(json!({
            "run": job.run, "host": "cloudflare", "state": "launch_unconfirmed",
            "note": "The start response was lost or refused. This approval is spent; inspect the run and its Jobs audit before planning another.",
        })),
    }
}

async fn current_managed(
    env: &Env,
    instance: &str,
    run: &str,
) -> Result<Option<(String, ManagedRun)>> {
    let Some(text) = setting(env, &managed_key(run)).await? else {
        return Ok(None);
    };
    let state: ManagedRun = serde_json::from_str(&text)?;
    Ok((state.instance == instance).then_some((text, state)))
}

async fn replace(env: &Env, run: &str, previous: String, state: &ManagedRun) -> Result<bool> {
    let result = directory(env, "setting.replace", json!({
        "key": managed_key(run), "previous": previous, "value": serde_json::to_string(state)?, "ttl_ms": KEPT_MS,
    })).await?;
    Ok(result.as_bool().unwrap_or_default())
}

pub(crate) async fn status(env: &Env, instance: &str, run: &str) -> Result<Option<Value>> {
    Ok(current_managed(env, instance, run)
        .await?
        .map(|(_, state)| {
            json!({
                "started_ms": state.started_ms, "deadline_ms": state.deadline_ms,
                "terminal": state.terminal, "audit_status": state.audit_status,
            })
        }))
}

/// Requests cancellation on the recorded Data Worker. A stopped request and confirmed
/// container exit are distinct until the durable runtime publishes its terminal notice.
pub(crate) async fn stop(env: &Env, instance: &str, run: &str) -> Result<Option<Value>> {
    let Some((_, state)) = current_managed(env, instance, run).await? else {
        return Ok(None);
    };
    if state.terminal.is_some() {
        return Ok(Some(
            json!({ "run": run, "state": "ended", "audit_status": state.audit_status }),
        ));
    }
    let record = instances::one(env, instance)
        .await?
        .ok_or_else(|| bad("the ingest instance no longer exists"))?;
    let (url, key) = data_store::reach(env, &record).await?;
    if url != state.execution.worker {
        return Err(bad(
            "the Data Worker changed during the run; recover the original Worker before cancelling",
        ));
    }
    net::data_post(
        &url,
        protocol::STOP_PATH,
        &key,
        now_ms(),
        &json!({ "run": run }),
    )
    .await?;
    Ok(Some(json!({
        "run": run, "state": "cancel_requested",
        "note": "Jobs records cancellation after the Cloudflare container is confirmed gone.",
    })))
}

fn pipeline_terminal(terminal: &Terminal) -> PipelineTerminal {
    let elapsed_ms = terminal
        .finished_ms
        .saturating_sub(terminal.started_ms)
        .max(0) as u128;
    let cost = (elapsed_ms * MICROS_PER_HOUR / 3_600_000).min(u128::from(u64::MAX)) as u64;
    let counts = &terminal.counters;
    PipelineTerminal {
        run_id: terminal.run.clone(),
        started_ms: terminal.started_ms as u64,
        finished_ms: terminal.finished_ms as u64,
        outcome: match terminal.outcome {
            Outcome::Succeeded => PipelineOutcome::Succeeded,
            Outcome::Failed => PipelineOutcome::Failed,
            Outcome::Cancelled => PipelineOutcome::Cancelled,
            Outcome::TimedOut => PipelineOutcome::TimedOut,
        },
        cleanup: Some(Cleanup::Confirmed),
        counts: PipelineCounts {
            examined: counts.files.saturating_add(counts.members),
            stored: counts.uploaded,
            duplicates: counts.duplicates,
            failed: counts.failed,
            bytes_read: counts.bytes_read,
            bytes_written: counts.uploaded_bytes,
        },
        estimated_usd_micros: Some(cost),
    }
}

/// Signed notice from the instance's Data Worker. Only the first terminal record for the approved
/// run is accepted. A repeated notice retries the same content-addressed audit record.
pub async fn callback(env: &Env, host: &str, mut req: Request) -> Result<Response> {
    if req.method() != Method::Post {
        return Response::error("method not allowed", 405);
    }
    let Some(signature) = req.headers().get(protocol::SIGNATURE_HEADER)? else {
        return Response::error("unauthorized", 401);
    };
    let body = req.bytes().await?;
    if body.len() > MAX_CALLBACK_BYTES {
        return Response::error("notice is too large", 413);
    }
    let Ok(notice) = serde_json::from_slice::<TerminalNotice>(&body) else {
        return Response::error("invalid notice", 400);
    };
    let terminal = notice.terminal;
    if !container_job::valid_run(&terminal.run) {
        return Response::error("invalid run", 400);
    }
    let Some((mut previous, mut state)) =
        current_managed(env, &notice.instance, &terminal.run).await?
    else {
        return Response::error("unauthorized", 401);
    };
    let Some(record) = instances::one(env, &notice.instance).await? else {
        return Response::error("unauthorized", 401);
    };
    let Ok((url, key)) = data_store::reach(env, &record).await else {
        return Response::error("unauthorized", 401);
    };
    let now = now_ms();
    if url != state.execution.worker
        || protocol::verify(
            &signature,
            "POST",
            container_job::TERMINAL_PATH,
            &body,
            key.as_bytes(),
            now,
        )
        .is_err()
    {
        return Response::error("unauthorized", 401);
    }
    if terminal.finished_ms < terminal.started_ms
        || terminal.finished_ms > now.saturating_add(protocol::MAX_SKEW_MS)
        || terminal.started_ms < state.started_ms.saturating_sub(protocol::MAX_SKEW_MS)
        || terminal.started_ms > state.deadline_ms
    {
        return Response::error("invalid terminal timestamps", 400);
    }
    if let Some(existing) = state.terminal.as_ref() {
        if existing != &terminal {
            return Response::error("terminal result already recorded", 409);
        }
        if state.audit_status == ExportStatus::Stored {
            return Ok(Response::empty()?.with_status(204));
        }
    } else {
        state.terminal = Some(terminal.clone());
        if !replace(env, &terminal.run, previous, &state).await? {
            return Response::error("retry terminal notice", 503);
        }
        previous = serde_json::to_string(&state)?;
    }
    state.audit_status = jobs_audit::export_pipeline(
        env,
        host,
        &notice.instance,
        (&state.execution.account, &state.execution.audit_bucket),
        PipelineKind::BackblazeIngest,
        &pipeline_terminal(&terminal),
    )
    .await;
    let stored = state.audit_status == ExportStatus::Stored;
    if !replace(env, &terminal.run, previous, &state).await? || !stored {
        return Response::error("retry terminal audit", 503);
    }
    Ok(Response::empty()?.with_status(204))
}

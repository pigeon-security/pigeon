//! Passkey approval, bootstrap, and scheduled cleanup for a Spaces data ingest.

use pigeon_cloud::{digitalocean_ingest as provider, transport::FetchTransport};
use pigeon_core::{
    approval::{Kind, payload_digest},
    oauth, r2, s3,
    sign::hex,
    webauthn,
};
use pigeon_ingest::{
    archive::Limits,
    audit::{Cleanup, PipelineCounts, PipelineKind, PipelineOutcome, PipelineTerminal},
    estimate::{Host, estimate},
    manifest::{self, DestSpec, JobSpec, Report, RunState, SourceSpec},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use worker::{Env, Method, Request, Response, Result};

use super::{KEPT_MS, KIND, RunRecord, UNIQUE_SHARE, bad, hosts, secret_value, setting, unready};
use crate::{
    Context, data_store,
    digitalocean::{health, spaces, team},
    instances::{self, record::Instance},
    jobs_audit::{self, ExportStatus},
    users::directory,
};

pub const BOOT_PATH: &str = "/internal/data-ingest/digitalocean-bootstrap";
const BOOTSTRAP: &str = include_str!("digitalocean/bootstrap.sh");
const ADAPTER: &str = "digitalocean";
const PREFIX: &str = "do-ingest-run:";
const MINUTE_MS: u64 = 60_000;
const VOLUME_RATE_USD_GIB_MONTH: f64 = 0.1;
const DROPLET_RATE_USD_HOUR: f64 = 0.071;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Planned {
    adapter: String,
    job: JobSpec,
    account: String,
    team: String,
    revision: u64,
    audit_bucket: String,
    volume_gib: u16,
    spool_bytes: u64,
    max_minutes: u16,
    bootstrap: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    QueueVolume,
    FindVolume,
    QueueDroplet,
    FindDroplet,
    Running,
    CleanupDroplet,
    CleanupVolume,
    Done,
}

#[derive(Clone, Deserialize, Serialize)]
struct Run {
    plan: Planned,
    approval: String,
    credential: Option<health::Credential>,
    started_ms: u64,
    deadline_ms: u64,
    next_ms: u64,
    lease_until_ms: u64,
    stage: Stage,
    volume: Option<String>,
    droplet: Option<u64>,
    // An uncertain create is reconciled and never repeated.
    volume_attempt_ms: Option<u64>,
    droplet_attempt_ms: Option<u64>,
    volume_deleted_ms: Option<u64>,
    droplet_deleted_ms: Option<u64>,
    boot_hash: Option<String>,
    outcome: Option<PipelineOutcome>,
    counts: Option<PipelineCounts>,
    terminal: Option<PipelineTerminal>,
    audit: ExportStatus,
    retrying: bool,
}

fn now() -> u64 {
    worker::Date::now().as_millis()
}

fn key(run: &str) -> String {
    format!("{PREFIX}{run}")
}

fn valid_run_id(run: &str) -> bool {
    run.len() == 32 && run.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn bootstrap_policy() -> String {
    oauth::hash(&format!(
        "{BOOTSTRAP}\0{}\0{}\0{}",
        provider::SIZE,
        provider::IMAGE,
        provider::VOLUME_FILESYSTEM,
    ))
}

fn sample_size(listing: &s3::Listing, args: &Value) -> (u64, u64, Value) {
    let count = listing.objects.len() as u64;
    let bytes = listing.objects.iter().map(|row| row.size).sum::<u64>();
    let total = args["size_bytes"].as_u64().unwrap_or(bytes);
    let objects = if total == bytes {
        count
    } else {
        ((u128::from(total) * u128::from(count)) / u128::from(bytes.max(1)))
            .min(u128::from(u64::MAX)) as u64
    };
    (
        total,
        objects,
        json!({"objects":count,"bytes":bytes,"first_page_only":listing.next.is_some()}),
    )
}

fn range(args: &Value, field: &str, default: u16, minimum: u16, maximum: u16) -> Result<u16> {
    match args.get(field) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| (minimum..=maximum).contains(value))
            .ok_or_else(|| {
                bad(format!(
                    "{field} must be an integer from {minimum} to {maximum}"
                ))
            }),
    }
}

fn bound(
    record: &Instance,
    job: &JobSpec,
    account: &str,
    approved_team: &str,
    audit_bucket: &str,
) -> bool {
    let Some(binding) = spaces::binding(record) else {
        return false;
    };
    super::data_enabled(record)
        && super::linked_id(record).as_deref() == Some(account)
        && team(record) == Some(approved_team)
        && jobs_audit::configured_bucket(record) == Some(audit_bucket)
        && job.instance == record.id
        && job.dest.bucket == data_store::store_name(record)
        && job.dest.endpoint == hosts::r2_endpoint(account)
        && matches!(&job.source, SourceSpec::S3 { account, endpoint, region, bucket, prefix }
            if account == "digitalocean:instance"
                && endpoint == &format!("https://{}.digitaloceanspaces.com", binding.region)
                && region == &binding.region
                && bucket == &binding.bucket
                && spaces::allows_prefix(&binding.prefix, prefix))
}

fn volume_bytes(gib: u16) -> u64 {
    // Reserve 20% of the provisioned GiB for filesystem overhead.
    u64::from(gib) * (1 << 30) * 4 / 5
}

fn estimated_cost(droplet_ms: u64, volume_ms: u64, volume_gib: u16) -> f64 {
    DROPLET_RATE_USD_HOUR * droplet_ms as f64 / 3_600_000.0
        + VOLUME_RATE_USD_GIB_MONTH * f64::from(volume_gib) * volume_ms as f64
            / (730.0 * 3_600_000.0)
}

pub(super) async fn plan(
    cx: &Context<'_>,
    host: &str,
    record: &Instance,
    args: &Value,
) -> Result<Value> {
    let Some(binding) = spaces::binding(record) else {
        return Ok(unready(
            vec!["Configure the approved DigitalOcean Spaces connection first.".into()],
            Value::Null,
            Value::Null,
        ));
    };
    if !super::data_enabled(record) {
        return Err(bad("Data and Data ingest must both be enabled."));
    }
    if args
        .get("source")
        .is_some_and(|source| source.as_str() != Some(ADAPTER))
        || args.get("bucket").is_some()
        || args
            .get("keep_archives")
            .is_some_and(|keep| keep.as_bool() != Some(true))
    {
        return Err(bad(
            "The approved Spaces bucket is fixed and archive containers are retained.",
        ));
    }
    let prefix = super::spaces_prefix(&binding.prefix, args).map_err(bad)?;
    let volume_gib = range(args, "scratch_gib", 32, 8, 1024)?;
    let max_minutes = range(args, "max_minutes", 30, 10, 360)?;
    // Reserve one half of the scratch budget for the source archive and processing overhead.
    let maximum_inflated = (volume_bytes(volume_gib) / (2 << 30)) as u16;
    let inflated_gib = range(
        args,
        "archive_inflated_gib",
        maximum_inflated.min(32),
        1,
        maximum_inflated,
    )?;
    let member_gib = range(
        args,
        "archive_member_gib",
        inflated_gib.min(8),
        1,
        inflated_gib,
    )?;
    let limits = Limits {
        max_total_bytes: u64::from(inflated_gib) << 30,
        max_member_bytes: u64::from(member_gib) << 30,
        ..Limits::default()
    };
    let run_id = hex(&Sha256::digest(webauthn::random()?.as_bytes()))[..32].to_owned();
    let requested_since = u64::from(max_minutes) * MINUTE_MS;
    let sample = spaces::ingest_sample(record, cx.env, prefix).await;
    let mut missing = Vec::new();
    let (est, sample) = match sample {
        Ok(listing) => {
            let (bytes, objects, sample) = sample_size(&listing, args);
            if sample["objects"] == 0 {
                missing.push("The chosen Spaces prefix contains no objects.".into());
            }
            let mut est =
                serde_json::to_value(estimate(Host::DigitalOcean, bytes, objects, UNIQUE_SHARE))?;
            est["compute_and_volume_at_deadline_usd"] =
                json!(estimated_cost(requested_since, requested_since, volume_gib));
            if let Some(assumptions) = est["assumptions"].as_array_mut() {
                assumptions.push(json!("Archive expansion can add egress and R2 writes. Cleanup and build time can extend the observed resource lifetime."));
            }
            (est, sample)
        }
        Err(_) => {
            missing.push(
                "Pigeon could not list the approved prefix with the saved read-only Spaces key."
                    .into(),
            );
            (Value::Null, Value::Null)
        }
    };
    let Some(account) = super::linked_id(record) else {
        missing.push("Link the instance to its approved Cloudflare account.".into());
        return Ok(unready(missing, est, sample));
    };
    let Some(audit_bucket) = jobs_audit::configured_bucket(record) else {
        missing.push("Configure the private Jobs audit bucket before provisioning compute.".into());
        return Ok(unready(missing, est, sample));
    };
    if data_store::reach(cx.env, record).await.is_err() {
        missing.push("Provision the Data collection with the approved instance plan first.".into());
    }
    let bucket = data_bucket(record);
    let dest = match crate::linked::account_for_bound(cx.env, host, &record.id, &account).await {
        Ok(link) if r2::require_private_bucket(&link, &bucket).await.is_ok() => Some(link),
        _ => {
            missing.push("Pigeon could not verify the instance's private R2 data bucket.".into());
            None
        }
    };
    if let Some(link) = &dest
        && r2::require_private_bucket(link, &named_bucket(audit_bucket))
            .await
            .is_err()
    {
        missing.push("Pigeon could not verify the private Jobs audit bucket.".into());
    }
    if bucket.name == audit_bucket {
        missing.push("The data and Jobs audit buckets must be distinct.".into());
    }
    if check_destination(cx.env, record, &account).await.is_err() {
        missing.push("Configure the bucket-scoped R2 credential for the Data runner.".into());
    }
    match health::for_ingest(cx.env, record).await {
        Ok(credential) => {
            let pat = health::open(cx.env, &record.id, &credential).await?;
            let target = provider::Target::new(&run_id, &binding.region)?;
            if provider::find_volume(&FetchTransport, &pat, &target)
                .await
                .is_err()
            {
                missing
                    .push("The saved compute key cannot read DigitalOcean Block Storage.".into());
            }
        }
        Err(_) => {
            missing.push("Configure the instance's DigitalOcean compute credential first.".into())
        }
    }
    let commit = match super::Ingest::commit(args) {
        Ok(commit) => Some(commit),
        Err(error) => {
            missing.push(error);
            None
        }
    };
    let (Some(commit), Some(team)) = (commit, team(record)) else {
        return Ok(unready(missing, est, sample));
    };
    if !missing.is_empty() {
        return Ok(unready(missing, est, sample));
    }

    let dry_run = args["dry_run"].as_bool().unwrap_or(true);
    let job = JobSpec {
        run: run_id,
        instance: record.id.clone(),
        source: SourceSpec::S3 {
            account: "digitalocean:instance".into(),
            endpoint: format!("https://{}.digitaloceanspaces.com", binding.region),
            region: binding.region.clone(),
            bucket: binding.bucket.clone(),
            prefix: prefix.into(),
        },
        dest: DestSpec {
            endpoint: hosts::r2_endpoint(&account),
            bucket: bucket.name,
        },
        dry_run,
        keep_archives: true,
        fingerprints: args["fingerprints"].as_bool().unwrap_or(false),
        limits,
        threads: 8,
        commit,
    };
    let approval = Planned {
        adapter: ADAPTER.into(),
        job,
        account,
        team: team.into(),
        revision: record.version,
        audit_bucket: audit_bucket.into(),
        volume_gib,
        spool_bytes: volume_bytes(volume_gib),
        max_minutes,
        bootstrap: bootstrap_policy(),
    };
    let summary = json!({
        "instance": record.id,
        "plan":[
            format!("Read Spaces {} in {} under prefix {}.",binding.bucket,binding.region,prefix),
            format!("Run one {} Droplet on {} with {} GiB attached scratch for at most {} minutes before cleanup.",provider::SIZE,provider::IMAGE,volume_gib,max_minutes),
            format!("Build commit {} and write run records to {}.",approval.job.commit,approval.job.dest.bucket),
            format!("Limit ZIP inflation to {inflated_gib} GiB per archive tree and {member_gib} GiB per member."),
            if dry_run {"Dry run. Read and report without writing payloads."} else {"Copy exact unique payloads, archive containers, and extracted members."},
            "Delete the owned Droplet and then the detached scratch volume. Confirm absence and export terminal audit."
        ],
        "estimate":est,
    });
    let caller = cx
        .caller
        .ok_or_else(|| bad("Planning requires a known caller."))?;
    let person = caller
        .acting_for
        .as_ref()
        .ok_or_else(|| bad("Planning requires a linked person."))?;
    let payload = serde_json::to_value(&approval)?;
    let kind = Kind::Other(KIND.into());
    let digest = payload_digest(&kind, &approval.job.run, &payload);
    let made = directory(cx.env, "approval.create", json!({
        "kind":KIND,"subject":approval.job.run,"digest":digest,"summary":summary,
        "payload":payload,"caller_id":caller.user.id,"approver_id":person.id,"pending_ms":super::PENDING_MS,
    })).await?;
    Ok(json!({
        "applies":true,"job":approval.job,"estimate":est,"sample":sample,"plan":summary["plan"],
        "approval":made["record"]["id"],"waits_until_ms":made["record"]["expires_ms"],
        "note":"Passkey approval starts the scheduled run once. Follow it with approvals_status and data_ingest_status.",
    }))
}

fn named_bucket(name: &str) -> r2::Bucket {
    r2::Bucket {
        name: name.to_owned(),
        jurisdiction: None,
    }
}

fn data_bucket(record: &Instance) -> r2::Bucket {
    named_bucket(&data_store::store_name(record))
}

async fn destination_keys(env: &Env, record: &Instance) -> Result<s3::Keys> {
    let key = secret_value(env, &hosts::r2_slot(&record.id))
        .await
        .ok_or_else(|| bad("The Data bucket runner credential is missing."))?;
    s3::parse_keys(&key)
}

async fn check_destination(env: &Env, record: &Instance, account: &str) -> Result<()> {
    let keys = destination_keys(env, record).await?;
    let endpoint = s3::Endpoint {
        base: hosts::r2_endpoint(account),
        region: "auto".into(),
    };
    s3::send(
        Method::Head,
        &endpoint,
        &s3::object_path(&data_store::store_name(record), ""),
        &[],
        &[],
        &keys,
    )
    .await?;
    Ok(())
}

fn keys_string(keys: s3::Keys) -> String {
    format!("{}:{}", keys.access_key_id, keys.secret_access_key)
}

pub(super) fn is_plan(raw: &Value) -> bool {
    raw["adapter"].as_str() == Some(ADAPTER)
}

async fn current_binding(env: &Env, p: &Planned) -> Result<Option<Instance>> {
    Ok(instances::one(env, &p.job.instance)
        .await?
        .filter(|record| bound(record, &p.job, &p.account, &p.team, &p.audit_bucket)))
}

async fn current(env: &Env, p: &Planned) -> Result<Instance> {
    current_binding(env, p)
        .await?
        .ok_or_else(|| bad("The approved Spaces or Data scope changed. Plan the run again."))
}

pub(super) async fn start(
    cx: &Context<'_>,
    host: &str,
    id: &str,
    approval: &Value,
    args: &Value,
) -> Result<Value> {
    let p: Planned = serde_json::from_value(approval["payload"].clone())?;
    let caller = cx
        .caller
        .ok_or_else(|| bad("Starting requires a known caller."))?;
    if !valid_run_id(&p.job.run)
        || approval["subject"].as_str() != Some(&p.job.run)
        || p.bootstrap != bootstrap_policy()
        || args
            .get("instance")
            .is_some_and(|instance| instance.as_str() != Some(&p.job.instance))
    {
        return Err(bad("The ingest start does not match the approved scope."));
    }
    crate::membership::check_tool(
        cx,
        super::NAME,
        "data_ingest_start",
        &json!({"instance":p.job.instance}),
    )
    .await?;
    let env = cx.env;
    let record = current(env, &p).await?;
    if record.version != p.revision {
        return Err(bad(
            "The approved instance settings changed. Plan the run again.",
        ));
    }
    let link = crate::linked::account_for_bound(env, host, &record.id, &p.account).await?;
    let credential = health::for_ingest(env, &record).await?;
    let binding = spaces::binding(&record).ok_or_else(|| bad("The Spaces binding changed."))?;
    spaces::open_keys(&record, env, binding).await?;
    check_destination(env, &record, &p.account).await?;
    let bucket = data_bucket(&record);
    r2::require_private_bucket(&link, &bucket).await?;
    r2::require_private_bucket(&link, &named_bucket(&p.audit_bucket)).await?;
    let kind = Kind::Other(KIND.into());
    let digest = payload_digest(&kind, &p.job.run, &approval["payload"]);
    let taken = directory(
        env,
        "approval.take",
        json!({
            "id":id,"digest":digest,"caller_id":caller.user.id,
        }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(bad(format!("The ingest plan did not start. {why}")));
    }
    if current(env, &p).await?.version != p.revision {
        return Err(bad(
            "The approved instance settings changed. Plan the run again.",
        ));
    }
    r2::put(
        &link,
        &bucket,
        &manifest::job_key(&p.job.run),
        "application/json",
        serde_json::to_vec(&p.job)?,
    )
    .await?;
    let time = now();
    let record = Run {
        approval: id.into(),
        credential: Some(credential),
        started_ms: time,
        next_ms: time,
        deadline_ms: time + u64::from(p.max_minutes) * MINUTE_MS,
        lease_until_ms: 0,
        stage: Stage::QueueVolume,
        volume: None,
        droplet: None,
        volume_attempt_ms: None,
        droplet_attempt_ms: None,
        volume_deleted_ms: None,
        droplet_deleted_ms: None,
        boot_hash: None,
        outcome: None,
        counts: None,
        terminal: None,
        audit: ExportStatus::Pending,
        retrying: false,
        plan: p,
    };
    super::keep(
        env,
        &key(&record.plan.job.run),
        &serde_json::to_string(&record)?,
    )
    .await?;
    Ok(
        json!({"instance":record.plan.job.instance,"run":record.plan.job.run,
        "stage":record.stage,"deadline_ms":record.deadline_ms}),
    )
}

async fn load(env: &Env, id: &str) -> Result<Option<(String, Run)>> {
    if !valid_run_id(id) {
        return Ok(None);
    }
    setting(env, &key(id))
        .await?
        .map(|raw| {
            let job = serde_json::from_str(&raw)?;
            Ok((raw, job))
        })
        .transpose()
}

async fn replace(env: &Env, before: &str, job: &Run) -> Result<Option<String>> {
    let raw = serde_json::to_string(job)?;
    let written = directory(
        env,
        "setting.replace",
        json!({
            "key":key(&job.plan.job.run),"previous":before,"value":raw,"ttl_ms":KEPT_MS,
        }),
    )
    .await?
        == true;
    Ok(written.then_some(raw))
}

fn checked_job_for(job: &Run, instance: &str) -> Result<()> {
    if job.plan.job.instance != instance {
        return Err(bad("The ingest run belongs to a different instance."));
    }
    Ok(())
}

fn run_record(job: &Run) -> RunRecord {
    let p = &job.plan.job;
    let (bucket, region) = match &p.source {
        SourceSpec::S3 { bucket, region, .. } => (bucket.clone(), region.clone()),
        _ => (String::new(), String::new()),
    };
    RunRecord {
        run: p.run.clone(),
        instance: p.instance.clone(),
        host: ADAPTER.into(),
        source: "digitalocean:instance".into(),
        bucket,
        region,
        vm: job.droplet.map(|id| id.to_string()),
        scaleway_volumes: Vec::new(),
        scaleway: None,
        vm_deleted: job.droplet_deleted_ms.is_some() || job.stage == Stage::Done,
        destination: Some(p.dest.clone()),
        data_worker: None,
        approval: job.approval.clone(),
        commit: p.commit.clone(),
        dry_run: p.dry_run,
        started_ms: job.started_ms as i64,
    }
}

pub(super) async fn runs(env: &Env, instance: &str) -> Result<Vec<RunRecord>> {
    let rows: Vec<String> = serde_json::from_value(
        directory(
            env,
            "data-ingest.droplet.list",
            json!({"instance":instance}),
        )
        .await?,
    )?;
    rows.into_iter()
        .map(|raw| {
            let job: Run = serde_json::from_str(&raw)?;
            checked_job_for(&job, instance)?;
            Ok(run_record(&job))
        })
        .collect()
}

async fn read_document(env: &Env, host: &str, job: &Run, path: &str) -> Result<Option<Vec<u8>>> {
    let link =
        crate::linked::account_for_bound(env, host, &job.plan.job.instance, &job.plan.account)
            .await?;
    r2::get(&link, &named_bucket(&job.plan.job.dest.bucket), path).await
}

pub(super) async fn read(
    env: &Env,
    host: &str,
    instance: &str,
    id: &str,
    report: bool,
) -> Result<Option<Value>> {
    let Some((_, job)) = load(env, id).await? else {
        return Ok(None);
    };
    checked_job_for(&job, instance)?;
    let path = if report {
        manifest::report_key(id)
    } else {
        manifest::status_key(id)
    };
    let result = read_document(env, host, &job, &path).await;
    let details = if report {
        result?
    } else {
        result.ok().flatten()
    }
    .map(|bytes| serde_json::from_slice::<Value>(&bytes))
    .transpose()?;
    if report {
        let mut data = details.unwrap_or_else(|| json!({"run":id,"report":null}));
        super::cap_report(&mut data);
        return Ok(Some(data));
    }
    Ok(Some(json!({
        "run":run_record(&job),"stage":job.stage,"status":details,"deadline_ms":job.deadline_ms,
        "volume":job.volume,"droplet_deleted_ms":job.droplet_deleted_ms,"volume_deleted_ms":job.volume_deleted_ms,
        "terminal":job.terminal,"audit_status":job.audit,"retrying":job.retrying,
    })))
}

pub(super) async fn cancel(env: &Env, instance: &str, id: &str) -> Result<Option<Value>> {
    let Some((prior, mut job)) = load(env, id).await? else {
        return Ok(None);
    };
    checked_job_for(&job, instance)?;
    if job.lease_until_ms > now() {
        return Err(bad(
            "The maintenance tick is updating this run. Try again shortly.",
        ));
    }
    if job.outcome.is_none() {
        cleanup(&mut job, PipelineOutcome::Cancelled);
        job.next_ms = now();
        if replace(env, &prior, &job).await?.is_none() {
            return Err(bad(
                "The ingest run changed while cancelling. Try again shortly.",
            ));
        }
    }
    Ok(Some(
        json!({"run":id,"stage":job.stage,"outcome":job.outcome}),
    ))
}

fn bootstrap_script(
    job: &Run,
    target: &provider::Target<'_>,
    host: &str,
    boot: &str,
) -> Result<String> {
    if !hosts::is_commit(&job.plan.job.commit)
        || !valid_run_id(&job.plan.job.run)
        || !host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        || !boot
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(bad("Invalid ingest bootstrap binding."));
    }
    let body = BOOTSTRAP
        .strip_prefix("#!/bin/bash\n")
        .ok_or_else(|| bad("Invalid ingest bootstrap template."))?;
    Ok(format!(
        "#!/bin/bash\nRUN='{}'\nVOLUME_NAME='{}'\nCOMMIT='{}'\nBOOT_TOKEN='{}'\nBOOT_URL='https://{}{}'\nDEADLINE_SECONDS='{}'\nSPOOL_BYTES='{}'\n{}",
        job.plan.job.run,
        target.name(),
        job.plan.job.commit,
        boot,
        host,
        BOOT_PATH,
        job.deadline_ms / 1_000,
        job.plan.spool_bytes,
        body,
    ))
}

pub async fn bootstrap(env: &Env, host: &str, mut request: Request) -> Result<Response> {
    let auth = request.headers().get("Authorization")?;
    let Some(token) = auth
        .as_deref()
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Response::error("Unauthorized", 401);
    };
    if !(32..=128).contains(&token.len())
        || request
            .headers()
            .get("Content-Length")?
            .and_then(|len| len.parse::<u32>().ok())
            .is_none_or(|len| len > 100)
    {
        return Response::error("Unauthorized", 401);
    }
    let body: Value = request.json().await?;
    let Some(id) = body["run"].as_str() else {
        return Response::error("Unauthorized", 401);
    };
    let Some((prior, mut job)) = load(env, id).await? else {
        return Response::error("Unauthorized", 401);
    };
    let digest = oauth::hash(token);
    if job.boot_hash.as_deref() != Some(&digest)
        || now() >= job.deadline_ms
        || !matches!(job.stage, Stage::FindDroplet | Stage::Running)
        || job.outcome.is_some()
    {
        return Response::error("Unauthorized", 401);
    }
    if job.stage != Stage::Running || job.lease_until_ms > now() {
        return Response::error("Retry provisioned runner", 409);
    }
    let record = current(env, &job.plan).await?;
    let binding = spaces::binding(&record).ok_or_else(|| bad("The Spaces binding changed."))?;
    let Some(id) = job.droplet else {
        return Response::error("Retry provisioned runner", 409);
    };
    let Some(peer) = request
        .headers()
        .get("CF-Connecting-IP")?
        .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
    else {
        return Response::error("Unauthorized", 401);
    };
    let credential = job
        .credential
        .as_ref()
        .ok_or_else(|| bad("The compute credential is unavailable."))?;
    let pat = health::open(env, &job.plan.job.instance, credential).await?;
    let target = provider::Target::new(&job.plan.job.run, &binding.region)?;
    match provider::public_ipv4(&FetchTransport, &pat, &target, id).await? {
        None => return Response::error("Retry provisioned runner", 409),
        Some(address) if address != peer => return Response::error("Unauthorized", 401),
        Some(_) => {}
    }
    let link = crate::linked::account_for_bound(env, host, &record.id, &job.plan.account).await?;
    r2::require_private_bucket(&link, &data_bucket(&record)).await?;
    let source = spaces::open_keys(&record, env, binding).await?;
    let destination = destination_keys(env, &record).await?;
    job.boot_hash = None;
    if replace(env, &prior, &job).await?.is_none() {
        return Response::error("Retry provisioned runner", 409);
    }
    let mut response = Response::from_json(&json!({
        "job":job.plan.job,"SOURCE_KEYS":keys_string(source),"DEST_KEYS":keys_string(destination),
    }))?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

fn cleanup(job: &mut Run, outcome: PipelineOutcome) {
    job.boot_hash = None;
    job.outcome = Some(outcome);
    job.stage = Stage::CleanupDroplet;
}

fn elapsed(attempted: Option<u64>, removed: Option<u64>) -> u64 {
    removed
        .zip(attempted)
        .map_or(0, |(end, start)| end.saturating_sub(start))
}

fn counts(report: &Report) -> PipelineCounts {
    let c = &report.counters;
    PipelineCounts {
        examined: c.files.saturating_add(c.members),
        stored: c.uploaded,
        duplicates: c.duplicates,
        failed: c.failed,
        bytes_read: c.bytes_read,
        bytes_written: c.uploaded_bytes,
    }
}

fn terminal(job: &Run, time: u64) -> PipelineTerminal {
    let cost = estimated_cost(
        elapsed(job.droplet_attempt_ms, job.droplet_deleted_ms),
        elapsed(job.volume_attempt_ms, job.volume_deleted_ms),
        job.plan.volume_gib,
    );
    PipelineTerminal {
        run_id: job.plan.job.run.clone(),
        started_ms: job.started_ms,
        finished_ms: time,
        outcome: job.outcome.unwrap_or(PipelineOutcome::Failed),
        cleanup: Some(Cleanup::Confirmed),
        counts: job.counts.clone().unwrap_or_default(),
        estimated_usd_micros: Some((cost * 1_000_000.0).ceil() as u64),
    }
}

async fn read_report(env: &Env, host: &str, job: &Run) -> Result<Option<Report>> {
    let bytes = read_document(env, host, job, &manifest::report_key(&job.plan.job.run)).await?;
    let report: Option<Report> = bytes
        .map(|bytes| serde_json::from_slice(&bytes))
        .transpose()?;
    Ok(report
        .filter(|report| report.run == job.plan.job.run && report.dry_run == job.plan.job.dry_run))
}

async fn step(env: &Env, host: &str, prior: &mut String, job: &mut Run) -> Result<()> {
    let time = now();
    if time >= job.deadline_ms && job.outcome.is_none() {
        cleanup(job, PipelineOutcome::TimedOut);
    }
    if job.stage == Stage::Done {
        if let Some(terminal) = &job.terminal {
            job.audit = jobs_audit::export_pipeline(
                env,
                host,
                &job.plan.job.instance,
                (&job.plan.account, &job.plan.audit_bucket),
                PipelineKind::DigitaloceanIngest,
                terminal,
            )
            .await;
            if job.audit == ExportStatus::Stored {
                job.next_ms = 0;
            }
        }
        return Ok(());
    }
    if job.outcome.is_none() && current_binding(env, &job.plan).await?.is_none() {
        cleanup(job, PipelineOutcome::Failed);
    }
    if matches!(job.stage, Stage::QueueVolume | Stage::QueueDroplet)
        && job.plan.bootstrap != bootstrap_policy()
    {
        cleanup(job, PipelineOutcome::Failed);
    }
    let region = match &job.plan.job.source {
        SourceSpec::S3 { region, .. } => region.clone(),
        _ => return Err(bad("The DigitalOcean run does not have a Spaces source.")),
    };
    let target = provider::Target::new(&job.plan.job.run, &region)?;
    let credential = job
        .credential
        .as_ref()
        .ok_or_else(|| bad("The compute credential is unavailable."))?;
    let pat = health::open(env, &job.plan.job.instance, credential).await?;
    if now() >= job.deadline_ms && job.outcome.is_none() {
        cleanup(job, PipelineOutcome::TimedOut);
    }
    match job.stage {
        Stage::QueueVolume => {
            job.stage = Stage::FindVolume;
            job.volume_attempt_ms = Some(time);
            let Some(armed) = replace(env, prior, job).await? else {
                return Ok(());
            };
            *prior = armed;
            match provider::create_volume(&FetchTransport, &pat, &target, job.plan.volume_gib).await
            {
                provider::Created::Accepted(id) => {
                    job.volume = Some(id);
                    job.stage = Stage::QueueDroplet;
                }
                provider::Created::Rejected(_) => {
                    job.volume_attempt_ms = None;
                    cleanup(job, PipelineOutcome::Failed);
                }
                provider::Created::Unknown => {}
            }
        }
        Stage::FindVolume => {
            if let Some(volume) = provider::find_volume(&FetchTransport, &pat, &target).await? {
                job.volume = Some(volume);
                job.stage = Stage::QueueDroplet;
            }
        }
        Stage::QueueDroplet => {
            let volume = job
                .volume
                .clone()
                .ok_or_else(|| bad("Ingest scratch was not created."))?;
            let boot = webauthn::random()?;
            let script = bootstrap_script(job, &target, host, &boot)?;
            job.stage = Stage::FindDroplet;
            job.droplet_attempt_ms = Some(time);
            job.boot_hash = Some(oauth::hash(&boot));
            let Some(armed) = replace(env, prior, job).await? else {
                return Ok(());
            };
            *prior = armed;
            match provider::create_droplet(&FetchTransport, &pat, &target, &volume, &script).await {
                provider::Created::Accepted(id) => {
                    job.droplet = Some(id);
                    job.stage = Stage::Running;
                }
                provider::Created::Rejected(_) => {
                    job.droplet_attempt_ms = None;
                    cleanup(job, PipelineOutcome::Failed);
                }
                provider::Created::Unknown => {}
            }
        }
        Stage::FindDroplet => {
            if let Some(id) = provider::find_droplet(&FetchTransport, &pat, &target).await? {
                job.droplet = Some(id);
                job.stage = Stage::Running;
            }
        }
        Stage::Running => {
            if let Some(report) = read_report(env, host, job).await? {
                let outcome = match report.state {
                    RunState::Done => Some(PipelineOutcome::Succeeded),
                    RunState::Failed => Some(PipelineOutcome::Failed),
                    RunState::Stopped => Some(PipelineOutcome::Cancelled),
                    _ => None,
                };
                if let Some(outcome) = outcome {
                    job.counts = Some(counts(&report));
                    cleanup(job, outcome);
                }
            }
        }
        Stage::CleanupDroplet => {
            if job.droplet.is_none() && job.droplet_attempt_ms.is_some() {
                job.droplet = provider::find_droplet(&FetchTransport, &pat, &target).await?;
                if job.droplet.is_none() {
                    return Ok(());
                }
            }
            if let Some(id) = job.droplet {
                if provider::remove_droplet(&FetchTransport, &pat, &target, id).await?
                    != provider::Removal::Absent
                {
                    return Ok(());
                }
                job.droplet_deleted_ms = Some(now());
            }
            job.stage = Stage::CleanupVolume;
        }
        Stage::CleanupVolume => {
            if job.volume.is_none() && job.volume_attempt_ms.is_some() {
                job.volume = provider::find_volume(&FetchTransport, &pat, &target).await?;
                if job.volume.is_none() {
                    return Ok(());
                }
            }
            if let Some(id) = &job.volume {
                if provider::remove_volume(&FetchTransport, &pat, &target, id).await?
                    != provider::Removal::Absent
                {
                    return Ok(());
                }
                job.volume_deleted_ms = Some(now());
            }
            if job.counts.is_none()
                && let Ok(Some(report)) = read_report(env, host, job).await
            {
                job.counts = Some(counts(&report));
            }
            job.terminal = Some(terminal(job, now()));
            job.stage = Stage::Done;
            job.credential = None;
        }
        Stage::Done => {}
    }
    Ok(())
}

/// Every scheduled tick claims a bounded number of due jobs before provider I/O.
pub async fn reap(env: &Env, host: &str) -> Result<()> {
    let rows: Vec<String> =
        serde_json::from_value(directory(env, "data-ingest.droplet.pending", json!({})).await?)?;
    for raw in rows {
        let mut job: Run = serde_json::from_str(&raw)?;
        let time = now();
        job.lease_until_ms = time + 2 * MINUTE_MS;
        let Some(mut prior) = replace(env, &raw, &job).await? else {
            continue;
        };
        job.next_ms = time + MINUTE_MS;
        job.retrying = step(env, host, &mut prior, &mut job).await.is_err();
        if job.retrying {
            // Never log provider bodies, identifiers from manifests, or credentials.
            worker::console_error!("A DigitalOcean ingest maintenance operation will retry");
        }
        job.lease_until_ms = 0;
        replace(env, &prior, &job).await?;
    }
    Ok(())
}

//! Pure builders for ingest runs: the job, the cloud-init user data, and the request bodies
//! for DigitalOcean and Scaleway (decision 13.6 of docs/plans/ingest.md). Nothing here does
//! I/O, so the smoke suite runs it natively.

use pigeon_ingest::archive::Limits;
use pigeon_ingest::estimate::Host;
use pigeon_ingest::manifest::{self, DestSpec, JobSpec, SourceSpec};
use serde_json::{Value, json};

use crate::data_read::{Account, Provider};

pub use pigeon_ingest::scaleway::{
    REPO_URL, SCW_IMAGE, SCW_TYPE, SCW_VOLUME_BYTES, TAG, scaleway_action_path,
    scaleway_cloud_init, scaleway_managed_cloud_init, scaleway_parts, scaleway_poweroff,
    scaleway_poweron, scaleway_run_tag, scaleway_server_body, scaleway_servers_path,
    scaleway_terminate, scaleway_user_data_path, scaleway_volume_names, vm_name, zone,
};
/// The DigitalOcean Droplet size.
pub const DO_SIZE: &str = "s-4vcpu-8gb";
/// The DigitalOcean image.
pub const DO_IMAGE: &str = "ubuntu-24-04-x64";
/// The placeholder that stands for a key in a plan.
pub const REDACTED: &str = "<redacted>";
/// Whether `s` is a full 40-character lowercase hex commit id.
pub fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The host that runs a source's job: DigitalOcean and Scaleway run next to their storage,
/// Backblaze runs in a Cloudflare Container.
pub fn host_for(provider: Provider) -> Host {
    match provider {
        Provider::DigitalOcean => Host::DigitalOcean,
        Provider::Scaleway => Host::Scaleway,
        Provider::Backblaze => Host::Cloudflare,
    }
}

/// The secret slot holding an instance's R2 key: `INGEST_R2_<INSTANCE>`.
pub fn r2_slot(instance: &str) -> String {
    format!("INGEST_R2_{}", instance.to_uppercase().replace('-', "_"))
}

/// The secret slot holding a storage account's host key, or none for Backblaze:
/// `INGEST_HOST_<PROVIDER>_<NAME>`.
pub fn host_slot(account: &Account) -> Option<String> {
    match account.provider {
        Provider::Backblaze => None,
        p => Some(
            format!("INGEST_HOST_{}_{}", p.slug(), account.name)
                .to_uppercase()
                .replace('-', "_"),
        ),
    }
}

/// The R2 S3 endpoint of an account.
pub fn r2_endpoint(account_id: &str) -> String {
    format!("https://{account_id}.r2.cloudflarestorage.com")
}

/// What a plan asks for, before defaults.
pub struct JobInputs<'a> {
    /// The instance id.
    pub instance: &'a str,
    /// The source account.
    pub account: &'a Account,
    /// The source bucket.
    pub bucket: &'a str,
    /// The source prefix; empty for all.
    pub prefix: &'a str,
    /// The destination bucket in the instance's account.
    pub dest_bucket: &'a str,
    /// The instance's Cloudflare account id.
    pub account_id: &'a str,
    /// Decide only; copy nothing. Default true.
    pub dry_run: bool,
    /// Keep archives as well as their members. Default true.
    pub keep_archives: bool,
    /// Compute SimHash. Default false.
    pub fingerprints: bool,
    /// The commit the runner is built from.
    pub commit: &'a str,
    /// The day, `yyyymmdd`.
    pub day: &'a str,
    /// Random bits for the run id.
    pub nonce: u32,
}

/// The job for a plan: the run id, the source and destination, default limits and 32 threads.
pub fn job_spec(i: &JobInputs<'_>) -> JobSpec {
    let a = i.account;
    JobSpec {
        run: manifest::run_id(i.day, &a.id(), i.bucket, i.nonce),
        instance: i.instance.into(),
        source: SourceSpec::S3 {
            account: a.id(),
            endpoint: a.endpoint.clone(),
            region: a.region.clone(),
            bucket: i.bucket.into(),
            prefix: i.prefix.into(),
        },
        dest: DestSpec {
            endpoint: r2_endpoint(i.account_id),
            bucket: i.dest_bucket.into(),
        },
        dry_run: i.dry_run,
        keep_archives: i.keep_archives,
        fingerprints: i.fingerprints,
        limits: Limits::default(),
        threads: 32,
        commit: i.commit.into(),
        max_minutes: None,
        resume: None,
        skip_over: None,
        shard: None,
    }
}

/// The cloud-init user data (a shell script) that builds the runner image at `commit`, runs it
/// once and powers the VM off. The keys go in a root-only env file.
pub fn cloud_init(job: &JobSpec, source_keys: &str, dest_keys: &str) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], "");
    let job_json = serde_json::to_string(job).unwrap_or_default();
    let c = &job.commit;
    format!(
        "#!/bin/bash\n\
         # pigeon ingest run {run}: build the runner at one commit, run it once, power off.\n\
         export DEBIAN_FRONTEND=noninteractive\n\
         apt-get update -y\n\
         apt-get install -y docker.io\n\
         systemctl start docker\n\
         mkdir -p /spool\n\
         chmod 777 /spool\n\
         umask 077\n\
         cat > /root/ingest.env <<'PIGEON_ENV_END'\n\
         INGEST_JOB={job_json}\n\
         SOURCE_KEYS={source}\n\
         DEST_KEYS={dest}\n\
         SPOOL_DIR=/spool\n\
         PIGEON_ENV_END\n\
         docker build -f src/ingest-runner/Dockerfile --build-arg PIGEON_COMMIT={c} -t pigeon-ingest-runner {REPO_URL}#{c} \
         && docker run --rm --env-file /root/ingest.env -v /spool:/spool pigeon-ingest-runner\n\
         shred -u /root/ingest.env\n\
         poweroff\n",
        run = job.run,
        source = one_line(source_keys),
        dest = one_line(dest_keys),
    )
}

/// [`cloud_init`] with both key values replaced by `<redacted>`, for plans and approvals.
pub fn cloud_init_redacted(job: &JobSpec) -> String {
    cloud_init(job, REDACTED, REDACTED)
}

/// The DigitalOcean `POST /v2/droplets` body. `user_data` is [`cloud_init`]'s text.
pub fn droplet_body(job: &JobSpec, region: &str, user_data: &str) -> Value {
    json!({
        "name": vm_name(&job.run),
        "region": region,
        "size": DO_SIZE,
        "image": DO_IMAGE,
        "tags": [TAG],
        "user_data": user_data,
    })
}

/// What an approval is bound to: the job, the redacted host request and the commit. It holds
/// no key.
pub fn approval_payload(job: &JobSpec, host_request: &Value) -> Value {
    json!({ "job": job, "host_request": host_request, "commit": job.commit })
}

/// The redacted host request for a plan, as the person sees it.
pub fn host_request(host: Host, job: &JobSpec, region: &str) -> Value {
    let user_data = cloud_init_redacted(job);
    match host {
        Host::DigitalOcean => json!({
            "host": "digitalocean", "method": "POST", "url": "https://api.digitalocean.com/v2/droplets",
            "body": droplet_body(job, region, &user_data),
        }),
        Host::Scaleway => scaleway_host_request(
            job,
            region,
            scaleway_cloud_init(job, "<approved-scratch-volume-id>", REDACTED, REDACTED),
        ),
        Host::Cloudflare => json!({
            "host": "cloudflare", "method": "POST", "path": pigeon_ingest::protocol::START_PATH,
            "body": { "job": job, "source_keys": REDACTED, "dest_keys": REDACTED },
        }),
        Host::Local => json!({ "host": "local" }),
    }
}

/// The redacted request shows the managed transport without exposing its grant.
pub fn scaleway_managed_host_request(job: &JobSpec, region: &str) -> Value {
    scaleway_host_request(
        job,
        region,
        scaleway_managed_cloud_init(job, "<approved-scratch-volume-id>", REDACTED, REDACTED),
    )
}

pub fn scaleway_broker_host_request(job: &JobSpec, region: &str) -> Value {
    json!({
        "host": "scaleway", "method": "POST", "path": pigeon_ingest::scaleway::BROKER_PATH,
        "body": { "operation": "launch", "job": job, "region": region, "source": REDACTED, "destination": REDACTED },
        "provider": scaleway_server_body(job, REDACTED),
    })
}

fn scaleway_host_request(job: &JobSpec, region: &str, user_data: String) -> Value {
    let zone = zone(region);
    json!({
        "host": "scaleway", "zone": zone,
        "create": { "method": "POST", "path": scaleway_servers_path(&zone),
            "body": scaleway_server_body(job, REDACTED) },
        "user_data": { "method": "PATCH", "path": scaleway_user_data_path(&zone, "<server>"),
            "body": user_data },
        "poweron": { "method": "POST", "path": scaleway_action_path(&zone, "<server>"),
            "body": scaleway_poweron() },
    })
}

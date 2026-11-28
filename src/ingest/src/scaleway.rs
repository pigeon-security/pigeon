//! Deterministic Scaleway server and encrypted scratch bootstrap builders.

use crate::manifest::JobSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The public repository the runner image is built from.
pub const REPO_URL: &str = "https://github.com/pigeon-security/pigeon.git";
/// The tag every pigeon ingest VM carries.
pub const TAG: &str = "pigeon-ingest";
/// The Scaleway instance type.
pub const SCW_TYPE: &str = "POP2-4C-16G";
/// The Scaleway image.
pub const SCW_IMAGE: &str = "ubuntu_noble";
/// The Scaleway boot volume, used by Ubuntu and the container image build.
const SCW_BOOT_BYTES: u64 = 40_000_000_000;
/// The separate block device mounted as the ingest spool.
pub const SCW_VOLUME_BYTES: u64 = 200_000_000_000;
/// Leave headroom for the filesystem and temporary writes.
const SCW_SPOOL_BYTES: u64 = 160_000_000_000;
/// The longest VM name pigeon asks for.
const NAME_MAX: usize = 63;

/// The Scaleway zone for a region: `fr-par` is `fr-par-1`.
pub fn zone(region: &str) -> String {
    format!("{region}-1")
}

/// A valid VM name for a run: `pigeon-ingest-{run}`, at most 63 characters of `a-z0-9-`,
/// not ending in `-`.
pub fn vm_name(run: &str) -> String {
    let mut s: String = format!("pigeon-ingest-{run}")
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s.truncate(NAME_MAX);
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// The Scaleway bootstrap mounts the extra device before installing or running the container.
pub fn scaleway_cloud_init(
    job: &JobSpec,
    scratch_volume: &str,
    source_keys: &str,
    dest_keys: &str,
) -> String {
    scaleway_bootstrap(job, scratch_volume, source_keys, "DEST_KEYS", dest_keys)
}

/// Bootstrap a scoped Scaleway runner with only its signed Data Worker grant.
pub fn scaleway_managed_cloud_init(
    job: &JobSpec,
    scratch_volume: &str,
    source_keys: &str,
    destination_json: &str,
) -> String {
    scaleway_bootstrap(
        job,
        scratch_volume,
        source_keys,
        "INGEST_DESTINATION",
        destination_json,
    )
}

fn scaleway_bootstrap(
    job: &JobSpec,
    scratch_volume: &str,
    source_keys: &str,
    destination_variable: &str,
    destination: &str,
) -> String {
    let one_line = |s: &str| s.replace(['\n', '\r'], "");
    let job_json = serde_json::to_string(job).unwrap_or_default();
    let mounts = include_str!("scaleway-spool.sh");
    let commit = &job.commit;
    format!(
        "#!/bin/bash\n\
         set -euo pipefail\n\
         umask 077\n\
         {mounts}\n\
         trap close_spool EXIT\n\
         export DEBIAN_FRONTEND=noninteractive\n\
         apt-get update -y\n\
         apt-get install -y docker.io cryptsetup-bin\n\
         mount_spool '{scratch_volume}' {SCW_VOLUME_BYTES}\n\
         mountpoint -q /spool\n\
         systemctl start docker\n\
         cat > /root/ingest.env <<'PIGEON_ENV_END'\n\
         INGEST_JOB={job_json}\n\
         SOURCE_KEYS={source}\n\
         {destination_variable}={dest}\n\
         SPOOL_DIR=/spool\n\
         SPOOL_BYTES={SCW_SPOOL_BYTES}\n\
         PIGEON_ENV_END\n\
         docker build -f src/ingest-runner/Dockerfile --build-arg PIGEON_COMMIT={commit} -t pigeon-ingest-runner {REPO_URL}#{commit}\n\
         docker run --rm --env-file /root/ingest.env -v /spool:/spool pigeon-ingest-runner\n",
        source = one_line(source_keys),
        dest = one_line(destination),
    )
}

/// A stable provider tag for reconciliation after an interrupted create request.
pub fn scaleway_run_tag(run: &str) -> String {
    let digest = Sha256::digest(run.as_bytes());
    let tag: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("pigeon-ingest-{tag}")
}

/// Names are tied to the run tag so a later cleanup can recover an omitted block ID.
pub fn scaleway_volume_names(run: &str) -> [String; 2] {
    let tag = scaleway_run_tag(run);
    [format!("{tag}-boot"), format!("{tag}-scratch")]
}

/// The Scaleway `POST .../servers` body, with separate boot and scratch volumes.
pub fn scaleway_server_body(job: &JobSpec, project: &str) -> Value {
    let name = vm_name(&job.run);
    let [boot, scratch] = scaleway_volume_names(&job.run);
    json!({
        "name": name,
        "commercial_type": SCW_TYPE,
        "image": SCW_IMAGE,
        "project": project,
        "tags": [TAG, scaleway_run_tag(&job.run)],
        "dynamic_ip_required": true,
        "volumes": {
            "0": {
                "name": boot,
                "boot": true,
                "size": SCW_BOOT_BYTES,
                "volume_type": "sbs_volume",
            },
            "1": {
                "name": scratch,
                "boot": false,
                "size": SCW_VOLUME_BYTES,
                "volume_type": "sbs_volume",
            },
        },
    })
}

/// The path of Scaleway's servers in `zone`.
pub fn scaleway_servers_path(zone: &str) -> String {
    format!("/instance/v1/zones/{zone}/servers")
}

/// The path of a Scaleway server's cloud-init user data. Send it with `PATCH` and
/// `Content-Type: text/plain`; the body is the user data itself.
pub fn scaleway_user_data_path(zone: &str, server: &str) -> String {
    format!(
        "{}/{server}/user_data/cloud-init",
        scaleway_servers_path(zone)
    )
}

/// The path of a Scaleway server's actions, for `POST`.
pub fn scaleway_action_path(zone: &str, server: &str) -> String {
    format!("{}/{server}/action", scaleway_servers_path(zone))
}

/// The body that powers a Scaleway server on.
pub fn scaleway_poweron() -> Value {
    json!({ "action": "poweron" })
}

/// Release the compute allocation of a running or in-place stopped Instance.
pub fn scaleway_poweroff() -> Value {
    json!({ "action": "poweroff" })
}

/// The action that terminates a running Scaleway server. Block volumes are deleted separately.
pub fn scaleway_terminate() -> Value {
    json!({ "action": "terminate" })
}

/// Splits Scaleway's `PROJECT_ID:SECRET_KEY` secret. The error never holds the value.
pub fn scaleway_parts(value: &str) -> Result<(String, String), String> {
    match value.trim().split_once(':') {
        Some((p, s)) if !p.trim().is_empty() && !s.trim().is_empty() => {
            Ok((p.trim().into(), s.trim().into()))
        }
        _ => Err("expected `PROJECT_ID:SECRET_KEY`".into()),
    }
}

/// Signed instance-to-Data Worker compute commands use this one fixed route.
pub const BROKER_PATH: &str = "/ingest/scaleway/compute";

/// A non-secret pointer to an immutable credential generation in the linked account.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputePin {
    pub project: String,
    pub generation: String,
}

impl ComputePin {
    pub fn binding(&self, instance: &str) -> Option<String> {
        if !valid_project(&self.project)
            || self.generation.len() != 24
            || !self
                .generation
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return None;
        }
        let scope = Sha256::digest(format!("{instance}:{}", self.project).as_bytes());
        let scope: String = scope[..12]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect();
        Some(format!(
            "SCW_COMPUTE_{scope}_{}",
            self.generation.to_ascii_uppercase()
        ))
    }
}

pub fn valid_project(project: &str) -> bool {
    project.len() == 36
        && project.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

pub fn valid_region(region: &str) -> bool {
    matches!(region, "fr-par" | "nl-ams" | "pl-waw")
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", deny_unknown_fields)]
pub enum ComputeCommand {
    Check { pin: ComputePin, region: String },
}

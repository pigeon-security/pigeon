//! `.pigeon/ci.toml`: the file in a repository that names its CI jobs, each with the script to
//! run, how long it may take and what its runner must provide. The gateway reads it at the
//! commit being run and never reads the script. Anything not given takes a small Linux
//! default; an unknown key is an error, so a typo never quietly becomes a default.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where the file is, from the repository's root.
pub const PATH: &str = ".pigeon/ci.toml";
/// A job's timeout when it names none, in minutes.
pub const DEFAULT_TIMEOUT_MIN: u32 = 30;
/// The longest timeout a job may name, in minutes.
pub const MAX_TIMEOUT_MIN: u32 = 360;

const MAX_JOBS: usize = 100;
const MAX_RUN_BYTES: usize = 256;

/// The operating system a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

/// The CPU architecture a job runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    Amd64,
    Arm64,
}

/// The network a job gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// Outbound connections are allowed.
    Egress,
    /// None beyond fetching its code.
    None,
}

/// What a job's runner must provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Needs {
    pub os: Os,
    pub arch: Arch,
    pub cpu: u32,
    pub memory_gb: u32,
    pub disk_gb: u32,
    pub docker: bool,
    pub gpu: bool,
    pub network: Network,
}

impl Default for Needs {
    fn default() -> Self {
        Needs {
            os: Os::Linux,
            arch: Arch::Amd64,
            cpu: 2,
            memory_gb: 4,
            disk_gb: 10,
            docker: false,
            gpu: false,
            network: Network::Egress,
        }
    }
}

/// One named job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// The script to run, from the repository's root.
    pub run: String,
    pub timeout_min: u32,
    pub needs: Needs,
    /// Whether the job's script gets its job token, as `PIGEON_JOB_TOKEN`: only a job that has
    /// to reach the platform with it, such as `deploy`, which the deploy proxy takes only as a
    /// landing's deploy job. Off by default; the runner withholds the token otherwise.
    #[serde(default)]
    pub token: bool,
}

/// The file, read and checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Config {
    /// The jobs, by name.
    pub jobs: BTreeMap<String, Job>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawConfig {
    jobs: Option<BTreeMap<String, RawJob>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawJob {
    run: Option<String>,
    timeout_min: Option<u32>,
    needs: RawNeeds,
    token: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct RawNeeds {
    os: Option<Os>,
    arch: Option<Arch>,
    cpu: Option<u32>,
    memory_gb: Option<u32>,
    disk_gb: Option<u32>,
    docker: Option<bool>,
    gpu: Option<bool>,
    network: Option<Network>,
}

impl RawNeeds {
    fn into_needs(self) -> Needs {
        let d = Needs::default();
        Needs {
            os: self.os.unwrap_or(d.os),
            arch: self.arch.unwrap_or(d.arch),
            cpu: self.cpu.unwrap_or(d.cpu),
            memory_gb: self.memory_gb.unwrap_or(d.memory_gb),
            disk_gb: self.disk_gb.unwrap_or(d.disk_gb),
            docker: self.docker.unwrap_or(d.docker),
            gpu: self.gpu.unwrap_or(d.gpu),
            network: self.network.unwrap_or(d.network),
        }
    }
}

/// Reads and checks the file.
pub fn parse(text: &str) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| format!("not valid {PATH}: {e}"))?;
    let raw_jobs = raw.jobs.unwrap_or_default();
    if raw_jobs.is_empty() {
        return Err("the file names no jobs".to_string());
    }
    if raw_jobs.len() > MAX_JOBS {
        return Err(format!("the file names more than {MAX_JOBS} jobs"));
    }
    let mut jobs = BTreeMap::new();
    for (name, raw_job) in raw_jobs {
        let job = build_job(&name, raw_job).map_err(|e| format!("job `{name}`: {e}"))?;
        jobs.insert(name, job);
    }
    Ok(Config { jobs })
}

fn build_job(name: &str, raw: RawJob) -> Result<Job, String> {
    job_name(name)?;
    let run = raw.run.ok_or_else(|| "run is required".to_string())?;
    check_run(&run)?;
    let timeout_min = raw.timeout_min.unwrap_or(DEFAULT_TIMEOUT_MIN);
    in_range("timeout_min", timeout_min, 1, MAX_TIMEOUT_MIN)?;
    let needs = raw.needs.into_needs();
    check_needs(&needs)?;
    Ok(Job {
        run,
        timeout_min,
        needs,
        token: raw.token.unwrap_or(false),
    })
}

/// Checks `needs` alone, as when a caller gives them directly.
pub fn check_needs(needs: &Needs) -> Result<(), String> {
    in_range("cpu", needs.cpu, 1, 64)?;
    in_range("memory_gb", needs.memory_gb, 1, 256)?;
    in_range("disk_gb", needs.disk_gb, 1, 2000)
}

/// Checks a job's name: 1 to 64 of `a-z`, `0-9`, `_` and `-`.
pub fn job_name(name: &str) -> Result<(), String> {
    let ok = (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-');
    if ok {
        Ok(())
    } else {
        Err("a job's name must be 1 to 64 of a-z, 0-9, '_' and '-'".to_string())
    }
}

/// Checks `run`: a relative path inside the repository.
fn check_run(run: &str) -> Result<(), String> {
    if run.is_empty() {
        return Err("run must not be empty".to_string());
    }
    if run.len() > MAX_RUN_BYTES {
        return Err(format!("run must be at most {MAX_RUN_BYTES} bytes"));
    }
    if run.starts_with('/') {
        return Err("run must be a path from the repository's root, not start with '/'".into());
    }
    if run.contains('\\') {
        return Err("run must not hold a backslash".to_string());
    }
    if run.chars().any(char::is_control) {
        return Err("run must not hold control characters".to_string());
    }
    for seg in run.split('/') {
        if seg.is_empty() {
            return Err("run must not have an empty path segment".to_string());
        }
        if seg == "." || seg == ".." {
            return Err("run must not have a '.' or '..' path segment".to_string());
        }
    }
    Ok(())
}

fn in_range(what: &str, v: u32, lo: u32, hi: u32) -> Result<(), String> {
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(format!("{what} must be from {lo} to {hi}"))
    }
}

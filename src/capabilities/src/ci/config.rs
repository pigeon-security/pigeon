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

#[cfg(test)]
mod tests {
    use super::*;

    fn one(job_body: &str) -> Result<Config, String> {
        parse(&format!("[jobs.t]\n{job_body}"))
    }

    fn run_body(run: &str) -> String {
        format!("run = {run:?}")
    }

    #[test]
    fn the_example_parses_with_its_defaults() {
        let text = r#"
[jobs.test]
run = "scripts/ci/test.sh"
timeout_min = 20

[jobs.test.needs]
cpu = 4
memory_gb = 8

[jobs.image]
run = "scripts/ci/image.sh"
needs = { os = "linux", docker = true }
"#;
        let cfg = parse(text).unwrap();
        assert_eq!(cfg.jobs.len(), 2);
        let t = &cfg.jobs["test"];
        assert_eq!(t.run, "scripts/ci/test.sh");
        assert_eq!(t.timeout_min, 20);
        assert_eq!(
            t.needs,
            Needs {
                cpu: 4,
                memory_gb: 8,
                ..Needs::default()
            }
        );
        let i = &cfg.jobs["image"];
        assert!(i.needs.docker);
        assert_eq!(i.needs.os, Os::Linux);
        assert_eq!(i.timeout_min, DEFAULT_TIMEOUT_MIN);
    }

    #[test]
    fn a_job_without_needs_takes_the_small_linux_default() {
        let cfg = one(r#"run = "a.sh""#).unwrap();
        assert_eq!(cfg.jobs["t"].needs, Needs::default());
        let d = Needs::default();
        assert_eq!((d.os, d.arch), (Os::Linux, Arch::Amd64));
        assert_eq!((d.cpu, d.memory_gb, d.disk_gb), (2, 4, 10));
        assert!(!d.docker && !d.gpu);
        assert_eq!(d.network, Network::Egress);
    }

    #[test]
    fn network_none_and_arm64_parse() {
        let cfg = one("run = \"a.sh\"\nneeds = { network = \"none\", arch = \"arm64\" }").unwrap();
        let n = &cfg.jobs["t"].needs;
        assert_eq!(n.network, Network::None);
        assert_eq!(n.arch, Arch::Arm64);
    }

    #[test]
    fn unknown_keys_are_errors_wherever_they_are() {
        assert!(parse("foo = 1\n[jobs.t]\nrun = \"a.sh\"").is_err());
        assert!(one("run = \"a.sh\"\nbogus = 1").is_err());
        assert!(one("run = \"a.sh\"\nneeds = { bogus = 1 }").is_err());
    }

    #[test]
    fn bad_run_paths_are_errors_naming_the_job() {
        let long = "a".repeat(257);
        for bad in [
            "/abs.sh",
            "../x.sh",
            "a/../b.sh",
            "./x.sh",
            "a//b.sh",
            "a\\b.sh",
            "a\tb.sh",
            "",
            long.as_str(),
        ] {
            let err = one(&run_body(bad)).expect_err(bad);
            assert!(err.contains("job `t`"), "error for {bad:?} was {err:?}");
        }
        assert!(one(&run_body(&"a".repeat(256))).is_ok());
        assert!(one("timeout_min = 5").unwrap_err().contains("job `t`"));
    }

    #[test]
    fn timeouts_and_sizes_stay_in_range() {
        for t in [0, 361] {
            assert!(one(&format!("run = \"a.sh\"\ntimeout_min = {t}")).is_err());
        }
        for t in [1, 360] {
            let cfg = one(&format!("run = \"a.sh\"\ntimeout_min = {t}")).unwrap();
            assert_eq!(cfg.jobs["t"].timeout_min, t);
        }
        for c in [0, 65] {
            assert!(one(&format!("run = \"a.sh\"\nneeds = {{ cpu = {c} }}")).is_err());
        }
        assert!(one("run = \"a.sh\"\nneeds = { disk_gb = 0 }").is_err());
        assert!(one("run = \"a.sh\"\nneeds = { cpu = -1 }").is_err());
    }

    #[test]
    fn job_names_are_short_and_lowercase() {
        for n in ["Test", "a b", "", &"x".repeat(65), "é"] {
            assert!(job_name(n).is_err(), "{n:?}");
        }
        assert!(job_name("a-b_1").is_ok());
        assert!(job_name(&"x".repeat(64)).is_ok());
        assert!(parse("[jobs.Bad]\nrun = \"a.sh\"").is_err());
    }

    #[test]
    fn the_repositorys_own_file_parses() {
        let cfg = parse(include_str!("../../../../.pigeon/ci.toml")).unwrap();
        assert_eq!(cfg.jobs["smoke"].run, "ci/runner/smoke.sh");
        assert_eq!(cfg.jobs["smoke"].needs, Needs::default());
        assert_eq!(cfg.jobs["smoke-fail"].run, "ci/runner/smoke-fail.sh");
    }

    #[test]
    fn junk_is_refused() {
        assert!(one("run = \"a.sh\"\nneeds = { os = \"beos\" }").is_err());
        assert!(parse("").is_err());
        assert!(parse("not toml [[").is_err());
    }
}

//! The workers map: which Worker owns which repository paths.
//!
//! The map is read from `ci/workers.toml`. It classifies the paths a landing changes, so
//! the Lander can pick tests, spot docs-only landings, decide on retests and order deploys. The
//! Lander reads the map from main, never from the branch being landed, so a branch never picks
//! its own lane. See docs/plans/workers-split.md.
//!
//! A path belongs to every Worker whose patterns match it: a library such as `src/models` is
//! built into each Worker that links it. `[checks]` paths run the tests and deploy nothing (the
//! gate, CI's job definitions). `[shared] core_first` names the paths whose change makes the
//! other lanes wait until core has deployed it (the contract between them).

use std::collections::BTreeSet;

use serde::Deserialize;

/// The smoke suite's root file. It belongs to no entry in the map.
pub const SMOKE: &str = "tests/smoke.rs";

/// The directory of the smoke suite's area modules.
pub const SMOKE_DIR: &str = "tests/smoke/";

/// True when the path is a file of the smoke suite: the root or an area module.
pub fn is_smoke(path: &str) -> bool {
    path == SMOKE || (path.starts_with(SMOKE_DIR) && path.ends_with(".rs"))
}

/// The name the one required core Worker must have.
const CORE: &str = "core";

/// The parsed workers map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    /// Patterns for paths that are merged without tests or deploys.
    pub docs: Vec<String>,
    /// Patterns for paths that run every Worker's tests.
    pub shared: Vec<String>,
    /// Patterns for paths that run the tests and deploy nothing.
    pub checks: Vec<String>,
    /// Patterns for paths whose change deploys core before any other lane; `None` (a map without
    /// the key) keeps the older rule: every landing that affects core.
    pub core_first: Option<Vec<String>>,
    /// The Workers, in file order.
    pub workers: Vec<Worker>,
}

/// One Worker and the paths it owns.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// The short name of the Worker, unique in the map.
    pub name: String,
    /// The Cloudflare script name of the Worker, unique in the map.
    pub script: String,
    /// The directory of the Worker, relative to the repository root.
    pub dir: String,
    /// Patterns for the paths this Worker owns.
    pub paths: Vec<String>,
    /// The name of the CI job that tests this Worker.
    pub test_job: String,
    /// The name of the CI job that deploys this Worker.
    pub deploy_job: String,
    /// The steps that run once this Worker has deployed.
    #[serde(default)]
    pub after: Vec<After>,
    /// The queues it produces to, which its deploy looks up by name (the deploy proxy's scope).
    #[serde(default)]
    pub queues: Vec<String>,
    /// Its container classes, whose container applications its deploy may change.
    #[serde(default)]
    pub containers: Vec<String>,
}

/// A lane that deploys no Worker: its deploy job builds a crate and uploads the build to a
/// bucket of the platform's own account, from which an instance plan reads it. Its map entry's
/// `script` is that bucket, so its deploy grant names the bucket, not a Worker; the deploy proxy
/// lets such a grant read and write only the build's objects under `prefix/` in that bucket, and
/// no Worker of its name exists or is deployed. A shared path deploys it like any lane. The list
/// is fixed here, not in the map: a map field would not parse in an older core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactLane {
    /// Its name in the map.
    pub name: &'static str,
    /// The bucket its build goes to: its map entry's `script`.
    pub bucket: &'static str,
    /// The folder in the bucket its build goes to.
    pub prefix: &'static str,
}

/// The artifact lanes: the data Worker's build (docs/plans/email.md, follow-up 2), which an
/// instance plan uploads to an instance's account (`instances::data_plan::ARTIFACTS`).
pub const ARTIFACT_LANES: [ArtifactLane; 1] = [ArtifactLane {
    name: "data-worker",
    bucket: "pigeon-data-artifacts",
    prefix: "data-worker",
}];

impl Worker {
    /// The artifact lane this entry is ([`ARTIFACT_LANES`]: its name and script both match), if
    /// it is one: it deploys no Worker.
    pub fn artifact(&self) -> Option<&'static ArtifactLane> {
        ARTIFACT_LANES
            .iter()
            .find(|a| a.name == self.name && a.bucket == self.script)
    }

    /// Whether this entry deploys a Worker: every entry but an artifact lane.
    pub fn is_worker(&self) -> bool {
        self.artifact().is_none()
    }
}

/// A step that runs once a Worker has deployed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum After {
    /// The live architecture page names the deployed commit.
    Architecture,
    /// The MCP server portal is synced, so it lists the deployed tools.
    PortalSync,
    /// The CI connectors' setup runs against the deployed settings.
    ConnectorsSetup,
}

/// The entry that owns a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner<'a> {
    /// The path is documentation.
    Docs,
    /// The path is shared by every Worker, or matched nothing.
    Shared,
    /// The path is a file of the smoke suite.
    Smoke,
    /// The path runs the tests and deploys nothing.
    Checks,
    /// The path belongs to these Workers (at least one), in file order.
    Workers(Vec<&'a Worker>),
}

/// The effect of a set of changed paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    /// True when the change is non-empty and every path is documentation.
    pub docs_only: bool,
    /// True when some changed path is shared.
    pub shared: bool,
    /// True when a file of the smoke suite is among the changed paths.
    pub smoke: bool,
    /// The names of the Workers to deploy.
    pub workers: BTreeSet<String>,
    /// True when the other lanes wait for core to deploy it first ([`Map::core_first`]).
    pub ordered: bool,
}

/// How a branch's changes relate to what main changed meanwhile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    /// The paths changed on both sides, sorted and without duplicates.
    pub same_paths: Vec<String>,
    /// True when the branch must be tested again on top of main.
    pub retest: bool,
}

/// Parses and validates the text of `ci/workers.toml`.
pub fn parse(text: &str) -> Result<Map, String> {
    let raw: Raw = toml::from_str(text).map_err(|e| e.to_string())?;
    if raw.version != 1 {
        return Err(format!("version: must be 1, got {}", raw.version));
    }
    check_patterns("docs.paths", &raw.docs.paths)?;
    check_patterns("shared.paths", &raw.shared.paths)?;
    if let Some(first) = &raw.shared.core_first {
        check_patterns("shared.core_first", first)?;
    }
    let checks = raw.checks.map(|c| c.paths).unwrap_or_default();
    check_patterns("checks.paths", &checks)?;
    if raw.worker.is_empty() {
        return Err("worker: at least one worker is required".to_string());
    }
    let mut names: BTreeSet<&str> = BTreeSet::new();
    let mut scripts: BTreeSet<&str> = BTreeSet::new();
    for w in &raw.worker {
        check_name("worker.name", &w.name, 32)?;
        if !names.insert(w.name.as_str()) {
            return Err(format!("worker.name: duplicate name {:?}", w.name));
        }
        let label = format!("worker {:?}", w.name);
        check_name(&format!("{label} script"), &w.script, 63)?;
        if !scripts.insert(w.script.as_str()) {
            return Err(format!("{label} script: duplicate script {:?}", w.script));
        }
        check_dir(&format!("{label} dir"), &w.dir)?;
        check_patterns(&format!("{label} paths"), &w.paths)?;
        if w.test_job.is_empty() {
            return Err(format!("{label} test_job: must not be empty"));
        }
        if w.deploy_job.is_empty() {
            return Err(format!("{label} deploy_job: must not be empty"));
        }
    }
    if !names.contains(CORE) {
        return Err("worker.name: exactly one worker must be named \"core\"".to_string());
    }
    Ok(Map {
        docs: raw.docs.paths,
        shared: raw.shared.paths,
        checks,
        core_first: raw.shared.core_first,
        workers: raw.worker,
    })
}

/// Returns true when the pattern matches the path.
///
/// A pattern ending in `*` matches any path starting with the text before the `*`. Any other
/// pattern matches only the identical path.
pub fn matches(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => path.starts_with(prefix),
        None => pattern == path,
    }
}

impl Map {
    /// Returns the entry that owns the path.
    ///
    /// The order is the smoke suite first, then docs, then shared, then checks; the
    /// first of those to match wins. Otherwise the path belongs to every Worker whose patterns
    /// match it, and a path nothing matches is shared.
    pub fn owner(&self, path: &str) -> Owner<'_> {
        if is_smoke(path) {
            return Owner::Smoke;
        }
        if self.docs.iter().any(|p| matches(p, path)) {
            return Owner::Docs;
        }
        if self.shared.iter().any(|p| matches(p, path)) {
            return Owner::Shared;
        }
        if self.checks.iter().any(|p| matches(p, path)) {
            return Owner::Checks;
        }
        let owners: Vec<&Worker> = self
            .workers
            .iter()
            .filter(|w| w.paths.iter().any(|p| matches(p, path)))
            .collect();
        if owners.is_empty() {
            Owner::Shared
        } else {
            Owner::Workers(owners)
        }
    }

    /// Returns the Worker with the given name, if there is one.
    pub fn worker(&self, name: &str) -> Option<&Worker> {
        self.workers.iter().find(|w| w.name == name)
    }

    /// Classifies a set of changed paths.
    ///
    /// A change to the smoke suite or to a checks path runs the tests and deploys nothing by
    /// itself; it never makes the change shared or docs-only. A shared path deploys every Worker.
    pub fn classify(&self, changed: &[&str]) -> Affected {
        let mut docs_only = !changed.is_empty();
        let mut shared = false;
        let mut smoke = false;
        let mut workers: BTreeSet<String> = BTreeSet::new();
        for &path in changed {
            match self.owner(path) {
                Owner::Docs => {}
                Owner::Shared => {
                    shared = true;
                    docs_only = false;
                }
                Owner::Smoke => {
                    smoke = true;
                    docs_only = false;
                }
                Owner::Checks => docs_only = false,
                Owner::Workers(ws) => {
                    workers.extend(ws.iter().map(|w| w.name.clone()));
                    docs_only = false;
                }
            }
        }
        if shared {
            workers = self.workers.iter().map(|w| w.name.clone()).collect();
        }
        let ordered = match &self.core_first {
            Some(first) => changed
                .iter()
                .any(|path| first.iter().any(|p| matches(p, path))),
            None => workers.contains(CORE),
        };
        Affected {
            docs_only,
            shared,
            smoke,
            workers,
            ordered,
        }
    }

    /// Compares a branch's changes with what main changed since the same base.
    ///
    /// A retest is needed when the branch is not docs-only and main moved a shared path,
    /// the smoke suite, or a path owned by a Worker the branch affects.
    pub fn overlap(&self, branch: &[&str], main_moved: &[&str]) -> Overlap {
        let on_branch: BTreeSet<&str> = branch.iter().copied().collect();
        let on_main: BTreeSet<&str> = main_moved.iter().copied().collect();
        let same_paths: Vec<String> = on_branch
            .intersection(&on_main)
            .map(|p| (*p).to_owned())
            .collect();
        let affected = self.classify(branch);
        let retest = !affected.docs_only
            && main_moved.iter().any(|&path| match self.owner(path) {
                Owner::Docs => false,
                Owner::Shared | Owner::Smoke | Owner::Checks => true,
                Owner::Workers(ws) => ws.iter().any(|w| affected.workers.contains(&w.name)),
            });
        Overlap { same_paths, retest }
    }

    /// Returns the Workers to deploy, in deploy order.
    ///
    /// The list is empty for a docs-only change. Otherwise `core` comes first when the change is
    /// shared or affects it, then the rest in file order.
    pub fn deploy_order(&self, affected: &Affected) -> Vec<&Worker> {
        let mut order: Vec<&Worker> = Vec::new();
        if affected.docs_only {
            return order;
        }
        if let Some(core) = self.worker(CORE)
            && (affected.shared || affected.workers.contains(CORE))
        {
            order.push(core);
        }
        for w in &self.workers {
            if w.name != CORE && affected.workers.contains(w.name.as_str()) {
                order.push(w);
            }
        }
        order
    }

    /// Returns the entries that deploy a Worker, in file order: every one but the artifact
    /// lanes ([`ARTIFACT_LANES`]).
    pub fn platform_workers(&self) -> impl Iterator<Item = &Worker> {
        self.workers.iter().filter(|w| w.is_worker())
    }

    /// Returns every Worker's script name, in file order; an artifact lane's bucket is none.
    pub fn scripts(&self) -> Vec<&str> {
        self.platform_workers().map(|w| w.script.as_str()).collect()
    }
}

/// The lockfile, whose change is shared unless [`lock_members_only`] says otherwise.
pub const LOCK: &str = "Cargo.lock";

/// Whether a lockfile change from `before` to `after` touches only the workspace's own crates'
/// entries (those without a `source`): such a change follows from those crates' own
/// `Cargo.toml` changes, which carry their lanes, so the lockfile need not deploy every Worker.
/// Any other difference (a package from a registry or git added, removed or changed, or a
/// lockfile either side that does not parse) is false: the change stays shared.
pub fn lock_members_only(before: &str, after: &str) -> bool {
    fn packages(text: &str) -> Option<(toml::Table, Vec<toml::Value>)> {
        let mut table: toml::Table = toml::from_str(text).ok()?;
        let packages = match table.remove("package") {
            Some(toml::Value::Array(a)) => a,
            None => Vec::new(),
            Some(_) => return None,
        };
        Some((table, packages))
    }
    fn external(packages: &[toml::Value]) -> Vec<&toml::Value> {
        packages
            .iter()
            .filter(|p| p.get("source").is_some())
            .collect()
    }
    let (Some((head_a, a)), Some((head_b, b))) = (packages(before), packages(after)) else {
        return false;
    };
    head_a == head_b && external(&a) == external(&b)
}

/// The file as it is read, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: u32,
    docs: Section,
    shared: Shared,
    #[serde(default)]
    checks: Option<Section>,
    #[serde(default)]
    worker: Vec<Worker>,
}

/// A `[docs]` or `[checks]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    paths: Vec<String>,
}

/// The `[shared]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shared {
    paths: Vec<String>,
    #[serde(default)]
    core_first: Option<Vec<String>>,
}

/// Checks every pattern in a list.
fn check_patterns(key: &str, patterns: &[String]) -> Result<(), String> {
    for pattern in patterns {
        check_pattern(key, pattern)?;
    }
    Ok(())
}

/// Checks one pattern against the pattern rules.
fn check_pattern(key: &str, pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err(format!("{key}: pattern must not be empty"));
    }
    if pattern.starts_with('/') {
        return Err(format!(
            "{key}: pattern {pattern:?} must not start with '/'"
        ));
    }
    if pattern.split('/').any(|segment| segment == "..") {
        return Err(format!(
            "{key}: pattern {pattern:?} must not contain '..' segments"
        ));
    }
    let body = pattern.strip_suffix('*').unwrap_or(pattern);
    if body.contains('*') {
        return Err(format!(
            "{key}: pattern {pattern:?} may contain '*' only as its last character"
        ));
    }
    Ok(())
}

/// Checks a name or script: characters `[a-z0-9-]`, length 1 to `max`.
fn check_name(key: &str, value: &str, max: usize) -> Result<(), String> {
    let charset_ok = value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if value.is_empty() || value.len() > max || !charset_ok {
        return Err(format!(
            "{key}: {value:?} must be 1 to {max} characters of [a-z0-9-]"
        ));
    }
    Ok(())
}

/// Checks a Worker directory against the relative path rules.
fn check_dir(key: &str, dir: &str) -> Result<(), String> {
    if dir.is_empty() {
        return Err(format!("{key}: must not be empty"));
    }
    if dir.starts_with('/') {
        return Err(format!("{key}: {dir:?} must not start with '/'"));
    }
    if dir.ends_with('/') {
        return Err(format!("{key}: {dir:?} must not end with '/'"));
    }
    if dir.split('/').any(|segment| segment == "..") {
        return Err(format!("{key}: {dir:?} must not contain '..' segments"));
    }
    Ok(())
}

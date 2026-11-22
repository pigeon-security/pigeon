//! The workers map: which Worker owns which repository paths.
//!
//! The map is read from `ci/workers.toml`. It classifies the paths a landing changes, so
//! the Lander can pick tests, spot docs-only landings, decide on retests and order deploys. The
//! Lander reads the map from main, never from the branch being landed, so a branch never picks
//! its own lane. See docs/plans/workers-split.md.

use std::collections::BTreeSet;

use serde::Deserialize;

/// The test-shared path. It belongs to no entry in the map.
pub const SMOKE: &str = "tests/smoke.rs";

/// The name the one required core Worker must have.
const CORE: &str = "core";

/// The parsed workers map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map {
    /// Patterns for paths that are merged without tests or deploys.
    pub docs: Vec<String>,
    /// Patterns for paths that run every Worker's tests.
    pub shared: Vec<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner<'a> {
    /// The path is documentation.
    Docs,
    /// The path is shared by every Worker, or matched nothing.
    Shared,
    /// The path is the test-shared path, [`SMOKE`].
    Smoke,
    /// The path belongs to this Worker.
    Worker(&'a Worker),
}

/// The effect of a set of changed paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    /// True when the change is non-empty and every path is documentation.
    pub docs_only: bool,
    /// True when some changed path is shared.
    pub shared: bool,
    /// True when [`SMOKE`] is among the changed paths.
    pub smoke: bool,
    /// The names of the Workers whose tests must run.
    pub workers: BTreeSet<String>,
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
    /// The order is [`SMOKE`] first, then docs, then shared, then the Workers in file order. The
    /// first match wins, and a path nothing matches is shared.
    pub fn owner(&self, path: &str) -> Owner<'_> {
        if path == SMOKE {
            return Owner::Smoke;
        }
        if self.docs.iter().any(|p| matches(p, path)) {
            return Owner::Docs;
        }
        if self.shared.iter().any(|p| matches(p, path)) {
            return Owner::Shared;
        }
        match self
            .workers
            .iter()
            .find(|w| w.paths.iter().any(|p| matches(p, path)))
        {
            Some(w) => Owner::Worker(w),
            None => Owner::Shared,
        }
    }

    /// Returns the Worker with the given name, if there is one.
    pub fn worker(&self, name: &str) -> Option<&Worker> {
        self.workers.iter().find(|w| w.name == name)
    }

    /// Classifies a set of changed paths.
    ///
    /// A change to [`SMOKE`] runs the tests of the Workers the other paths affect. When no other
    /// path affects a Worker, it runs every Worker's tests. It never makes the change shared or
    /// docs-only.
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
                Owner::Worker(w) => {
                    workers.insert(w.name.clone());
                    docs_only = false;
                }
            }
        }
        if shared || (smoke && workers.is_empty()) {
            workers = self.workers.iter().map(|w| w.name.clone()).collect();
        }
        Affected {
            docs_only,
            shared,
            smoke,
            workers,
        }
    }

    /// Compares a branch's changes with what main changed since the same base.
    ///
    /// A retest is needed when the branch is not docs-only and main moved a shared path,
    /// [`SMOKE`], or a path owned by a Worker the branch affects.
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
                Owner::Shared | Owner::Smoke => true,
                Owner::Worker(w) => affected.workers.contains(w.name.as_str()),
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

    /// Returns every Worker's script name, in file order.
    pub fn scripts(&self) -> Vec<&str> {
        self.workers.iter().map(|w| w.script.as_str()).collect()
    }
}

/// The file as it is read, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    version: u32,
    docs: Section,
    shared: Section,
    #[serde(default)]
    worker: Vec<Worker>,
}

/// A `[docs]` or `[shared]` table.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    paths: Vec<String>,
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

//! Generates the iOS pipelines' steps for docs/plans/ios.md: a build pipeline and a release
//! pipeline. It is off until the person sets `Ios` in the CI settings and has a macOS queue.
//! Nothing uses it yet.
//!
//! It is pure: it makes the steps as data, checks their shape with [`validate`], and renders
//! block YAML with [`yaml`]. There is no YAML library in the workspace, so [`shape`] gives a
//! light structural check of the rendered text.

use std::collections::HashSet;

/// The iOS pipelines' settings. `None` in the CI settings leaves them off.
#[derive(Clone, Debug, PartialEq)]
pub struct Ios {
    /// The platform host the bootstrap is fetched from, such as `platform.pigeon.dev`.
    pub host: String,
    /// The build pipeline's slug, such as `pigeon-ios`.
    pub build_pipeline: String,
    /// The release pipeline's slug, such as `pigeon-ios-release`.
    pub release_pipeline: String,
    /// The macOS queue that builds, such as `pigeon-macos`.
    pub queue: String,
    /// The macOS queue that only signs and uploads, such as `pigeon-macos-sign`.
    pub sign_queue: String,
    /// The branch releases are cut from, such as `main`.
    pub release_branch: String,
}

/// One command step.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    /// The step's unique key: `[a-z0-9-]` only.
    pub key: String,
    /// The step's label. It must not hold `:`, which Buildkite reads as an emoji name.
    pub label: String,
    /// The queue whose agents run the step.
    pub queue: String,
    /// The shell command.
    pub command: String,
    /// The keys of earlier steps this one waits for.
    pub depends_on: Vec<String>,
    /// The step's timeout in minutes, 1 to 120.
    pub timeout_min: u32,
    /// The branches the step runs on, if limited.
    pub branches: Option<String>,
    /// The concurrency group. Set together with `concurrency`.
    pub concurrency_group: Option<String>,
    /// How many steps of the group may run at once. Set together with the group.
    pub concurrency: Option<u32>,
    /// The files the step uploads as artifacts.
    pub artifact_paths: Vec<String>,
    /// The step's environment, in order.
    pub env: Vec<(String, String)>,
}

/// A pipeline's steps.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipeline {
    /// The pipeline's slug.
    pub name: String,
    /// Its steps, in order.
    pub steps: Vec<Step>,
}

/// What to set to turn the iOS pipelines on.
pub fn off_note() -> &'static str {
    "iOS pipelines are off: set `ios` in the Buildkite settings (src/ci/src/pigeon.rs) and add a macOS queue; see docs/plans/ios.md"
}

/// The build and release pipelines, each validated. `None` is the off state and answers
/// [`off_note`]. Any problem is returned, all listed.
pub fn pipelines(ios: Option<&Ios>) -> Result<(Pipeline, Pipeline), String> {
    let ios = ios.ok_or_else(|| off_note().to_string())?;
    let build = build_pipeline(ios);
    let release = release_pipeline(ios);
    let macos = [ios.queue.as_str(), ios.sign_queue.as_str()];
    let sign = [ios.sign_queue.as_str()];
    let mut problems = Vec::new();
    for p in [&build, &release] {
        for e in validate(p, &macos, &sign) {
            problems.push(format!("{}: {e}", p.name));
        }
    }
    if problems.is_empty() {
        Ok((build, release))
    } else {
        Err(problems.join("; "))
    }
}

/// The platform bootstrap command, as `steps_yaml` runs it.
fn bootstrap(host: &str) -> String {
    format!("curl -fsSL --retry 3 https://{host}/ci/buildkite.sh | sh")
}

/// The build pipeline: one step, for session branches before they land and for main.
pub fn build_pipeline(ios: &Ios) -> Pipeline {
    Pipeline {
        name: ios.build_pipeline.clone(),
        steps: vec![Step {
            key: "ios-check".into(),
            label: "iOS build and tests".into(),
            queue: ios.queue.clone(),
            command: bootstrap(&ios.host),
            depends_on: vec![],
            timeout_min: 60,
            branches: None,
            concurrency_group: None,
            concurrency: None,
            artifact_paths: vec![],
            env: vec![("PIGEON_JOB_KIND".into(), "ios-check".into())],
        }],
    }
}

/// The release pipeline: an unsigned archive, then signing and upload to TestFlight on the
/// signing queue, one at a time, from the release branch only.
pub fn release_pipeline(ios: &Ios) -> Pipeline {
    let archive = Step {
        key: "ios-archive".into(),
        label: "iOS archive, unsigned".into(),
        queue: ios.queue.clone(),
        command: bootstrap(&ios.host),
        depends_on: vec![],
        timeout_min: 60,
        branches: Some(ios.release_branch.clone()),
        concurrency_group: None,
        concurrency: None,
        artifact_paths: vec!["ios/build/Pigeon.xcarchive.zip".into()],
        env: vec![("PIGEON_JOB_KIND".into(), "ios-archive".into())],
    };
    // The sign bootstrap (`/ci/buildkite-sign.sh`) is designed in docs/plans/ios.md and is not
    // built yet.
    let sign = Step {
        key: "ios-sign-upload".into(),
        label: "iOS sign and upload to TestFlight".into(),
        queue: ios.sign_queue.clone(),
        command: format!(
            "curl -fsSL --retry 3 https://{}/ci/buildkite-sign.sh | sh",
            ios.host
        ),
        depends_on: vec!["ios-archive".into()],
        timeout_min: 30,
        branches: Some(ios.release_branch.clone()),
        concurrency_group: Some(format!("{}/sign", ios.release_pipeline)),
        concurrency: Some(1),
        artifact_paths: vec![],
        env: vec![("PIGEON_JOB_KIND".into(), "ios-sign-upload".into())],
    };
    Pipeline {
        name: ios.release_pipeline.clone(),
        steps: vec![archive, sign],
    }
}

/// Every string in a step, for the `$` check.
fn strings(s: &Step) -> Vec<&str> {
    let mut v = vec![
        s.key.as_str(),
        s.label.as_str(),
        s.queue.as_str(),
        s.command.as_str(),
    ];
    v.extend(s.depends_on.iter().map(String::as_str));
    v.extend(s.branches.as_deref());
    v.extend(s.concurrency_group.as_deref());
    v.extend(s.artifact_paths.iter().map(String::as_str));
    for (k, val) in &s.env {
        v.push(k);
        v.push(val);
    }
    v
}

/// The problems with a pipeline, empty when it is fine. Queues must be in `macos_queues`.
/// Steps on a queue in `sign_queues` must have concurrency 1 and a `branches` value, since
/// signing runs one at a time and only from the release branch. No string may hold `$`,
/// which Buildkite interpolates when steps are uploaded.
pub fn validate(p: &Pipeline, macos_queues: &[&str], sign_queues: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    if p.steps.is_empty() {
        out.push("the pipeline has no steps".to_string());
    }
    if p.name.contains('$') {
        out.push("the pipeline name holds `$`".to_string());
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for s in &p.steps {
        let at = format!("step `{}`", s.key);
        if s.key.is_empty() {
            out.push("a step key is empty".to_string());
        } else if !s
            .key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            out.push(format!("{at}: key must be only a-z, 0-9 and `-`"));
        }
        if !seen.insert(s.key.as_str()) {
            out.push(format!("{at}: duplicate key"));
        }
        if s.label.is_empty() {
            out.push(format!("{at}: label is empty"));
        }
        if s.label.contains(':') {
            out.push(format!("{at}: label holds `:`"));
        }
        if !macos_queues.contains(&s.queue.as_str()) {
            out.push(format!("{at}: queue `{}` is not a macOS queue", s.queue));
        }
        for d in &s.depends_on {
            // `seen` holds this step's own key too, so exclude it.
            if d == &s.key || !seen.contains(d.as_str()) {
                out.push(format!("{at}: depends_on `{d}` is not an earlier step"));
            }
        }
        if !(1..=120).contains(&s.timeout_min) {
            out.push(format!("{at}: timeout_min must be 1 to 120"));
        }
        if strings(s).iter().any(|t| t.contains('$')) {
            out.push(format!("{at}: a string holds `$`"));
        }
        if s.concurrency.is_some() != s.concurrency_group.is_some() {
            out.push(format!(
                "{at}: concurrency and concurrency_group must be set together"
            ));
        }
        if s.concurrency == Some(0) {
            out.push(format!("{at}: concurrency must be at least 1"));
        }
        if sign_queues.contains(&s.queue.as_str()) {
            if s.concurrency != Some(1) {
                out.push(format!("{at}: a signing step must have concurrency 1"));
            }
            if s.branches.is_none() {
                out.push(format!("{at}: a signing step must be limited to a branch"));
            }
        }
    }
    out
}

/// A string as a YAML double-quoted scalar. JSON escaping is valid YAML here.
fn q(s: &str) -> String {
    // Serializing a string cannot fail; the fallback keeps this free of panics.
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// The pipeline as deterministic block YAML, in `steps_yaml`'s style. Every string is a
/// JSON-escaped double-quoted scalar, so no value can break the shape. It ends with one newline.
pub fn yaml(p: &Pipeline) -> String {
    let mut o = String::from("steps:\n");
    for s in &p.steps {
        o.push_str(&format!("  - label: {}\n", q(&s.label)));
        o.push_str(&format!("    key: {}\n", q(&s.key)));
        if !s.depends_on.is_empty() {
            let l: Vec<String> = s.depends_on.iter().map(|d| q(d)).collect();
            o.push_str(&format!("    depends_on: [{}]\n", l.join(", ")));
        }
        if let Some(b) = &s.branches {
            o.push_str(&format!("    branches: {}\n", q(b)));
        }
        o.push_str(&format!("    agents:\n      queue: {}\n", q(&s.queue)));
        o.push_str("    checkout:\n      skip: true\n");
        o.push_str(&format!("    command: {}\n", q(&s.command)));
        if !s.env.is_empty() {
            o.push_str("    env:\n");
            for (k, v) in &s.env {
                o.push_str(&format!("      {k}: {}\n", q(v)));
            }
        }
        if !s.artifact_paths.is_empty() {
            let l: Vec<String> = s.artifact_paths.iter().map(|d| q(d)).collect();
            o.push_str(&format!("    artifact_paths: [{}]\n", l.join(", ")));
        }
        o.push_str(&format!("    timeout_in_minutes: {}\n", s.timeout_min));
        if let Some(g) = &s.concurrency_group {
            o.push_str(&format!("    concurrency_group: {}\n", q(g)));
        }
        if let Some(c) = s.concurrency {
            o.push_str(&format!("    concurrency: {c}\n"));
        }
    }
    o
}

/// Whether `k` is a YAML key this generator writes: letters, digits and `_`, not starting with
/// a digit. Lower case is the rule for fixed keys; env names are upper case.
fn key_ok(k: &str) -> bool {
    !k.is_empty()
        && !k.starts_with(|c: char| c.is_ascii_digit())
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A light structural check of rendered text, since there is no YAML parser: the first line is
/// `steps:`, no tabs, even indents, every list item starts `- `, and every other line is `key:`
/// or `key: value`. Answers the offending lines.
pub fn shape(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    if yaml.lines().next() != Some("steps:") {
        out.push("the first line is not `steps:`".to_string());
    }
    if !yaml.ends_with('\n') {
        out.push("no final newline".to_string());
    }
    for (i, line) in yaml.lines().enumerate().skip(1) {
        let bad = line.contains('\t') || {
            let body = line.trim_start_matches(' ');
            let indent = line.len() - body.len();
            let rest = line.strip_prefix("  - ").unwrap_or(body);
            let is_item = body.starts_with('-');
            let key = match rest.split_once(':') {
                Some((k, tail)) if tail.is_empty() || tail.starts_with(' ') => Some(k),
                _ => None,
            };
            indent % 2 != 0 || (is_item && !line.starts_with("  - ")) || !key.is_some_and(key_ok)
        };
        if bad {
            out.push(format!("line {}: {line}", i + 1));
        }
    }
    out
}

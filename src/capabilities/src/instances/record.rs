//! The shape of an instance record.
//!
//! An instance is a named unit of what the platform runs. It may live in another Cloudflare account that a person linked.
//! Code only declares the shape. Every value is data that a person approves.
//! This module checks records, digests them and diffs them.
//! A passkey approval is bound to the digest.
//! The person sees the diff before approving.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A named unit of what the platform runs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    /// The instance id, such as "pigeon-security/platform" or "noisypigeon".
    pub id: String,
    /// A short human readable title.
    pub title: String,
    /// The Cloudflare account the instance lives in.
    pub account: AccountRef,
    /// The DNS zones this instance may serve hostnames in.
    #[serde(default)]
    pub zones: Vec<String>,
    /// The source repository, if there is one.
    #[serde(default)]
    pub repo: Option<RepoRef>,
    /// The sites that belong to this instance.
    #[serde(default)]
    pub sites: Vec<SiteRecord>,
    /// The one Worker in the instance's account that serves all its sites, on its zone, its
    /// pigeon.dev subdomain or both: by default [`worker_for`], such as `noisypigeon`.
    #[serde(default)]
    pub worker: Option<String>,
    /// Whether `worker` may differ from the name [`worker_for`] gives. The person sees this in
    /// the diff they approve.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worker_override: bool,
    /// Version pins: a component name to the requirement the instance holds it to, such as
    /// `{"blog": "^1"}`. Set only by the person approving the record. Absent when empty.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pins: std::collections::BTreeMap<String, String>,
    /// The record version, which is not part of a diff.
    #[serde(default)]
    pub version: u64,
}

/// Which Cloudflare account an instance lives in.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountRef {
    /// The account the platform itself runs in.
    Home,
    /// An account that a person linked.
    Linked {
        /// The 32 character account id, once it is known.
        #[serde(default)]
        account_id: Option<String>,
    },
}

/// Where the source of an instance lives.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepoRef {
    /// A repository kept in Artifacts.
    Artifacts {
        /// The Artifacts namespace.
        namespace: String,
        /// The repository name.
        repo: String,
    },
    /// A repository on GitHub.
    Github {
        /// The repository as "owner/name".
        full_name: String,
        /// The GitHub App installation id, if known.
        #[serde(default)]
        installation: Option<u64>,
    },
}

/// One site that an instance runs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SiteRecord {
    /// The site name, unique within the instance.
    pub name: String,
    /// The bucket that holds the site files: by default [`bucket_for`] the instance, such as
    /// `noisypigeon-site`. An instance's sites share it.
    pub bucket: String,
    /// Whether `bucket` may differ from the name [`bucket_for`] gives. The person sees this in
    /// the diff they approve.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bucket_override: bool,
    /// The bucket jurisdiction, either "eu" or "fedramp".
    #[serde(default)]
    pub jurisdiction: Option<String>,
    /// A workload path in the repo, such as "workloads/noisypigeon.com/blog".
    #[serde(default)]
    pub build: Option<String>,
    /// Where the site is published.
    #[serde(default)]
    pub targets: Vec<Target>,
}

/// A place where a site is published.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// A preview of the site.
    Preview,
    /// A hostname in one of the instance zones.
    Zone {
        /// The full hostname.
        hostname: String,
        /// The Worker route, which starts with the hostname and a slash.
        route: String,
        /// Whether the route is switched on.
        #[serde(default)]
        enabled: bool,
    },
    /// A hostname in one of the instance zones, served by the instance's Worker as a Workers
    /// Custom Domain: Cloudflare creates and owns its DNS record and certificate.
    Domain {
        /// The full hostname.
        hostname: String,
    },
    /// A subdomain of the platform zone.
    Subdomain {
        /// The single DNS label.
        label: String,
    },
}

/// What validation is checked against.
#[derive(Clone, Copy, Debug)]
pub struct Rules<'a> {
    /// The zone that belongs to the platform.
    pub platform_zone: &'a str,
    /// Subdomain labels that nobody may take.
    pub reserved_labels: &'a [String],
    /// Worker names that nobody may take, such as `pigeon-platform`.
    pub reserved_workers: &'a [String],
}

/// One difference between two records.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Change {
    /// The path of the value, such as "sites[0].targets[1].label".
    pub path: String,
    /// The old value, or null when it was absent.
    pub from: Value,
    /// The new value, or null when it is now absent.
    pub to: Value,
}

/// Tells whether a string is a DNS label of 1 to 63 characters.
pub fn label(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Tells whether a string is a DNS name with at least two labels.
pub fn dns_name(s: &str) -> bool {
    (1..=253).contains(&s.len()) && s.split('.').count() >= 2 && s.split('.').all(label)
}

fn repo_part(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn build_path(s: &str) -> bool {
    (1..=200).contains(&s.len())
        && !s.starts_with('/')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
        && s.split('/').all(|seg| !seg.is_empty() && seg != "..")
}

fn check_id(instance: &Instance, problems: &mut Vec<String>) {
    let id = &instance.id;
    let len = id.chars().count();
    if !(1..=64).contains(&len) {
        problems.push(format!(
            "The id must be 1 to 64 characters long, but it is {len} characters."
        ));
    }
    if id.matches('/').count() > 1 {
        problems.push(format!("The id \"{id}\" may contain at most one slash."));
    }
    if !id.is_empty() && !id.split('/').all(label) {
        problems.push(format!(
            "The id \"{id}\" must be made of one or two parts split by a slash, and each part may only use lowercase letters, digits and dashes, and must not start or end with a dash."
        ));
    }
}

fn check_title(instance: &Instance, problems: &mut Vec<String>) {
    let len = instance.title.chars().count();
    if !(1..=80).contains(&len) {
        problems.push(format!(
            "The title must be 1 to 80 characters long, but it is {len} characters."
        ));
    }
    if instance.title.chars().any(char::is_control) {
        problems.push("The title must not contain control characters.".to_string());
    }
}

fn check_account(instance: &Instance, problems: &mut Vec<String>) {
    if let AccountRef::Linked {
        account_id: Some(a),
    } = &instance.account
    {
        let ok = a.len() == 32 && a.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !ok {
            problems.push(format!(
                "The account.account_id \"{a}\" must be 32 lowercase hex characters."
            ));
        }
    }
}

fn check_zones(instance: &Instance, rules: &Rules, problems: &mut Vec<String>) {
    let linked = matches!(instance.account, AccountRef::Linked { .. });
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (i, zone) in instance.zones.iter().enumerate() {
        if !dns_name(zone) {
            problems.push(format!(
                "The zones[{i}] value \"{zone}\" is not a valid DNS name."
            ));
        }
        if !seen.insert(zone.as_str()) {
            problems.push(format!(
                "The zones[{i}] value \"{zone}\" is listed more than once."
            ));
        }
        if linked && zone == rules.platform_zone {
            problems.push(format!(
                "The zones[{i}] value \"{zone}\" is the platform zone, which a linked instance may not list."
            ));
        }
    }
}

fn check_repo(instance: &Instance, problems: &mut Vec<String>) {
    match &instance.repo {
        None => {}
        Some(RepoRef::Artifacts { namespace, repo }) => {
            if !label(namespace) {
                problems.push(format!(
                    "The repo.namespace \"{namespace}\" must be a DNS label of lowercase letters, digits and dashes."
                ));
            }
            if !label(repo) {
                problems.push(format!(
                    "The repo.repo \"{repo}\" must be a DNS label of lowercase letters, digits and dashes."
                ));
            }
        }
        Some(RepoRef::Github { full_name, .. }) => match full_name.split_once('/') {
            Some((owner, name)) if repo_part(owner) && repo_part(name) => {}
            _ => problems.push(format!(
                "The repo.full_name \"{full_name}\" must look like owner/name, where each part is 1 to 100 letters, digits, dashes, underscores or dots."
            )),
        },
    }
}

/// The bucket an instance's sites live in by default: the instance's name, the same value as
/// its Worker, then `-site`. `noisypigeon` gives `noisypigeon-site`; a subdomain-only instance
/// `foo` gives `foo-site`. Adding or removing a zone or subdomain later never renames it.
pub fn bucket_for(instance: &Instance) -> Option<String> {
    let name = match &instance.worker {
        Some(w) => Some(w.clone()),
        None => worker_for(instance).ok().flatten(),
    };
    name.map(|n| format!("{n}-site"))
}

fn check_bucket(
    path: &str,
    site: &SiteRecord,
    instance: &Instance,
    others: &[Instance],
    problems: &mut Vec<String>,
) {
    let name = &site.bucket;
    if !(3..=63).contains(&name.len()) || !label(name) {
        problems.push(format!(
            "The {path} \"{name}\" must be 3 to 63 characters of lowercase letters, digits and dashes."
        ));
        return;
    }
    for other in others {
        if other.id != instance.id && other.sites.iter().any(|s| s.bucket == *name) {
            problems.push(format!(
                "The {path} \"{name}\" is already used by instance {}.",
                other.id
            ));
        }
    }
    if site.bucket_override {
        return;
    }
    match bucket_for(instance) {
        Some(expected) if expected != *name => problems.push(format!(
            "The {path} \"{name}\" should be \"{expected}\", named after the instance like its Worker; set bucket_override to keep another name."
        )),
        None => problems.push(format!(
            "The {path} is named after the instance, but it has no zone or subdomain to name it after yet."
        )),
        _ => {}
    }
}

/// The name of an instance's Worker, from what it serves: its first zone without the TLD
/// (`noisypigeon.com` gives `noisypigeon`, `foo.co.uk` gives `foo-co`), else its first pigeon.dev
/// subdomain label. `Err` when the zone and a subdomain label name the instance differently, for
/// the person to choose; `Ok(None)` when it serves nothing yet.
pub fn worker_for(instance: &Instance) -> Result<Option<String>, String> {
    let from_zone = instance.zones.first().and_then(|z| {
        z.rsplit_once('.')
            .map(|(rest, _tld)| rest.replace('.', "-"))
    });
    let labels: Vec<&String> = instance
        .sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .filter_map(|t| match t {
            Target::Subdomain { label } => Some(label),
            _ => None,
        })
        .collect();
    match from_zone {
        Some(name) => match labels.iter().find(|l| ***l != name) {
            Some(other) => Err(format!(
                "the zone names this instance's Worker \"{name}\" but the subdomain is \"{other}\": make them the same, or set worker and worker_override"
            )),
            None => Ok(Some(name)),
        },
        None => Ok(labels.first().map(|l| (*l).clone())),
    }
}

fn check_worker(
    instance: &Instance,
    others: &[Instance],
    rules: &Rules,
    problems: &mut Vec<String>,
) {
    let derived = worker_for(instance);
    let Some(name) = &instance.worker else {
        if !instance.sites.is_empty() {
            match derived {
                Ok(Some(n)) => problems.push(format!(
                    "The worker is missing: it should be \"{n}\", one Worker for the whole instance."
                )),
                Ok(None) => problems.push(
                    "The worker is missing, and the sites have no zone or subdomain to name it after.".into(),
                ),
                Err(why) => problems.push(format!("The worker: {why}.")),
            }
        }
        return;
    };
    if !label(name) {
        problems.push(format!(
            "The worker \"{name}\" must be 1 to 63 characters of lowercase letters, digits and dashes."
        ));
    }
    if rules.reserved_workers.iter().any(|r| r == name) {
        problems.push(format!(
            "The worker \"{name}\" is the platform's own and cannot be used."
        ));
    }
    for other in others {
        if other.id != instance.id && other.worker.as_deref() == Some(name.as_str()) {
            problems.push(format!(
                "The worker \"{name}\" is already used by instance {}.",
                other.id
            ));
        }
    }
    if !instance.worker_override {
        match derived {
            Ok(Some(n)) if n != *name => problems.push(format!(
                "The worker \"{name}\" should be \"{n}\", named after the instance's zone or subdomain; set worker_override to keep another name."
            )),
            Err(why) => problems.push(format!("The worker: {why}.")),
            _ => {}
        }
    }
}

fn uses_label(instance: &Instance, wanted: &str) -> bool {
    instance.sites.iter().any(|s| {
        s.targets
            .iter()
            .any(|t| matches!(t, Target::Subdomain { label } if label == wanted))
    })
}

fn check_target(
    path: &str,
    target: &Target,
    instance: &Instance,
    others: &[Instance],
    rules: &Rules,
    used_labels: &mut Vec<String>,
    problems: &mut Vec<String>,
) {
    match target {
        Target::Preview => {}
        Target::Zone {
            hostname, route, ..
        } => {
            if !dns_name(hostname) {
                problems.push(format!(
                    "The {path}.hostname \"{hostname}\" is not a valid DNS name."
                ));
            } else {
                let inside = instance
                    .zones
                    .iter()
                    .any(|z| hostname == z || hostname.ends_with(&format!(".{z}")));
                if !inside {
                    problems.push(format!(
                        "The {path}.hostname: hostname {hostname} is not in this instance's zones."
                    ));
                }
            }
            if !route.starts_with(&format!("{hostname}/")) {
                problems.push(format!(
                    "The {path}.route \"{route}\" must start with \"{hostname}/\"."
                ));
            }
        }
        Target::Domain { hostname } => {
            if !dns_name(hostname) {
                problems.push(format!(
                    "The {path}.hostname \"{hostname}\" is not a valid DNS name."
                ));
            } else if !instance
                .zones
                .iter()
                .any(|z| hostname == z || hostname.ends_with(&format!(".{z}")))
            {
                problems.push(format!(
                    "The {path}.hostname: hostname {hostname} is not in this instance's zones."
                ));
            }
        }
        Target::Subdomain { label: sub } => {
            if !label(sub) {
                problems.push(format!(
                    "The {path}.label \"{sub}\" must be a DNS label of lowercase letters, digits and dashes."
                ));
            }
            if rules.reserved_labels.iter().any(|r| r == sub) {
                problems.push(format!(
                    "The {path}.label \"{sub}\" is reserved and cannot be used."
                ));
            }
            if used_labels.iter().any(|u| u == sub) {
                problems.push(format!(
                    "The {path}.label \"{sub}\" is used more than once in this instance."
                ));
            } else {
                used_labels.push(sub.clone());
            }
            for other in others {
                if other.id != instance.id && uses_label(other, sub) {
                    problems.push(format!(
                        "The {path}.label: the subdomain {sub} is already used by instance {}.",
                        other.id
                    ));
                }
            }
        }
    }
}

fn check_sites(
    instance: &Instance,
    others: &[Instance],
    rules: &Rules,
    problems: &mut Vec<String>,
) {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    let mut used_labels: Vec<String> = Vec::new();
    for (i, site) in instance.sites.iter().enumerate() {
        let p = format!("sites[{i}]");
        if !label(&site.name) {
            problems.push(format!(
                "The {p}.name \"{}\" must be a DNS label of lowercase letters, digits and dashes.",
                site.name
            ));
        }
        if !names.insert(site.name.as_str()) {
            problems.push(format!(
                "The {p}.name \"{}\" is used by more than one site in this instance.",
                site.name
            ));
        }
        check_bucket(&format!("{p}.bucket"), site, instance, others, problems);
        if let Some(j) = &site.jurisdiction
            && j != "eu"
            && j != "fedramp"
        {
            problems.push(format!(
                "The {p}.jurisdiction \"{j}\" must be \"eu\" or \"fedramp\"."
            ));
        }
        if let Some(b) = &site.build
            && !build_path(b)
        {
            problems.push(format!(
                "The {p}.build \"{b}\" must be a relative path of 1 to 200 letters, digits, dots, underscores, dashes and slashes, with no empty or \"..\" parts."
            ));
        }
        let mut previews = 0usize;
        for (j, target) in site.targets.iter().enumerate() {
            if matches!(target, Target::Preview) {
                previews += 1;
                if previews == 2 {
                    problems.push(format!(
                        "The {p}.targets list has more than one preview target."
                    ));
                }
            }
            check_target(
                &format!("{p}.targets[{j}]"),
                target,
                instance,
                others,
                rules,
                &mut used_labels,
                problems,
            );
        }
    }
}

/// Finds every problem with a record. An empty list means the record is valid.
pub fn validate(instance: &Instance, others: &[Instance], rules: &Rules) -> Vec<String> {
    let mut problems = Vec::new();
    check_id(instance, &mut problems);
    check_title(instance, &mut problems);
    check_account(instance, &mut problems);
    check_zones(instance, rules, &mut problems);
    check_repo(instance, &mut problems);
    check_sites(instance, others, rules, &mut problems);
    check_worker(instance, others, rules, &mut problems);
    if others.iter().any(|o| o.id == instance.id) {
        problems.push(format!(
            "An instance with id {} already exists.",
            instance.id
        ));
    }
    problems
}

/// Returns the base64url SHA-256 of the record as JSON.
pub fn digest(instance: &Instance) -> String {
    pigeon_core::oauth::hash(&serde_json::to_string(instance).unwrap_or_default())
}

fn flatten(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, child, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), child, out);
            }
        }
        leaf => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), leaf.clone());
            }
        }
    }
}

fn leaves(instance: &Instance) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let value = serde_json::to_value(instance).unwrap_or(Value::Null);
    flatten("", &value, &mut out);
    out.remove("version");
    out
}

/// Lists the leaf values that differ between two records, sorted by path.
pub fn diff(old: Option<&Instance>, new: &Instance) -> Vec<Change> {
    let old_leaves = old.map(leaves).unwrap_or_default();
    let new_leaves = leaves(new);
    let keys: BTreeSet<&String> = old_leaves.keys().chain(new_leaves.keys()).collect();
    let mut changes = Vec::new();
    for key in keys {
        let from = old_leaves.get(key).cloned().unwrap_or(Value::Null);
        let to = new_leaves.get(key).cloned().unwrap_or(Value::Null);
        if from != to {
            changes.push(Change {
                path: key.clone(),
                from,
                to,
            });
        }
    }
    changes
}

/// Lists every subdomain target as label, instance id and site name, sorted by label.
pub fn subdomains(instances: &[Instance]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for instance in instances {
        for site in &instance.sites {
            for target in &site.targets {
                if let Target::Subdomain { label } = target {
                    out.push((label.clone(), instance.id.clone(), site.name.clone()));
                }
            }
        }
    }
    out.sort();
    out
}

/// Lists the labels that nobody may take as a subdomain of the zone.
///
/// It holds the first label of each hostname directly under the zone, plus "www", "mcp", "proxy" and "platform".
pub fn reserved_labels(hostnames: &[String], zone: &str) -> Vec<String> {
    let suffix = format!(".{}", zone.to_ascii_lowercase());
    let mut out: Vec<String> = ["www", "mcp", "proxy", "platform"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for host in hostnames {
        let host = host.to_ascii_lowercase();
        if let Some(prefix) = host.strip_suffix(&suffix)
            && !prefix.is_empty()
            && !prefix.contains('.')
        {
            out.push(prefix.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Builds the full hostname for a subdomain label in a zone.
pub fn subdomain_host(label: &str, zone: &str) -> String {
    format!("{label}.{zone}")
}

/// Finds the instance and site whose subdomain matches a host, ignoring ASCII case.
pub fn site_for_host<'a>(
    instances: &'a [Instance],
    host: &str,
    zone: &str,
) -> Option<(&'a Instance, &'a SiteRecord)> {
    for instance in instances {
        for site in &instance.sites {
            for target in &site.targets {
                if let Target::Subdomain { label } = target
                    && subdomain_host(label, zone).eq_ignore_ascii_case(host)
                {
                    return Some((instance, site));
                }
            }
        }
    }
    None
}

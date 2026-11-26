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
    /// The instance id, such as "noisypigeon" or "pigeon-security/platform". Only the home
    /// instance's may hold a dot, and only as one of its zones: `pigeon.dev`, which R1's
    /// migration (`instance_rename_home`) renames it to.
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
    /// The site that `site_deploy` and `site_rollback` publish. Absent: nothing is publishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publishing: Option<pigeon_core::site_publish::Profile>,
    /// Optional read-only access to a declared object storage account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<crate::storage::scope::Profile>,
    /// Opt in to read-only tools for this record's approved repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Toggle>,
    /// Optional read-only CI visibility for this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci: Option<crate::ci::Profile>,
    /// Optional read-only metrics for the approved Worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability: Option<Toggle>,
    /// Optional approved ingestion source and destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest: Option<crate::ingest::Profile>,
    /// Hostnames in the instance's zones that answer with an HTTP redirect to another URL, served
    /// by the instance's own Worker (docs/plans/redirects.md). Absent when empty, so the digests
    /// of records without it do not change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirects: Vec<Redirect>,
    /// Where the platform itself (console, MCP, OAuth) is served: only the home instance names
    /// it, and it is platform-owned, so plans never touch it. Absent when unset, so the digests
    /// of records without it do not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<PlatformRef>,
    /// The catalogue capabilities (`catalogue::CATALOGUE`) the instance enables beyond its
    /// profile fields, by name, each with its settings (docs/plans/capability-scoping.md, M2).
    /// Absent when empty, so the digests of records without it do not change. Checked by
    /// `catalogue::problems`: no home-only capability off home, none on a linked instance until
    /// M4 turns each on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub capabilities: BTreeMap<String, CapabilitySettings>,
    /// The record version, which is not part of a diff.
    #[serde(default)]
    pub version: u64,
}

/// An opt-in profile with no settings of its own, serialized as `{"state": "enabled"}` or
/// `{"state": "disabled"}`. Record digests cover that form, so it must not change.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Toggle {
    Enabled,
    Disabled,
}

impl Toggle {
    pub fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// One hostname that answers with a redirect: `host` (in one of the instance's zones) sends
/// every request to `to`, an absolute `https://` URL, with `status` (301 unless set), appending
/// the request's path and query unless `keep_path` is false. Unset fields stay absent, so the
/// record reads back as the person wrote it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Redirect {
    pub host: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_path: Option<bool>,
}

impl Redirect {
    /// The status it answers with: 301 unless set.
    pub fn status(&self) -> u16 {
        self.status.unwrap_or(301)
    }

    /// Whether the request's path and query are appended to `to`: yes unless set to false.
    pub fn keep_path(&self) -> bool {
        self.keep_path.unwrap_or(true)
    }
}

/// The most redirects one record may hold.
pub const MAX_REDIRECTS: usize = 20;
/// The statuses a redirect may answer with.
pub const REDIRECT_STATUSES: [u16; 4] = [301, 302, 307, 308];

/// One catalogue capability's settings on an instance record (docs/plans/capability-scoping.md,
/// M3): where it runs. Absent fields stay absent, so `{}` reads back as `{}` and a record's
/// digest does not change.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySettings {
    /// Where the capability runs for this instance: `inline` or `own_worker`, one of the
    /// catalogue's offered placements. On home it is read-only and must say what is deployed
    /// (`catalogue::Entry::home`); on a linked instance only `inline` is accepted for now.
    /// Absent: the catalogue's default for the instance (`catalogue::placement`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<crate::catalogue::Placement>,
    /// The expected team identifier for the DigitalOcean connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// The optional read-only DigitalOcean Spaces source binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spaces: Option<pigeon_cloud::digitalocean_spaces::Binding>,
    /// Whether this instance permits approved temporary Droplet health checks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub droplet_health: bool,
}

/// Where the home instance's platform (console, MCP, OAuth) lives.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlatformRef {
    /// Such as `platform.pigeon.dev`: a platform-owned hostname of the platform zone.
    pub host: String,
}

/// The one template a record may name for a site today: the landing page, which alone may
/// serve the home instance's apex. The same as `pigeon_landing::TEMPLATE` (the smoke suite
/// checks it), without depending on the template crate.
pub const LANDING_TEMPLATE: &str = "landing@1";

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
    /// How visitors sign in: absent (public) or "passkey". Only a site on platform subdomains
    /// may set it, as only pigeon-sites can gate those.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Who may sign in when `auth` is set: user ids, or "site" for every site user of the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// The template the site is built from, such as `landing@1` ([`LANDING_TEMPLATE`]). Absent
    /// when unset, so the digests of records without it do not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

impl SiteRecord {
    /// Whether the site is built from the landing template.
    pub fn landing(&self) -> bool {
        self.template.as_deref() == Some(LANDING_TEMPLATE)
    }
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
    /// The Worker the platform itself runs as, `pigeon-platform`. Only the home instance may
    /// name it as its `worker`, as a platform-owned Worker that plans never touch.
    pub platform_worker: &'a str,
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
    // R1: the home instance may be named for one of its zones, such as `pigeon.dev`.
    if id.contains('.') && !id.contains('/') {
        if instance.account != AccountRef::Home {
            problems.push(format!(
                "The id \"{id}\" holds a dot, which only the home instance's may, as one of its zones."
            ));
        } else if !dns_name(id) || !instance.zones.iter().any(|z| z == id) {
            problems.push(format!(
                "The id \"{id}\" holds a dot, so it must be one of the home instance's zones ({}).",
                instance.zones.join(", ")
            ));
        }
        return;
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

/// Whether two account references are the same kind: both home or both linked.
fn same_kind(a: &AccountRef, b: &AccountRef) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// At most one instance runs in the platform's own account.
fn check_home(instance: &Instance, others: &[Instance], problems: &mut Vec<String>) {
    if instance.account != AccountRef::Home {
        return;
    }
    for other in others
        .iter()
        .filter(|o| o.id != instance.id && o.account == AccountRef::Home)
    {
        problems.push(format!(
            "The account kind is home, but instance {} already runs in the platform's own account, and there is only one home instance.",
            other.id
        ));
    }
}

/// Why `next` may not replace `current`: a record's account kind (home or linked) never
/// changes once it is stored. `None` when it may.
pub fn account_change(current: Option<&Instance>, next: &Instance) -> Option<String> {
    let current = current?;
    let name = |a: &AccountRef| match a {
        AccountRef::Home => "home",
        AccountRef::Linked { .. } => "linked",
    };
    (!same_kind(&current.account, &next.account)).then(|| {
        format!(
            "The account kind of instance {} is {}, and a draft may not change it to {}.",
            next.id,
            name(&current.account),
            name(&next.account)
        )
    })
}

/// What approving `next` would break in the stored set `stored` (every instance, `next`'s own
/// stored version among them): a second home instance, or a changed account kind. Checked again
/// when a draft is approved, against what is stored then.
pub fn approval_problems(next: &Instance, stored: &[Instance]) -> Vec<String> {
    let mut problems = Vec::new();
    check_home(next, stored, &mut problems);
    if let Some(why) = account_change(stored.iter().find(|i| i.id == next.id), next) {
        problems.push(why);
    }
    problems
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
        if !instance.sites.is_empty() || !instance.redirects.is_empty() {
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
    let platform_own = platform_worker(instance, rules);
    if !platform_own
        && (name == rules.platform_worker || rules.reserved_workers.iter().any(|r| r == name))
    {
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
    if !instance.worker_override && !platform_own {
        match derived {
            Ok(Some(n)) if n != *name => problems.push(format!(
                "The worker \"{name}\" should be \"{n}\", named after the instance's zone or subdomain; set worker_override to keep another name."
            )),
            Err(why) => problems.push(format!("The worker: {why}.")),
            _ => {}
        }
    }
}

/// Whether `instance` is the home instance naming the platform's own Worker as its `worker`: a
/// platform-owned Worker, not one an instance made, so plans never upload or attach to it.
pub fn platform_worker(instance: &Instance, rules: &Rules) -> bool {
    instance.account == AccountRef::Home
        && instance.worker.as_deref() == Some(rules.platform_worker)
}

/// The hostnames the platform owns in its zone: the apex and each reserved label under it
/// (`platform.`, `proxy.`, `mcp.`, `www.` and every hostname a capability declares there).
pub fn platform_hosts(rules: &Rules) -> Vec<String> {
    let zone = rules.platform_zone.to_ascii_lowercase();
    let mut out = vec![zone.clone()];
    out.extend(
        rules
            .reserved_labels
            .iter()
            .map(|l| subdomain_host(&l.to_ascii_lowercase(), &zone)),
    );
    out.sort();
    out.dedup();
    out
}

/// Whether `hostname` is the platform zone's apex, ignoring ASCII case.
pub fn is_apex(hostname: &str, rules: &Rules) -> bool {
    hostname.eq_ignore_ascii_case(rules.platform_zone)
}

/// The home instance's zone and domain targets may not name a platform-owned hostname, except
/// the apex as a custom domain of a site built from the landing template: the platform's Worker
/// serves it, and the plan keeps it as it is.
fn check_home_hosts(instance: &Instance, rules: &Rules, problems: &mut Vec<String>) {
    if instance.account != AccountRef::Home {
        return;
    }
    let owned = platform_hosts(rules);
    for (i, site) in instance.sites.iter().enumerate() {
        for (j, target) in site.targets.iter().enumerate() {
            let (Target::Zone { hostname, .. } | Target::Domain { hostname }) = target else {
                continue;
            };
            let landing_apex = site.landing()
                && matches!(target, Target::Domain { .. })
                && is_apex(hostname, rules);
            if !landing_apex && owned.contains(&hostname.to_ascii_lowercase()) {
                problems.push(format!(
                    "The sites[{i}].targets[{j}].hostname \"{hostname}\" is the platform's own, and the home instance may not serve it."
                ));
            }
        }
    }
}

/// Only the home instance names where the platform lives, and only as a platform-owned host of
/// the platform zone other than the apex.
fn check_platform(instance: &Instance, rules: &Rules, problems: &mut Vec<String>) {
    let Some(platform) = &instance.platform else {
        return;
    };
    if instance.account != AccountRef::Home {
        problems.push("Only the home instance may name the platform's host.".into());
        return;
    }
    let host = platform.host.to_ascii_lowercase();
    if !dns_name(&host) || is_apex(&host, rules) || !platform_hosts(rules).contains(&host) {
        problems.push(format!(
            "The platform.host \"{}\" must be one of the platform's own hostnames under {}, such as platform.{}, and not the apex.",
            platform.host, rules.platform_zone, rules.platform_zone
        ));
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

/// Reports problems with a site's sign-in settings.
fn check_auth(path: &str, site: &SiteRecord, problems: &mut Vec<String>) {
    let Some(auth) = &site.auth else {
        return;
    };
    if auth != "passkey" {
        problems.push(format!("The {path}.auth \"{auth}\" must be \"passkey\"."));
        return;
    }
    if site.allow.is_empty() {
        problems.push(format!(
            "The {path}.allow must list a user id or \"site\" when auth is \"passkey\"."
        ));
    }
    if site
        .targets
        .iter()
        .any(|t| !matches!(t, Target::Subdomain { .. }))
    {
        problems.push(format!(
            "The {path}.auth needs every target to be a platform subdomain; only pigeon-sites can gate those."
        ));
    }
}

fn check_sites(
    instance: &Instance,
    others: &[Instance],
    rules: &Rules,
    problems: &mut Vec<String>,
) {
    if instance.sites.len() > 1 && instance.sites.iter().any(|site| site.auth.is_some()) {
        problems.push("A protected origin needs a dedicated instance with one site.".into());
    }
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
        check_auth(&p, site, problems);
        if let Some(t) = &site.template
            && t != LANDING_TEMPLATE
        {
            problems.push(format!(
                "The {p}.template \"{t}\" must be \"{LANDING_TEMPLATE}\", the one template a record names."
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

/// Whether `host` is `zone` or a name under it.
fn in_zone(host: &str, zone: &str) -> bool {
    host == zone || host.ends_with(&format!(".{zone}"))
}

/// The hostnames an instance's sites are published at in its zones: its Zone and Domain
/// targets, lowercased.
fn site_hosts(instance: &Instance) -> Vec<String> {
    instance
        .sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .filter_map(|t| match t {
            Target::Zone { hostname, .. } | Target::Domain { hostname } => {
                Some(hostname.to_ascii_lowercase())
            }
            _ => None,
        })
        .collect()
}

/// A redirect's `to` split into its host and the rest (path, empty or starting with `/`), or
/// why it is not an absolute `https://` URL with a host, no user, query or fragment.
pub fn redirect_target(to: &str) -> Result<(String, String), String> {
    let rest = to
        .strip_prefix("https://")
        .ok_or_else(|| "must start with https://".to_string())?;
    if to.len() > 2048 {
        return Err("must be at most 2048 characters long".into());
    }
    if to
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\\'))
    {
        return Err("must not contain spaces, quotes, angle brackets or backslashes".into());
    }
    if to.contains(['?', '#']) {
        return Err("must not have a query or a fragment".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err("must not name a user".into());
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if !dns_name(&host) {
        return Err(format!(
            "names host \"{host}\", which is not a valid DNS name"
        ));
    }
    if let Some(p) = port
        && (p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("has a port that is not a number".into());
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err("must not have \"..\" in its path".into());
    }
    Ok((host, path.to_string()))
}

/// Reports problems with the record's redirects: at most [`MAX_REDIRECTS`]; each host a DNS
/// name in the instance's zones, listed once, not one of its sites' hostnames, not used by
/// another instance, and not platform-owned; each `to` an `https://` URL that does not point at
/// a redirect host of this instance (no loops); each status one of [`REDIRECT_STATUSES`].
fn check_redirects(
    instance: &Instance,
    others: &[Instance],
    rules: &Rules,
    problems: &mut Vec<String>,
) {
    if instance.capabilities.contains_key("redirects") {
        problems.push(
            "The capability \"redirects\" is set by the record's redirects list; leave it out of capabilities.".into(),
        );
    }
    if instance.redirects.is_empty() {
        return;
    }
    if instance.redirects.len() > MAX_REDIRECTS {
        problems.push(format!(
            "The redirects list holds {} entries, and at most {MAX_REDIRECTS} are allowed.",
            instance.redirects.len()
        ));
    }
    let sites = site_hosts(instance);
    let owned = if instance.account == AccountRef::Home {
        platform_hosts(rules)
    } else {
        Vec::new()
    };
    let hosts: Vec<String> = instance
        .redirects
        .iter()
        .map(|r| r.host.to_ascii_lowercase())
        .collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (i, r) in instance.redirects.iter().enumerate() {
        let p = format!("redirects[{i}]");
        let host = &r.host;
        if !dns_name(host) {
            problems.push(format!(
                "The {p}.host \"{host}\" is not a valid lowercase DNS name."
            ));
        } else if !instance.zones.iter().any(|z| in_zone(host, z)) {
            problems.push(format!(
                "The {p}.host: hostname {host} is not in this instance's zones."
            ));
        }
        if !seen.insert(host.as_str()) {
            problems.push(format!("The {p}.host \"{host}\" is listed more than once."));
        }
        if sites.contains(host) {
            problems.push(format!(
                "The {p}.host \"{host}\" is also a site target of this instance; a host either serves a site or redirects."
            ));
        }
        if owned.contains(host) || uses_platform_label(others, instance, host, rules) {
            problems.push(format!(
                "The {p}.host \"{host}\" is the platform's own, and the home instance may not redirect it."
            ));
        }
        for other in others.iter().filter(|o| o.id != instance.id) {
            if site_hosts(other).contains(host)
                || other
                    .redirects
                    .iter()
                    .any(|o| o.host.eq_ignore_ascii_case(host))
            {
                problems.push(format!(
                    "The {p}.host \"{host}\" is already used by instance {}.",
                    other.id
                ));
            }
        }
        match redirect_target(&r.to) {
            Err(why) => problems.push(format!("The {p}.to \"{}\" {why}.", r.to)),
            Ok((to_host, _)) if hosts.contains(&to_host) => problems.push(format!(
                "The {p}.to \"{}\" points back at {to_host}, a redirect host of this instance, which would loop.",
                r.to
            )),
            Ok(_) => {}
        }
        if !REDIRECT_STATUSES.contains(&r.status()) {
            problems.push(format!(
                "The {p}.status {} must be 301, 302, 307 or 308.",
                r.status()
            ));
        }
    }
}

/// Whether `host` is the home instance's platform subdomain of any instance (`label.zone`),
/// which pigeon-sites serves: never a redirect host.
fn uses_platform_label(
    others: &[Instance],
    instance: &Instance,
    host: &str,
    rules: &Rules,
) -> bool {
    let Some(label) = host.strip_suffix(&format!(".{}", rules.platform_zone)) else {
        return false;
    };
    others
        .iter()
        .chain(std::iter::once(instance))
        .any(|i| uses_label(i, label))
}

/// Finds every problem with a record. An empty list means the record is valid.
pub fn validate(instance: &Instance, others: &[Instance], rules: &Rules) -> Vec<String> {
    let mut problems = Vec::new();
    check_id(instance, &mut problems);
    check_title(instance, &mut problems);
    check_account(instance, &mut problems);
    check_home(instance, others, &mut problems);
    check_zones(instance, rules, &mut problems);
    check_repo(instance, &mut problems);
    check_sites(instance, others, rules, &mut problems);
    check_home_hosts(instance, rules, &mut problems);
    check_platform(instance, rules, &mut problems);
    check_worker(instance, others, rules, &mut problems);
    check_redirects(instance, others, rules, &mut problems);
    problems.extend(crate::catalogue::problems(instance));
    if let Some(pigeon_core::site_publish::Profile::Enabled { site }) = &instance.publishing {
        let named = instance.sites.iter().find(|record| record.name == *site);
        if named.is_none() {
            problems.push(format!("Publishing names an unknown site: {site}."));
        }
        if instance.account == AccountRef::Home {
            // The home instance publishes from its Artifacts repository, with no GitHub App
            // and no linked account, and only the landing template (instances/home_site.rs).
            if !matches!(instance.repo, Some(RepoRef::Artifacts { .. })) {
                problems.push(
                    "The home instance publishes from its Artifacts repository: set repo.kind to artifacts.".into(),
                );
            }
            if let Some(named) = named.filter(|n| !n.landing()) {
                problems.push(format!(
                    "The home instance publishes only the {LANDING_TEMPLATE} template, and site {} does not name it.",
                    named.name
                ));
            }
        } else if !matches!(instance.repo, Some(RepoRef::Github { .. }))
            || !matches!(
                instance.account,
                AccountRef::Linked {
                    account_id: Some(_)
                }
            )
        {
            problems.push("Publishing needs a GitHub repository and a linked account.".into());
        }
    }

    if let Some(profile) = instance
        .storage
        .as_ref()
        .filter(|profile| profile.enabled())
        && let Err(problem) = profile.scope()
    {
        problems.push(problem);
    }

    if instance.source.is_some_and(Toggle::enabled)
        && let Err(problem) = crate::source::read::approved_repo(instance)
    {
        problems.push(problem);
    }

    match crate::ci::scope(instance) {
        Ok(Some((repo, jobs))) => {
            for other in others.iter().filter(|other| other.id != instance.id) {
                if let Ok(Some((other_repo, other_jobs))) = crate::ci::scope(other)
                    && repo == other_repo
                    && jobs.iter().any(|job| other_jobs.contains(job))
                {
                    problems.push(format!("CI ownership overlaps with instance {}.", other.id));
                }
            }
        }
        Ok(None) => {}
        Err(problem) => problems.push(problem),
    }

    if let Err(problem) = crate::observability::instance::validate(instance) {
        problems.push(problem);
    }
    if let Some(profile) = &instance.ingest {
        if let Err(problem) = profile.validate() {
            problems.push(problem);
        }
        if profile.enabled()
            && !matches!(
                instance.account,
                AccountRef::Linked {
                    account_id: Some(_)
                }
            )
        {
            problems.push("Ingest requires an approved linked account.".into());
        }
    }

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

//! What an instance's resources in its linked account need, worked out from what is there and
//! what pigeon made (its ledger). Pure: the capability observes, this decides, the capability
//! applies. Anything with pigeon's name that pigeon did not make is a conflict and is never
//! touched; nothing here deletes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::record::{AccountRef, Instance, Rules};

/// The R2 binding the site Worker reads its bucket through.
pub const SITE_BINDING: &str = "SITE";
/// The site Worker's compatibility date.
pub const COMPATIBILITY_DATE: &str = "2026-11-01";
/// The site Worker: serves `live`'s manifest from the bucket.
pub const SITE_WORKER: &str = include_str!("site_worker.js");
pub const ORIGIN_KEY_BINDING: &str = "PIGEON_ORIGIN_KEY";

pub fn protected_site_worker() -> String {
    format!(
        "{}\n{}",
        SITE_WORKER.replacen("export default {", "const site = {", 1),
        include_str!("site_origin.js")
    )
}

/// What pigeon made in an instance's account, kept in the Directory under [`ledger_key`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    /// Buckets pigeon created.
    #[serde(default)]
    pub buckets: Vec<String>,
    /// Workers pigeon uploaded, with the digest of what it last uploaded.
    #[serde(default)]
    pub workers: Vec<(String, String)>,
    /// Site Workers that were first installed as protected origins.
    #[serde(default)]
    pub protected_workers: Vec<String>,
    /// Buckets that were first created for a protected site.
    #[serde(default)]
    pub protected_buckets: Vec<String>,
    /// Hostnames pigeon attached to its Worker as custom domains in the linked account.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    /// Platform subdomains pigeon attached to the sites Worker for this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subdomains: Vec<String>,
    /// Secrets Stores pigeon created in the linked account for the `secrets` capability (M4).
    /// Retire leaves them: they hold the person's secrets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_stores: Vec<String>,
}

/// The Directory key of instance `id`'s ledger.
pub fn ledger_key(id: &str) -> String {
    format!("ledger:{id}")
}

/// What is there now, read from the account.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Observed {
    pub account_id: String,
    pub bucket_exists: bool,
    pub worker_exists: bool,
    /// Whether the Worker's workers.dev address is on; `None` when there is no Worker.
    pub workers_dev: Option<bool>,
    /// The account's workers.dev subdomain, if it has one.
    pub account_subdomain: Option<String>,
    /// Each Domain target's hostname: its zone's id (if the zone is in the account) and the
    /// Worker its custom domain serves now, if any.
    #[serde(default)]
    pub domains: Vec<SeenDomain>,
    /// Each Subdomain target as the platform's own account has it.
    #[serde(default)]
    pub subdomains: Vec<SeenSubdomain>,
}

/// A Subdomain target's hostname in the platform's zone, as the platform's account has it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SeenSubdomain {
    /// Such as `noisypigeon.pigeon.dev`.
    pub hostname: String,
    /// The platform zone's id, when it is found.
    pub zone_id: Option<String>,
    /// The Worker that serves platform subdomains by host (pigeon-sites).
    pub worker: String,
    /// The Worker its custom domain serves now, if any.
    pub service: Option<String>,
    /// The upstream kept for it (`subdomain:{hostname}`), if any.
    pub upstream: Option<String>,
}

/// The Directory key of a subdomain's upstream, which the platform serves it from.
pub fn subdomain_key(hostname: &str) -> String {
    format!("subdomain:{}", hostname.to_ascii_lowercase())
}

/// The instance Worker's workers.dev address, which a subdomain is served from.
pub fn upstream_for(worker: &str, account_subdomain: &str) -> String {
    format!("https://{worker}.{account_subdomain}.workers.dev")
}

/// A Domain target's hostname as the account has it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SeenDomain {
    pub hostname: String,
    pub zone_id: Option<String>,
    pub service: Option<String>,
}

/// One step of a plan.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Already as it should be.
    Ok { what: String, name: String },
    /// Create the R2 bucket.
    CreateBucket {
        name: String,
        jurisdiction: Option<String>,
        #[serde(default, skip_serializing_if = "is_public")]
        protected: bool,
    },
    /// Upload the site Worker, bound to the bucket (a new Worker, or a new version of pigeon's).
    /// An instance with redirects and no site has no bucket: its Worker answers its redirects
    /// alone.
    UploadWorker {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bucket: Option<String>,
        jurisdiction: Option<String>,
        module: String,
        replaces: bool,
        /// Used to bind the protected origin key to this specific endpoint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// The redirect map (`redirects::config`) the Worker reads, when the instance has
        /// redirects; absent otherwise, so plans without them read as before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirects: Option<String>,
    },
    /// Turn on the Worker's workers.dev address, the preview.
    EnableWorkersDev { name: String },
    /// Serve a hostname from the Worker as a Workers Custom Domain; Cloudflare makes its DNS
    /// record and certificate.
    AttachDomain {
        hostname: String,
        zone_id: String,
        worker: String,
    },
    /// Serve a platform subdomain, in the platform's own account, from the sites Worker, which
    /// passes it through to the instance Worker's workers.dev address.
    AttachSubdomain {
        hostname: String,
        zone_id: String,
        worker: String,
        upstream: String,
    },
    /// Something with pigeon's name that pigeon did not make: left alone, and the plan stops.
    Conflict {
        what: String,
        name: String,
        why: String,
    },
    /// Upload a capability's own Worker into the linked account (capability scoping, M5):
    /// `<instance>-<capability>`, which the instance Worker then reaches through a Service
    /// Binding named `binding`. Recorded in the ledger as pigeon's, so retire removes it.
    UploadCapabilityWorker {
        capability: String,
        name: String,
        binding: String,
        module: String,
        replaces: bool,
    },
    /// Create the linked account's Secrets Store, for the `secrets` capability its record
    /// enables (capability scoping, M4).
    CreateSecretsStore { name: String },
    /// Something only the person can do first.
    Blocked { why: String },
    /// There, but not the plan's to change: a platform-owned Worker that the home instance
    /// names. Left alone, and the plan goes on.
    Keep {
        what: String,
        name: String,
        why: String,
    },
}

fn is_public(protected: &bool) -> bool {
    !protected
}

impl Step {
    /// Whether applying it changes the account.
    pub fn changes(&self) -> bool {
        matches!(
            self,
            Step::CreateBucket { .. }
                | Step::UploadWorker { .. }
                | Step::CreateSecretsStore { .. }
                | Step::UploadCapabilityWorker { .. }
                | Step::EnableWorkersDev { .. }
                | Step::AttachDomain { .. }
                | Step::AttachSubdomain { .. }
        )
    }

    /// Whether it stops the plan from being applied.
    pub fn stops(&self) -> bool {
        matches!(self, Step::Conflict { .. } | Step::Blocked { .. })
    }
}

/// The digest of what is uploaded as the Worker: its module, compatibility date and binding.
pub fn module_digest(bucket: &str, jurisdiction: Option<&str>) -> String {
    pigeon_core::oauth::hash(
        &json!({
            "module": SITE_WORKER,
            "compatibility_date": COMPATIBILITY_DATE,
            "binding": SITE_BINDING,
            "bucket": bucket,
            "jurisdiction": jurisdiction,
        })
        .to_string(),
    )
}

/// What in the platform's own account belongs to the platform, not to the home instance: the
/// Workers in its footprint and the hostnames it serves. A plan never uploads, attaches or turns
/// on workers.dev for any of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlatformOwned {
    /// Such as `pigeon-platform`, `pigeon-sites`, the gateways, `proxy` and `ai-agents`.
    pub workers: Vec<String>,
    /// The platform zone's apex and its reserved hostnames, such as `platform.pigeon.dev`.
    pub hosts: Vec<String>,
    /// The platform zone's apex, such as `pigeon.dev`: platform-owned, and kept as it is when the
    /// home instance's landing site names it.
    pub apex: String,
}

impl PlatformOwned {
    /// Whether Worker `name` is the platform's.
    pub fn worker(&self, name: &str) -> bool {
        self.workers.iter().any(|w| w == name)
    }

    /// Whether `hostname` is the platform's, ignoring ASCII case.
    pub fn host(&self, hostname: &str) -> bool {
        self.hosts.iter().any(|h| h.eq_ignore_ascii_case(hostname))
    }
}

/// What is platform-owned for `instance`, from the reserved names `rules` carries (themselves
/// derived from every capability's footprint). `None` for a linked instance: nothing in a linked
/// account is the platform's.
pub fn platform_owned(instance: &Instance, rules: &Rules) -> Option<PlatformOwned> {
    if instance.account != AccountRef::Home {
        return None;
    }
    let mut workers = rules.reserved_workers.to_vec();
    workers.push(rules.platform_worker.to_string());
    workers.push(super::SITES_WORKER.to_string());
    workers.retain(|w| !w.is_empty());
    workers.sort();
    workers.dedup();
    Some(PlatformOwned {
        workers,
        hosts: super::record::platform_hosts(rules),
        apex: rules.platform_zone.to_ascii_lowercase(),
    })
}

/// Why a platform-owned name is left alone.
pub const PLATFORM_OWNED: &str = "platform-owned: the platform deploys it, never an instance plan";

/// `step` as the home instance may take it: anything that would upload, attach or turn on
/// workers.dev for a platform-owned Worker or hostname is kept or blocked, never an action.
fn guard(step: Step, owned: &PlatformOwned) -> Step {
    let keep = |what: &str, name: &str| Step::Keep {
        what: what.into(),
        name: name.into(),
        why: PLATFORM_OWNED.into(),
    };
    let blocked = |name: &str| Step::Blocked {
        why: format!("{name} is {PLATFORM_OWNED}"),
    };
    let named_host = |what: &str| what == "custom domain" || what == "subdomain";
    let named_worker = |what: &str| what == "worker" || what == "workers.dev";
    match &step {
        Step::UploadWorker { name, .. } if owned.worker(name) => keep("worker", name),
        Step::EnableWorkersDev { name } if owned.worker(name) => keep("workers.dev", name),
        Step::Ok { what, name } | Step::Conflict { what, name, .. }
            if named_worker(what) && owned.worker(name) =>
        {
            keep(what, name)
        }
        Step::Ok { what, name } | Step::Conflict { what, name, .. }
            if named_host(what) && owned.host(name) =>
        {
            blocked(name)
        }
        Step::AttachDomain {
            hostname, worker, ..
        }
        | Step::AttachSubdomain {
            hostname, worker, ..
        } => {
            if owned.host(hostname) {
                blocked(hostname)
            } else if owned.worker(worker) {
                blocked(&format!("{hostname}'s Worker {worker}"))
            } else {
                step
            }
        }
        // The home instance's credential (`instance_home`) holds no Workers-scripts write: the
        // platform deploys Workers, never an instance plan, even one the home instance names.
        Step::UploadWorker { name, .. } | Step::EnableWorkersDev { name } => Step::Blocked {
            why: format!("Worker {name}: {NO_SCRIPTS}"),
        },
        _ => step,
    }
}

/// Why the home instance's plan never uploads a Worker or turns on its workers.dev address.
pub const NO_SCRIPTS: &str = "the home instance's credential (instance_home) holds no \
                              Workers-scripts write; the platform deploys Workers, never an \
                              instance plan";

/// The custom domain step for `hostname`, a Domain target of the home instance's `site`, when
/// what is platform-owned decides it: the apex of a landing site is kept (the platform's Worker
/// serves it); any other platform host, or any host the platform's Worker would serve, is
/// blocked. `None` when the ordinary domain step applies.
fn home_domain(
    site: &super::record::SiteRecord,
    hostname: &str,
    worker: &str,
    owned: &PlatformOwned,
) -> Option<Step> {
    if site.landing() && hostname.eq_ignore_ascii_case(&owned.apex) {
        return Some(Step::Keep {
            what: "custom domain".into(),
            name: hostname.to_string(),
            why: PLATFORM_OWNED.into(),
        });
    }
    if owned.host(hostname) {
        return Some(Step::Blocked {
            why: format!("{hostname} is {PLATFORM_OWNED}"),
        });
    }
    owned.worker(worker).then(|| Step::Blocked {
        why: format!("{hostname}'s Worker {worker} is {PLATFORM_OWNED}"),
    })
}

/// Why the home instance's `steps` may not be applied, checked again just before applying: a
/// step that stops the plan, or a change naming anything platform-owned, or any change but a
/// bucket or a custom domain (the only things `instance_home` may make). `None` when they may.
/// Fails closed: without the platform-owned set, nothing is applied.
pub fn home_refusal(steps: &[Step], owned: Option<&PlatformOwned>) -> Option<String> {
    let Some(owned) = owned else {
        return Some("what is platform-owned is not known, so nothing is applied".into());
    };
    if let Some(Step::Blocked { why } | Step::Conflict { why, .. }) =
        steps.iter().find(|s| s.stops())
    {
        return Some(format!("something is in the way: {why}"));
    }
    for step in steps.iter().filter(|s| s.changes()) {
        let touches = match step {
            Step::CreateBucket { .. } => None,
            Step::AttachDomain {
                hostname, worker, ..
            } if owned.host(hostname) || owned.worker(worker) => Some(hostname.as_str()),
            Step::AttachDomain { .. } => None,
            Step::UploadWorker { name, .. }
            | Step::EnableWorkersDev { name }
            | Step::UploadCapabilityWorker { name, .. } => Some(name.as_str()),
            Step::AttachSubdomain { hostname, .. } => Some(hostname.as_str()),
            _ => None,
        };
        if let Some(name) = touches {
            return Some(format!(
                "the plan would change {name}, which the home instance may not: it is \
                 platform-owned or needs Workers-scripts write"
            ));
        }
    }
    None
}

/// The plan for `instance` given what is there and its ledger. An instance with no site or no
/// Worker needs nothing. The home instance's plan also needs `owned`, what is the platform's
/// ([`platform_owned`]): without it, its plan is blocked.
pub fn plan(
    instance: &Instance,
    seen: &Observed,
    ledger: &Ledger,
    owned: Option<&PlatformOwned>,
) -> Vec<Step> {
    if instance.account != AccountRef::Home {
        return wanted(instance, seen, ledger, None);
    }
    let Some(owned) = owned else {
        return vec![Step::Blocked {
            why: "the home instance's plan needs what is platform-owned, and it was not given"
                .into(),
        }];
    };
    wanted(instance, seen, ledger, Some(owned))
        .into_iter()
        .map(|s| guard(s, owned))
        .collect()
}

/// The plan's steps before the home instance's guard. `owned` is given for the home instance
/// alone: its custom domains are decided by [`home_domain`] first.
fn wanted(
    instance: &Instance,
    seen: &Observed,
    ledger: &Ledger,
    owned: Option<&PlatformOwned>,
) -> Vec<Step> {
    let Some(worker) = instance.worker.as_deref() else {
        return Vec::new();
    };
    let redirects = super::redirects::config(instance);
    let Some(site) = instance.sites.first() else {
        return match redirects {
            Some(map) => redirect_only(instance, worker, map, seen, ledger, owned),
            None => Vec::new(),
        };
    };
    let protected = site.auth.as_deref() == Some("passkey");
    if instance.sites.iter().any(|site| site.auth.is_some()) && instance.sites.len() != 1 {
        return vec![Step::Blocked {
            why: "a protected origin needs a dedicated instance with one site".into(),
        }];
    }
    let protected_worker = ledger.protected_workers.iter().any(|name| name == worker);
    let protected_bucket = ledger.protected_buckets.contains(&site.bucket);
    if (protected
        && ((seen.worker_exists && !protected_worker) || (seen.bucket_exists && !protected_bucket)))
        || (!protected && (protected_worker || protected_bucket))
    {
        return vec![Step::Blocked {
            why: "switching origin visibility needs a new Worker and a dedicated private bucket"
                .into(),
        }];
    }
    let origin = if protected {
        let Some(subdomain) = seen.account_subdomain.as_deref() else {
            return vec![Step::Blocked {
                why: "the linked account needs a workers.dev subdomain for the protected origin"
                    .into(),
            }];
        };
        Some(upstream_for(worker, subdomain))
    } else {
        None
    };
    let bucket = site.bucket.as_str();
    let jurisdiction = site.jurisdiction.clone();
    let mut steps = Vec::new();

    let bucket_ours = ledger.buckets.iter().any(|b| b == bucket);
    steps.push(match (seen.bucket_exists, bucket_ours) {
        (false, _) => Step::CreateBucket {
            name: bucket.into(),
            jurisdiction: jurisdiction.clone(),
            protected,
        },
        (true, true) => Step::Ok {
            what: "bucket".into(),
            name: bucket.into(),
        },
        (true, false) => Step::Conflict {
            what: "bucket".into(),
            name: bucket.into(),
            why: "a bucket with this name exists, and pigeon did not create it".into(),
        },
    });

    let module = if let Some(upstream) = origin.as_deref() {
        pigeon_core::oauth::hash(
            &json!({
                "public_module": module_digest(bucket, jurisdiction.as_deref()),
                "protected_module": protected_site_worker(),
                "origin_key": ORIGIN_KEY_BINDING,
                "instance": instance.id,
                "site": site.name,
                "upstream": upstream,
            })
            .to_string(),
        )
    } else {
        module_digest(bucket, jurisdiction.as_deref())
    };
    let module = with_redirects(Some(&module), redirects.as_deref());
    let module = with_services(&module, &services(instance));
    let uploaded = ledger
        .workers
        .iter()
        .find(|(n, _)| n == worker)
        .map(|(_, d)| d.as_str());
    steps.push(match (seen.worker_exists, uploaded) {
        (false, _) => Step::UploadWorker {
            name: worker.into(),
            bucket: Some(bucket.into()),
            jurisdiction: jurisdiction.clone(),
            module: module.clone(),
            replaces: false,
            origin: origin.clone(),
            redirects: redirects.clone(),
        },
        (true, Some(d)) if d == module => Step::Ok {
            what: "worker".into(),
            name: worker.into(),
        },
        (true, Some(_)) => Step::UploadWorker {
            name: worker.into(),
            bucket: Some(bucket.into()),
            jurisdiction: jurisdiction.clone(),
            module: module.clone(),
            replaces: true,
            origin,
            redirects: redirects.clone(),
        },
        (true, None) => Step::Conflict {
            what: "worker".into(),
            name: worker.into(),
            why: "a Worker with this name exists, and pigeon did not upload it".into(),
        },
    });

    let previews = site
        .targets
        .iter()
        .any(|t| matches!(t, super::record::Target::Preview));
    if previews || protected {
        steps.push(match (&seen.account_subdomain, seen.workers_dev) {
            (None, _) => Step::Blocked {
                why: "the account has no workers.dev subdomain yet: the person picks one in the \
                      dashboard (Workers & Pages, then the subdomain), once"
                    .into(),
            },
            (Some(_), Some(true)) => Step::Ok {
                what: "workers.dev".into(),
                name: worker.into(),
            },
            (Some(_), _) => Step::EnableWorkersDev {
                name: worker.into(),
            },
        });
    }
    for target in &site.targets {
        let super::record::Target::Domain { hostname } = target else {
            continue;
        };
        if let Some(step) = owned.and_then(|o| home_domain(site, hostname, worker, o)) {
            steps.push(step);
            continue;
        }
        steps.push(domain_step(seen, hostname, worker));
    }
    steps.extend(redirect_domains(instance, worker, seen, owned));
    for seen_sub in &seen.subdomains {
        if owned.is_some_and(|o| o.worker(&seen_sub.worker)) {
            steps.push(Step::Blocked {
                why: format!(
                    "{}'s Worker {} is {PLATFORM_OWNED}",
                    seen_sub.hostname, seen_sub.worker
                ),
            });
            continue;
        }
        let upstream = seen
            .account_subdomain
            .as_deref()
            .map(|a| upstream_for(worker, a));
        steps.push(match (&seen_sub.service, &seen_sub.zone_id, upstream) {
            (_, _, None) => Step::Blocked {
                why: format!(
                    "{} is served from the instance's workers.dev address, and the account has \
                     no workers.dev subdomain yet",
                    seen_sub.hostname
                ),
            },
            (Some(s), _, _) if *s != seen_sub.worker => Step::Conflict {
                what: "subdomain".into(),
                name: seen_sub.hostname.clone(),
                why: format!("it is already served by Worker {s}"),
            },
            (Some(_), _, Some(u)) if seen_sub.upstream.as_deref() == Some(u.as_str()) => Step::Ok {
                what: "subdomain".into(),
                name: seen_sub.hostname.clone(),
            },
            (_, None, _) => Step::Blocked {
                why: format!(
                    "the platform's zone for {} was not found",
                    seen_sub.hostname
                ),
            },
            (_, Some(z), Some(u)) => Step::AttachSubdomain {
                hostname: seen_sub.hostname.clone(),
                zone_id: z.clone(),
                worker: seen_sub.worker.clone(),
                upstream: u,
            },
        });
    }
    steps
}

/// The step for custom domain `hostname` on `worker`, from what the account has: unchanged when
/// the Worker serves it, a conflict when another does, attached when its zone is in the account,
/// else blocked.
fn domain_step(seen: &Observed, hostname: &str, worker: &str) -> Step {
    match seen.domains.iter().find(|d| d.hostname == hostname) {
        Some(SeenDomain {
            service: Some(s), ..
        }) if s == worker => Step::Ok {
            what: "custom domain".into(),
            name: hostname.to_string(),
        },
        Some(SeenDomain {
            service: Some(s), ..
        }) => Step::Conflict {
            what: "custom domain".into(),
            name: hostname.to_string(),
            why: format!("it is already served by Worker {s}"),
        },
        Some(SeenDomain {
            zone_id: Some(z), ..
        }) => Step::AttachDomain {
            hostname: hostname.to_string(),
            zone_id: z.clone(),
            worker: worker.into(),
        },
        _ => Step::Blocked {
            why: format!("the zone of {hostname} is not in the instance's account"),
        },
    }
}

/// Why the home instance's redirects are never served: they need the instance's own Worker to
/// carry the redirect map, and the home instance's plan uploads no Worker.
pub const HOME_REDIRECTS: &str = "redirects are answered by the instance's own Worker, and the \
                                  home instance's plan uploads none: the platform deploys its \
                                  Workers";

/// The custom domain steps of the instance's redirect hosts, each like a Domain target's. The
/// home instance's are blocked ([`HOME_REDIRECTS`]).
fn redirect_domains(
    instance: &Instance,
    worker: &str,
    seen: &Observed,
    owned: Option<&PlatformOwned>,
) -> Vec<Step> {
    if instance.redirects.is_empty() {
        return Vec::new();
    }
    if owned.is_some() {
        return vec![Step::Blocked {
            why: HOME_REDIRECTS.into(),
        }];
    }
    instance
        .redirects
        .iter()
        .map(|r| domain_step(seen, &r.host.to_ascii_lowercase(), worker))
        .collect()
}

/// The digest of what is uploaded as the Worker once its redirects are added: `site` (the site
/// module's digest, `None` with no site) as it is when there are none, so instances without
/// redirects keep their digests and read back unchanged.
pub fn with_redirects(site: Option<&str>, redirects: Option<&str>) -> String {
    match (site, redirects) {
        (Some(site), None) => site.to_string(),
        (site, redirects) => pigeon_core::oauth::hash(
            &json!({
                "site": site,
                "redirect_module": super::redirects::MODULE,
                "compatibility_date": COMPATIBILITY_DATE,
                "binding": super::redirects::BINDING,
                "redirects": redirects,
            })
            .to_string(),
        ),
    }
}

/// The plan of an instance with redirects and no site: its Worker, answering only its
/// redirects (no bucket, no preview), and each redirect host as a custom domain of it.
fn redirect_only(
    instance: &Instance,
    worker: &str,
    map: String,
    seen: &Observed,
    ledger: &Ledger,
    owned: Option<&PlatformOwned>,
) -> Vec<Step> {
    if owned.is_some() {
        return vec![Step::Blocked {
            why: HOME_REDIRECTS.into(),
        }];
    }
    let module = with_redirects(None, Some(&map));
    let module = with_services(&module, &services(instance));
    let uploaded = ledger
        .workers
        .iter()
        .find(|(n, _)| n == worker)
        .map(|(_, d)| d.as_str());
    let upload = |replaces: bool| Step::UploadWorker {
        name: worker.into(),
        bucket: None,
        jurisdiction: None,
        module: module.clone(),
        replaces,
        origin: None,
        redirects: Some(map.clone()),
    };
    let mut steps = vec![match (seen.worker_exists, uploaded) {
        (false, _) => upload(false),
        (true, Some(d)) if d == module => Step::Ok {
            what: "worker".into(),
            name: worker.into(),
        },
        (true, Some(_)) => upload(true),
        (true, None) => Step::Conflict {
            what: "worker".into(),
            name: worker.into(),
            why: "a Worker with this name exists, and pigeon did not upload it".into(),
        },
    }];
    steps.extend(redirect_domains(instance, worker, seen, owned));
    steps
}

/// What the plan says about redirects beside its steps: that removing one detaches nothing.
pub const REDIRECTS_NOTE: &str = "Removing a redirect detaches nothing: pigeon never deletes, so \
                                  its host stays a custom domain of the instance's Worker (which \
                                  then serves the site there, or answers 404 with no site) until \
                                  a person detaches it or the instance is retired.";

/// What a linked account holds for the catalogue capabilities its record enables
/// (docs/plans/capability-scoping.md, M4), read only for those it enables, so a record that
/// enables none is observed, planned and digested as before.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct SeenCapabilities {
    /// Whether the account has a Secrets Store: `None` when the record does not enable
    /// `secrets`, or the link may not read it (see `refused`).
    pub secrets_store: Option<bool>,
    /// For each capability the link cannot serve, why: such as "re-link to enable secrets".
    pub refused: Vec<(String, String)>,
    /// The capability Workers (M5) that exist in the account, of those the record places
    /// `own_worker` ([`own_workers`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workers: Vec<String>,
}

/// The steps the record's catalogue capabilities need in its linked account, after the site's:
/// a capability the link cannot serve is left alone with why (a person re-links first), so the
/// site's steps still apply; `secrets` needs a Secrets Store, made when the account has none. A
/// home record needs none here: the platform runs them.
pub fn capability_steps(instance: &Instance, seen: &SeenCapabilities) -> Vec<Step> {
    if instance.account == AccountRef::Home {
        return Vec::new();
    }
    let mut steps: Vec<Step> = seen
        .refused
        .iter()
        .map(|(name, why)| Step::Keep {
            what: "capability".into(),
            name: name.clone(),
            why: why.clone(),
        })
        .collect();
    match seen.secrets_store {
        Some(true) => steps.push(Step::Ok {
            what: "secrets store".into(),
            name: "the account's".into(),
        }),
        Some(false) => steps.push(Step::CreateSecretsStore {
            name: crate::secrets::LINKED_STORE.into(),
        }),
        None => {}
    }
    steps
}

/// A capability's own-Worker build for a linked account (capability scoping, M5): its one JS
/// module and the Service Binding name the instance Worker reaches it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkedWorker {
    pub capability: &'static str,
    pub binding: &'static str,
    pub module: &'static str,
}

/// The capabilities with an own-Worker build for a linked account: none yet. `models` and
/// `ai-agents` offer `own_worker`, but neither has a build fit for a linked account (M4 steps 5
/// and 6), so `catalogue::linked_placements` offers `inline` alone for every entry until one is
/// listed here.
pub const LINKED_WORKERS: &[LinkedWorker] = &[];

/// The digest of what is uploaded as capability Worker `w`: its module and compatibility date.
pub fn capability_worker_digest(w: &LinkedWorker) -> String {
    pigeon_core::oauth::hash(
        &json!({
            "capability": w.capability,
            "module": w.module,
            "compatibility_date": COMPATIBILITY_DATE,
        })
        .to_string(),
    )
}

/// The record's capabilities placed `own_worker` that have a build in `builds`, as the instance
/// Worker's Service Bindings: `(binding, worker)`, in the record's order.
pub fn services_with(instance: &Instance, builds: &[LinkedWorker]) -> Vec<(String, String)> {
    if instance.account == AccountRef::Home {
        return Vec::new();
    }
    instance
        .capabilities
        .iter()
        .filter(|(_, s)| s.placement == Some(crate::catalogue::Placement::OwnWorker))
        .filter_map(|(name, _)| {
            let w = builds.iter().find(|w| w.capability == name)?;
            Some((
                w.binding.to_string(),
                crate::catalogue::own_worker_name(&instance.id, name),
            ))
        })
        .collect()
}

/// [`services_with`] the builds there are ([`LINKED_WORKERS`]).
pub fn services(instance: &Instance) -> Vec<(String, String)> {
    services_with(instance, LINKED_WORKERS)
}

/// The instance Worker's digest once its Service Bindings are added: `module` as it is when
/// there are none, so every instance without an own-Worker capability keeps its digest.
pub fn with_services(module: &str, services: &[(String, String)]) -> String {
    if services.is_empty() {
        return module.to_string();
    }
    pigeon_core::oauth::hash(&json!({ "module": module, "services": services }).to_string())
}

/// The steps for the record's capabilities placed `own_worker` in its linked account, before
/// the instance Worker's (which binds them): each Worker uploaded when missing or changed,
/// unchanged when pigeon's last upload matches, a conflict when a Worker of that name is not
/// pigeon's, and blocked when there is no build for it (`builds`). `seen` names the own Workers
/// that exist. Nothing for home, or for a record with none.
pub fn capability_workers_with(
    instance: &Instance,
    seen: &SeenCapabilities,
    ledger: &Ledger,
    builds: &[LinkedWorker],
) -> Vec<Step> {
    if instance.account == AccountRef::Home {
        return Vec::new();
    }
    let mut steps = Vec::new();
    for (cap, settings) in &instance.capabilities {
        if settings.placement != Some(crate::catalogue::Placement::OwnWorker) {
            continue;
        }
        let name = crate::catalogue::own_worker_name(&instance.id, cap);
        let Some(build) = builds.iter().find(|w| w.capability == cap) else {
            steps.push(Step::Blocked {
                why: format!("{cap} has no Worker build for a linked account yet"),
            });
            continue;
        };
        let module = capability_worker_digest(build);
        let uploaded = ledger
            .workers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, d)| d.as_str());
        let upload = |replaces: bool| Step::UploadCapabilityWorker {
            capability: cap.clone(),
            name: name.clone(),
            binding: build.binding.to_string(),
            module: module.clone(),
            replaces,
        };
        steps.push(match (seen.workers.contains(&name), uploaded) {
            (false, _) => upload(false),
            (true, Some(d)) if d == module => Step::Ok {
                what: "worker".into(),
                name,
            },
            (true, Some(_)) => upload(true),
            (true, None) => Step::Conflict {
                what: "worker".into(),
                name,
                why: "a Worker with this name exists, and pigeon did not upload it".into(),
            },
        });
    }
    steps
}

/// [`capability_workers_with`] the builds there are ([`LINKED_WORKERS`]).
pub fn capability_workers(
    instance: &Instance,
    seen: &SeenCapabilities,
    ledger: &Ledger,
) -> Vec<Step> {
    capability_workers_with(instance, seen, ledger, LINKED_WORKERS)
}

/// The names of the own Workers `instance`'s record places in its linked account, which
/// [`SeenCapabilities::workers`] says exist or not.
pub fn own_workers(instance: &Instance) -> Vec<String> {
    instance
        .capabilities
        .iter()
        .filter(|(_, s)| s.placement == Some(crate::catalogue::Placement::OwnWorker))
        .map(|(name, _)| crate::catalogue::own_worker_name(&instance.id, name))
        .collect()
}

/// The digest an approval binds: the instance, its version, the account and the steps.
pub fn digest(instance: &Instance, account_id: &str, steps: &[Step]) -> String {
    pigeon_core::oauth::hash(
        &json!({
            "instance": instance.id,
            "version": instance.version,
            "account_id": account_id,
            "steps": steps,
        })
        .to_string(),
    )
}

/// The ledger after `step` was applied.
pub fn record(ledger: &mut Ledger, step: &Step) {
    match step {
        Step::CreateBucket {
            name, protected, ..
        } => {
            if !ledger.buckets.contains(name) {
                ledger.buckets.push(name.clone());
            }
            if *protected && !ledger.protected_buckets.contains(name) {
                ledger.protected_buckets.push(name.clone());
            }
        }
        Step::UploadCapabilityWorker { name, module, .. } => {
            ledger.workers.retain(|(n, _)| n != name);
            ledger.workers.push((name.clone(), module.clone()));
        }
        Step::UploadWorker {
            name,
            module,
            origin,
            ..
        } => {
            if origin.is_some() && !ledger.protected_workers.contains(name) {
                ledger.protected_workers.push(name.clone());
            }
            ledger.workers.retain(|(n, _)| n != name);
            ledger.workers.push((name.clone(), module.clone()));
        }
        Step::AttachDomain { hostname, .. } if !ledger.domains.contains(hostname) => {
            ledger.domains.push(hostname.clone());
        }
        Step::AttachSubdomain { hostname, .. } if !ledger.subdomains.contains(hostname) => {
            ledger.subdomains.push(hostname.clone());
        }
        Step::CreateSecretsStore { name } if !ledger.secrets_stores.contains(name) => {
            ledger.secrets_stores.push(name.clone());
        }
        _ => {}
    }
}

/// The plan as the person reads it: one line per step.
pub fn describe(steps: &[Step]) -> Vec<Value> {
    steps
        .iter()
        .map(|s| match s {
            Step::Ok { what, name } => json!(format!("{what} {name}: already there, unchanged")),
            Step::CreateBucket {
                name,
                jurisdiction,
                protected,
            } => json!(format!(
                "create {}R2 bucket {name}{}",
                if *protected { "private " } else { "" },
                jurisdiction
                    .as_deref()
                    .map(|j| format!(" (jurisdiction {j})"))
                    .unwrap_or_default()
            )),
            Step::UploadWorker {
                name,
                bucket,
                replaces,
                origin,
                redirects,
                ..
            } => {
                let verb = if *replaces { "update" } else { "upload" };
                let hosts = redirects
                    .as_deref()
                    .and_then(|m| serde_json::from_str::<serde_json::Map<String, Value>>(m).ok())
                    .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
                    .filter(|h| !h.is_empty());
                let first = hosts
                    .as_deref()
                    .map(|h| format!("; it answers {h} with its redirect first"))
                    .unwrap_or_default();
                match bucket {
                    Some(bucket) => json!(format!(
                        "{verb} {}Worker {name}, reading bucket {bucket} as {SITE_BINDING}; it \
                         serves the live deploy, or \"nothing deployed yet\" until the first{first}",
                        if origin.is_some() {
                            "authenticated origin "
                        } else {
                            ""
                        }
                    )),
                    None => json!(format!(
                        "{verb} Worker {name}, with no site: it answers only its redirects ({}) \
                         and 404 for anything else",
                        hosts.as_deref().unwrap_or("none")
                    )),
                }
            }
            Step::EnableWorkersDev { name } => {
                json!(format!("turn on {name}'s workers.dev address"))
            }
            Step::UploadCapabilityWorker {
                capability,
                name,
                binding,
                replaces,
                ..
            } => json!(format!(
                "{} {capability}'s own Worker {name}; the instance's Worker reaches it as \
                 {binding}, and retire removes it",
                if *replaces { "update" } else { "upload" }
            )),
            Step::CreateSecretsStore { name } => json!(format!(
                "create the account's Secrets Store, {name}, for the secrets capability; its \
                 secrets are listed on Access › Secrets, and retire leaves it"
            )),
            Step::AttachDomain {
                hostname, worker, ..
            } => json!(format!(
                "serve {hostname} from Worker {worker} as a Workers Custom Domain; Cloudflare \
                 creates its DNS record and certificate"
            )),
            Step::AttachSubdomain {
                hostname,
                worker,
                upstream,
                ..
            } => json!(format!(
                "serve {hostname} from Worker {worker} as a Workers Custom Domain in the \
                 platform's account (Cloudflare creates its DNS record and certificate); it \
                 passes requests through to {upstream}"
            )),
            Step::Conflict { what, name, why } => {
                json!(format!(
                    "stop: {what} {name}: {why}; pigeon leaves it alone"
                ))
            }
            Step::Blocked { why } => json!(format!("stop: {why}")),
            Step::Keep { what, name, why } => {
                json!(format!("{what} {name}: {why}; pigeon leaves it alone"))
            }
        })
        .collect()
}

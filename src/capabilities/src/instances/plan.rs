//! What an instance's resources in its linked account need, worked out from what is there and
//! what pigeon made (its ledger). Pure: the capability observes, this decides, the capability
//! applies. Anything with pigeon's name that pigeon did not make is a conflict and is never
//! touched; nothing here deletes.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::record::Instance;

/// The R2 binding the site Worker reads its bucket through.
pub const SITE_BINDING: &str = "SITE";
/// The site Worker's compatibility date.
pub const COMPATIBILITY_DATE: &str = "2026-11-01";
/// The site Worker: serves `live`'s manifest from the bucket.
pub const SITE_WORKER: &str = include_str!("site_worker.js");

/// What pigeon made in an instance's account, kept in the Directory under [`ledger_key`].
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    /// Buckets pigeon created.
    #[serde(default)]
    pub buckets: Vec<String>,
    /// Workers pigeon uploaded, with the digest of what it last uploaded.
    #[serde(default)]
    pub workers: Vec<(String, String)>,
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
    },
    /// Upload the site Worker, bound to the bucket (a new Worker, or a new version of pigeon's).
    UploadWorker {
        name: String,
        bucket: String,
        jurisdiction: Option<String>,
        module: String,
        replaces: bool,
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
    /// Something with pigeon's name that pigeon did not make: left alone, and the plan stops.
    Conflict {
        what: String,
        name: String,
        why: String,
    },
    /// Something only the person can do first.
    Blocked { why: String },
}

impl Step {
    /// Whether applying it changes the account.
    pub fn changes(&self) -> bool {
        matches!(
            self,
            Step::CreateBucket { .. }
                | Step::UploadWorker { .. }
                | Step::EnableWorkersDev { .. }
                | Step::AttachDomain { .. }
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

/// The plan for `instance` given what is there and its ledger. An instance with no site or no
/// Worker needs nothing.
pub fn plan(instance: &Instance, seen: &Observed, ledger: &Ledger) -> Vec<Step> {
    let (Some(site), Some(worker)) = (instance.sites.first(), instance.worker.as_deref()) else {
        return Vec::new();
    };
    let bucket = site.bucket.as_str();
    let jurisdiction = site.jurisdiction.clone();
    let mut steps = Vec::new();

    let bucket_ours = ledger.buckets.iter().any(|b| b == bucket);
    steps.push(match (seen.bucket_exists, bucket_ours) {
        (false, _) => Step::CreateBucket {
            name: bucket.into(),
            jurisdiction: jurisdiction.clone(),
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

    let module = module_digest(bucket, jurisdiction.as_deref());
    let uploaded = ledger
        .workers
        .iter()
        .find(|(n, _)| n == worker)
        .map(|(_, d)| d.as_str());
    steps.push(match (seen.worker_exists, uploaded) {
        (false, _) => Step::UploadWorker {
            name: worker.into(),
            bucket: bucket.into(),
            jurisdiction: jurisdiction.clone(),
            module: module.clone(),
            replaces: false,
        },
        (true, Some(d)) if d == module => Step::Ok {
            what: "worker".into(),
            name: worker.into(),
        },
        (true, Some(_)) => Step::UploadWorker {
            name: worker.into(),
            bucket: bucket.into(),
            jurisdiction: jurisdiction.clone(),
            module: module.clone(),
            replaces: true,
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
    if previews {
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
        let seen_domain = seen.domains.iter().find(|d| &d.hostname == hostname);
        steps.push(match seen_domain {
            Some(SeenDomain {
                service: Some(s), ..
            }) if s == worker => Step::Ok {
                what: "custom domain".into(),
                name: hostname.clone(),
            },
            Some(SeenDomain {
                service: Some(s), ..
            }) => Step::Conflict {
                what: "custom domain".into(),
                name: hostname.clone(),
                why: format!("it is already served by Worker {s}"),
            },
            Some(SeenDomain {
                zone_id: Some(z), ..
            }) => Step::AttachDomain {
                hostname: hostname.clone(),
                zone_id: z.clone(),
                worker: worker.into(),
            },
            _ => Step::Blocked {
                why: format!("the zone of {hostname} is not in the linked account"),
            },
        });
    }
    steps
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
        Step::CreateBucket { name, .. } if !ledger.buckets.contains(name) => {
            ledger.buckets.push(name.clone());
        }
        Step::UploadWorker { name, module, .. } => {
            ledger.workers.retain(|(n, _)| n != name);
            ledger.workers.push((name.clone(), module.clone()));
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
            Step::CreateBucket { name, jurisdiction } => json!(format!(
                "create R2 bucket {name}{}",
                jurisdiction
                    .as_deref()
                    .map(|j| format!(" (jurisdiction {j})"))
                    .unwrap_or_default()
            )),
            Step::UploadWorker {
                name,
                bucket,
                replaces,
                ..
            } => json!(format!(
                "{} Worker {name}, reading bucket {bucket} as {SITE_BINDING}; it serves the live \
                 deploy, or \"nothing deployed yet\" until the first",
                if *replaces { "update" } else { "upload" }
            )),
            Step::EnableWorkersDev { name } => {
                json!(format!(
                    "turn on {name}'s workers.dev address (the preview)"
                ))
            }
            Step::AttachDomain {
                hostname, worker, ..
            } => json!(format!(
                "serve {hostname} from Worker {worker} as a Workers Custom Domain; Cloudflare \
                 creates its DNS record and certificate"
            )),
            Step::Conflict { what, name, why } => {
                json!(format!(
                    "stop: {what} {name}: {why}; pigeon leaves it alone"
                ))
            }
            Step::Blocked { why } => json!(format!("stop: {why}")),
        })
        .collect()
}

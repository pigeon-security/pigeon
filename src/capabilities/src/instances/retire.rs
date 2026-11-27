//! Retiring an instance: removing what pigeon made for it, and nothing else. Pure: the
//! capability observes ([`Seen`]), this decides ([`plan`]), the capability applies each step in
//! the order [`plan`] gives, recording each in a [`Journal`] so a run that stopped part way
//! resumes where it stopped.
//!
//! Only what pigeon's ledger says pigeon made in the linked account is removed: its Worker
//! (after turning off its workers.dev address), the custom domains it attached to that Worker,
//! the platform subdomains it attached for the instance, and its R2 bucket after emptying it.
//! Anything else with the instance's names is listed as kept. Then pigeon's own state for the
//! instance goes: its site users (with their passkeys and sessions), its publishing queue rows,
//! its ledger and link, and last its record. The home instance is never retired, nor one that
//! another instance depends on.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::plan::{Ledger, SeenDomain, SeenSubdomain, ledger_key, subdomain_key};
use super::record::{AccountRef, Instance, Target};

/// A Worker the retire plan looked at in the linked account.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SeenWorker {
    pub name: String,
    pub exists: bool,
    /// Whether its workers.dev address is on; `None` when it does not exist.
    pub workers_dev: Option<bool>,
}

/// A bucket the retire plan looked at in the linked account.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SeenBucket {
    pub name: String,
    pub jurisdiction: Option<String>,
    pub exists: bool,
    /// How many objects it holds; counted only for a bucket pigeon made.
    pub objects: u64,
}

/// A Directory site user of one of the instance's hosts (kind `site:{host}`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SiteUser {
    pub id: String,
    pub host: String,
}

/// What is there now, for [`plan`].
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// The linked account read, or `None` when there is none to read (never linked, or the
    /// link is gone).
    pub account_id: Option<String>,
    pub workers: Vec<SeenWorker>,
    pub buckets: Vec<SeenBucket>,
    /// Each Domain target as the linked account has it.
    pub domains: Vec<SeenDomain>,
    /// Each Subdomain target as the platform's own account has it.
    pub subdomains: Vec<SeenSubdomain>,
    pub site_users: Vec<SiteUser>,
    /// Which of [`state_keys`] hold a value now.
    pub settings: Vec<String>,
    /// Whether the instance has an account link kept.
    pub linked: bool,
}

/// One step of a retire plan, in the order it runs.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// There, but not pigeon's to remove: left alone.
    Keep {
        what: String,
        name: String,
        why: String,
    },
    /// Something that stops the plan.
    Blocked { why: String },
    /// Stop serving a platform subdomain: detach its custom domain from `worker` (the sites
    /// Worker) in the platform's account, when `worker` is set, then forget its upstream.
    DetachSubdomain {
        hostname: String,
        worker: Option<String>,
    },
    /// Detach a custom domain from pigeon's Worker in the linked account.
    DetachDomain { hostname: String, worker: String },
    /// Turn off the Worker's workers.dev address.
    DisableWorkersDev { name: String },
    /// Delete pigeon's Worker.
    DeleteWorker { name: String },
    /// Delete every object in pigeon's bucket.
    EmptyBucket {
        name: String,
        jurisdiction: Option<String>,
        objects: u64,
    },
    /// Delete pigeon's (now empty) bucket.
    DeleteBucket {
        name: String,
        jurisdiction: Option<String>,
    },
    /// Remove a Directory site user of the instance's host, with its passkeys and sessions.
    RemoveSiteUser { id: String, host: String },
    /// Forget one of pigeon's Directory settings for the instance.
    Forget { key: String },
    /// Revoke the linked account grant and forget the link.
    Unlink,
    /// Forget the link without revoking its grant, because other instances are linked to the
    /// same account and revoking could cut them off.
    ForgetLink { shared_with: Vec<String> },
    /// Remove the instance's record and its place in the index: always last.
    RemoveRecord { id: String },
}

impl Step {
    /// Whether running it removes something.
    pub fn removes(&self) -> bool {
        !matches!(self, Step::Keep { .. } | Step::Blocked { .. })
    }

    /// Whether it stops the plan from being applied.
    pub fn stops(&self) -> bool {
        matches!(self, Step::Blocked { .. })
    }

    /// Whether it acts on the linked account, which must then be the one the plan read.
    pub fn in_linked_account(&self) -> bool {
        matches!(
            self,
            Step::DetachDomain { .. }
                | Step::DisableWorkersDev { .. }
                | Step::DeleteWorker { .. }
                | Step::EmptyBucket { .. }
                | Step::DeleteBucket { .. }
        )
    }

    /// Its place in the order: stop serving, then the Worker, then the bucket, then pigeon's
    /// own state, the record last. Kept and blocked lines sort first.
    pub fn phase(&self) -> u8 {
        match self {
            Step::Keep { .. } | Step::Blocked { .. } => 0,
            Step::DetachSubdomain { .. } => 1,
            Step::DetachDomain { .. } => 2,
            Step::DisableWorkersDev { .. } => 3,
            Step::DeleteWorker { .. } => 4,
            Step::EmptyBucket { .. } => 5,
            Step::DeleteBucket { .. } => 6,
            Step::RemoveSiteUser { .. } => 7,
            Step::Forget { .. } => 8,
            Step::Unlink | Step::ForgetLink { .. } => 9,
            Step::RemoveRecord { .. } => 10,
        }
    }
}

/// pigeon's Directory settings for instance `id`, other than its record and link: its
/// publishing queue rows, ingest state, sealed DigitalOcean token, and last its
/// ledger.
pub fn state_keys(id: &str) -> Vec<String> {
    vec![
        crate::github::deploy_key(id),
        crate::github::push_key(id),
        format!("ingest:runs:{id}"),
        format!("data_url:{id}"),
        format!("data_key:{id}"),
        crate::digitalocean::token_key(id),
        crate::digitalocean::spaces::key(id),
        crate::digitalocean::health::token_key(id),
        ledger_key(id),
    ]
}

/// Every hostname the instance's sites are published at: its platform subdomains under
/// `zone`, its Domain targets and its Zone targets.
pub fn hosts(instance: &Instance, zone: &str) -> Vec<String> {
    let mut out: Vec<String> = instance
        .sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .filter_map(|t| match t {
            Target::Subdomain { label } => Some(super::record::subdomain_host(label, zone)),
            Target::Domain { hostname } | Target::Zone { hostname, .. } => {
                Some(hostname.to_ascii_lowercase())
            }
            Target::Preview => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Why the instance may not be retired at all, if it may not.
pub fn refusal(instance: &Instance) -> Option<String> {
    (instance.account == AccountRef::Home).then(|| {
        format!(
            "{} is the home instance, the platform's own account: it is never retired",
            instance.id
        )
    })
}

/// What other instances use of what retiring this one would remove: its Worker and its
/// buckets, by a site, an ingest destination or source, or a storage binding.
pub fn dependents(instance: &Instance, ledger: &Ledger, others: &[Instance]) -> Vec<String> {
    let workers: Vec<&str> = ledger.workers.iter().map(|(n, _)| n.as_str()).collect();
    let buckets: Vec<&str> = ledger.buckets.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for o in others.iter().filter(|o| o.id != instance.id) {
        if let Some(w) = o.worker.as_deref().filter(|w| workers.contains(w)) {
            out.push(format!("instance {} is served by Worker {w}", o.id));
        }
        for s in &o.sites {
            if buckets.contains(&s.bucket.as_str()) {
                out.push(format!(
                    "instance {}'s site {} reads bucket {}",
                    o.id, s.name, s.bucket
                ));
            }
        }
        if let Some(crate::storage::scope::Profile::Enabled { bucket, .. }) =
            crate::data_store::read(o)
            && buckets.contains(&bucket.as_str())
        {
            out.push(format!(
                "instance {}'s data.read reads bucket {bucket}",
                o.id
            ));
        }
    }
    out
}

/// The other instances linked to the same account as `instance`. Revoking the grant could cut
/// them off, so their presence turns the unlink into forgetting this instance's link only.
pub fn sharing_account(instance: &Instance, others: &[Instance]) -> Vec<String> {
    use super::record::AccountRef;
    let AccountRef::Linked {
        account_id: Some(id),
        ..
    } = &instance.account
    else {
        return Vec::new();
    };
    others
        .iter()
        .filter(|o| o.id != instance.id)
        .filter(|o| matches!(&o.account, AccountRef::Linked { account_id: Some(a), .. } if a == id))
        .map(|o| o.id.clone())
        .collect()
}

/// The retire plan for `instance`, given what is there, its ledger and the other instances.
/// `keep_link` keeps the account link, so the instance can be proposed and applied again
/// without linking it anew.
pub fn plan(
    instance: &Instance,
    seen: &Seen,
    ledger: &Ledger,
    others: &[Instance],
    keep_link: bool,
) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(why) = refusal(instance) {
        return vec![Step::Blocked { why }];
    }
    for why in dependents(instance, ledger, others) {
        steps.push(Step::Blocked {
            why: format!("{why}: retire or change it first"),
        });
    }
    let made_in_account = !ledger.workers.is_empty() || !ledger.buckets.is_empty();
    if seen.account_id.is_none() && made_in_account {
        steps.push(Step::Blocked {
            why: "pigeon's ledger lists a Worker or bucket it made in the linked account, but \
                  the account is not linked now: link it again so they can be removed"
                .into(),
        });
    }
    let ours_worker = |name: &str| ledger.workers.iter().any(|(n, _)| n == name);

    for sub in &seen.subdomains {
        let ours = ledger.subdomains.contains(&sub.hostname) || sub.upstream.is_some();
        match (&sub.service, ours) {
            (Some(s), true) if *s == sub.worker => steps.push(Step::DetachSubdomain {
                hostname: sub.hostname.clone(),
                worker: Some(s.clone()),
            }),
            (Some(s), _) => {
                steps.push(Step::Keep {
                    what: "subdomain".into(),
                    name: sub.hostname.clone(),
                    why: if *s == sub.worker {
                        "pigeon did not attach it for this instance".into()
                    } else {
                        format!("it is served by Worker {s}, not {}", sub.worker)
                    },
                });
                if sub.upstream.is_some() {
                    steps.push(Step::DetachSubdomain {
                        hostname: sub.hostname.clone(),
                        worker: None,
                    });
                }
            }
            (None, _) if sub.upstream.is_some() => steps.push(Step::DetachSubdomain {
                hostname: sub.hostname.clone(),
                worker: None,
            }),
            (None, _) => {}
        }
    }
    for d in &seen.domains {
        match d.service.as_deref() {
            Some(s) if ours_worker(s) => steps.push(Step::DetachDomain {
                hostname: d.hostname.clone(),
                worker: s.to_string(),
            }),
            Some(s) => steps.push(Step::Keep {
                what: "custom domain".into(),
                name: d.hostname.clone(),
                why: format!("it is served by Worker {s}, which pigeon did not upload"),
            }),
            None => {}
        }
    }
    for t in instance.sites.iter().flat_map(|s| s.targets.iter()) {
        if let Target::Zone { route, .. } = t {
            steps.push(Step::Keep {
                what: "route".into(),
                name: route.clone(),
                why: "pigeon never creates routes, so it leaves any route alone".into(),
            });
        }
    }
    for w in seen.workers.iter().filter(|w| w.exists) {
        if !ours_worker(&w.name) {
            steps.push(Step::Keep {
                what: "Worker".into(),
                name: w.name.clone(),
                why: "pigeon did not upload it".into(),
            });
            continue;
        }
        if w.workers_dev == Some(true) {
            steps.push(Step::DisableWorkersDev {
                name: w.name.clone(),
            });
        }
        steps.push(Step::DeleteWorker {
            name: w.name.clone(),
        });
    }
    for b in seen.buckets.iter().filter(|b| b.exists) {
        if !ledger.buckets.contains(&b.name) {
            steps.push(Step::Keep {
                what: "bucket".into(),
                name: b.name.clone(),
                why: "pigeon did not create it".into(),
            });
            continue;
        }
        if b.objects > 0 {
            steps.push(Step::EmptyBucket {
                name: b.name.clone(),
                jurisdiction: b.jurisdiction.clone(),
                objects: b.objects,
            });
        }
        steps.push(Step::DeleteBucket {
            name: b.name.clone(),
            jurisdiction: b.jurisdiction.clone(),
        });
    }
    for u in &seen.site_users {
        steps.push(Step::RemoveSiteUser {
            id: u.id.clone(),
            host: u.host.clone(),
        });
    }
    for key in state_keys(&instance.id) {
        if seen.settings.contains(&key) {
            steps.push(Step::Forget { key });
        }
    }
    if seen.linked {
        if keep_link {
            steps.push(Step::Keep {
                what: "account link".into(),
                name: instance.id.clone(),
                why: "kept, as asked, so the instance can be applied again".into(),
            });
        } else {
            let shared_with = sharing_account(instance, others);
            if shared_with.is_empty() {
                steps.push(Step::Unlink);
            } else {
                steps.push(Step::ForgetLink { shared_with });
            }
        }
    }
    steps.push(Step::RemoveRecord {
        id: instance.id.clone(),
    });
    steps.sort_by_key(Step::phase);
    steps
}

/// The digest an approval binds: marked as a retire, the instance, its version, the account
/// read and the steps.
pub fn digest(instance: &Instance, account_id: Option<&str>, steps: &[Step]) -> String {
    pigeon_core::oauth::hash(
        &json!({
            "retire": true,
            "instance": instance.id,
            "version": instance.version,
            "account_id": account_id,
            "steps": steps,
        })
        .to_string(),
    )
}

/// The plan as the person reads it: one line per step.
pub fn describe(steps: &[Step]) -> Vec<Value> {
    steps
        .iter()
        .map(|s| {
            json!(match s {
                Step::Keep { what, name, why } => format!("keep {what} {name}: {why}"),
                Step::Blocked { why } => format!("stop: {why}"),
                Step::DetachSubdomain {
                    hostname,
                    worker: Some(w),
                } => format!(
                    "stop serving {hostname}: detach it from Worker {w} in the platform's \
                     account and forget its upstream"
                ),
                Step::DetachSubdomain {
                    hostname,
                    worker: None,
                } => format!("forget {hostname}'s upstream"),
                Step::DetachDomain { hostname, worker } => format!(
                    "stop serving {hostname}: detach the custom domain from Worker {worker}; \
                     Cloudflare removes the DNS record and certificate it made"
                ),
                Step::DisableWorkersDev { name } => {
                    format!("turn off {name}'s workers.dev address")
                }
                Step::DeleteWorker { name } => format!("delete Worker {name}"),
                Step::EmptyBucket { name, objects, .. } => {
                    format!("delete the {objects} objects in R2 bucket {name}")
                }
                Step::DeleteBucket { name, .. } => format!("delete R2 bucket {name}"),
                Step::RemoveSiteUser { id, host } =>
                    format!("remove site user {id} of {host}, with their passkeys and sessions"),
                Step::Forget { key } => format!("forget {key}"),
                Step::Unlink => "revoke the account link's grant and forget the link".into(),
                Step::ForgetLink { shared_with } => format!(
                    "forget the account link, keeping its grant: {} use the same account",
                    shared_with.join(", ")
                ),
                Step::RemoveRecord { id } => format!("remove instance {id}'s record"),
            })
        })
        .collect()
}

/// The Directory key of the journal of the retire plan approved as `approval`.
pub fn journal_key(approval: &str) -> String {
    format!("retire:{approval}")
}

/// A retire run: the approved steps and which have been done, kept after each step so a run
/// that stopped resumes at the first step not done.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Journal {
    pub instance: String,
    pub account_id: Option<String>,
    /// The caller the plan was approved for; only it may resume.
    pub caller_id: String,
    pub steps: Vec<Step>,
    pub done: Vec<bool>,
    /// What stopped the last run, if one stopped.
    #[serde(default)]
    pub failed: Option<String>,
}

impl Journal {
    /// A run of `steps`, none done: only the steps that remove something.
    pub fn new(instance: &str, account_id: Option<&str>, caller_id: &str, steps: &[Step]) -> Self {
        let steps: Vec<Step> = steps.iter().filter(|s| s.removes()).cloned().collect();
        Journal {
            instance: instance.into(),
            account_id: account_id.map(str::to_string),
            caller_id: caller_id.into(),
            done: vec![false; steps.len()],
            steps,
            failed: None,
        }
    }

    /// The first step not done, if any.
    pub fn next(&self) -> Option<usize> {
        self.done.iter().position(|d| !d)
    }

    /// Records step `i` as done.
    pub fn finish(&mut self, i: usize) {
        if let Some(d) = self.done.get_mut(i) {
            *d = true;
        }
        self.failed = None;
    }

    /// Records that step `i` failed with `why`; it runs again on resume.
    pub fn fail(&mut self, i: usize, why: &str) {
        self.failed = Some(format!("step {} failed: {why}", i + 1));
    }

    /// Whether every step is done.
    pub fn complete(&self) -> bool {
        self.next().is_none()
    }

    /// Whether a step still to run acts on the linked account.
    pub fn needs_account(&self) -> bool {
        self.steps
            .iter()
            .zip(&self.done)
            .any(|(s, d)| !d && s.in_linked_account())
    }
}

/// The ledger after step `step` was done: what it removed is no longer pigeon's.
pub fn unrecord(ledger: &mut Ledger, step: &Step) {
    match step {
        Step::DeleteWorker { name } => {
            ledger.workers.retain(|(n, _)| n != name);
            ledger.protected_workers.retain(|n| n != name);
        }
        Step::DeleteBucket { name, .. } => {
            ledger.buckets.retain(|b| b != name);
            ledger.protected_buckets.retain(|b| b != name);
        }
        Step::DetachDomain { hostname, .. } => ledger.domains.retain(|d| d != hostname),
        Step::DetachSubdomain { hostname, .. } => ledger.subdomains.retain(|d| d != hostname),
        _ => {}
    }
}

/// The Directory key a platform subdomain's upstream is kept under; [`Step::DetachSubdomain`]
/// forgets it.
pub fn upstream_key(hostname: &str) -> String {
    subdomain_key(hostname)
}

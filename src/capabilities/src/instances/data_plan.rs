//! The data store's and email's plan steps (docs/plans/email.md, "Plan steps"): worked out
//! from what the account has ([`SeenData`]) and pigeon's ledger, as `plan::plan` does for a
//! site. Pure: `data_apply` observes and applies.
//!
//! The DNS steps read the zone first: a live MX that is not Cloudflare's stops the plan; an old
//! provider's stale records are listed, one step each, as "stale, to replace"; nothing is
//! deleted that the plan does not show.

use pigeon_ingest::datastore::{self, EmailConfig};
use serde::Serialize;
use serde_json::json;

use super::plan::{InlineData, Ledger, PlatformOwned, Step};
use super::record::Instance;

/// One DNS record of the zone, as the account answers it.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct DnsRecord {
    pub id: String,
    /// The full name, such as `pigeon.dev` or `fm1._domainkey.pigeon.dev`.
    pub name: String,
    /// `MX`, `TXT`, `CNAME` and so on.
    pub kind: String,
    pub content: String,
}

/// A forward destination as the account has it.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SeenDestination {
    pub email: String,
    pub verified: bool,
}

/// What the account has of the data store and the zone's mail, read before planning.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SeenData {
    pub bucket_exists: bool,
    /// The D1 database's id, when one by the store's name exists.
    pub database: Option<String>,
    /// Its tables.
    pub tables: Vec<String>,
    /// The migrations recorded in its `migrations` table.
    pub applied: Vec<String>,
    pub worker_exists: bool,
    /// The digest of the newest data Worker build pigeon holds, if any.
    pub artifact: Option<String>,
    /// The digest of the email configuration row, if there is one.
    pub config_digest: Option<String>,
    /// The account's queue names.
    pub queues: Vec<String>,
    /// Scripts consuming the backup queue, with unrelated consumers represented as a conflict.
    pub backup_consumers: Vec<String>,
    /// Whether the bucket already sends its events to the backup queue.
    pub notification: bool,
    /// Whether the backup's B2 key is set (`data_store::b2_slot`).
    pub backup_key: bool,
    /// The email domain's zone id, when it is in the account.
    pub zone_id: Option<String>,
    /// The zone's records that matter to mail: MX, TXT and `_domainkey` CNAMEs.
    pub dns: Vec<DnsRecord>,
    pub routing_enabled: bool,
    /// The records Cloudflare says Email Routing adds to the zone when it is turned on (its MX,
    /// SPF and DKIM), read only while it is off; `content` of an MX carries its priority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing_dns: Vec<DnsRecord>,
    /// The Worker the catch-all rule sends to, when it is enabled with a Worker action.
    pub catch_all: Option<String>,
    /// Whether the catch-all is enabled with some other action (forward, drop).
    pub catch_all_other: bool,
    pub destinations: Vec<SeenDestination>,
}

/// What the DNS review found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DnsReview {
    /// MX records that are not Cloudflare's.
    pub foreign_mx: Vec<DnsRecord>,
    /// Old-provider records to replace, each with why.
    pub stale: Vec<(DnsRecord, String)>,
    /// Mail records kept as they are, each with why.
    pub kept: Vec<(DnsRecord, String)>,
}

/// Cloudflare's Email Routing MX hosts end with this.
const CLOUDFLARE_MX: &str = ".mx.cloudflare.net";

/// The old providers, by a mark in their record contents.
const PROVIDERS: &[(&str, &[&str])] = &[
    ("Fastmail", &["messagingengine.com", "fastmail"]),
    ("Proton", &["protonmail.ch", "proton.me", "protonmail"]),
];

fn provider_of(text: &str) -> Option<&'static str> {
    let t = text.to_ascii_lowercase();
    PROVIDERS
        .iter()
        .find(|(_, marks)| marks.iter().any(|m| t.contains(m)))
        .map(|(p, _)| *p)
}

/// Reviews the zone's mail records for `zone`: which MX are foreign, which old-provider
/// records are stale (an apex SPF naming Fastmail or Proton, their DKIM selectors, Proton's
/// verification TXT, a `_dmarc` reporting to either), which are kept.
pub fn dns_review(zone: &str, records: &[DnsRecord]) -> DnsReview {
    let zone = zone.to_ascii_lowercase();
    let mut out = DnsReview::default();
    for r in records {
        let name = r.name.to_ascii_lowercase();
        let kind = r.kind.to_ascii_uppercase();
        let content = r.content.trim().trim_matches('"').to_string();
        let lower = content.to_ascii_lowercase();
        let dkim = name.ends_with(&format!("._domainkey.{zone}"));
        let selector = name.split('.').next().unwrap_or("");
        match kind.as_str() {
            "MX" if lower.trim_end_matches('.').ends_with(CLOUDFLARE_MX) => out
                .kept
                .push((r.clone(), "Cloudflare's Email Routing MX".into())),
            "MX" => out.foreign_mx.push(r.clone()),
            "TXT" if name == zone && lower.starts_with("v=spf1") => match provider_of(&lower) {
                Some(p) => out
                    .stale
                    .push((r.clone(), format!("SPF that authorises {p}"))),
                None => out
                    .kept
                    .push((r.clone(), "an SPF naming no old provider".into())),
            },
            "TXT" if lower.starts_with("protonmail-verification=") => out
                .stale
                .push((r.clone(), "Proton's domain verification".into())),
            "TXT" if name == format!("_dmarc.{zone}") => match provider_of(&lower) {
                Some(p) => out
                    .stale
                    .push((r.clone(), format!("DMARC reporting to {p}"))),
                None => out
                    .kept
                    .push((r.clone(), "a DMARC policy naming no old provider".into())),
            },
            "TXT" | "CNAME" if dkim && matches!(selector, "fm1" | "fm2" | "fm3") => {
                out.stale.push((r.clone(), "Fastmail's DKIM key".into()))
            }
            "TXT" | "CNAME" if dkim && selector.starts_with("protonmail") => {
                out.stale.push((r.clone(), "Proton's DKIM key".into()))
            }
            "TXT" | "CNAME" if dkim && provider_of(&lower).is_some() => out.stale.push((
                r.clone(),
                format!(
                    "{}'s DKIM key",
                    provider_of(&lower).unwrap_or("an old provider")
                ),
            )),
            _ => {}
        }
    }
    out
}

/// What turning Email Routing on does to the zone, one line per record Cloudflare names
/// (`wanted`): added, or already there; beside any record of the same type and name the zone
/// has (`have`), which stays. Empty when Cloudflare named none.
pub fn routing_adds(wanted: &[DnsRecord], have: &[DnsRecord]) -> Vec<String> {
    let same = |a: &str, b: &str| {
        a.trim().trim_matches('"').trim_end_matches('.')
            == b.trim().trim_matches('"').trim_end_matches('.')
    };
    wanted
        .iter()
        .map(|w| {
            let kind = w.kind.to_ascii_uppercase();
            let beside: Vec<&DnsRecord> = have
                .iter()
                .filter(|h| {
                    h.kind.eq_ignore_ascii_case(&kind) && h.name.eq_ignore_ascii_case(&w.name)
                })
                .collect();
            let content = w.content.trim().trim_matches('"');
            if beside.iter().any(|h| same(&h.content, &w.content)) {
                format!("{kind} {} \"{content}\": already there", w.name)
            } else if beside.is_empty() {
                format!("add {kind} {} \"{content}\"", w.name)
            } else {
                let kept: Vec<String> = beside
                    .iter()
                    .map(|h| format!("\"{}\"", h.content.trim().trim_matches('"')))
                    .collect();
                format!(
                    "add {kind} {} \"{content}\", beside {} (kept)",
                    w.name,
                    kept.join(", ")
                )
            }
        })
        .collect()
}

/// The digest of what is uploaded as the data Worker: its build and every binding but the
/// secret's value.
pub fn data_worker_digest(
    artifact: &str,
    name: &str,
    backup: Option<&str>,
    mail_import: bool,
) -> String {
    let mut spec = json!({
        "artifact": artifact,
        "name": name,
        "bindings": ["DATA", "DB", "INSTANCE", "DATA_KEY", "BACKUP", "B2_KEYS"],
        "backup": backup,
        "compatibility_date": super::plan::COMPATIBILITY_DATE,
    });
    if mail_import {
        spec["mail_import"] = json!(true);
    }
    pigeon_core::oauth::hash(&spec.to_string())
}

/// What to do about the email domain's mail records and routing.
fn mail_steps(
    config: &EmailConfig,
    worker: &str,
    seen: &SeenData,
    ledger: &Ledger,
    steps: &mut Vec<Step>,
) {
    let zone = config.domain.clone();
    let Some(zone_id) = seen.zone_id.clone() else {
        steps.push(Step::Blocked {
            why: format!("the zone {zone} is not in the instance's account"),
        });
        return;
    };
    let review = dns_review(&zone, &seen.dns);
    if !review.foreign_mx.is_empty() {
        let mx: Vec<String> = review
            .foreign_mx
            .iter()
            .map(|r| r.content.clone())
            .collect();
        steps.push(Step::Blocked {
            why: format!(
                "{zone} has a live MX that is not Cloudflare's ({}): another provider still \
                 receives its mail; delete it by hand first, if that is what you want",
                mx.join(", ")
            ),
        });
        return;
    }
    for (r, why) in &review.stale {
        steps.push(Step::RemoveStaleDns {
            zone_id: zone_id.clone(),
            id: r.id.clone(),
            name: r.name.clone(),
            kind: r.kind.clone(),
            content: r.content.clone(),
            why: why.clone(),
        });
    }
    for (r, why) in &review.kept {
        steps.push(Step::Ok {
            what: format!("{} record", r.kind),
            name: format!("{} ({why}, kept)", r.name),
        });
    }
    steps.push(if seen.routing_enabled {
        Step::Ok {
            what: "Email Routing".into(),
            name: zone.clone(),
        }
    } else {
        Step::EnableEmailRouting {
            zone: zone.clone(),
            zone_id: zone_id.clone(),
            adds: routing_adds(&seen.routing_dns, &seen.dns),
        }
    });
    steps.push(match (&seen.catch_all, seen.catch_all_other) {
        (Some(w), _) if w == worker => Step::Ok {
            what: "catch-all".into(),
            name: zone.clone(),
        },
        (Some(w), _) if !ledger.email_zones.contains(&zone) => Step::Conflict {
            what: "catch-all".into(),
            name: zone.clone(),
            why: format!("it sends to Worker {w}, which pigeon did not set"),
        },
        (None, true) if !ledger.email_zones.contains(&zone) => Step::Conflict {
            what: "catch-all".into(),
            name: zone.clone(),
            why: "it is enabled with another action, which pigeon did not set".into(),
        },
        _ => Step::SetCatchAll {
            zone: zone.clone(),
            zone_id,
            worker: worker.to_string(),
        },
    });
    for d in config.destinations() {
        match seen.destinations.iter().find(|s| s.email == d) {
            Some(s) if s.verified => steps.push(Step::Ok {
                what: "forward destination".into(),
                name: d,
            }),
            Some(_) => steps.push(Step::Pending {
                what: format!("forward destination {d}"),
                why: "not verified yet: open the link Cloudflare emailed to it; mail meant for \
                      it is stored and the failed forward recorded until then"
                    .into(),
            }),
            None => {
                steps.push(Step::AddDestination { email: d.clone() });
                steps.push(Step::Pending {
                    what: format!("forward destination {d}"),
                    why: "Cloudflare emails it a link once added; forwarding waits until it is \
                          verified"
                        .into(),
                });
            }
        }
    }
}

/// The data store's and email's steps for `instance`. Empty when its record does not enable
/// the store. `owned` is what is platform-owned on home: the store's Worker may never be one.
pub fn data_steps(
    instance: &Instance,
    seen: &SeenData,
    ledger: &Ledger,
    owned: Option<&PlatformOwned>,
) -> Vec<Step> {
    let mut parts = data_parts(instance, seen, ledger, owned);
    parts.store.append(&mut parts.rest);
    parts.store
}

/// The data plan in the order a linked instance's plan places it (docs/plans/linked-data.md,
/// decision 7): `store` (bucket, D1, migrations and, in a Worker of its own, its upload) before
/// the instance Worker's steps, `rest` (backup, email configuration, mail) after them, and the
/// store `inline` in the instance Worker when the record places it there and there is a build.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataParts {
    pub store: Vec<Step>,
    pub rest: Vec<Step>,
    pub inline: Option<InlineData>,
}

/// [`data_steps`] in its [`DataParts`].
pub fn data_parts(
    instance: &Instance,
    seen: &SeenData,
    ledger: &Ledger,
    owned: Option<&PlatformOwned>,
) -> DataParts {
    let mut parts = DataParts::default();
    let Some(settings) = crate::data_store::settings(instance) else {
        return parts;
    };
    let name = crate::data_store::store_name(instance);
    let blocked = |why: String| DataParts {
        store: vec![Step::Blocked { why }],
        ..Default::default()
    };
    if let Some(t) = settings
        .tier
        .as_deref()
        .filter(|t| *t != datastore::TIER_LITE)
    {
        return blocked(format!("the data store's tier {t} is not built yet"));
    }
    if owned.is_some_and(|o| o.worker(&name)) {
        return blocked(format!("{name} is {}", super::plan::PLATFORM_OWNED));
    }
    let inline = crate::data_store::inline(instance);
    let Some(worker) = crate::data_store::worker(instance) else {
        return blocked(
            "the data store runs inline in the instance's own Worker, and the record names none"
                .into(),
        );
    };
    let mut steps = Vec::new();
    let bucket_ours = ledger.buckets.contains(&name);
    steps.push(match (seen.bucket_exists, bucket_ours) {
        (false, _) => Step::CreateBucket {
            name: name.clone(),
            jurisdiction: None,
            protected: false,
        },
        (true, true) => Step::Ok {
            what: "bucket".into(),
            name: name.clone(),
        },
        (true, false) => Step::Conflict {
            what: "bucket".into(),
            name: name.clone(),
            why: "a bucket with this name exists, and pigeon did not create it".into(),
        },
    });
    let db_ours = ledger.databases.contains(&name);
    match (&seen.database, db_ours) {
        (None, _) => steps.push(Step::CreateDatabase { name: name.clone() }),
        (Some(_), true) => steps.push(Step::Ok {
            what: "D1 database".into(),
            name: name.clone(),
        }),
        (Some(_), false) => steps.push(Step::Conflict {
            what: "D1 database".into(),
            name: name.clone(),
            why: "a D1 database with this name exists, and pigeon did not create it".into(),
        }),
    }
    if let Some(why) = datastore::migration_refusal(&seen.tables) {
        steps.push(Step::Blocked { why });
    }
    let pending: Vec<String> = datastore::pending_migrations(&seen.applied)
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    steps.push(if pending.is_empty() {
        Step::Ok {
            what: "migrations".into(),
            name: name.clone(),
        }
    } else {
        Step::Migrate {
            database: name.clone(),
            migrations: pending,
        }
    });
    let backup = settings.backup.as_ref().map(|b| b.bucket.clone());
    let backup_config = backup_generation(instance);
    if backup.is_some() {
        for q in [datastore::backup_dlq(&name), datastore::backup_queue(&name)] {
            steps.push(
                match (seen.queues.contains(&q), ledger.queues.contains(&q)) {
                    (false, _) => Step::CreateQueue {
                        name: q,
                        consumer: None,
                        dead_letter: None,
                    },
                    (true, true) => Step::Ok {
                        what: "queue".into(),
                        name: q,
                    },
                    (true, false) => Step::Conflict {
                        what: "queue".into(),
                        name: q,
                        why: "a queue with this name exists, and pigeon did not create it".into(),
                    },
                },
            );
        }
    }
    let mail_import = crate::data_store::mail_import_enabled(instance);
    match &seen.artifact {
        None => steps.push(Step::Blocked {
            why: format!(
                "there is no build of the data Worker yet: build src/data-worker and put it in \
                 {ARTIFACTS} (docs/plans/email.md, follow-up 2)"
            ),
        }),
        Some(artifact) if inline => {
            parts.inline = Some(InlineData {
                artifact: artifact.clone(),
                bucket: name.clone(),
                database: name.clone(),
                backup: backup.clone(),
                backup_config: backup_config.clone(),
                mail_import,
            });
        }
        Some(artifact) => {
            let module = data_worker_digest(artifact, &name, backup_config.as_deref(), mail_import);
            let uploaded = ledger
                .workers
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d.as_str());
            let upload = |replaces| Step::UploadDataWorker {
                name: name.clone(),
                artifact: artifact.clone(),
                bucket: name.clone(),
                database: name.clone(),
                backup: backup.clone(),
                mail_import,
                module: module.clone(),
                replaces,
            };
            steps.push(match (seen.worker_exists, uploaded) {
                (false, _) => upload(false),
                (true, Some(d)) if d == module => Step::Ok {
                    what: "data Worker".into(),
                    name: name.clone(),
                },
                (true, Some(_)) => upload(true),
                (true, None) => Step::Conflict {
                    what: "worker".into(),
                    name: name.clone(),
                    why: "a Worker with this name exists, and pigeon did not upload it".into(),
                },
            });
        }
    }
    parts.store = steps;
    let mut steps = Vec::new();
    // The B2 backup (ingest.md decision 10).
    match &settings.backup {
        None => steps.push(Step::Pending {
            what: "B2 backup".into(),
            why: "the record names no backup bucket (data.backup); nothing is \
                  copied offsite until it does"
                .into(),
        }),
        Some(b) => {
            let queue = datastore::backup_queue(&name);
            let dlq = datastore::backup_dlq(&name);
            steps.push(match seen.backup_consumers.as_slice() {
                [] => Step::ConfigureBackupConsumer {
                    queue: queue.clone(),
                    worker: worker.clone(),
                    dead_letter: dlq,
                },
                [consumer] if consumer == &worker => Step::Ok {
                    what: "backup consumer".into(),
                    name: worker.clone(),
                },
                _ => Step::Conflict {
                    what: "backup queue".into(),
                    name: queue.clone(),
                    why: "a different consumer already reads the backup queue".into(),
                },
            });
            steps.push(if seen.notification {
                Step::Ok {
                    what: "bucket notification".into(),
                    name: format!("{name} → {queue}"),
                }
            } else {
                Step::AddBucketNotification {
                    bucket: name.clone(),
                    queue: queue.clone(),
                }
            });
            if let Some(generation) = backup_config
                && seen.backup_key
                && ledger.backup_inventory.as_deref() != Some(&generation)
            {
                steps.push(Step::QueueBackupInventory { queue, generation });
            }
            if !seen.backup_key {
                let slot = crate::data_store::b2_slot(instance);
                let expected = format!(
                    "`KEY_ID:APPLICATION_KEY`, scoped to backup bucket {}",
                    b.bucket
                );
                steps.push(if instance.account == super::record::AccountRef::Home {
                    Step::Pending {
                        what: format!("B2 key for {}", b.bucket),
                        why: format!("set {slot} on Access ({expected})"),
                    }
                } else {
                    Step::Blocked {
                        why: format!("set {slot} in the linked account's Cloudflare Secrets Store ({expected})"),
                    }
                });
            }
        }
    }
    if let Some(config) = crate::email::config(instance) {
        if seen.config_digest.as_deref() != Some(config.digest().as_str()) {
            steps.push(Step::WriteEmailConfig {
                database: name.clone(),
                digest: config.digest(),
            });
        } else {
            steps.push(Step::Ok {
                what: "email configuration".into(),
                name: name.clone(),
            });
        }
        mail_steps(&config, &worker, seen, ledger, &mut steps);
    }
    parts.rest = steps;
    parts
}

/// Where the data Worker's builds are kept, in the platform's own account: one folder per
/// digest, and `data-worker/current.json` naming the newest.
pub const ARTIFACTS: &str = "pigeon-data-artifacts/data-worker/";

/// Why the data steps (`data_steps`'s, apart from the site's) may not be applied: a step that stops the plan, or
/// an upload of anything but the store's own Worker. `None` when they may.
pub fn data_refusal(
    instance: &Instance,
    steps: &[Step],
    owned: Option<&PlatformOwned>,
) -> Option<String> {
    if let Some(Step::Blocked { why } | Step::Conflict { why, .. }) =
        steps.iter().find(|s| s.stops())
    {
        return Some(format!("something is in the way: {why}"));
    }
    let name = crate::data_store::store_name(instance);
    for s in steps {
        if let Step::UploadDataWorker { name: n, .. } = s
            && (*n != name || owned.is_some_and(|o| o.worker(n)))
        {
            return Some(format!(
                "the plan would upload {n}, which is not the data store's own Worker"
            ));
        }
    }
    None
}

/// One module of the data Worker's build, as `current.json` lists it.
#[derive(serde::Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Module {
    /// Its name in the upload, such as `shim.mjs` or `index.wasm`.
    pub name: String,
    /// `application/javascript+module` or `application/wasm`.
    pub content_type: String,
}

/// A build of the data Worker: `data-worker/{digest}/{module}` in [`ARTIFACTS`]' bucket, and
/// `data-worker/current.json` naming the newest.
#[derive(serde::Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// The SHA-256 of the modules' SHA-256s, in order (`artifact_digest`).
    pub digest: String,
    /// The module the runtime starts, one of `modules`.
    pub main: String,
    pub modules: Vec<Module>,
}

/// The bucket of [`ARTIFACTS`].
pub const ARTIFACT_BUCKET: &str = "pigeon-data-artifacts";

/// The key of a build's module.
pub fn module_key(digest: &str, name: &str) -> String {
    format!("data-worker/{digest}/{name}")
}

/// A build's digest from its modules' bytes, in order: checked against `current.json` before
/// anything is uploaded, so the person approves exactly what runs.
pub fn artifact_digest(modules: &[(&str, &[u8])]) -> String {
    let lines: Vec<String> = modules
        .iter()
        .map(|(name, bytes)| format!("{name} {}", datastore::sha256_hex(bytes)))
        .collect();
    datastore::sha256_hex(lines.join("\n").as_bytes())
}

/// What the data Worker's upload binds, by the names `src/data-worker` reads.
pub struct WorkerBindings<'a> {
    pub instance: &'a str,
    pub bucket: &'a str,
    pub database_id: &'a str,
    pub data_key: &'a str,
    pub mail_import: bool,
    /// The backup bucket, its endpoint and region, with the Secrets Store holding its key.
    pub backup: Option<(&'a crate::data_store::Backup, &'a str, &'a str)>,
}

/// The upload's metadata: the main module, the compatibility date and the bindings.
pub fn worker_metadata(main: &str, b: &WorkerBindings<'_>) -> serde_json::Value {
    let mut bindings = vec![
        json!({ "type": "r2_bucket", "name": "DATA", "bucket_name": b.bucket }),
        json!({ "type": "d1", "name": "DB", "id": b.database_id }),
        json!({ "type": "plain_text", "name": "INSTANCE", "text": b.instance }),
        json!({ "type": "secret_text", "name": "DATA_KEY", "text": b.data_key }),
    ];
    if b.mail_import {
        bindings.push(json!({
            "type": "plain_text", "name": "MAIL_IMPORT_INSTANCE", "text": b.instance,
        }));
        // Cloudflare drops an unresolved inherit binding on a first upload. If an operator set
        // this secret on the Worker, later plans keep just this named binding.
        bindings.push(json!({ "type": "inherit", "name": "MAIL_IMPORT_KEY" }));
    }
    if let Some((backup, store_id, secret)) = b.backup {
        bindings.push(json!({
            "type": "plain_text", "name": "BACKUP",
            "text": json!({ "endpoint": backup.endpoint, "region": backup.region, "bucket": backup.bucket }).to_string(),
        }));
        bindings.push(json!({
            "type": "secrets_store_secret", "name": "B2_KEYS",
            "store_id": store_id, "secret_name": secret,
        }));
    }
    json!({
        "main_module": main,
        "compatibility_date": super::plan::COMPATIBILITY_DATE,
        "bindings": bindings,
    })
}

pub fn backup_generation(instance: &Instance) -> Option<String> {
    Some(pigeon_core::oauth::hash(
        &json!({
            "layout": "inventory-v1",
            "destination": crate::data_store::backup(instance)?,
            "audit": crate::data_store::backup_audit_bucket(instance)
        })
        .to_string(),
    ))
}

/// Bind an approved backup's producer queue and its inventory generation.
pub fn bind_backup_inventory(metadata: &mut serde_json::Value, instance: &Instance) {
    if let Some(generation) = backup_generation(instance)
        && let Some(bindings) = metadata["bindings"].as_array_mut()
    {
        let queue = datastore::backup_queue(&crate::data_store::store_name(instance));
        bindings.push(json!({ "type": "queue", "name": "BACKUP_QUEUE", "queue_name": queue }));
        bindings
            .push(json!({ "type": "plain_text", "name": "BACKUP_GENERATION", "text": generation }));
    }
}

/// Bind the Jobs bucket the Data planner verified is private and approved for this instance.
pub fn bind_jobs_audit(metadata: &mut serde_json::Value, bucket: &str) {
    if let Some(bindings) = metadata["bindings"].as_array_mut()
        && !bindings
            .iter()
            .any(|binding| binding["name"] == "JOBS_AUDIT")
    {
        bindings.push(json!({ "type": "r2_bucket", "name": "JOBS_AUDIT", "bucket_name": bucket }));
    }
}

pub fn bind_backup_audit(metadata: &mut serde_json::Value, instance: &Instance) {
    if let Some(bucket) = crate::data_store::backup_audit_bucket(instance) {
        bind_jobs_audit(metadata, bucket);
        if let Some(bindings) = metadata["bindings"].as_array_mut() {
            bindings.push(
                json!({ "type": "plain_text", "name": "BACKUP_AUDIT_REQUIRED", "text": "1" }),
            );
        }
    }
}

/// The multipart body of an upload with several modules (a JS shim and its WASM).
pub fn multipart(
    boundary: &str,
    metadata: &serde_json::Value,
    parts: &[(String, String, Vec<u8>)],
) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut text = |s: &str| out.extend_from_slice(s.as_bytes());
    text(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: \
         application/json\r\n\r\n{metadata}\r\n"
    ));
    for (name, content_type, bytes) in parts {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; \
                 filename=\"{name}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

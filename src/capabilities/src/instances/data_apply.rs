//! What the data store's and email's plan steps (`data_plan`) read and do in the account,
//! through Cloudflare's REST API with one short-lived token. Nothing here decides: the steps
//! come from `data_plan::data_steps`, approved by the person's passkey, and only those run.
//!
//! The home instance's token is of purpose [`PURPOSE`] (`data_home`), minted for one plan or
//! one apply and revoked after. Unlike `instance_home` it holds Workers-scripts write, used for
//! the data store's own Worker alone (`data_plan::data_refusal`). None of these calls has run
//! against Cloudflare yet (docs/plans/email.md, Log).
//!
//! A linked instance's data steps use its link's token instead (docs/plans/linked-data.md),
//! never a minted one: the same observe and apply, with [`upload_inline`] for a store that runs
//! inline in the instance's own Worker.

use std::future::Future;

use pigeon_core::Account;
// Every call here uses a `data_home` token minted moments before, which some services (D1 among
// them) refuse with 401 until it reaches them: `call_fresh` retries those.
use pigeon_core::cloudflare::{call_fresh as call, call_raw};
use pigeon_core::tokens::{Policy, Scope};
use pigeon_ingest::datastore;
use pigeon_ingest::index::Statement;
use serde_json::{Value, json};
use worker::{Env, Error, Method, Result};

use super::data_plan::{self, Artifact, DnsRecord, SeenData, SeenDestination};
use super::home::Minter;
use super::plan::{self, Ledger, Step};
use super::record::Instance;
use crate::users::directory;

/// The credential purpose of the home instance's data steps, in
/// [`crate::split::purpose_permissions`], minted by core alone.
pub const PURPOSE: &str = "data_home";
/// The name the token is minted under.
const TOKEN_PURPOSE: &str = "data-home";
/// How long it lives, in seconds, if revoking it fails.
const TTL_SECS: u64 = 900;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Whether permission `name` is the zone's (DNS, Email Routing rules, zone settings) rather
/// than the account's.
fn zone_wide(name: &str) -> bool {
    name.starts_with("DNS") || name.starts_with("Email Routing Rules") || name.starts_with("Zone")
}

/// The token's policies from `permissions` (what [`PURPOSE`] brings): the zone's on zone
/// `zone_id` alone, the rest on the account. Refuses a purpose that is missing or empty.
pub fn policies(
    permissions: Option<&[&str]>,
    zone_id: &str,
) -> std::result::Result<Vec<Policy>, String> {
    let Some(permissions) = permissions.filter(|p| !p.is_empty()) else {
        return Err(format!(
            "`{PURPOSE}` is not a credential purpose of this platform yet; nothing was changed"
        ));
    };
    if zone_id.is_empty() {
        return Err("the email domain's zone was not found in the account".into());
    }
    let account: Vec<&str> = permissions
        .iter()
        .copied()
        .filter(|p| !zone_wide(p))
        .collect();
    let zone: Vec<&str> = permissions
        .iter()
        .copied()
        .filter(|p| zone_wide(p))
        .collect();
    let mut out = vec![Policy::new(Scope::Account, &account)];
    if !zone.is_empty() {
        out.push(Policy::new(Scope::Zones(vec![zone_id.to_string()]), &zone));
    }
    Ok(out)
}

/// Runs `work` with a [`PURPOSE`] token minted by `minter`, then revokes it,
/// whether `work` succeeded or not.
pub async fn with_token<M, F, Fut, T>(minter: &M, zone_id: &str, work: F) -> Result<T>
where
    M: Minter,
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let policies = policies(crate::split::purpose_permissions(PURPOSE), zone_id).map_err(bad)?;
    let (id, value) = minter
        .mint(TOKEN_PURPOSE, policies, TTL_SECS)
        .await
        .map_err(|e| bad(format!("the parent token could not mint `{PURPOSE}` ({e})")))?;
    let result = work(value).await;
    let revoked = minter.revoke(&id).await;
    let value = result?;
    revoked.map_err(|e| {
        bad(format!(
            "the work was done, but the `{PURPOSE}` token was not revoked ({e}); it expires on \
             its own within {TTL_SECS} s"
        ))
    })?;
    Ok(value)
}

/// Whether an API error says the thing is not there.
fn missing(e: &Error) -> bool {
    let t = e.to_string();
    t.contains(": 404:") || pigeon_core::workers::is_not_found(&t)
}

/// `Ok(Some(v))`, `Ok(None)` when not there, or the error.
async fn get(token: &str, path: &str) -> Result<Option<Value>> {
    match call(token, Method::Get, path, None).await {
        Ok(v) => Ok(Some(v)),
        Err(e) if missing(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The id of the zone named `domain` in `account_id`.
pub async fn zone_id(token: &str, account_id: &str, domain: &str) -> Result<Option<String>> {
    let zones = call(
        token,
        Method::Get,
        &format!("/zones?name={domain}&account.id={account_id}"),
        None,
    )
    .await?;
    Ok(zones
        .as_array()
        .and_then(|z| z.first())
        .and_then(|z| z["id"].as_str())
        .map(str::to_string))
}

/// Runs one D1 query and answers its rows.
async fn d1(token: &str, account_id: &str, db: &str, s: &Statement) -> Result<Vec<Value>> {
    let out = call(
        token,
        Method::Post,
        &format!("/accounts/{account_id}/d1/database/{db}/query"),
        Some(&json!({ "sql": s.sql, "params": s.params })),
    )
    .await?;
    Ok(out
        .as_array()
        .and_then(|r| r.first())
        .and_then(|r| r["results"].as_array())
        .cloned()
        .unwrap_or_default())
}

/// The newest data Worker build pigeon holds, read with the platform's own account.
pub async fn artifact(parent: &Account) -> Result<Option<Artifact>> {
    let bucket = pigeon_core::r2::Bucket {
        name: data_plan::ARTIFACT_BUCKET.into(),
        jurisdiction: None,
    };
    let Ok(found) = pigeon_core::r2::get(parent, &bucket, "data-worker/current.json").await else {
        return Ok(None);
    };
    Ok(found.and_then(|b| serde_json::from_slice(&b).ok()))
}

/// What the account has of `instance`'s store and mail.
pub async fn observe(
    token: &str,
    account_id: &str,
    secrets_store: &str,
    instance: &Instance,
    built: Option<&Artifact>,
) -> Result<SeenData> {
    let name = crate::data_store::store_name(instance);
    let a = account_id;
    let mut seen = SeenData {
        artifact: built.map(|b| b.digest.clone()),
        ..Default::default()
    };
    seen.bucket_exists = get(token, &format!("/accounts/{a}/r2/buckets/{name}"))
        .await?
        .is_some();
    let dbs = call(
        token,
        Method::Get,
        &format!("/accounts/{a}/d1/database?name={name}"),
        None,
    )
    .await?;
    seen.database = dbs
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["name"].as_str() == Some(name.as_str()))
        .and_then(|d| d["uuid"].as_str())
        .map(str::to_string);
    if let Some(db) = &seen.database {
        let tables = Statement {
            sql: "SELECT name FROM sqlite_master WHERE type = 'table'".into(),
            params: Vec::new(),
        };
        seen.tables = d1(token, a, db, &tables)
            .await?
            .iter()
            .filter_map(|r| r["name"].as_str().map(str::to_string))
            .collect();
        if seen.tables.iter().any(|t| t == "migrations") {
            let applied = Statement {
                sql: "SELECT id FROM migrations".into(),
                params: Vec::new(),
            };
            seen.applied = d1(token, a, db, &applied)
                .await?
                .iter()
                .filter_map(|r| r["id"].as_str().map(str::to_string))
                .collect();
        }
        if seen.tables.iter().any(|t| t == "config") {
            seen.config_digest = d1(token, a, db, &datastore::config_query())
                .await?
                .first()
                .and_then(|r| r["digest"].as_str())
                .map(str::to_string);
        }
    }
    seen.worker_exists = get(
        token,
        &format!("/accounts/{a}/workers/scripts/{name}/settings"),
    )
    .await?
    .is_some();
    // Only consult optional Queues and Secrets Store scopes on a linked account with backup.
    let backup_read = instance.account == crate::instances::record::AccountRef::Home
        || crate::data_store::settings(instance).is_some_and(|s| s.backup.is_some());
    if backup_read {
        observe_backup(token, a, secrets_store, instance, &name, &mut seen).await?;
    }
    let Some(domain) = crate::email::domain(instance).filter(|_| crate::email::enabled(instance))
    else {
        return Ok(seen);
    };
    observe_mail(token, a, &domain, &mut seen).await?;
    Ok(seen)
}

fn queue_id<'a>(queues: &'a Value, name: &str) -> Option<&'a str> {
    queues
        .as_array()?
        .iter()
        .find(|q| q["queue_name"].as_str() == Some(name))
        .and_then(|q| q["queue_id"].as_str())
}

async fn backup_queue_id(token: &str, account: &str, name: &str) -> Result<String> {
    let queues = call(
        token,
        Method::Get,
        &format!("/accounts/{account}/queues"),
        None,
    )
    .await?;
    queue_id(&queues, name)
        .map(str::to_string)
        .ok_or_else(|| bad(format!("queue {name} is not there")))
}

/// The backup's queues, the bucket's notification and the B2 key's presence.
async fn observe_backup(
    token: &str,
    a: &str,
    secrets_store: &str,
    instance: &Instance,
    name: &str,
    seen: &mut SeenData,
) -> Result<()> {
    let queues = call(token, Method::Get, &format!("/accounts/{a}/queues"), None).await?;
    seen.queues = queues
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| q["queue_name"].as_str().map(str::to_string))
        .collect();
    let backup = datastore::backup_queue(name);
    if let Some(id) = queue_id(&queues, &backup) {
        let consumers = call(
            token,
            Method::Get,
            &format!("/accounts/{a}/queues/{id}/consumers"),
            None,
        )
        .await?;
        seen.backup_consumers = consumers
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| c["script_name"].as_str().unwrap_or_default().to_string())
            .collect();
    }
    if seen.bucket_exists {
        let config = get(
            token,
            &format!("/accounts/{a}/event_notifications/r2/{name}/configuration"),
        )
        .await?
        .unwrap_or(Value::Null);
        seen.notification = config["queues"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|q| q["queueName"].as_str() == Some(backup.as_str()));
    }
    let slot = crate::data_store::b2_slot(instance);
    let secrets = get(
        token,
        &format!("/accounts/{a}/secrets_store/stores/{secrets_store}/secrets?search={slot}"),
    )
    .await?
    .unwrap_or(Value::Null);
    seen.backup_key = secrets
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["name"].as_str() == Some(slot.as_str()));
    Ok(())
}

/// The email domain's zone, its mail records, Email Routing, the catch-all and the account's
/// forward destinations.
async fn observe_mail(token: &str, a: &str, domain: &str, seen: &mut SeenData) -> Result<()> {
    seen.zone_id = zone_id(token, a, domain).await?;
    let Some(z) = seen.zone_id.clone() else {
        return Ok(());
    };
    let records = call(
        token,
        Method::Get,
        &format!("/zones/{z}/dns_records?per_page=500"),
        None,
    )
    .await?;
    seen.dns = records
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| matches!(r["type"].as_str(), Some("MX" | "TXT" | "CNAME")))
        .map(|r| DnsRecord {
            id: r["id"].as_str().unwrap_or_default().into(),
            name: r["name"].as_str().unwrap_or_default().into(),
            kind: r["type"].as_str().unwrap_or_default().into(),
            content: r["content"].as_str().unwrap_or_default().into(),
        })
        .collect();
    let routing = get(token, &format!("/zones/{z}/email/routing"))
        .await?
        .unwrap_or(Value::Null);
    seen.routing_enabled = routing["enabled"].as_bool().unwrap_or(false);
    if !seen.routing_enabled {
        // What turning it on writes, for the plan to show; unread, the plan says it generally.
        let wanted = get(token, &format!("/zones/{z}/email/routing/dns"))
            .await
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        seen.routing_dns = routing_records(&wanted);
    }
    let catch_all = get(token, &format!("/zones/{z}/email/routing/rules/catch_all"))
        .await?
        .unwrap_or(Value::Null);
    if catch_all["enabled"].as_bool() == Some(true) {
        let action = &catch_all["actions"][0];
        if action["type"].as_str() == Some("worker") {
            seen.catch_all = action["value"][0].as_str().map(str::to_string);
        } else {
            seen.catch_all_other = true;
        }
    }
    let addresses = call(
        token,
        Method::Get,
        &format!("/accounts/{a}/email/routing/addresses?per_page=50"),
        None,
    )
    .await?;
    seen.destinations = addresses
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(SeenDestination {
                email: d["email"].as_str()?.to_ascii_lowercase(),
                verified: !d["verified"].is_null(),
            })
        })
        .collect();
    Ok(())
}

/// The records of Cloudflare's Email Routing DNS answer: a list, or one under `record` or
/// `records`; an MX's priority is kept in its content.
fn routing_records(v: &Value) -> Vec<DnsRecord> {
    let list = v
        .as_array()
        .or_else(|| v["record"].as_array())
        .or_else(|| v["records"].as_array());
    list.into_iter()
        .flatten()
        .filter_map(|r| {
            let kind = r["type"].as_str()?.to_string();
            let mut content = r["content"].as_str()?.to_string();
            if let Some(p) = r["priority"].as_u64() {
                content = format!("{content} (priority {p})");
            }
            Some(DnsRecord {
                id: String::new(),
                name: r["name"].as_str().unwrap_or_default().to_string(),
                kind,
                content,
            })
        })
        .collect()
}

/// What an apply needs beyond the steps.
pub struct Apply<'a> {
    pub env: &'a Env,
    /// The platform's own account, which holds the builds.
    pub parent: &'a Account,
    pub token: &'a str,
    pub account_id: &'a str,
    pub secrets_store: &'a str,
    pub built: Option<&'a Artifact>,
}

/// Applies each changing step of `steps` in order, recording each in `ledger` as it is done.
/// Answers a line per step done.
pub async fn run(
    a: &Apply<'_>,
    instance: &Instance,
    steps: &[Step],
    ledger: &mut Ledger,
) -> Result<Vec<String>> {
    let (t, acc) = (a.token, a.account_id);
    let name = crate::data_store::store_name(instance);
    let mut database: Option<String> = None;
    let mut done = Vec::new();
    for step in steps.iter().filter(|s| s.changes()) {
        let line = match step {
            Step::CreateBucket { name, .. } => {
                call(
                    t,
                    Method::Post,
                    &format!("/accounts/{acc}/r2/buckets"),
                    Some(&json!({ "name": name })),
                )
                .await?;
                format!("created bucket {name}")
            }
            Step::CreateDatabase { name } => {
                let made = call(
                    t,
                    Method::Post,
                    &format!("/accounts/{acc}/d1/database"),
                    Some(&json!({ "name": name })),
                )
                .await?;
                database = made["uuid"].as_str().map(str::to_string);
                format!("created D1 database {name}")
            }
            Step::Migrate { migrations, .. } => {
                let db = database_id(t, acc, &name, &mut database).await?;
                let create = Statement {
                    sql: datastore::MIGRATIONS_TABLE.into(),
                    params: Vec::new(),
                };
                d1(t, acc, &db, &create).await?;
                let at = now();
                for m in datastore::MIGRATIONS
                    .iter()
                    .filter(|m| migrations.iter().any(|id| id == m.id))
                {
                    for s in datastore::migration_statements(m, &at) {
                        d1(t, acc, &db, &s).await?;
                    }
                }
                format!("applied migrations {}", migrations.join(", "))
            }
            Step::UploadDataWorker {
                name,
                artifact,
                backup,
                ..
            } => {
                let built = a
                    .built
                    .filter(|b| &b.digest == artifact)
                    .ok_or_else(|| bad("the approved build is no longer the newest: plan again"))?;
                let db = database_id(t, acc, name, &mut database).await?;
                upload(a, instance, name, built, &db, backup.is_some()).await?;
                format!("uploaded data Worker {name} (build {artifact})")
            }
            Step::WriteEmailConfig { digest, .. } => {
                let config = crate::email::config(instance)
                    .filter(|c| &c.digest() == digest)
                    .ok_or_else(|| {
                        bad("the email configuration changed since the plan: plan again")
                    })?;
                let db = database_id(t, acc, &name, &mut database).await?;
                d1(t, acc, &db, &datastore::config_statement(&config, &now())).await?;
                "wrote the email configuration".to_string()
            }
            Step::CreateQueue {
                name: q,
                consumer,
                dead_letter,
            } => {
                let made = call(
                    t,
                    Method::Post,
                    &format!("/accounts/{acc}/queues"),
                    Some(&json!({ "queue_name": q })),
                )
                .await?;
                let id = made["queue_id"].as_str().unwrap_or_default().to_string();
                if let Some(script) = consumer {
                    call(
                        t,
                        Method::Post,
                        &format!("/accounts/{acc}/queues/{id}/consumers"),
                        Some(&json!({
                            "type": "worker", "script_name": script,
                            "dead_letter_queue": dead_letter,
                            "settings": { "batch_size": 10, "max_retries": datastore::BACKUP_MAX_RETRIES },
                        })),
                    )
                    .await?;
                }
                format!("created queue {q}")
            }
            Step::ConfigureBackupConsumer {
                queue,
                worker,
                dead_letter,
            } => {
                let id = backup_queue_id(t, acc, queue).await?;
                call(
                    t, Method::Post,
                    &format!("/accounts/{acc}/queues/{id}/consumers"),
                    Some(&json!({
                        "type": "worker", "script_name": worker,
                        "dead_letter_queue": dead_letter,
                        "settings": { "batch_size": 1, "max_retries": datastore::BACKUP_MAX_RETRIES }
                    })),
                ).await?;
                format!("configured backup consumer on {queue}")
            }
            Step::QueueBackupInventory { queue, generation } => {
                let id = backup_queue_id(t, acc, queue).await?;
                let body =
                    serde_json::to_string(&pigeon_ingest::backup_inventory::Message::Inventory {
                        generation: generation.clone(),
                        cursor: None,
                    })
                    .map_err(|_| bad("could not encode the backup inventory checkpoint"))?;
                call(
                    t,
                    Method::Post,
                    &format!("/accounts/{acc}/queues/{id}/messages"),
                    Some(&json!({ "body": body, "content_type": "json" })),
                )
                .await?;
                format!("queued the initial Data backup inventory on {queue}")
            }
            Step::AddBucketNotification { bucket, queue } => {
                let id = backup_queue_id(t, acc, queue).await?;
                call(
                    t,
                    Method::Put,
                    &format!("/accounts/{acc}/event_notifications/r2/{bucket}/configuration/queues/{id}"),
                    Some(&json!({ "rules": [{ "actions": ["PutObject", "CopyObject", "CompleteMultipartUpload"] }] })),
                )
                .await?;
                format!("bucket {bucket} now sends new objects to {queue}")
            }
            Step::RemoveStaleDns {
                zone_id,
                id,
                name,
                kind,
                content,
                ..
            } => {
                // Exactly the record shown: re-read it and delete only if it is unchanged.
                let now = get(t, &format!("/zones/{zone_id}/dns_records/{id}")).await?;
                let same = now.as_ref().is_some_and(|r| {
                    r["name"].as_str() == Some(name.as_str())
                        && r["type"].as_str() == Some(kind.as_str())
                        && r["content"].as_str() == Some(content.as_str())
                });
                if !same {
                    return Err(bad(format!(
                        "{kind} {name} changed since the plan; nothing more was done"
                    )));
                }
                call(
                    t,
                    Method::Delete,
                    &format!("/zones/{zone_id}/dns_records/{id}"),
                    None,
                )
                .await?;
                format!("deleted stale {kind} {name}")
            }
            Step::EnableEmailRouting { zone, zone_id, .. } => {
                call(
                    t,
                    Method::Post,
                    &format!("/zones/{zone_id}/email/routing/enable"),
                    Some(&json!({})),
                )
                .await?;
                format!("Email Routing on for {zone}")
            }
            Step::SetCatchAll {
                zone,
                zone_id,
                worker,
            } => {
                call(
                    t,
                    Method::Put,
                    &format!("/zones/{zone_id}/email/routing/rules/catch_all"),
                    Some(&json!({
                        "name": "pigeon: every address to the data Worker",
                        "enabled": true,
                        "matchers": [{ "type": "all" }],
                        "actions": [{ "type": "worker", "value": [worker] }],
                    })),
                )
                .await?;
                format!("catch-all of {zone} sends to {worker}")
            }
            Step::AddDestination { email } => {
                call(
                    t,
                    Method::Post,
                    &format!("/accounts/{acc}/email/routing/addresses"),
                    Some(&json!({ "email": email })),
                )
                .await?;
                format!("added forward destination {email}; Cloudflare emailed it a link")
            }
            other => {
                return Err(bad(format!(
                    "not a data step: {:?}",
                    plan::describe(std::slice::from_ref(other))
                )));
            }
        };
        plan::record(ledger, step);
        done.push(line);
    }
    Ok(done)
}

fn now() -> String {
    String::from(worker::js_sys::Date::new_0().to_iso_string())
}

/// The store's D1 id: the one made in this apply, else looked up by name.
pub(crate) async fn database_id(
    t: &str,
    acc: &str,
    name: &str,
    made: &mut Option<String>,
) -> Result<String> {
    if let Some(id) = made {
        return Ok(id.clone());
    }
    let dbs = call(
        t,
        Method::Get,
        &format!("/accounts/{acc}/d1/database?name={name}"),
        None,
    )
    .await?;
    let id = dbs
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["name"].as_str() == Some(name))
        .and_then(|d| d["uuid"].as_str())
        .ok_or_else(|| bad(format!("D1 database {name} is not there")))?
        .to_string();
    *made = Some(id.clone());
    Ok(id)
}

/// The modules of `built`, read from the platform's own account and checked against its
/// digest: `(name, content type, bytes)`, in order.
pub async fn modules(parent: &Account, built: &Artifact) -> Result<Vec<(String, String, Vec<u8>)>> {
    let bucket = pigeon_core::r2::Bucket {
        name: data_plan::ARTIFACT_BUCKET.into(),
        jurisdiction: None,
    };
    let mut parts = Vec::new();
    for m in &built.modules {
        let bytes = pigeon_core::r2::get(
            parent,
            &bucket,
            &data_plan::module_key(&built.digest, &m.name),
        )
        .await?
        .ok_or_else(|| bad(format!("module {} of the build is missing", m.name)))?;
        parts.push((m.name.clone(), m.content_type.clone(), bytes));
    }
    let named: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(n, _, b)| (n.as_str(), b.as_slice()))
        .collect();
    if data_plan::artifact_digest(&named) != built.digest {
        return Err(bad(
            "the build's modules do not match its digest; nothing was uploaded",
        ));
    }
    Ok(parts)
}

/// A new `DATA_KEY`.
fn new_key() -> Result<String> {
    Ok(pigeon_core::sign::hex(
        pigeon_core::webauthn::random()?.as_bytes(),
    ))
}

/// The account's workers.dev subdomain.
async fn account_subdomain(token: &str, acc: &str) -> Result<String> {
    let sub = call(
        token,
        Method::Get,
        &format!("/accounts/{acc}/workers/subdomain"),
        None,
    )
    .await?;
    sub["subdomain"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad("the account has no workers.dev subdomain"))
}

/// Keeps the store's key and URL in the Directory, where `data_store::reach` reads them.
async fn keep_reach(env: &Env, instance: &Instance, key: String, url: String) -> Result<()> {
    for (k, v) in [
        (crate::data_store::key_setting(&instance.id), key),
        (crate::data_store::url_setting(&instance.id), url),
    ] {
        directory(
            env,
            "setting.put",
            json!({ "key": k, "value": v, "ttl_ms": KEPT_MS }),
        )
        .await?;
    }
    Ok(())
}

/// What an inline store's upload needs (docs/plans/linked-data.md, decisions 4 and 5).
pub struct Inline<'a> {
    pub env: &'a Env,
    /// The platform's own account, which holds the builds.
    pub parent: &'a Account,
    /// The linked account, with its link's token as bearer.
    pub account: &'a Account,
    pub instance: &'a Instance,
    /// The instance Worker.
    pub worker: &'a str,
    /// The site module, as an instance without a store has it (redirects included).
    pub site: &'a str,
    /// The site's bindings.
    pub bindings: &'a [pigeon_core::workers::Binding],
    pub data: &'a plan::InlineData,
}

/// Uploads the instance Worker with its data store inline: the composing module as
/// `worker.js`, the site module as `site.js` and the data Worker's build, with the site's
/// bindings and the store's (`DATA`, `DB`, `INSTANCE` and a new `DATA_KEY`). Keeps the key and
/// the Worker's workers.dev URL in the Directory; the plan's `EnableWorkersDev` turns that
/// address on.
pub async fn upload_inline(i: &Inline<'_>) -> Result<()> {
    let token = i
        .account
        .bearer
        .as_deref()
        .ok_or_else(|| bad("the instance's account is not a linked one"))?;
    let acc = i.account.id.as_str();
    let built = artifact(i.parent)
        .await?
        .filter(|b| b.digest == i.data.artifact)
        .ok_or_else(|| bad("the approved build is no longer the newest: plan again"))?;
    let data_modules = modules(i.parent, &built).await?;
    if data_modules
        .iter()
        .any(|(n, _, _)| n == INLINE_MAIN || n == INLINE_SITE)
    {
        return Err(bad(
            "the data Worker's build names a module worker.js or site.js; nothing was uploaded",
        ));
    }
    let mut database = None;
    let db = database_id(token, acc, &i.data.database, &mut database).await?;
    let key = new_key()?;
    let site = pigeon_core::workers::metadata(super::plan::COMPATIBILITY_DATE, i.bindings);
    let slot = crate::data_store::b2_slot(i.instance);
    let backup = crate::data_store::backup(i.instance);
    let secrets_store = if backup.is_some() {
        crate::data_store::verify_data_jobs_audit(i.account, i.instance).await?;
        Some(
            crate::secrets::linked_store(i.account)
                .await?
                .ok_or_else(|| bad("The linked instance's Secrets Store disappeared."))?,
        )
    } else {
        None
    };
    let mut data = data_plan::worker_metadata(
        INLINE_MAIN,
        &data_plan::WorkerBindings {
            instance: &i.instance.id,
            bucket: &i.data.bucket,
            database_id: &db,
            data_key: &key,
            backup: backup
                .zip(secrets_store.as_deref())
                .map(|(b, store)| (b, store, slot.as_str())),
        },
    );
    data_plan::bind_backup_audit(&mut data, i.instance);
    data_plan::bind_backup_inventory(&mut data, i.instance);
    let mut bindings = site["bindings"].as_array().cloned().unwrap_or_default();
    bindings.extend(data["bindings"].as_array().cloned().unwrap_or_default());
    let metadata = json!({
        "main_module": INLINE_MAIN,
        "compatibility_date": super::plan::COMPATIBILITY_DATE,
        "bindings": bindings,
    });
    let js = "application/javascript+module".to_string();
    let mut parts = vec![
        (
            INLINE_MAIN.to_string(),
            js.clone(),
            plan::data_inline_module(&built.main).into_bytes(),
        ),
        (INLINE_SITE.to_string(), js, i.site.as_bytes().to_vec()),
    ];
    parts.extend(data_modules);
    let boundary = pigeon_core::workers::boundary_from(&pigeon_core::webauthn::random()?);
    let body = data_plan::multipart(&boundary, &metadata, &parts);
    let ctype = pigeon_core::workers::content_type(&boundary);
    let worker = i.worker;
    call_raw(
        token,
        Method::Put,
        &format!("/accounts/{acc}/workers/scripts/{worker}"),
        Some((&ctype, body)),
    )
    .await
    .map_err(|_| {
        bad(format!(
            "uploading Worker {worker} with its data store failed"
        ))
    })?;
    let url = plan::upstream_for(worker, &account_subdomain(token, acc).await?);
    keep_reach(i.env, i.instance, key, url).await
}

/// The inline Worker's main module and its site module, by name.
const INLINE_MAIN: &str = "worker.js";
const INLINE_SITE: &str = "site.js";

/// Uploads the data Worker from `built` (each module checked against its digest), with a new
/// `DATA_KEY` kept in the Directory; turns on its workers.dev address and keeps its URL.
async fn upload(
    a: &Apply<'_>,
    instance: &Instance,
    name: &str,
    built: &Artifact,
    database_id: &str,
    backup: bool,
) -> Result<()> {
    if backup && instance.account != crate::instances::record::AccountRef::Home {
        let account = Account {
            id: a.account_id.to_string(),
            parent: String::new(),
            secrets_store: a.secrets_store.to_string(),
            bearer: Some(a.token.to_string()),
            token_ledger: None,
        };
        crate::data_store::verify_data_jobs_audit(&account, instance).await?;
    }
    let parts = modules(a.parent, built).await?;
    let key = new_key()?;
    let slot = crate::data_store::b2_slot(instance);
    let settings = crate::data_store::settings(instance);
    let bindings = data_plan::WorkerBindings {
        instance: &instance.id,
        bucket: name,
        database_id,
        data_key: &key,
        backup: settings
            .and_then(|s| s.backup.as_ref())
            .filter(|_| backup)
            .map(|b| (b, a.secrets_store, slot.as_str())),
    };
    let mut metadata = data_plan::worker_metadata(&built.main, &bindings);
    data_plan::bind_backup_audit(&mut metadata, instance);
    data_plan::bind_backup_inventory(&mut metadata, instance);
    let boundary = pigeon_core::workers::boundary_from(&pigeon_core::webauthn::random()?);
    let body = data_plan::multipart(&boundary, &metadata, &parts);
    let ctype = pigeon_core::workers::content_type(&boundary);
    let (t, acc) = (a.token, a.account_id);
    call_raw(
        t,
        Method::Put,
        &format!("/accounts/{acc}/workers/scripts/{name}"),
        Some((&ctype, body)),
    )
    .await
    .map_err(|_| bad(format!("uploading data Worker {name} failed")))?;
    call(
        t,
        Method::Post,
        &format!("/accounts/{acc}/workers/scripts/{name}/subdomain"),
        Some(&json!({ "enabled": true })),
    )
    .await?;
    let url = plan::upstream_for(name, &account_subdomain(t, acc).await?);
    keep_reach(a.env, instance, key, url).await
}

/// How long the store's key and URL are kept in the Directory: a century.
const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;

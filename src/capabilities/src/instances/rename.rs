//! R1: the home instance's id rename, `pigeon-security/platform` → `pigeon.dev`
//! (docs/plans/platform-self-hosting.md, "R1: instance id rename migration").
//!
//! Two halves:
//!
//! - **The alias** ([`ALIASES`], [`Stored`], [`Tombstone`]): inert until the migration has run.
//!   The migration leaves a tombstone at the old record's key naming the new id; while it is
//!   there, record reads, CI scope reads, console URLs, observability and every tool's
//!   `instance` argument naming the old id are answered for the new one. Before it, nothing
//!   reads differently.
//! - **The migration** ([`plan`], [`run`]): the steps derived from what is stored now, so a
//!   second run finds nothing to do and a run that stopped part way plans only what remains.
//!   Each step is safe to run again. It runs once, after the person approves exactly that plan
//!   with their passkey (`instance_rename_home`, in `renaming.rs`); the approval binds the old
//!   id, the new id, the home record's digest and the steps.
//!
//! Step 5 of the plan (not now): once [`COMPAT_MS`] has passed since the rename, a change
//! removes the [`ALIASES`] entry and the tombstone (the key `instance:pigeon-security/platform`),
//! and the tests and docs that name the old id.
//!
//! This module is pure but for [`observe`] and [`run`], which go through a [`Store`]: the
//! Directory and the CI scheduler in production, a double in the smoke tests.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::Result;

use super::record::{AccountRef, Instance};

/// The home instance's id before R1.
pub const FROM: &str = "pigeon-security/platform";
/// The home instance's id after R1: its zone.
pub const TO: &str = "pigeon.dev";
/// The renames the platform answers for, old id to new id, once each one's tombstone exists.
/// Step 5 removes the entry.
pub const ALIASES: &[(&str, &str)] = &[(FROM, TO)];
/// How long old ids are answered for after the rename: 30 days. Recorded in the tombstone.
pub const COMPAT_MS: i64 = 30 * 24 * 3_600_000;
/// The migration's tool.
pub const TOOL: &str = "instance_rename_home";

/// The id `id` is renamed to by an [`ALIASES`] entry, whether or not the rename has run. Pure.
pub fn alias_of(id: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(from, _)| *from == id)
        .map(|(_, to)| *to)
}

/// The id that an [`ALIASES`] entry renames to `id`, whether or not the rename has run. Pure.
pub fn renamed_from(id: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find(|(_, to)| *to == id)
        .map(|(from, _)| *from)
}

/// What the old record's key holds once the rename has run.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    /// The id the instance has now.
    pub renamed_to: String,
    /// The id it had: this key's.
    pub renamed_from: String,
    /// When it was renamed, in milliseconds since the epoch.
    pub renamed_ms: i64,
    /// Until when old ids are answered (console URLs, CI reads, MCP calls): step 5 removes the
    /// alias and this tombstone after it.
    pub compat_until_ms: i64,
    /// The approval the person gave with their passkey.
    pub approval: String,
}

/// What an `instance:{id}` key holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Stored {
    Record(Box<Instance>),
    Tombstone(Tombstone),
}

impl Stored {
    /// The value at an `instance:{id}` key: a tombstone when it names `renamed_to`, else a
    /// record. Pure.
    pub fn parse(text: &str) -> serde_json::Result<Self> {
        let v: Value = serde_json::from_str(text)?;
        if v.get("renamed_to").is_some() {
            Ok(Stored::Tombstone(serde_json::from_value(v)?))
        } else {
            Ok(Stored::Record(Box::new(serde_json::from_value(v)?)))
        }
    }
}

/// The state keys one instance may hold besides its record, moved with it: its ledger, its
/// GitHub deploy and push state, its ingest runs and data store (the home instance has none
/// but the ledger; each is moved only when present).
pub fn state_keys(id: &str) -> Vec<String> {
    super::retire::state_keys(id)
}

/// One state key's value under the old id and under the new one.
#[derive(Clone, Debug, PartialEq)]
pub struct SeenKey {
    pub from: String,
    pub to: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// What is stored now, for [`plan`]. Read by [`observe`].
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    /// What `instance:{from}` holds.
    pub old: Option<Stored>,
    /// What `instance:{to}` holds.
    pub new: Option<Stored>,
    /// The instances index.
    pub index: Vec<String>,
    pub keys: Vec<SeenKey>,
    /// The open approvals (pending or approved) whose subject is the old id, other than rename
    /// plans.
    pub approvals: Vec<String>,
    /// The CI jobs attributed to the old id.
    pub jobs: u64,
}

/// One step of the rename.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Write the renamed record (the same record, its id the new one and a version on) at
    /// `instance:{to}`.
    WriteRecord { key: String, record: Box<Instance> },
    /// Write `from`'s value at `to` (unless `to` already holds it), then forget `from`.
    MoveKey { from: String, to: String },
    /// Expire the open approvals whose subject is the old id: their digests name it.
    ExpireApprovals { subject: String, ids: Vec<String> },
    /// The index names `to` and no longer `from`.
    SwapIndex { from: String, to: String },
    /// `UPDATE jobs SET instance = to WHERE instance = from` in the CI scheduler.
    ReattributeJobs { from: String, to: String, jobs: u64 },
    /// Replace the old record with a [`Tombstone`] naming `renamed_to`, its compat window
    /// starting when it is written.
    Tombstone {
        key: String,
        renamed_from: String,
        renamed_to: String,
    },
    /// Something is in the way: nothing runs.
    Blocked { why: String },
}

impl Step {
    /// Whether this step stops the plan.
    pub fn stops(&self) -> bool {
        matches!(self, Step::Blocked { .. })
    }
}

fn blocked(why: String) -> Vec<Step> {
    vec![Step::Blocked { why }]
}

/// The home record renamed: id `to`, a version on, the rest the same. Pure.
pub fn renamed(record: &Instance, to: &str) -> Instance {
    let mut next = record.clone();
    next.id = to.to_string();
    next.version = record.version + 1;
    next
}

/// The steps renaming the home instance `from` → `to`, from what is stored now. Empty when the
/// rename has run and nothing remains; one [`Step::Blocked`] when it may not run: the old id is
/// not the home instance, the new id is not one of its zones, `to` already holds a different
/// record, or a state key already holds a different value under the new id. Pure.
pub fn plan(seen: &Seen, from: &str, to: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut tombstone = false;
    match (&seen.old, &seen.new) {
        (None, Some(Stored::Record(n))) if n.account == AccountRef::Home && n.id == to => {
            // Renamed, and its tombstone already removed (step 5): nothing is left to do.
            return steps;
        }
        (None, _) => return blocked(format!("there is no instance {from}")),
        (Some(Stored::Tombstone(t)), Some(Stored::Record(n)))
            if t.renamed_to == to && n.id == to && n.account == AccountRef::Home => {}
        (Some(Stored::Tombstone(t)), _) => {
            return blocked(format!(
                "instance {from} was renamed to {}, but {to} holds no home record",
                t.renamed_to
            ));
        }
        (Some(Stored::Record(old)), new) => {
            if old.account != AccountRef::Home {
                return blocked(format!("instance {from} is not the home instance"));
            }
            if !old.zones.iter().any(|z| z == to) {
                return blocked(format!(
                    "{to} is not one of the home instance's zones ({}): the home id must be \
                     one of them",
                    old.zones.join(", ")
                ));
            }
            let next = renamed(old, to);
            match new {
                None => steps.push(Step::WriteRecord {
                    key: super::key(to),
                    record: Box::new(next),
                }),
                Some(Stored::Record(n)) if **n == next => {}
                Some(Stored::Record(n)) => {
                    return blocked(format!(
                        "{to} already exists as a different record (version {}, account {}); \
                         nothing was changed",
                        n.version,
                        match n.account {
                            AccountRef::Home => "home",
                            AccountRef::Linked { .. } => "linked",
                        }
                    ));
                }
                Some(Stored::Tombstone(_)) => {
                    return blocked(format!("{to} is itself a renamed id"));
                }
            }
            tombstone = true;
        }
    }
    for k in &seen.keys {
        match (&k.old, &k.new) {
            (None, _) => {}
            (Some(a), Some(b)) if a != b => {
                return blocked(format!(
                    "{} already holds a different value than {}; nothing was changed",
                    k.to, k.from
                ));
            }
            _ => steps.push(Step::MoveKey {
                from: k.from.clone(),
                to: k.to.clone(),
            }),
        }
    }
    if !seen.approvals.is_empty() {
        steps.push(Step::ExpireApprovals {
            subject: from.to_string(),
            ids: seen.approvals.clone(),
        });
    }
    if seen.index.iter().any(|i| i == from) || !seen.index.iter().any(|i| i == to) {
        steps.push(Step::SwapIndex {
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    if seen.jobs > 0 {
        steps.push(Step::ReattributeJobs {
            from: from.to_string(),
            to: to.to_string(),
            jobs: seen.jobs,
        });
    }
    if tombstone {
        steps.push(Step::Tombstone {
            key: super::key(from),
            renamed_from: from.to_string(),
            renamed_to: to.to_string(),
        });
    }
    steps
}

/// The digest of the old record as stored: what the approval binds besides the ids and steps.
/// Empty once the old record is a tombstone. Pure.
pub fn record_digest(seen: &Seen) -> String {
    match &seen.old {
        Some(Stored::Record(r)) => super::record::digest(r),
        _ => String::new(),
    }
}

/// What the approval binds: the old id, the new id, the old record's digest and the steps.
pub fn payload(from: &str, to: &str, record_digest: &str, steps: &[Step]) -> Value {
    json!({
        "rename": true,
        "from": from,
        "to": to,
        "record_digest": record_digest,
        "steps": steps,
    })
}

/// The plan as the person reads it: one line per step. Pure.
pub fn describe(steps: &[Step]) -> Vec<Value> {
    steps
        .iter()
        .map(|s| {
            json!(match s {
                Step::WriteRecord { key, record } => format!(
                    "write {key}: the home record as {}, version {}",
                    record.id, record.version
                ),
                Step::MoveKey { from, to } => format!("move {from} to {to}"),
                Step::ExpireApprovals { subject, ids } => format!(
                    "expire {} open approval(s) for {subject}: {}",
                    ids.len(),
                    ids.join(", ")
                ),
                Step::SwapIndex { from, to } =>
                    format!("list {to} in instances:index in place of {from}"),
                Step::ReattributeJobs { from, to, jobs } => format!(
                    "re-attribute {jobs} CI job(s) from {from} to {to} (UPDATE jobs SET instance)"
                ),
                Step::Tombstone {
                    key, renamed_to, ..
                } => format!(
                    "replace {key} with a tombstone naming {renamed_to}: old console URLs, CI \
                     reads and MCP calls naming the old id are answered for {renamed_to} for \
                     30 days"
                ),
                Step::Blocked { why } => format!("stop: {why}"),
            })
        })
        .collect()
}

/// Where the rename reads and writes: the Directory and the CI scheduler in production, a
/// double in the smoke tests.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait Store {
    /// A Directory setting's value; `None` when absent or forgotten.
    async fn get(&self, key: &str) -> Result<Option<String>>;
    /// Keeps a Directory setting for good.
    async fn put(&self, key: &str, value: &str) -> Result<()>;
    /// Forgets a Directory setting.
    async fn forget(&self, key: &str) -> Result<()>;
    /// The open approvals whose subject is `subject`, other than rename plans.
    async fn open_approvals(&self, subject: &str) -> Result<Vec<String>>;
    /// Expires those of approvals `ids` still open, noting `note`.
    async fn expire_approvals(&self, ids: &[String], note: &str) -> Result<()>;
    /// The CI jobs attributed to `from`; with `apply`, re-attributes them to `to` first.
    async fn jobs(&self, from: &str, to: &str, apply: bool) -> Result<u64>;
}

/// What instance `id`'s key holds: its record, a tombstone, or nothing.
pub async fn stored<S: Store>(store: &S, id: &str) -> Result<Option<Stored>> {
    Ok(match store.get(&super::key(id)).await? {
        Some(text) => Some(Stored::parse(&text)?),
        None => None,
    })
}

/// Instance `id`'s record, if there is one; an id whose rename has run answers the record it
/// was renamed to. Reads only.
pub async fn record<S: Store>(store: &S, id: &str) -> Result<Option<Instance>> {
    Ok(match stored(store, id).await? {
        Some(Stored::Record(r)) => Some(*r),
        Some(Stored::Tombstone(t)) => match stored(store, &t.renamed_to).await? {
            Some(Stored::Record(r)) => Some(*r),
            _ => None,
        },
        None => None,
    })
}

/// The id `id` was renamed to, once its rename has run: only an [`ALIASES`] id whose tombstone
/// exists. Any other id is answered without a read; before the migration, `None`.
pub async fn resolve<S: Store>(store: &S, id: &str) -> Result<Option<String>> {
    let Some(to) = alias_of(id) else {
        return Ok(None);
    };
    Ok(match stored(store, id).await? {
        Some(Stored::Tombstone(t)) if t.renamed_to == to => Some(t.renamed_to),
        _ => None,
    })
}

/// `args` with its `instance` argument naming the id it was renamed to, when it names a renamed
/// id ([`resolve`]); `None` when nothing changes.
pub async fn current_args<S: Store>(store: &S, args: &Value) -> Result<Option<Value>> {
    let Some(id) = args.get("instance").and_then(Value::as_str) else {
        return Ok(None);
    };
    Ok(resolve(store, id).await?.map(|to| {
        let mut args = args.clone();
        args["instance"] = Value::String(to);
        args
    }))
}

/// What is stored now for renaming `from` → `to`. Reads only.
pub async fn observe<S: Store>(store: &S, from: &str, to: &str) -> Result<Seen> {
    let stored = |text: Option<String>| -> Result<Option<Stored>> {
        Ok(match text {
            Some(t) => Some(Stored::parse(&t)?),
            None => None,
        })
    };
    let old = stored(store.get(&super::key(from)).await?)?;
    let new = stored(store.get(&super::key(to)).await?)?;
    let index: Vec<String> = match store.get(super::INDEX).await? {
        Some(t) => serde_json::from_str(&t)?,
        None => Vec::new(),
    };
    let mut keys = Vec::new();
    for (a, b) in state_keys(from).into_iter().zip(state_keys(to)) {
        keys.push(SeenKey {
            old: store.get(&a).await?,
            new: store.get(&b).await?,
            from: a,
            to: b,
        });
    }
    Ok(Seen {
        old,
        new,
        index,
        keys,
        approvals: store.open_approvals(from).await?,
        jobs: store.jobs(from, to, false).await?,
    })
}

/// Runs `steps` in order, `approval` being the person's approval and `now_ms` the time: what
/// each did. Each step is safe to run again, so a run that stopped is planned and run again
/// for what remains.
pub async fn run<S: Store>(
    store: &S,
    steps: &[Step],
    approval: &str,
    now_ms: i64,
) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for step in steps {
        let line = match step {
            Step::Blocked { why } => {
                return Err(worker::Error::RustError(format!(
                    "the rename does not run: {why}"
                )));
            }
            Step::WriteRecord { key, record } => {
                store.put(key, &serde_json::to_string(record)?).await?;
                format!("wrote {key}")
            }
            Step::MoveKey { from, to } => {
                if let Some(value) = store.get(from).await? {
                    if store.get(to).await?.is_none() {
                        store.put(to, &value).await?;
                    }
                    store.forget(from).await?;
                }
                format!("moved {from} to {to}")
            }
            Step::ExpireApprovals { subject, ids } => {
                let note = format!("instance {subject} was renamed; ask again for its new id");
                store.expire_approvals(ids, &note).await?;
                format!("expired {} approval(s) for {subject}", ids.len())
            }
            Step::SwapIndex { from, to } => {
                let mut index: Vec<String> = match store.get(super::INDEX).await? {
                    Some(t) => serde_json::from_str(&t)?,
                    None => Vec::new(),
                };
                index.retain(|i| i != from);
                if !index.contains(to) {
                    index.push(to.clone());
                }
                index.sort();
                store
                    .put(super::INDEX, &serde_json::to_string(&index)?)
                    .await?;
                format!("instances:index lists {to}, not {from}")
            }
            Step::ReattributeJobs { from, to, .. } => {
                let n = store.jobs(from, to, true).await?;
                format!("re-attributed {n} CI job(s) from {from} to {to}")
            }
            Step::Tombstone {
                key,
                renamed_from,
                renamed_to,
            } => {
                let t = Tombstone {
                    renamed_to: renamed_to.clone(),
                    renamed_from: renamed_from.clone(),
                    renamed_ms: now_ms,
                    compat_until_ms: now_ms.saturating_add(COMPAT_MS),
                    approval: approval.to_string(),
                };
                store.put(key, &serde_json::to_string(&t)?).await?;
                format!("{key} is a tombstone naming {renamed_to}")
            }
        };
        done.push(line);
    }
    Ok(done)
}

//! The instance record's layout (docs/plans/record-reshape.md): where each setting sits in the
//! record as it is stored, listed, diffed and digested.
//!
//! [`Instance`] is the in-memory model, keyed by catalogue id. [`Model`] is its field-by-field
//! serde form (the flat layout records had before the reshape). [`shape`] turns that into the
//! record's layout: each catalogue capability at its entry's `field` (`data.email`,
//! `cloud.gateway`, …), `publishing` and the sites' passkey sign-in under `site`, `source` under
//! `repo`, `redirects` under `network`. [`unshape`] reads the layout back, and also the flat one
//! (with `email` as `data-email`, `storage` as `data.read`), so records not yet migrated and
//! drafts proposed before the change still read. A setting given in both places is refused.
//! [`migrate`] rewrites the stored records once, when the Directory starts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::record::{
    AccountRef, CapabilitySettings, Instance, PlatformRef, Redirect, RepoRef, SiteRecord, Toggle,
};

/// The serde form of [`Instance`], field by field: the flat layout.
#[derive(Serialize, Deserialize)]
#[serde(remote = "Instance", deny_unknown_fields)]
pub(crate) struct Model {
    id: String,
    title: String,
    account: AccountRef,
    #[serde(default)]
    zones: Vec<String>,
    #[serde(default)]
    repo: Option<RepoRef>,
    #[serde(default)]
    sites: Vec<SiteRecord>,
    #[serde(default)]
    worker: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    worker_override: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pins: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publishing: Option<pigeon_core::site_publish::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ci: Option<crate::ci::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observability: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    redirects: Vec<Redirect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<PlatformRef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    capabilities: BTreeMap<String, CapabilitySettings>,
    #[serde(default)]
    version: u64,
}

/// The namespaces of the layout that are no setting of their own: an empty one reads as absent
/// and is never written.
pub const NAMESPACES: &[&str] = &["site", "network", "cloud", "instance"];

/// The catalogue ids the flat layout used before a rename: `email` is `data-email`.
const RENAMED: &[(&str, &str)] = &[("email", crate::email::NAME)];

/// Where catalogue capability `name` sits in the record: its entry's `field`, or
/// `capabilities.<name>`.
pub fn path_of(name: &str) -> Vec<String> {
    match crate::catalogue::find(name).and_then(|e| e.field) {
        Some(f) => f.split('.').map(str::to_string).collect(),
        None => vec!["capabilities".into(), name.into()],
    }
}

/// The catalogue entries with a `field`, deepest first, so children are taken out before the
/// capability they sit in (`data.email` before `data`).
fn fields() -> Vec<(&'static str, Vec<&'static str>)> {
    let mut out: Vec<(&'static str, Vec<&'static str>)> = crate::catalogue::CATALOGUE
        .iter()
        .filter_map(|e| e.field.map(|f| (e.name, f.split('.').collect())))
        .collect();
    out.sort_by_key(|(_, p)| std::cmp::Reverse(p.len()));
    out
}

/// Puts `value` at `path` in `out`, making the objects on the way; an object meeting an object
/// is merged (a capability's settings beside its children).
fn put(out: &mut Map<String, Value>, path: &[String], value: Value) {
    let (last, parents) = path.split_last().expect("a path has a segment");
    let mut at = out;
    for p in parents {
        let next = at
            .entry(p.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        if !next.is_object() {
            *next = Value::Object(Map::new());
        }
        at = next.as_object_mut().expect("made an object");
    }
    match (at.get_mut(last), value) {
        (Some(Value::Object(have)), Value::Object(more)) => have.extend(more),
        (_, value) => {
            at.insert(last.clone(), value);
        }
    }
}

/// The flat form (what [`Model`] writes) in the record's layout.
pub fn shape(flat: Value) -> Value {
    let Value::Object(mut m) = flat else {
        return flat;
    };
    let mut out = Map::new();
    let mut residual = Map::new();
    if let Some(Value::Object(caps)) = m.remove("capabilities") {
        for (name, settings) in caps {
            if crate::catalogue::find(&name)
                .and_then(|e| e.field)
                .is_some()
            {
                put(&mut out, &path_of(&name), settings);
            } else {
                residual.insert(name, settings);
            }
        }
    }
    if !residual.is_empty() {
        out.insert("capabilities".into(), Value::Object(residual));
    }
    if let Some(p) = m.remove("publishing") {
        put(&mut out, &["site".into(), "publishing".into()], p);
    }
    if let Some(Value::Array(sites)) = m.get_mut("sites") {
        for site in sites.iter_mut().filter_map(Value::as_object_mut) {
            if site.get("auth").and_then(Value::as_str) != Some("passkey") {
                continue;
            }
            site.remove("auth");
            let allow = site.remove("allow").unwrap_or_else(|| json!([]));
            let name = site
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            put(
                &mut out,
                &["site".into(), "passkeys".into(), name],
                json!({ "allow": allow }),
            );
        }
    }
    if let Some(source) = m.remove("source") {
        let mut repo = match m.remove("repo") {
            Some(Value::Object(r)) => r,
            _ => Map::new(),
        };
        repo.insert("source".into(), source);
        m.insert("repo".into(), Value::Object(repo));
    }
    if let Some(r) = m.remove("redirects") {
        put(&mut out, &["network".into(), "redirects".into()], r);
    }
    for (k, v) in out {
        m.insert(k, v);
    }
    Value::Object(m)
}

/// Takes the value at `path` out of `m`, if there is one.
fn take(m: &mut Map<String, Value>, path: &[&str]) -> Option<Value> {
    let (last, parents) = path.split_last()?;
    let mut at = m;
    for p in parents {
        at = at.get_mut(*p)?.as_object_mut()?;
    }
    at.remove(*last)
}

/// Sets flat key `key` of `m` to `value`, unless the old layout already set it.
fn set(m: &mut Map<String, Value>, key: &str, value: Value, path: &str) -> Result<(), String> {
    if m.contains_key(key) {
        return Err(format!(
            "the record sets both {path} and the old {key}: keep {path}"
        ));
    }
    m.insert(key.into(), value);
    Ok(())
}

/// The record, in its layout or the flat one before it, in the flat form [`Model`] reads.
pub fn unshape(value: Value) -> Result<Value, String> {
    let Value::Object(mut m) = value else {
        return Err("an instance record is a JSON object".into());
    };
    // Catalogue capabilities, by id, from `capabilities` (the old ids renamed) and their fields.
    let mut caps = match m.remove("capabilities") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(c)) => c,
        Some(_) => return Err("capabilities must be an object".into()),
    };
    for (old, new) in RENAMED {
        if let Some(v) = caps.remove(*old) {
            if caps.contains_key(*new) {
                return Err(format!(
                    "the record sets both capabilities.{old} and {}: keep {}",
                    path_of(new).join("."),
                    path_of(new).join(".")
                ));
            }
            caps.insert((*new).into(), v);
        }
    }
    for (name, path) in fields() {
        let Some(v) = take(&mut m, &path) else {
            continue;
        };
        if caps.contains_key(name) {
            return Err(format!(
                "the record sets both {} and capabilities.{name}: keep {}",
                path.join("."),
                path.join(".")
            ));
        }
        caps.insert(name.into(), v);
    }
    // `site`: publishing and the sites' passkey sign-in.
    let mut passkeys = Map::new();
    if let Some(site) = m.remove("site") {
        let Value::Object(mut site) = site else {
            return Err("site must be an object".into());
        };
        if let Some(p) = site.remove("publishing") {
            set(&mut m, "publishing", p, "site.publishing")?;
        }
        match site.remove("passkeys") {
            None => {}
            Some(Value::Object(p)) => passkeys = p,
            Some(_) => return Err("site.passkeys must be an object of site names".into()),
        }
        if let Some(k) = site.keys().next() {
            return Err(format!("unknown field site.{k}"));
        }
    }
    if !passkeys.is_empty() {
        let Some(Value::Array(sites)) = m.get_mut("sites") else {
            return Err("site.passkeys names sites, and the record has none".into());
        };
        for (name, entry) in passkeys {
            let Value::Object(mut entry) = entry else {
                return Err(format!("site.passkeys.{name} must be an object"));
            };
            let allow = entry.remove("allow").unwrap_or_else(|| json!([]));
            if let Some(k) = entry.keys().next() {
                return Err(format!("unknown field site.passkeys.{name}.{k}"));
            }
            let Some(site) = sites
                .iter_mut()
                .filter_map(Value::as_object_mut)
                .find(|s| s.get("name").and_then(Value::as_str) == Some(name.as_str()))
            else {
                return Err(format!("site.passkeys.{name} names no site of the record"));
            };
            if site.contains_key("auth") {
                return Err(format!(
                    "the record sets both site.passkeys.{name} and the old sites[].auth: keep site.passkeys"
                ));
            }
            site.insert("auth".into(), json!("passkey"));
            site.insert("allow".into(), allow);
        }
    }
    // `repo.source`.
    if let Some(Value::Object(repo)) = m.get_mut("repo")
        && let Some(source) = repo.remove("source")
    {
        let none = repo.is_empty();
        set(&mut m, "source", source, "repo.source")?;
        if none {
            m.insert("repo".into(), Value::Null);
        }
    }
    // `network.redirects`.
    if let Some(network) = m.remove("network") {
        let Value::Object(mut network) = network else {
            return Err("network must be an object".into());
        };
        if let Some(r) = network.remove("redirects") {
            set(&mut m, "redirects", r, "network.redirects")?;
        }
        if let Some(k) = network.keys().next() {
            return Err(format!("unknown field network.{k}"));
        }
    }
    // What is left of a namespace is unknown.
    for ns in NAMESPACES {
        match m.remove(*ns) {
            None => {}
            Some(Value::Object(rest)) if rest.is_empty() => {}
            Some(Value::Object(rest)) => {
                let k = rest.keys().next().cloned().unwrap_or_default();
                return Err(format!("unknown field {ns}.{k}"));
            }
            Some(_) => return Err(format!("{ns} must be an object")),
        }
    }
    // The old `storage` profile is the data store's `read`.
    if let Some(storage) = m.remove("storage") {
        let Some(Value::Object(data)) = caps.get_mut(crate::data_store::NAME) else {
            return Err(
                "the record's old storage profile is read by the data store now: put it under \
                 data.read, with data enabled"
                    .into(),
            );
        };
        if data.contains_key("read") {
            return Err(
                "the record sets both data.read and the old storage: keep data.read".into(),
            );
        }
        data.insert("read".into(), storage);
    }
    // The old `ingest` profile is removed: `data.ingest` supersedes it.
    if let Some(ingest) = m.remove("ingest")
        && ingest.get("state").and_then(Value::as_str) != Some("disabled")
    {
        return Err(
            "the old ingest profile is removed: enable data.ingest (data-ingest) instead".into(),
        );
    }
    if !caps.is_empty() {
        m.insert("capabilities".into(), Value::Object(caps));
    }
    Ok(Value::Object(m))
}

/// The Directory setting that keeps the migration's log.
pub const MIGRATION_KEY: &str = "migration:record-reshape";

/// One record the migration rewrote.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Migrated {
    pub instance: String,
    /// The digest of the record as it was stored: what `record::digest` answered before.
    pub before: String,
    /// The digest of the record in its new layout.
    pub after: String,
    pub at_ms: i64,
}

/// What the migration does with one stored record's text: `Ok(Some(text))` to write it in the
/// new layout, `Ok(None)` when it already is (or is no record, such as a rename's tombstone),
/// `Err` when it cannot be read.
pub fn migrated(text: &str) -> Result<Option<String>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if value.get("renamed_to").is_some() {
        return Ok(None);
    }
    let record: Instance = serde_json::from_value(value).map_err(|e| e.to_string())?;
    let next = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    Ok((next != text).then_some(next))
}

/// The Directory's view of its settings, for [`migrate`].
pub trait Settings {
    /// The value and expiry of setting `key`, if it is kept.
    fn get(&self, key: &str) -> Option<(String, i64)>;
    /// Keeps `value` under `key` until `until`.
    fn keep(&self, key: &str, value: &str, until: i64);
}

/// What one run of [`migrate`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Migration {
    /// The records it rewrote, each with its digests before and after.
    pub done: Vec<Migrated>,
    /// The records it could not read, each with why: left as they are.
    pub failed: Vec<(String, String)>,
}

/// Rewrites every stored record that is not in the new layout yet, with identical settings and
/// the same version and expiry, and keeps a log of each one with its digests before and after
/// under [`MIGRATION_KEY`]. Never fails: a record it cannot read is left as it is and reported.
pub fn migrate(settings: &dyn Settings, now_ms: i64) -> Migration {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let Some((index, _)) = settings.get(super::INDEX) else {
        return Migration::default();
    };
    let ids: Vec<String> = serde_json::from_str(&index).unwrap_or_default();
    for id in ids {
        let key = super::key(&id);
        let Some((text, until)) = settings.get(&key) else {
            continue;
        };
        match migrated(&text) {
            Ok(Some(next)) => {
                let m = Migrated {
                    instance: id,
                    before: pigeon_core::oauth::hash(&text),
                    after: pigeon_core::oauth::hash(&next),
                    at_ms: now_ms,
                };
                settings.keep(&key, &next, until);
                done.push(m);
            }
            Ok(None) => {}
            Err(why) => failed.push((id, why)),
        }
    }
    if !done.is_empty() {
        let mut log: Vec<Migrated> = settings
            .get(MIGRATION_KEY)
            .and_then(|(t, _)| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        log.extend(done.iter().cloned());
        if let Ok(text) = serde_json::to_string(&log) {
            settings.keep(MIGRATION_KEY, &text, now_ms.saturating_add(super::KEPT_MS));
        }
    }
    Migration { done, failed }
}

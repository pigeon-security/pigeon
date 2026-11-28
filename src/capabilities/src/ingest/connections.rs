//! Named, instance-approved bucket scopes and their independently rotated read credentials.

use pigeon_cloud::{digitalocean_spaces, transport::FetchTransport};
use pigeon_core::{crypto, oauth, s3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Method, Result};

use crate::{
    Context,
    console::write::Write,
    instances::{self, record::Instance},
    users::directory,
};

pub const LIST: &str = "data_ingest_connections";
pub const VERIFY: &str = "data_ingest_connection_verify";
pub const SET_KEY: &str = "data-ingest-connection-key-set";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum Binding {
    DigitalOcean(digitalocean_spaces::Binding),
    Backblaze(pigeon_ingest::backblaze::Source),
    Scaleway(super::scaleway_source::Source),
}

impl Binding {
    pub fn provider(&self) -> &'static str {
        match self {
            Self::DigitalOcean(_) => "digitalocean",
            Self::Backblaze(_) => "backblaze",
            Self::Scaleway(_) => "scaleway",
        }
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        match self {
            Self::DigitalOcean(binding) => binding.validate().map_err(str::to_owned),
            Self::Backblaze(binding) => binding.validate().map_err(str::to_owned),
            Self::Scaleway(binding) => binding.validate(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub name: String,
    pub scope: String,
    pub generation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Saved {
    pub scope: String,
    pub sealed: String,
}

pub fn valid_name(name: &str) -> bool {
    (1..=48).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn valid_generation(generation: &str) -> bool {
    generation.len() == 43
        && generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub fn binding<'a>(record: &'a Instance, name: &str) -> Result<&'a Binding> {
    let found = record
        .capabilities
        .get(super::NAME)
        .filter(|_| super::data_enabled(record))
        .and_then(|settings| settings.connections.get(name))
        .ok_or_else(|| {
            super::bad("This instance has no approved bucket connection with that name.")
        })?;
    found.validate().map_err(super::bad)?;
    Ok(found)
}

pub fn field_problems(record: &Instance, capability: &str) -> Vec<String> {
    let connections = &record.capabilities[capability].connections;
    if connections.is_empty() {
        return Vec::new();
    }
    if capability != super::NAME || connections.len() > 32 {
        return vec!["Data Ingest supports up to 32 named bucket connections.".into()];
    }
    connections
        .iter()
        .filter_map(|(name, binding)| {
            if !valid_name(name) {
                return Some(format!("Invalid bucket connection name {name}."));
            }
            if let Err(error) = binding.validate() {
                return Some(format!("{name}: {error}"));
            }
            match binding {
                Binding::DigitalOcean(_) if crate::digitalocean::team(record).is_none() => {
                    Some(format!("{name} requires an approved DigitalOcean team."))
                }
                Binding::Backblaze(source)
                    if crate::data_store::settings(record)
                        .and_then(|data| data.backup.as_ref())
                        .is_some_and(|backup| {
                            backup.bucket.eq_ignore_ascii_case(&source.bucket)
                        }) =>
                {
                    Some(format!(
                        "{name} must use a bucket separate from the Data backup."
                    ))
                }
                _ => None,
            }
        })
        .collect()
}

pub(crate) fn key(instance: &str, name: &str) -> String {
    format!("data-ingest-bucket:{instance}:{name}")
}

pub(crate) fn scope(record: &Instance, name: &str) -> Result<String> {
    let binding = binding(record, name)?;
    let team = if matches!(binding, Binding::DigitalOcean(_)) {
        Some(
            crate::digitalocean::team(record)
                .ok_or_else(|| super::bad("Approve a DigitalOcean team first."))?,
        )
    } else {
        None
    };
    Ok(oauth::hash(&serde_json::to_string(&(
        "data-ingest-bucket-v1",
        &record.id,
        &record.account,
        name,
        binding,
        team,
    ))?))
}

pub(crate) fn generation(saved: &Saved) -> String {
    oauth::hash(&saved.sealed)
}

async fn saved(env: &Env, instance: &str, name: &str) -> Result<Option<Saved>> {
    super::setting(env, &key(instance, name))
        .await?
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|_| super::bad("The saved bucket connection is unreadable."))
        })
        .transpose()
}

/// A named Spaces scope may use the existing sealed key only if the previously approved
/// DigitalOcean binding matches it exactly. The approval still pins the old ciphertext generation.
pub async fn open(env: &Env, record: &Instance, name: &str) -> Result<(Pin, s3::Keys)> {
    let binding = binding(record, name)?;
    let scope = scope(record, name)?;
    let (generation, keys) = match saved(env, &record.id, name).await? {
        Some(saved) if saved.scope == scope => {
            let sealing = crate::linked::sealing_key(env).await?;
            let plaintext = crypto::open(&sealing, &saved.sealed, scope.as_bytes())
                .await
                .map_err(|_| super::bad("The saved bucket credential could not be opened."))?;
            let pair = std::str::from_utf8(&plaintext)
                .map_err(|_| super::bad("The saved bucket credential is invalid."))?;
            (generation(&saved), s3::parse_keys(pair)?)
        }
        Some(_) => {
            return Err(super::bad(
                "Replace the saved credential for the newly approved bucket scope.",
            ));
        }
        None => {
            let Binding::DigitalOcean(binding) = binding else {
                return Err(super::bad(
                    "Set a credential for the approved bucket connection.",
                ));
            };
            crate::digitalocean::spaces::legacy_credential(record, env, binding)
                .await?
                .ok_or_else(|| {
                    super::bad("Set a read-only Spaces key for the named bucket connection.")
                })?
        }
    };
    Ok((
        Pin {
            name: name.into(),
            scope,
            generation,
        },
        keys,
    ))
}

pub async fn open_pinned(env: &Env, record: &Instance, pin: &Pin) -> Result<s3::Keys> {
    let (current, keys) = open(env, record, &pin.name).await?;
    if &current != pin {
        return Err(super::bad(
            "The bucket scope or credential changed. Make a new ingest plan.",
        ));
    }
    Ok(keys)
}

async fn probe(env: &Env, record: &Instance, binding: &Binding, keys: &s3::Keys) -> Result<()> {
    let (endpoint, region, bucket, prefix) = match binding {
        Binding::DigitalOcean(binding) => {
            let team = crate::digitalocean::team(record)
                .ok_or_else(|| super::bad("Approve a DigitalOcean team first."))?;
            let pat = crate::digitalocean::open_pat(record, env).await?;
            return digitalocean_spaces::probe(
                &FetchTransport,
                team,
                &pat,
                binding,
                keys,
                super::now_ms(),
            )
            .await
            .map_err(|_| {
                super::bad("DigitalOcean could not verify the bucket-scoped read-only key.")
            });
        }
        Binding::Backblaze(binding) => (
            format!("https://s3.{}.backblazeb2.com", binding.region),
            &binding.region,
            &binding.bucket,
            &binding.prefix,
        ),
        Binding::Scaleway(binding) => (
            format!("https://s3.{}.scw.cloud", binding.region),
            &binding.region,
            &binding.bucket,
            &binding.prefix,
        ),
    };
    let endpoint = s3::Endpoint {
        base: endpoint,
        region: region.clone(),
    };
    let query = s3::list_objects_query(
        Some(prefix.as_str()).filter(|p| !p.is_empty()),
        None,
        1,
        None,
    );
    let path = s3::object_path(bucket, "");
    let date = s3::amz_date(worker::js_sys::Date::now());
    let signed = s3::sign(
        "GET",
        &endpoint,
        &path,
        &query,
        &[],
        s3::EMPTY_SHA256,
        keys,
        &date,
    );
    let headers: Vec<_> = signed
        .iter()
        .filter(|(name, _)| name.as_str() != "host")
        .map(|(name, value)| (name.as_str(), value.clone()))
        .collect();
    let query = query
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                s3::uri_encode(key, true),
                s3::uri_encode(value, true)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{}{path}?{query}", endpoint.base);
    let (status, body) = super::net::http(Method::Get, &url, &headers, None)
        .await
        .map_err(|_| super::bad("The provider could not be reached."))?;
    if status != 200 {
        return Err(super::bad(
            "The provider did not allow a read of the approved bucket prefix.",
        ));
    }
    s3::parse_list_objects(&body)
        .map_err(|_| super::bad("The provider returned an invalid bucket listing."))?;
    Ok(())
}

pub(super) async fn preflight(
    env: &Env,
    record: &Instance,
    name: &str,
    args: &Value,
) -> Result<Value> {
    let binding = binding(record, name)?;
    if args
        .get("source")
        .is_some_and(|source| source.as_str() != Some(binding.provider()))
        || args.get("bucket").is_some()
    {
        return Err(super::bad(
            "The approved connection fixes the provider and bucket.",
        ));
    }
    let approved_prefix = match binding {
        Binding::DigitalOcean(source) => &source.prefix,
        Binding::Scaleway(source) => &source.prefix,
        Binding::Backblaze(source) => &source.prefix,
    };
    let prefix = super::spaces_prefix(approved_prefix, args).map_err(super::bad)?;
    let mut missing = vec![
        "Named bucket runs require the managed Data writer grant before an approval can be issued."
            .into(),
    ];
    let pin = match open(env, record, name).await {
        Ok((pin, keys)) => {
            if probe(env, record, binding, &keys).await.is_err() {
                missing.push("The provider could not verify the saved bucket credential.".into());
            }
            Some(pin)
        }
        Err(error) => {
            missing.push(error.to_string());
            None
        }
    };
    let mut out = super::unready(missing, Value::Null, Value::Null);
    out["source"] = json!({"connection": name, "provider": binding.provider(), "prefix": prefix, "binding": binding});
    out["source_pin"] = json!(pin);
    Ok(out)
}

pub(super) async fn listings(env: &Env, record: &Instance) -> Result<Value> {
    let Some(config) = record
        .capabilities
        .get(super::NAME)
        .filter(|_| super::data_enabled(record))
    else {
        return Err(super::bad("Enable Data Ingest on this instance first."));
    };
    let mut result = Vec::new();
    for (name, binding) in &config.connections {
        let current_scope = scope(record, name)?;
        let stored = saved(env, &record.id, name).await?;
        let state = match &stored {
            Some(saved) if saved.scope == current_scope => "configured",
            Some(_) => "scope_changed",
            None if matches!(binding, Binding::DigitalOcean(b) if crate::digitalocean::spaces::legacy_generation(record, env, b).await?.is_some()) => {
                "legacy_spaces_key"
            }
            None => "missing",
        };
        result.push(json!({
            "name": name, "binding": binding, "state": state, "generation": stored.as_ref().map(generation),
        }));
    }
    Ok(json!({"instance": record.id, "version": record.version, "connections": result}))
}

pub(super) async fn verify(env: &Env, record: &Instance, name: &str) -> Result<Value> {
    let (pin, keys) = open(env, record, name).await?;
    probe(env, record, binding(record, name)?, &keys).await?;
    Ok(
        json!({"instance": record.id, "connection": name, "generation": pin.generation, "verified": true}),
    )
}

async fn checked_target(env: &Env, target: &Value) -> Result<(Instance, String, String)> {
    let instance = super::arg(target, "instance")?;
    let name = super::arg(target, "name")?;
    let record = instances::one(env, instance)
        .await?
        .filter(|record| target["version"].as_u64() == Some(record.version))
        .ok_or_else(|| super::bad("The approved instance changed. Review the connection again."))?;
    let scope = scope(&record, name)?;
    let previous = saved(env, instance, name).await?.as_ref().map(generation);
    if previous.as_deref() != target["generation"].as_str() {
        return Err(super::bad(
            "The bucket credential changed. Refresh the connection and try again.",
        ));
    }
    Ok((record, name.into(), scope))
}

pub(super) async fn check_write(cx: &Context<'_>, write: &Write<'_>) -> Result<Value> {
    let (record, name, _) = checked_target(cx.env, &write.target).await?;
    Ok(json!({"connection": binding(&record, &name)?}))
}

pub(super) async fn save(cx: &Context<'_>, write: &Write<'_>) -> Result<Value> {
    let (record, name, scope) = checked_target(cx.env, &write.target).await?;
    let pair = super::arg(&write.change, "value")?;
    if pair.len() > 1024 || pair.chars().any(char::is_control) {
        return Err(super::bad(
            "Supply the bucket credential as ACCESS_KEY_ID:SECRET_ACCESS_KEY.",
        ));
    }
    let keys = s3::parse_keys(pair)?;
    probe(cx.env, &record, binding(&record, &name)?, &keys).await?;
    let sealing = crate::linked::sealing_key(cx.env).await?;
    let sealed = crypto::seal(&sealing, pair.as_bytes(), scope.as_bytes()).await?;
    let value = serde_json::to_string(&Saved { scope, sealed })?;
    let updated = directory(
        cx.env,
        "data-ingest.bucket.put",
        json!({
            "target": write.target, "value": value,
        }),
    )
    .await?;
    if updated != true {
        return Err(super::bad(
            "The approved scope or credential changed while verifying. Try again.",
        ));
    }
    Ok(json!({"instance": record.id, "connection": name, "verified": true}))
}

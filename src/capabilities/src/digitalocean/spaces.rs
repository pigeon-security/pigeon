//! The instance's optional read-only DigitalOcean Spaces connection.

use pigeon_cloud::digitalocean_spaces::Binding;
use pigeon_core::{crypto, s3};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Method, Result};

use crate::console::write::Write;
use crate::instances::record::Instance;
use crate::{Context, users::directory};

use super::{bad, expected, for_write, open_pat, team};

pub const STATUS: &str = "digitalocean_spaces_status";
pub const VERIFY: &str = "digitalocean_spaces_verify";
pub const SET_KEY: &str = "digitalocean-spaces-key-set";

/// An enabled provider and a valid approved binding are both required.
pub fn binding(record: &Instance) -> Option<&Binding> {
    team(record)?;
    record
        .capabilities
        .get(super::NAME)?
        .spaces
        .as_ref()
        .filter(|s| s.validate().is_ok())
}

fn required(record: &Instance) -> Result<&Binding> {
    binding(record).ok_or_else(|| bad("Spaces is not configured on the approved instance"))
}

pub fn key(instance: &str) -> String {
    format!("digitalocean-spaces-key:{instance}")
}

fn context(record: &Instance, binding: &Binding) -> Result<Vec<u8>> {
    let team = expected(record)?;
    Ok(format!(
        "pigeon:digitalocean-spaces:v1\0{instance}\0{team}\0{region}\0{bucket}\0{prefix}",
        instance = record.id,
        region = binding.region,
        bucket = binding.bucket,
        prefix = binding.prefix,
    )
    .into_bytes())
}

#[derive(Serialize, Deserialize)]
struct Saved {
    team_id: String,
    binding: Binding,
    sealed: String,
}

async fn saved(env: &Env, instance: &str) -> Result<Option<Saved>> {
    let value = directory(env, "setting.get", json!({"key": key(instance)})).await?;
    value
        .as_str()
        .map(|text| {
            serde_json::from_str(text)
                .map_err(|_| bad("the saved Spaces connection could not be read"))
        })
        .transpose()
}

fn current(saved: &Saved, record: &Instance, binding: &Binding) -> bool {
    &saved.binding == binding && team(record) == Some(saved.team_id.as_str())
}

pub(super) async fn status(record: &Instance, env: &Env) -> Result<Value> {
    let binding = required(record)?;
    let state = match saved(env, &record.id).await? {
        None => "missing",
        Some(saved) if current(&saved, record, binding) => "connected",
        Some(_) => "unverified",
    };
    Ok(json!({
        "instance": record.id,
        "region": binding.region,
        "bucket": binding.bucket,
        "prefix": binding.prefix,
        "state": state,
    }))
}

pub(crate) async fn legacy_generation(
    record: &Instance,
    env: &Env,
    binding: &Binding,
) -> Result<Option<String>> {
    if self::binding(record) != Some(binding) {
        return Ok(None);
    }
    Ok(saved(env, &record.id)
        .await?
        .filter(|saved| current(saved, record, binding))
        .map(|saved| pigeon_core::oauth::hash(&saved.sealed)))
}

pub(crate) async fn legacy_credential(
    record: &Instance,
    env: &Env,
    binding: &Binding,
) -> Result<Option<(String, s3::Keys)>> {
    if self::binding(record) != Some(binding) {
        return Ok(None);
    }
    let Some(saved) = saved(env, &record.id)
        .await?
        .filter(|saved| current(saved, record, binding))
    else {
        return Ok(None);
    };
    let key = crate::linked::sealing_key(env).await?;
    let plaintext = crypto::open(&key, &saved.sealed, &context(record, binding)?)
        .await
        .map_err(|_| bad("the saved Spaces key could not be opened"))?;
    let pair =
        std::str::from_utf8(&plaintext).map_err(|_| bad("the saved Spaces key is invalid"))?;
    Ok(Some((
        pigeon_core::oauth::hash(&saved.sealed),
        s3::parse_keys(pair)?,
    )))
}

pub(crate) async fn open_keys(record: &Instance, env: &Env, binding: &Binding) -> Result<s3::Keys> {
    legacy_credential(record, env, binding)
        .await?
        .map(|(_, keys)| keys)
        .ok_or_else(|| bad("set a read-only Spaces key for the approved bucket before verifying"))
}

pub(super) async fn verify(record: &Instance, env: &Env) -> Result<Value> {
    let binding = required(record)?;
    let keys = open_keys(record, env, binding).await?;
    let pat = open_pat(record, env).await?;
    crate::cloud_scope::verify_digitalocean_spaces(record, &pat, &keys).await?;
    Ok(json!({"instance": record.id, "verified": true}))
}

pub(crate) fn allows_prefix(approved: &str, prefix: &str) -> bool {
    prefix.starts_with(approved) && prefix.len() <= 1024 && !prefix.chars().any(char::is_control)
}

/// List one page within the approved binding for a data-ingest preflight.
pub(crate) async fn ingest_sample(
    record: &Instance,
    env: &Env,
    prefix: &str,
) -> Result<s3::Listing> {
    let binding = required(record)?;
    if !allows_prefix(&binding.prefix, prefix) {
        return Err(bad(
            "the requested prefix is outside the approved Spaces binding",
        ));
    }
    let keys = open_keys(record, env, binding).await?;
    let endpoint = s3::Endpoint {
        base: format!("https://{}.digitaloceanspaces.com", binding.region),
        region: binding.region.clone(),
    };
    let query = s3::list_objects_query(Some(prefix).filter(|p| !p.is_empty()), None, 1000, None);
    let mut response = s3::send(
        Method::Get,
        &endpoint,
        &s3::object_path(&binding.bucket, ""),
        &query,
        &[],
        &keys,
    )
    .await?;
    s3::parse_list_objects(&response.text().await?)
}

async fn check(env: &Env, record: &Instance, pair: &str) -> Result<()> {
    let keys = s3::parse_keys(pair)?;
    let pat = open_pat(record, env).await?;
    crate::cloud_scope::verify_digitalocean_spaces(record, &pat, &keys).await
}

pub(super) async fn check_write(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    required(&record)?;
    Ok(Value::Null)
}

pub(super) async fn save(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    let binding = required(&record)?;
    let pair = w.change["value"].as_str().unwrap_or_default();
    if pair.len() > 1024 {
        return Err(bad("the Spaces key is too long"));
    }
    let key = crate::linked::sealing_key(cx.env).await?;
    check(cx.env, &record, pair).await?;
    let sealed = crypto::seal(&key, pair.as_bytes(), &context(&record, binding)?).await?;
    let value = serde_json::to_string(&Saved {
        team_id: expected(&record)?.to_string(),
        binding: binding.clone(),
        sealed,
    })?;
    directory(
        cx.env,
        "digitalocean.spaces.put",
        json!({"target":w.target,"value": value}),
    )
    .await?;
    Ok(json!({"instance": record.id, "verified": true}))
}

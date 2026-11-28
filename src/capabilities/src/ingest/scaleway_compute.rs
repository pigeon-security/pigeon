//! Passkey registration of immutable compute generations in an instance's Secrets Store.
use pigeon_core::secrets_store::Writer;
use pigeon_ingest::scaleway::ComputePin;
use serde_json::{Value, json};
use worker::{Env, Result};

use super::{arg, bad, connections, setting};
use crate::{
    Context,
    console::write::Write,
    instances::{self, record::Instance},
    users::directory,
};

pub const SET: &str = "data-ingest-scaleway-compute-register";
pub(crate) const ACTIVE_TTL_MS: i64 = 100 * 365 * 86_400_000;

pub(crate) const MAX_GENERATIONS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Registry {
    pub account: String,
    pub store: String,
    pub pins: Vec<Registered>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Registered {
    pub pin: ComputePin,
    pub fingerprint: String,
}

impl Registry {
    pub fn active(&self, project: &str) -> Option<&ComputePin> {
        self.pins
            .iter()
            .rev()
            .find(|entry| entry.pin.project == project)
            .map(|entry| &entry.pin)
    }
    pub fn bind(&self, metadata: &mut Value, instance: &str) -> Result<()> {
        let bindings = metadata["bindings"]
            .as_array_mut()
            .ok_or_else(|| bad("missing Data Worker bindings"))?;
        for entry in &self.pins {
            let slot = entry
                .pin
                .binding(instance)
                .ok_or_else(|| bad("invalid stored Scaleway credential pin"))?;
            bindings.push(json!({
                "type": "secrets_store_secret", "name": slot, "store_id": self.store, "secret_name": slot,
            }));
        }
        Ok(())
    }
}

pub(crate) fn registry_key(instance: &str) -> String {
    format!("scaleway-compute:{instance}")
}

pub(crate) async fn registry(env: &Env, instance: &str) -> Result<Option<Registry>> {
    let value = setting(env, &registry_key(instance)).await?;
    Ok(value.map(|raw| serde_json::from_str(&raw)).transpose()?)
}

pub(crate) async fn active(env: &Env, instance: &str, project: &str) -> Result<Option<ComputePin>> {
    Ok(registry(env, instance)
        .await?
        .and_then(|registry| registry.active(project).cloned()))
}

pub fn valid_generation(generation: &str) -> bool {
    generation.len() == 24
        && generation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub(crate) fn project_scope<'r>(record: &'r Instance, name: &str) -> Result<(&'r str, &'r str)> {
    if crate::catalogue::is_home(record) || crate::data_store::inline(record) {
        return Err(bad(
            "Scaleway compute credentials need a linked standalone Data instance",
        ));
    }
    let connections::Binding::Scaleway(scope) = connections::binding(record, name)? else {
        return Err(bad("the approved connection is not a Scaleway source"));
    };
    Ok((&scope.project, &scope.region))
}

async fn checked(env: &Env, target: &Value) -> Result<Instance> {
    let instance = arg(target, "instance")?;
    let name = arg(target, "name")?;
    let record = instances::one(env, instance)
        .await?
        .filter(|r| Some(r.version) == target["version"].as_u64())
        .ok_or_else(|| bad("the approved instance changed"))?;
    let (project, _) = project_scope(&record, name)?;
    let previous = active(env, instance, project).await?;
    if previous.as_ref().map(|pin| pin.generation.as_str()) != target["generation"].as_str() {
        return Err(bad("the Scaleway compute generation changed"));
    }
    Ok(record)
}

pub(crate) fn next_pin(instance: &str, project: &str, previous: Option<&str>) -> ComputePin {
    use sha2::{Digest, Sha256};
    let generation = pigeon_core::sign::hex(&Sha256::digest(
        format!(
            "pigeon-scaleway-compute-v1:{instance}:{project}:{}",
            previous.unwrap_or_default()
        )
        .as_bytes(),
    ))[..24]
        .to_string();
    ComputePin {
        project: project.into(),
        generation,
    }
}

async fn registration_account(cx: &Context<'_>, record: &Instance) -> Result<pigeon_core::Account> {
    let host = cx.env.var("PLATFORM_HOST")?.to_string();
    let mut account =
        crate::linked::account_for_capability(cx.env, &host, &record.id, "secrets").await?;
    if super::linked_id(record).as_deref() != Some(account.id.as_str()) {
        return Err(bad("the linked account differs from the approved instance"));
    }
    account.secrets_store = crate::secrets::linked_store(&account)
        .await?
        .ok_or_else(|| bad("create a Secrets Store in the linked account first"))?;
    if let Some(previous) = registry(cx.env, &record.id).await? {
        if previous.account != account.id || previous.store != account.secrets_store {
            return Err(bad("the approved compute Secrets Store has changed"));
        }
        if previous.pins.len() >= MAX_GENERATIONS {
            return Err(bad("the compute credential history has reached its limit"));
        }
    }
    Ok(account)
}

fn metadata_fingerprint(secret: &pigeon_core::secrets_store::Secret) -> String {
    pigeon_core::oauth::hash(
        &json!({
            "id": secret.id, "modified": secret.modified, "slot": secret.name,
        })
        .to_string(),
    )
}

pub(crate) async fn verify_registry(
    account: &pigeon_core::Account,
    instance: &str,
    registry: &Registry,
) -> Result<()> {
    if account.id != registry.account {
        return Err(bad("the Scaleway compute Secrets Store account changed"));
    }
    let mut account = account.clone();
    account.secrets_store.clone_from(&registry.store);
    let writer = Writer::open(&account).await?;
    let listed = writer.list().await;
    let listed = writer.finish(listed).await?;
    for entry in &registry.pins {
        let slot = entry
            .pin
            .binding(instance)
            .ok_or_else(|| bad("invalid stored Scaleway compute credential"))?;
        if !listed
            .iter()
            .any(|secret| secret.name == slot && metadata_fingerprint(secret) == entry.fingerprint)
        {
            return Err(bad(
                "a registered Scaleway compute slot changed. Keep earlier generations for their run cleanup.",
            ));
        }
    }
    Ok(())
}

async fn fingerprint(account: &pigeon_core::Account, slot: &str) -> Result<String> {
    let writer = Writer::open(account).await?;
    let listed = writer.list().await;
    let secret = writer
        .finish(listed)
        .await?
        .into_iter()
        .find(|secret| secret.name == slot)
        .ok_or_else(|| bad("add the requested credential to the linked Secrets Store first"))?;
    Ok(metadata_fingerprint(&secret))
}

async fn reviewed(
    cx: &Context<'_>,
    record: &Instance,
    target: &Value,
) -> Result<(pigeon_core::Account, Registered)> {
    let (project, _) = project_scope(record, arg(target, "name")?)?;
    let pin = next_pin(&record.id, project, target["generation"].as_str());
    let slot = pin
        .binding(&record.id)
        .ok_or_else(|| bad("invalid Scaleway project credential"))?;
    let account = registration_account(cx, record).await?;
    let fingerprint = fingerprint(&account, &slot).await?;
    if target["fingerprint"].as_str() != Some(&fingerprint) {
        return Err(bad(
            "the linked secret fingerprint changed. Review the credential again",
        ));
    }
    Ok((account, Registered { pin, fingerprint }))
}

pub(super) async fn check_write(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = checked(cx.env, &w.target).await?;
    let (_, registered) = reviewed(cx, &record, &w.target).await?;
    Ok(
        json!({"project": registered.pin.project, "generation": registered.pin.generation, "retains_prior_generations": true}),
    )
}

pub(super) async fn save(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = checked(cx.env, &w.target).await?;
    let (account, registered) = reviewed(cx, &record, &w.target).await?;
    let updated = directory(
        cx.env,
        "scaleway.compute.activate",
        json!({
            "target": w.target, "registered": registered, "account": account.id, "store": account.secrets_store,
        }),
    )
    .await?;
    if updated != true {
        return Err(bad(
            "the approved record or active credential changed; review the registration again",
        ));
    }
    Ok(
        json!({"instance": record.id, "project": registered.pin.project, "generation": registered.pin.generation,
        "registered": true, "provisioning_required": true}),
    )
}

pub(super) async fn preview(
    cx: &Context<'_>,
    record: &Instance,
    name: &str,
    verify: bool,
) -> Result<Value> {
    let (project, _) = project_scope(record, name)?;
    let previous = active(cx.env, &record.id, project).await?;
    let pin = next_pin(
        &record.id,
        project,
        previous.as_ref().map(|pin| pin.generation.as_str()),
    );
    let slot = pin
        .binding(&record.id)
        .ok_or_else(|| bad("invalid Scaleway project credential"))?;
    let credential_fingerprint = if verify {
        let account = registration_account(cx, record).await?;
        Some(fingerprint(&account, &slot).await?)
    } else {
        None
    };
    Ok(
        json!({"generation": previous.map(|pin| pin.generation), "slot": slot, "fingerprint": credential_fingerprint}),
    )
}

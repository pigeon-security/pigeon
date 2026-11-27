//! Instance-approved Scaleway Object Storage and compute identities for Data Ingest.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::data_read::{Account, Mode};
use crate::instances::record::{CapabilitySettings, Instance};

use super::{NAME, bucket_ok};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub project: String,
    pub region: String,
    pub bucket: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
}

impl Source {
    pub fn validate(&self) -> Result<(), String> {
        if !super::net::scw_uuid(&self.project) {
            return Err("Scaleway ingest needs a valid project UUID.".into());
        }
        if !matches!(self.region.as_str(), "fr-par" | "nl-ams" | "pl-waw") {
            return Err(
                "Scaleway ingest needs an approved region (fr-par, nl-ams or pl-waw).".into(),
            );
        }
        if !bucket_ok(&self.bucket) || !valid_prefix(&self.prefix) {
            return Err("Scaleway ingest needs a valid bucket and prefix.".into());
        }
        Ok(())
    }

    pub fn account(&self, instance: &str) -> Account {
        let mut account = Account::scaleway(&format!("data-{}", slug(instance)), &self.region);
        account.mode = Mode::ReadOnly;
        account.buckets = vec![self.bucket.clone()];
        account
    }

    pub fn prefix<'a>(&'a self, args: &'a Value) -> Result<&'a str, String> {
        self.validate()?;
        if args
            .get("bucket")
            .is_some_and(|v| v.as_str() != Some(&self.bucket))
        {
            return Err("The Scaleway source bucket is fixed by its approved binding.".into());
        }
        match args.get("prefix") {
            None => Ok(&self.prefix),
            Some(prefix) => prefix
                .as_str()
                .filter(|prefix| valid_prefix(prefix) && prefix.starts_with(&self.prefix))
                .ok_or_else(|| "The Scaleway prefix must stay within the approved source.".into()),
        }
    }
}

fn valid_prefix(prefix: &str) -> bool {
    prefix.len() <= 1024 && !prefix.chars().any(char::is_control)
}

fn slug(instance: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalized: String = instance
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if normalized == instance && normalized.len() <= 24 {
        return normalized;
    }
    let digest = pigeon_core::sign::hex(&Sha256::digest(instance.as_bytes()));
    digest[..24].to_string()
}

pub fn source_slot(instance: &str) -> String {
    format!(
        "INGEST_SCALEWAY_SOURCE_{}",
        slug(instance).to_ascii_uppercase().replace('-', "_")
    )
}

pub fn compute_slot(instance: &str) -> String {
    format!(
        "INGEST_SCALEWAY_COMPUTE_{}",
        slug(instance).to_ascii_uppercase().replace('-', "_")
    )
}

pub fn binding(record: &Instance) -> Option<&Source> {
    record.capabilities.get(NAME)?.scaleway.as_ref()
}

pub fn field_problems(name: &str, config: &CapabilitySettings) -> Option<String> {
    let source = config.scaleway.as_ref()?;
    if name != NAME {
        Some(format!(
            "The capability {name} does not accept a Scaleway ingest source."
        ))
    } else {
        source.validate().err()
    }
}

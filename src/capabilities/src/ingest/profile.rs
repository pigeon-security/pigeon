//! Approved bindings for an instance's ingest pipeline.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bucket_ok;
use crate::storage::Provider;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled {
        source: String,
        bucket: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        prefix: String,
        destination_bucket: String,
    },
    Disabled,
}

impl Profile {
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    pub fn validate(&self) -> Result<(), String> {
        let Self::Enabled {
            source,
            bucket,
            prefix,
            destination_bucket,
        } = self
        else {
            return Ok(());
        };
        let (provider, name) = source.split_once(':').unwrap_or_default();
        if !Provider::ALL.iter().any(|p| p.slug() == provider)
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("ingest source must name a declared storage account".into());
        }
        if !bucket_ok(bucket) || !bucket_ok(destination_bucket) {
            return Err("ingest binding must name valid source and destination buckets".into());
        }
        if prefix.len() > 1024 || prefix.chars().any(char::is_control) {
            return Err(
                "ingest prefix must be at most 1024 bytes and contain no control characters".into(),
            );
        }
        Ok(())
    }

    /// Bind an existing planning request to the approved source and destination.
    pub fn bind<'a>(&'a self, args: &'a Value) -> Result<Bindings<'a>, String> {
        self.validate()?;
        let Self::Enabled {
            source,
            bucket,
            prefix,
            destination_bucket,
        } = self
        else {
            return Err("ingest is disabled for this instance".into());
        };
        for (key, value) in [("source", source), ("bucket", bucket), ("prefix", prefix)] {
            if args
                .get(key)
                .is_some_and(|arg| arg.as_str() != Some(value.as_str()))
            {
                return Err(format!("ingest {key} is fixed by the approved instance"));
            }
        }
        Ok(Bindings {
            source,
            bucket,
            prefix,
            destination_bucket,
        })
    }
}

pub struct Bindings<'a> {
    pub source: &'a str,
    pub bucket: &'a str,
    pub prefix: &'a str,
    pub destination_bucket: &'a str,
}

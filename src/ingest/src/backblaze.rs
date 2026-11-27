//! An instance's approved read-only B2 ingest source. Destination backup credentials use a
//! different capability and namespace.

use serde::{Deserialize, Serialize};

use crate::manifest::SourceSpec;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Stable source name for provenance, independent of credential rotation.
    pub name: String,
    pub region: String,
    pub bucket: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// A reviewed digest in the instance account's Cloudflare Containers registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::container_job::Runtime>,
}

impl Source {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !label(&self.name, 1, 32) {
            return Err(
                "the Backblaze source name must be a lowercase label of at most 32 characters",
            );
        }
        let region = &self.region;
        if !label(region, 5, 32)
            || !region.rsplit_once('-').is_some_and(|(_, suffix)| {
                suffix.len() == 3 && suffix.bytes().all(|b| b.is_ascii_digit())
            })
            || !region
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
        {
            return Err("the Backblaze region must be an S3 region such as us-west-004");
        }
        // B2 names can contain capitals. The endpoint is always path-style.
        if !(6..=63).contains(&self.bucket.len())
            || !self
                .bucket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || self.bucket.starts_with('-')
            || self.bucket.ends_with('-')
        {
            return Err("the Backblaze source must name a valid B2 bucket");
        }
        if !valid_prefix(&self.prefix) {
            return Err(
                "the Backblaze source prefix contains control characters or exceeds 1024 bytes",
            );
        }
        Ok(())
    }

    /// S3 ListObjects and GET have byte-prefix semantics, including when the approved prefix
    /// lacks a slash. Confinement matches that provider boundary exactly.
    pub fn bind(&self, narrowed_prefix: Option<&str>) -> Result<SourceSpec, &'static str> {
        self.validate()?;
        let prefix = narrowed_prefix.unwrap_or(&self.prefix);
        if !valid_prefix(prefix) || !prefix.starts_with(&self.prefix) {
            return Err("the ingest prefix must stay within the approved Backblaze prefix");
        }
        Ok(SourceSpec::S3 {
            account: format!("backblaze:{}", self.name),
            endpoint: format!("https://s3.{}.backblazeb2.com", self.region),
            region: self.region.clone(),
            bucket: self.bucket.clone(),
            prefix: prefix.to_string(),
        })
    }
}

fn valid_prefix(prefix: &str) -> bool {
    prefix.len() <= 1024 && !prefix.chars().any(char::is_control)
}

fn label(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len())
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

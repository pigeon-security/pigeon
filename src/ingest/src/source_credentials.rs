//! Named source references and the linked Data Worker's credential verification contract.

use serde::{Deserialize, Serialize};

pub const READY_PATH: &str = "/ingest/source/verify";
pub const SOURCES_BINDING: &str = "INGEST_SOURCE_BINDINGS";

/// A generation is an opaque reference, assigned when a new owning-account key slot is staged.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub name: String,
    pub scope: String,
    pub generation: String,
}

pub fn valid_generation(generation: &str) -> bool {
    generation.len() == 43
        && generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The exact settings deployed with the owning-account Worker, established by the instance plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bound {
    pub reference: Reference,
    pub slot: String,
    pub provider: String,
    /// The approved team or project for providers that scope credentials to one.
    pub owner: Option<String>,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
}

impl Bound {
    /// Use the existing bounded provider formats and construct an endpoint without any user URL.
    pub fn endpoint(&self) -> Option<String> {
        let region = &self.region;
        if region.is_empty()
            || region.len() > 32
            || !region
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return None;
        }
        match self.provider.as_str() {
            "digitalocean" => Some(format!("https://{region}.digitaloceanspaces.com")),
            "backblaze" => Some(format!("https://s3.{region}.backblazeb2.com")),
            "scaleway" => Some(format!("https://s3.{region}.scw.cloud")),
            _ => None,
        }
    }
}

/// Use a bounded, stable Workers binding name even when the alias is at its maximum length.
pub fn slot(instance: &str, account: &str, reference: &Reference) -> String {
    let input = format!(
        "ingest-source-v1\0{instance}\0{account}\0{}\0{}\0{}",
        reference.name, reference.scope, reference.generation,
    );
    let hash = super::datastore::sha256_hex(input.as_bytes());
    format!("INGEST_SRC_{}", &hash[..40])
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Verify {
    pub instance: String,
    pub reference: Reference,
    /// Omitted for a permission probe, otherwise the requested prefix inside the approved scope.
    pub prefix: Option<String>,
}

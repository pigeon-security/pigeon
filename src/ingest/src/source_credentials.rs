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

pub const RUN_PATH: &str = "/ingest/source/read";
pub const RUN_ENV: &str = "INGEST_MANAGED_SOURCE";
pub const RUN_MAX_MS: i64 = super::managed_destination::MAX_GRANT_MS;
pub const GET_MAX_SECONDS: u32 = 60;
pub const REQUEST_MAX: usize = 32 * 1024;
pub const RESPONSE_MAX: usize = 4 * 1024 * 1024;

/// An opaque runner credential for the account's exact approved source and run.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerSource {
    pub origin: String,
    pub grant: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunGrant {
    pub instance: String,
    pub run: String,
    pub reference: Reference,
    pub prefix: String,
    pub issued_ms: i64,
    pub expires_ms: i64,
}

impl RunGrant {
    fn valid(&self, now: i64) -> bool {
        !self.instance.is_empty()
            && self.instance.len() <= 160
            && super::container_job::valid_run(&self.run)
            && valid_generation(&self.reference.scope)
            && valid_generation(&self.reference.generation)
            && valid_read_path(&self.prefix)
            && self.issued_ms <= now
            && now < self.expires_ms
            && self.expires_ms.saturating_sub(self.issued_ms) <= RUN_MAX_MS
    }

    /// Keep read authority fixed to both the deployed configuration and the approved run prefix.
    pub fn permits(&self, bound: &Bound, key: &str) -> bool {
        self.reference == bound.reference
            && self.prefix.starts_with(&bound.prefix)
            && key.starts_with(&self.prefix)
            && valid_read_path(key)
    }

    pub fn get_lifetime(&self, now: i64) -> Option<u32> {
        let remaining = self.expires_ms.saturating_sub(now) / 1000;
        (remaining > 0).then(|| remaining.min(i64::from(GET_MAX_SECONDS)) as u32)
    }
}

pub fn valid_read_path(value: &str) -> bool {
    value.len() <= 1024
        && !value.chars().any(char::is_control)
        && !value
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Read {
    List {
        after: Option<String>,
        cursor: Option<String>,
    },
    Get {
        key: String,
    },
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Download {
    pub url: String,
}

fn signed(key: &[u8], body: &str) -> Option<hmac::Hmac<sha2::Sha256>> {
    use hmac::Mac;
    if key.is_empty() {
        return None;
    }
    let mut mac = hmac::Hmac::new_from_slice(key).ok()?;
    mac.update(b"pigeon-data-ingest-source-v1\n");
    mac.update(body.as_bytes());
    Some(mac)
}

pub fn issue(grant: &RunGrant, key: &[u8], now: i64) -> Option<String> {
    use hmac::Mac;
    if !grant.valid(now) {
        return None;
    }
    let body = super::managed_destination::hex(&serde_json::to_vec(grant).ok()?);
    let signature = super::managed_destination::hex(&signed(key, &body)?.finalize().into_bytes());
    Some(format!("{body}.{signature}"))
}

pub fn authenticate(token: &str, key: &[u8], instance: &str, now: i64) -> Option<RunGrant> {
    use hmac::Mac;
    if token.len() > REQUEST_MAX {
        return None;
    }
    let (body, signature) = token.split_once('.')?;
    signed(key, body)?
        .verify_slice(&super::managed_destination::unhex(signature)?)
        .ok()?;
    let grant: RunGrant = serde_json::from_slice(&super::managed_destination::unhex(body)?).ok()?;
    (grant.instance == instance && grant.valid(now)).then_some(grant)
}

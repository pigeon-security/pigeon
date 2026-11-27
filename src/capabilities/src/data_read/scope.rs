//! The portions of a declared storage account an approved instance can read.

use pigeon_core::{s3::Listing, sign, webauthn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled {
        store: String,
        bucket: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        prefix: String,
    },
    Disabled,
}

impl Profile {
    pub fn enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    /// Validate stored bindings before they can be used for credentials or network access.
    pub fn scope(&self) -> Result<Scope<'_>, String> {
        let Self::Enabled {
            store,
            bucket,
            prefix,
        } = self
        else {
            return Err("object storage is disabled for this instance".into());
        };
        let (provider, name) = store.split_once(':').unwrap_or_default();
        if !super::Provider::ALL.iter().any(|p| p.slug() == provider)
            || name.is_empty()
            || name.len() > 32
            || name.starts_with('-')
            || name.ends_with('-')
            || !super::slug_chars(name, &[])
        {
            return Err(
                "storage store must name a declared account such as scaleway:archive".into(),
            );
        }
        if !super::valid_bucket(bucket) {
            return Err("storage bucket must be a plain S3 bucket name".into());
        }
        if !prefix.is_empty()
            && (prefix.len() > 1024 || !prefix.ends_with('/') || !relative_path(prefix, true))
        {
            return Err("storage prefix must be a relative path ending in /".into());
        }
        Ok(Scope {
            store,
            bucket,
            prefix,
        })
    }
}

pub struct Scope<'a> {
    pub store: &'a str,
    pub bucket: &'a str,
    prefix: &'a str,
}

impl Scope<'_> {
    pub fn key(&self, relative: &str) -> Result<String, String> {
        self.join(relative, false)
    }

    pub fn listing_prefix(&self, relative: &str) -> Result<String, String> {
        self.join(relative, true)
    }

    fn join(&self, relative: &str, allow_empty: bool) -> Result<String, String> {
        if !relative_path(relative, allow_empty) {
            return Err("expected a relative object path without dot segments".into());
        }
        if self.prefix.len() + relative.len() > 1024 {
            return Err("the scoped object path exceeds 1024 bytes".into());
        }
        Ok(format!("{}{relative}", self.prefix))
    }

    /// Filter the service's response to the exact requested prefix and return instance-relative names.
    pub fn filter_listing(&self, requested: &str, listing: &mut Listing) {
        let relative = |key: &str| {
            key.starts_with(requested)
                .then(|| key.strip_prefix(self.prefix))
                .flatten()
                .filter(|key| relative_path(key, false))
                .map(str::to_string)
        };
        listing.objects.retain_mut(|object| {
            if let Some(key) = relative(&object.key) {
                object.key = key;
                true
            } else {
                false
            }
        });
        listing.prefixes = listing
            .prefixes
            .iter()
            .filter_map(|prefix| relative(prefix))
            .collect();
    }

    /// Bind a provider continuation to a specific instance, backend and listing.
    pub fn page<'a>(
        &'a self,
        instance: &'a str,
        endpoint: &'a str,
        region: &'a str,
        prefix: &'a str,
        delimiter: Option<&'a str>,
    ) -> PageScope<'a> {
        PageScope {
            instance,
            store: self.store,
            bucket: self.bucket,
            approved_prefix: self.prefix,
            endpoint,
            region,
            prefix,
            delimiter,
        }
    }
}

fn relative_path(path: &str, allow_empty: bool) -> bool {
    (allow_empty || !path.is_empty())
        && !path.starts_with('/')
        && !path.contains("//")
        && !path.chars().any(|c| c == '\\' || c.is_control())
        // The shared S3 path builder escapes percent bytes, so a literal "%2e" stays a key.
        && !path.split('/').any(|segment| matches!(segment, "." | ".."))
}

#[derive(Serialize)]
pub struct PageScope<'a> {
    instance: &'a str,
    store: &'a str,
    bucket: &'a str,
    approved_prefix: &'a str,
    endpoint: &'a str,
    region: &'a str,
    prefix: &'a str,
    delimiter: Option<&'a str>,
}

impl PageScope<'_> {
    fn message(&self, token: &str) -> Vec<u8> {
        serde_json::json!(["pigeon-storage-page-v1", self, token])
            .to_string()
            .into_bytes()
    }

    pub fn encode(&self, secret: &[u8], token: &str) -> String {
        let msg = self.message(token);
        format!(
            "{}.{}",
            webauthn::b64url(token.as_bytes()),
            sign::hex(&sign::hmac_sha256(secret, &msg))
        )
    }

    pub fn decode(&self, secret: &[u8], cursor: &str) -> Result<String, String> {
        let bad = || "invalid continuation token for this instance and listing".to_string();
        if cursor.len() > 16_384 {
            return Err(bad());
        }
        let (data, mac) = cursor.split_once('.').ok_or_else(bad)?;
        let token =
            String::from_utf8(webauthn::unb64url(data).map_err(|_| bad())?).map_err(|_| bad())?;
        if token.is_empty() || !sign::verify_hex(secret, &self.message(&token), mac) {
            return Err(bad());
        }
        Ok(token)
    }
}

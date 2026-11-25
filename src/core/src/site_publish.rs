//! Shared publishing profile and release identifiers.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Profile {
    Enabled { site: String },
    Disabled,
}

/// Resolve an approved site. Legacy records published the first site.
pub fn site(record: &Value) -> Result<&Value, String> {
    let sites = record["sites"].as_array().ok_or("instance has no sites")?;
    if record["publishing"].is_null() {
        return sites.first().ok_or_else(|| "instance has no site".into());
    }
    match serde_json::from_value::<Profile>(record["publishing"].clone())
        .map_err(|_| "invalid publishing profile".to_string())?
    {
        Profile::Disabled => Err("site publishing is disabled for this instance".into()),
        Profile::Enabled { site } => sites
            .iter()
            .find(|entry| entry["name"] == site)
            .ok_or_else(|| "publishing profile names an unknown site".into()),
    }
}

/// The site's canonical public URL. An approved direct hostname takes precedence over a
/// platform subdomain. A linked Worker's preview origin is never used as a canonical URL.
pub fn base_url(site: &Value, platform_zone: &str) -> Option<String> {
    let targets = site["targets"].as_array()?;
    if let Some(host) = targets
        .iter()
        .find(|target| matches!(target["kind"].as_str(), Some("domain" | "zone")))
        .and_then(|target| target["hostname"].as_str())
    {
        return Some(format!("https://{host}"));
    }

    let dns_label = |label: &str| {
        (1..=63).contains(&label.len())
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let label = targets
        .iter()
        .find(|target| target["kind"] == "subdomain")?["label"]
        .as_str()?;
    (dns_label(label)
        && platform_zone.contains('.')
        && platform_zone.split('.').all(dns_label)
        && label.len() + 1 + platform_zone.len() <= 253)
        .then(|| format!("https://{label}.{platform_zone}"))
}

/// Identifies the complete release inputs and manifest. An identical deployment is idempotent.
pub fn release_id(provenance: &Value, manifest: &crate::r2::Manifest) -> String {
    let data = serde_json::json!([provenance, manifest]);
    format!("release-{}", crate::oauth::hash(&data.to_string()))
}

pub fn release_key(id: &str) -> String {
    format!("release-meta/{id}.json")
}

pub fn is_release_id(id: &str) -> bool {
    id.strip_prefix("release-").is_some_and(|digest| {
        digest.len() == 43
            && digest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

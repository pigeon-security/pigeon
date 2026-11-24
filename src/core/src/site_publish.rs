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

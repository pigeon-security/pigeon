//! Redirects (docs/plans/redirects.md): hostnames in an instance's zones that answer with an
//! HTTP redirect, served by the instance's own site Worker. The record holds them
//! (`record::Redirect`); this module turns them into the map the Worker reads, decides a
//! request's answer the way the Worker does, and wraps the site Worker's module with the small
//! script that answers them. Pure.

use serde_json::{Map, Value, json};

use super::record::Instance;

/// The plain-text binding the site Worker reads the redirect map from.
pub const BINDING: &str = "PIGEON_REDIRECTS";
/// The script appended to the site Worker's module when the instance has redirects.
pub const MODULE: &str = include_str!("site_redirect.js");

/// The `cache-control` of a permanent redirect (301, 308): browsers keep it for an hour.
pub const PERMANENT_CACHE: &str = "public, max-age=3600";
/// The `cache-control` of a temporary one (302, 307): never kept.
pub const TEMPORARY_CACHE: &str = "no-store";

/// The redirect map the site Worker reads from [`BINDING`]: each lowercase host to
/// `{to, status, keep_path, cache}`, where `to` has no trailing slash when the path is kept (the
/// request's path, which starts with `/`, is appended to it). `None` when the instance has no
/// redirects. Keys are sorted, so the same record always gives the same text.
pub fn config(instance: &Instance) -> Option<String> {
    if instance.redirects.is_empty() {
        return None;
    }
    let mut map = Map::new();
    for r in &instance.redirects {
        let keep = r.keep_path();
        let to = if keep {
            r.to.trim_end_matches('/').to_string()
        } else {
            r.to.clone()
        };
        let status = r.status();
        let cache = if matches!(status, 301 | 308) {
            PERMANENT_CACHE
        } else {
            TEMPORARY_CACHE
        };
        map.insert(
            r.host.to_ascii_lowercase(),
            json!({ "to": to, "status": status, "keep_path": keep, "cache": cache }),
        );
    }
    // serde_json's Map is ordered by key, so the text is stable.
    Some(Value::Object(map).to_string())
}

/// The answer the site Worker gives a request to `host` for `path` (starting with `/`) and
/// `search` (empty or starting with `?`), given the map [`config`] made: the status, the
/// `Location` and the `cache-control`. `None` when `host` is not a redirect host and the site
/// answers as before. The same decision as `site_redirect.js`.
pub fn answer(config: &str, host: &str, path: &str, search: &str) -> Option<(u16, String, String)> {
    let map: Value = serde_json::from_str(config).ok()?;
    let rule = map.get(host.to_ascii_lowercase())?;
    let to = rule["to"].as_str()?;
    let location = if rule["keep_path"].as_bool() == Some(true) {
        format!("{to}{path}{search}")
    } else {
        to.to_string()
    };
    Some((
        rule["status"].as_u64()? as u16,
        location,
        rule["cache"].as_str()?.to_string(),
    ))
}

/// `module` (the site Worker, public or protected) with the redirect script in front: its
/// export renamed to `served`, which the script calls for every host that is not a redirect.
pub fn wrap(module: &str) -> String {
    format!(
        "{}\n{MODULE}",
        module.replacen("export default {", "const served = {", 1)
    )
}

/// One line per redirect, as the plan shows it: `host → to (status, path kept or not)`.
pub fn lines(instance: &Instance) -> Vec<String> {
    instance
        .redirects
        .iter()
        .map(|r| {
            format!(
                "{} → {} ({}{})",
                r.host,
                r.to,
                r.status(),
                if r.keep_path() {
                    ", path and query kept"
                } else {
                    ""
                }
            )
        })
        .collect()
}

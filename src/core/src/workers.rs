//! Wraps Cloudflare's REST API for small Workers.
//!
//! It uploads a module Worker with its bindings, turns on the workers.dev
//! address, and manages zone routes. It also lists custom domains.
//! The pure functions build the request bodies and decide what to do.
//! The async functions each mint one scoped token, do their work, and revoke it.
//! A route is only ever created where no other Worker is routed, and only
//! ever deleted when it routes the script the caller names.

use crate::Account;
use crate::cloudflare::{call, call_raw};
use crate::tokens::{Policy, Scope, Scoped};
use crate::webauthn::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Method, Result};

/// A binding that a Worker receives at runtime.
#[derive(Clone, PartialEq)]
pub enum Binding {
    /// A write-only secret binding.
    Secret { name: String, text: String },
    /// An R2 bucket, optionally in a jurisdiction such as `eu`.
    R2 {
        /// The name the Worker sees on `env`.
        name: String,
        /// The bucket's name.
        bucket: String,
        /// The bucket's jurisdiction, if it has one.
        jurisdiction: Option<String>,
    },
    /// A plain text variable.
    Text {
        /// The name the Worker sees on `env`.
        name: String,
        /// The text value.
        text: String,
    },
}

/// A Workers route on a zone.
#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Route {
    /// The route's id.
    pub id: String,
    /// The URL pattern the route matches.
    pub pattern: String,
    /// The script the route sends requests to, if any.
    pub script: Option<String>,
}

/// What to do to make a route exist.
#[derive(Debug, PartialEq)]
pub enum RoutePlan {
    /// No route has this pattern. Create one.
    Create,
    /// The same route already exists. Holds its id.
    Present(String),
    /// The pattern is taken. Holds the other script's name, or an empty string if it has none.
    Conflict(String),
}

/// What to do to remove a route.
#[derive(Debug, PartialEq)]
pub enum DeletePlan {
    /// The route is already gone.
    Gone,
    /// The route belongs to the expected script. Delete it.
    Delete,
    /// The route belongs to someone else. Holds the other script's name, or an empty string if it has none.
    Foreign(String),
}

/// Tells whether a string is safe to use as one path segment.
pub fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Tells whether a string is safe to use as a host name in a query.
pub fn valid_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Builds the JSON for one binding.
fn binding_json(binding: &Binding) -> Value {
    match binding {
        Binding::R2 {
            name,
            bucket,
            jurisdiction,
        } => {
            let mut v = json!({"type": "r2_bucket", "name": name, "bucket_name": bucket});
            if let (Some(j), Some(map)) = (jurisdiction, v.as_object_mut()) {
                map.insert("jurisdiction".to_string(), Value::String(j.clone()));
            }
            v
        }
        Binding::Secret { name, text } => {
            json!({"type": "secret_text", "name": name, "text": text})
        }
        Binding::Text { name, text } => json!({"type": "plain_text", "name": name, "text": text}),
    }
}

/// Builds the upload metadata for a module Worker.
pub fn metadata(compatibility_date: &str, bindings: &[Binding]) -> Value {
    json!({
        "main_module": "worker.js",
        "compatibility_date": compatibility_date,
        "bindings": bindings.iter().map(binding_json).collect::<Vec<_>>(),
    })
}

/// Builds the multipart body: a `metadata` part, then the `worker.js` module part.
pub fn multipart(boundary: &str, metadata: &Value, module: &str) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str("Content-Disposition: form-data; name=\"metadata\"\r\n");
    out.push_str("Content-Type: application/json\r\n\r\n");
    out.push_str(&metadata.to_string());
    out.push_str("\r\n");
    out.push_str(&format!("--{boundary}\r\n"));
    out.push_str("Content-Disposition: form-data; name=\"worker.js\"; filename=\"worker.js\"\r\n");
    out.push_str("Content-Type: application/javascript+module\r\n\r\n");
    out.push_str(module);
    out.push_str("\r\n");
    out.push_str(&format!("--{boundary}--\r\n"));
    out.into_bytes()
}

/// Builds the Content-Type header value for a multipart body.
pub fn content_type(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

/// Turns random text into a boundary by dropping everything but letters and digits.
pub fn boundary_from(random_text: &str) -> String {
    random_text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Finds the route with exactly this pattern.
pub fn route_for<'a>(routes: &'a [Route], pattern: &str) -> Option<&'a Route> {
    routes.iter().find(|r| r.pattern == pattern)
}

/// Decides how to make `pattern` route to `script` without touching anyone else's route.
pub fn plan_route(routes: &[Route], pattern: &str, script: &str) -> RoutePlan {
    if let Some(same) = routes
        .iter()
        .find(|r| r.pattern == pattern && r.script.as_deref() == Some(script))
    {
        return RoutePlan::Present(same.id.clone());
    }
    match route_for(routes, pattern) {
        Some(other) => RoutePlan::Conflict(other.script.clone().unwrap_or_default()),
        None => RoutePlan::Create,
    }
}

/// Decides whether the route with this id may be deleted for `expected_script`.
pub fn plan_delete(routes: &[Route], id: &str, expected_script: &str) -> DeletePlan {
    match routes.iter().find(|r| r.id == id) {
        None => DeletePlan::Gone,
        Some(r) if r.script.as_deref() == Some(expected_script) => DeletePlan::Delete,
        Some(r) => DeletePlan::Foreign(r.script.clone().unwrap_or_default()),
    }
}

/// Builds the workers.dev address of a script.
pub fn workers_dev_url(script: &str, account_subdomain: &str) -> String {
    format!("https://{script}.{account_subdomain}.workers.dev")
}

/// Tells whether an API error text means "not found".
pub fn is_not_found(message: &str) -> bool {
    message.contains(": 404:")
}

/// Reads the `id` out of an API answer.
pub fn id_of(value: &Value) -> Result<String> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::RustError("the answer had no id".to_string()))
}

/// Reads the account's workers.dev subdomain out of an API answer.
pub fn subdomain_of(value: &Value) -> Result<String> {
    value
        .get("subdomain")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::RustError("the account has no workers.dev subdomain".to_string()))
}

/// Words the error for a route that belongs to someone else.
pub fn conflict_message(pattern: &str, other: &str) -> String {
    if other.is_empty() {
        format!("the route {pattern} already exists and is not bound to a worker")
    } else {
        format!("the route {pattern} already sends to the worker {other}")
    }
}

/// Fails with a plain message unless the check passed.
fn require(ok: bool, what: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::RustError(format!("{what} is not valid")))
    }
}

/// The policy for account-level Workers work.
fn account_policy() -> Vec<Policy> {
    vec![Policy::new(Scope::Account, &["Workers Scripts Write"])]
}

/// The policy for zone route work.
fn zone_policy() -> Vec<Policy> {
    vec![Policy::new(
        Scope::AllZones,
        &["Workers Routes Write", "Zone Read"],
    )]
}

/// Reads a zone's routes with an existing token.
async fn list_routes(token: &str, zone_id: &str) -> Result<Vec<Route>> {
    let path = format!("/zones/{zone_id}/workers/routes");
    let result = call(token, Method::Get, &path, None).await?;
    serde_json::from_value::<Vec<Route>>(result)
        .map_err(|_| Error::RustError("the routes answer was not understood".to_string()))
}

/// Uploads a module Worker with its bindings. Answers the API's description of it.
pub async fn upload(
    account: &Account,
    script: &str,
    compatibility_date: &str,
    bindings: &[Binding],
    module: &str,
) -> Result<Value> {
    require(valid_segment(script), "the script name")?;
    let boundary = boundary_from(&random()?);
    require(!boundary.is_empty(), "the upload boundary")?;
    let body = multipart(&boundary, &metadata(compatibility_date, bindings), module);
    let ctype = content_type(&boundary);
    let path = format!("/accounts/{}/workers/scripts/{script}", account.id);
    let token = Scoped::open(account, "upload worker", account_policy()).await?;
    let r = call_raw(token.value(), Method::Put, &path, Some((&ctype, body))).await;
    let result = token.finish(r).await;
    if bindings
        .iter()
        .any(|binding| matches!(binding, Binding::Secret { .. }))
    {
        return result.map_err(|_| Error::RustError("protected origin upload failed".into()));
    }
    result
}

/// Tells whether a script exists in the account.
pub async fn exists(account: &Account, script: &str) -> Result<bool> {
    require(valid_segment(script), "the script name")?;
    let path = format!("/accounts/{}/workers/scripts/{script}/settings", account.id);
    let token = Scoped::open(account, "check worker", account_policy()).await?;
    let r = match call(token.value(), Method::Get, &path, None).await {
        Ok(_) => Ok(true),
        Err(e) if is_not_found(&e.to_string()) => Ok(false),
        Err(e) => Err(e),
    };
    token.finish(r).await
}

/// Turns on the workers.dev address of a script. Answers its URL.
pub async fn enable_workers_dev(account: &Account, script: &str) -> Result<String> {
    require(valid_segment(script), "the script name")?;
    let token = Scoped::open(account, "enable workers.dev", account_policy()).await?;
    let r = enable_with(token.value(), &account.id, script).await;
    token.finish(r).await
}

/// Enables workers.dev, then reads the account subdomain, using an existing token.
async fn enable_with(token: &str, account_id: &str, script: &str) -> Result<String> {
    let path = format!("/accounts/{account_id}/workers/scripts/{script}/subdomain");
    let body = json!({"enabled": true, "previews_enabled": false});
    call(token, Method::Post, &path, Some(&body)).await?;
    let sub_path = format!("/accounts/{account_id}/workers/subdomain");
    let answer = call(token, Method::Get, &sub_path, None).await?;
    let sub = subdomain_of(&answer)?;
    Ok(workers_dev_url(script, &sub))
}

/// Lists the Workers routes of a zone.
pub async fn routes(account: &Account, zone_id: &str) -> Result<Vec<Route>> {
    require(valid_segment(zone_id), "the zone id")?;
    let token = Scoped::open(account, "list routes", zone_policy()).await?;
    let r = list_routes(token.value(), zone_id).await;
    token.finish(r).await
}

/// Adds a route to a script. Answers the route id.
///
/// It refuses when the pattern already routes elsewhere.
/// It does nothing when the same route exists.
pub async fn add_route(
    account: &Account,
    zone_id: &str,
    pattern: &str,
    script: &str,
) -> Result<String> {
    require(valid_segment(zone_id), "the zone id")?;
    require(valid_segment(script), "the script name")?;
    require(!pattern.is_empty(), "the route pattern")?;
    let token = Scoped::open(account, "add route", zone_policy()).await?;
    let r = add_route_with(token.value(), zone_id, pattern, script).await;
    token.finish(r).await
}

/// Plans and creates a route using an existing token.
async fn add_route_with(token: &str, zone_id: &str, pattern: &str, script: &str) -> Result<String> {
    let existing = list_routes(token, zone_id).await?;
    match plan_route(&existing, pattern, script) {
        RoutePlan::Present(id) => Ok(id),
        RoutePlan::Conflict(other) => Err(Error::RustError(conflict_message(pattern, &other))),
        RoutePlan::Create => {
            let body = json!({"pattern": pattern, "script": script});
            let path = format!("/zones/{zone_id}/workers/routes");
            let made = call(token, Method::Post, &path, Some(&body)).await?;
            id_of(&made)
        }
    }
}

/// Deletes a route, but only if it sends to `expected_script`.
///
/// Answers false when the route is already gone.
pub async fn delete_route(
    account: &Account,
    zone_id: &str,
    id: &str,
    expected_script: &str,
) -> Result<bool> {
    require(valid_segment(zone_id), "the zone id")?;
    require(valid_segment(id), "the route id")?;
    let token = Scoped::open(account, "delete route", zone_policy()).await?;
    let r = delete_route_with(token.value(), zone_id, id, expected_script).await;
    token.finish(r).await
}

/// Checks and deletes a route using an existing token.
async fn delete_route_with(
    token: &str,
    zone_id: &str,
    id: &str,
    expected_script: &str,
) -> Result<bool> {
    let existing = list_routes(token, zone_id).await?;
    match plan_delete(&existing, id, expected_script) {
        DeletePlan::Gone => Ok(false),
        DeletePlan::Foreign(other) => Err(Error::RustError(if other.is_empty() {
            format!("the route {id} is not bound to the worker {expected_script}")
        } else {
            format!("the route {id} belongs to the worker {other}, not {expected_script}")
        })),
        DeletePlan::Delete => {
            let path = format!("/zones/{zone_id}/workers/routes/{id}");
            call(token, Method::Delete, &path, None).await?;
            Ok(true)
        }
    }
}

/// Lists the custom domains that match a host name.
pub async fn custom_domains(account: &Account, hostname: &str) -> Result<Vec<Value>> {
    require(valid_hostname(hostname), "the host name")?;
    let path = format!(
        "/accounts/{}/workers/domains?hostname={hostname}",
        account.id
    );
    let token = Scoped::open(account, "list custom domains", account_policy()).await?;
    let r = call(token.value(), Method::Get, &path, None)
        .await
        .map(|v| match v {
            Value::Array(items) => items,
            _ => Vec::new(),
        });
    token.finish(r).await
}

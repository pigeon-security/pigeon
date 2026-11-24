//! Scaleway IAM requests and answers, as pure functions (checked against Scaleway's generated
//! Go SDK, `api/iam/v1alpha1/iam_sdk.go`). Auth is the header `X-Auth-Token: <secret key>`.
//! The one place a secret key comes back is the answer to creating an API key, and it is held
//! in a [`SecretKey`], whose `Debug` hides it. Every other parser here has no field for one.

use std::fmt;

use pigeon_core::s3::uri_encode;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::transport::HttpRequest;

/// The IAM API's base URL.
pub const API: &str = "https://api.scaleway.com/iam/v1alpha1";

/// A secret that never shows in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(String);

impl SecretKey {
    /// Holds `value`.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<hidden>)")
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// A request to the IAM API: `path` under [`API`], the query, and an optional JSON body.
pub fn request(
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    body: Option<&Value>,
    token: &SecretKey,
) -> HttpRequest {
    let mut url = format!("{API}{path}");
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", uri_encode(k, true), uri_encode(v, true)))
            .collect();
        url.push('?');
        url.push_str(&pairs.join("&"));
    }
    let mut headers = vec![
        ("X-Auth-Token".to_string(), token.expose().to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    if body.is_some() {
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
    }
    HttpRequest {
        method: method.into(),
        url,
        headers,
        body: body.map(Value::to_string).unwrap_or_default(),
    }
}

/// An API key as a read shows it. It has no secret field.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKey {
    /// The access key, `SCW` and 17 more.
    pub access_key: String,
    /// The application that bears it, when one does.
    pub application_id: Option<String>,
    /// Its description.
    pub description: String,
    /// When it was made.
    pub created_at: Option<String>,
    /// When it expires; none for a key that does not.
    pub expires_at: Option<String>,
    /// Its default project, for Object Storage.
    pub default_project_id: String,
    /// Whether it can be edited.
    pub editable: bool,
    /// Whether it can be deleted.
    pub deletable: bool,
    /// Whether Scaleway manages it.
    pub managed: bool,
}

impl ApiKey {
    /// The key as the tools show it.
    pub fn to_json(&self) -> Value {
        json!({
            "access_key": self.access_key,
            "application_id": self.application_id,
            "description": self.description,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "default_project_id": self.default_project_id,
            "editable": self.editable,
            "deletable": self.deletable,
            "managed": self.managed,
        })
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().filter(|s| !s.is_empty()).map(String::from)
}

/// Reads one API key. Any `secret_key` in `v` is ignored.
pub fn parse_key(v: &Value) -> Result<ApiKey> {
    Ok(ApiKey {
        access_key: text(v, "access_key").ok_or_else(|| bad("the API key has no access_key"))?,
        application_id: text(v, "application_id"),
        description: text(v, "description").unwrap_or_default(),
        created_at: text(v, "created_at"),
        expires_at: text(v, "expires_at"),
        default_project_id: text(v, "default_project_id").unwrap_or_default(),
        editable: v["editable"].as_bool().unwrap_or(false),
        deletable: v["deletable"].as_bool().unwrap_or(false),
        managed: v["managed"].as_bool().unwrap_or(false),
    })
}

/// Reads a page of API keys and their total count.
pub fn parse_keys(v: &Value) -> Result<(Vec<ApiKey>, u64)> {
    let keys = v["api_keys"]
        .as_array()
        .ok_or_else(|| bad("the answer is not an API key list"))?
        .iter()
        .map(parse_key)
        .collect::<Result<Vec<_>>>()?;
    let total = v["total_count"].as_u64().unwrap_or(keys.len() as u64);
    Ok((keys, total))
}

/// A new API key: what was made, and the secret key shown this once.
pub struct NewKey {
    /// The key as a read would show it.
    pub key: ApiKey,
    /// Its secret key.
    pub secret: SecretKey,
}

impl fmt::Debug for NewKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewKey")
            .field("key", &self.key)
            .field("secret", &self.secret)
            .finish()
    }
}

/// Reads the answer to creating an API key; it must carry the secret key.
pub fn parse_new_key(v: &Value) -> Result<NewKey> {
    let secret = text(v, "secret_key").ok_or_else(|| bad("the new API key has no secret key"))?;
    Ok(NewKey {
        key: parse_key(v)?,
        secret: SecretKey::new(secret),
    })
}

/// An IAM application.
#[derive(Debug, Clone, PartialEq)]
pub struct Application {
    /// Its ID.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its tags.
    pub tags: Vec<String>,
}

impl Application {
    /// The application as the tools show it.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name, "tags": self.tags })
    }
}

/// Reads one application.
pub fn parse_application(v: &Value) -> Result<Application> {
    Ok(Application {
        id: text(v, "id").ok_or_else(|| bad("the application has no id"))?,
        name: text(v, "name").unwrap_or_default(),
        tags: v["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Reads a page of applications.
pub fn parse_applications(v: &Value) -> Result<Vec<Application>> {
    v["applications"]
        .as_array()
        .ok_or_else(|| bad("the answer is not an application list"))?
        .iter()
        .map(parse_application)
        .collect()
}

/// An IAM policy, as far as the tools show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Its ID.
    pub id: String,
    /// Its name.
    pub name: String,
    /// The application it is attributed to, when it is.
    pub application_id: Option<String>,
}

impl Policy {
    /// The policy as the tools show it.
    pub fn to_json(&self) -> Value {
        json!({ "id": self.id, "name": self.name })
    }
}

/// Reads a page of policies.
pub fn parse_policies(v: &Value) -> Result<Vec<Policy>> {
    v["policies"]
        .as_array()
        .ok_or_else(|| bad("the answer is not a policy list"))?
        .iter()
        .map(|p| {
            Ok(Policy {
                id: text(p, "id").ok_or_else(|| bad("the policy has no id"))?,
                name: text(p, "name").unwrap_or_default(),
                application_id: text(p, "application_id"),
            })
        })
        .collect()
}

/// The ID of what a create answered.
pub fn parse_id(v: &Value) -> Result<String> {
    text(v, "id").ok_or_else(|| bad("the answer has no id"))
}

/// Scaleway's message from an error body, or the start of the body.
pub fn error_message(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| text(&v, "message"))
        .unwrap_or_else(|| body.trim().to_string());
    message.chars().take(200).collect()
}

/// Removes every `secret_key` field, at any depth. Applied to each answer a tool gives.
pub fn strip_secrets(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("secret_key");
            m.values_mut().for_each(strip_secrets);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_secrets),
        _ => {}
    }
}

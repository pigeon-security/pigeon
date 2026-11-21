//! Workers scripts: uploads a generated module Worker, such as a site rendered by another
//! Worker. The upload keeps the script's routes and custom domains as they are.

use serde_json::{Value, json};
use worker::{Method, Result};

use crate::Account;
use crate::cloudflare::{call, call_raw};
use crate::tokens::{Policy, Scope, Scoped};

/// What an upload's token holds.
pub const PERMISSIONS: [&str; 1] = ["Workers Scripts Write"];

/// A module Worker of one file and no bindings.
pub struct Script<'a> {
    pub name: &'a str,
    /// The module's JavaScript; its default export handles `fetch`.
    pub module: &'a str,
    pub compatibility_date: &'a str,
}

/// Uploads `script`, creating or replacing it; answers what the API says of it.
pub async fn upload(account: &Account, script: &Script<'_>) -> Result<Value> {
    let policies = vec![Policy::new(Scope::Account, &PERMISSIONS)];
    let token = Scoped::open(account, "workers", policies).await?;
    let boundary = "pigeon-worker-upload";
    let metadata = json!({
        "main_module": "worker.js",
        "compatibility_date": script.compatibility_date,
        "bindings": [],
    });
    let mut body = String::new();
    for (name, filename, content_type, content) in [
        ("metadata", "", "application/json", metadata.to_string()),
        (
            "worker.js",
            "worker.js",
            "application/javascript+module",
            script.module.to_string(),
        ),
    ] {
        let file = if filename.is_empty() {
            String::new()
        } else {
            format!("; filename=\"{filename}\"")
        };
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{file}\r\nContent-Type: {content_type}\r\n\r\n{content}\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    let path = format!("/accounts/{}/workers/scripts/{}", account.id, script.name);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let uploaded = call_raw(
        token.value(),
        Method::Put,
        &path,
        Some((&content_type, body.into_bytes())),
    )
    .await;
    token.finish(uploaded).await
}

/// Deletes Worker `name`, with its routes and custom domains; answers false when it did not
/// exist.
pub async fn delete(account: &Account, name: &str) -> Result<bool> {
    let policies = vec![Policy::new(Scope::Account, &PERMISSIONS)];
    let token = Scoped::open(account, "workers", policies).await?;
    let path = format!("/accounts/{}/workers/scripts/{name}?force=true", account.id);
    let deleted = match call(token.value(), Method::Delete, &path, None).await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains(": 404:") => Ok(false),
        Err(e) => Err(e),
    };
    token.finish(deleted).await
}

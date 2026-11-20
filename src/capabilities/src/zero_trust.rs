//! Cloudflare Access: self-hosted applications, reusable policies and service tokens. Each
//! operation finds its object by name or domain first, so running it again changes nothing.

use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::cloudflare::call;
use crate::tokens::{Policy, Scope};

/// What a token for this capability holds.
pub fn policies() -> Vec<Policy> {
    vec![Policy::new(
        Scope::Account,
        &[
            "Access: Apps and Policies Write",
            "Access: Service Tokens Write",
        ],
    )]
}

/// How long a service token lives.
const SERVICE_TOKEN_DURATION: &str = "8760h";

/// How long an Access session lasts.
const SESSION_DURATION: &str = "24h";

/// A service token. `client_secret` is known only when it was just created or rotated.
pub struct ServiceToken {
    pub id: String,
    pub client_id: String,
    pub client_secret: Option<String>,
}

#[derive(Deserialize)]
struct Listed {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    domain: String,
}

async fn list(token: &str, account: &str, what: &str) -> Result<Vec<Listed>> {
    let v = call(
        token,
        Method::Get,
        &format!("/accounts/{account}/access/{what}"),
        None,
    )
    .await?;
    Ok(serde_json::from_value(v)?)
}

fn id(v: &Value, what: &str) -> Result<String> {
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("the {what} came back without its id")))
}

/// Service token `name`: the existing one, without its secret, or a new one, with it.
pub async fn service_token(token: &str, account: &str, name: &str) -> Result<ServiceToken> {
    if let Some(t) = list(token, account, "service_tokens")
        .await?
        .into_iter()
        .find(|t| t.name == name)
    {
        return Ok(ServiceToken {
            id: t.id,
            client_id: t.client_id,
            client_secret: None,
        });
    }
    let body = json!({ "name": name, "duration": SERVICE_TOKEN_DURATION });
    let v = call(
        token,
        Method::Post,
        &format!("/accounts/{account}/access/service_tokens"),
        Some(&body),
    )
    .await?;
    Ok(ServiceToken {
        id: id(&v, "service token")?,
        client_id: v["client_id"].as_str().unwrap_or_default().to_string(),
        client_secret: v["client_secret"].as_str().map(String::from),
    })
}

/// Rotates service token `id`: its new client secret.
pub async fn rotate_service_token(token: &str, account: &str, id: &str) -> Result<String> {
    let v = call(
        token,
        Method::Post,
        &format!("/accounts/{account}/access/service_tokens/{id}/rotate"),
        None,
    )
    .await?;
    v["client_secret"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| {
            Error::RustError("the rotated service token came back without its secret".into())
        })
}

/// Reusable policy `name`, letting in service token `service_token_id`: its ID.
pub async fn service_token_policy(
    token: &str,
    account: &str,
    name: &str,
    service_token_id: &str,
) -> Result<String> {
    let body = json!({
        "name": name,
        "decision": "non_identity",
        "include": [{ "service_token": { "token_id": service_token_id } }],
    });
    let path = format!("/accounts/{account}/access/policies");
    match list(token, account, "policies")
        .await?
        .into_iter()
        .find(|p| p.name == name)
    {
        Some(p) => {
            call(token, Method::Put, &format!("{path}/{}", p.id), Some(&body)).await?;
            Ok(p.id)
        }
        None => id(
            &call(token, Method::Post, &path, Some(&body)).await?,
            "policy",
        ),
    }
}

/// Self-hosted application `name` on `domain`, held by reusable policy `policy_id`: its ID.
pub async fn self_hosted_app(
    token: &str,
    account: &str,
    name: &str,
    domain: &str,
    policy_id: &str,
) -> Result<String> {
    let body = json!({
        "name": name,
        "domain": domain,
        "type": "self_hosted",
        "session_duration": SESSION_DURATION,
        "policies": [{ "id": policy_id, "precedence": 1 }],
    });
    let path = format!("/accounts/{account}/access/apps");
    match list(token, account, "apps")
        .await?
        .into_iter()
        .find(|a| a.domain == domain)
    {
        Some(a) => {
            call(token, Method::Put, &format!("{path}/{}", a.id), Some(&body)).await?;
            Ok(a.id)
        }
        None => id(
            &call(token, Method::Post, &path, Some(&body)).await?,
            "application",
        ),
    }
}

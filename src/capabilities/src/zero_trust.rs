//! Cloudflare Access: self-hosted applications, reusable policies and service tokens. Each
//! operation finds its object by name or domain first, so running it again changes nothing.

use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use serde::Serialize;

use crate::Account;
use crate::cloudflare::call;
use crate::secrets_store::Writer;
use crate::tokens::{Policy, Scope, Scoped};

/// What a token for this capability holds.
fn policies() -> Vec<Policy> {
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
struct ServiceToken {
    id: String,
    client_id: String,
    client_secret: Option<String>,
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
async fn service_token(token: &str, account: &str, name: &str) -> Result<ServiceToken> {
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
async fn rotate_service_token(token: &str, account: &str, id: &str) -> Result<String> {
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
async fn service_token_policy(
    token: &str,
    account: &str,
    name: &str,
    service_token_ids: &[String],
) -> Result<String> {
    let include: Vec<Value> = service_token_ids
        .iter()
        .map(|id| json!({ "service_token": { "token_id": id } }))
        .collect();
    let body = json!({
        "name": name,
        "decision": "non_identity",
        "include": include,
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
async fn self_hosted_app(
    token: &str,
    account: &str,
    name: &str,
    domain: &str,
    policy_id: &str,
    token_header: Option<&str>,
) -> Result<String> {
    let mut body = json!({
        "name": name,
        "domain": domain,
        "type": "self_hosted",
        "session_duration": SESSION_DURATION,
        "policies": [{ "id": policy_id, "precedence": 1 }],
    });
    if let Some(header) = token_header {
        // The client ID and secret then also come as one JSON header:
        // {"cf-access-client-id": ..., "cf-access-client-secret": ...}.
        body["read_service_tokens_from_header"] = json!(header);
    }
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

/// What protects a hostname: the Access application's name, its service token's name, and the
/// Secrets Store names that keep the token's client ID and secret.
pub struct Protection {
    pub app: String,
    pub service_token: String,
    pub client_id_secret: String,
    pub client_secret_secret: String,
    /// More service tokens the application lets in, by name, for callers outside the account
    /// such as Claude sessions. Their secrets are not stored: each is answered once, when its
    /// token is created.
    pub guests: Vec<String>,
    /// A header the application also reads a service token from, as one JSON value, for
    /// callers that can send only one header.
    pub token_header: Option<String>,
}

/// What [`protect`] made.
#[derive(Serialize)]
pub struct Protected {
    pub access_app: String,
    pub client_id: String,
    /// The Secrets Store names holding the client ID and secret.
    pub secrets: [String; 2],
    pub guests: Vec<Guest>,
}

/// A guest service token. `client_secret` is present only in the answer that created it.
#[derive(Serialize)]
pub struct Guest {
    pub name: String,
    pub client_id: String,
    pub client_secret: Option<String>,
}

/// Puts `domain` behind Access, letting in only `protection`'s service token; idempotent:
/// 1. the service token exists, with its client ID and secret in the Secrets Store;
/// 2. a reusable policy lets that token in;
/// 3. a self-hosted application on `domain` holds that policy.
pub async fn protect(
    account: &Account,
    domain: &str,
    protection: &Protection,
) -> Result<Protected> {
    let token = Scoped::open(account, "zero-trust", policies()).await?;
    let made = protect_with(account, token.value(), domain, protection).await;
    token.finish(made).await
}

async fn protect_with(
    account: &Account,
    token: &str,
    domain: &str,
    protection: &Protection,
) -> Result<Protected> {
    let service = service_token(token, &account.id, &protection.service_token).await?;
    let writer = Writer::open(account).await?;
    let stored = store(&writer, token, &account.id, &service, protection).await;
    writer.finish(stored).await?;
    let mut ids = vec![service.id.clone()];
    let mut guests = Vec::new();
    for name in &protection.guests {
        let guest = service_token(token, &account.id, name).await?;
        ids.push(guest.id);
        guests.push(Guest {
            name: name.clone(),
            client_id: guest.client_id,
            client_secret: guest.client_secret,
        });
    }
    let policy = service_token_policy(token, &account.id, &protection.app, &ids).await?;
    let app = self_hosted_app(
        token,
        &account.id,
        &protection.app,
        domain,
        &policy,
        protection.token_header.as_deref(),
    )
    .await?;
    Ok(Protected {
        access_app: app,
        client_id: service.client_id,
        secrets: [
            protection.client_id_secret.clone(),
            protection.client_secret_secret.clone(),
        ],
        guests,
    })
}

/// Keeps the service token's client ID and secret in the Secrets Store. An existing token whose
/// secret the store lacks is rotated, since its secret can't be read back.
async fn store(
    writer: &Writer<'_>,
    token: &str,
    account: &str,
    service: &ServiceToken,
    protection: &Protection,
) -> Result<()> {
    let comment = format!("Access service token `{}`", protection.service_token);
    let secret = match &service.client_secret {
        Some(s) => Some(s.clone()),
        None if writer
            .list()
            .await?
            .iter()
            .any(|s| s.name == protection.client_secret_secret) =>
        {
            None
        }
        None => Some(rotate_service_token(token, account, &service.id).await?),
    };
    writer
        .put(&protection.client_id_secret, &service.client_id, &comment)
        .await?;
    match secret {
        Some(s) => {
            writer
                .put(&protection.client_secret_secret, &s, &comment)
                .await
        }
        None => Ok(()),
    }
}

/// What [`unprotect`] removed; each is `None` or empty when it was already gone.
#[derive(Serialize)]
pub struct Retired {
    pub access_app: Option<String>,
    /// The policy `protection.app` names, and any other policy that lets the service token in.
    pub policies: Vec<String>,
    pub service_token: Option<String>,
    pub secrets: Vec<String>,
}

/// Takes `domain` out from behind Access and removes what `protection` named: the application
/// on `domain`, its reusable policy and any other policy that lets its service token in, the
/// token, and the token's secrets in the Secrets Store; idempotent. Each goes before what it
/// holds: the application, then the policies, then the token.
pub async fn unprotect(
    account: &Account,
    domain: &str,
    protection: &Protection,
) -> Result<Retired> {
    let token = Scoped::open(account, "zero-trust", policies()).await?;
    let removed = unprotect_with(account, token.value(), domain, protection).await;
    token.finish(removed).await
}

async fn unprotect_with(
    account: &Account,
    token: &str,
    domain: &str,
    protection: &Protection,
) -> Result<Retired> {
    let access = format!("/accounts/{}/access", account.id);
    let app = remove(
        token,
        &access,
        "apps",
        list(token, &account.id, "apps").await?,
        |a| a.domain == domain,
    )
    .await?;
    let service = list(token, &account.id, "service_tokens")
        .await?
        .into_iter()
        .find(|t| t.name == protection.service_token)
        .map(|t| t.id);
    let policies = remove_policies(token, &access, &protection.app, service.as_deref()).await?;
    let service_token = remove(
        token,
        &access,
        "service_tokens",
        list(token, &account.id, "service_tokens").await?,
        |t| t.name == protection.service_token,
    )
    .await?;
    let writer = Writer::open(account).await?;
    let secrets = remove_secrets(&writer, protection).await;
    let secrets = writer.finish(secrets).await?;
    Ok(Retired {
        access_app: app,
        policies,
        service_token,
        secrets,
    })
}

/// Deletes the first of `listed` that `pick` matches, from `access/what`: its ID, or `None`.
async fn remove(
    token: &str,
    access: &str,
    what: &str,
    listed: Vec<Listed>,
    pick: impl Fn(&Listed) -> bool,
) -> Result<Option<String>> {
    let Some(found) = listed.into_iter().find(|l| pick(l)) else {
        return Ok(None);
    };
    call(
        token,
        Method::Delete,
        &format!("{access}/{what}/{}", found.id),
        None,
    )
    .await?;
    Ok(Some(found.id))
}

async fn remove_secrets(writer: &Writer<'_>, protection: &Protection) -> Result<Vec<String>> {
    let names = [
        &protection.client_id_secret,
        &protection.client_secret_secret,
    ];
    let mut removed = Vec::new();
    for secret in writer.list().await? {
        if names.contains(&&secret.name) {
            writer.delete(&secret.id).await?;
            removed.push(secret.name);
        }
    }
    Ok(removed)
}

/// Deletes reusable policy `name` and every policy that includes service token `service`:
/// their IDs.
async fn remove_policies(
    token: &str,
    access: &str,
    name: &str,
    service: Option<&str>,
) -> Result<Vec<String>> {
    let listed = call(token, Method::Get, &format!("{access}/policies"), None).await?;
    let mut removed = Vec::new();
    for policy in listed.as_array().into_iter().flatten() {
        let named = policy["name"].as_str() == Some(name);
        let holds_token = service.is_some_and(|id| {
            policy["include"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|rule| rule["service_token"]["token_id"].as_str() == Some(id))
        });
        if let (true, Some(id)) = (named || holds_token, policy["id"].as_str()) {
            call(
                token,
                Method::Delete,
                &format!("{access}/policies/{id}"),
                None,
            )
            .await?;
            removed.push(id.to_string());
        }
    }
    Ok(removed)
}

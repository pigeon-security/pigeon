//! Cloudflare Access objects: service tokens, reusable policies and self-hosted applications.
//! A [`Session`] holds one minted token for a run of operations. Each finds its object by name
//! or domain first, so running it again changes nothing.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call;
use crate::tokens::{Policy, Scope, Scoped};

/// What a session's token holds: the organization read is for the team domain.
pub const PERMISSIONS: [&str; 3] = [
    "Access: Apps and Policies Write",
    "Access: Service Tokens Write",
    "Access: Organizations, Identity Providers, and Groups Read",
];

/// How long a service token lives.
const SERVICE_TOKEN_DURATION: &str = "8760h";

/// How long an Access session lasts.
const SESSION_DURATION: &str = "24h";

/// A service token. `client_secret` is known only when it was just created or rotated.
#[derive(Serialize)]
pub struct ServiceToken {
    pub id: String,
    pub name: String,
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
    #[serde(default)]
    aud: String,
    /// The application's type: pigeon makes only `self_hosted` ones.
    #[serde(default, rename = "type")]
    kind: String,
}

impl Listed {
    /// Whether this is pigeon's kind of application on `domain`: self-hosted. Others on the same
    /// domain, such as the `mcp` application an MCP server portal makes for a server, are not
    /// pigeon's to update, remove or verify tokens against.
    fn ours_on(&self, domain: &str) -> bool {
        self.domain == domain && self.kind == SELF_HOSTED
    }
}

/// The application type pigeon makes.
const SELF_HOSTED: &str = "self_hosted";

/// Access operations on one account, as one minted token. Call [`Session::finish`] to revoke
/// the token.
pub struct Session<'a> {
    account: &'a Account,
    token: Scoped<'a>,
}

impl<'a> Session<'a> {
    /// Mints the session's token.
    pub async fn open(account: &'a Account) -> Result<Self> {
        let policies = vec![Policy::new(Scope::Account, &PERMISSIONS)];
        let token = Scoped::open(account, "access", policies).await?;
        Ok(Self { account, token })
    }

    /// Revokes the session's token, then answers `result`.
    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        self.token.finish(result).await
    }

    fn path(&self, what: &str) -> String {
        format!("/accounts/{}/access/{what}", self.account.id)
    }

    async fn list(&self, what: &str) -> Result<Vec<Listed>> {
        let v = call(self.token.value(), Method::Get, &self.path(what), None).await?;
        Ok(serde_json::from_value(v)?)
    }

    async fn delete(&self, what: &str, id: &str) -> Result<()> {
        let path = format!("{}/{id}", self.path(what));
        call(self.token.value(), Method::Delete, &path, None).await?;
        Ok(())
    }

    /// Service token `name`: the existing one, without its secret, or a new one, with it.
    pub async fn service_token(&self, name: &str) -> Result<ServiceToken> {
        if let Some(t) = self
            .list("service_tokens")
            .await?
            .into_iter()
            .find(|t| t.name == name)
        {
            return Ok(ServiceToken {
                id: t.id,
                name: name.into(),
                client_id: t.client_id,
                client_secret: None,
            });
        }
        let body = json!({ "name": name, "duration": SERVICE_TOKEN_DURATION });
        let v = call(
            self.token.value(),
            Method::Post,
            &self.path("service_tokens"),
            Some(&body),
        )
        .await?;
        Ok(ServiceToken {
            id: id(&v, "service token")?,
            name: name.into(),
            client_id: v["client_id"].as_str().unwrap_or_default().to_string(),
            client_secret: v["client_secret"].as_str().map(String::from),
        })
    }

    /// Rotates service token `id`: its new client secret.
    pub async fn rotate(&self, id: &str) -> Result<String> {
        let path = format!("{}/{id}/rotate", self.path("service_tokens"));
        let v = call(self.token.value(), Method::Post, &path, None).await?;
        v["client_secret"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                Error::RustError("the rotated token came back without its secret".into())
            })
    }

    /// Reusable policy `name`, letting in the service tokens `ids`: its ID.
    pub async fn policy(&self, name: &str, ids: &[String]) -> Result<String> {
        let include: Vec<Value> = ids
            .iter()
            .map(|id| json!({ "service_token": { "token_id": id } }))
            .collect();
        let body = json!({ "name": name, "decision": "non_identity", "include": include });
        let path = self.path("policies");
        match self
            .list("policies")
            .await?
            .into_iter()
            .find(|p| p.name == name)
        {
            Some(p) => {
                let at = format!("{path}/{}", p.id);
                call(self.token.value(), Method::Put, &at, Some(&body)).await?;
                Ok(p.id)
            }
            None => id(
                &call(self.token.value(), Method::Post, &path, Some(&body)).await?,
                "policy",
            ),
        }
    }

    /// Self-hosted application `name` on `domain`, held by reusable policy `policy_id`: its ID.
    /// With `token_header`, it also reads a service token from that one header, as JSON:
    /// `{"cf-access-client-id": ..., "cf-access-client-secret": ...}`.
    pub async fn app(
        &self,
        name: &str,
        domain: &str,
        policy_id: &str,
        token_header: Option<&str>,
    ) -> Result<String> {
        let mut body = json!({
            "name": name,
            "domain": domain,
            "type": SELF_HOSTED,
            "session_duration": SESSION_DURATION,
            "policies": [{ "id": policy_id, "precedence": 1 }],
        });
        if let Some(header) = token_header {
            body["read_service_tokens_from_header"] = json!(header);
        }
        let path = self.path("apps");
        match self
            .list("apps")
            .await?
            .into_iter()
            .find(|a| a.ours_on(domain))
        {
            Some(a) => {
                let at = format!("{path}/{}", a.id);
                call(self.token.value(), Method::Put, &at, Some(&body)).await?;
                Ok(a.id)
            }
            None => id(
                &call(self.token.value(), Method::Post, &path, Some(&body)).await?,
                "application",
            ),
        }
    }

    /// Deletes self-hosted application `name` on `domain`, as pigeon made it: its ID, or `None`
    /// when there was none. An application of another name or type on the domain is someone
    /// else's and stays.
    pub async fn remove_app(&self, name: &str, domain: &str) -> Result<Option<String>> {
        let Some(a) = self
            .list("apps")
            .await?
            .into_iter()
            .find(|a| a.ours_on(domain) && a.name == name)
        else {
            return Ok(None);
        };
        self.delete("apps", &a.id).await?;
        Ok(Some(a.id))
    }

    /// Deletes reusable policy `name` and every policy that includes service token `token_id`;
    /// a token cannot be deleted while a policy holds it. Answers their IDs.
    pub async fn remove_policies(&self, name: &str, token_id: Option<&str>) -> Result<Vec<String>> {
        let listed = call(
            self.token.value(),
            Method::Get,
            &self.path("policies"),
            None,
        )
        .await?;
        let mut removed = Vec::new();
        for policy in listed.as_array().into_iter().flatten() {
            let named = policy["name"].as_str() == Some(name);
            let holds = token_id.is_some_and(|t| {
                policy["include"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|rule| rule["service_token"]["token_id"].as_str() == Some(t))
            });
            if let (true, Some(id)) = (named || holds, policy["id"].as_str()) {
                self.delete("policies", id).await?;
                removed.push(id.to_string());
            }
        }
        Ok(removed)
    }

    /// The ID of service token `name`, if it exists.
    pub async fn find_service_token(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .list("service_tokens")
            .await?
            .into_iter()
            .find(|t| t.name == name)
            .map(|t| t.id))
    }

    /// A reusable policy `name` that lets everyone through, for paths that check callers
    /// themselves: its ID.
    pub async fn bypass_policy(&self, name: &str) -> Result<String> {
        let body = json!({ "name": name, "decision": "bypass", "include": [{ "everyone": {} }] });
        let path = self.path("policies");
        match self
            .list("policies")
            .await?
            .into_iter()
            .find(|p| p.name == name)
        {
            Some(p) => {
                let at = format!("{path}/{}", p.id);
                call(self.token.value(), Method::Put, &at, Some(&body)).await?;
                Ok(p.id)
            }
            None => id(
                &call(self.token.value(), Method::Post, &path, Some(&body)).await?,
                "policy",
            ),
        }
    }

    /// The AUD tag of the self-hosted application on `domain`, the audience of the tokens it signs.
    pub async fn app_aud(&self, domain: &str) -> Result<Option<String>> {
        Ok(self
            .list("apps")
            .await?
            .into_iter()
            .find(|a| a.ours_on(domain))
            .map(|a| a.aud))
    }

    /// The team domain Access signs as, such as `team.cloudflareaccess.com`.
    pub async fn team_domain(&self) -> Result<String> {
        let v = call(
            self.token.value(),
            Method::Get,
            &self.path("organizations"),
            None,
        )
        .await?;
        v["auth_domain"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::RustError("the Access organization has no team domain".into()))
    }

    /// The name of the service token whose client ID is `client_id`, if it exists.
    pub async fn service_token_name(&self, client_id: &str) -> Result<Option<String>> {
        Ok(self
            .list("service_tokens")
            .await?
            .into_iter()
            .find(|t| t.client_id == client_id)
            .map(|t| t.name))
    }

    /// Deletes service token `id`.
    pub async fn remove_service_token(&self, id: &str) -> Result<()> {
        self.delete("service_tokens", id).await
    }
}

fn id(v: &Value, what: &str) -> Result<String> {
    v["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| Error::RustError(format!("the {what} came back without its id")))
}

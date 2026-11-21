//! Cloudflare Artifacts: git repositories hosted on Cloudflare, grouped into namespaces. This
//! capability creates repositories (empty or imported from a public git remote) and mints,
//! lists, revokes and stores the git tokens that reach them. Git sends a token as
//! `Authorization: Bearer <token>`.

use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare;
use crate::secrets_store::Writer;
use crate::tokens::{Policy, Scope, Scoped};

/// A repository to have.
pub struct Spec {
    /// Its namespace, created if missing.
    pub namespace: String,
    /// Its name in the namespace.
    pub name: String,
    pub default_branch: String,
    pub description: Option<String>,
    /// A public git remote to import from when the repository is created; empty otherwise.
    pub import_from: Option<String>,
}

/// A repository, as the API describes it.
#[derive(Debug, Serialize)]
pub struct Repo {
    pub namespace: String,
    pub name: String,
    /// Its git remote URL.
    pub remote: String,
    pub default_branch: Option<String>,
}

/// What a git token may do.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Fetch and clone.
    Read,
    /// Push too.
    Write,
}

impl Access {
    fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// A minted git token. `value` is the secret.
pub struct GitToken {
    pub id: Option<String>,
    pub value: String,
    pub access: Access,
    pub ttl_secs: u64,
}

/// A git token as listed: never its value.
#[derive(Debug, Serialize)]
pub struct Listed {
    pub id: Option<String>,
    /// Whatever else the API says of it, value fields removed.
    pub details: Value,
}

/// What [`ensure`] did.
#[derive(Serialize)]
pub struct Ensured {
    pub repo: Repo,
    pub namespace_created: bool,
    /// `created`, `imported` or `existing`.
    pub repo_state: &'static str,
}

/// The Cloudflare permissions an Artifacts call needs.
fn policies() -> Vec<Policy> {
    vec![Policy::new(
        Scope::Account,
        &["Artifacts Read", "Artifacts Write"],
    )]
}

/// Has repository `spec` exist; idempotent. A missing namespace is created; a missing repository
/// is imported from `spec.import_from` when set, created empty otherwise.
pub async fn ensure(account: &Account, spec: &Spec) -> Result<Ensured> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let api = Api::new(account, token.value());
    let ensured = ensure_with(&api, spec).await;
    token.finish(ensured).await
}

/// Repository `name` in `namespace`, if it exists.
pub async fn repo(account: &Account, namespace: &str, name: &str) -> Result<Option<Repo>> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let found = Api::new(account, token.value()).repo(namespace, name).await;
    token.finish(found).await
}

/// Deletes repository `name` in `namespace`.
pub async fn delete(account: &Account, namespace: &str, name: &str) -> Result<()> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let api = Api::new(account, token.value());
    let path = api.repo_path(namespace, name);
    let deleted = call(api.token, Method::Delete, &path, None)
        .await
        .map(|_| ());
    token.finish(deleted).await
}

/// Mints a git token for repository `name` in `namespace`, living `ttl_secs`.
pub async fn mint(
    account: &Account,
    namespace: &str,
    name: &str,
    access: Access,
    ttl_secs: u64,
) -> Result<GitToken> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let api = Api::new(account, token.value());
    let minted = api.mint(namespace, name, access, ttl_secs).await;
    token.finish(minted).await
}

/// The git tokens of repository `name` in `namespace`, live and past, without their values.
pub async fn tokens(account: &Account, namespace: &str, name: &str) -> Result<Vec<Listed>> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let api = Api::new(account, token.value());
    let path = format!(
        "{}/tokens?state=all&per_page=100",
        api.repo_path(namespace, name)
    );
    let listed = call(api.token, Method::Get, &path, None)
        .await
        .map(|v| listed(&v));
    token.finish(listed).await
}

/// Revokes git token `id` of repository `name` in `namespace`.
pub async fn revoke(account: &Account, namespace: &str, name: &str, id: &str) -> Result<()> {
    let token = Scoped::open(account, "artifacts", policies()).await?;
    let api = Api::new(account, token.value());
    let revoked = api.revoke(namespace, name, id).await;
    token.finish(revoked).await
}

impl Api<'_> {
    /// Revokes token `id`, where tokens are minted: under the namespace first, then the
    /// repository, moving on only when a path does not exist (404).
    async fn revoke(&self, namespace: &str, name: &str, id: &str) -> Result<()> {
        let at_namespace = format!("{}/{namespace}/tokens/{id}", self.namespaces());
        let at_repo = format!("{}/tokens/{id}", self.repo_path(namespace, name));
        let tries = [
            (Method::Delete, at_namespace.clone()),
            (Method::Delete, at_repo),
            (Method::Post, format!("{at_namespace}/revoke")),
        ];
        let mut misses = Vec::new();
        for (method, path) in tries {
            match call(self.token, method, &path, None).await {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains(": 404:") => misses.push(e.to_string()),
                Err(e) => return Err(e),
            }
        }
        Err(Error::RustError(format!(
            "no revoke path answered: {}",
            misses.join("; ")
        )))
    }
}

/// Mints a git token and keeps it in the Secrets Store as `secret`, replacing what was there;
/// answers the token's ID, never its value.
pub async fn store(
    account: &Account,
    namespace: &str,
    name: &str,
    access: Access,
    ttl_secs: u64,
    secret: &str,
) -> Result<Option<String>> {
    let minted = mint(account, namespace, name, access, ttl_secs).await?;
    let writer = Writer::open(account).await?;
    let comment = format!("Artifacts {} token for {namespace}/{name}", access.word());
    let put = writer.put(secret, &minted.value, &comment).await;
    writer.finish(put).await?;
    Ok(minted.id)
}

/// Artifacts calls on one account, as one minted token.
struct Api<'a> {
    account: &'a str,
    token: &'a str,
}

impl<'a> Api<'a> {
    fn new(account: &'a Account, token: &'a str) -> Self {
        Self {
            account: &account.id,
            token,
        }
    }

    fn namespaces(&self) -> String {
        format!("/accounts/{}/artifacts/namespaces", self.account)
    }

    fn repo_path(&self, namespace: &str, name: &str) -> String {
        format!("{}/{namespace}/repos/{name}", self.namespaces())
    }

    /// `path`'s result, or `None` when it answers 404.
    /// The rows `path` lists. Artifacts is found by listing: asking for a missing namespace or
    /// repository by name answers 401, not 404.
    async fn list(&self, path: &str) -> Result<Vec<Value>> {
        let v = call(
            self.token,
            Method::Get,
            &format!("{path}?per_page=100"),
            None,
        )
        .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    async fn has_namespace(&self, namespace: &str) -> Result<bool> {
        Ok(self
            .list(&self.namespaces())
            .await?
            .iter()
            .any(|n| named(n, &["namespace", "name"], namespace)))
    }

    /// Ensures `namespace` exists; whether it was created.
    async fn namespace(&self, namespace: &str) -> Result<bool> {
        if self.has_namespace(namespace).await? {
            return Ok(false);
        }
        let body = json!({ "namespace": namespace });
        call(self.token, Method::Post, &self.namespaces(), Some(&body)).await?;
        Ok(true)
    }

    async fn repo(&self, namespace: &str, name: &str) -> Result<Option<Repo>> {
        if !self.has_namespace(namespace).await? {
            return Ok(None);
        }
        let repos = format!("{}/{namespace}/repos", self.namespaces());
        let Some(listed) = self
            .list(&repos)
            .await?
            .into_iter()
            .find(|r| named(r, &["name"], name))
        else {
            return Ok(None);
        };
        // A listed repository may leave out its remote; the repository itself has it.
        let v = if listed.get("remote").is_some() || listed.get("url").is_some() {
            listed
        } else {
            call(
                self.token,
                Method::Get,
                &self.repo_path(namespace, name),
                None,
            )
            .await?
        };
        repo_of(&v, namespace, name).map(Some)
    }

    async fn mint(
        &self,
        namespace: &str,
        name: &str,
        access: Access,
        ttl_secs: u64,
    ) -> Result<GitToken> {
        let scope = access.word();
        let at_namespace = format!("{}/{namespace}/tokens", self.namespaces());
        let body = json!({ "repo": name, "scope": scope, "ttl": ttl_secs });
        let v = match call(self.token, Method::Post, &at_namespace, Some(&body)).await {
            Ok(v) => v,
            // Some accounts mint under the repository instead.
            Err(first) => {
                let at_repo = format!("{}/tokens", self.repo_path(namespace, name));
                let body = json!({ "scope": scope, "ttl": ttl_secs });
                call(self.token, Method::Post, &at_repo, Some(&body))
                    .await
                    .map_err(|second| Error::RustError(format!("{first}; then {second}")))?
            }
        };
        let value = token_in(&v).ok_or_else(|| {
            Error::RustError(format!(
                "Artifacts gave no token; its answer's fields are {}",
                fields(&v)
            ))
        })?;
        Ok(GitToken {
            id: v["id"].as_str().map(String::from),
            value,
            access,
            ttl_secs,
        })
    }
}

async fn ensure_with(api: &Api<'_>, spec: &Spec) -> Result<Ensured> {
    let namespace_created = api.namespace(&spec.namespace).await?;
    if let Some(repo) = api.repo(&spec.namespace, &spec.name).await? {
        return Ok(Ensured {
            repo,
            namespace_created,
            repo_state: "existing",
        });
    }
    let repos = format!("{}/{}/repos", api.namespaces(), spec.namespace);
    let (v, repo_state) = match &spec.import_from {
        Some(url) => {
            let path = format!("{}/import", api.repo_path(&spec.namespace, &spec.name));
            let body = json!({ "url": url, "branch": spec.default_branch });
            (
                call(api.token, Method::Post, &path, Some(&body)).await?,
                "imported",
            )
        }
        None => {
            let mut body = json!({ "name": spec.name, "default_branch": spec.default_branch });
            if let Some(d) = &spec.description {
                body["description"] = json!(d);
            }
            (
                call(api.token, Method::Post, &repos, Some(&body)).await?,
                "created",
            )
        }
    };
    let repo = match api.repo(&spec.namespace, &spec.name).await? {
        Some(repo) => repo,
        None => repo_of(&v, &spec.namespace, &spec.name)?,
    };
    Ok(Ensured {
        repo,
        namespace_created,
        repo_state,
    })
}

fn repo_of(v: &Value, namespace: &str, name: &str) -> Result<Repo> {
    let remote = ["remote", "url", "git_url"]
        .iter()
        .find_map(|k| v[*k].as_str())
        .ok_or_else(|| {
            Error::RustError(format!("repository `{namespace}/{name}` has no remote"))
        })?;
    Ok(Repo {
        namespace: namespace.to_string(),
        name: v["name"].as_str().unwrap_or(name).to_string(),
        remote: remote.to_string(),
        default_branch: v["default_branch"].as_str().map(String::from),
    })
}

/// Where a token's value may sit in an answer.
const VALUE_FIELDS: [&str; 6] = [
    "token",
    "plaintext",
    "value",
    "secret",
    "access_token",
    "password",
];

/// The token value in an answer: the answer itself, a value field, or one nested in `token` or
/// `data`.
fn token_in(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    VALUE_FIELDS
        .iter()
        .find_map(|k| v[*k].as_str())
        .map(String::from)
        .or_else(|| {
            ["token", "data"]
                .iter()
                .find_map(|k| v.get(*k).filter(|n| n.is_object()).and_then(token_in))
        })
}

/// An answer's top-level field names, to say what came back without showing values.
fn fields(v: &Value) -> String {
    v.as_object()
        .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_else(|| "none".into())
}

/// The listed tokens in an answer, their value fields removed.
fn listed(v: &Value) -> Vec<Listed> {
    let rows = v
        .as_array()
        .or_else(|| v["tokens"].as_array())
        .or_else(|| v["items"].as_array());
    rows.into_iter()
        .flatten()
        .map(|t| {
            let mut details = t.clone();
            if let Some(o) = details.as_object_mut() {
                for k in VALUE_FIELDS {
                    o.remove(k);
                }
            }
            Listed {
                id: t["id"].as_str().map(String::from),
                details,
            }
        })
        .collect()
}

/// Whether listed row `v` is `name`: the row itself, or one of its `fields`.
fn named(v: &Value, fields: &[&str], name: &str) -> bool {
    v.as_str() == Some(name) || fields.iter().any(|f| v[*f].as_str() == Some(name))
}

/// How long to wait before each retry of a call Artifacts refuses with 401.
const NOT_YET_ACTIVE_WAITS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// [`cloudflare::call`], retried while Artifacts answers 401: it refuses a token minted moments
/// before, until the token reaches it.
async fn call(token: &str, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    let mut waits = NOT_YET_ACTIVE_WAITS_MS.iter();
    loop {
        match cloudflare::call(token, method.clone(), path, body).await {
            Err(e) if e.to_string().contains(": 401:") => match waits.next() {
                Some(ms) => worker::Delay::from(std::time::Duration::from_millis(*ms)).await,
                None => return Err(e),
            },
            other => return other,
        }
    }
}

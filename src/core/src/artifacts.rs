//! Cloudflare Artifacts: git repositories hosted on Cloudflare, grouped into namespaces. This
//! capability creates repositories (empty or imported from a public git remote) and mints,
//! lists, revokes and stores the git tokens that reach them. Git sends a token as
//! `Authorization: Bearer <token>`.

use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Method, Result};

use crate::Account;
use crate::cloudflare::call_fresh as call;
use crate::secrets_store::Writer;
use crate::tokens::{self, MintRequest, Policy, Scope, Scoped};

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
#[derive(Debug, Clone, Serialize)]
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

/// How long an account token must still have to be reused, in milliseconds.
const REUSE_MIN_LEFT_MS: i64 = 5 * 60 * 1000;

/// One Cloudflare account token kept for reuse: the account it is for, its value and when it
/// expires, in milliseconds since the epoch.
type Kept = (String, String, i64);

/// A slot holding one account token, so requests share it instead of minting one each.
#[derive(Default)]
pub struct TokenCache {
    kept: std::cell::RefCell<Option<Kept>>,
}

impl TokenCache {
    /// The kept token's value for `account`, if it has at least five minutes left at `now_ms`.
    fn fresh(&self, account: &str, now_ms: i64) -> Option<String> {
        let kept = self.kept.borrow();
        let (a, value, expires) = kept.as_ref()?;
        (a == account && *expires - now_ms >= REUSE_MIN_LEFT_MS).then(|| value.clone())
    }

    /// Keeps `value`, expiring at `expires_ms`, for `account`.
    fn put(&self, account: &str, value: &str, expires_ms: i64) {
        *self.kept.borrow_mut() = Some((account.to_string(), value.to_string(), expires_ms));
    }

    /// Forgets the kept token.
    fn clear(&self) {
        *self.kept.borrow_mut() = None;
    }
}

std::thread_local! {
    /// The account token the git proxy's Artifacts calls share, for this isolate.
    static ACCOUNT_TOKEN: std::rc::Rc<TokenCache> = std::rc::Rc::default();
}

/// Runs `work` with a cached account token for `account`. `mint` makes a token as
/// `(value, expires_ms)`, called only when none is kept with five minutes left. When `work`
/// fails with a 401 on a kept token, the token is dropped and one new one is minted for a second
/// try; a freshly minted token that gets a 401 is dropped and the error returned. Never loops.
/// Kept tokens are not revoked: they expire on their own.
pub async fn with_cached_token<T, M, MF, W, WF>(
    cache: &TokenCache,
    account: &str,
    now_ms: i64,
    mut mint: M,
    mut work: W,
) -> Result<T>
where
    M: FnMut() -> MF,
    MF: std::future::Future<Output = Result<(String, i64)>>,
    W: FnMut(String) -> WF,
    WF: std::future::Future<Output = Result<T>>,
{
    let mut minted_here = false;
    loop {
        let value = match cache.fresh(account, now_ms) {
            Some(v) => v,
            None => {
                let (value, expires_ms) = mint().await?;
                cache.put(account, &value, expires_ms);
                minted_here = true;
                value
            }
        };
        match work(value).await {
            Err(e) if e.to_string().contains(": 401:") => {
                cache.clear();
                if minted_here {
                    return Err(e);
                }
            }
            other => return other,
        }
    }
}

/// [`with_cached_token`] for the real account: a linked account's bearer is used as it is.
async fn with_account_token<T, W, WF>(account: &Account, work: W) -> Result<T>
where
    W: FnMut(String) -> WF,
    WF: std::future::Future<Output = Result<T>>,
{
    if let Some(bearer) = account.bearer.as_deref() {
        let mut work = work;
        return work(bearer.to_string()).await;
    }
    let now_ms = worker::Date::now().as_millis() as i64;
    let mint = || async {
        let request = MintRequest::new("artifacts", policies());
        let m = tokens::mint_for(account, &request).await?;
        let expires = tokens::parse_ms(&m.expires_on)
            .ok_or_else(|| Error::RustError("the minted token has no expiry".into()))?;
        Ok((m.value, expires))
    };
    let cache = ACCOUNT_TOKEN.with(std::rc::Rc::clone);
    with_cached_token(&cache, &account.id, now_ms, mint, work).await
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

/// How long this isolate keeps a repository it looked up, in milliseconds: a repository's
/// remote rarely changes, and the lookup is two or three list calls.
pub const REPO_KEPT_MS: i64 = 5 * 60 * 1000;

/// The most repositories one isolate keeps; past it, the kept ones are forgotten.
const REPOS_KEPT: usize = 64;

/// Repositories looked up, per account, namespace and name, with when each was looked up.
#[derive(Default)]
pub struct RepoCache {
    kept: std::cell::RefCell<std::collections::HashMap<String, (Repo, i64)>>,
}

impl RepoCache {
    /// The key of repository `name` in `namespace` on `account`.
    pub fn key(account: &str, namespace: &str, name: &str) -> String {
        format!("{account}/{namespace}/{name}")
    }

    /// The repository kept under `key`, if looked up less than [`REPO_KEPT_MS`] before `now_ms`.
    pub fn get(&self, key: &str, now_ms: i64) -> Option<Repo> {
        let kept = self.kept.borrow();
        let (repo, at) = kept.get(key)?;
        (now_ms - at < REPO_KEPT_MS && now_ms >= *at).then(|| repo.clone())
    }

    /// Keeps `repo` under `key`, looked up at `now_ms`.
    pub fn put(&self, key: &str, repo: &Repo, now_ms: i64) {
        let mut kept = self.kept.borrow_mut();
        if kept.len() >= REPOS_KEPT && !kept.contains_key(key) {
            kept.clear();
        }
        kept.insert(key.to_string(), (repo.clone(), now_ms));
    }

    /// Forgets the repository under `key`.
    pub fn forget(&self, key: &str) {
        self.kept.borrow_mut().remove(key);
    }
}

std::thread_local! {
    /// The repositories `session` looked up, for this isolate.
    static REPOS: std::rc::Rc<RepoCache> = std::rc::Rc::default();
}

/// Forgets repository `name` in `namespace` on account `account` (its id), so the next
/// [`session`] looks it up again: for a caller that found the kept remote wrong (a 404).
pub fn forget_repo(account: &str, namespace: &str, name: &str) {
    REPOS.with(|r| r.forget(&RepoCache::key(account, namespace, name)));
}

/// The repository under `key`, from `cache` or else from `lookup` (kept when found), then
/// `work` on it. `lookup` answering `None` (no such repository), or failing, keeps nothing; when
/// `work` fails the kept repository is forgotten, since a wrong remote or a deleted repository
/// shows there first.
pub async fn with_cached_repo<T, L, LF, W, WF>(
    cache: &RepoCache,
    key: &str,
    now_ms: i64,
    lookup: L,
    work: W,
) -> Result<(Repo, T)>
where
    L: FnOnce() -> LF,
    LF: std::future::Future<Output = Result<Option<Repo>>>,
    W: FnOnce(Repo) -> WF,
    WF: std::future::Future<Output = Result<T>>,
{
    let repo = match cache.get(key, now_ms) {
        Some(repo) => repo,
        None => {
            cache.forget(key);
            let repo = lookup()
                .await?
                .ok_or_else(|| Error::RustError(format!("repository `{key}` does not exist")))?;
            cache.put(key, &repo, now_ms);
            repo
        }
    };
    match work(repo.clone()).await {
        Ok(v) => Ok((repo, v)),
        Err(e) => {
            cache.forget(key);
            Err(e)
        }
    }
}

/// Repository `name` in `namespace` and a git token for it, living `ttl_secs`: both under one
/// minted Cloudflare token, for a caller that needs the remote and a token together. The
/// repository is looked up at most every [`REPO_KEPT_MS`] per isolate ([`RepoCache`]); the git
/// token is minted every time.
pub async fn session(
    account: &Account,
    namespace: &str,
    name: &str,
    access: Access,
    ttl_secs: u64,
) -> Result<(Repo, GitToken)> {
    with_account_token(account, |token| async move {
        let api = Api::new(account, &token);
        let cache = REPOS.with(std::rc::Rc::clone);
        let key = RepoCache::key(&account.id, namespace, name);
        let now_ms = worker::Date::now().as_millis() as i64;
        with_cached_repo(
            &cache,
            &key,
            now_ms,
            || api.repo(namespace, name),
            |_| api.mint(namespace, name, access, ttl_secs),
        )
        .await
        .map_err(|e| {
            // The key holds the account id: name the repository as the caller does.
            Error::RustError(e.to_string().replace(&key, &format!("{namespace}/{name}")))
        })
    })
    .await
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
    with_account_token(account, |token| async move {
        Api::new(account, &token).revoke(namespace, name, id).await
    })
    .await
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

/// What a token needs to subscribe to a repository's events, besides the queue's own.
pub const EVENTS_PERMISSIONS: [&str; 1] = ["Artifacts Read"];

/// The event-subscription source for repository `name` in `namespace`, in the two ways the
/// docs name its repository field.
pub fn events_source(namespace: &str, name: &str) -> Vec<Value> {
    vec![
        json!({ "type": "artifacts.repo", "namespace": namespace, "repo_name": name }),
        json!({ "type": "artifacts.repo", "namespace": namespace, "repoName": name }),
    ]
}

/// The file at `path` in `commit` of `namespace/name`, read with a git token minted for it and
/// revoked after: the commit alone, one commit deep. Shared by core and the CI gateway.
pub async fn read_file(
    account: &Account,
    namespace: &str,
    repo: &str,
    commit: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let (found, token) = session(account, namespace, repo, Access::Read, 900).await?;
    let remote = crate::git::Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let read = crate::git::read_file(&remote, commit, path).await;
    let revoked = match &token.id {
        Some(id) => revoke(account, namespace, repo, id).await,
        None => Err(Error::RustError(
            "a git token came back without an id".into(),
        )),
    };
    let file = read?;
    revoked?;
    Ok(file)
}

/// Runs `work` over one temporary read-only Git session on `namespace/repo`: the remote and
/// its advertised refs. The session's token is revoked after, whether `work` succeeded or not;
/// `work`'s own error comes first.
async fn read_session<T, F, Fut>(
    account: &Account,
    namespace: &str,
    repo: &str,
    work: F,
) -> Result<T>
where
    F: FnOnce(crate::git::Remote) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let (found, token) = session(account, namespace, repo, Access::Read, 900).await?;
    let remote = crate::git::Remote {
        url: found.remote.trim_end_matches('/').to_string(),
        authorization: format!("Bearer {}", token.value),
    };
    let outcome = work(remote).await;
    let revoked = match &token.id {
        Some(id) => revoke(account, namespace, repo, id).await,
        None => Err(Error::RustError(
            "the Git token came back without an id".into(),
        )),
    };
    let result = outcome?;
    revoked?;
    Ok(result)
}

/// The commit `revision` (a commit id, `HEAD`, a branch or `refs/heads/…`) names in `advertised`.
fn resolved(advertised: &crate::git::Refs, revision: &str) -> Result<String> {
    if crate::git::is_id(revision) {
        return Ok(revision.to_ascii_lowercase());
    }
    let name = if revision == "HEAD" || revision.starts_with("refs/heads/") {
        revision.to_string()
    } else {
        format!("refs/heads/{revision}")
    };
    Ok(advertised
        .refs
        .get(&name)
        .filter(|sha| crate::git::is_id(sha))
        .ok_or_else(|| Error::RustError("branch was not found in the approved repository".into()))?
        .to_ascii_lowercase())
}

/// The objects of `commit` alone, one commit deep, checked to hold that commit.
async fn commit_objects(
    remote: &crate::git::Remote,
    caps: &[String],
    commit: &str,
) -> Result<crate::pack::Objects> {
    let bad = Error::RustError;
    let fetched = crate::git::fetch_with(
        remote,
        caps,
        std::slice::from_ref(&commit.to_string()),
        &[],
        None,
        Some(1),
    )
    .await?;
    let objects = crate::pack::objects(&fetched.pack).map_err(bad)?;
    if !objects
        .by_id
        .get(commit)
        .is_some_and(|(kind, _)| *kind == 1)
    {
        return Err(bad("the resolved object is not a repository commit".into()));
    }
    Ok(objects)
}

/// Resolve a revision and optionally read one file over one temporary read-only Git session.
pub async fn read_revision(
    account: &Account,
    namespace: &str,
    repo: &str,
    revision: &str,
    path: Option<&str>,
) -> Result<(String, Option<Vec<u8>>)> {
    let bad = Error::RustError;
    let revision = crate::repository::revision(revision).map_err(bad)?;
    if let Some(path) = path {
        crate::repository::file_path(path).map_err(bad)?;
    }
    read_session(account, namespace, repo, |remote| async move {
        let advertised = crate::git::refs(&remote, "git-upload-pack").await?;
        let commit = resolved(&advertised, revision)?;
        let file = if path.is_some() || crate::git::is_id(revision) {
            let objects = commit_objects(&remote, &advertised.caps, &commit).await?;
            path.map(|path| crate::pack::read_path(&objects, &commit, path))
                .transpose()
                .map_err(bad)?
                .flatten()
        } else {
            None
        };
        Ok((commit, file))
    })
    .await
}

/// Resolve a revision and read every regular file under directory `root` of it (the whole
/// tree when `None`), within `limits`, over one temporary read-only Git session: the commit and
/// the files, paths relative to `root` (`pack::read_tree`). The commit alone is fetched, one
/// commit deep.
pub async fn read_tree(
    account: &Account,
    namespace: &str,
    repo: &str,
    revision: &str,
    root: Option<&str>,
    limits: &crate::pack::TreeLimits,
) -> Result<(String, Vec<(String, Vec<u8>)>)> {
    let bad = Error::RustError;
    let revision = crate::repository::revision(revision).map_err(bad)?;
    read_session(account, namespace, repo, |remote| async move {
        let advertised = crate::git::refs(&remote, "git-upload-pack").await?;
        let commit = resolved(&advertised, revision)?;
        let objects = commit_objects(&remote, &advertised.caps, &commit).await?;
        let files = crate::pack::read_tree(&objects, &commit, root, limits).map_err(bad)?;
        Ok((commit, files))
    })
    .await
}

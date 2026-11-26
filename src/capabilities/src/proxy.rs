//! Proxy credentials: what a cloud session holds to reach the platform's proxies, `/git/` and
//! `/cf/`, and nothing else. A proxy credential is a bearer token of its own kind, so the MCP
//! endpoint, which looks up only access tokens, never accepts one; and the proxies accept
//! nothing else. Its value is shown once, when it is issued, and kept only as a hash; its id is
//! the first characters of that hash. The console's Access › Tokens lists, issues and revokes
//! them: seeing the list takes the console's session, and issuing or revoking takes a passkey of
//! its own whose challenge is bound to the person and that one credential's label and lifetime,
//! or id. Every proxied request is logged
//! as one audit line: who, which route, and what came of it.
//!
//! The checks the proxies make at request time are in [`pigeon_proxies::proxy`], shared with
//! the proxy Worker, and re-exported here. [`Local`] is core's [`Host`] for them: its own
//! Directory, CI scheduler and Lander, and the parent token.
//!
//! The proxy Worker (`pigeon-proxy`) serves the proxy hostname. Core keeps the same proxies as a
//! [`Backup`], served under [`Tokens`] rather than as capabilities of their own: they answer only
//! while the hostname is attached to core (`setup_proxy_host {"worker": "pigeon-platform"}`),
//! since core's fetch handler routes `/git/` and `/cf/` on the proxy hostname alone.

use pigeon_core::Account;
use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
// The proxies' modules, for the Worker that builds the backup.
pub use pigeon_proxies::cf_proxy::{self, CfProxy};
pub use pigeon_proxies::git_proxy::{self, GitProxy};
use pigeon_proxies::{Grant, Host, Pending, Purpose};
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result};

use crate::console::sections::Section;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Footprint, HostFootprint, Serve, unknown};

pub use pigeon_proxies::proxy::{
    Audit, ID_LEN, Outcome, PREFIX, ProxyCaller, audit, audit_line, host_serves, reaches,
};

/// A credential's lifetime unless asked otherwise, in days, and at most.
pub const DEFAULT_DAYS: i64 = 30;
/// The longest a credential may live, in days.
pub const MAX_DAYS: i64 = 90;

/// A credential's label, trimmed: 1 to 64 characters, none of them control characters.
pub fn label(raw: &str) -> std::result::Result<String, String> {
    let trimmed = raw.trim();
    let count = trimmed.chars().count();
    if count == 0 {
        return Err("the label is empty".into());
    }
    if count > 64 {
        return Err("the label is longer than 64 characters".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("the label holds control characters".into());
    }
    Ok(trimmed.to_string())
}

/// A credential's lifetime in days: [`DEFAULT_DAYS`] when not given, else an integer from 1 to
/// [`MAX_DAYS`].
pub fn days(v: &Value) -> std::result::Result<i64, String> {
    if v.is_null() {
        return Ok(DEFAULT_DAYS);
    }
    match v.as_i64() {
        Some(n) if (1..=MAX_DAYS).contains(&n) => Ok(n),
        Some(_) => Err(format!("days must be from 1 to {MAX_DAYS}")),
        None => Err("days must be a whole number".into()),
    }
}

/// A credential's id: exactly [`ID_LEN`] characters of base64url.
pub fn id(raw: &str) -> std::result::Result<&str, String> {
    if raw.len() == ID_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(raw)
    } else {
        Err("not a proxy credential id".into())
    }
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// The holder of proxy credential `presented` (the whole bearer value), when it is live: the
/// Directory's answer, by the credential's hash. Core's own check, and what its `verify` op
/// answers the proxy Worker.
pub async fn verify(env: &Env, presented: &str) -> Result<Option<ProxyCaller>> {
    let found = directory(env, "proxy.token_get", json!({ "hash": hash(presented) })).await?;
    if found.is_null() {
        return Ok(None);
    }
    Ok(serde_json::from_value(found).ok())
}

/// How long an AI agent run's git grant lives, in milliseconds: longer than an executor's
/// budget, shorter than a day.
pub const RUN_GRANT_MS: i64 = 12 * 3_600_000;

/// The branch prefix AI agent run `run` pushes under, when `run` is a run's id.
pub fn run_branch_prefix(run: &str) -> std::result::Result<String, String> {
    if run.is_empty() || run.len() > 32 || !run.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("a run grant names its run: up to 32 letters and digits".into());
    }
    Ok(format!("claude/ai-agent-{run}"))
}

/// Mints AI agent run `run`'s git grant for `person` (a user id): a proxy credential that
/// reaches `/git/` only and pushes only `claude/ai-agent-{run}` and `claude/ai-agent-{run}-…`,
/// for [`RUN_GRANT_MS`].
/// Answers the token, shown only to the run's sandbox, and when it expires.
pub async fn issue_run_grant(env: &Env, person: &str, run: &str) -> Result<(String, i64)> {
    let prefix = run_branch_prefix(run).map_err(|e| bad(&e))?;
    let (secret, secret_hash) = pigeon_core::oauth::token(PREFIX)?;
    directory(
        env,
        "run_grant.add",
        json!({
            "hash": secret_hash,
            "user_id": person,
            "label": format!("AI agent run {run}"),
            "scope": prefix,
            "ttl_ms": RUN_GRANT_MS,
        }),
    )
    .await?;
    Ok((secret, worker::js_sys::Date::now() as i64 + RUN_GRANT_MS))
}

/// Revokes AI agent run `run`'s git grants held by `person`, when the run ends. Answers how
/// many went; none left is not an error.
pub async fn revoke_run_grant(env: &Env, person: &str, run: &str) -> Result<u64> {
    let scope = run_branch_prefix(run).map_err(|e| bad(&e))?;
    let n = directory(
        env,
        "run_grant.revoke",
        json!({ "user_id": person, "scope": scope }),
    )
    .await?;
    Ok(n.as_u64().unwrap_or(0))
}

/// The grant of CI job token `presented` when it is live and for repository `repo`: core's
/// own check, and what its `grant` op answers the proxy Worker.
pub async fn job_grant(env: &Env, presented: &str, repo: &str) -> Result<Option<Grant>> {
    Ok(crate::ci::job_grant(env, presented)
        .await?
        .filter(|g| g.repo == repo))
}

/// Core as the proxies' [`Host`]: credentials from its Directory, job grants from its CI
/// scheduler, deploy grants from its Lander, and its own account, holding the parent token,
/// for every purpose.
pub struct Local<'a> {
    /// The request's context.
    pub cx: &'a Context<'a>,
}

impl Host for Local<'_> {
    fn account_id(&self) -> String {
        self.cx.account.id.clone()
    }

    fn verify<'a>(&'a self, presented: &'a str) -> Pending<'a, Option<ProxyCaller>> {
        Box::pin(verify(self.cx.env, presented))
    }

    fn job_grant<'a>(&'a self, presented: &'a str, repo: &'a str) -> Pending<'a, Option<Grant>> {
        Box::pin(job_grant(self.cx.env, presented, repo))
    }

    fn deploy_grant<'a>(
        &'a self,
        presented: &'a str,
        repo: &'a str,
    ) -> Pending<'a, Option<ProxyCaller>> {
        Box::pin(crate::land::deploy_caller(self.cx.env, presented, repo))
    }

    fn account<'a>(&'a self, _purpose: Purpose) -> Pending<'a, Account> {
        let account = self.cx.account.clone();
        Box::pin(async move { Ok(account) })
    }
}

/// The proxies as core serves them, the backup to the proxy Worker: the same
/// [`pigeon_proxies`] request handling, behind [`Local`].
pub struct Backup {
    /// The git proxy, `/git/`.
    pub git: GitProxy,
    /// The Cloudflare API proxy, `/cf/`.
    pub cf: CfProxy,
}

/// The routes the backup serves.
pub const BACKUP_ROUTES: [&str; 2] = [
    "/git/", // route: token
    "/cf/",  // route: token
];

/// The console's `token-issue` and `token-revoke` writes, and the backup proxies.
pub struct Tokens {
    /// The hostname the proxies are on, which a credential is saved for.
    pub proxy_host: String,
    /// The proxies core serves while the proxy hostname is attached to it; `None` serves none.
    pub backup: Option<Backup>,
}

impl Tokens {
    /// `token-issue`: a credential labelled `label` for `person`, living `days`; its secret is
    /// answered once.
    async fn issue(&self, env: &Env, label: &str, days: i64, person: &User) -> Result<Value> {
        let (secret, secret_hash) = token(PREFIX)?;
        directory(
            env,
            "proxy.token_add",
            json!({
                "hash": secret_hash, "label": label, "user_id": person.id,
                "ttl_ms": days * 86_400_000,
            }),
        )
        .await?;
        let message = format!(
            "Shown once. Save it as the cloud environment's network secret for {}: \
             header Authorization, prefix Bearer.",
            self.proxy_host
        );
        Ok(json!({
            "token": secret,
            "id": &secret_hash[..ID_LEN],
            "label": label,
            "expires_in_days": days,
            // The new console's ShowOnce (P3).
            "reveal": { "value": secret, "note": message, "kind": "token" },
            "message": message,
        }))
    }
}

impl Capability for Tokens {
    fn name(&self) -> &'static str {
        "proxy"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "proxy",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            area: "access",
            id: "tokens",
            title: "Tokens",
            description: "Live proxy credentials and when each ends. Issuing or revoking one asks for your passkey.",
            order: 20,
        }]
    }

    fn write_make<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            let person = w.person;
            match w.action.id {
                "token-issue" => {
                    let days = w.change["days"].as_i64().unwrap_or(DEFAULT_DAYS);
                    self.issue(cx.env, w.text(), days, person).await
                }
                "token-revoke" => {
                    let id = w.text();
                    directory(cx.env, "proxy.token_remove", json!({ "id": id })).await?;
                    Ok(json!({ "message": format!("{id} is revoked.") }))
                }
                other => Err(bad(&format!("the proxy makes no write `{other}`"))),
            }
        })
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        match self.backup {
            Some(_) => BACKUP_ROUTES.to_vec(),
            None => Vec::new(),
        }
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(async move {
            let Some(b) = &self.backup else {
                return Response::error("not found", 404);
            };
            let path = req.path();
            if path.starts_with("/git/") {
                b.git.serve(&Local { cx }, cx.worker, req).await
            } else if path.starts_with("/cf/") {
                b.cf.serve(&Local { cx }, req).await
            } else {
                Response::error("not found", 404)
            }
        })
    }

    fn footprint(&self) -> Footprint {
        let Some(b) = &self.backup else {
            return Footprint::default();
        };
        Footprint {
            hostnames: vec![HostFootprint {
                host: b.git.host.clone(),
                worker: String::new(),
                by: self.name().into(),
                access: Vec::new(),
                public_paths: BACKUP_ROUTES.iter().map(|r| (*r).to_string()).collect(),
            }],
            repos: vec![format!("{}/{}", b.git.namespace, b.git.repo)],
            ..Footprint::default()
        }
    }
}

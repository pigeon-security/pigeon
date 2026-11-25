//! Proxy credentials: what a cloud session holds to reach the platform's proxies, `/git/` and
//! `/cf/`, and nothing else. A proxy credential is a bearer token of its own kind, so the MCP
//! endpoint, which looks up only access tokens, never accepts one; and the proxies accept
//! nothing else. Its value is shown once, when it is issued, and kept only as a hash; its id is
//! the first characters of that hash. The app's Access page lists, issues and revokes them
//! (the old `/tokens` page redirects there): seeing the list takes the console's session, and
//! issuing or revoking takes a passkey of its own whose challenge
//! is bound to that one credential's label and lifetime, or id. Every proxied request is logged
//! as one audit line: who, which route, and what came of it.
//!
//! The checks the proxies make at request time are in [`pigeon_proxies::proxy`], shared with
//! the proxy Worker, and re-exported here. [`Local`] is core's [`Host`] for them: its own
//! Directory, CI scheduler and Lander, and the parent token.

use pigeon_core::Account;
use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
use pigeon_proxies::{Grant, Host, Pending, Purpose};
use serde_json::{Value, json};
use worker::{Env, Error, Method, Request, Response, Result};

use crate::console::page;
use crate::console::sections::Section;
use crate::passkeys::Passkeys;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Serve, unknown};

pub use pigeon_proxies::proxy::{
    Audit, ID_LEN, Outcome, PREFIX, ProxyCaller, audit, audit_line, host_serves, reaches,
};

/// A credential's lifetime unless asked otherwise, in days, and at most.
pub const DEFAULT_DAYS: i64 = 30;
/// The longest a credential may live, in days.
pub const MAX_DAYS: i64 = 90;

/// The challenge purpose for issuing a credential labelled with hash `label_hash`, living
/// `days`.
pub fn issue_purpose(label_hash: &str, days: i64) -> String {
    format!("proxy-token:issue:{label_hash}:{days}")
}

/// The challenge purpose for revoking credential `id`.
pub fn revoke_purpose(id: &str) -> String {
    format!("proxy-token:revoke:{id}")
}

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

/// A JSON answer that is never cached.
fn answer(value: &Value, status: u16) -> Result<Response> {
    let mut res = Response::from_json(value)?.with_status(status);
    res.headers_mut().set("Cache-Control", "no-store")?;
    Ok(res)
}

/// The old `/tokens` page's redirect, and the console's issue and revoke calls.
pub struct Tokens {
    /// The hostname the proxies are on, which a credential is saved for.
    pub proxy_host: String,
    pub passkeys: Passkeys,
}

impl Tokens {
    /// The passkey's user, who must be a person.
    async fn person(env: &Env, user_id: &str) -> Result<User> {
        let user: Option<User> =
            serde_json::from_value(directory(env, "users.get", json!({ "id": user_id })).await?)?;
        user.filter(|u| u.kind == "standard")
            .ok_or_else(|| bad("only a person can see or change proxy credentials"))
    }

    /// The label, lifetime and challenge purpose an issue call asks for.
    fn issue_input(body: &Value) -> Result<(String, i64, String)> {
        let label = label(body["label"].as_str().unwrap_or("")).map_err(|e| bad(&e))?;
        let days = days(&body["days"]).map_err(|e| bad(&e))?;
        let purpose = issue_purpose(&hash(&label), days);
        Ok((label, days, purpose))
    }

    /// The id and challenge purpose a revoke call asks for.
    fn revoke_input(body: &Value) -> Result<(String, String)> {
        let id = id(body["id"].as_str().unwrap_or(""))
            .map_err(|e| bad(&e))?
            .to_string();
        let purpose = revoke_purpose(&id);
        Ok((id, purpose))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let path = req.path();
        match (req.method(), path.as_str()) {
            (Method::Get, "/tokens") => page::redirect(page::CREDENTIALS),
            (
                Method::Post,
                "/tokens/issue/begin"
                | "/tokens/issue/finish"
                | "/tokens/revoke/begin"
                | "/tokens/revoke/finish",
            ) => {
                let body: Value = req.json().await.unwrap_or(Value::Null);
                match self.post(cx.env, &path, &body).await {
                    Ok(v) => answer(&v, 200),
                    Err(e) => answer(&json!({ "error": e.to_string() }), 400),
                }
            }
            _ => Response::error("not found", 404),
        }
    }

    async fn post(&self, env: &Env, path: &str, body: &Value) -> Result<Value> {
        match path {
            "/tokens/issue/begin" => {
                let (_, _, purpose) = Self::issue_input(body)?;
                self.passkeys.sign_in_begin(env, &purpose, None).await
            }
            "/tokens/issue/finish" => {
                // The purpose is worked out again from what is posted now, so the passkey
                // approved this label and lifetime.
                let (label, days, purpose) = Self::issue_input(body)?;
                let key = self
                    .passkeys
                    .sign_in_finish(env, &purpose, body, None)
                    .await?;
                let person = Self::person(env, &key.user_id).await?;
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
                Ok(json!({
                    "token": secret,
                    "id": &secret_hash[..ID_LEN],
                    "label": label,
                    "expires_in_days": days,
                    "message": format!(
                        "Shown once. Save it as the cloud environment's network secret for {}: \
                         header Authorization, prefix Bearer.",
                        self.proxy_host
                    ),
                }))
            }
            "/tokens/revoke/begin" => {
                let (_, purpose) = Self::revoke_input(body)?;
                self.passkeys.sign_in_begin(env, &purpose, None).await
            }
            "/tokens/revoke/finish" => {
                let (id, purpose) = Self::revoke_input(body)?;
                let key = self
                    .passkeys
                    .sign_in_finish(env, &purpose, body, None)
                    .await?;
                Self::person(env, &key.user_id).await?;
                directory(env, "proxy.token_remove", json!({ "id": id })).await?;
                Ok(json!({ "message": format!("{id} is revoked.") }))
            }
            _ => Err(bad("not found")),
        }
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
        vec![crate::secrets::keys_section()]
    }

    /// The live proxy credentials: id, label, who issued each, when, and when it ends; and
    /// every one's expiry, with those ending within a week or ended in the last two, for the
    /// overview.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let tokens = directory(cx.env, "proxy.tokens", json!({})).await?;
            let expiry = crate::expiry::proxy(cx.env).await?;
            let expiring: Vec<&crate::expiry::Row> =
                expiry.iter().filter(|r| r.needs_you()).collect();
            Ok(json!({ "tokens": tokens, "expiry": expiry, "expiring": expiring }))
        })
    }

    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move { Err(unknown(tool)) })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/tokens"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

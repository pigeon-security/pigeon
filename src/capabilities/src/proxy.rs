//! Proxy credentials: what a cloud session holds to reach the platform's proxies, `/git/` and
//! `/cf/`, and nothing else. A proxy credential is a bearer token of its own kind, so the MCP
//! endpoint, which looks up only access tokens, never accepts one; and the proxies accept
//! nothing else. Its value is shown once, when it is issued, and kept only as a hash; its id is
//! the first characters of that hash. The console's Keys section lists, issues and revokes them
//! (the old `/tokens` page redirects there): seeing the list takes the console's session, and
//! issuing or revoking takes a passkey of its own whose challenge
//! is bound to that one credential's label and lifetime, or id. Every proxied request is logged
//! as one audit line: who, which route, and what came of it.

use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{hash, token};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Method, Request, Response, Result, console_log};

use crate::console::page;
use crate::console::sections::Section;
use crate::passkeys::Passkeys;
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Serve, unknown};

/// How many characters of a credential's hash make its id.
pub const ID_LEN: usize = 12;
/// What every proxy credential starts with.
pub const PREFIX: &str = "pgn_px_";
/// A credential's lifetime unless asked otherwise, in days, and at most.
pub const DEFAULT_DAYS: i64 = 30;
pub const MAX_DAYS: i64 = 90;

/// Whether `path` holds an encoded separator or dot, a backslash, or a dot segment.
fn suspicious(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("%2f")
        || lower.contains("%5c")
        || lower.contains("%2e")
        || path.contains('\\')
        || path.split('/').any(|seg| seg == ".." || seg == ".")
}

/// Whether a bearer token of `kind` may reach raw `path`: a `proxy` token only the proxies
/// under `/git/` and `/cf/`, an `access` token only `/mcp`, any other kind nothing. No kind
/// reaches a path with an encoded separator or dot, a backslash, or a dot segment.
pub fn reaches(kind: &str, path: &str) -> bool {
    if suspicious(path) {
        return false;
    }
    match kind {
        "proxy" => path.starts_with("/git/") || path.starts_with("/cf/"),
        "access" => path == "/mcp",
        _ => false,
    }
}

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

/// Who holds the proxy credential a request carries.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProxyCaller {
    /// The credential's id.
    pub id: String,
    pub label: String,
    /// The person who issued it.
    pub person: String,
}

/// What came of a proxied request.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Sent on.
    Forwarded,
    /// Refused before anything was sent on.
    Refused,
    /// Sent on, or about to be, and it failed.
    Failed,
}

/// One proxied request, for the audit line.
pub struct Audit<'a> {
    /// `git` or `cf`.
    pub route: &'a str,
    pub method: &'a str,
    /// The request's path; a query string is cut off before it is logged.
    pub path: &'a str,
    pub outcome: Outcome,
    pub status: Option<u16>,
    /// Why, when refused or failed.
    pub why: Option<&'a str>,
}

/// `a` as one JSON audit line. It holds the credential's id, never its value or hash.
pub fn audit_line(caller: Option<&ProxyCaller>, a: &Audit<'_>) -> String {
    let path = a.path.split_once('?').map_or(a.path, |(p, _)| p);
    json!({
        "audit": "proxy",
        "route": a.route,
        "caller": caller.map(|c| format!("{} ({})", c.label, c.person)),
        "token": caller.map(|c| c.id.as_str()),
        "method": a.method,
        "path": path,
        "outcome": a.outcome,
        "status": a.status,
        "why": a.why,
    })
    .to_string()
}

/// Whether a request for `path` on `host` is served, when the proxies have `proxy_host` to
/// themselves: there, only the proxies and `/health`; anywhere else, everything but the
/// proxies. So the credential a cloud session's network proxy adds for the proxy host never
/// reaches the platform's other paths, and the proxies never answer on its other hosts.
pub fn host_serves(host: &str, proxy_host: &str, path: &str) -> bool {
    let proxied = path.starts_with("/git/") || path.starts_with("/cf/");
    if host.eq_ignore_ascii_case(proxy_host) {
        proxied || path == "/health"
    } else {
        !proxied
    }
}

/// Writes `a`'s audit line to the Worker's log.
pub fn audit(caller: Option<&ProxyCaller>, a: &Audit<'_>) {
    console_log!("{}", audit_line(caller, a));
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// The holder of the proxy credential `req` carries as `Authorization: Bearer`, if it is live
/// and reaches the request's path. Errors never repeat the header.
pub async fn caller(env: &Env, req: &Request) -> Result<ProxyCaller> {
    let unknown = || bad("the proxy credential is unknown or has expired");
    let header = req
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .ok_or_else(|| bad("no proxy credential"))?;
    let (scheme, rest) = header
        .trim()
        .split_once(' ')
        .ok_or_else(|| bad("no proxy credential"))?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(bad("no proxy credential"));
    }
    let presented = rest.trim();
    if !presented.starts_with(PREFIX) {
        return Err(bad("not a proxy credential"));
    }
    if !reaches("proxy", &req.path()) {
        return Err(bad("a proxy credential does not reach this path"));
    }
    let found = directory(env, "proxy.token_get", json!({ "hash": hash(presented) }))
        .await
        .map_err(|_| unknown())?;
    if found.is_null() {
        return Err(unknown());
    }
    serde_json::from_value(found).map_err(|_| unknown())
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
            (Method::Get, "/tokens") => page::redirect("/settings#credentials"),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PATHS: [&str; 15] = [
        "/git/x",
        "/cf/client/v4/x",
        "/git",
        "/gitx/",
        "/cf",
        "/cfx",
        "/mcp",
        "/mcp/",
        "/tokens",
        "/git/a%2Fb",
        "/git/a%2fb",
        "/git/a/../b",
        "/git/./a",
        "/cf/a\\b",
        "/git/%2E%2E/x",
    ];

    fn reached(kind: &str) -> Vec<&'static str> {
        PATHS.into_iter().filter(|p| reaches(kind, p)).collect()
    }

    #[test]
    fn a_proxy_token_reaches_only_the_proxies() {
        assert_eq!(reached("proxy"), ["/git/x", "/cf/client/v4/x"]);
    }

    #[test]
    fn an_access_token_reaches_only_mcp() {
        assert_eq!(reached("access"), ["/mcp"]);
    }

    #[test]
    fn other_kinds_reach_nothing() {
        assert!(reached("refresh").is_empty());
        assert!(reached("").is_empty());
        assert!(reached("Proxy").is_empty());
    }

    #[test]
    fn the_proxy_host_serves_only_the_proxies() {
        let p = "proxy.pigeon.dev";
        for (host, path, served) in [
            (p, "/git/ns/r.git/info/refs", true),
            (p, "/cf/client/v4/accounts", true),
            (p, "/health", true),
            ("PROXY.pigeon.dev", "/git/x", true),
            (p, "/mcp", false),
            (p, "/tokens", false),
            (p, "/oauth/token", false),
            (p, "/.well-known/oauth-protected-resource", false),
            (p, "/", false),
            ("platform.pigeon.dev", "/git/ns/r.git/info/refs", false),
            ("platform.pigeon.dev", "/cf/client/v4/x", false),
            ("platform.pigeon.dev", "/mcp", true),
            ("platform.pigeon.dev", "/tokens", true),
            ("platform.pigeon.dev", "/health", true),
        ] {
            assert_eq!(host_serves(host, p, path), served, "{host}{path}");
        }
    }

    #[test]
    fn approvals_are_bound_to_one_credential() {
        let a = issue_purpose(&hash("env"), 30);
        assert_ne!(a, issue_purpose(&hash("env2"), 30));
        assert_ne!(a, issue_purpose(&hash("env"), 31));
        assert_ne!(
            revoke_purpose("aaaaaaaaaaaa"),
            revoke_purpose("aaaaaaaaaaab")
        );
    }

    #[test]
    fn labels_are_trimmed_short_and_printable() {
        for l in ["", "   ", &"a".repeat(65), "a\nb", "a\u{7}"] {
            assert!(label(l).is_err(), "{l:?}");
        }
        assert_eq!(label(" cloud env ").as_deref(), Ok("cloud env"));
        assert!(label(&"é".repeat(64)).is_ok());
    }

    #[test]
    fn days_default_and_stay_in_range() {
        assert_eq!(days(&Value::Null), Ok(DEFAULT_DAYS));
        assert_eq!(days(&json!(1)), Ok(1));
        assert_eq!(days(&json!(90)), Ok(90));
        for v in [json!(0), json!(91), json!(-1), json!(1.5), json!("30")] {
            assert!(days(&v).is_err(), "{v}");
        }
    }

    #[test]
    fn ids_are_twelve_base64url_characters() {
        assert_eq!(id("abcdefghij_-"), Ok("abcdefghij_-"));
        for i in [
            "abcdefghijk",
            "abcdefghijklm",
            "abcdefghij+/",
            "abcdefghij%2",
        ] {
            assert!(id(i).is_err(), "{i}");
        }
    }

    #[test]
    fn audit_lines_hold_no_query_and_name_the_caller() {
        let a = Audit {
            route: "git",
            method: "GET",
            path: "/git/x/info/refs?service=git-upload-pack",
            outcome: Outcome::Forwarded,
            status: Some(200),
            why: None,
        };
        let none: Value = serde_json::from_str(&audit_line(None, &a)).unwrap();
        assert_eq!(none["path"], "/git/x/info/refs");
        assert!(none["caller"].is_null() && none["token"].is_null());
        assert_eq!(none["outcome"], "forwarded");
        let c = ProxyCaller {
            id: "abcdefghijkl".into(),
            label: "env".into(),
            person: "Ana".into(),
        };
        let some: Value = serde_json::from_str(&audit_line(Some(&c), &a)).unwrap();
        assert_eq!(some["caller"], "env (Ana)");
        assert_eq!(some["token"], "abcdefghijkl");
        assert_eq!(some["status"], 200);
    }
}

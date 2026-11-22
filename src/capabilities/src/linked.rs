//! Linked accounts: another Cloudflare account a person links to an instance with Cloudflare's
//! own OAuth consent screen, so no token passes through a person's hands or a chat.
//!
//! - `linked_setup` registers pigeon's OAuth client in the platform's own account, once, and
//!   keeps its secret and the key that seals linked tokens in the Secrets Store. The Worker reads
//!   them through bindings a later landing adds (`CF_OAUTH_CLIENT_SECRET`, `LINK_KEY`); until
//!   then every link answers "not set up".
//! - On the console, Link (the signed-in person, same origin) sends the browser to Cloudflare;
//!   the callback checks the state it was given, exchanges the code, checks the granted account
//!   against the instance's record, and keeps the tokens sealed with AES-GCM in the Directory.
//! - Unlink revokes the grant and forgets the tokens, with the person's passkey.
//! - [`account_for`] hands other capabilities a linked account's `Account`, refreshing its
//!   access token first when it is about to expire.

use pigeon_core::cloudflare::call;
use pigeon_core::cloudflare_oauth::{self as oauth, Client, Tokens};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::tokens::{Policy, Scope, Scoped};
use pigeon_core::{Account, crypto};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::instances::{self, record::AccountRef};
use crate::passkeys::Passkeys;
use crate::users::directory;
use crate::{Call, Capability, Context, Serve, arg, unknown};

/// The Secrets Store names (and binding names) this capability keeps.
pub const CLIENT_SECRET: &str = "CF_OAUTH_CLIENT_SECRET";
pub const LINK_KEY: &str = "LINK_KEY";
/// The Directory key holding the registered client's id.
const CLIENT_KEY: &str = "linked:oauth_client";
/// How long a Link's state is good for.
const STATE_TTL_MS: i64 = 10 * 60_000;
/// How long a link is kept: until unlinked, in practice.
const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;

/// What the client asks for: the API token permission names pigeon needs in a linked account,
/// matched against Cloudflare's scope list to find their scope IDs.
pub const PERMISSIONS: [&str; 6] = [
    "Account Settings Read",
    "Zone Read",
    "DNS Write",
    "Workers Scripts Write",
    "Workers Routes Write",
    "Workers R2 Storage Write",
];

/// The capability.
pub struct Linked {
    /// The platform's host, where the callback is.
    pub host: String,
    pub passkeys: Passkeys,
}

/// A link as kept in the Directory: the account, who linked it, and the sealed tokens.
#[derive(Serialize, Deserialize, Clone)]
pub struct Link {
    pub instance: String,
    pub account_id: String,
    pub account_name: String,
    pub linked_by: String,
    pub linked_ms: i64,
    pub expires_at_ms: f64,
    pub scope: Option<String>,
    /// `Tokens`, as JSON, sealed with `LINK_KEY` and the instance id as associated data.
    pub sealed: String,
    /// The last time a refresh worked, and the last refusal, if any.
    pub refreshed_ms: Option<i64>,
    pub failed: Option<String>,
}

/// What the console shows of a link: never a token.
pub fn status(link: Option<&Link>, now_ms: f64) -> Value {
    match link {
        None => json!({ "status": "not linked" }),
        Some(l) if l.failed.is_some() => json!({
            "status": "refresh failed", "account_id": l.account_id, "why": l.failed,
        }),
        Some(l) => json!({
            "status": "linked", "account_id": l.account_id, "account_name": l.account_name,
            "linked_by": l.linked_by, "linked_ms": l.linked_ms,
            "access_expires_ms": l.expires_at_ms, "scope": l.scope,
            "fresh": l.expires_at_ms > now_ms,
        }),
    }
}

/// The scope IDs for [`PERMISSIONS`] in Cloudflare's scope list (`{id, name}` entries), and the
/// names it does not have. Answers in the order of [`PERMISSIONS`].
pub fn scope_ids(listed: &Value) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut missing = Vec::new();
    for want in PERMISSIONS {
        let found = listed.as_array().and_then(|all| {
            all.iter()
                .find(|s| {
                    s["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(want))
                })
                .and_then(|s| s["id"].as_str())
        });
        match found {
            Some(id) => ids.push(id.to_string()),
            None => missing.push(want.to_string()),
        }
    }
    (ids, missing)
}

/// What the planned per-instance data Worker (docs/plans/data-worker.md) would need a linked
/// account to grant, as words to look for in Cloudflare's scope names.
pub const DATA_NEEDS: [&str; 9] = [
    "Pipelines",
    "Data Catalog",
    "Email Routing",
    "Zone Settings",
    "Workers AI",
    "API Tokens",
    "Vectorize",
    "Queues",
    "D1",
];

/// For each of `needs`, the scopes in Cloudflare's list (`{id, name, ...}` entries) whose name
/// contains it, ignoring case: an empty list means no OAuth scope offers it. Pure.
pub fn needs_found(listed: &Value, needs: &[&str]) -> Value {
    let all = listed.as_array().cloned().unwrap_or_default();
    let mut out = serde_json::Map::new();
    for need in needs {
        let want = need.to_ascii_lowercase();
        let found: Vec<Value> = all
            .iter()
            .filter(|s| {
                s["name"]
                    .as_str()
                    .is_some_and(|n| n.to_ascii_lowercase().contains(&want))
            })
            .map(|s| json!({ "id": s["id"], "name": s["name"] }))
            .collect();
        out.insert((*need).to_string(), Value::Array(found));
    }
    Value::Object(out)
}

/// Which granted account a link may take for an instance whose record names `wanted`: that one
/// when the grant has it; the only one when the record names none; otherwise why not.
pub fn choose_account(
    wanted: Option<&str>,
    granted: &[(String, String)],
) -> std::result::Result<(String, String), String> {
    match wanted {
        Some(id) => granted
            .iter()
            .find(|(g, _)| g == id)
            .cloned()
            .ok_or_else(|| format!("the grant does not include account {id}, which the instance names")),
        None => match granted {
            [one] => Ok(one.clone()),
            [] => Err("the grant includes no account".into()),
            _ => Err("the grant includes several accounts: set account.account_id in the instance's record to choose one".into()),
        },
    }
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now_ms() -> f64 {
    worker::js_sys::Date::now()
}

fn key(instance: &str) -> String {
    format!("link:{instance}")
}

async fn setting(env: &Env, key: &str) -> Result<Option<String>> {
    Ok(
        match directory(env, "setting.get", json!({ "key": key })).await? {
            Value::String(s) => Some(s),
            _ => None,
        },
    )
}

async fn keep(env: &Env, key: &str, value: &str, ttl_ms: i64) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": value, "ttl_ms": ttl_ms }),
    )
    .await?;
    Ok(())
}

/// The link for `instance`, if there is one.
pub async fn link(env: &Env, instance: &str) -> Result<Option<Link>> {
    Ok(match setting(env, &key(instance)).await? {
        Some(text) => Some(serde_json::from_str(&text)?),
        None => None,
    })
}

/// A secret this capability keeps, through its binding; `None` until the binding exists.
async fn secret(env: &Env, name: &str) -> Option<String> {
    let binding = env.secret_store(name).ok()?;
    secrets_store::read(&binding).await.ok().flatten()
}

async fn sealing_key(env: &Env) -> Result<[u8; 32]> {
    let text = secret(env, LINK_KEY)
        .await
        .ok_or_else(|| bad("linking is not set up yet: LINK_KEY is not bound"))?;
    crypto::key_from(&text).ok_or_else(|| bad("LINK_KEY is not a 32-byte key"))
}

async fn client(env: &Env, host: &str) -> Result<(Client, String)> {
    let client_id = setting(env, CLIENT_KEY)
        .await?
        .ok_or_else(|| bad("linking is not set up yet: run linked_setup"))?;
    let secret = secret(env, CLIENT_SECRET)
        .await
        .ok_or_else(|| bad("linking is not set up yet: CF_OAUTH_CLIENT_SECRET is not bound"))?;
    let scopes = setting(env, "linked:oauth_scopes")
        .await?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default();
    Ok((
        Client {
            client_id,
            redirect_uri: format!("https://{host}/linked/callback"),
            scopes,
        },
        secret,
    ))
}

/// A linked account as an `Account` whose calls use its granted access token, refreshed first
/// when under five minutes remain. The new tokens are kept before the access token is used,
/// since a refresh token may work only once.
pub async fn account_for(env: &Env, host: &str, instance: &str) -> Result<Account> {
    let mut l = link(env, instance)
        .await?
        .ok_or_else(|| bad(&format!("instance {instance} is not linked")))?;
    let k = sealing_key(env).await?;
    let opened = crypto::open(&k, &l.sealed, instance.as_bytes()).await?;
    let mut tokens: Tokens = serde_json::from_slice(&opened)?;
    if oauth::needs_refresh(&tokens, now_ms()) {
        let (c, secret) = client(env, host).await?;
        let refresh = tokens
            .refresh
            .clone()
            .ok_or_else(|| bad("the link has no refresh token: link it again"))?;
        match oauth::refresh(&c.client_id, &secret, &refresh, now_ms()).await {
            Ok(t) => {
                tokens = t;
                l.sealed =
                    crypto::seal(&k, &serde_json::to_vec(&tokens)?, instance.as_bytes()).await?;
                l.expires_at_ms = tokens.expires_at_ms;
                l.refreshed_ms = Some(now_ms() as i64);
                l.failed = None;
                keep(env, &key(instance), &serde_json::to_string(&l)?, KEPT_MS).await?;
            }
            Err(e) => {
                l.failed = Some(e.to_string());
                keep(env, &key(instance), &serde_json::to_string(&l)?, KEPT_MS).await?;
                return Err(bad(&format!(
                    "the link to {instance} could not refresh: link it again ({e})"
                )));
            }
        }
    }
    Ok(Account {
        id: l.account_id,
        parent: String::new(),
        secrets_store: String::new(),
        bearer: Some(tokens.access),
    })
}

impl Linked {
    /// Registers pigeon's OAuth client in the platform's own account, once, and keeps its secret
    /// and a fresh sealing key in the Secrets Store. Safe to run twice: an existing client is
    /// kept, and the key is never replaced (that would make every link unreadable).
    async fn setup(&self, cx: &Context<'_>) -> Result<Value> {
        if let Some(id) = setting(cx.env, CLIENT_KEY).await? {
            return Ok(json!({ "client_id": id, "done": "already set up" }));
        }
        let account = cx.account;
        let token = Scoped::open(
            account,
            "oauth-client",
            vec![Policy::new(Scope::Account, &["OAuth Client Write"])],
        )
        .await?;
        let made = async {
            let listed = call(token.value(), Method::Get, "/oauth/scopes", None).await?;
            let (scopes, missing) = scope_ids(&listed);
            if !missing.is_empty() {
                return Err(bad(&format!(
                    "Cloudflare lists no OAuth scope for: {}",
                    missing.join(", ")
                )));
            }
            let body = json!({
                "client_name": "pigeon",
                "redirect_uris": [format!("https://{}/linked/callback", self.host)],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "client_secret_basic",
                "scopes": scopes,
            });
            let path = format!("/accounts/{}/oauth_clients", account.id);
            let v = call(token.value(), Method::Post, &path, Some(&body)).await?;
            Ok((v, scopes))
        }
        .await;
        let (made, scopes) = token.finish(made).await?;
        let id = made["client_id"]
            .as_str()
            .or_else(|| made["id"].as_str())
            .ok_or_else(|| bad("the new OAuth client came back without its id"))?
            .to_string();
        let client_secret = made["client_secret"]
            .as_str()
            .ok_or_else(|| bad("the new OAuth client came back without its secret"))?;
        let writer = Writer::open(account).await?;
        let key = crypto_key()?;
        let written = async {
            writer
                .put_scoped(
                    CLIENT_SECRET,
                    client_secret,
                    "pigeon's OAuth client, by linked_setup",
                    &[secrets_store::WORKERS],
                )
                .await?;
            if !writer.list().await?.iter().any(|s| s.name == LINK_KEY) {
                writer
                    .put_scoped(
                        LINK_KEY,
                        &key,
                        "seals linked accounts' tokens, by linked_setup",
                        &[secrets_store::WORKERS],
                    )
                    .await?;
            }
            Ok(())
        }
        .await;
        writer.finish(written).await?;
        keep(cx.env, CLIENT_KEY, &id, KEPT_MS).await?;
        keep(
            cx.env,
            "linked:oauth_scopes",
            &serde_json::to_string(&scopes)?,
            KEPT_MS,
        )
        .await?;
        Ok(json!({
            "client_id": id,
            "scopes": scopes,
            "next": "land the CF_OAUTH_CLIENT_SECRET and LINK_KEY bindings in wrangler.toml",
        }))
    }

    /// Starts linking `instance`: its record must be a linked one; answers Cloudflare's consent
    /// URL with a state that only this callback, within ten minutes, will take.
    async fn begin(&self, env: &Env, body: &Value, person: &str) -> Result<Value> {
        let instance = arg(body, "instance")?;
        let record = instances::one(env, instance)
            .await?
            .ok_or_else(|| bad("no such instance"))?;
        if !matches!(record.account, AccountRef::Linked { .. }) {
            return Err(bad(
                "this instance runs in the platform's own account: there is nothing to link",
            ));
        }
        let (c, _) = client(env, &self.host).await?;
        let verifier = oauth::verifier()?;
        let state = pigeon_core::webauthn::random()?;
        let pending = json!({ "instance": instance, "verifier": verifier, "person": person });
        keep(
            env,
            &format!("linked:state:{state}"),
            &pending.to_string(),
            STATE_TTL_MS,
        )
        .await?;
        Ok(json!({ "url": oauth::authorize_url(&c, &state, &oauth::challenge(&verifier)) }))
    }

    /// The consent screen's redirect: checks the state, exchanges the code, checks the account
    /// against the record, and keeps the link. Answers a page that says what happened.
    async fn callback(&self, env: &Env, query: &str) -> Result<String> {
        let (code, state) = oauth::callback_params(query).map_err(|e| bad(&e))?;
        let skey = format!("linked:state:{state}");
        let pending: Value = serde_json::from_str(
            &setting(env, &skey)
                .await?
                .ok_or_else(|| bad("this link has expired or was used: start again"))?,
        )?;
        keep(env, &skey, "", 1).await?;
        let instance = pending["instance"].as_str().unwrap_or_default();
        let record = instances::one(env, instance)
            .await?
            .ok_or_else(|| bad("no such instance"))?;
        let wanted = match &record.account {
            AccountRef::Linked { account_id } => account_id.clone(),
            AccountRef::Home => {
                return Err(bad("this instance runs in the platform's own account"));
            }
        };
        let (c, secret) = client(env, &self.host).await?;
        let verifier = pending["verifier"].as_str().unwrap_or_default();
        let tokens = oauth::exchange(&c, &secret, &code, verifier, now_ms()).await?;
        let granted = oauth::granted_accounts(&tokens.access).await?;
        let (account_id, account_name) = match choose_account(wanted.as_deref(), &granted) {
            Ok(a) => a,
            Err(why) => {
                if let Some(r) = &tokens.refresh {
                    let _ = oauth::revoke(&c.client_id, &secret, r).await;
                }
                return Err(bad(&why));
            }
        };
        let k = sealing_key(env).await?;
        let sealed = crypto::seal(&k, &serde_json::to_vec(&tokens)?, instance.as_bytes()).await?;
        let l = Link {
            instance: instance.into(),
            account_id: account_id.clone(),
            account_name: account_name.clone(),
            linked_by: pending["person"].as_str().unwrap_or_default().into(),
            linked_ms: now_ms() as i64,
            expires_at_ms: tokens.expires_at_ms,
            scope: tokens.scope.clone(),
            sealed,
            refreshed_ms: None,
            failed: None,
        };
        keep(env, &key(instance), &serde_json::to_string(&l)?, KEPT_MS).await?;
        Ok(format!(
            "{instance} is linked to the Cloudflare account {account_name} ({account_id})."
        ))
    }

    /// Unlinks `instance` with the person's passkey: revokes the grant and forgets the tokens.
    async fn unlink(&self, cx: &Context<'_>, body: &Value, begin: bool) -> Result<Value> {
        let instance = arg(body, "instance")?;
        let purpose = format!("linked:unlink:{instance}");
        if begin {
            return self.passkeys.sign_in_begin(cx.env, &purpose, None).await;
        }
        self.passkeys
            .sign_in_finish(cx.env, &purpose, body, None)
            .await?;
        let Some(l) = link(cx.env, instance).await? else {
            return Ok(json!({ "message": format!("{instance} was not linked.") }));
        };
        let revoked = async {
            let k = sealing_key(cx.env).await?;
            let tokens: Tokens =
                serde_json::from_slice(&crypto::open(&k, &l.sealed, instance.as_bytes()).await?)?;
            let (c, secret) = client(cx.env, &self.host).await?;
            if let Some(r) = tokens.refresh {
                oauth::revoke(&c.client_id, &secret, &r).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        keep(cx.env, &key(instance), "", 1).await?;
        Ok(json!({ "message": match revoked {
            Ok(()) => format!("{instance} is unlinked and its grant revoked. What pigeon made in that account stays until you remove it."),
            Err(e) => format!("{instance} is unlinked, but the grant could not be revoked ({e}): revoke pigeon under Profile → Access Management → Connected Applications in Cloudflare."),
        } }))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let path = req.path();
        if req.method() == Method::Get && path == "/linked/callback" {
            let query = req.url()?.query().unwrap_or_default().to_string();
            let text = match self.callback(cx.env, &query).await {
                Ok(done) => format!("{done} You can close this page."),
                Err(e) => format!("Linking failed: {e}"),
            };
            let headers = Headers::new();
            headers.set("Content-Type", "text/plain; charset=utf-8")?;
            headers.set("Cache-Control", "no-store")?;
            return Ok(Response::ok(text)?.with_headers(headers));
        }
        if req.method() != Method::Post {
            return Response::error("not found", 404);
        }
        let host = req.url()?.host_str().unwrap_or_default().to_string();
        let origin = req.headers().get("Origin")?;
        if !crate::console::session::same_origin(origin.as_deref(), &host) {
            return Response::error("forbidden", 403);
        }
        let Some(person) = crate::console::signed_person(cx.env, &req).await? else {
            return Response::error("sign in to the console first", 401);
        };
        let body: Value = req.json().await.unwrap_or(Value::Null);
        let answer = match path.as_str() {
            "/linked/begin" => self.begin(cx.env, &body, &person.name).await,
            "/linked/unlink/begin" => self.unlink(cx, &body, true).await,
            "/linked/unlink/finish" => self.unlink(cx, &body, false).await,
            _ => return Response::error("not found", 404),
        };
        let headers = Headers::new();
        headers.set("Cache-Control", "no-store")?;
        match answer {
            Ok(v) => Ok(Response::from_json(&v)?.with_headers(headers)),
            Err(e) => Ok(Response::from_json(&json!({ "error": e.to_string() }))?
                .with_status(400)
                .with_headers(headers)),
        }
    }
}

/// A new 32-byte sealing key, base64url.
fn crypto_key() -> Result<String> {
    pigeon_core::webauthn::random()
}

impl Linked {
    /// `linked_check`: refreshes the link's access token when due (or when asked), then makes
    /// only GET calls with it: the account, and each zone the instance's record names. Answers
    /// what Cloudflare said, never a token.
    async fn check(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let id = arg(args, "instance")?;
        let instance = instances::one(cx.env, id)
            .await?
            .ok_or_else(|| bad(&format!("there is no instance {id}")))?;
        if args["refresh"].as_bool() == Some(true) {
            force_refresh_due(cx.env, id).await?;
        }
        let before = link(cx.env, id).await?.and_then(|l| l.refreshed_ms);
        let account = account_for(cx.env, &self.host, id).await?;
        let after = link(cx.env, id).await?;
        let token = account
            .bearer
            .as_deref()
            .ok_or_else(|| bad("the linked account has no access token"))?;
        let acct = call(
            token,
            Method::Get,
            &format!("/accounts/{}", account.id),
            None,
        )
        .await?;
        let mut zones = Vec::new();
        for z in &instance.zones {
            let path = format!("/zones?account.id={}&name={z}", account.id);
            let found = call(token, Method::Get, &path, None).await?;
            let first = found.as_array().and_then(|a| a.first()).cloned();
            zones.push(match first {
                Some(zone) => json!({
                    "name": z, "id": zone["id"], "status": zone["status"],
                    "name_servers": zone["name_servers"],
                }),
                None => json!({ "name": z, "found": false }),
            });
        }
        Ok(json!({
            "instance": id,
            "account": { "id": acct["id"], "name": acct["name"] },
            "zones": zones,
            "refreshed": after.as_ref().and_then(|l| l.refreshed_ms) != before,
            "access_expires_ms": after.as_ref().map(|l| l.expires_at_ms),
        }))
    }
}

/// Marks a link's access token as expired, so the next [`account_for`] refreshes it.
async fn force_refresh_due(env: &Env, instance: &str) -> Result<()> {
    let mut l = link(env, instance)
        .await?
        .ok_or_else(|| bad(&format!("instance {instance} is not linked")))?;
    let k = sealing_key(env).await?;
    let opened = crypto::open(&k, &l.sealed, instance.as_bytes()).await?;
    let mut tokens: Tokens = serde_json::from_slice(&opened)?;
    if tokens.refresh.is_none() {
        return Err(bad(
            "the link has no refresh token, so it cannot be refreshed: link it again",
        ));
    }
    tokens.expires_at_ms = 0.0;
    l.sealed = crypto::seal(&k, &serde_json::to_vec(&tokens)?, instance.as_bytes()).await?;
    keep(env, &key(instance), &serde_json::to_string(&l)?, KEPT_MS).await
}

impl Capability for Linked {
    fn name(&self) -> &'static str {
        "linked"
    }

    fn footprint(&self) -> crate::Footprint {
        crate::Footprint {
            externals: vec![crate::External::new(
                "Linked Cloudflare accounts",
                "OAuth",
                "other accounts linked to an instance with Cloudflare's OAuth consent",
            )],
            ..crate::Footprint::default()
        }
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "linked_setup",
                "Register pigeon's OAuth client in the platform's own account, once, so a person \
                 can link another Cloudflare account from the console. Keeps the client secret \
                 and the sealing key in the Secrets Store; safe to run twice.",
            ),
            Tool {
                name: "linked_check",
                description: "Check one instance's link against Cloudflare, read-only: refresh \
                              its access token if it is due (or now, with `refresh`), then read \
                              the linked account and the instance's zones with it. Never a token.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "instance": { "type": "string", "description": "The instance's id, such as `noisypigeon`." },
                        "refresh": { "type": "boolean", "description": "Refresh the access token now, even if it is not due." }
                    },
                    "required": ["instance"]
                }),
            },
            Tool::bare(
                "linked_scopes",
                "Cloudflare's OAuth scope list, read-only: every scope a linked account could \
                 grant (id, name, category), and which of the data Worker's needs \
                 (docs/plans/data-worker.md) it offers. Writes nothing; touches no OAuth client \
                 and no link.",
            ),
            Tool::bare(
                "linked_status",
                "Each instance's link: the account, who linked it, when the access token \
                 expires, and whether refreshing failed. Never a token.",
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "linked_setup" => self.setup(cx).await,
                "linked_check" => self.check(cx, args).await,
                "linked_scopes" => {
                    // A short-lived token, as linked_setup's, used only to read the list.
                    let token = Scoped::open(
                        cx.account,
                        "oauth-scopes",
                        vec![Policy::new(Scope::Account, &["OAuth Client Write"])],
                    )
                    .await?;
                    let listed = call(token.value(), Method::Get, "/oauth/scopes", None).await;
                    let listed = token.finish(listed).await?;
                    let scopes: Vec<Value> = listed
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|s| json!({ "id": s["id"], "name": s["name"], "category": s["category"] }))
                        .collect();
                    Ok(json!({
                        "count": scopes.len(),
                        "data_needs": needs_found(&listed, &DATA_NEEDS),
                        "scopes": scopes,
                    }))
                }
                "linked_status" => {
                    let mut out = serde_json::Map::new();
                    for i in instances::all(cx.env).await? {
                        let l = link(cx.env, &i.id).await?;
                        out.insert(i.id.clone(), status(l.as_ref(), now_ms()));
                    }
                    Ok(Value::Object(out))
                }
                other => Err(unknown(other)),
            }
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/linked/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

//! OAuth for the MCP endpoint, as the MCP authorization spec asks, so any MCP client can sign
//! in: it finds the server from `/.well-known/`, registers itself, and sends the person to
//! `/oauth/authorize`, where they sign in with a passkey and allow it. Each registered client is
//! a service user; allowing it links it to the person, so it acts for them. Its tokens are
//! bearer tokens, kept only as hashes. A service user can also be issued a long-lived token
//! directly, for agents with no browser.

use pigeon_core::mcp::Tool;
use pigeon_core::oauth::{self, encode, form, hash, pkce, redirect_allowed};
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::passkeys::{Action, Passkeys, html};
use crate::users::{Caller, User, directory};
use crate::{Call, Capability, Context, Serve, arg, unknown};

/// How long tokens live, in milliseconds.
const ACCESS_TTL_MS: i64 = 60 * 60 * 1000;
const REFRESH_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const ISSUED_DEFAULT_DAYS: i64 = 90;
const ISSUED_MAX_DAYS: i64 = 365;

/// OAuth on the host whose `/mcp` it protects, signing people in with its passkeys.
pub struct OAuth {
    pub host: String,
    pub passkeys: Passkeys,
}

/// The calling user of bearer token `token`, if it is a live access token.
pub async fn caller(env: &Env, token: &str) -> Result<Caller> {
    let row = directory(
        env,
        "oauth.token_get",
        json!({ "hash": hash(token), "kind": "access" }),
    )
    .await?;
    let user = row["service_user_id"]
        .as_str()
        .ok_or_else(|| Error::RustError("the bearer token is unknown or has expired".into()))?;
    let caller = directory(env, "caller.of", json!({ "user_id": user })).await?;
    serde_json::from_value(caller)
        .map_err(|_| Error::RustError("the bearer token's user is gone".into()))
}

/// Form field `k`, or empty.
fn field<'a>(fields: &'a [(String, String)], k: &str) -> &'a str {
    fields
        .iter()
        .find(|(key, _)| key == k)
        .map_or("", |(_, v)| v.as_str())
}

/// An OAuth error response: `code` such as `invalid_grant`.
fn oauth_error(status: u16, code: &str, description: &str) -> Result<Response> {
    let res = Response::from_json(&json!({ "error": code, "error_description": description }))?;
    Ok(cors(res.with_status(status)))
}

/// Lets any origin read the metadata, register and exchange tokens.
fn cors(res: Response) -> Response {
    let headers = res.headers().clone();
    let _ = headers.set("Access-Control-Allow-Origin", "*");
    let _ = headers.set(
        "Access-Control-Allow-Headers",
        "Authorization, Content-Type, MCP-Protocol-Version",
    );
    let _ = headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    let _ = headers.set("Cache-Control", "no-store");
    res.with_headers(headers)
}

impl OAuth {
    fn issuer(&self) -> String {
        format!("https://{}", self.host)
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path().to_string();
        let query = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        let issuer = self.issuer();
        match (req.method(), path.as_str()) {
            (Method::Options, _) => Ok(cors(Response::empty()?.with_status(204))),
            (Method::Get, p) if p.starts_with("/.well-known/oauth-protected-resource") => Ok(cors(
                Response::from_json(&oauth::resource_metadata(&format!("{issuer}/mcp"), &issuer))?,
            )),
            // The iOS app's associated domain (docs/plans/ios.md, phase 5): JSON, no redirect,
            // 404 while APPLE_TEAM_ID is unset.
            (Method::Get, "/.well-known/apple-app-site-association") => {
                match crate::apple::aasa(cx.env).await {
                    Some(doc) => {
                        let headers = Headers::new();
                        headers.set("Content-Type", "application/json")?;
                        headers.set("Cache-Control", "public, max-age=3600")?;
                        Ok(Response::from_json(&doc)?.with_headers(headers))
                    }
                    None => Response::error("not found", 404),
                }
            }
            (Method::Get, p) if p.starts_with("/.well-known/oauth-authorization-server") => {
                Ok(cors(Response::from_json(&oauth::server_metadata(&issuer))?))
            }
            (Method::Post, "/oauth/register") => {
                let body: Value = req.json().await.unwrap_or(Value::Null);
                self.register(cx.env, &body).await
            }
            (Method::Get, "/oauth/authorize") => {
                let page = match self.authorize(cx.env, &query).await {
                    Ok((request, client)) => self.passkeys.page(
                        "Allow access",
                        &format!(
                            "<strong>{}</strong> asks to act for you on {}. Sign in with your passkey to allow it.",
                            pigeon_core::design::escape(&client),
                            pigeon_core::design::escape(&self.passkeys.site_name)
                        ),
                        Some(Action {
                            base: "/oauth/authorize",
                            ceremony: "get",
                            field: "request",
                            value: &request,
                            label: "Sign in with passkey and allow",
                        }),
                    ),
                    Err(e) => self.passkeys.page(
                        "Sign-in not usable",
                        &pigeon_core::design::escape(&e.to_string()),
                        None,
                    ),
                };
                html(&page)
            }
            (Method::Post, "/oauth/authorize/begin" | "/oauth/authorize/finish") => {
                let body: Value = req.json().await.unwrap_or(Value::Null);
                let answer = if path.ends_with("/begin") {
                    self.authorize_begin(cx.env, &body).await
                } else {
                    self.authorize_finish(cx.env, &body).await
                };
                match answer {
                    Ok(v) => Response::from_json(&v),
                    Err(e) => {
                        Ok(Response::from_json(&json!({ "error": e.to_string() }))?
                            .with_status(400))
                    }
                }
            }
            (Method::Post, "/oauth/token") => {
                let body = req.text().await?;
                let fields = form(&body);
                match field(&fields, "grant_type") {
                    "authorization_code" => self.exchange_code(cx.env, &fields).await,
                    "refresh_token" => self.exchange_refresh(cx.env, &fields).await,
                    other => oauth_error(
                        400,
                        "unsupported_grant_type",
                        &format!("grant type `{other}` is not supported"),
                    ),
                }
            }
            _ => Response::error("not found", 404),
        }
    }

    /// Registers a client (RFC 7591): it becomes a service user, named as it asks.
    async fn register(&self, env: &Env, body: &Value) -> Result<Response> {
        let uris: Vec<String> = body["redirect_uris"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|u| u.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if uris.is_empty() || !uris.iter().all(|u| redirect_allowed(u)) {
            return oauth_error(
                400,
                "invalid_redirect_uri",
                "redirect_uris must be HTTPS, or HTTP to the loopback address",
            );
        }
        let name = body["client_name"].as_str().unwrap_or("MCP client");
        let made = directory(
            env,
            "oauth.client_add",
            json!({ "name": name, "redirect_uris": uris }),
        )
        .await?;
        let res = Response::from_json(&json!({
            "client_id": made["id"],
            "client_name": name,
            "redirect_uris": uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))?;
        Ok(cors(res.with_status(201)))
    }

    /// Checks an authorization request and keeps it while the person signs in: its ID and
    /// the client's name.
    async fn authorize(
        &self,
        env: &Env,
        query: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(String, String)> {
        let bad = |why: &str| Error::RustError(why.to_string());
        let client_id = query("client_id").ok_or_else(|| bad("the request names no client"))?;
        let client = directory(env, "oauth.client_get", json!({ "id": client_id })).await?;
        if client.is_null() {
            return Err(bad("this client is not registered"));
        }
        let uris: Vec<String> =
            serde_json::from_str(client["redirect_uris"].as_str().unwrap_or("[]"))?;
        let redirect = query("redirect_uri").ok_or_else(|| bad("the request names no redirect"))?;
        if !uris.contains(&redirect) {
            return Err(bad("this redirect is not one the client registered"));
        }
        if query("response_type").as_deref() != Some("code") {
            return Err(bad("only the code response type is supported"));
        }
        if query("code_challenge_method").as_deref() != Some("S256") {
            return Err(bad("a PKCE S256 code challenge is required"));
        }
        let challenge =
            query("code_challenge").ok_or_else(|| bad("the request has no code challenge"))?;
        let id = directory(
            env,
            "oauth.request_add",
            json!({
                "client_id": client_id,
                "redirect_uri": redirect,
                "challenge": challenge,
                "state": query("state"),
            }),
        )
        .await?;
        Ok((
            id.as_str().unwrap_or_default().to_string(),
            client["name"].as_str().unwrap_or_default().to_string(),
        ))
    }

    async fn authorize_begin(&self, env: &Env, body: &Value) -> Result<Value> {
        let request = arg(body, "request")?;
        directory(env, "oauth.request_get", json!({ "id": request })).await?;
        self.passkeys
            .sign_in_begin(env, &format!("oauth:{request}"), None)
            .await
    }

    /// The person signed in: their passkey's standard user is linked to the client's service
    /// user, and the browser goes back to the client with a one-time code.
    async fn authorize_finish(&self, env: &Env, body: &Value) -> Result<Value> {
        let request = arg(body, "request")?;
        let r = directory(env, "oauth.request_get", json!({ "id": request })).await?;
        let key = self
            .passkeys
            .sign_in_finish(env, &format!("oauth:{request}"), body, None)
            .await?;
        let person: Option<User> = serde_json::from_value(
            directory(env, "users.get", json!({ "id": key.user_id })).await?,
        )?;
        let person = person
            .filter(|u| u.kind == "standard")
            .ok_or_else(|| Error::RustError("only a person can allow a client".into()))?;
        let client = directory(env, "oauth.client_get", json!({ "id": r["client_id"] })).await?;
        directory(
            env,
            "link.set",
            json!({
                "service_user_id": client["service_user_id"],
                "standard_user_id": person.id,
                "passkey_id": key.id,
            }),
        )
        .await?;
        let (code, code_hash) = oauth::token("pgn_code_")?;
        directory(
            env,
            "oauth.code_add",
            json!({
                "hash": code_hash,
                "client_id": r["client_id"],
                "redirect_uri": r["redirect_uri"],
                "challenge": r["challenge"],
            }),
        )
        .await?;
        directory(env, "oauth.request_remove", json!({ "id": request })).await?;
        let redirect = r["redirect_uri"].as_str().unwrap_or_default();
        let sep = if redirect.contains('?') { '&' } else { '?' };
        let mut to = format!("{redirect}{sep}code={}", encode(&code));
        if let Some(state) = r["state"].as_str() {
            to.push_str(&format!("&state={}", encode(state)));
        }
        Ok(json!({ "redirect": to }))
    }

    async fn exchange_code(&self, env: &Env, fields: &[(String, String)]) -> Result<Response> {
        let Ok(code) = directory(
            env,
            "oauth.code_take",
            json!({ "hash": hash(field(fields, "code")) }),
        )
        .await
        else {
            return oauth_error(400, "invalid_grant", "the code is used or has expired");
        };
        if code["client_id"].as_str() != Some(field(fields, "client_id"))
            || code["redirect_uri"].as_str() != Some(field(fields, "redirect_uri"))
        {
            return oauth_error(
                400,
                "invalid_grant",
                "the code was issued for another client",
            );
        }
        if !pkce(
            field(fields, "code_verifier"),
            code["challenge"].as_str().unwrap_or_default(),
        ) {
            return oauth_error(400, "invalid_grant", "the code verifier does not match");
        }
        let client = directory(
            env,
            "oauth.client_get",
            json!({ "id": field(fields, "client_id") }),
        )
        .await?;
        self.issue_pair(
            env,
            field(fields, "client_id"),
            client["service_user_id"].as_str().unwrap_or_default(),
        )
        .await
    }

    async fn exchange_refresh(&self, env: &Env, fields: &[(String, String)]) -> Result<Response> {
        let old = hash(field(fields, "refresh_token"));
        let row = directory(
            env,
            "oauth.token_get",
            json!({ "hash": old, "kind": "refresh" }),
        )
        .await?;
        if row.is_null() || row["client_id"].as_str() != Some(field(fields, "client_id")) {
            return oauth_error(
                400,
                "invalid_grant",
                "the refresh token is unknown or has expired",
            );
        }
        directory(env, "oauth.token_remove", json!({ "hash": old })).await?;
        self.issue_pair(
            env,
            field(fields, "client_id"),
            row["service_user_id"].as_str().unwrap_or_default(),
        )
        .await
    }

    /// A new access token and refresh token for the client's service user.
    async fn issue_pair(
        &self,
        env: &Env,
        client_id: &str,
        service_user_id: &str,
    ) -> Result<Response> {
        let (access, access_hash) = oauth::token("pgn_at_")?;
        let (refresh, refresh_hash) = oauth::token("pgn_rt_")?;
        for (h, kind, ttl) in [
            (&access_hash, "access", ACCESS_TTL_MS),
            (&refresh_hash, "refresh", REFRESH_TTL_MS),
        ] {
            directory(
                env,
                "oauth.token_add",
                json!({
                    "hash": h, "kind": kind, "client_id": client_id,
                    "service_user_id": service_user_id, "ttl_ms": ttl,
                }),
            )
            .await?;
        }
        Ok(cors(Response::from_json(&json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL_MS / 1000,
            "refresh_token": refresh,
        }))?))
    }
}

impl Capability for OAuth {
    fn name(&self) -> &'static str {
        "oauth"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "oauth_clients",
                "List the registered OAuth clients, each with its service user and who it acts for.",
            ),
            Tool {
                name: "oauth_revoke",
                description: "Remove an OAuth client: its tokens and its link end.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "client_id": { "type": "string" } },
                    "required": ["client_id"]
                }),
            },
            Tool {
                name: "oauth_issue_token",
                description: "Issue the calling service user a long-lived bearer token for the MCP \
                              endpoint, for an agent with no browser. Shown once.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "days": { "type": "integer", "description": "90 by default, 365 at most." } }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "oauth_clients" => directory(cx.env, "oauth.clients", json!({})).await?,
                "oauth_revoke" => {
                    directory(
                        cx.env,
                        "oauth.client_remove",
                        json!({ "id": arg(args, "client_id")? }),
                    )
                    .await?
                }
                "oauth_issue_token" => {
                    let me = cx
                        .caller
                        .filter(|c| c.user.kind == "service")
                        .ok_or_else(|| {
                            Error::RustError("only a service user can do this".into())
                        })?;
                    let days = args["days"]
                        .as_i64()
                        .unwrap_or(ISSUED_DEFAULT_DAYS)
                        .clamp(1, ISSUED_MAX_DAYS);
                    let (token, token_hash) = oauth::token("pgn_at_")?;
                    directory(
                        cx.env,
                        "oauth.token_add",
                        json!({
                            "hash": token_hash, "kind": "access",
                            "service_user_id": me.user.id, "ttl_ms": days * 24 * 60 * 60 * 1000,
                        }),
                    )
                    .await?;
                    json!({ "token": token, "user": me.user, "expires_in_days": days })
                }
                other => return Err(unknown(other)),
            })
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/.well-known/", "/oauth/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

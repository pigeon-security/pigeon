//! Passkeys: how a person proves who they are. It serves the `/auth` pages Access lets
//! everyone reach, where the passkey is the only gate: registering a passkey from a
//! single-use enrolment link, and approving a service user's link with one. Later, the same
//! sign-in grants OAuth.

use pigeon_core::design::{Page, escape};
use pigeon_core::mcp::Tool;
use pigeon_core::webauthn::{self, Assertion, Credential, Registration, RelyingParty, b64url};
use serde_json::{Value, json};
use worker::{Error, Headers, Method, Request, Response, Result};

use crate::users::{Passkey, User, directory};
use crate::{Call, Capability, Context, Footprint, Serve, arg, to_value, unknown};

/// The passkeys' relying party: the hostname the `/auth` pages are on.
pub struct Passkeys {
    pub host: String,
    /// What the browser shows as the site's name.
    pub site_name: String,
}

/// The pages' policy: their own script and calls, inline styles, nothing else.
const CSP: &str = "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The pages' script: it runs the browser's side of each ceremony.
const SCRIPT: &str = include_str!("passkeys/passkey.js");

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

impl Passkeys {
    fn rp(&self) -> (String, String) {
        (self.host.clone(), format!("https://{}", self.host))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let query = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        match (req.method(), url.path()) {
            (Method::Get, "/auth/passkey.js") => {
                let headers = Headers::new();
                headers.set("Content-Type", "text/javascript; charset=utf-8")?;
                headers.set("X-Content-Type-Options", "nosniff")?;
                Ok(Response::ok(SCRIPT)?.with_headers(headers))
            }
            (Method::Get, "/auth/enrol") => {
                let code = query("code");
                let page = match directory(cx.env, "enrol.peek", json!({ "code": code })).await {
                    Ok(user) => self.page(
                        "Register a passkey",
                        &format!(
                            "Register a passkey for <strong>{}</strong> on this device. It is how you will sign in to {}.",
                            escape(user["name"].as_str().unwrap_or_default()),
                            escape(&self.site_name)
                        ),
                        Some(("enrol", "code", &code, "Register passkey")),
                    ),
                    Err(e) => self.page("Link not usable", &escape(&e.to_string()), None),
                };
                html(&page)
            }
            (Method::Get, "/auth/link") => {
                let request = query("request");
                let page = match directory(cx.env, "link.request_get", json!({ "id": request })).await {
                    Ok(v) => self.page(
                        "Approve a link",
                        &format!(
                            "The service user <strong>{}</strong> asks to act for <strong>{}</strong>. Approve with that person's passkey.",
                            escape(v["service"]["name"].as_str().unwrap_or_default()),
                            escape(v["standard"]["name"].as_str().unwrap_or_default())
                        ),
                        Some(("link", "request", &request, "Approve with passkey")),
                    ),
                    Err(e) => self.page("Link not usable", &escape(&e.to_string()), None),
                };
                html(&page)
            }
            (Method::Post, path) => {
                let body: Value = req.json().await.unwrap_or(Value::Null);
                let answer = match path {
                    "/auth/enrol/begin" => self.enrol_begin(cx, &body).await,
                    "/auth/enrol/finish" => self.enrol_finish(cx, &body).await,
                    "/auth/link/begin" => self.link_begin(cx, &body).await,
                    "/auth/link/finish" => self.link_finish(cx, &body).await,
                    _ => return Response::error("not found", 404),
                };
                match answer {
                    Ok(v) => Response::from_json(&v),
                    Err(e) => {
                        Ok(Response::from_json(&json!({ "error": e.to_string() }))?
                            .with_status(400))
                    }
                }
            }
            _ => Response::error("not found", 404),
        }
    }

    /// Starts registering a passkey for the enrolment code's user.
    async fn enrol_begin(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let code = arg(body, "code")?;
        let user: User = serde_json::from_value(
            directory(cx.env, "enrol.peek", json!({ "code": code })).await?,
        )?;
        let c = directory(
            cx.env,
            "challenge.create",
            json!({ "purpose": format!("enrol:{code}") }),
        )
        .await?;
        Ok(json!({
            "challenge_id": c["id"],
            "options": {
                "challenge": c["challenge"],
                "rp": { "id": self.host, "name": self.site_name },
                "user": { "id": user.id, "name": user.name, "displayName": user.name },
            }
        }))
    }

    /// Checks the new passkey and keeps it for the enrolment code's user; the code is used up.
    async fn enrol_finish(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let code = arg(body, "code")?;
        let challenge = directory(
            cx.env,
            "challenge.take",
            json!({ "id": arg(body, "challenge_id")?, "purpose": format!("enrol:{code}") }),
        )
        .await?;
        let registration: Registration = serde_json::from_value(body["credential"].clone())?;
        let (id, origin) = self.rp();
        let rp = RelyingParty {
            id: &id,
            origin: &origin,
        };
        let credential =
            webauthn::register(&registration, challenge.as_str().unwrap_or_default(), &rp)?;
        let user: User = serde_json::from_value(
            directory(cx.env, "enrol.take", json!({ "code": code })).await?,
        )?;
        directory(
            cx.env,
            "passkey.add",
            json!({
                "id": credential.id,
                "user_id": user.id,
                "point": b64url(&credential.point),
                "sign_count": credential.sign_count,
                "name": format!("passkey {}", &credential.id[..8.min(credential.id.len())]),
            }),
        )
        .await?;
        Ok(
            json!({ "message": format!("Passkey registered for {}. You can close this page.", user.name) }),
        )
    }

    /// Starts approving a link request with a passkey of its standard user.
    async fn link_begin(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let request = arg(body, "request")?;
        let r = directory(cx.env, "link.request_get", json!({ "id": request })).await?;
        let standard = r["standard"]["id"].as_str().unwrap_or_default();
        let keys: Vec<Passkey> = serde_json::from_value(
            directory(cx.env, "passkey.list", json!({ "user_id": standard })).await?,
        )?;
        if keys.is_empty() {
            return Err(bad("this person has no passkey yet; enrol one first"));
        }
        let c = directory(
            cx.env,
            "challenge.create",
            json!({ "purpose": format!("link:{request}") }),
        )
        .await?;
        Ok(json!({
            "challenge_id": c["id"],
            "options": {
                "challenge": c["challenge"],
                "rpId": self.host,
                "allow": keys.iter().map(|k| k.id.clone()).collect::<Vec<_>>(),
            }
        }))
    }

    /// Checks the passkey belongs to the request's standard user, then links the service user.
    async fn link_finish(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let request = arg(body, "request")?;
        let challenge = directory(
            cx.env,
            "challenge.take",
            json!({ "id": arg(body, "challenge_id")?, "purpose": format!("link:{request}") }),
        )
        .await?;
        let r = directory(cx.env, "link.request_get", json!({ "id": request })).await?;
        let assertion: Assertion = serde_json::from_value(body["assertion"].clone())?;
        let key: Option<Passkey> = serde_json::from_value(
            directory(cx.env, "passkey.get", json!({ "id": assertion.id })).await?,
        )?;
        let key = key
            .filter(|k| Some(k.user_id.as_str()) == r["standard"]["id"].as_str())
            .ok_or_else(|| bad("this passkey is not the person's the link asks for"))?;
        let (id, origin) = self.rp();
        let rp = RelyingParty {
            id: &id,
            origin: &origin,
        };
        let credential = Credential {
            id: key.id.clone(),
            point: webauthn::unb64url(&key.point)?,
            sign_count: u32::try_from(key.sign_count).unwrap_or(0),
        };
        let count = webauthn::assert(
            &assertion,
            &credential,
            challenge.as_str().unwrap_or_default(),
            &rp,
        )?;
        directory(
            cx.env,
            "passkey.touch",
            json!({ "id": key.id, "sign_count": count }),
        )
        .await?;
        directory(
            cx.env,
            "link.approve",
            json!({ "id": request, "passkey_id": key.id }),
        )
        .await?;
        Ok(json!({
            "message": format!(
                "{} now acts for {}. You can close this page.",
                r["service"]["name"].as_str().unwrap_or_default(),
                r["standard"]["name"].as_str().unwrap_or_default()
            )
        }))
    }

    /// A page with a message and, optionally, a button that runs a ceremony:
    /// `(flow, field, value, label)`.
    fn page(&self, title: &str, message: &str, action: Option<(&str, &str, &str, &str)>) -> String {
        let button = action.map_or(String::new(), |(flow, field, value, label)| {
            format!(
                r#"<button class="button" data-flow="{}" data-key="{}" data-value="{}">{}</button>"#,
                escape(flow),
                escape(field),
                escape(value),
                escape(label)
            )
        });
        let body = format!(
            r#"<main style="max-width:34rem"><span class="eyebrow">{} · passkeys</span><h1>{}</h1><article class="card"><p>{message}</p>{button}<p data-status role="status"></p></article></main><script src="/auth/passkey.js" defer></script>"#,
            escape(&self.site_name),
            escape(title)
        );
        Page {
            title,
            description: "Passkeys",
            body: &body,
        }
        .render()
    }
}

fn html(page: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Content-Security-Policy", CSP)?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Referrer-Policy", "no-referrer")?;
    headers.set("Cache-Control", "no-store")?;
    Ok(Response::ok(page)?.with_headers(headers))
}

impl Capability for Passkeys {
    fn name(&self) -> &'static str {
        "passkeys"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "passkeys_list",
                description: "List passkeys, of one user or all, without their keys.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "user_id": { "type": "string" } }
                }),
            },
            Tool {
                name: "passkeys_remove",
                description: "Remove a passkey by id; a user's last passkey is kept.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "passkeys_list" => {
                    let keys: Vec<Passkey> = serde_json::from_value(
                        directory(
                            cx.env,
                            "passkey.list",
                            json!({ "user_id": args["user_id"] }),
                        )
                        .await?,
                    )?;
                    let listed: Vec<Value> = keys
                        .iter()
                        .map(|k| {
                            json!({
                                "id": k.id, "user_id": k.user_id, "name": k.name,
                                "created_at": k.created_at, "last_used_at": k.last_used_at,
                            })
                        })
                        .collect();
                    to_value(listed)
                }
                "passkeys_remove" => {
                    directory(cx.env, "passkey.remove", json!({ "id": arg(args, "id")? })).await?
                }
                other => return Err(unknown(other)),
            })
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/auth/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }

    fn footprint(&self) -> Footprint {
        Footprint::default()
    }
}

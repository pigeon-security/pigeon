//! Secrets: the one way a value gets into the platform, so no one has to open Cloudflare's
//! dashboard and no value passes through a chat. Each capability declares the secrets it needs;
//! the `/secrets` page lists them, set or missing, and lets a person replace one. Seeing the list
//! takes a passkey, and each write takes a passkey of its own whose challenge is bound to the
//! secret's name and a hash of the new value, so an approval covers that one value. Before it is
//! written, the owning capability checks the value works. Values are never shown, logged or kept
//! by pigeon: they go straight to the Secrets Store. Over MCP, secrets can be listed and a link
//! to set one handed out, but never set.

use pigeon_core::design::{Page, escape};
use pigeon_core::mcp::Tool;
use pigeon_core::oauth::hash;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::tokens::{self, MintRequest, Policy, Scope};
use serde::Serialize;
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::passkeys::{Passkeys, html};
use crate::users::{User, directory};
use crate::{Call, Capability, Context, Serve, Slot, arg, to_value, unknown};

/// The secrets page and its tools.
pub struct Secrets {
    /// The hostname the page is on, which is also the passkeys' relying party.
    pub host: String,
    pub passkeys: Passkeys,
    /// The Secrets Store binding holding the parent token, which this capability owns.
    pub parent: String,
}

/// The page's script: signing in to see the list and approving each write.
const SCRIPT: &str = include_str!("secrets/secrets.js");

/// The challenge purpose for seeing the list.
const VIEW: &str = "secrets:view";

/// The challenge purpose for writing `name` with a value whose hash is `digest`.
fn write_purpose(name: &str, digest: &str) -> String {
    format!("secret:{name}:{digest}")
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// A declared secret and whether it is set: never its value.
#[derive(Serialize, Debug, PartialEq)]
pub struct Listed {
    pub name: String,
    /// The capability that needs it; empty for a secret no capability declares.
    pub owner: String,
    pub purpose: String,
    pub set: bool,
    pub modified: Option<String>,
}

/// Every declared slot, by owner, then the store's other secrets: `stored` is the store's
/// `(name, modified)` pairs.
pub fn listing(declared: &[(String, Slot)], stored: &[(String, Option<String>)]) -> Vec<Listed> {
    let mut out: Vec<Listed> = declared
        .iter()
        .map(|(owner, slot)| {
            let found = stored.iter().find(|(n, _)| *n == slot.name);
            Listed {
                name: slot.name.clone(),
                owner: owner.clone(),
                purpose: slot.purpose.clone(),
                set: found.is_some(),
                modified: found.and_then(|(_, m)| m.clone()),
            }
        })
        .collect();
    let mut others: Vec<Listed> = stored
        .iter()
        .filter(|(n, _)| !declared.iter().any(|(_, s)| s.name == *n))
        .map(|(n, m)| Listed {
            name: n.clone(),
            owner: String::new(),
            purpose: String::new(),
            set: true,
            modified: m.clone(),
        })
        .collect();
    others.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(others);
    out
}

impl Secrets {
    fn own_slot(&self) -> Slot {
        Slot {
            name: self.parent.clone(),
            purpose: "The parent token every other Cloudflare token is minted from. An account \
                      API token holding Account API Tokens Write and nothing else. It is checked \
                      by minting and revoking a token with it before it replaces the current one."
                .into(),
            scopes: &[secrets_store::WORKERS],
        }
    }

    /// Every hosted capability's slots, with the capability's name.
    fn declared(cx: &Context<'_>) -> Vec<(String, Slot)> {
        cx.hosted
            .capabilities
            .iter()
            .flat_map(|c| c.secrets().into_iter().map(|s| (c.name().to_string(), s)))
            .collect()
    }

    /// The declared slots and the store's other secrets, set or missing.
    async fn list(cx: &Context<'_>) -> Result<Vec<Listed>> {
        let writer = Writer::open(cx.account).await?;
        let stored = writer.list().await;
        let stored = writer.finish(stored).await?;
        let stored: Vec<(String, Option<String>)> =
            stored.into_iter().map(|s| (s.name, s.modified)).collect();
        Ok(listing(&Self::declared(cx), &stored))
    }

    fn url(&self, name: Option<&str>) -> String {
        match name {
            Some(n) => format!("https://{}/secrets#{n}", self.host),
            None => format!("https://{}/secrets", self.host),
        }
    }

    /// The passkey's user, who must be a person.
    async fn person(env: &Env, user_id: &str) -> Result<User> {
        let user: Option<User> =
            serde_json::from_value(directory(env, "users.get", json!({ "id": user_id })).await?)?;
        user.filter(|u| u.kind == "standard")
            .ok_or_else(|| bad("only a person can see or change secrets"))
    }

    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let path = req.path();
        match (req.method(), path.as_str()) {
            (Method::Get, "/secrets") => html(&self.page()),
            (Method::Get, "/secrets/secrets.js") => {
                let headers = Headers::new();
                headers.set("Content-Type", "text/javascript; charset=utf-8")?;
                headers.set("X-Content-Type-Options", "nosniff")?;
                headers.set("Cache-Control", "no-store")?;
                Ok(Response::ok(SCRIPT)?.with_headers(headers))
            }
            (Method::Post, p) => {
                let body: Value = req.json().await.unwrap_or(Value::Null);
                let answer = match p {
                    "/secrets/view/begin" => self.passkeys.sign_in_begin(cx.env, VIEW, None).await,
                    "/secrets/view/finish" => self.view_finish(cx, &body).await,
                    "/secrets/put/begin" => self.put_begin(cx, &body).await,
                    "/secrets/put/finish" => self.put_finish(cx, &body).await,
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
            _ => Response::error("not found", 404),
        }
    }

    async fn view_finish(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let key = self
            .passkeys
            .sign_in_finish(cx.env, VIEW, body, None)
            .await?;
        let person = Self::person(cx.env, &key.user_id).await?;
        Ok(json!({ "person": person.name, "secrets": Self::list(cx).await? }))
    }

    /// Starts approving one write: `name` must be a declared slot, `digest` the new value's
    /// base64url SHA-256, which the browser works out so the value itself is sent only once.
    async fn put_begin(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let name = arg(body, "name")?;
        let digest = arg(body, "digest")?;
        self.owner(cx, name)?;
        self.passkeys
            .sign_in_begin(cx.env, &write_purpose(name, digest), None)
            .await
    }

    /// Checks the passkey approved this name and this value, has the owner check the value,
    /// then writes it.
    async fn put_finish(&self, cx: &Context<'_>, body: &Value) -> Result<Value> {
        let name = arg(body, "name")?;
        let value = arg(body, "value")?.trim();
        if value.is_empty() {
            return Err(bad("the value is empty"));
        }
        let owner = self.owner(cx, name)?;
        // The challenge was issued for the hash of the value as typed; the browser trims too.
        let purpose = write_purpose(name, &hash(value));
        let key = self
            .passkeys
            .sign_in_finish(cx.env, &purpose, body, None)
            .await?;
        let person = Self::person(cx.env, &key.user_id).await?;
        let (checked, scopes) = match owner {
            Some(c) => {
                let slot = c.secrets().into_iter().find(|s| s.name == name);
                let scopes = slot.map_or(&[secrets_store::WORKERS][..], |s| s.scopes);
                (c.check_secret(cx, name, value).await?, scopes)
            }
            None => (self.check_parent(cx, value).await?, self.own_slot().scopes),
        };
        let writer = Writer::open(cx.account).await?;
        let comment = format!("set on the secrets page by {}", person.name);
        let written = writer.put_scoped(name, value, &comment, scopes).await;
        let id = writer.finish(written).await?;
        let mut message = format!(
            "{name} is set. Check: {}.",
            checked.as_str().unwrap_or("done")
        );
        if let Some(c) = owner {
            // The value is stored either way; what the owner does with it is reported.
            match c.secret_set(cx, name, &id).await {
                Ok(Value::String(done)) => message.push_str(&format!(" Then: {done}.")),
                Ok(_) => {}
                Err(e) => {
                    message.push_str(&format!(" But {} could not use it yet: {e}.", c.name()))
                }
            }
        }
        Ok(json!({ "message": message }))
    }

    /// The capability that declares `name`: `None` for this capability's own parent token. A
    /// name no one declares is refused, so the page writes only what the platform uses.
    fn owner<'a>(&self, cx: &'a Context<'a>, name: &str) -> Result<Option<&'a dyn Capability>> {
        if name == self.parent {
            return Ok(None);
        }
        cx.hosted
            .capabilities
            .iter()
            .find(|c| c.secrets().iter().any(|s| s.name == name))
            .map(|c| Some(c.as_ref()))
            .ok_or_else(|| bad("no capability declares this secret"))
    }

    /// A new parent token must be active and able to mint: it mints a token and revokes it.
    async fn check_parent(&self, cx: &Context<'_>, value: &str) -> Result<Value> {
        let status = tokens::verify(value, &cx.account.id).await?;
        if status != "active" {
            return Err(bad(&format!("this token is {status}, not active")));
        }
        let request = MintRequest {
            ttl_secs: 60,
            ..MintRequest::new(
                "secrets-check",
                vec![Policy::new(Scope::Account, &["Account Settings Read"])],
            )
        };
        let minted = tokens::mint(value, &cx.account.id, &request)
            .await
            .map_err(|e| bad(&format!("this token cannot mint tokens: {e}")))?;
        tokens::revoke(value, &cx.account.id, &minted.id)
            .await
            .map_err(|e| bad(&format!("this token minted one but cannot revoke it: {e}")))?;
        Ok(Value::String(
            "it is active and can mint and revoke tokens".into(),
        ))
    }

    fn page(&self) -> String {
        let body = format!(
            r#"<main style="max-width:44rem"><span class="eyebrow">{} · secrets</span><h1>Secrets</h1><article class="card"><p>The secrets the platform's capabilities need. Values are never shown: you can only replace one, and each change is approved with your passkey.</p><button class="button" data-view>Sign in with passkey</button><p data-status role="status"></p></article><div data-list class="layer" style="margin-top:16px"></div></main><script src="/auth/passkey.js" defer></script><script src="/secrets/secrets.js" defer></script>"#,
            escape(&self.passkeys.site_name)
        );
        Page {
            title: "Secrets",
            description: "Set the platform's secrets",
            body: &body,
        }
        .render()
    }
}

impl Capability for Secrets {
    fn name(&self) -> &'static str {
        "secrets"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::bare(
                "secrets_list",
                "List the secrets the platform's capabilities need, and the store's others: \
                 each one's owner, purpose, whether it is set and when it last changed. Never \
                 values.",
            ),
            Tool {
                name: "secrets_request",
                description: "A link for the person to set a secret on the passkey-gated \
                              secrets page. Hand them the link; never ask for a value in chat.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "name": { "type": "string", "description": "A declared secret; omit for the whole page." } }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "secrets_list" => to_value(Self::list(cx).await?),
                "secrets_request" => {
                    let name = args["name"].as_str();
                    if let Some(n) = name {
                        self.owner(cx, n)?;
                    }
                    json!({ "url": self.url(name) })
                }
                other => return Err(unknown(other)),
            })
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        vec![self.own_slot()]
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/secrets"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str) -> Slot {
        Slot {
            name: name.into(),
            purpose: format!("{name} purpose"),
            scopes: &[secrets_store::WORKERS],
        }
    }

    #[test]
    fn declared_slots_come_first_set_or_missing_then_the_others_by_name() {
        let declared = vec![
            ("secrets".to_string(), slot("CF_PARENT_TOKEN")),
            ("mirror".to_string(), slot("GITHUB_MIRROR_TOKEN")),
        ];
        let stored = vec![
            ("ZETA".to_string(), None),
            (
                "CF_PARENT_TOKEN".to_string(),
                Some("2026-11-01".to_string()),
            ),
            ("ALPHA".to_string(), None),
        ];
        let got = listing(&declared, &stored);
        let names: Vec<&str> = got.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            ["CF_PARENT_TOKEN", "GITHUB_MIRROR_TOKEN", "ALPHA", "ZETA"]
        );
        assert!(got[0].set);
        assert_eq!(got[0].modified.as_deref(), Some("2026-11-01"));
        assert_eq!(got[0].owner, "secrets");
        assert!(!got[1].set);
        assert_eq!(got[1].owner, "mirror");
        assert!(got[2].set && got[2].owner.is_empty());
    }

    #[test]
    fn a_write_approval_is_bound_to_name_and_value() {
        let a = write_purpose("X", &hash("one"));
        assert_ne!(a, write_purpose("X", &hash("two")));
        assert_ne!(a, write_purpose("Y", &hash("one")));
        assert_ne!(a, VIEW);
    }
}

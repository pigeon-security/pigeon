//! Secrets: the one way a value gets into the platform, so no one has to open Cloudflare's
//! dashboard and no value passes through a chat. Each capability declares the secrets it needs;
//! the console's Access › Secrets lists them, set or missing, and lets a person replace one (the
//! console's `secret-set` write). Seeing the list takes the console's session, and each write
//! takes a passkey of its own whose challenge is bound to the person, the secret's name and a
//! hash of the new value, so an approval covers that one value. Before it is
//! written, the owning capability checks the value works. Values are never shown, logged or kept
//! by pigeon: they go straight to the Secrets Store. Over MCP, secrets can be listed and a link
//! to set one handed out, but never set.

use pigeon_core::mcp::Tool;

use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::tokens::{self, MintRequest, Policy, Scope};
use serde::Serialize;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::console::sections::Section;
use crate::expiry::{self, Expiry};
use crate::instances::record::Instance;
use crate::users::User;
use crate::{Call, Capability, Context, Expiring, Slot, to_value, unknown};

/// Secrets: the console's `secret-set` write and the secrets tools.
pub struct Secrets {
    /// The console's hostname, which `secrets_request` links to.
    pub host: String,
    /// The Secrets Store binding holding the parent token, which this capability owns.
    pub parent: String,
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// Access › Secrets.
fn secrets_view() -> Section {
    Section {
        area: "access",
        id: "secrets",
        title: "Secrets",
        description: "Each secret with its expiry. Values are never shown; setting or replacing one asks for your passkey, bound to that name.",
        order: 10,
    }
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

/// The id of the first Secrets Store in Cloudflare's list of an account's stores (`[{id, name,
/// ...}]`), if it has one. An account holds one store in practice. Pure.
pub fn store_of(listed: &Value) -> Option<String> {
    listed
        .as_array()?
        .iter()
        .find_map(|s| s["id"].as_str().filter(|id| !id.is_empty()))
        .map(str::to_string)
}

/// The name of the Secrets Store an instance plan creates in a linked account that has none
/// (capability scoping, M4).
pub const LINKED_STORE: &str = "pigeon";

/// Why `instance` has no secrets to list yet: its account has no Secrets Store, which its plan
/// creates.
pub fn no_store(instance: &str) -> String {
    format!(
        "{instance}'s Cloudflare account has no Secrets Store yet: instance_plan {{instance: \
         \"{instance}\"}} creates one, once the person approves it."
    )
}

/// The Secrets Store of a linked account, read with its link's bearer: `None` when it has none.
pub async fn linked_store(account: &pigeon_core::Account) -> Result<Option<String>> {
    let token = account
        .bearer
        .as_deref()
        .ok_or_else(|| bad("the instance's account is not a linked one"))?;
    let path = format!("/accounts/{}/secrets_store/stores", account.id);
    let listed = pigeon_core::cloudflare::call(token, worker::Method::Get, &path, None).await?;
    Ok(store_of(&listed))
}

impl Secrets {
    /// `secrets_list {instance}` for a linked instance whose record enables `secrets` (M4): the
    /// names in its own account's Secrets Store and when each changed, never values. Nothing is
    /// declared there yet, so each is listed with no owner. Refused when the record does not
    /// enable secrets, and with "re-link to enable secrets" when its link lacks the scope.
    async fn list_linked(&self, cx: &Context<'_>, record: &Instance) -> Result<Vec<Listed>> {
        let id = record.id.as_str();
        if !crate::catalogue::hosted_enabled(record, "secrets") {
            return Err(bad(&format!(
                "secrets is not enabled on {id}: enable it on the instance's Capabilities view"
            )));
        }
        let explain = |e: Error| bad(&crate::linked::explain(id, "secrets", &e.to_string()));
        let mut account =
            crate::linked::account_for_capability(cx.env, &self.host, id, "secrets").await?;
        let store = linked_store(&account)
            .await
            .map_err(explain)?
            .ok_or_else(|| bad(&no_store(id)))?;
        account.secrets_store = store;
        let writer = Writer::open(&account).await?;
        let stored = writer.list().await;
        let stored = writer.finish(stored).await.map_err(explain)?;
        let stored: Vec<(String, Option<String>)> =
            stored.into_iter().map(|s| (s.name, s.modified)).collect();
        Ok(listing(&[], &stored))
    }

    /// `secrets_list`: the platform's, or with `instance` that instance's: home's are the
    /// platform's; a linked one's are in its own account ([`Self::list_linked`]).
    async fn list_for(&self, cx: &Context<'_>, args: &Value) -> Result<Vec<Listed>> {
        let Some(id) = args["instance"].as_str() else {
            return Self::list(cx).await;
        };
        let record = crate::instances::one(cx.env, id)
            .await?
            .ok_or_else(|| bad(&format!("there is no instance {id}")))?;
        if crate::catalogue::is_home(&record) {
            return Self::list(cx).await;
        }
        self.list_linked(cx, &record).await
    }

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

    /// Every declared secret's expiry: the set ones as their issuers report it, at most a day
    /// old, and the missing ones as missing.
    async fn expiry(cx: &Context<'_>, listed: &[Listed]) -> Vec<expiry::Row> {
        let set: Vec<String> = listed
            .iter()
            .filter(|l| l.set)
            .map(|l| l.name.clone())
            .collect();
        expiry::secrets(cx, &set).await
    }

    /// Every credential and when it expires, the ones that need a person first: declared
    /// secrets and proxy credentials. Never a value.
    async fn credentials(cx: &Context<'_>) -> Result<Value> {
        let listed = Self::list(cx).await?;
        let mut rows = Self::expiry(cx, &listed).await;
        rows.extend(expiry::proxy(cx.env).await?);
        rows.sort_by_key(|r| !r.needs_you());
        Ok(json!({ "warn_days": expiry::WARN_DAYS, "credentials": rows }))
    }

    /// The `secret-set` write, approved by `person`'s passkey for this name and this value: has
    /// the owner check the value, then writes it.
    async fn put(&self, cx: &Context<'_>, name: &str, value: &str, person: &User) -> Result<Value> {
        let value = value.trim();
        if value.is_empty() {
            return Err(bad("the value is empty"));
        }
        let owner = self.owner(cx, name)?;
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
        // A new value may expire differently: the next look asks its issuer again.
        let _ = expiry::forget(cx.env, name).await;
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
        let minted = tokens::mint_as(cx.account, &request, Some(value))
            .await
            .map_err(|e| bad(&format!("this token cannot mint tokens: {e}")))?;
        tokens::revoke_as(cx.account, &minted.id, Some(value))
            .await
            .map_err(|e| bad(&format!("this token minted one but cannot revoke it: {e}")))?;
        Ok(Value::String(
            "it is active and can mint and revoke tokens".into(),
        ))
    }
}

/// What Cloudflare's tokens/verify answer `v` says of a token's expiry: refused unless its
/// status is `active`, else its `expires_on`, or never without one.
pub fn cloudflare_expiry(v: &Value) -> Expiry {
    match v["status"].as_str() {
        Some("active") => expiry::from_field(v["expires_on"].as_str()),
        other => Expiry::Refused {
            why: format!(
                "Cloudflare says the token is {}",
                other.unwrap_or("unknown")
            ),
        },
    }
}

impl Capability for Secrets {
    fn name(&self) -> &'static str {
        "secrets"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "secrets",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn sections(&self) -> Vec<Section> {
        vec![secrets_view()]
    }

    /// `secret-set`: only a name some capability declares (or the parent token's).
    fn write_check<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            self.owner(cx, w.text())?;
            Ok(Value::Null)
        })
    }

    fn write_make<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            let person = w.person;
            let value = w.change["value"].as_str().unwrap_or_default();
            self.put(cx, w.text(), value, person).await
        })
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "secrets_list",
                description: "List the secrets the platform's capabilities need, and the store's \
                              others: each one's owner, purpose, whether it is set and when it \
                              last changed. Never values. With `instance`, a linked instance \
                              whose record enables secrets lists its own account's Secrets \
                              Store instead.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "instance": { "type": "string", "description": "An instance's id; omit for the platform's." } }
                }),
            },
            Tool::bare(
                "credentials_expiry",
                "List every credential the platform holds and when it expires: each declared \
                 secret, as its issuer reports it (checked at most daily), and each proxy \
                 credential. Each has a status: ok, soon (within 7 days), expired (or refused \
                 by its issuer), unknown or missing. Read-only; never values.",
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
                "secrets_list" => to_value(self.list_for(cx, args).await?),
                "credentials_expiry" => Self::credentials(cx).await?,
                "secrets_request" => {
                    let name = args["name"].as_str();
                    if let Some(n) = name {
                        self.owner(cx, n)?;
                    }
                    let page = crate::console::home_url(cx.env, "access", Some("secrets")).await;
                    let url = format!("https://{}{page}", self.host);
                    json!({ "url": match name {
                        Some(n) => format!("{url}#{n}"),
                        None => url,
                    } })
                }
                other => return Err(unknown(other)),
            })
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        vec![self.own_slot()]
    }

    /// Cloudflare's tokens/verify answers the parent token's `expires_on`, when it has one.
    fn secret_expiry<'a>(&'a self, cx: &'a Context<'a>, name: &'a str) -> Expiring<'a> {
        Box::pin(async move {
            if name != self.parent {
                return Ok(Expiry::Unknown {
                    why: "this capability does not use this secret".into(),
                });
            }
            match tokens::verify_detail(&cx.account.parent, &cx.account.id).await {
                Ok(v) => Ok(cloudflare_expiry(&v)),
                Err(e) if e.to_string().contains(": 401:") => Ok(Expiry::Refused {
                    why: "Cloudflare refuses the token (401): it has expired or been revoked"
                        .into(),
                }),
                Err(e) => Err(e),
            }
        })
    }
}

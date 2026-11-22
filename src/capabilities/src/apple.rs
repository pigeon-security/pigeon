//! The `apple` capability (docs/plans/ios.md): the App Store Connect credentials as slots on the
//! secrets page, each checked before it is written; `apple_status`, which reads (never writes)
//! what Apple holds for the app `dev.pigeon.app`; and, from phase 2, two console writes, each
//! bound to the signed-in person's passkey:
//!
//! - **Adopt `dev.pigeon.app`**: registers the bundle id if Apple has none, and adds the two
//!   capabilities the app needs (Push Notifications, Associated Domains) where they are missing.
//! - **Create signing identity**: a new RSA-2048 key made in this Worker, a CSR for it, an Apple
//!   Distribution certificate, and an App Store profile for the bundle id and that certificate.
//!   The key is kept only sealed with `APPLE_SEAL_KEY` (AES-256-GCM), which pigeon makes in the
//!   Secrets Store on the first press; the person never types it.
//!
//! They are console writes and not MCP tools, so no agent can mint a signing identity. Neither
//! touches a certificate or profile pigeon did not make.
//!
//! The key and every token stay in this Worker. No answer holds a key, a token, a certificate or
//! a profile's bytes.

use pigeon_core::apple::{self, BUNDLE_ID};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::{crypto, expiry::parse_time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Headers, Method, Request, Response, Result};

use crate::expiry::Expiry;
use crate::passkeys::Passkeys;
use crate::users::directory;
use crate::{Call, Capability, Context, Expiring, Serve, Slot};

/// The App Store Connect Team API key (`.p8`).
pub const ASC_KEY: &str = "APPLE_ASC_KEY";
/// Its key id.
pub const ASC_KEY_ID: &str = "APPLE_ASC_KEY_ID";
/// The team's issuer id.
pub const ASC_ISSUER_ID: &str = "APPLE_ASC_ISSUER_ID";
/// The Developer team id.
pub const TEAM_ID: &str = "APPLE_TEAM_ID";

/// Seals the distribution keys pigeon makes: 32 random bytes, base64url. pigeon makes it in the
/// Secrets Store on the first "Create signing identity"; it is not a slot, since no one types it.
pub const SEAL_KEY: &str = "APPLE_SEAL_KEY";

/// The capabilities `dev.pigeon.app` needs, as App Store Connect names them.
pub const CAPABILITIES: [&str; 2] = ["PUSH_NOTIFICATIONS", "ASSOCIATED_DOMAINS"];

/// The common name in pigeon's certificate requests.
pub const COMMON_NAME: &str = "pigeon dev.pigeon.app";

/// The Directory key listing the ids of the identities pigeon made.
const IDENTITIES: &str = "apple:identities";
/// How long the Directory keeps them: until removed, in practice.
const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;
/// A new identity is refused while pigeon holds one with longer than this left.
const RENEW_WITHIN_MS: i64 = 30 * 24 * 3_600_000;

/// The slots, in the order the person fills them.
pub const SLOTS: [&str; 4] = [TEAM_ID, ASC_ISSUER_ID, ASC_KEY_ID, ASC_KEY];

/// What to do while the capability is off.
pub const SETUP: &str = "Apple is not set up. On the console's secrets page, set APPLE_TEAM_ID, \
APPLE_ASC_ISSUER_ID, APPLE_ASC_KEY_ID and APPLE_ASC_KEY (docs/plans/ios.md, follow-ups 1 and 3). \
Once all four are set, the bindings that let this Worker read them land, and apple_status checks \
them against Apple.";

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now_ms() -> i64 {
    worker::Date::now().as_millis() as i64
}

/// An App Store profile pigeon made, with its bytes (base64, as Apple gives them).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: String,
    pub uuid: String,
    pub name: String,
    pub expires: String,
    pub content: String,
}

/// A signing identity pigeon made: Apple's certificate (DER, base64) and its private key sealed
/// with `APPLE_SEAL_KEY`, the certificate's id as associated data; and its profile, once made.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Identity {
    pub id: String,
    pub serial: String,
    pub expires: String,
    pub certificate: String,
    pub sealed_key: String,
    pub made_by: String,
    pub made_ms: i64,
    pub profile: Option<Profile>,
}

impl Identity {
    /// What the console and `apple_status` may show: never the key, the certificate or the
    /// profile's bytes.
    pub fn shown(&self) -> Value {
        json!({
            "certificate_id": self.id,
            "serial": self.serial,
            "expires": self.expires,
            "made_by": self.made_by,
            "profile": self.profile.as_ref().map(|p| json!({
                "id": p.id, "uuid": p.uuid, "name": p.name, "expires": p.expires,
            })),
        })
    }

    /// The earliest of its certificate's and profile's expiry, in ms.
    pub fn expires_ms(&self) -> Option<i64> {
        let cert = parse_time(&self.expires);
        let prof = self.profile.as_ref().and_then(|p| parse_time(&p.expires));
        match (cert, prof) {
            (Some(c), Some(p)) => Some(c.min(p)),
            (c, None) => c,
            (None, p) => p,
        }
    }
}

/// The associated data that binds a sealed key to its certificate.
pub fn seal_aad(certificate_id: &str) -> String {
    format!("apple:identity:{certificate_id}")
}

/// The passkey purpose of each console write, naming the bundle id it acts on.
pub fn purpose(write: &str) -> Option<String> {
    matches!(write, "adopt" | "identity").then(|| format!("apple:{write}:{BUNDLE_ID}"))
}

/// The capabilities in `wanted` that `have` lacks, in `wanted`'s order.
pub fn missing<'a>(wanted: &[&'a str], have: &[String]) -> Vec<&'a str> {
    wanted
        .iter()
        .filter(|w| !have.iter().any(|h| h == *w))
        .copied()
        .collect()
}

/// The JSON:API body that asks for an Apple Distribution certificate for `csr_der`.
pub fn certificate_request(csr_der: &[u8]) -> Value {
    json!({ "data": { "type": "certificates", "attributes": {
        "certificateType": "DISTRIBUTION",
        "csrContent": apple::b64(csr_der),
    } } })
}

/// The JSON:API body that asks for an App Store profile for a bundle id and one certificate.
pub fn profile_request(name: &str, bundle_id: &str, certificate_id: &str) -> Value {
    json!({ "data": { "type": "profiles",
        "attributes": { "name": name, "profileType": "IOS_APP_STORE" },
        "relationships": {
            "bundleId": { "data": { "type": "bundleIds", "id": bundle_id } },
            "certificates": { "data": [{ "type": "certificates", "id": certificate_id }] },
        },
    } })
}

async fn setting(env: &Env, key: &str) -> Result<Option<String>> {
    Ok(
        match directory(env, "setting.get", json!({ "key": key })).await? {
            Value::String(s) if !s.is_empty() => Some(s),
            _ => None,
        },
    )
}

async fn keep(env: &Env, key: &str, value: &str) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": value, "ttl_ms": KEPT_MS }),
    )
    .await?;
    Ok(())
}

/// The identities pigeon made, oldest first.
pub async fn identities(env: &Env) -> Result<Vec<Identity>> {
    let ids: Vec<String> = match setting(env, IDENTITIES).await? {
        Some(t) => serde_json::from_str(&t)?,
        None => Vec::new(),
    };
    let mut out = Vec::new();
    for id in ids {
        if let Some(t) = setting(env, &format!("apple:identity:{id}")).await? {
            out.push(serde_json::from_str(&t)?);
        }
    }
    Ok(out)
}

async fn save(env: &Env, identity: &Identity) -> Result<()> {
    keep(
        env,
        &format!("apple:identity:{}", identity.id),
        &serde_json::to_string(identity)?,
    )
    .await?;
    let mut ids: Vec<String> = match setting(env, IDENTITIES).await? {
        Some(t) => serde_json::from_str(&t)?,
        None => Vec::new(),
    };
    if !ids.contains(&identity.id) {
        ids.push(identity.id.clone());
        keep(env, IDENTITIES, &serde_json::to_string(&ids)?).await?;
    }
    Ok(())
}

/// The capability types a bundle id has, read with the bundle id itself (`include`), as Apple
/// documents for `GET /v1/bundleIds/{id}`.
async fn capabilities(token: &str, bundle: &str) -> Result<Vec<String>> {
    let b = apple::get(
        token,
        &format!("/v1/bundleIds/{bundle}?include=bundleIdCapabilities"),
    )
    .await?;
    Ok(capability_types(&b))
}

/// The capability types in a `GET /v1/bundleIds/{id}?include=bundleIdCapabilities` answer.
pub fn capability_types(answer: &Value) -> Vec<String> {
    answer["included"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "bundleIdCapabilities")
        .filter_map(|c| c["attributes"]["capabilityType"].as_str().map(String::from))
        .collect()
}

/// The bundle id's App Store Connect id, or None when Apple has none.
async fn bundle_id(token: &str) -> Result<Option<String>> {
    let ids = apple::get(
        token,
        &format!("/v1/bundleIds?filter[identifier]={BUNDLE_ID}&fields[bundleIds]=identifier"),
    )
    .await?;
    // The filter matches prefixes too; only the exact identifier counts.
    Ok(ids["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["attributes"]["identifier"].as_str() == Some(BUNDLE_ID))
        .and_then(|b| b["id"].as_str())
        .map(String::from))
}

/// The secret `name`, or None when it is unset or not bound to this Worker yet.
async fn secret(env: &Env, name: &str) -> Option<String> {
    let binding = env.secret_store(name).ok()?;
    secrets_store::read(&binding)
        .await
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A token from the stored key and ids, or None when any of them is unreadable.
async fn token(env: &Env) -> Option<Result<String>> {
    let key = secret(env, ASC_KEY).await?;
    let key_id = secret(env, ASC_KEY_ID).await?;
    let issuer = secret(env, ASC_ISSUER_ID).await?;
    let Some(der) = apple::p8_der(&key) else {
        return Some(Err(bad(
            "the stored APPLE_ASC_KEY is not a .p8 key; set it again",
        )));
    };
    let now = worker::Date::now().as_millis() / 1000;
    Some(apple::sign_jwt(&der, &key_id, &issuer, now).await)
}

/// Checks a value for slot `name` before it is written. Answers what was found.
pub async fn check(env: &Env, name: &str, value: &str) -> Result<String> {
    let v = value.trim();
    match name {
        TEAM_ID => {
            if !apple::is_apple_id(v) {
                return Err(bad(
                    "a team id is 10 characters of A-Z and 0-9 (Membership details)",
                ));
            }
            Ok("a team id in the right form".into())
        }
        ASC_KEY_ID => {
            if !apple::is_apple_id(v) {
                return Err(bad(
                    "a key id is 10 characters of A-Z and 0-9 (Users and Access, Integrations)",
                ));
            }
            Ok("a key id in the right form".into())
        }
        ASC_ISSUER_ID => {
            if !apple::is_issuer_id(v) {
                return Err(bad(
                    "an issuer id is a UUID (Users and Access, Integrations, above the keys)",
                ));
            }
            Ok("an issuer id in the right form".into())
        }
        ASC_KEY => {
            let der = apple::p8_der(v).ok_or_else(|| {
                bad("paste the whole .p8 file, from -----BEGIN PRIVATE KEY----- to -----END PRIVATE KEY-----")
            })?;
            let now = worker::Date::now().as_millis() / 1000;
            // With the ids readable, the key is tried against Apple; before that, a token signed
            // with placeholder ids proves it is a P-256 key WebCrypto accepts.
            match (
                secret(env, ASC_KEY_ID).await,
                secret(env, ASC_ISSUER_ID).await,
            ) {
                (Some(key_id), Some(issuer)) => {
                    let jwt = apple::sign_jwt(&der, &key_id, &issuer, now).await?;
                    apple::get(&jwt, "/v1/apps?limit=1")
                        .await
                        .map_err(|e| bad(&format!("Apple refused this key: {e}")))?;
                    Ok("the key signs in to App Store Connect".into())
                }
                _ => {
                    apple::sign_jwt(
                        &der,
                        "AAAAAAAAAA",
                        "00000000-0000-0000-0000-000000000000",
                        now,
                    )
                    .await?;
                    Ok("a P-256 key that signs; Apple checks it once the ids are readable".into())
                }
            }
        }
        _ => Err(bad(&format!("apple has no secret `{name}`"))),
    }
}

/// `apple_status`: what is set, and what Apple holds for `dev.pigeon.app`. Read-only.
pub async fn status(env: &Env) -> Result<Value> {
    let mut set = serde_json::Map::new();
    for name in SLOTS {
        set.insert(name.into(), json!(secret(env, name).await.is_some()));
    }
    let Some(token) = token(env).await else {
        return Ok(json!({ "readable": set, "ready": false, "next": SETUP }));
    };
    let token = token?;
    let team = secret(env, TEAM_ID).await;

    let apps = apple::get(
        &token,
        &format!("/v1/apps?filter[bundleId]={BUNDLE_ID}&fields[apps]=name,bundleId,sku"),
    )
    .await?;
    let app = &apps["data"][0];
    let app_id = app["id"].as_str().map(String::from);

    // The highest build number uploaded for the app: new builds must exceed it.
    let mut highest_build: Option<u64> = None;
    let mut builds_seen = 0;
    if let Some(id) = &app_id {
        let builds = apple::get(
            &token,
            &format!("/v1/builds?filter[app]={id}&fields[builds]=version&limit=200"),
        )
        .await?;
        for b in builds["data"].as_array().into_iter().flatten() {
            builds_seen += 1;
            if let Some(n) = b["attributes"]["version"]
                .as_str()
                .and_then(|v| v.parse().ok())
            {
                highest_build = highest_build.max(Some(n));
            }
        }
    }

    let ids = apple::get(
        &token,
        &format!(
            "/v1/bundleIds?filter[identifier]={BUNDLE_ID}&fields[bundleIds]=identifier,seedId"
        ),
    )
    .await?;
    let bundle = ids["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["attributes"]["identifier"].as_str() == Some(BUNDLE_ID))
        .cloned()
        .unwrap_or(Value::Null);
    let held = identities(env).await?;
    let caps = match bundle["id"].as_str() {
        Some(id) => match capabilities(&token, id).await {
            Ok(c) => json!(c),
            Err(e) => json!({ "unreadable": e.to_string() }),
        },
        None => Value::Null,
    };

    let certs = apple::get(
        &token,
        "/v1/certificates?filter[certificateType]=DISTRIBUTION&fields[certificates]=name,displayName,expirationDate,serialNumber",
    )
    .await?;
    let certificates: Vec<Value> = certs["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            json!({
                "id": c["id"],
                "name": c["attributes"]["displayName"].as_str().or(c["attributes"]["name"].as_str()),
                "serial": c["attributes"]["serialNumber"],
                "expires": c["attributes"]["expirationDate"],
                "pigeon_holds_key": held.iter().any(|i| c["id"].as_str() == Some(i.id.as_str())),
            })
        })
        .collect();

    let profs = apple::get(
        &token,
        "/v1/profiles?filter[profileType]=IOS_APP_STORE&fields[profiles]=name,profileState,expirationDate",
    )
    .await?;
    let profiles: Vec<Value> = profs["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            json!({
                "id": p["id"],
                "name": p["attributes"]["name"],
                "state": p["attributes"]["profileState"],
                "expires": p["attributes"]["expirationDate"],
            })
        })
        .collect();

    Ok(json!({
        "readable": set,
        "ready": true,
        "key": "signs in to App Store Connect",
        "team_matches_bundle_id": match (&team, bundle["attributes"]["seedId"].as_str()) {
            (Some(t), Some(seed)) => json!(t == seed),
            _ => Value::Null,
        },
        "bundle_id": { "identifier": BUNDLE_ID, "registered": !bundle.is_null(), "id": bundle["id"], "capabilities": caps },
        "app": if app.is_null() { Value::Null } else { json!({
            "id": app["id"],
            "name": app["attributes"]["name"],
            "builds_seen": builds_seen,
            "highest_build_number": highest_build,
            "next_build_number_at_least": highest_build.map(|n| n + 1),
        }) },
        "distribution_certificates": certificates,
        "app_store_profiles": profiles,
        "seal_key_readable": secret(env, SEAL_KEY).await.is_some(),
        "pigeon_identities": held.iter().map(Identity::shown).collect::<Vec<_>>(),
    }))
}

/// The seal key, or None while it is unreadable (not made yet, or its binding not landed).
async fn seal_key(env: &Env) -> Option<[u8; 32]> {
    crypto::key_from(&secret(env, SEAL_KEY).await?)
}

/// Adopt `dev.pigeon.app`: registers it if Apple has none, and adds the missing capabilities.
async fn adopt(env: &Env) -> Result<String> {
    let token = token(env).await.ok_or_else(|| bad(SETUP))??;
    let (id, registered) = match bundle_id(&token).await? {
        Some(id) => (id, false),
        None => {
            let made = apple::post(
                &token,
                "/v1/bundleIds",
                &json!({ "data": { "type": "bundleIds", "attributes": {
                    "identifier": BUNDLE_ID, "name": "pigeon", "platform": "IOS",
                } } }),
            )
            .await?;
            let id = made["data"]["id"]
                .as_str()
                .ok_or_else(|| bad("Apple registered the bundle id but answered no id"))?;
            (id.to_string(), true)
        }
    };
    let have = capabilities(&token, &id)
        .await
        .map_err(|e| bad(&format!("reading {BUNDLE_ID}'s capabilities: {e}")))?;
    let add = missing(&CAPABILITIES, &have);
    for cap in &add {
        apple::post(
            &token,
            "/v1/bundleIdCapabilities",
            &json!({ "data": { "type": "bundleIdCapabilities",
                "attributes": { "capabilityType": cap },
                "relationships": { "bundleId": { "data": { "type": "bundleIds", "id": id } } },
            } }),
        )
        .await
        .map_err(|e| bad(&format!("adding {cap} to {BUNDLE_ID}: {e}")))?;
    }
    let mut said = if registered {
        format!("Registered {BUNDLE_ID}.")
    } else {
        format!("{BUNDLE_ID} is registered.")
    };
    said.push_str(&if add.is_empty() {
        " It already has Push Notifications and Associated Domains.".to_string()
    } else {
        format!(" Added: {}.", add.join(", "))
    });
    Ok(said)
}

/// Create signing identity. On the first press it only makes `APPLE_SEAL_KEY`; once that is
/// bound, it makes the key, the certificate and the profile. A certificate whose profile failed
/// is kept, and the next press makes only the profile.
async fn create_identity(cx: &Context<'_>, person: &str) -> Result<String> {
    let env = cx.env;
    let token = token(env).await.ok_or_else(|| bad(SETUP))??;
    let Some(key) = seal_key(env).await else {
        let writer = Writer::open(cx.account).await?;
        let made = async {
            if writer.list().await?.iter().any(|s| s.name == SEAL_KEY) {
                return Ok(false);
            }
            writer
                .put_scoped(
                    SEAL_KEY,
                    &pigeon_core::webauthn::random()?,
                    "seals pigeon's Apple distribution keys, by Create signing identity",
                    &[secrets_store::WORKERS],
                )
                .await?;
            Ok(true)
        }
        .await;
        let made = writer.finish(made).await?;
        return Ok(format!(
            "{} Its binding to this Worker lands next; then press Create signing identity again. \
             Nothing was asked of Apple.",
            if made {
                "pigeon made APPLE_SEAL_KEY in the Secrets Store."
            } else {
                "APPLE_SEAL_KEY is in the Secrets Store but not yet readable here."
            }
        ));
    };
    let bundle = bundle_id(&token)
        .await?
        .ok_or_else(|| bad(&format!("Apple has no {BUNDLE_ID}: press Adopt first")))?;

    let now = now_ms();
    let held = identities(env).await?;
    let live: Vec<&Identity> = held
        .iter()
        .filter(|i| parse_time(&i.expires).is_some_and(|t| t > now))
        .collect();
    if let Some(i) = live
        .iter()
        .find(|i| i.profile.is_some() && i.expires_ms().is_some_and(|t| t > now + RENEW_WITHIN_MS))
    {
        return Err(bad(&format!(
            "pigeon already holds a signing identity (certificate {}, until {}); a new one is \
             made within 30 days of its expiry",
            i.serial, i.expires
        )));
    }

    let mut identity = match live.iter().find(|i| i.profile.is_none()) {
        Some(i) => (*i).clone(),
        None => {
            let new = apple::new_key(COMMON_NAME).await?;
            let made = apple::post(&token, "/v1/certificates", &certificate_request(&new.csr))
                .await
                .map_err(|e| bad(&format!("Apple did not make the certificate: {e}")))?;
            let d = &made["data"];
            let a = &d["attributes"];
            let (Some(id), Some(content)) = (d["id"].as_str(), a["certificateContent"].as_str())
            else {
                return Err(bad(
                    "Apple made a certificate but answered no id or content",
                ));
            };
            let sealed_key = crypto::seal(&key, &new.pkcs8, seal_aad(id).as_bytes()).await?;
            let identity = Identity {
                id: id.into(),
                serial: a["serialNumber"].as_str().unwrap_or_default().into(),
                expires: a["expirationDate"].as_str().unwrap_or_default().into(),
                certificate: content.into(),
                sealed_key,
                made_by: person.into(),
                made_ms: now,
                profile: None,
            };
            save(env, &identity).await?;
            identity
        }
    };

    let day = worker::js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_default();
    let name = format!("pigeon {BUNDLE_ID} {}", day.get(..10).unwrap_or_default());
    let made = apple::post(
        &token,
        "/v1/profiles",
        &profile_request(&name, &bundle, &identity.id),
    )
    .await
    .map_err(|e| {
        bad(&format!(
            "the certificate {} is made and kept, but Apple did not make the profile ({e}); \
                 press Create signing identity again to retry the profile",
            identity.serial
        ))
    })?;
    let d = &made["data"];
    let a = &d["attributes"];
    identity.profile = Some(Profile {
        id: d["id"].as_str().unwrap_or_default().into(),
        uuid: a["uuid"].as_str().unwrap_or_default().into(),
        name: a["name"].as_str().unwrap_or(&name).into(),
        expires: a["expirationDate"].as_str().unwrap_or_default().into(),
        content: a["profileContent"].as_str().unwrap_or_default().into(),
    });
    save(env, &identity).await?;
    Ok(format!(
        "pigeon's signing identity is made: certificate {} until {}, profile \"{}\". The key is \
         kept sealed; releases will use it from phase 4.",
        identity.serial, identity.expires, name
    ))
}

/// The capability. Everything it knows is read from the slots, the Directory and Apple.
pub struct Apple {
    /// Approves the console writes.
    pub passkeys: Passkeys,
}

impl Apple {
    async fn serve(&self, cx: &Context<'_>, mut req: Request) -> Result<Response> {
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
        let path = req.path();
        let (write, step) = path
            .strip_prefix("/apple/")
            .and_then(|p| p.split_once('/'))
            .unwrap_or_default();
        let answer = match (purpose(write), step) {
            (Some(p), "begin") => self.passkeys.sign_in_begin(cx.env, &p, None).await,
            (Some(p), "finish") => {
                async {
                    self.passkeys
                        .sign_in_finish(cx.env, &p, &body, None)
                        .await?;
                    let message = match write {
                        "adopt" => adopt(cx.env).await?,
                        _ => create_identity(cx, &person.name).await?,
                    };
                    Ok(json!({ "message": message }))
                }
                .await
            }
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

impl Capability for Apple {
    fn name(&self) -> &'static str {
        "apple"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: "apple_status",
            description: "Read-only: which Apple slots are readable, whether the App Store Connect \
                          key signs in, and what Apple holds for dev.pigeon.app: the bundle id, \
                          the app record and its highest build number, the distribution \
                          certificates and App Store profiles (names, serials, expiry; never \
                          keys or profile bytes). With the slots unset it says how to set them.",
            input_schema: json!({ "type": "object", "properties": {} }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "apple_status" => status(cx.env).await,
                other => Err(crate::unknown(other)),
            }
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        let slot = |name: &str, purpose: &str| Slot {
            name: name.into(),
            purpose: purpose.into(),
            scopes: &[secrets_store::WORKERS],
        };
        vec![
            slot(
                TEAM_ID,
                "The Apple Developer team id: 10 characters, on developer.apple.com under \
                 Membership details (docs/plans/ios.md, follow-up 1).",
            ),
            slot(
                ASC_ISSUER_ID,
                "The App Store Connect issuer id: a UUID, in App Store Connect under Users and \
                 Access, Integrations, App Store Connect API, above the keys.",
            ),
            slot(
                ASC_KEY_ID,
                "The App Store Connect Team API key's id: 10 characters, beside the key in the \
                 same list.",
            ),
            slot(
                ASC_KEY,
                "The App Store Connect Team API key itself: paste the whole .p8 file (role App \
                 Manager), then delete the download. Apple lets you download it once. pigeon \
                 checks it signs before keeping it.",
            ),
        ]
    }

    fn check_secret<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move { Ok(Value::String(check(cx.env, name, value).await?)) })
    }

    fn secret_expiry<'a>(&'a self, cx: &'a Context<'a>, name: &'a str) -> Expiring<'a> {
        // Team API keys and the ids do not expire. The key's row carries the earliest expiry
        // among the certificates and profiles pigeon made, so the console's warnings cover them.
        Box::pin(async move {
            if name != ASC_KEY {
                return Ok(Expiry::Never);
            }
            let held = identities(cx.env).await?;
            Ok(held
                .iter()
                .filter_map(Identity::expires_ms)
                .min()
                .map_or(Expiry::Never, |at_ms| Expiry::At { at_ms }))
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/apple/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

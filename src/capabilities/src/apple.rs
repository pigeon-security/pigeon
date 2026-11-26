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
//! The release step (`release::identity`) ensures Push: it adds PUSH_NOTIFICATIONS to the bundle
//! id if missing, and makes a new profile for the same certificate if the held one lacks it or
//! is not ACTIVE. It never deletes or changes a certificate or profile.
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
pub const SLOTS: [&str; 6] = [
    TEAM_ID,
    ASC_ISSUER_ID,
    ASC_KEY_ID,
    ASC_KEY,
    APNS_KEY_ID,
    APNS_KEY,
];

/// The APNs provider key (`.p8`), for push notifications.
pub const APNS_KEY: &str = "APNS_KEY";
/// The APNs provider key's id.
pub const APNS_KEY_ID: &str = "APNS_KEY_ID";

/// What to do while push is off.
pub const APNS_SETUP: &str = "Push is not set up. On the console's secrets page, set \
APPLE_TEAM_ID, APNS_KEY_ID and APNS_KEY (an APNs key from developer.apple.com, Keys; \
docs/plans/ios.md, follow-up 5). Nothing is sent until all three are readable.";

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
                "push": profile_has_push(&p.content),
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

/// True when a profile (base64, as Apple gives it) grants `aps-environment`, that is Push
/// Notifications. The plist sits in clear text inside the CMS envelope. False on bad base64.
pub fn profile_has_push(content_b64: &str) -> bool {
    let key = b"<key>aps-environment</key>";
    apple::unb64(content_b64).is_some_and(|b| b.windows(key.len()).any(|w| w == key))
}

/// The associated data that binds a sealed key to its certificate.
pub fn seal_aad(certificate_id: &str) -> String {
    format!("apple:identity:{certificate_id}")
}

/// The apple-app-site-association document for `team`: it lets `dev.pigeon.app` use this host
/// for passkeys (`webcredentials`) and claims `/app/oauth` as its sign-in callback (`applinks`),
/// as the app's associated-domains entitlement names both. None unless `team` is a team id, so a
/// bad slot serves nothing.
pub fn aasa_document(team: &str) -> Option<Value> {
    let app = format!("{team}.{BUNDLE_ID}");
    apple::is_apple_id(team).then(|| {
        json!({
            "applinks": { "details": [{ "appIDs": [app], "components": [{ "/": "/app/oauth*" }] }] },
            "webcredentials": { "apps": [app] },
        })
    })
}

/// The AASA document from `APPLE_TEAM_ID`, or None while it is unset.
pub async fn aasa(env: &Env) -> Option<Value> {
    aasa_document(&secret(env, TEAM_ID).await?)
}

/// What `/app/oauth` shows when iOS did not hand the sign-in back to the app: never the code.
pub const APP_RETURN_PAGE: &str = "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Pigeon</title><body style=\"font:16px -apple-system,system-ui,sans-serif;margin:3rem 1.5rem;text-align:center\"><p>Sign-in is done here.</p><p>Return to the Pigeon app. If it still shows the sign-in screen, close this page and try again.</p></body>";

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
        APNS_KEY_ID => {
            if !apple::is_apple_id(v) {
                return Err(bad(
                    "an APNs key id is 10 characters of A-Z and 0-9 (Keys, beside the key)",
                ));
            }
            Ok("a key id in the right form".into())
        }
        APNS_KEY => {
            let der = apple::p8_der(v).ok_or_else(|| {
                bad("paste the whole .p8 file, from -----BEGIN PRIVATE KEY----- to -----END PRIVATE KEY-----")
            })?;
            let now = worker::Date::now().as_millis() / 1000;
            apple::sign_jwt(
                &der,
                "AAAAAAAAAA",
                "00000000-0000-0000-0000-000000000000",
                now,
            )
            .await?;
            Ok("a P-256 key that signs; APNs checks it on the first send".into())
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

/// Adds the capabilities in `wanted` that `have` lacks to the bundle id `id`, one POST each.
/// Answers the ones it added.
async fn add_missing<'a>(
    token: &str,
    id: &str,
    wanted: &[&'a str],
    have: &[String],
) -> Result<Vec<&'a str>> {
    let add = missing(wanted, have);
    for cap in &add {
        apple::post(
            token,
            "/v1/bundleIdCapabilities",
            &json!({ "data": { "type": "bundleIdCapabilities",
                "attributes": { "capabilityType": cap },
                "relationships": { "bundleId": { "data": { "type": "bundleIds", "id": id } } },
            } }),
        )
        .await
        .map_err(|e| bad(&format!("adding {cap} to {BUNDLE_ID}: {e}")))?;
    }
    Ok(add)
}

/// Reads the bundle id's capabilities and adds those in `wanted` that are missing.
async fn add_capabilities(token: &str, id: &str, wanted: &[&str]) -> Result<()> {
    let have = capabilities(token, id)
        .await
        .map_err(|e| bad(&format!("reading {BUNDLE_ID}'s capabilities: {e}")))?;
    add_missing(token, id, wanted, &have).await?;
    Ok(())
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
    let add = add_missing(&token, &id, &CAPABILITIES, &have).await?;
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
pub struct Apple;

impl Apple {
    async fn serve(&self, cx: &Context<'_>, req: Request) -> Result<Response> {
        if req.method() == Method::Get && req.path() == "/app/oauth" {
            // route: open
            let headers = Headers::new();
            headers.set("Content-Type", "text/html; charset=utf-8")?;
            headers.set("Cache-Control", "no-store")?;
            headers.set("Referrer-Policy", "no-referrer")?;
            return Ok(Response::ok(APP_RETURN_PAGE)?.with_headers(headers));
        }
        if req.method() != Method::Post {
            return Response::error("not found", 404);
        }
        if req.path().starts_with("/apple/release/") {
            // route: job
            return release::serve(cx, req).await;
        }
        Response::error("not found", 404)
    }
}

impl Capability for Apple {
    fn name(&self) -> &'static str {
        "apple"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "apple",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        let mut tools = vec![
            release::tool(),
            Tool {
                name: "apple_status",
                description: "Read-only: which Apple slots are readable, whether the App Store Connect \
                          key signs in, and what Apple holds for dev.pigeon.app: the bundle id, \
                          the app record and its highest build number, the distribution \
                          certificates and App Store profiles (names, serials, expiry; never \
                          keys or profile bytes). With the slots unset it says how to set them.",
                input_schema: json!({ "type": "object", "properties": {} }),
            },
        ];
        tools.extend(push::tools());
        tools
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "apple_status" => status(cx.env).await,
                "ios_releases" => release::list(cx.env).await,
                t if t.starts_with("apns_") => push::call(cx, t, args).await,
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
            slot(
                APNS_KEY_ID,
                "The APNs key's id: 10 characters, on developer.apple.com under Keys, beside the \
                 key (docs/plans/ios.md, follow-up 5).",
            ),
            slot(
                APNS_KEY,
                "The APNs key itself: paste the whole .p8 file (a key with Apple Push \
                 Notifications service enabled), then delete the download. pigeon checks it \
                 signs before keeping it.",
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
        vec!["/apple/", "/app/oauth"]
    }

    /// `apple-adopt` and `apple-identity`, through the console's one write path, bound to the
    /// signed-in person's passkey.
    fn write_make<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            let person = w.person;
            let message = match w.action.id {
                "apple-adopt" => adopt(cx.env).await?,
                "apple-identity" => create_identity(cx, &person.name).await?,
                other => {
                    return Err(Error::RustError(format!("apple makes no write `{other}`")));
                }
            };
            Ok(json!({ "message": message }))
        })
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }
}

/// Push notifications (docs/plans/ios.md, phase 6): device registration, status and send. All of
/// it is off until `APNS_KEY`, `APNS_KEY_ID` and `APPLE_TEAM_ID` are readable. No answer, log or
/// audit line holds a device token.
pub mod push {
    use super::*;
    use crate::arg;
    use pigeon_core::apns;
    use worker::{Fetch, RequestInit};

    /// The push tools.
    pub fn tools() -> Vec<Tool> {
        vec![
            Tool {
                name: "apns_register",
                description: "Register this device for push: the caller must be a service user \
                              acting for a person. Replaces the caller's earlier token for the app.",
                input_schema: json!({ "type": "object", "required": ["app", "token", "environment"],
                    "properties": {
                        "app": { "type": "string" },
                        "token": { "type": "string" },
                        "environment": { "type": "string", "enum": ["development", "production"] },
                    } }),
            },
            Tool {
                name: "apns_unregister",
                description: "Remove the caller's push registration for an app.",
                input_schema: json!({ "type": "object", "required": ["app"],
                    "properties": { "app": { "type": "string" } } }),
            },
            Tool {
                name: "apns_status",
                description: "Read-only: whether push is set up, what to set and where, and how \
                              many devices are registered per environment (never a token). With \
                              probe true it sends an unsigned request to APNs' sandbox and \
                              answers the HTTP status, apns-id and reason, to show the Worker's \
                              fetch speaks HTTP/2 there. It needs no key.",
                input_schema: json!({ "type": "object", "properties": {
                    "app": { "type": "string" }, "probe": { "type": "boolean" } } }),
            },
            Tool {
                name: "apns_send",
                description: "Send a push to every device of a person for an app (the person \
                              argument, or the one the caller acts for). Answers row ids and \
                              outcomes, never a token. Says what to set while the APNs key is \
                              not readable.",
                input_schema: json!({ "type": "object", "required": ["app", "alert"],
                    "properties": {
                        "app": { "type": "string" },
                        "person": { "type": "string" },
                        "alert": { "type": "object", "required": ["title", "body"],
                            "properties": { "title": { "type": "string" },
                                            "body": { "type": "string" } } },
                        "data": { "type": "object", "additionalProperties": true },
                        "push_type": { "type": "string", "enum": ["alert", "background"] },
                        "collapse_id": { "type": "string" },
                    } }),
            },
        ]
    }

    /// True when the three secrets a send needs can be read.
    async fn configured(env: &Env) -> bool {
        secret(env, APNS_KEY).await.is_some()
            && secret(env, APNS_KEY_ID).await.is_some()
            && secret(env, TEAM_ID).await.is_some()
    }

    /// When true, a new approval that waits for a person's passkey sends that person an alert
    /// push. On since the person asked for it.
    pub const NOTIFY_APPROVALS: bool = true;

    /// The iOS notification category of an approval push (`NotificationRoute.approvalCategory`).
    pub const APPROVAL_CATEGORY: &str = "APPROVAL";

    /// The payload and headers of one alert push for the "pigeon" app, with a link in its data.
    pub fn alert_parts(
        title: &str,
        body: &str,
        link: &str,
        collapse_id: &str,
    ) -> Result<(String, Vec<(&'static str, String)>)> {
        // An approval push (collapse id `approval:<id>`) names its approval in the data and uses
        // the app's APPROVAL category, whose one Review action only opens the app: nothing is
        // decided from the notification.
        let approval = collapse_id
            .strip_prefix("approval:")
            .filter(|id| !id.is_empty());
        let data = match approval {
            Some(id) => json!({ "link": link, "approval": id }),
            None => json!({ "link": link }),
        };
        let mut payload = apns::payload(apns::PushType::Alert, Some((title, body)), Some(&data))
            .map_err(|e| bad(&e))?;
        if approval.is_some() {
            let mut v: Value = serde_json::from_str(&payload).map_err(|e| bad(&e.to_string()))?;
            v["aps"]["category"] = json!(APPROVAL_CATEGORY);
            payload = v.to_string();
            if payload.len() > apns::MAX_PAYLOAD {
                return Err(bad("the approval push is larger than APNs allows"));
            }
        }
        let headers = apns::headers("pigeon", apns::PushType::Alert, Some(collapse_id))
            .map_err(|e| bad(&e))?;
        Ok((payload, headers))
    }

    /// Sends an alert push to `person`'s devices for the "pigeon" app. When push is not set up
    /// it answers `{ "sent": 0, "configured": false }`. Answers carry no device token.
    pub async fn notify(
        env: &Env,
        person: &str,
        title: &str,
        body: &str,
        link: &str,
        collapse_id: &str,
    ) -> Result<Value> {
        if !configured(env).await {
            return Ok(json!({ "sent": 0, "configured": false }));
        }
        let (payload, headers) = alert_parts(title, body, link, collapse_id)?;
        send(env, "pigeon", person, &payload, &headers).await
    }

    fn declared(args: &Value) -> Result<&str> {
        let app = arg(args, "app")?;
        apns::bundle_id(app).ok_or_else(|| bad("that app is not declared"))?;
        Ok(app)
    }

    /// Runs push tool `tool`.
    pub async fn call(cx: &Context<'_>, tool: &str, args: &Value) -> Result<Value> {
        let env = cx.env;
        match tool {
            "apns_register" => {
                let app = declared(args)?;
                let token = arg(args, "token")?.trim().to_lowercase();
                let environment = arg(args, "environment")?;
                let caller = cx
                    .caller
                    .filter(|c| c.user.kind == "service")
                    .ok_or_else(|| bad("only a service user can register a device"))?;
                let person = caller
                    .acting_for
                    .as_ref()
                    .filter(|p| p.kind == "standard")
                    .ok_or_else(|| bad("this client is not linked to a person"))?;
                if !apns::is_environment(environment) {
                    return Err(bad("environment is development or production"));
                }
                if !apns::is_device_token(&token) {
                    return Err(bad("that is not a device token"));
                }
                directory(
                    env,
                    "apns.register",
                    json!({ "app": app, "person_id": person.id, "service_user_id": caller.user.id,
                        "token": token, "environment": environment }),
                )
                .await?;
                Ok(json!({ "registered": true, "app": app, "environment": environment }))
            }
            "apns_unregister" => {
                let app = declared(args)?;
                let caller = cx
                    .caller
                    .filter(|c| c.user.kind == "service")
                    .ok_or_else(|| bad("only a service user can unregister a device"))?;
                directory(
                    env,
                    "apns.unregister",
                    json!({ "app": app, "service_user_id": caller.user.id }),
                )
                .await?;
                Ok(json!({ "registered": false, "app": app }))
            }
            "apns_status" => status(env, args).await,
            "apns_send" => {
                if !configured(env).await {
                    return Ok(json!({ "sent": 0, "configured": false, "next": APNS_SETUP }));
                }
                let app = declared(args)?;
                let push_type =
                    apns::PushType::parse(args["push_type"].as_str().unwrap_or("alert"))
                        .ok_or_else(|| bad("push_type is alert or background"))?;
                let title = args["alert"]["title"].as_str().unwrap_or_default();
                let body = args["alert"]["body"].as_str().unwrap_or_default();
                let payload = apns::payload(push_type, Some((title, body)), args.get("data"))
                    .map_err(|e| bad(&e))?;
                let headers = apns::headers(app, push_type, args["collapse_id"].as_str())
                    .map_err(|e| bad(&e))?;
                let person = match args["person"].as_str().filter(|p| !p.is_empty()) {
                    Some(p) => p.to_string(),
                    None => {
                        cx.caller
                            .and_then(|c| {
                                c.acting_for.as_ref().map(|p| p.id.clone()).or_else(|| {
                                    (c.user.kind == "standard").then(|| c.user.id.clone())
                                })
                            })
                            .ok_or_else(|| bad("name a person: the caller does not act for one"))?
                    }
                };
                send(env, app, &person, &payload, &headers).await
            }
            other => Err(crate::unknown(other)),
        }
    }

    /// Where the provider token is cached in the Directory. Never answered, logged or audited.
    const TOKEN_KEY: &str = "apns:provider-token";
    /// How long the cached provider token is kept: 55 minutes.
    const TOKEN_TTL_MS: i64 = 55 * 60 * 1000;
    /// How long a device is skipped after APNs's 429 TooManyRequests: 10 minutes.
    const BACKOFF_MS: i64 = 10 * 60 * 1000;
    /// A day, in milliseconds: how long send times are kept.
    const DAY_MS: i64 = 24 * 3_600_000;
    /// Inline retries for a 500 or 503 answer.
    const RETRIES: u32 = 2;

    fn now_secs() -> u64 {
        worker::Date::now().as_millis() / 1000
    }

    /// Writes a Directory setting that expires after `ttl_ms`.
    async fn put(env: &Env, key: &str, value: &str, ttl_ms: i64) -> Result<()> {
        directory(
            env,
            "setting.put",
            json!({ "key": key, "value": value, "ttl_ms": ttl_ms }),
        )
        .await?;
        Ok(())
    }

    /// The APNs provider token. It reuses the cached one while `apns::reuse_token` says so.
    /// With `forced` it signs a new one, unless the cached one for the same key is under 20
    /// minutes old: then it answers None, because APNs would refuse the refresh.
    async fn provider_token(env: &Env, forced: bool) -> Result<Option<String>> {
        let (Some(key), Some(key_id), Some(team)) = (
            secret(env, APNS_KEY).await,
            secret(env, APNS_KEY_ID).await,
            secret(env, TEAM_ID).await,
        ) else {
            return Err(bad(APNS_SETUP));
        };
        let now = now_secs();
        let cached: Option<Value> = setting(env, TOKEN_KEY)
            .await?
            .and_then(|t| serde_json::from_str(&t).ok());
        let issued = cached.as_ref().and_then(|c| c["issued_at"].as_u64());
        let same = cached
            .as_ref()
            .is_some_and(|c| c["key_id"] == key_id.as_str() && c["team"] == team.as_str());
        if apns::reuse_token(issued, same, now, forced)
            && let Some(t) = cached.as_ref().and_then(|c| c["token"].as_str())
        {
            return Ok(Some(t.to_string()));
        }
        if forced && same && issued.is_some_and(|at| !apns::may_refresh(at, now)) {
            return Ok(None);
        }
        let der = apple::p8_der(&key)
            .ok_or_else(|| bad("the stored APNS_KEY is not a .p8 key; set it again"))?;
        let token = apple::sign_es256(&der, &apns::signing_input(&key_id, &team, now)).await?;
        let record = json!({ "token": token, "key_id": key_id, "team": team, "issued_at": now });
        put(env, TOKEN_KEY, &record.to_string(), TOKEN_TTL_MS).await?;
        Ok(Some(token))
    }

    /// What APNs answered for one device.
    struct Answer {
        status: u16,
        apns_id: Option<String>,
        reason: Option<String>,
        timestamp: Option<u64>,
    }

    /// One POST to APNs. Holds the device token only for the URL; nothing here is logged.
    async fn post(
        host: &str,
        device: &str,
        bearer: &str,
        headers: &[(&'static str, String)],
        payload: &str,
    ) -> Result<Answer> {
        let h = Headers::new();
        h.set("authorization", &format!("bearer {bearer}"))?;
        h.set("content-type", "application/json")?;
        for (k, v) in headers {
            h.set(k, v)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post);
        init.with_headers(h);
        init.with_body(Some(payload.into()));
        let request = Request::new_with_init(&format!("https://{host}/3/device/{device}"), &init)?;
        let mut res = Fetch::Request(request).send().await?;
        let status = res.status_code();
        let apns_id = res.headers().get("apns-id")?;
        let text = res.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or_default();
        Ok(Answer {
            status,
            apns_id,
            reason: body["reason"].as_str().map(String::from),
            timestamp: body["timestamp"].as_u64(),
        })
    }

    /// Sends to every device of `person` for `app`. Answers row ids and outcomes, never a device
    /// token. Departures from docs/plans/ios.md, §3: a 400 BadDeviceToken drops the row at once
    /// (the app registers again on every launch, so a wrong-environment token comes back
    /// corrected), and a 500 or 503 is retried inline twice with a short delay, not through the
    /// platform's queue. The rate limit is counted per call, in Directory settings.
    async fn send(
        env: &Env,
        app: &str,
        person: &str,
        payload: &str,
        headers: &[(&'static str, String)],
    ) -> Result<Value> {
        let now = now_secs();
        let sends_key = format!("apns:sends:{app}:{person}");
        let mut past: Vec<u64> = setting(env, &sends_key)
            .await?
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        match apns::rate_limit(&past, now) {
            Err(apns::Limited::Minute) => {
                return Err(bad("too many pushes for this person: 30 a minute"));
            }
            Err(apns::Limited::Day) => {
                return Err(bad("too many pushes for this person: 500 a day"));
            }
            Ok(()) => {}
        }
        past.retain(|t| now.saturating_sub(*t) < 86_400);
        past.push(now);
        put(env, &sends_key, &serde_json::to_string(&past)?, DAY_MS).await?;

        let rows = directory(
            env,
            "apns.devices",
            json!({ "app": app, "person_id": person }),
        )
        .await?;
        let mut bearer = provider_token(env, false)
            .await?
            .ok_or_else(|| bad("no provider token"))?;
        let mut refreshed = false;
        let (mut sent, mut failed) = (0u32, 0u32);
        let mut out: Vec<Value> = Vec::new();
        let mut stopped: Option<String> = None;
        'devices: for row in rows.as_array().into_iter().flatten() {
            let id = row["id"].as_str().unwrap_or_default();
            let environment = row["environment"].as_str().unwrap_or_default();
            let entry = |status: u16, reason: Option<&str>, apns_id: Option<&str>| {
                json!({ "id": id, "environment": environment, "status": status,
                    "reason": reason, "apns_id": apns_id })
            };
            let Some(host) = apns::host(environment) else {
                failed += 1;
                out.push(entry(0, Some("unknown environment"), None));
                continue;
            };
            let backoff_key = format!("apns:backoff:{id}");
            if setting(env, &backoff_key).await?.is_some() {
                failed += 1;
                out.push(entry(0, Some("backed off"), None));
                continue;
            }
            let device = row["token"].as_str().unwrap_or_default();
            let created_at = row["created_at"].as_u64().unwrap_or_default();
            let mut tries = 0;
            loop {
                let Ok(a) = post(host, device, &bearer, headers, payload).await else {
                    failed += 1;
                    out.push(entry(0, Some("the request to APNs failed"), None));
                    continue 'devices;
                };
                let reason = a.reason.as_deref();
                let apns_id = a.apns_id.as_deref();
                let mut fail = |out: &mut Vec<Value>| {
                    failed += 1;
                    out.push(entry(a.status, reason, apns_id));
                };
                match apns::action(a.status, reason.unwrap_or_default()) {
                    apns::Action::Ok => {
                        directory(env, "apns.ok", json!({ "id": id })).await?;
                        sent += 1;
                        out.push(entry(a.status, reason, apns_id));
                    }
                    apns::Action::MarkDevice => {
                        directory(env, "apns.forget", json!({ "id": id })).await?;
                        fail(&mut out);
                    }
                    apns::Action::DeleteIfNewer => {
                        if apns::drop_on_gone(a.timestamp, created_at) {
                            directory(env, "apns.forget", json!({ "id": id })).await?;
                        } else {
                            let why = reason.unwrap_or("Unregistered");
                            directory(env, "apns.mark", json!({ "id": id, "error": why })).await?;
                        }
                        fail(&mut out);
                    }
                    apns::Action::RefreshToken => {
                        if !refreshed {
                            refreshed = true;
                            if let Some(t) = provider_token(env, true).await? {
                                bearer = t;
                                continue;
                            }
                        }
                        fail(&mut out);
                        stopped = Some(reason.unwrap_or("provider token refused").to_string());
                        break 'devices;
                    }
                    apns::Action::StopReport | apns::Action::KeepTokenReport => {
                        fail(&mut out);
                        stopped = Some(reason.unwrap_or("APNs refused").to_string());
                        break 'devices;
                    }
                    apns::Action::RefuseCaller => {
                        return Err(bad(&format!(
                            "APNs refused the push: {}",
                            reason.unwrap_or("payload")
                        )));
                    }
                    apns::Action::BackOff => {
                        put(env, &backoff_key, "1", BACKOFF_MS).await?;
                        fail(&mut out);
                    }
                    apns::Action::Retry => {
                        if tries < RETRIES {
                            tries += 1;
                            let wait = std::time::Duration::from_millis(300 * u64::from(tries));
                            worker::Delay::from(wait).await;
                            continue;
                        }
                        let why = reason.unwrap_or("ServiceUnavailable");
                        directory(env, "apns.mark", json!({ "id": id, "error": why })).await?;
                        fail(&mut out);
                    }
                    apns::Action::Report => {
                        let why = reason.unwrap_or("unknown");
                        directory(env, "apns.mark", json!({ "id": id, "error": why })).await?;
                        fail(&mut out);
                    }
                }
                break;
            }
        }
        let mut answer = json!({ "sent": sent, "failed": failed, "configured": true,
            "devices": out });
        if let Some(why) = stopped {
            answer["stopped"] = json!(why);
        }
        Ok(answer)
    }

    /// `apns_status`: setup state, counts per environment, and the optional HTTP/2 probe.
    async fn status(env: &Env, args: &Value) -> Result<Value> {
        let app = args["app"].as_str().unwrap_or("pigeon");
        apns::bundle_id(app).ok_or_else(|| bad("that app is not declared"))?;
        let ready = configured(env).await;
        let rows = directory(env, "apns.devices", json!({ "app": app })).await?;
        let count = |e: &str| {
            rows.as_array()
                .map_or(0, |r| r.iter().filter(|d| d["environment"] == e).count())
        };
        let mut out = json!({
            "app": app,
            "configured": ready,
            "devices": { "development": count("development"), "production": count("production") },
        });
        if !ready {
            out["next"] = json!(APNS_SETUP);
        }
        if args["probe"].as_bool().unwrap_or(false) {
            out["probe"] = probe().await;
        }
        Ok(out)
    }

    /// An unsigned POST to APNs' sandbox. A 403 MissingProviderToken or 400 BadDeviceToken with
    /// an apns-id means the Worker's fetch speaks HTTP/2 to APNs.
    async fn probe() -> Value {
        let url = format!(
            "https://api.sandbox.push.apple.com/3/device/{}",
            "0".repeat(64)
        );
        let go = async {
            let mut init = RequestInit::new();
            init.with_method(Method::Post);
            init.with_body(Some("{}".into()));
            let request = Request::new_with_init(&url, &init)?;
            let mut res = Fetch::Request(request).send().await?;
            let status = res.status_code();
            let id = res.headers().get("apns-id")?;
            let text = res.text().await.unwrap_or_default();
            let reason = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["reason"].as_str().map(String::from));
            Ok::<Value, Error>(json!({ "status": status, "apns_id": id, "reason": reason }))
        };
        go.await
            .unwrap_or_else(|e| json!({ "error": e.to_string() }))
    }
}

/// iOS releases (docs/plans/ios.md, phase 4). A release is one run of CI job `ios-release` at
/// main's head when it starts, on Buildkite's hosted macOS. The job's own token is its only
/// credential; with it the job asks core, in order and once each:
///
/// 1. `/apple/release/start`: its build number, above every build Apple holds for the app. Main's
///    head is checked here only; later steps trust the release record's commit;
/// 2. `/apple/release/identity`: pigeon's signing identity (certificate, key, profile), once;
/// 3. `/apple/release/upload`: core takes the signed IPA and uploads it to App Store Connect
///    itself, with Apple's `buildUploads` API, so the Mac never holds the API key or a token.
///
/// `ios_releases` lists them and reads Apple's processing state. Nothing here touches a
/// certificate or profile pigeon did not make.
pub mod release {
    use super::*;

    /// The CI job a release runs as.
    pub const JOB: &str = "ios-release";
    /// The branch releases are cut from.
    pub const BRANCH: &str = "refs/heads/main";
    /// The Directory key listing the releases' job ids, oldest first.
    const LIST: &str = "apple:releases";
    /// The last build number given out.
    const LAST: &str = "apple:release:last_number";
    /// How long a release record stays usable after its last update: 2 hours, in milliseconds.
    pub const EXPIRY: i64 = 2 * 3_600_000;
    /// How many releases `ios_releases` shows.
    const SHOWN: usize = 10;

    /// One release, kept by its CI job id.
    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    pub struct Release {
        pub job: String,
        pub commit: String,
        pub build_number: u64,
        /// `started`, `signing`, `uploading`, `processing`, `complete` or `failed`.
        pub state: String,
        pub version: Option<String>,
        /// The certificate the identity handed out was.
        pub certificate: Option<String>,
        /// Apple's buildUpload id, once uploaded.
        pub upload: Option<String>,
        /// Apple's processing state of the build, once it has one.
        pub processing: Option<String>,
        pub detail: Option<String>,
        pub started_ms: i64,
        pub updated_ms: i64,
    }

    /// Whether a release in `state` still waits on its job or on Apple.
    pub fn open(state: &str) -> bool {
        matches!(state, "started" | "signing" | "uploading" | "processing")
    }

    /// The next build number: above the last pigeon gave and above Apple's highest.
    pub fn next_number(last: Option<u64>, apple_highest: Option<u64>) -> u64 {
        last.unwrap_or(0).max(apple_highest.unwrap_or(0)) + 1
    }

    /// The step a release in `state` may take next, or why not. `step` is `identity` or `upload`.
    pub fn may(state: &str, step: &str) -> std::result::Result<(), String> {
        match (step, state) {
            ("identity", "started") | ("upload", "signing") => Ok(()),
            ("identity", _) => Err(format!(
                "this release is {state}: the identity is handed out once, right after start"
            )),
            _ => Err(format!("this release is {state}, not waiting for its IPA")),
        }
    }

    /// Whether `step` may go ahead for `job` at `commit`. Main's head matters only at `start`:
    /// a job answering `start` again, or any later step, is trusted by its release record, which
    /// proves the release started at main's head. `main_head` is ignored after `start`.
    pub fn admit(
        step: &str,
        job: &str,
        commit: &str,
        main_head: Option<&str>,
        record: Option<&Release>,
        now_ms: i64,
    ) -> std::result::Result<(), String> {
        match step {
            "start" => {
                if record.is_some_and(|r| r.job == job && r.commit.eq_ignore_ascii_case(commit)) {
                    return Ok(());
                }
                match main_head {
                    Some(h) if h.eq_ignore_ascii_case(commit) => Ok(()),
                    _ => Err("a release is cut only from main's head".into()),
                }
            }
            "identity" | "upload" => {
                let r = record.ok_or("no release for this job: start it first")?;
                if r.job != job {
                    return Err("the release belongs to another job".into());
                }
                if !r.commit.eq_ignore_ascii_case(commit) {
                    return Err("the release's commit is not this job's commit".into());
                }
                if !open(&r.state) {
                    return Err("the release is over".into());
                }
                if now_ms - r.updated_ms > EXPIRY {
                    return Err("the release has expired".into());
                }
                Ok(())
            }
            _ => Err("not a release step".into()),
        }
    }

    /// The tool.
    pub fn tool() -> Tool {
        Tool {
            name: "ios_releases",
            description: "Read-only: the latest iOS releases (CI job ios-release, cut at main's head): \
                          each one's build number, commit, state and Apple's processing state, \
                          read fresh from App Store Connect for those still processing. Start a \
                          release with ci_submit of job ios-release at main's head.",
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }

    async fn load(env: &Env, job: &str) -> Result<Option<Release>> {
        Ok(match setting(env, &format!("apple:release:{job}")).await? {
            Some(t) => Some(serde_json::from_str(&t)?),
            None => None,
        })
    }

    async fn store(env: &Env, r: &Release) -> Result<()> {
        keep(
            env,
            &format!("apple:release:{}", r.job),
            &serde_json::to_string(r)?,
        )
        .await
    }

    async fn jobs(env: &Env) -> Result<Vec<String>> {
        Ok(match setting(env, LIST).await? {
            Some(t) => serde_json::from_str(&t)?,
            None => Vec::new(),
        })
    }

    /// The release job a request's token stands for: a live job token of a running `ios-release`
    /// job on Buildkite (hosted agents only, which the connector enforces). It answers the job id,
    /// the commit and the repository; `serve` checks main's head, at `start` only.
    async fn caller(
        cx: &Context<'_>,
        req: &Request,
    ) -> std::result::Result<(String, String, String), (u16, String)> {
        let auth = req
            .headers()
            .get("Authorization")
            .ok()
            .flatten()
            .unwrap_or_default();
        let presented = auth.strip_prefix("Bearer ").unwrap_or_default();
        let deny = |code: u16, why: &str| (code, why.to_string());
        let grant = crate::ci::job_grant(cx.env, presented)
            .await
            .map_err(|e| deny(500, &e.to_string()))?
            .ok_or_else(|| deny(401, "a live CI job token is needed"))?;
        let job = crate::ci::job_status(cx.env, &grant.job)
            .await
            .map_err(|e| deny(500, &e.to_string()))?;
        if job["job"] != JOB {
            return Err(deny(403, "only CI job ios-release makes releases"));
        }
        if job["connector"] != "buildkite" || job["state"] != "running" {
            return Err(deny(
                403,
                "a release runs on Buildkite's hosted macOS, while it runs",
            ));
        }
        Ok((grant.job, grant.commit, grant.repo))
    }

    pub async fn serve(cx: &Context<'_>, mut req: Request) -> Result<Response> {
        let step = req.path().trim_start_matches("/apple/release/").to_string();
        let (job, commit, repo) = match caller(cx, &req).await {
            Ok(c) => c,
            Err((code, why)) => return Response::error(why, code),
        };
        if !matches!(step.as_str(), "start" | "identity" | "upload") {
            return Response::error("not found", 404);
        }
        let record = load(cx.env, &job).await?;
        let mut head = None;
        if step == "start"
            && record
                .as_ref()
                .is_none_or(|r| !r.commit.eq_ignore_ascii_case(&commit))
        {
            let Some((ns, name)) = repo.split_once('/') else {
                return Response::error("not a repository", 403);
            };
            head = match crate::git_proxy::tip(cx.account, ns, name, BRANCH).await {
                Ok(h) => h,
                Err(e) => return Response::error(e.to_string(), 500),
            };
        }
        if let Err(why) = admit(
            &step,
            &job,
            &commit,
            head.as_deref(),
            record.as_ref(),
            now_ms(),
        ) {
            return Response::error(why, 403);
        }
        let answer = match step.as_str() {
            "start" => start(cx.env, &job, &commit).await,
            "identity" => identity(cx.env, &job).await,
            "upload" => {
                let version = req
                    .url()?
                    .query_pairs()
                    .find(|(k, _)| k == "version")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_default();
                let bytes = req.bytes().await?;
                upload(cx.env, &job, &version, &bytes).await
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

    /// Starts (or, for the same job, answers again) a release with its build number.
    async fn start(env: &Env, job: &str, commit: &str) -> Result<Value> {
        if let Some(r) = load(env, job).await? {
            return Ok(json!({ "build_number": r.build_number, "state": r.state }));
        }
        let now = now_ms();
        let mut list = jobs(env).await?;
        for other in &list {
            let Some(mut o) = load(env, other).await? else {
                continue;
            };
            if !matches!(o.state.as_str(), "started" | "signing" | "uploading") {
                continue;
            }
            // A release whose job has ended without uploading is over: it failed.
            let ended = crate::ci::job_status(env, &o.job)
                .await
                .map(|j| j["state"] != "running")
                .unwrap_or(true);
            if ended || now - o.updated_ms > EXPIRY {
                o.detail = Some(format!("its job ended while the release was {}", o.state));
                o.state = "failed".into();
                o.updated_ms = now;
                store(env, &o).await?;
                continue;
            }
            return Err(bad(&format!(
                "release {} (build {}) is still {}",
                o.job, o.build_number, o.state
            )));
        }
        let token = token(env).await.ok_or_else(|| bad(SETUP))??;
        let apps = apple::get(
            &token,
            &format!("/v1/apps?filter[bundleId]={BUNDLE_ID}&fields[apps]=bundleId"),
        )
        .await?;
        let app = apps["data"][0]["id"]
            .as_str()
            .ok_or_else(|| bad("App Store Connect has no app for dev.pigeon.app"))?;
        let builds = apple::get(
            &token,
            &format!("/v1/builds?filter[app]={app}&fields[builds]=version&limit=200"),
        )
        .await?;
        let highest = builds["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["attributes"]["version"].as_str()?.parse::<u64>().ok())
            .max();
        let last = setting(env, LAST).await?.and_then(|t| t.parse().ok());
        let number = next_number(last, highest);
        keep(env, LAST, &number.to_string()).await?;
        let r = Release {
            job: job.into(),
            commit: commit.into(),
            build_number: number,
            state: "started".into(),
            version: None,
            certificate: None,
            upload: None,
            processing: None,
            detail: None,
            started_ms: now,
            updated_ms: now,
        };
        store(env, &r).await?;
        list.push(job.into());
        keep(env, LIST, &serde_json::to_string(&list)?).await?;
        Ok(json!({ "build_number": number, "state": r.state }))
    }

    /// Makes sure `id` holds a profile with Push Notifications, and returns it. Adds the
    /// capability to the bundle id if it is missing. If the held profile lacks push or Apple says
    /// it is not ACTIVE, makes a new profile for the same certificate and keeps it in `id`. Never
    /// deletes or changes any certificate or profile.
    async fn ensure_push(env: &Env, id: &mut Identity) -> Result<()> {
        let token = token(env).await.ok_or_else(|| bad(SETUP))??;
        let bundle = bundle_id(&token)
            .await?
            .ok_or_else(|| bad(&format!("Apple has no {BUNDLE_ID}: press Adopt first")))?;
        add_capabilities(&token, &bundle, &["PUSH_NOTIFICATIONS"]).await?;
        let held = id.profile.as_ref().ok_or_else(|| bad("no profile"))?;
        let mut usable = profile_has_push(&held.content);
        if usable {
            let state = apple::get(
                &token,
                &format!("/v1/profiles/{}?fields[profiles]=profileState", held.id),
            )
            .await
            .map_err(|e| bad(&format!("reading the profile's state: {e}")))?;
            usable = state["data"]["attributes"]["profileState"].as_str() == Some("ACTIVE");
        }
        if usable {
            return Ok(());
        }
        let stamp = worker::js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        let day = stamp.get(..10).unwrap_or_default();
        let hhmm = format!(
            "{}{}",
            stamp.get(11..13).unwrap_or_default(),
            stamp.get(14..16).unwrap_or_default()
        );
        let name = format!("pigeon {BUNDLE_ID} {day}");
        let (name, made) = match apple::post(
            &token,
            "/v1/profiles",
            &profile_request(&name, &bundle, &id.id),
        )
        .await
        {
            Ok(m) => (name, m),
            Err(_) => {
                let again = format!("{name} {hhmm}");
                let m = apple::post(
                    &token,
                    "/v1/profiles",
                    &profile_request(&again, &bundle, &id.id),
                )
                .await
                .map_err(|e| bad(&format!("Apple did not make a profile with Push: {e}")))?;
                (again, m)
            }
        };
        let d = &made["data"];
        let a = &d["attributes"];
        let content = a["profileContent"].as_str().unwrap_or_default();
        if d["id"].as_str().is_none() || !profile_has_push(content) {
            return Err(bad(
                "Apple made a profile but it has no id or no Push Notifications",
            ));
        }
        id.profile = Some(Profile {
            id: d["id"].as_str().unwrap_or_default().into(),
            uuid: a["uuid"].as_str().unwrap_or_default().into(),
            name: a["name"].as_str().unwrap_or(&name).into(),
            expires: a["expirationDate"].as_str().unwrap_or_default().into(),
            content: content.into(),
        });
        save(env, id).await
    }

    /// Hands the release its signing identity, once: the newest pigeon identity with a profile
    /// that has more than a day left. The profile is made to carry Push Notifications first
    /// (`ensure_push`), before the release is marked "signing".
    async fn identity(env: &Env, job: &str) -> Result<Value> {
        let mut r = load(env, job)
            .await?
            .ok_or_else(|| bad("start the release first"))?;
        may(&r.state, "identity").map_err(|e| bad(&e))?;
        let key = seal_key(env)
            .await
            .ok_or_else(|| bad("APPLE_SEAL_KEY is not readable"))?;
        let now = now_ms();
        let held = identities(env).await?;
        let id = held
            .iter()
            .rev()
            .find(|i| i.profile.is_some() && i.expires_ms().is_some_and(|t| t > now + 86_400_000))
            .cloned()
            .ok_or_else(|| {
                bad("pigeon holds no signing identity: press Create signing identity")
            })?;
        let mut id = id;
        ensure_push(env, &mut id).await?;
        let id = &id;
        let profile = id.profile.as_ref().ok_or_else(|| bad("no profile"))?;
        let pkcs8 = crypto::open(&key, &id.sealed_key, seal_aad(&id.id).as_bytes()).await?;
        let team = secret(env, TEAM_ID)
            .await
            .ok_or_else(|| bad("APPLE_TEAM_ID is not readable"))?;
        // Marked handed out before it is answered: a lost answer means a new release, never a
        // second copy.
        r.state = "signing".into();
        r.certificate = Some(id.id.clone());
        r.updated_ms = now;
        store(env, &r).await?;
        Ok(json!({
            "build_number": r.build_number,
            "team_id": team,
            "bundle_id": BUNDLE_ID,
            "certificate": id.certificate,
            "private_key": apple::pkcs8_pem(&pkcs8),
            "profile": profile.content,
            "profile_uuid": profile.uuid,
            "profile_name": profile.name,
        }))
    }

    /// Takes the signed IPA and uploads it to App Store Connect as the release's build.
    async fn upload(env: &Env, job: &str, version: &str, ipa: &[u8]) -> Result<Value> {
        let mut r = load(env, job)
            .await?
            .ok_or_else(|| bad("start the release first"))?;
        may(&r.state, "upload").map_err(|e| bad(&e))?;
        if !apple::is_version(version) {
            return Err(bad(
                "upload?version= must be the app's version, such as 0.2.0",
            ));
        }
        if ipa.len() < 1024 || !ipa.starts_with(b"PK") {
            return Err(bad("the body is not an IPA"));
        }
        r.state = "uploading".into();
        r.version = Some(version.into());
        r.updated_ms = now_ms();
        store(env, &r).await?;
        let done = send(env, &r, version, ipa).await;
        r.updated_ms = now_ms();
        match done {
            Ok(id) => {
                r.state = "processing".into();
                r.upload = Some(id.clone());
                store(env, &r).await?;
                Ok(json!({ "build_number": r.build_number, "upload": id, "state": r.state }))
            }
            Err(e) => {
                r.state = "failed".into();
                r.detail = Some(e.to_string());
                store(env, &r).await?;
                Err(e)
            }
        }
    }

    async fn send(env: &Env, r: &Release, version: &str, ipa: &[u8]) -> Result<String> {
        let token = token(env).await.ok_or_else(|| bad(SETUP))??;
        let apps = apple::get(
            &token,
            &format!("/v1/apps?filter[bundleId]={BUNDLE_ID}&fields[apps]=bundleId"),
        )
        .await?;
        let app = apps["data"][0]["id"]
            .as_str()
            .ok_or_else(|| bad("App Store Connect has no app for dev.pigeon.app"))?
            .to_string();
        let made = apple::post(
            &token,
            "/v1/buildUploads",
            &upload_request(&app, version, r.build_number),
        )
        .await
        .map_err(|e| bad(&format!("creating the build upload: {e}")))?;
        let upload = made["data"]["id"]
            .as_str()
            .ok_or_else(|| bad("Apple made a build upload but answered no id"))?
            .to_string();
        let file = apple::post(
            &token,
            "/v1/buildUploadFiles",
            &file_request(&upload, ipa.len()),
        )
        .await
        .map_err(|e| bad(&format!("reserving the IPA's upload: {e}")))?;
        let file_id = file["data"]["id"]
            .as_str()
            .ok_or_else(|| bad("Apple reserved the upload but answered no id"))?
            .to_string();
        let parts = apple::parts(&file["data"]["attributes"]["uploadOperations"], ipa.len())
            .map_err(|e| bad(&e))?;
        for part in &parts {
            apple::send_part(part, ipa).await?;
        }
        apple::patch(
            &token,
            &format!("/v1/buildUploadFiles/{file_id}"),
            &json!({ "data": { "type": "buildUploadFiles", "id": file_id,
                "attributes": { "uploaded": true } } }),
        )
        .await
        .map_err(|e| bad(&format!("committing the IPA's upload: {e}")))?;
        Ok(upload)
    }

    /// The body that creates a build upload for the app.
    pub fn upload_request(app: &str, version: &str, build_number: u64) -> Value {
        json!({ "data": { "type": "buildUploads",
            "attributes": {
                "cfBundleShortVersionString": version,
                "cfBundleVersion": build_number.to_string(),
                "platform": "IOS",
            },
            "relationships": { "app": { "data": { "type": "apps", "id": app } } },
        } })
    }

    /// The body that reserves the IPA's upload.
    pub fn file_request(upload: &str, size: usize) -> Value {
        json!({ "data": { "type": "buildUploadFiles",
            "attributes": {
                "assetType": "ASSET",
                "fileName": "Pigeon.ipa",
                "fileSize": size,
                "uti": "com.apple.ipa",
            },
            "relationships": { "buildUpload": { "data": { "type": "buildUploads", "id": upload } } },
        } })
    }

    /// A build upload's state: `{state, errors, warnings, infos}` in App Store Connect's answer
    /// (`BuildUpload.Attributes.State`), or a bare string.
    pub fn upload_state(st: &Value) -> &str {
        st["state"].as_str().or(st.as_str()).unwrap_or_default()
    }

    /// The errors and warnings of a build upload's state, at most 1500 characters.
    pub fn upload_problems(st: &Value) -> String {
        let mut out = Vec::new();
        for kind in ["errors", "warnings"] {
            for d in st[kind].as_array().into_iter().flatten() {
                let code = d["code"].as_str().unwrap_or_default();
                let msg = d["description"]
                    .as_str()
                    .or(d["message"].as_str())
                    .unwrap_or_default();
                out.push(format!("{kind}: {code} {msg}"));
            }
        }
        out.join("; ").chars().take(1500).collect()
    }

    /// `ios_releases`: the latest releases, newest first, those still processing read fresh.
    pub async fn list(env: &Env) -> Result<Value> {
        let ids = jobs(env).await?;
        let mut out = Vec::new();
        for job in ids.iter().rev().take(SHOWN) {
            let Some(mut r) = load(env, job).await? else {
                continue;
            };
            if r.state == "processing"
                && let (Some(upload), Some(Ok(token))) = (r.upload.clone(), token(env).await)
            {
                match apple::get(
                    &token,
                    &format!("/v1/buildUploads/{upload}?include=build&fields[builds]=version,processingState"),
                )
                .await
                {
                    Ok(v) => {
                        let st = &v["data"]["attributes"]["state"];
                        let state = upload_state(st);
                        r.processing = v["included"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|i| i["type"] == "builds")
                            .and_then(|b| b["attributes"]["processingState"].as_str())
                            .map(String::from)
                            .or(Some(state.to_string()));
                        match state {
                            "COMPLETE" => r.state = "complete".into(),
                            "FAILED" => {
                                r.state = "failed".into();
                                r.detail = Some(upload_problems(st));
                            }
                            _ => {
                                let p = upload_problems(st);
                                if !p.is_empty() {
                                    r.detail = Some(p);
                                }
                            }
                        }
                        r.updated_ms = now_ms();
                        store(env, &r).await?;
                    }
                    Err(e) => r.detail = Some(e.to_string()),
                }
            }
            out.push(json!({
                "job": r.job, "commit": r.commit, "build_number": r.build_number,
                "version": r.version, "state": r.state, "apple_processing": r.processing,
                "certificate": r.certificate, "upload": r.upload, "detail": r.detail,
                "started_ms": r.started_ms,
            }));
        }
        Ok(
            json!({ "releases": out, "open": out.iter().any(|r| open(r["state"].as_str().unwrap_or_default())) }),
        )
    }
}

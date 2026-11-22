//! The `apple` capability, phase 1 (docs/plans/ios.md): the App Store Connect credentials as
//! slots on the secrets page, each checked before it is written, and `apple_status`, which reads
//! (never writes) what Apple holds for the app `dev.pigeon.app`. Off until the person sets the
//! slots and their bindings land: with any of them unreadable, the tool only says what to do.
//!
//! The key and every token stay in this Worker. No answer holds the key, a token, a certificate
//! or a profile's bytes.

use pigeon_core::apple::{self, BUNDLE_ID};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store;
use serde_json::{Value, json};
use worker::{Env, Error};

use crate::expiry::Expiry;
use crate::{Call, Capability, Context, Expiring, Slot};

/// The App Store Connect Team API key (`.p8`).
pub const ASC_KEY: &str = "APPLE_ASC_KEY";
/// Its key id.
pub const ASC_KEY_ID: &str = "APPLE_ASC_KEY_ID";
/// The team's issuer id.
pub const ASC_ISSUER_ID: &str = "APPLE_ASC_ISSUER_ID";
/// The Developer team id.
pub const TEAM_ID: &str = "APPLE_TEAM_ID";

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
async fn token(env: &Env) -> Option<Result<String, Error>> {
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
pub async fn check(env: &Env, name: &str, value: &str) -> Result<String, Error> {
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
pub async fn status(env: &Env) -> Result<Value, Error> {
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
    let bundle = &ids["data"][0];

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
        "bundle_id": { "identifier": BUNDLE_ID, "registered": !bundle.is_null(), "id": bundle["id"] },
        "app": if app.is_null() { Value::Null } else { json!({
            "id": app["id"],
            "name": app["attributes"]["name"],
            "builds_seen": builds_seen,
            "highest_build_number": highest_build,
            "next_build_number_at_least": highest_build.map(|n| n + 1),
        }) },
        "distribution_certificates": certificates,
        "app_store_profiles": profiles,
    }))
}

/// The capability. It holds no state: everything is read from the slots and from Apple.
pub struct Apple;

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

    fn secret_expiry<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str) -> Expiring<'a> {
        // Team API keys and the ids do not expire; the certificates' expiry comes with phase 2.
        Box::pin(async { Ok(Expiry::Never) })
    }
}

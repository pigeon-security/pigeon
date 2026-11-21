//! Who sent a request through Cloudflare Access. Access adds a signed JWT to each request it
//! lets through, in `Cf-Access-Jwt-Assertion`; this checks its RS256 signature against the
//! team's published keys, its issuer, its audience (the Access application) and its expiry.

use serde::Deserialize;
use serde_json::Value;
use worker::js_sys::{self, Array, Function, JSON, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Error, Fetch, Request, Result};

use crate::webauthn::unb64url;

/// The header Access signs each request it lets through with.
pub const HEADER: &str = "Cf-Access-Jwt-Assertion";

/// What a valid token must say: its issuer (the team domain, such as
/// `team.cloudflareaccess.com`) and its audience (the Access application's AUD tag).
pub struct Expected {
    pub team_domain: String,
    pub aud: String,
}

/// The token's claims that say who called.
#[derive(Deserialize, Debug)]
pub struct Claims {
    /// A service token's client ID, for a machine.
    #[serde(default)]
    pub common_name: Option<String>,
    /// The signed-in person's email, for a person.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub sub: String,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
struct Checked {
    iss: String,
    aud: Value,
    exp: f64,
}

/// The verified claims of `req`'s Access token, or `None` when it carries none.
pub async fn caller(req: &Request, expected: &Expected) -> Result<Option<Claims>> {
    let Some(jwt) = req.headers().get(HEADER)? else {
        return Ok(None);
    };
    verify(&jwt, expected).await.map(Some)
}

/// The claims of `jwt`, once its signature, issuer, audience and expiry are checked.
pub async fn verify(jwt: &str, expected: &Expected) -> Result<Claims> {
    let bad = |why: &str| Error::RustError(format!("Access token: {why}"));
    let mut parts = jwt.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("not a JWT"));
    };
    let header: Header = serde_json::from_slice(&unb64url(h)?).map_err(|_| bad("bad header"))?;
    if header.alg != "RS256" {
        return Err(bad("not RS256"));
    }
    let payload = unb64url(p)?;
    let checked: Checked = serde_json::from_slice(&payload).map_err(|_| bad("bad claims"))?;
    let issuer = format!("https://{}", expected.team_domain);
    if checked.iss != issuer {
        return Err(bad("from another team"));
    }
    let for_us = match &checked.aud {
        Value::String(a) => *a == expected.aud,
        Value::Array(a) => a.iter().any(|x| x.as_str() == Some(&expected.aud)),
        _ => false,
    };
    if !for_us {
        return Err(bad("for another application"));
    }
    if checked.exp * 1000.0 < js_sys::Date::now() {
        return Err(bad("expired"));
    }
    let key = team_key(&issuer, &header.kid).await?;
    let signed = format!("{h}.{p}");
    if !rs256(&key, signed.as_bytes(), &unb64url(s)?).await? {
        return Err(bad("the signature does not verify"));
    }
    serde_json::from_slice(&payload).map_err(|_| bad("bad claims"))
}

/// The team's public key `kid`, as a JWK, from its published certs.
async fn team_key(issuer: &str, kid: &str) -> Result<Value> {
    let url = format!("{issuer}/cdn-cgi/access/certs");
    let mut res = Fetch::Url(url.parse()?).send().await?;
    let certs: Value = res.json().await?;
    certs["keys"]
        .as_array()
        .and_then(|keys| keys.iter().find(|k| k["kid"].as_str() == Some(kid)))
        .cloned()
        .ok_or_else(|| Error::RustError(format!("Access token: no team key `{kid}`")))
}

/// Whether `signature` is `jwk`'s RS256 signature of `data`, through the runtime's WebCrypto.
async fn rs256(jwk: &Value, data: &[u8], signature: &[u8]) -> Result<bool> {
    let subtle = Reflect::get(
        &Reflect::get(&js_sys::global(), &"crypto".into())?,
        &"subtle".into(),
    )?;
    let alg = JSON::parse(r#"{"name":"RSASSA-PKCS1-v1_5","hash":"SHA-256"}"#)?;
    let import: Function = Reflect::get(&subtle, &"importKey".into())?.dyn_into()?;
    let args = Array::of5(
        &"jwk".into(),
        &JSON::parse(&jwk.to_string())?,
        &alg,
        &JsValue::FALSE,
        &Array::of1(&"verify".into()),
    );
    let key = JsFuture::from(Promise::resolve(&import.apply(&subtle, &args)?)).await?;
    let verify: Function = Reflect::get(&subtle, &"verify".into())?.dyn_into()?;
    let args = Array::of4(
        &alg,
        &key,
        &Uint8Array::from(signature).into(),
        &Uint8Array::from(data).into(),
    );
    let ok = JsFuture::from(Promise::resolve(&verify.apply(&subtle, &args)?)).await?;
    Ok(ok.as_bool() == Some(true))
}

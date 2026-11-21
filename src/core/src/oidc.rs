//! OIDC ID tokens, verified: a JWT signed with RS256, checked against the issuer's JWKS (which
//! the caller fetches), then its standard claims. Only RS256 is accepted, so a token cannot pick
//! a weaker or symmetric algorithm; the claims are read only once the signature verifies; and no
//! error repeats the token.

use rsa::{
    BigUint, RsaPublicKey,
    pkcs1v15::{Signature, VerifyingKey},
    signature::Verifier,
    traits::PublicKeyParts,
};
use serde_json::{Map, Value};
use sha2::Sha256;

use crate::webauthn::unb64url;

/// The longest token accepted, in bytes.
const MAX_TOKEN_LEN: usize = 8 * 1024;
/// The smallest RSA modulus accepted, in bits.
const MIN_MODULUS_BITS: usize = 2048;

/// What a token must say beyond its signature.
pub struct Expect<'a> {
    /// The exact `iss`.
    pub issuer: &'a str,
    /// What `aud` must be, or contain.
    pub audience: &'a str,
    /// Seconds since the epoch, now.
    pub now: i64,
    /// Clock skew allowed on `exp`, `nbf` and `iat`, in seconds.
    pub leeway: i64,
    /// The longest lifetime (`exp - iat`) accepted, in seconds.
    pub max_lifetime: i64,
}

/// Verifies `jwt` with the key in `jwks` (the JWKS document) whose `kid` its header names, then
/// its standard claims; answers the claims.
pub fn verify(jwt: &str, jwks: &Value, expect: &Expect) -> Result<Map<String, Value>, String> {
    if jwt.len() > MAX_TOKEN_LEN {
        return Err("the token is longer than 8 KiB".into());
    }
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() != 3 {
        return Err("the token does not have exactly three parts".into());
    }
    let (header_b64, payload_b64, sig_b64) = (parts[0], parts[1], parts[2]);
    let header_bytes = decode_part(header_b64, "header")?;
    let payload_bytes = decode_part(payload_b64, "payload")?;
    let sig_bytes = decode_part(sig_b64, "signature")?;

    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| "the token header is not valid JSON".to_string())?;
    let header = header
        .as_object()
        .ok_or_else(|| "the token header is not a JSON object".to_string())?;
    match header.get("alg") {
        Some(Value::String(alg)) if alg == "RS256" => {}
        _ => return Err("the token algorithm is not RS256".into()),
    }
    let kid = match header.get("kid") {
        Some(Value::String(kid)) if !kid.is_empty() => kid.as_str(),
        _ => return Err("the token header has no key id".into()),
    };
    match header.get("typ") {
        None => {}
        Some(Value::String(typ)) if typ == "JWT" => {}
        Some(_) => return Err("the token type is not JWT".into()),
    }

    let key = find_key(jwks, kid)?;
    let signed = format!("{header_b64}.{payload_b64}");
    let signature = Signature::try_from(sig_bytes.as_slice())
        .map_err(|_| "the signature does not verify".to_string())?;
    VerifyingKey::<Sha256>::new(key)
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| "the signature does not verify".to_string())?;

    let payload: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|_| "the token payload is not valid JSON".to_string())?;
    let Value::Object(claims) = payload else {
        return Err("the token payload is not a JSON object".into());
    };
    check_claims(&claims, expect)?;
    Ok(claims)
}

/// One non-empty base64url part of the token, decoded.
fn decode_part(part: &str, name: &str) -> Result<Vec<u8>, String> {
    if part.is_empty() {
        return Err(format!("the token {name} is empty"));
    }
    unb64url(part).map_err(|_| format!("the token {name} is not valid base64url"))
}

/// The RSA key `kid` in the JWKS.
fn find_key(jwks: &Value, kid: &str) -> Result<RsaPublicKey, String> {
    let keys = jwks
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| "the issuer's keys have no `keys` array".to_string())?;
    let jwk = keys
        .iter()
        .find(|k| k.get("kid").and_then(Value::as_str) == Some(kid))
        .ok_or_else(|| format!("no key `{kid}` in the issuer's keys"))?;
    if jwk.get("kty").and_then(Value::as_str) != Some("RSA") {
        return Err("the issuer's key is not an RSA key".into());
    }
    if let Some(alg) = jwk.get("alg")
        && alg.as_str() != Some("RS256")
    {
        return Err("the issuer's key is not for RS256".into());
    }
    if let Some(usage) = jwk.get("use")
        && usage.as_str() != Some("sig")
    {
        return Err("the issuer's key is not for signatures".into());
    }
    let n = key_number(jwk, "n")?;
    let e = key_number(jwk, "e")?;
    if n.bits() < MIN_MODULUS_BITS {
        return Err("the issuer's key is shorter than 2048 bits".into());
    }
    let key = RsaPublicKey::new(n, e)
        .map_err(|_| "the issuer's key is not a valid RSA key".to_string())?;
    debug_assert!(key.size() * 8 >= MIN_MODULUS_BITS);
    Ok(key)
}

/// A base64url big-endian integer from a JWK member.
fn key_number(jwk: &Value, name: &str) -> Result<BigUint, String> {
    let text = jwk
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("the issuer's key has no `{name}`"))?;
    let bytes =
        unb64url(text).map_err(|_| format!("the issuer's key `{name}` is not valid base64url"))?;
    if bytes.is_empty() {
        return Err(format!("the issuer's key `{name}` is empty"));
    }
    Ok(BigUint::from_bytes_be(&bytes))
}

/// A JSON number as whole seconds.
fn number(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.is_finite())
            .map(|f| f.trunc() as i64)
    })
}

/// The standard claims, against what is expected.
fn check_claims(claims: &Map<String, Value>, expect: &Expect) -> Result<(), String> {
    match claims.get("iss") {
        Some(Value::String(iss)) if iss == expect.issuer => {}
        _ => return Err("the token issuer does not match".into()),
    }
    let aud_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud == expect.audience,
        Some(Value::Array(auds)) => {
            if auds.iter().any(|a| !a.is_string()) {
                return Err("the token audience is not a list of strings".into());
            }
            auds.iter().any(|a| a.as_str() == Some(expect.audience))
        }
        _ => false,
    };
    if !aud_ok {
        return Err("the token audience does not match".into());
    }
    let exp = claims
        .get("exp")
        .and_then(number)
        .ok_or_else(|| "the token has no expiry".to_string())?;
    if expect.now > exp.saturating_add(expect.leeway) {
        return Err("expired".into());
    }
    let iat = claims
        .get("iat")
        .and_then(number)
        .ok_or_else(|| "the token has no issue time".to_string())?;
    if iat > expect.now.saturating_add(expect.leeway) {
        return Err("the token was issued in the future".into());
    }
    if exp.saturating_sub(iat) > expect.max_lifetime {
        return Err("the token lifetime is too long".into());
    }
    if let Some(nbf) = claims.get("nbf") {
        let nbf =
            number(nbf).ok_or_else(|| "the token's not-before is not a number".to_string())?;
        if expect.now.saturating_add(expect.leeway) < nbf {
            return Err("the token is not valid yet".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rsa::RsaPrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, Signer};
    use serde_json::json;
    use std::sync::OnceLock;

    use super::*;
    use crate::webauthn::b64url;

    fn key() -> &'static RsaPrivateKey {
        static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
        KEY.get_or_init(|| RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap())
    }

    fn jwks() -> Value {
        let public = key().to_public_key();
        json!({ "keys": [{
            "kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig",
            "n": b64url(&public.n().to_bytes_be()), "e": b64url(&public.e().to_bytes_be()),
        }] })
    }

    fn sign(header: &Value, claims: &Value) -> String {
        let signed = format!(
            "{}.{}",
            b64url(header.to_string().as_bytes()),
            b64url(claims.to_string().as_bytes())
        );
        let sig = SigningKey::<Sha256>::new(key().clone()).sign(signed.as_bytes());
        format!("{signed}.{}", b64url(&sig.to_bytes()))
    }

    fn header() -> Value {
        json!({ "alg": "RS256", "kid": "k1", "typ": "JWT" })
    }

    fn claims() -> Value {
        json!({ "iss": "https://agent.buildkite.com", "aud": "https://platform.example/ci/token",
                "iat": 1_000, "exp": 1_300, "build_id": "b1" })
    }

    fn expect() -> Expect<'static> {
        Expect {
            issuer: "https://agent.buildkite.com",
            audience: "https://platform.example/ci/token",
            now: 1_100,
            leeway: 30,
            max_lifetime: 600,
        }
    }

    #[test]
    fn a_good_token_verifies_and_answers_its_claims() {
        let c = verify(&sign(&header(), &claims()), &jwks(), &expect()).unwrap();
        assert_eq!(c["build_id"], "b1");
        let mut list = claims();
        list["aud"] = json!(["other", "https://platform.example/ci/token"]);
        assert!(verify(&sign(&header(), &list), &jwks(), &expect()).is_ok());
    }

    #[test]
    fn a_changed_payload_or_signature_does_not_verify() {
        let token = sign(&header(), &claims());
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        let mut forged = claims();
        forged["build_id"] = json!("b2");
        parts[1] = b64url(forged.to_string().as_bytes());
        assert_eq!(
            verify(&parts.join("."), &jwks(), &expect()).unwrap_err(),
            "the signature does not verify"
        );
        let cut = &token[..token.len() - 4];
        assert!(verify(cut, &jwks(), &expect()).is_err());
    }

    #[test]
    fn only_rs256_with_a_known_key_is_accepted() {
        for h in [
            json!({ "alg": "none", "kid": "k1" }),
            json!({ "alg": "HS256", "kid": "k1" }),
            json!({ "alg": "RS256" }),
            json!({ "alg": "RS256", "kid": "k1", "typ": "at+jwt" }),
        ] {
            assert!(
                verify(&sign(&h, &claims()), &jwks(), &expect()).is_err(),
                "{h}"
            );
        }
        let h = json!({ "alg": "RS256", "kid": "k2" });
        assert_eq!(
            verify(&sign(&h, &claims()), &jwks(), &expect()).unwrap_err(),
            "no key `k2` in the issuer's keys"
        );
        let mut weak = jwks();
        weak["keys"][0]["n"] = json!(b64url(&[0xff; 128]));
        assert!(verify(&sign(&header(), &claims()), &weak, &expect()).is_err());
    }

    #[test]
    fn issuer_audience_and_times_are_checked() {
        let cases = [
            ("iss", json!("https://evil.example")),
            ("aud", json!("https://other.example")),
            ("aud", json!(["a", 1])),
            ("exp", json!(1_000)),
            ("iat", json!(2_000)),
            ("nbf", json!(1_200)),
        ];
        for (k, v) in cases {
            let mut c = claims();
            c[k] = v.clone();
            assert!(
                verify(&sign(&header(), &c), &jwks(), &expect()).is_err(),
                "{k}={v}"
            );
        }
        let mut long = claims();
        long["exp"] = json!(1_000 + 601);
        assert_eq!(
            verify(&sign(&header(), &long), &jwks(), &expect()).unwrap_err(),
            "the token lifetime is too long"
        );
        let mut no_iat = claims();
        no_iat.as_object_mut().unwrap().remove("iat");
        assert!(verify(&sign(&header(), &no_iat), &jwks(), &expect()).is_err());
        let mut late = expect();
        late.now = 1_330;
        assert!(verify(&sign(&header(), &claims()), &jwks(), &late).is_ok());
        late.now = 1_331;
        assert_eq!(
            verify(&sign(&header(), &claims()), &jwks(), &late).unwrap_err(),
            "expired"
        );
    }

    #[test]
    fn junk_is_refused() {
        for t in ["", "a.b", "a.b.c.d", "..", &"a".repeat(9000)] {
            assert!(verify(t, &jwks(), &expect()).is_err(), "{t}");
        }
    }
}

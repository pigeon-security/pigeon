//! WebAuthn passkeys, checked on the server: ES256 credentials only, user presence and
//! verification required. The browser hands over a new credential's public key with
//! `getPublicKey()` and its authenticator data with `getAuthenticatorData()`, so no CBOR is
//! read here.

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use worker::{Error, Result};

/// COSE's ES256: ECDSA on P-256 with SHA-256.
pub const ES256: i64 = -7;

/// How long a challenge may be answered, in milliseconds.
pub const CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;

const USER_PRESENT: u8 = 0x01;
const USER_VERIFIED: u8 = 0x04;

/// A P-256 public key's SubjectPublicKeyInfo, up to its 65-byte uncompressed point.
const SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a,
    0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

/// Who the passkeys are for: the relying party's ID (a hostname) and the origin pages come from.
pub struct RelyingParty<'a> {
    pub id: &'a str,
    pub origin: &'a str,
}

/// 32 random bytes, as base64url: a challenge, or an ID nobody can guess.
pub fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| Error::RustError(format!("random: {e}")))?;
    Ok(b64url(&bytes))
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
}

/// The authenticator data's sign count, after its relying party and flags are checked.
fn authenticator(data: &[u8], rp: &RelyingParty) -> Result<u32> {
    if data.len() < 37 {
        return Err(bad("authenticator data is too short"));
    }
    if data[..32] != Sha256::digest(rp.id.as_bytes())[..] {
        return Err(bad("the passkey is for another site"));
    }
    let flags = data[32];
    if flags & USER_PRESENT == 0 || flags & USER_VERIFIED == 0 {
        return Err(bad("the authenticator did not verify the user"));
    }
    Ok(u32::from_be_bytes([data[33], data[34], data[35], data[36]]))
}

fn client(json: &[u8], kind: &str, challenge: &str, rp: &RelyingParty) -> Result<()> {
    let c: ClientData =
        serde_json::from_slice(json).map_err(|e| bad(&format!("client data: {e}")))?;
    if c.kind != kind {
        return Err(bad(&format!("client data is {}, not {kind}", c.kind)));
    }
    if c.challenge != challenge {
        return Err(bad("the challenge does not match"));
    }
    if c.origin != rp.origin {
        return Err(bad(&format!("origin {} is not {}", c.origin, rp.origin)));
    }
    Ok(())
}

/// A registration, as the browser sends it (each field base64url).
#[derive(Deserialize)]
pub struct Registration {
    pub id: String,
    #[serde(rename = "clientDataJSON")]
    pub client_data: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    /// `getPublicKey()`: SubjectPublicKeyInfo DER.
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "publicKeyAlgorithm")]
    pub algorithm: i64,
}

/// A registered passkey: its credential ID, uncompressed P-256 point and sign count.
pub struct Credential {
    pub id: String,
    pub point: Vec<u8>,
    pub sign_count: u32,
}

/// Checks a registration answering `challenge`; answers the credential to keep.
pub fn register(r: &Registration, challenge: &str, rp: &RelyingParty) -> Result<Credential> {
    if r.algorithm != ES256 {
        return Err(bad("only ES256 passkeys are taken"));
    }
    client(&unb64url(&r.client_data)?, "webauthn.create", challenge, rp)?;
    let sign_count = authenticator(&unb64url(&r.authenticator_data)?, rp)?;
    let spki = unb64url(&r.public_key)?;
    let point = spki
        .strip_prefix(&SPKI_PREFIX[..])
        .filter(|p| p.len() == 65)
        .ok_or_else(|| bad("the public key is not a P-256 key"))?;
    VerifyingKey::from_sec1_bytes(point).map_err(|_| bad("the public key is not on P-256"))?;
    Ok(Credential {
        id: r.id.clone(),
        point: point.to_vec(),
        sign_count,
    })
}

/// An assertion, as the browser sends it (each field base64url).
#[derive(Deserialize)]
pub struct Assertion {
    pub id: String,
    #[serde(rename = "clientDataJSON")]
    pub client_data: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    pub signature: String,
}

/// Checks an assertion answering `challenge` against `credential`; answers its new sign count.
/// A count that does not move forward, when the authenticator counts, is refused as a clone.
pub fn assert(
    a: &Assertion,
    credential: &Credential,
    challenge: &str,
    rp: &RelyingParty,
) -> Result<u32> {
    let client_data = unb64url(&a.client_data)?;
    client(&client_data, "webauthn.get", challenge, rp)?;
    let data = unb64url(&a.authenticator_data)?;
    let count = authenticator(&data, rp)?;
    if (count != 0 || credential.sign_count != 0) && count <= credential.sign_count {
        return Err(bad("the passkey's counter went backwards"));
    }
    let key = VerifyingKey::from_sec1_bytes(&credential.point)
        .map_err(|_| bad("the stored key is not on P-256"))?;
    let signature = der_signature(&unb64url(&a.signature)?)?;
    let mut signed = data;
    signed.extend_from_slice(&Sha256::digest(&client_data));
    key.verify(&signed, &signature)
        .map_err(|_| bad("the signature does not verify"))?;
    Ok(count)
}

/// An ECDSA signature from its DER `SEQUENCE { INTEGER r, INTEGER s }`.
fn der_signature(der: &[u8]) -> Result<Signature> {
    let malformed = || bad("the signature is not DER");
    let body = match der {
        [0x30, len, rest @ ..] if usize::from(*len) == rest.len() => rest,
        _ => return Err(malformed()),
    };
    let (r, rest) = der_integer(body).ok_or_else(malformed)?;
    let (s, rest) = der_integer(rest).ok_or_else(malformed)?;
    if !rest.is_empty() {
        return Err(malformed());
    }
    let mut bytes = [0u8; 64];
    bytes[32 - r.len()..32].copy_from_slice(r);
    bytes[64 - s.len()..].copy_from_slice(s);
    Signature::from_slice(&bytes).map_err(|_| malformed())
}

/// One DER INTEGER, without its leading zero, at most 32 bytes; and what follows it.
fn der_integer(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let [0x02, len, rest @ ..] = der else {
        return None;
    };
    let len = usize::from(*len);
    let (int, rest) = (rest.get(..len)?, rest.get(len..)?);
    let int = match int {
        [0, tail @ ..] if !tail.is_empty() => tail,
        int => int,
    };
    (int.len() <= 32).then_some((int, rest))
}

fn bad(why: &str) -> Error {
    Error::RustError(format!("passkey: {why}"))
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `bytes` as unpadded base64url.
pub fn b64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

/// Unpadded (or padded) base64url, decoded.
pub fn unb64url(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0_u32, 0);
    for c in s.trim_end_matches('=').bytes() {
        let v = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or_else(|| bad("not base64url"))? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

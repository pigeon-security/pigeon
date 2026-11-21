//! Wraps AES-256-GCM from WebCrypto.
//!
//! It keeps a linked account's tokens encrypted at rest.
//! A sealed value is the base64url of a random 12-byte IV followed by the ciphertext.
//! The pure functions join, split and parse. The async functions call WebCrypto.

use crate::webauthn::{b64url, unb64url};
use worker::js_sys::{self, Array, Function, Object, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::{Error, Result};

/// The length of the IV in bytes.
const IV_LEN: usize = 12;
/// The length of the GCM tag in bytes.
const TAG_LEN: usize = 16;
/// The message for every failure to open a sealed value.
const OPEN_FAILED: &str = "could not open the sealed value";

/// Joins an IV and a ciphertext into one byte string.
pub fn join_sealed(iv: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(iv.len() + ciphertext.len());
    out.extend_from_slice(iv);
    out.extend_from_slice(ciphertext);
    out
}

/// Splits sealed bytes into the IV and the ciphertext. Answers None when they are too short.
pub fn split_sealed(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.len() < IV_LEN + TAG_LEN {
        return None;
    }
    Some(bytes.split_at(IV_LEN))
}

/// Reads a stored key: base64url of exactly 32 bytes. Anything else gives None.
pub fn key_from(secret: &str) -> Option<[u8; 32]> {
    let bytes = unb64url(secret).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Gets `crypto.subtle` from the global scope.
fn subtle() -> Result<JsValue> {
    let crypto = Reflect::get(&js_sys::global(), &"crypto".into())?;
    Ok(Reflect::get(&crypto, &"subtle".into())?)
}

/// Builds the AES-GCM algorithm object.
fn algorithm(iv: &[u8], aad: &[u8]) -> Result<Object> {
    let alg = Object::new();
    Reflect::set(&alg, &"name".into(), &"AES-GCM".into())?;
    Reflect::set(&alg, &"iv".into(), &Uint8Array::from(iv))?;
    Reflect::set(&alg, &"additionalData".into(), &Uint8Array::from(aad))?;
    Ok(alg)
}

/// Imports the key and runs `encrypt` or `decrypt` over the data.
async fn run(operation: &str, key: &[u8], iv: &[u8], aad: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let subtle = subtle()?;
    let import: Function = Reflect::get(&subtle, &"importKey".into())?.dyn_into()?;
    let args = Array::of5(
        &"raw".into(),
        &Uint8Array::from(key),
        &"AES-GCM".into(),
        &JsValue::FALSE,
        &Array::of1(&operation.into()),
    );
    let crypto_key = JsFuture::from(Promise::resolve(&import.apply(&subtle, &args)?)).await?;

    let alg = algorithm(iv, aad)?;
    let op: Function = Reflect::get(&subtle, &operation.into())?.dyn_into()?;
    let args = Array::of3(&alg, &crypto_key, &Uint8Array::from(data));
    let buffer = JsFuture::from(Promise::resolve(&op.apply(&subtle, &args)?)).await?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// Encrypts `plaintext` with a 32-byte key and a random IV.
///
/// The `aad` is authenticated but not stored. Answers `b64url(iv || ciphertext)`.
pub async fn seal(key: &[u8], plaintext: &[u8], aad: &[u8]) -> Result<String> {
    if key.len() != 32 {
        return Err(Error::RustError("the key must be 32 bytes".to_string()));
    }
    let mut iv = [0u8; IV_LEN];
    getrandom::getrandom(&mut iv)
        .map_err(|_| Error::RustError("could not get random bytes".to_string()))?;
    let ciphertext = run("encrypt", key, &iv, aad, plaintext).await?;
    Ok(b64url(&join_sealed(&iv, &ciphertext)))
}

/// Decrypts a value made by `seal`.
///
/// A wrong key, wrong `aad`, damaged value or short input all fail the same way.
pub async fn open(key: &[u8], sealed: &str, aad: &[u8]) -> Result<Vec<u8>> {
    let fail = || Error::RustError(OPEN_FAILED.to_string());
    if key.len() != 32 {
        return Err(fail());
    }
    let bytes = unb64url(sealed).map_err(|_| fail())?;
    let (iv, ciphertext) = split_sealed(&bytes).ok_or_else(fail)?;
    run("decrypt", key, iv, aad, ciphertext)
        .await
        .map_err(|_| fail())
}

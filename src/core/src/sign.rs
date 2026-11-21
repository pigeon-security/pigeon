//! HMAC-SHA256 signing and constant-time comparison: for checking a webhook delivery before
//! anything in it is read, and for signing what pigeon posts. A secret is never compared in a
//! way that stops at the first differing byte.

use hmac::{Hmac, Mac};
use sha2::Sha256;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// HMAC-SHA256 of `message` with `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    // HMAC takes a key of any length, so `new_from_slice` cannot fail; the arm exists only so
    // there is no panic path.
    match Hmac::<Sha256>::new_from_slice(key) {
        Ok(mut mac) => {
            mac.update(message);
            let mut out = [0u8; 32];
            out.copy_from_slice(&mac.finalize().into_bytes());
            out
        }
        Err(_) => [0u8; 32],
    }
}

/// `bytes` as lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(b >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Lowercase or uppercase hex, decoded; `None` when not hex or of odd length.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Some((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

/// Whether `a` and `b` are equal, in time that depends only on their lengths.
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// Whether `signature_hex` is the HMAC-SHA256 of `message` with `key`, compared in constant
/// time.
pub fn verify_hex(key: &[u8], message: &[u8], signature_hex: &str) -> bool {
    match unhex(signature_hex) {
        Some(signature) => equal(&hmac_sha256(key, message), &signature),
        None => false,
    }
}

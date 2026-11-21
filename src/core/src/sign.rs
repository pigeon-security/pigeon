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
    if bytes.len() % 2 != 0 {
        return None;
    }
    bytes
        .chunks_exact(2)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_case_1() {
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc4231_case_6() {
        let mac = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn keys_of_any_length_work() {
        let macs: Vec<[u8; 32]> = [0usize, 1, 64, 200]
            .iter()
            .map(|&n| hmac_sha256(&vec![0x5a; n], b"message"))
            .collect();
        for (i, mac) in macs.iter().enumerate() {
            assert_ne!(*mac, [0u8; 32]);
            for other in &macs[i + 1..] {
                assert_ne!(mac, other);
            }
        }
    }

    #[test]
    fn hex_and_unhex_round_trip() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert_eq!(unhex(&hex(&data)), Some(data));
        assert_eq!(unhex("ABcd"), Some(vec![0xab, 0xcd]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn equal_compares_content_and_length() {
        assert!(equal(b"same", b"same"));
        assert!(!equal(b"same", b"sane"));
        assert!(!equal(b"same", b"same!"));
    }

    #[test]
    fn verify_hex_takes_uppercase_and_refuses_changes() {
        let sig = hex(&hmac_sha256(b"key", b"body"));
        assert!(verify_hex(b"key", b"body", &sig.to_uppercase()));
        assert!(!verify_hex(b"key", b"body", &sig[..sig.len() - 2]));
        let mut mac = hmac_sha256(b"key", b"body");
        mac[7] ^= 0x01;
        assert!(!verify_hex(b"key", b"body", &hex(&mac)));
        assert!(!verify_hex(b"other", b"body", &sig));
    }
}

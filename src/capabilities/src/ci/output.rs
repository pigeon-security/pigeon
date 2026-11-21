//! A job's output, as its runner reports it: the last few KiB of what the script printed, sent
//! as standard base64 so no JSON escaping is needed in the runner's shell. What is kept is cut
//! to [`MAX_BYTES`] and has anything token-shaped replaced first: pigeon's own tokens, the token
//! formats of the CI systems and model providers pigeon talks to, and whatever follows
//! `Bearer`, `Basic` or `token=`. Redaction is a safety net, not a guarantee: a script that
//! prints a secret in another shape keeps it in its output.

use pigeon_core::webauthn::unb64url;

/// The most output kept, in bytes, from the end.
pub const MAX_BYTES: usize = 4096;
/// The longest base64 accepted from a runner.
pub const MAX_ENCODED: usize = 8192;

/// Prefixes whose following run of token characters is replaced.
const PREFIXES: [&str; 9] = [
    "pgn_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "bkua_",
    "sk-ant-",
];

/// Words whose following value is replaced, matched without regard to case.
const BEFORE_SECRET: [&str; 5] = ["bearer ", "basic ", "token=", "password=", "secret="];

const REDACTED: &str = "[redacted]";

fn token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'+' || b == b'/'
}

/// `text` with token-shaped strings replaced by `[redacted]`.
pub fn redact(text: &str) -> String {
    let bytes = text.as_bytes();
    let lower = text.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    'scan: while i < bytes.len() {
        for p in PREFIXES {
            if bytes[i..].starts_with(p.as_bytes()) {
                let start = i + p.len();
                let end = start
                    + bytes[start..]
                        .iter()
                        .take_while(|b| token_char(**b))
                        .count();
                if end - start >= 8 {
                    out.push_str(p);
                    out.push_str(REDACTED);
                    i = end;
                    continue 'scan;
                }
            }
        }
        for w in BEFORE_SECRET {
            if lower[i..].starts_with(w.as_bytes()) {
                let start = i + w.len();
                let end = start
                    + bytes[start..]
                        .iter()
                        .take_while(|b| !b.is_ascii_whitespace() && **b != b'"' && **b != b'\'')
                        .count();
                if end > start {
                    out.push_str(&text[i..start]);
                    out.push_str(REDACTED);
                    i = end;
                    continue 'scan;
                }
            }
        }
        // Whole characters only: `i` only ever stops on a character boundary.
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// A runner's base64 output as the text to keep: decoded, cut to its last [`MAX_BYTES`] and
/// redacted. `None` when it is missing, too long or not base64.
pub fn keep(encoded: Option<&str>) -> Option<String> {
    let encoded = encoded?.trim();
    if encoded.is_empty() || encoded.len() > MAX_ENCODED {
        return None;
    }
    // Standard base64 as base64url, which the decoder reads.
    let url: String = encoded
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    let bytes = unb64url(&url).ok()?;
    let tail = &bytes[bytes.len().saturating_sub(MAX_BYTES)..];
    Some(redact(&String::from_utf8_lossy(tail)))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_redacted_and_the_rest_is_kept() {
        let text = "fetch ok\nAuthorization: Bearer pgn_ci_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\nuse pgn_px_BBBBBBBBBBBB then ghp_CCCCCCCCCCCCCCCC done\n";
        let r = redact(text);
        assert!(
            !r.contains("AAAA") && !r.contains("BBBB") && !r.contains("CCCC"),
            "{r}"
        );
        assert!(
            r.starts_with("fetch ok\nAuthorization: Bearer [redacted]\n"),
            "{r}"
        );
        assert!(r.contains("pgn_[redacted] then ghp_[redacted] done"), "{r}");
    }

    #[test]
    fn values_after_secret_words_are_redacted_whatever_their_case() {
        assert_eq!(
            redact("curl -u x: BASIC abc123 TOKEN=xyz password='p'"),
            "curl -u x: BASIC [redacted] TOKEN=[redacted] password='p'"
        );
        assert_eq!(redact("token= next"), "token= next");
    }

    #[test]
    fn short_or_ordinary_text_is_left_alone() {
        for t in [
            "pgn_ci is a prefix",
            "commit 0123456789abcdef0123456789abcdef01234567",
            "naïve ünïcode ✓",
            "",
        ] {
            assert_eq!(redact(t), t);
        }
    }

    #[test]
    fn output_is_decoded_cut_to_its_tail_and_redacted() {
        // "hello\n" in standard base64.
        assert_eq!(keep(Some("aGVsbG8K")).as_deref(), Some("hello\n"));
        // Bytes 0xfb 0xff use `+` and `/` in standard base64.
        assert!(keep(Some("+/8=")).is_some());
        assert_eq!(keep(None), None);
        assert_eq!(keep(Some("")), None);
        assert_eq!(keep(Some("not base64!")), None);
        assert_eq!(keep(Some(&"A".repeat(MAX_ENCODED + 4))), None);
        let long = pigeon_core::git::base64(&vec![b'x'; 6000]);
        assert_eq!(keep(Some(&long)).unwrap().len(), MAX_BYTES);
        let secret = pigeon_core::git::base64(b"Bearer pgn_ci_SECRETSECRETSECRET");
        assert_eq!(keep(Some(&secret)).as_deref(), Some("Bearer [redacted]"));
    }
}

//! One redactor for every error that came from a provider: it hides each secret the gateway
//! knows and anything shaped like a Scaleway access key.

/// What a hidden value is replaced by.
pub const MARK: &str = "<redacted>";

/// `text` with each non-empty value of `known` replaced by `<redacted>`, and then every run of
/// capitals and digits that starts with `SCW` and holds at least 17 more.
pub fn redact(text: &str, known: &[&str]) -> String {
    let mut values: Vec<&str> = known.iter().copied().filter(|k| !k.is_empty()).collect();
    // Longest first, so a value that holds another is hidden whole.
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let mut out = text.to_string();
    for value in values {
        out = out.replace(value, MARK);
    }
    scrub_access_keys(&out)
}

/// Whether `b` is a capital letter or a digit.
fn key_char(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

/// `text` with every access-key-shaped run replaced.
fn scrub_access_keys(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"SCW") {
            let mut end = i + 3;
            while end < bytes.len() && key_char(bytes[end]) {
                end += 1;
            }
            if end - (i + 3) >= 17 {
                out.push_str(&text[copied..i]);
                out.push_str(MARK);
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

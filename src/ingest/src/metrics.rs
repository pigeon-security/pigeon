//! The metrics of one file: size, SHA-256 and BLAKE3 in one streaming pass, the sniffed type,
//! the normalised name, and the optional text fingerprint (decision 13.1).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::Error;

/// How many leading bytes the sniffer looks at.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// The largest text the fingerprint reads, in bytes.
pub const FINGERPRINT_MAX: u64 = 16 * 1024 * 1024;

/// A broad kind of content, from the sniffed type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Document,
    Image,
    Video,
    Audio,
    Archive,
    Email,
    Text,
    Other,
}

/// What is known of one file's bytes once it has been read whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    /// Bytes read, counted while streaming.
    pub size: u64,
    /// 64 lowercase hex.
    pub sha256: String,
    /// 64 lowercase hex.
    pub blake3: String,
    /// The sniffed media type, such as `application/pdf`; `application/octet-stream` when unknown.
    pub mime: String,
    pub kind: Kind,
    /// 16 lowercase hex: the 64-bit SimHash of the text, for text-like files when asked for.
    pub simhash: Option<String>,
}

/// Hashes bytes as they stream: size, SHA-256, BLAKE3, and the first [`SNIFF_BYTES`] kept for
/// the sniffer (and, when `fingerprint` is on, text up to [`FINGERPRINT_MAX`]).
pub struct Hasher {
    fingerprint: bool,
    size: u64,
    sha: Sha256,
    b3: blake3::Hasher,
    head: Vec<u8>,
    /// Everything read so far, kept only while fingerprinting and under the limit.
    text: Vec<u8>,
}

impl Hasher {
    /// A new hasher; `fingerprint` keeps text for [`simhash`].
    pub fn new(fingerprint: bool) -> Self {
        Self {
            fingerprint,
            size: 0,
            sha: Sha256::new(),
            b3: blake3::Hasher::new(),
            head: Vec::new(),
            text: Vec::new(),
        }
    }

    /// Adds bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        self.size += bytes.len() as u64;
        self.sha.update(bytes);
        self.b3.update(bytes);
        if self.head.len() < SNIFF_BYTES {
            let take = (SNIFF_BYTES - self.head.len()).min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
        }
        if self.fingerprint {
            if self.size <= FINGERPRINT_MAX {
                self.text.extend_from_slice(bytes);
            } else {
                self.text = Vec::new();
            }
        }
    }

    /// The metrics of everything added.
    pub fn finish(self) -> Result<Metrics, Error> {
        let (mime, kind) = sniff(&self.head);
        let text_like = matches!(kind, Kind::Text | Kind::Email);
        let simhash = if self.fingerprint && text_like && self.size <= FINGERPRINT_MAX {
            std::str::from_utf8(&self.text)
                .ok()
                .map(|t| format!("{:016x}", simhash(t)))
        } else {
            None
        };
        Ok(Metrics {
            size: self.size,
            sha256: hex(&self.sha.finalize()),
            blake3: self.b3.finalize().to_hex().to_string(),
            mime,
            kind,
            simhash,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The media type and kind of content starting with `head`, by magic bytes, then a UTF-8 text
/// check. Never by name.
pub fn sniff(head: &[u8]) -> (String, Kind) {
    let m = |mime: &str, kind: Kind| (mime.to_string(), kind);
    if head.starts_with(b"%PDF-") {
        return m("application/pdf", Kind::Document);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return m("image/png", Kind::Image);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return m("image/jpeg", Kind::Image);
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return m("image/gif", Kind::Image);
    }
    if head.starts_with(b"II*\0") || head.starts_with(b"MM\0*") {
        return m("image/tiff", Kind::Image);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" {
        match &head[8..12] {
            b"WEBP" => return m("image/webp", Kind::Image),
            b"WAVE" => return m("audio/wav", Kind::Audio),
            _ => {}
        }
    }
    if crate::archive::is_zip(head) {
        return m("application/zip", Kind::Archive);
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return m("application/gzip", Kind::Archive);
    }
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return m("application/x-tar", Kind::Archive);
    }
    if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return m("application/x-7z-compressed", Kind::Archive);
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return m("application/vnd.rar", Kind::Archive);
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        if &head[8..12] == b"qt  " {
            return m("video/quicktime", Kind::Video);
        }
        return m("video/mp4", Kind::Video);
    }
    if head.starts_with(b"OggS") {
        return m("audio/ogg", Kind::Audio);
    }
    if head.starts_with(b"fLaC") {
        return m("audio/flac", Kind::Audio);
    }
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        return m("audio/mpeg", Kind::Audio);
    }
    if head.is_empty() || head.contains(&0) {
        return m("application/octet-stream", Kind::Other);
    }
    let text_ok = match std::str::from_utf8(head) {
        Ok(_) => true,
        // A multi-byte character cut by the end of the sample is still text.
        Err(e) => e.error_len().is_none(),
    };
    if !text_ok {
        return m("application/octet-stream", Kind::Other);
    }
    let lower: Vec<u8> = head
        .iter()
        .take(16)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    if [
        &b"from:"[..],
        b"received:",
        b"return-path:",
        b"delivered-to:",
    ]
    .iter()
    .any(|p| lower.starts_with(p))
    {
        return m("message/rfc822", Kind::Email);
    }
    m("text/plain", Kind::Text)
}

/// A path's normalised file name and normalised path (decision 13.1): NFC, lowercase, `\` to
/// `/`, whitespace collapsed, copy markers removed, extension aliases folded.
pub fn normalise(path: &str) -> (String, String) {
    let flat: String = path
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .replace('\\', "/");
    let segments: Vec<String> = flat
        .split('/')
        .map(|s| clean_segment(&s.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    let name = segments.last().cloned().unwrap_or_default();
    (name, segments.join("/"))
}

/// Removes copy markers from the stem and folds the extension of one path segment.
fn clean_segment(seg: &str) -> String {
    let (mut stem, ext) = match seg.rfind('.') {
        Some(i) if i > 0 => (seg[..i].to_string(), Some(&seg[i + 1..])),
        _ => (seg.to_string(), None),
    };
    loop {
        let before = stem.clone();
        if let Some(rest) = stem.strip_prefix("copy of ") {
            stem = rest.trim().to_string();
        }
        if let Some(rest) = stem.strip_suffix(" - copy") {
            stem = rest.trim().to_string();
        }
        if stem.ends_with(')')
            && let Some(open) = stem.rfind('(')
        {
            let inner = &stem[open + 1..stem.len() - 1];
            if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
                stem = stem[..open].trim().to_string();
            }
        }
        if stem == before {
            break;
        }
    }
    match ext {
        Some(e) => {
            let e = match e {
                "jpeg" => "jpg",
                "htm" => "html",
                "tiff" => "tif",
                other => other,
            };
            format!("{stem}.{e}")
        }
        None => stem,
    }
}

/// The 64-bit SimHash of `text` over word 3-shingles, after normalising it.
pub fn simhash(text: &str) -> u64 {
    let norm: String = text.nfc().collect::<String>().to_lowercase();
    let words: Vec<&str> = norm
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return 0;
    }
    let mut counts = [0i64; 64];
    let mut add = |shingle: &str| {
        let h = blake3::hash(shingle.as_bytes());
        let mut b = [0u8; 8];
        b.copy_from_slice(&h.as_bytes()[..8]);
        let v = u64::from_le_bytes(b);
        for (i, c) in counts.iter_mut().enumerate() {
            *c += if (v >> i) & 1 == 1 { 1 } else { -1 };
        }
    };
    if words.len() < 3 {
        add(&words.join(" "));
    } else {
        for w in words.windows(3) {
            add(&w.join(" "));
        }
    }
    counts.iter().enumerate().fold(
        0u64,
        |acc, (i, &c)| if c > 0 { acc | (1 << i) } else { acc },
    )
}

/// The number of differing bits between two SimHashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

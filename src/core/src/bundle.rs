//! Git bundles, as `git bundle create` writes them (formats v2 and v3), read far enough to push
//! one: the commits it needs the repository to have already (its prerequisites), the refs it
//! carries and the pack after them, whose SHA-1 trailer is checked. The pack is passed on as it
//! is, so every commit keeps its id, author, committer, message and trailers. A bundle's pack is
//! usually thin (objects stored as changes to objects of its prerequisites); the receiving git
//! resolves those, as it does for any push.
//!
//! The bundle is untrusted: anything malformed is an error naming what, never a panic.

use sha1::{Digest, Sha1};

/// The largest bundle taken, decoded: 8 MiB.
pub const MAX: usize = 8 * 1024 * 1024;
/// The largest header (signature, capabilities, prerequisites and refs) taken.
pub const HEADER_MAX: usize = 64 * 1024;
/// The most prerequisites a bundle may name: each is checked against the repository.
pub const PREREQUISITES_MAX: usize = 8;

/// A parsed bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle<'a> {
    /// The commits the repository must already have, by id.
    pub prerequisites: Vec<String>,
    /// The refs it carries: (commit id, ref name).
    pub refs: Vec<(String, String)>,
    /// The pack, trailer included.
    pub pack: &'a [u8],
}

fn bad(what: &str) -> String {
    format!("the bundle is malformed: {what}")
}

/// Whether `s` is a full lowercase hex object id.
fn is_id(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `text` (standard base64, padded or not, whitespace allowed) as bytes, refused before it is
/// decoded when it would pass [`MAX`].
pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let chars = text.bytes().filter(|b| !b.is_ascii_whitespace()).count();
    if chars > MAX.div_ceil(3) * 4 {
        return Err(too_large(chars / 4 * 3));
    }
    let mut data = Vec::with_capacity(chars / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for b in text.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b if b.is_ascii_whitespace() => continue,
            _ => return Err("`bundle` is not standard base64".into()),
        };
        if pad > 0 {
            return Err("`bundle` is not standard base64: data after its padding".into());
        }
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            data.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if pad > 2 || (chars - pad) % 4 == 1 {
        return Err("`bundle` is not standard base64: it ends mid-byte".into());
    }
    if data.len() > MAX {
        return Err(too_large(data.len()));
    }
    Ok(data)
}

/// Why a bundle of `n` bytes is refused.
pub fn too_large(n: usize) -> String {
    format!(
        "the bundle is {n} bytes; branch_push takes {} bytes (8 MiB) at most. Push in smaller \
         steps, or use session_credential and git push for a large change",
        MAX
    )
}

/// `data` as a bundle: its header read line by line, then its pack, whose trailer must be the
/// SHA-1 of the rest. A v3 bundle may name only `@object-format=sha1`: a filtered (partial)
/// bundle is refused.
pub fn parse(data: &[u8]) -> Result<Bundle<'_>, String> {
    if data.len() > MAX {
        return Err(too_large(data.len()));
    }
    let mut at = 0;
    let mut line = || -> Result<&str, String> {
        let rest = data.get(at..).unwrap_or_default();
        let end = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| bad("the header does not end"))?;
        if at + end > HEADER_MAX {
            return Err(bad("the header is too long"));
        }
        let text = std::str::from_utf8(&rest[..end]).map_err(|_| bad("the header is not UTF-8"))?;
        at += end + 1;
        Ok(text)
    };
    let v3 = match line()? {
        "# v2 git bundle" => false,
        "# v3 git bundle" => true,
        _ => return Err(bad("no `# v2 git bundle` or `# v3 git bundle` signature")),
    };
    let mut prerequisites = Vec::new();
    let mut refs = Vec::new();
    loop {
        let l = line()?;
        if l.is_empty() {
            break;
        }
        if let Some(cap) = l.strip_prefix('@') {
            if !v3 || cap != "object-format=sha1" {
                return Err(bad(&format!(
                    "capability `@{cap}` is not taken: only a whole (unfiltered) SHA-1 bundle is"
                )));
            }
            continue;
        }
        if let Some(p) = l.strip_prefix('-') {
            let id = p.split(' ').next().unwrap_or_default();
            if !is_id(id) {
                return Err(bad("a prerequisite is not a commit id"));
            }
            prerequisites.push(id.to_string());
            continue;
        }
        let (id, name) = l
            .split_once(' ')
            .ok_or_else(|| bad("a ref line has no name"))?;
        if !is_id(id) || name.is_empty() || name.contains(char::is_whitespace) {
            return Err(bad("a ref line is not `<commit id> <ref>`"));
        }
        refs.push((id.to_string(), name.to_string()));
    }
    if refs.is_empty() {
        return Err(bad("it carries no ref"));
    }
    if prerequisites.len() > PREREQUISITES_MAX {
        return Err(format!(
            "the bundle names {} prerequisites; {PREREQUISITES_MAX} at most",
            prerequisites.len()
        ));
    }
    prerequisites.sort();
    prerequisites.dedup();
    let pack = &data[at..];
    if pack.len() < 32 || !pack.starts_with(b"PACK") {
        return Err(bad("no pack after the header"));
    }
    let (body, trailer) = pack.split_at(pack.len() - 20);
    let sum: [u8; 20] = Sha1::digest(body).into();
    if sum[..] != *trailer {
        return Err(bad(
            "the pack's checksum does not match: it was cut short or changed in transit",
        ));
    }
    Ok(Bundle {
        prerequisites,
        refs,
        pack,
    })
}

impl Bundle<'_> {
    /// The one commit its refs point at: the tip to push. Refs at different commits are refused,
    /// since which one to push would be a guess.
    pub fn tip(&self) -> Result<String, String> {
        let mut ids: Vec<&str> = self.refs.iter().map(|(id, _)| id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        match ids.as_slice() {
            [one] => Ok((*one).to_string()),
            _ => Err(
                "the bundle's refs point at different commits: bundle one branch (`git bundle \
                 create <file> <base>..HEAD`)"
                    .into(),
            ),
        }
    }
}

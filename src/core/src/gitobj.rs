//! Git objects without a git binary: blobs, trees of files and commits, their SHA-1 ids, and an
//! undeltified version-2 pack of them, enough to push a small commit such as a notes commit.
//! The pack's zlib streams are stored deflate blocks, so no compressor is needed.

use sha1::{Digest, Sha1};

/// The type of a git object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Commit,
    Tree,
    Blob,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
        }
    }

    fn pack_type(self) -> u8 {
        match self {
            Kind::Commit => 1,
            Kind::Tree => 2,
            Kind::Blob => 3,
        }
    }
}

/// A git object: its kind and its body, without the `<kind> <len>\0` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub kind: Kind,
    pub body: Vec<u8>,
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    s
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `id`, 40 hex digits, as its 20 bytes.
fn parse_id(id: &str) -> Result<[u8; 20], String> {
    let b = id.as_bytes();
    if b.len() != 40 {
        return Err(format!("not an object id: {id:?}"));
    }
    let mut out = [0u8; 20];
    for (i, pair) in b.chunks(2).enumerate() {
        match (hex_val(pair[0]), hex_val(pair[1])) {
            (Some(h), Some(l)) => out[i] = (h << 4) | l,
            _ => return Err(format!("not an object id: {id:?}")),
        }
    }
    Ok(out)
}

impl Object {
    /// A blob holding `body`.
    pub fn blob(body: &[u8]) -> Object {
        Object {
            kind: Kind::Blob,
            body: body.to_vec(),
        }
    }

    /// A tree of regular files (mode 100644), from `(name, 40-hex blob id)`, sorted by name.
    pub fn tree(entries: &[(String, String)]) -> Result<Object, String> {
        let mut parsed = Vec::with_capacity(entries.len());
        for (name, id) in entries {
            if name.is_empty() || name.contains('/') || name.contains('\0') {
                return Err(format!("not a file name for a tree: {name:?}"));
            }
            parsed.push((name.as_bytes(), parse_id(id)?));
        }
        parsed.sort_by(|a, b| a.0.cmp(b.0));
        let mut body = Vec::new();
        for (name, raw) in parsed {
            body.extend_from_slice(b"100644 ");
            body.extend_from_slice(name);
            body.push(0);
            body.extend_from_slice(&raw);
        }
        Ok(Object {
            kind: Kind::Tree,
            body,
        })
    }

    /// A commit of `tree` with `parents`, by `who` (such as `pigeon <ci@pigeon.dev>`) at
    /// `unix_secs`, UTC; the message gains a trailing newline when it has none.
    pub fn commit(
        tree: &str,
        parents: &[String],
        who: &str,
        unix_secs: i64,
        message: &str,
    ) -> Result<Object, String> {
        let mut s = format!("tree {}\n", to_hex(&parse_id(tree)?));
        for p in parents {
            s.push_str(&format!("parent {}\n", to_hex(&parse_id(p)?)));
        }
        s.push_str(&format!("author {who} {unix_secs} +0000\n"));
        s.push_str(&format!("committer {who} {unix_secs} +0000\n"));
        s.push('\n');
        s.push_str(message);
        if !message.ends_with('\n') {
            s.push('\n');
        }
        Ok(Object {
            kind: Kind::Commit,
            body: s.into_bytes(),
        })
    }

    /// The object's id: the lowercase hex SHA-1 of `<kind> <len>\0<body>`.
    pub fn id(&self) -> String {
        let mut h = Sha1::new();
        h.update(format!("{} {}\0", self.kind.word(), self.body.len()).as_bytes());
        h.update(&self.body);
        to_hex(&h.finalize())
    }
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// `data` as a zlib stream of stored deflate blocks, with its Adler-32 after.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let n = data.len().div_ceil(65535);
        for (i, chunk) in data.chunks(65535).enumerate() {
            out.push(u8::from(i + 1 == n));
            // At most 65535, by the chunk size.
            let len = chunk.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// A version-2 pack of `objects`, undeltified, with its SHA-1 trailer.
pub fn pack(objects: &[Object]) -> Vec<u8> {
    let mut out = b"PACK".to_vec();
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for o in objects {
        let mut size = o.body.len();
        let mut byte = (o.kind.pack_type() << 4) | (size & 0x0f) as u8;
        size >>= 4;
        while size > 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);
        out.extend_from_slice(&zlib_stored(&o.body));
    }
    let digest = Sha1::digest(&out);
    out.extend_from_slice(&digest);
    out
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

    #[test]
    fn blob_ids_match_git() {
        assert_eq!(Object::blob(b"hello\n").id(), HELLO);
        assert_eq!(
            Object::blob(b"").id(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn tree_ids_match_git() {
        assert_eq!(
            Object::tree(&[]).unwrap().id(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
        let t = Object::tree(&[("hello.txt".into(), HELLO.into())]).unwrap();
        assert_eq!(t.id(), "aaa96ced2d9a1c8e72c56b253a0e2fe78393feb7");
    }

    #[test]
    fn tree_entries_are_sorted_by_name() {
        let t = Object::tree(&[("b".into(), HELLO.into()), ("a".into(), HELLO.into())]).unwrap();
        assert!(t.body.starts_with(b"100644 a\0"));
        assert_eq!(&t.body[29..38], b"100644 b\0");
    }

    #[test]
    fn commit_body_is_exact() {
        let tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
        let c = Object::commit(
            tree,
            &[HELLO.to_uppercase()],
            "pigeon <ci@pigeon.dev>",
            1700000000,
            "msg",
        )
        .unwrap();
        let want = format!(
            "tree {tree}\nparent {HELLO}\nauthor pigeon <ci@pigeon.dev> 1700000000 +0000\n\
             committer pigeon <ci@pigeon.dev> 1700000000 +0000\n\nmsg\n"
        );
        assert_eq!(String::from_utf8(c.body).unwrap(), want);
    }

    #[test]
    fn bad_names_and_ids_are_refused() {
        assert!(Object::tree(&[("a".into(), "xyz".into())]).is_err());
        assert!(Object::tree(&[("a/b".into(), HELLO.into())]).is_err());
        assert!(Object::tree(&[("".into(), HELLO.into())]).is_err());
        assert!(Object::commit("nope", &[], "p <p@p>", 0, "m").is_err());
    }

    #[test]
    fn zlib_trailer_is_adler32() {
        let z = zlib_stored(b"Wikipedia");
        assert_eq!(&z[z.len() - 4..], &0x11E6_0398u32.to_be_bytes());
    }

    #[test]
    fn zlib_splits_large_input_into_blocks() {
        let z = zlib_stored(&vec![7u8; 70_000]);
        assert_eq!(&z[0..2], &[0x78, 0x01]);
        assert_eq!(z[2], 0x00);
        assert_eq!(&z[3..5], &65535u16.to_le_bytes());
        assert_eq!(&z[5..7], &0u16.to_le_bytes());
        let s = 7 + 65535;
        assert_eq!(z[s], 0x01);
        assert_eq!(&z[s + 1..s + 3], &4465u16.to_le_bytes());
        assert_eq!(&z[s + 3..s + 5], &(!4465u16).to_le_bytes());
    }

    #[test]
    fn pack_has_header_object_headers_and_trailer() {
        let objs = [
            Object::blob(&[1u8; 20]),
            Object::blob(b"hello\n"),
            Object::tree(&[]).unwrap(),
        ];
        let p = pack(&objs);
        assert!(p.starts_with(b"PACK\0\0\0\x02\0\0\0\x03"));
        assert_eq!(p[12], 0x80 | (3 << 4) | 4);
        assert_eq!(p[13], 0x01);
        let (body, trailer) = p.split_at(p.len() - 20);
        assert_eq!(trailer, &Sha1::digest(body)[..]);
    }
}

#[cfg(test)]
mod git_accepts {
    //! Checks the pack with `git index-pack` when `PIGEON_PACK_OUT` names a file to write it to.
    use super::*;

    #[test]
    fn write_a_notes_like_pack_for_git_to_check() {
        let Ok(path) = std::env::var("PIGEON_PACK_OUT") else {
            return;
        };
        let blob = Object::blob(&vec![b'x'; 70_000]);
        let tree = Object::tree(&[("a".repeat(40), blob.id())]).unwrap();
        let commit = Object::commit(&tree.id(), &[], "pigeon <ci@pigeon.dev>", 1, "notes").unwrap();
        std::fs::write(&path, pack(&[blob, tree, commit.clone()])).unwrap();
        std::fs::write(format!("{path}.commit"), commit.id()).unwrap();
    }
}

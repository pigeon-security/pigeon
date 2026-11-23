//! Zip archives, opened safely (decision 13.2): limits on depth, entries, member size, ratio
//! and total inflated bytes, counted on the bytes as they inflate; encrypted members not
//! opened; member names normalised and never used as paths.

use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};

use crate::Error;

/// The limits for one top-level archive, nested archives included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Archives nested deeper than this are kept as files, not opened. The top level is depth 0.
    pub max_depth: u32,
    pub max_entries: u64,
    pub max_member_bytes: u64,
    /// Inflated over compressed, checked for members over [`Limits::ratio_floor`] bytes.
    pub max_ratio: u64,
    pub ratio_floor: u64,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_entries: 100_000,
            max_member_bytes: 8 << 30,
            max_ratio: 200,
            ratio_floor: 1 << 20,
            max_total_bytes: 32 << 30,
        }
    }
}

/// Why a file or member was treated specially. Serialised in snake case, joined by commas in
/// the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flag {
    Encrypted,
    UnsafePath,
    Symlink,
    BombSuspect,
    TooManyEntries,
    MemberTooLarge,
    DepthLimit,
    CrcMismatch,
    Unreadable,
    /// An archive type this runner does not open (tar, gzip, 7z, rar).
    NotOpened,
    /// Larger than the spool: hashed while streaming, not opened.
    OverSpool,
}

/// Whether `head` starts like a zip archive (a local file header or an empty archive's end
/// record).
pub fn is_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06")
}

/// One entry of an archive's central directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Its position in the central directory, from 0.
    pub index: u64,
    /// The name as stored, escaped for display (no control characters).
    pub raw_name: String,
    /// The normalised path: `\` to `/`, no leading `/` or drive, no `.` or `..` segments.
    pub path: String,
    pub declared_size: u64,
    pub compressed_size: u64,
    pub is_dir: bool,
    /// Flags from the directory alone: `encrypted`, `unsafe_path`, `symlink`.
    pub flags: Vec<Flag>,
}

/// The inflated bytes budget shared by an archive and the archives nested in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub limits: Limits,
    /// Inflated bytes used so far.
    pub used: u64,
    /// Entries seen so far.
    pub entries: u64,
}

impl Budget {
    /// A fresh budget under `limits`.
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            used: 0,
            entries: 0,
        }
    }
}

impl Budget {
    /// Counts one more entry opened. Answers `TooManyEntries` when the limit is already reached.
    pub fn take_entry(&mut self) -> Result<(), Flag> {
        if self.entries >= self.limits.max_entries {
            return Err(Flag::TooManyEntries);
        }
        self.entries += 1;
        Ok(())
    }
}

/// An open zip archive. The `zip` crate keeps one entry per name, so when a directory holds the
/// same name twice only one of them is listed (the later data wins).
pub struct Zip<R: Read + Seek> {
    archive: zip::ZipArchive<R>,
    entries: Vec<Entry>,
}

/// The AES extra field id, which marks an encrypted member.
const AES_EXTRA_ID: u16 = 0x9901;

/// Whether an extra-field block holds a field with id `want`.
fn has_extra(mut extra: &[u8], want: u16) -> bool {
    while extra.len() >= 4 {
        let id = u16::from_le_bytes([extra[0], extra[1]]);
        let len = usize::from(u16::from_le_bytes([extra[2], extra[3]]));
        if id == want {
            return true;
        }
        match extra.get(4 + len..) {
            Some(rest) => extra = rest,
            None => return false,
        }
    }
    false
}

/// Normalises a stored member name (decision 13.2): `\` to `/`, no leading `/` or drive letter,
/// no `.` or `..` segments. Answers the path and whether the name was unsafe: it needed any of
/// that, or holds a NUL or another control character.
pub fn safe_path(raw: &str) -> (String, bool) {
    let flat = raw.replace('\\', "/");
    let mut rest = flat.as_str();
    let b = rest.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        rest = &rest[2..];
    }
    let parts: Vec<&str> = rest
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    let path = parts.join("/");
    let trimmed = raw.strip_suffix('/').unwrap_or(raw);
    let unsafe_path = path != trimmed || raw.chars().any(|c| c.is_control());
    (path, unsafe_path)
}

/// Escapes control characters for display, so a raw name is safe to print and store.
fn escape_name(raw: &str) -> String {
    raw.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

impl<R: Read + Seek> Zip<R> {
    /// Reads the central directory. An unreadable one is an [`Error::Archive`]; the caller keeps
    /// the archive as a file with flag `unreadable`.
    pub fn open(reader: R) -> Result<Self, Error> {
        let mut archive =
            zip::ZipArchive::new(reader).map_err(|e| Error::Archive(e.to_string()))?;
        let mut entries = Vec::with_capacity(archive.len());
        for i in 0..archive.len() {
            let f = archive
                .by_index_raw(i)
                .map_err(|e| Error::Archive(e.to_string()))?;
            let name = f.name().to_string();
            let (path, unsafe_path) = safe_path(&name);
            let mut flags = Vec::new();
            if f.encrypted() || f.extra_data().is_some_and(|x| has_extra(x, AES_EXTRA_ID)) {
                flags.push(Flag::Encrypted);
            }
            if unsafe_path {
                flags.push(Flag::UnsafePath);
            }
            if f.is_symlink() {
                flags.push(Flag::Symlink);
            }
            entries.push(Entry {
                index: i as u64,
                raw_name: escape_name(&name),
                path,
                declared_size: f.size(),
                compressed_size: f.compressed_size(),
                is_dir: f.is_dir(),
                flags,
            });
        }
        Ok(Self { archive, entries })
    }

    /// The entries, in directory order.
    pub fn entries(&self) -> Vec<Entry> {
        self.entries.clone()
    }

    /// Inflates entry `index` into `out`, charging `budget`. Answers the bytes written, or the
    /// flag that stopped it (`member_too_large`, `bomb_suspect`, `crc_mismatch`, `encrypted`,
    /// `symlink`, `too_many_entries`); when a flag stops it, what was written to `out` must be
    /// discarded. Encrypted members and symlinks are never read. Each call counts one entry
    /// against the budget. A bad `index` or a directory is an [`Error::Archive`]; a failing
    /// `out` is an [`Error::Io`]. A member that cannot be inflated is `crc_mismatch`.
    pub fn extract(
        &mut self,
        index: u64,
        budget: &mut Budget,
        out: &mut dyn std::io::Write,
    ) -> Result<Result<u64, Flag>, Error> {
        let entry = usize::try_from(index)
            .ok()
            .and_then(|i| self.entries.get(i))
            .ok_or_else(|| Error::Archive(format!("no entry {index}")))?;
        if entry.is_dir {
            return Err(Error::Archive(format!("entry {index} is a directory")));
        }
        if entry.flags.contains(&Flag::Encrypted) {
            return Ok(Err(Flag::Encrypted));
        }
        if entry.flags.contains(&Flag::Symlink) {
            return Ok(Err(Flag::Symlink));
        }
        let compressed = entry.compressed_size.max(1);
        if let Err(flag) = budget.take_entry() {
            return Ok(Err(flag));
        }
        let limits = budget.limits.clone();
        let mut file = match self.archive.by_index(index as usize) {
            Ok(f) => f,
            Err(zip::result::ZipError::UnsupportedArchive(_)) => return Ok(Err(Flag::Encrypted)),
            Err(_) => return Ok(Err(Flag::CrcMismatch)),
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut member: u64 = 0;
        loop {
            let n = match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    return Ok(Err(Flag::CrcMismatch));
                }
                Err(e) => return Err(Error::Io(e.to_string())),
            };
            member += n as u64;
            budget.used = budget.used.saturating_add(n as u64);
            if member > limits.max_member_bytes {
                return Ok(Err(Flag::MemberTooLarge));
            }
            if budget.used > limits.max_total_bytes {
                return Ok(Err(Flag::BombSuspect));
            }
            if member > limits.ratio_floor && member / compressed > limits.max_ratio {
                return Ok(Err(Flag::BombSuspect));
            }
            out.write_all(&buf[..n])?;
        }
        Ok(Ok(member))
    }
}

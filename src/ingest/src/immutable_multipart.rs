//! Bounded, server-side checkpoints for a create-only R2 blob upload.
use serde::{Deserialize, Serialize};

use crate::{backup_digest::BackupDigest, datastore};

pub const PART_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_BYTES: u64 = 10_000 * PART_BYTES;

#[derive(Debug, PartialEq, Eq)]
pub enum PartAction {
    Upload,
    Replay,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reconciliation {
    Ours,
    AlreadyPresent,
    Unresolved,
}

/// The atomic completion is the only write that publishes an object. An existing object's
/// metadata is proof of our own write only if it carries this upload's unique write ID.
pub fn reconcile(
    expected: &Write,
    observed: Option<(u64, Option<&str>, Option<&str>)>,
) -> Reconciliation {
    match observed {
        Some((size, Some(digest), Some(id)))
            if size == expected.size && digest == expected.sha256 && id == expected.write_id =>
        {
            Reconciliation::Ours
        }
        Some(_) => Reconciliation::AlreadyPresent,
        None => Reconciliation::Unresolved,
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    pub size: u64,
    pub sha256: String,
    pub write_id: String,
}

impl Write {
    pub fn new(key: &str, size: u64, write_id: String) -> Option<Self> {
        let sha256 = key.rsplit('/').next()?;
        if !(PART_BYTES + 1..=MAX_BYTES).contains(&size)
            || !digest_ok(sha256)
            || key != crate::blob_key(sha256)
            || !digest_ok(&write_id)
        {
            return None;
        }
        Some(Self {
            size,
            sha256: sha256.into(),
            write_id,
        })
    }

    pub fn key(&self) -> String {
        crate::blob_key(&self.sha256)
    }
}

fn digest_ok(sha: &str) -> bool {
    sha.len() == 64
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Debug, Deserialize, Serialize)]
struct Part {
    sha256: String,
    etag: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub write: Write,
    digest: BackupDigest,
    parts: Vec<Part>,
    pending: Option<String>,
}

impl Checkpoint {
    pub fn new(write: Write) -> Self {
        Self {
            write,
            digest: BackupDigest::default(),
            parts: Vec::new(),
            pending: None,
        }
    }

    pub fn part_bytes(&self, number: u32) -> Option<u64> {
        let offset = u64::from(number.checked_sub(1)?) * PART_BYTES;
        let remaining = self.write.size.checked_sub(offset)?;
        (remaining > 0).then_some(remaining.min(PART_BYTES))
    }

    /// Persist this pin before the provider request. A delayed or retried request can then only
    /// upload the same bytes for a part number, even if its Worker lease has expired.
    pub fn pin(&mut self, number: u32, bytes: &[u8]) -> Option<PartAction> {
        if self.part_bytes(number)? != bytes.len() as u64 {
            return None;
        }
        let index = number as usize - 1;
        let digest = datastore::sha256_hex(bytes);
        if let Some(part) = self.parts.get(index) {
            return (part.sha256 == digest).then_some(PartAction::Replay);
        }
        if index != self.parts.len() {
            return None;
        }
        if let Some(pinned) = &self.pending
            && pinned != &digest
        {
            return None;
        }
        if self.part_bytes(number + 1).is_none()
            && self.digest.clone().finish(bytes).as_deref() != Some(self.write.sha256.as_str())
        {
            return None;
        }
        self.pending = Some(digest);
        Some(PartAction::Upload)
    }

    pub fn confirm(&mut self, number: u32, bytes: &[u8], etag: &str) -> Option<()> {
        let etag = etag.trim_matches('"');
        if !valid_etag(etag) || self.pin(number, bytes)? != PartAction::Upload {
            return None;
        }
        if self.part_bytes(number + 1).is_some() {
            self.digest.update(bytes)?;
        }
        self.parts.push(Part {
            sha256: self.pending.take()?,
            etag: etag.to_string(),
        });
        Some(())
    }

    /// Only fully verified part lists can be encoded for create-only completion.
    pub fn completion_xml(&self) -> Option<String> {
        if self.pending.is_some() || self.parts.len() as u64 != self.write.size.div_ceil(PART_BYTES)
        {
            return None;
        }
        let mut xml = String::from("<CompleteMultipartUpload>");
        for (i, part) in self.parts.iter().enumerate() {
            if !valid_etag(&part.etag) {
                return None;
            }
            let number = i + 1;
            let etag = &part.etag;
            xml.push_str(&format!(
                "<Part><PartNumber>{number}</PartNumber><ETag>\"{etag}\"</ETag></Part>"
            ));
        }
        xml.push_str("</CompleteMultipartUpload>");
        Some(xml)
    }
}

fn valid_etag(etag: &str) -> bool {
    etag.len() == 32 && etag.bytes().all(|c| c.is_ascii_hexdigit())
}

/// CompleteMultipartUpload evaluates this condition atomically at commit. A conditional
/// initiation alone never provides create-only publication.
pub const CREATE_ONLY: (&str, &str) = ("if-none-match", "*");

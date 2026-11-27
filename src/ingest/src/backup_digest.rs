//! SHA-256 checkpoints at full block boundaries for a bounded multipart backup.
use serde::{Deserialize, Serialize};

const BLOCK_BYTES: usize = 64;
const INITIAL: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupDigest {
    state: [u32; 8],
    bytes: u64,
}

impl Default for BackupDigest {
    fn default() -> Self {
        Self {
            state: INITIAL,
            bytes: 0,
        }
    }
}

impl BackupDigest {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// A nonterminal chunk must end at a block boundary so its state has no buffered bytes.
    pub fn update(&mut self, bytes: &[u8]) -> Option<()> {
        if !bytes.len().is_multiple_of(BLOCK_BYTES)
            || !self.bytes.is_multiple_of(BLOCK_BYTES as u64)
        {
            return None;
        }
        let total = self.bytes.checked_add(bytes.len() as u64)?;
        total.checked_mul(8)?;
        // Keep the temporary compressor input small even when a provider part is large.
        let mut blocks = Vec::with_capacity(32);
        for batch in bytes.chunks(32 * BLOCK_BYTES) {
            for block in batch.as_chunks::<BLOCK_BYTES>().0 {
                blocks.push((*block).into());
            }
            sha2::compress256(&mut self.state, &blocks);
            blocks.clear();
        }
        self.bytes = total;
        Some(())
    }

    /// Finish with the final range, including SHA-256's fixed-width bit count.
    pub fn finish(mut self, bytes: &[u8]) -> Option<String> {
        let full = bytes.len() / BLOCK_BYTES * BLOCK_BYTES;
        self.update(&bytes[..full])?;
        let tail = &bytes[full..];
        let bit_count = self.bytes.checked_add(tail.len() as u64)?.checked_mul(8)?;
        let mut padding = [0_u8; 2 * BLOCK_BYTES];
        padding[..tail.len()].copy_from_slice(tail);
        padding[tail.len()] = 128;
        let length = if tail.len() < 56 {
            BLOCK_BYTES
        } else {
            2 * BLOCK_BYTES
        };
        padding[length - 8..length].copy_from_slice(&bit_count.to_be_bytes());
        self.update(&padding[..length])?;
        Some(
            self.state
                .iter()
                .map(|word| format!("{word:08x}"))
                .collect(),
        )
    }
}

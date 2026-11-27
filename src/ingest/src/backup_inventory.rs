//! Checkpoint messages for enumerating an instance's existing Data bucket into a backup queue.
use serde::{Deserialize, Serialize};

pub const PAGE_SIZE: u32 = 48;

/// The queue's initial R2 event may be delivered more than once.
pub const INSERT_PROGRESS: &str =
    "INSERT OR IGNORE INTO backup_progress (id, body) VALUES (?1, ?2)";

/// Redelivery of a claimed message resumes its own lease. Other messages cannot race a part.
pub const CLAIM_PROGRESS: &str = "UPDATE backup_progress SET lease_owner = ?1, lease_until = ?2
     WHERE id = ?3 AND (lease_owner IS NULL OR lease_owner = ?1 OR lease_until <= ?4)";

/// A stale worker cannot replace a checkpoint. Hold the lease while delivering a terminal audit.
pub const SAVE_PROGRESS: &str = "UPDATE backup_progress SET body = ?1,
     lease_owner = CASE WHEN ?4 THEN NULL ELSE lease_owner END,
     lease_until = CASE WHEN ?4 THEN 0 ELSE lease_until END
     WHERE id = ?2 AND lease_owner = ?3";

#[derive(Deserialize, Serialize)]
#[serde(tag = "pigeon_backup", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Inventory {
        generation: String,
        cursor: Option<String>,
    },
    Copy {
        generation: String,
        key: String,
        etag: String,
    },
    Multipart {
        generation: String,
        id: String,
    },
}

impl Message {
    pub fn generation(&self) -> &str {
        match self {
            Self::Inventory { generation, .. }
            | Self::Copy { generation, .. }
            | Self::Multipart { generation, .. } => generation,
        }
    }
}

/// Generate at most one bounded page and its opaque continuation checkpoint.
pub fn next(
    generation: &str,
    input_cursor: Option<&str>,
    entries: Vec<(String, String)>,
    next_cursor: Option<String>,
) -> Option<Vec<Message>> {
    if entries.len() > PAGE_SIZE as usize
        || next_cursor
            .as_deref()
            .is_some_and(|next| Some(next) == input_cursor)
    {
        return None;
    }
    let mut out: Vec<Message> = entries
        .into_iter()
        .map(|(key, etag)| Message::Copy {
            generation: generation.into(),
            key,
            etag,
        })
        .collect();
    if let Some(cursor) = next_cursor {
        out.push(Message::Inventory {
            generation: generation.into(),
            cursor: Some(cursor),
        });
    }
    Some(out)
}

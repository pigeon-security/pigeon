//! Checkpoint messages for enumerating an instance's existing Data bucket into a backup queue.
use serde::{Deserialize, Serialize};

pub const PAGE_SIZE: u32 = 48;

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
}

impl Message {
    pub fn generation(&self) -> &str {
        match self {
            Self::Inventory { generation, .. } | Self::Copy { generation, .. } => generation,
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

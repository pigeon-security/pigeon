//! Resumable backup of one immutable R2 object version across Queue deliveries.
use pigeon_ingest::audit::Cleanup;
use pigeon_ingest::backup_digest::BackupDigest;
use pigeon_ingest::backup_inventory::{CLAIM_PROGRESS, INSERT_PROGRESS, Message, SAVE_PROGRESS};
use pigeon_ingest::datastore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use worker::{D1Database, Env, Result};

use super::{Copy, MAX_PARTS, PART_BYTES, Source, Target, audit_at, fail};

/// One invocation may own the checkpoint for its full Queue wall-time budget. The same
/// Queue message can acquire its lease again when Cloudflare redelivers it.
const LEASE_MS: u64 = 15 * 60 * 1000 + 1000;

#[derive(Deserialize, Serialize)]
struct Progress {
    generation: String,
    key: String,
    etag: String,
    size: u64,
    started_ms: u64,
    stage: Stage,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
enum Stage {
    Hash {
        digest: BackupDigest,
    },
    Upload {
        sha: String,
        id: Option<String>,
        parts: Vec<String>,
        digest: BackupDigest,
    },
    Done {
        copied: Option<bool>,
        finished_ms: u64,
        cleanup: Option<bool>,
        audited: bool,
    },
}

fn now() -> u64 {
    worker::js_sys::Date::now() as u64
}

fn message(generation: &str, id: &str) -> Message {
    Message::Multipart {
        generation: generation.into(),
        id: id.into(),
    }
}

pub(super) async fn enqueue(
    env: &Env,
    generation: String,
    source: &Source<'_>,
    started_ms: u64,
) -> Result<()> {
    let id = datastore::sha256_hex(
        &serde_json::to_vec(&(&generation, source.key, &source.etag, source.size))
            .map_err(|_| fail("could not identify the backup object version"))?,
    );
    let progress = Progress {
        generation,
        key: source.key.into(),
        etag: source.etag.clone(),
        size: source.size,
        started_ms,
        stage: Stage::Hash {
            digest: BackupDigest::default(),
        },
    };
    let body = encode(&progress)?;
    let db = env.d1("DB")?;
    db.prepare(INSERT_PROGRESS)
        .bind(&[crate::store::js(&json!(id)), crate::store::js(&json!(body))])?
        .run()
        .await?;
    env.queue("BACKUP_QUEUE")?
        .send(message(&progress.generation, &id))
        .await
}

fn encode(progress: &Progress) -> Result<String> {
    serde_json::to_string(progress).map_err(|_| fail("could not encode the backup checkpoint"))
}

async fn claim(db: &D1Database, id: &str, owner: &str) -> Result<bool> {
    let now = now();
    let result = db
        .prepare(CLAIM_PROGRESS)
        .bind(&[
            crate::store::js(&json!(owner)),
            crate::store::js(&json!(now + LEASE_MS)),
            crate::store::js(&json!(id)),
            crate::store::js(&json!(now)),
        ])?
        .run()
        .await?;
    Ok(result.meta()?.and_then(|m| m.changes) == Some(1))
}

/// Persist before queueing continuation. If enqueue fails, redelivery reloads this checkpoint.
async fn save(
    db: &D1Database,
    id: &str,
    owner: &str,
    progress: &Progress,
    release: bool,
) -> Result<()> {
    let result = db
        .prepare(SAVE_PROGRESS)
        .bind(&[
            crate::store::js(&json!(encode(progress)?)),
            crate::store::js(&json!(id)),
            crate::store::js(&json!(owner)),
            crate::store::js(&json!(u8::from(release))),
        ])?
        .run()
        .await?;
    if result.meta()?.and_then(|m| m.changes) != Some(1) {
        return Err(fail("the backup checkpoint lease changed"));
    }
    Ok(())
}

fn done(copied: Option<bool>, cleanup: Option<bool>) -> Stage {
    Stage::Done {
        copied,
        finished_ms: now(),
        cleanup,
        audited: false,
    }
}

async fn advance(env: &Env, progress: &mut Progress) -> Result<()> {
    let source = Source {
        bucket: env.bucket("DATA")?,
        key: &progress.key,
        etag: progress.etag.clone(),
        size: progress.size,
    };
    match &mut progress.stage {
        Stage::Hash { digest } => {
            let offset = digest.bytes();
            let left = source
                .size
                .checked_sub(offset)
                .ok_or_else(|| fail("invalid backup hash checkpoint"))?;
            if left == 0 || !offset.is_multiple_of(PART_BYTES) {
                return Err(fail("invalid backup hash offset"));
            }
            let bytes = source.read(offset, left.min(PART_BYTES)).await?;
            if left > PART_BYTES {
                digest
                    .update(&bytes)
                    .ok_or_else(|| fail("invalid backup hash range"))?;
            } else {
                let sha = digest
                    .clone()
                    .finish(&bytes)
                    .ok_or_else(|| fail("invalid backup hash tail"))?;
                if source.key.starts_with("blobs/sha256/")
                    && source.key != pigeon_ingest::blob_key(&sha)
                {
                    return Err(fail("the Data blob does not match the checksum in its key"));
                }
                progress.stage = Stage::Upload {
                    sha,
                    id: None,
                    parts: Vec::new(),
                    digest: BackupDigest::default(),
                };
            }
        }
        Stage::Upload {
            sha,
            id,
            parts,
            digest,
        } => {
            let destination = datastore::backup_object_key(source.key, sha);
            let Some(upload) = id.as_deref() else {
                let target = Target::verified(env).await?;
                if target.exists(&destination, sha, source.size).await? {
                    progress.stage = done(Some(false), None);
                } else {
                    *id = Some(
                        target
                            .begin_multipart(source.key, &destination, sha)
                            .await?,
                    );
                }
                return Ok(());
            };
            let part_count = parts.len() as u64;
            let expected_parts = source.size.div_ceil(PART_BYTES);
            if part_count > expected_parts {
                return Err(fail("the backup checkpoint has too many uploaded parts"));
            }
            let offset = part_count * PART_BYTES;
            if part_count == expected_parts {
                let target = Target::verified(env).await?;
                if target.exists(&destination, sha, source.size).await? {
                    // Could be retrying an accepted completion or racing an identical old copy.
                    let clean = target.abort_multipart(&destination, upload).await.is_ok();
                    progress.stage = done(Some(true), Some(clean));
                    return Ok(());
                }
                target
                    .complete_multipart(&destination, upload, parts)
                    .await?;
                if !target.exists(&destination, sha, source.size).await? {
                    return Err(fail("B2 did not confirm the protected multipart copy"));
                }
                progress.stage = done(Some(true), Some(true));
                return Ok(());
            }
            if digest.bytes() != offset {
                return Err(fail("invalid backup upload checksum checkpoint"));
            }
            let remaining = source.size - offset;
            let bytes = source.read(offset, remaining.min(PART_BYTES)).await?;
            if remaining > PART_BYTES {
                digest
                    .update(&bytes)
                    .ok_or_else(|| fail("invalid backup multipart range"))?;
            } else if digest.clone().finish(&bytes).as_ref() != Some(sha) {
                return Err(fail("the Data object changed while copying"));
            }
            let target = Target::open(env).await?;
            parts.push(
                target
                    .put_part(&destination, upload, parts.len() + 1, &bytes)
                    .await?,
            );
        }
        Stage::Done { .. } => {}
    }
    Ok(())
}

async fn abort(env: &Env, progress: &Progress) -> Option<bool> {
    let Stage::Upload {
        sha, id: Some(id), ..
    } = &progress.stage
    else {
        return None;
    };
    let destination = datastore::backup_object_key(&progress.key, sha);
    Some(match Target::open(env).await {
        Ok(target) => target.abort_multipart(&destination, id).await.is_ok(),
        Err(_) => false,
    })
}

pub async fn resume(
    env: &Env,
    generation: &str,
    id: &str,
    owner: &str,
    final_attempt: bool,
) -> Result<()> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(fail("invalid backup checkpoint ID"));
    }
    if env.var("BACKUP_AUDIT_REQUIRED").is_ok() {
        env.bucket("JOBS_AUDIT")?;
    }
    let db = env.d1("DB")?;
    if !claim(&db, id, owner).await? {
        // An active worker will enqueue the next checkpoint. An expired duplicate has no work.
        return Ok(());
    }
    let body = db
        .prepare("SELECT body FROM backup_progress WHERE id = ?1")
        .bind(&[crate::store::js(&json!(id))])?
        .first::<String>(Some("body"))
        .await?
        .ok_or_else(|| fail("backup checkpoint disappeared"))?;
    let mut progress: Progress =
        serde_json::from_str(&body).map_err(|_| fail("invalid backup checkpoint"))?;
    if progress.generation != generation
        || progress.size <= PART_BYTES
        || progress.size.div_ceil(PART_BYTES) > MAX_PARTS
    {
        return Err(fail("backup checkpoint is outside the approved generation"));
    }

    if let Err(err) = advance(env, &mut progress).await {
        if !final_attempt {
            // Do not save an in-memory hash mutation if the matching part was not confirmed.
            // Redelivery of this same message can reacquire its lease and retry the same part.
            return Err(err);
        }
        progress.stage = done(None, abort(env, &progress).await);
    }
    if matches!(progress.stage, Stage::Done { audited: false, .. }) {
        // Commit the stable terminal time and outcome before writing the receipt.
        save(&db, id, owner, &progress, false).await?;
    }
    if let Stage::Done {
        copied,
        finished_ms,
        cleanup,
        audited,
    } = &mut progress.stage
    {
        if !*audited {
            let result = Ok(copied.map(|stored| Copy {
                size: progress.size,
                stored,
                scheduled: false,
            }));
            audit_at(
                env,
                id,
                progress.started_ms,
                *finished_ms,
                cleanup.map(|clean| {
                    if clean {
                        Cleanup::Confirmed
                    } else {
                        Cleanup::Unconfirmed
                    }
                }),
                &result,
            )
            .await?;
            *audited = true;
        }
        return save(&db, id, owner, &progress, true).await;
    }
    save(&db, id, owner, &progress, true).await?;
    env.queue("BACKUP_QUEUE")?
        .send(message(generation, id))
        .await
}

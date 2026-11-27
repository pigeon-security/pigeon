//! The data Worker's email handler, without the Workers runtime (docs/plans/email.md, "The
//! flow"): hash and store the bytes once, write the record rows (`items`, `sightings`,
//! `emails`), pick the mailbox, decide the forward. The runtime's pieces (R2, D1, the message's
//! forward) come in through [`Bindings`] and the caller, so the smoke suite runs this file
//! natively with stand-ins.
//!
//! It names no crate of its own (`crate::`): the smoke suite includes it by path.

use pigeon_email::{ContentRef, Message, Source};
use pigeon_ingest::datastore::{self, Auth, CATCH_ALL, EmailConfig, SOURCE_EMAIL_ROUTING};
use pigeon_ingest::index::Statement;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What the handler needs of the runtime. Each answer's error is a short reason that names no
/// content.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait Bindings {
    /// Whether the bucket holds `key`.
    async fn has(&self, key: &str) -> Result<bool, String>;
    /// Writes `bytes` at `key`, checked against `sha256` by R2.
    async fn put(&self, key: &str, bytes: &[u8], sha256: &str) -> Result<(), String>;
    /// Runs `statements` as one D1 batch.
    async fn run(&self, statements: Vec<Statement>) -> Result<(), String>;
    /// The handler's configuration, if the plan wrote one.
    async fn config(&self) -> Result<Option<EmailConfig>, String>;
}

/// One message as Email Routing hands it over.
pub struct Incoming<'a> {
    pub envelope_from: &'a str,
    pub envelope_to: &'a str,
    /// The whole message, as received.
    pub raw: &'a [u8],
    /// Its SHA-256 and size, computed while the bytes streamed in ([`Hasher`]).
    pub content: ContentRef,
    /// The values of every [`datastore::LOOP_HEADER`] it arrived with.
    pub loop_marks: Vec<String>,
    /// When it arrived, RFC 3339 UTC.
    pub received_at: String,
}

/// What became of a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Not stored: reject it, so the sender gets a bounce. The reason is safe to send.
    Reject(String),
    /// Stored (or already held); forward it if `forward` names a destination.
    Stored(Stored),
}

/// A stored message.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    /// The instance the configuration names, carried in the forward's loop header.
    pub instance: String,
    pub item_id: String,
    pub size: u64,
    pub mailbox: String,
    /// The bytes were already in the store: only a sighting was added.
    pub duplicate: bool,
    /// Where to forward it, if anywhere.
    pub forward: Option<String>,
    /// It came back through this instance's own forward, so it is not forwarded again.
    pub looped: bool,
}

/// The reason a message is rejected when the store is off.
pub const NOT_SET_UP: &str = "this address is not set up";
/// The reason a message is rejected when storing it failed.
pub const NOT_STORED: &str = "the message could not be stored; try again later";
/// The reason a message over the size limit is rejected.
pub const TOO_LARGE: &str = "the message is larger than 25 MiB";

/// SHA-256 and size of bytes as they stream in, so the hash is ready when the last chunk is.
#[derive(Default)]
pub struct Hasher {
    digest: Sha256,
    size: u64,
}

impl Hasher {
    pub fn update(&mut self, chunk: &[u8]) {
        self.digest.update(chunk);
        self.size += chunk.len() as u64;
    }

    /// The bytes so far, in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn finish(self) -> ContentRef {
        ContentRef {
            sha256: format!("{:x}", self.digest.finalize()),
            size: self.size,
        }
    }
}

/// The kind of an item of media type `t` (ingest.md decision 3).
pub fn kind_of(t: &str) -> &'static str {
    let t = t.to_ascii_lowercase();
    match t.split('/').next().unwrap_or("") {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ if t == "message/rfc822" => "email",
        _ if t.contains("zip") || t.contains("tar") || t.contains("compressed") => "archive",
        "text" | "application" => "document",
        _ => "other",
    }
}

/// The message fingerprint (ingest.md decision 2, tier 2): SHA-256 of the normalized
/// Message-ID, else of From, Date, Subject and the text's hash.
pub fn fingerprint(m: &Message) -> String {
    let basis = match &m.message_id {
        Some(id) => format!("id:{}", id.trim().to_ascii_lowercase()),
        None => format!(
            "from:{}\ndate:{}\nsubject:{}\ntext:{}",
            m.from
                .iter()
                .map(|a| a.normalized.as_str())
                .collect::<Vec<_>>()
                .join(","),
            m.date.as_deref().unwrap_or(""),
            m.subject.as_deref().unwrap_or("").trim(),
            datastore::sha256_hex(m.text.as_deref().unwrap_or("").as_bytes()),
        ),
    };
    datastore::sha256_hex(basis.as_bytes())
}

/// The trusted sender checks of `m` (`datastore::auth_results` over its headers).
pub fn auth_of(m: &Message) -> Option<Auth> {
    let values: Vec<String> = m
        .headers
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case("authentication-results"))
        .map(|h| {
            h.decoded
                .clone()
                .unwrap_or_else(|| h.value.clone())
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    datastore::auth_results(&values)
}

/// The key of a message's plain text in the store.
pub fn text_key(item_id: &str) -> String {
    format!("derived/{item_id}/text.txt")
}

fn list(v: impl serde::Serialize) -> Value {
    json!(serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()))
}

/// One `items` row before it is written.
struct Item<'a> {
    id: &'a str,
    size: u64,
    media_type: &'a str,
    filename: Option<&'a str>,
    parent: Option<&'a str>,
    status: &'a str,
    error: Option<String>,
}

fn item_row(i: Item<'_>, at: &str) -> Statement {
    Statement {
        sql: "INSERT OR IGNORE INTO items (item_id, sha256, blake3, size, media_type, kind, \
              filename, text_sha256, parent_id, first_seen, status, error, version) \
              VALUES (?, ?, NULL, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 1)"
            .into(),
        params: vec![
            json!(i.id),
            json!(i.id),
            json!(i.size),
            json!(i.media_type),
            json!(kind_of(i.media_type)),
            json!(i.filename),
            json!(i.parent),
            json!(at),
            json!(i.status),
            json!(i.error),
        ],
    }
}

/// The `emails` row of `m`, item `id`.
pub fn email_row(id: &str, m: &Message, text_ref: Option<&str>) -> Statement {
    Statement {
        sql: "INSERT OR IGNORE INTO emails (item_id, schema, fingerprint, message_id, \
              in_reply_to, \"references\", subject, date, \"from\", \"to\", cc, bcc, reply_to, \
              sender, delivered_to, headers, signals, text_ref, html_ref) \
              VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            .into(),
        params: vec![
            json!(id),
            json!(m.schema),
            json!(fingerprint(m)),
            json!(m.message_id),
            list(&m.in_reply_to),
            list(&m.references),
            json!(m.subject),
            json!(m.date),
            list(&m.from),
            list(&m.to),
            list(&m.cc),
            list(&m.bcc),
            list(&m.reply_to),
            list(m.sender.iter().collect::<Vec<_>>()),
            list(&m.delivered_to),
            list(&m.headers),
            list(&m.taxonomy.signals),
            json!(text_ref),
            json!(m.html.as_ref().map(|h| h.sha256.clone())),
        ],
    }
}

/// The `sightings` row of a message received at `to` in `mailbox`.
pub fn sighting_row(id: &str, to: &str, at: &str, mailbox: &str, auth: Option<&Auth>) -> Statement {
    let extra = json!({ "mailbox": mailbox, "auth": auth });
    Statement {
        sql: "INSERT OR IGNORE INTO sightings (item_id, source, account, locator, seen_at, extra) \
              VALUES (?, ?, NULL, ?, ?, ?)"
            .into(),
        params: vec![
            json!(id),
            json!(SOURCE_EMAIL_ROUTING),
            json!(to.trim().to_ascii_lowercase()),
            json!(at),
            json!(extra.to_string()),
        ],
    }
}

/// The longest reason [`forward_error`] keeps, in characters.
pub const FORWARD_ERROR_MAX: usize = 160;

/// The reason recorded for a forward Email Routing refused: Cloudflare's own error `message`
/// (such as "destination address not verified"), on one line and cut to
/// [`FORWARD_ERROR_MAX`] characters, with the destination `to` and any other word holding an
/// address replaced by `[address]`. It never holds the message's content: the error is
/// Cloudflare's, raised before anything is sent.
pub fn forward_error(message: &str, to: &str) -> String {
    const PREFIX: &str = "Email Routing refused the forward";
    let to = to.trim().to_ascii_lowercase();
    let words: Vec<String> = message
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let lower = w.to_ascii_lowercase();
            if w.contains('@') || (!to.is_empty() && lower.contains(&to)) {
                "[address]".to_string()
            } else {
                w.to_string()
            }
        })
        .collect();
    let reason = words.join(" ");
    let reason = reason.trim_start_matches("Error:").trim();
    let full = if reason.is_empty() {
        PREFIX.to_string()
    } else {
        format!("{PREFIX}: {reason}")
    };
    match full.char_indices().nth(FORWARD_ERROR_MAX) {
        Some((cut, _)) => format!("{}…", &full[..cut]),
        None => full,
    }
}

/// The `email_deliveries` row of what happened to a stored message: `outcome` is `stored`
/// (nothing to forward), `forwarded`, `forward_failed` or `loop`.
pub fn delivery_row(
    s: &Stored,
    to: &str,
    at: &str,
    outcome: &str,
    error: Option<&str>,
) -> Statement {
    Statement {
        sql: "INSERT OR IGNORE INTO email_deliveries (item_id, locator, at, mailbox, forward_to, \
              outcome, error) VALUES (?, ?, ?, ?, ?, ?, ?)"
            .into(),
        params: vec![
            json!(s.item_id),
            json!(to.trim().to_ascii_lowercase()),
            json!(at),
            json!(s.mailbox),
            json!(s.forward),
            json!(outcome),
            json!(error),
        ],
    }
}

/// The one log line of a message: its id, size, mailbox and outcome, never its content or
/// addresses.
pub fn log_line(id: &str, size: u64, mailbox: &str, outcome: &str) -> String {
    format!("email {id} size={size} mailbox={mailbox} outcome={outcome}")
}

/// Stores original bytes, repairs any interrupted extraction, and returns rows to commit with
/// the source sighting in one transaction.
async fn store_message<B: Bindings>(
    b: &B,
    raw: &[u8],
    content: &ContentRef,
    source: Source,
    at: &str,
) -> Result<(bool, Option<Auth>, Vec<Statement>), &'static str> {
    let id = content.sha256.as_str();
    let key = content.key();
    let duplicate = match b.has(&key).await {
        Ok(d) => d,
        Err(_) => return Err(NOT_STORED),
    };
    let parsed = pigeon_email::parse(raw, source);
    let auth = parsed.as_ref().ok().and_then(|p| auth_of(&p.message));
    let mut rows = Vec::new();
    if !duplicate && b.put(&key, raw, id).await.is_err() {
        return Err(NOT_STORED);
    }
    match &parsed {
        Ok(p) => {
            for (content, bytes) in &p.blobs {
                let k = content.key();
                let held = b.has(&k).await.map_err(|_| NOT_STORED)?;
                if !held && b.put(&k, bytes, &content.sha256).await.is_err() {
                    return Err(NOT_STORED);
                }
            }
            let text_ref = match &p.message.text {
                Some(t) => {
                    let k = text_key(id);
                    let sha = datastore::sha256_hex(t.as_bytes());
                    if !b.has(&k).await.map_err(|_| NOT_STORED)?
                        && b.put(&k, t.as_bytes(), &sha).await.is_err()
                    {
                        return Err(NOT_STORED);
                    }
                    Some(k)
                }
                None => None,
            };
            rows.push(item_row(
                Item {
                    id,
                    size: content.size,
                    media_type: "message/rfc822",
                    filename: None,
                    parent: None,
                    status: "stored",
                    error: None,
                },
                at,
            ));
            rows.push(email_row(id, &p.message, text_ref.as_deref()));
            for (index, a) in p.message.attachments.iter().enumerate() {
                rows.push(item_row(
                    Item {
                        id: &a.content.sha256,
                        size: a.content.size,
                        media_type: &a.content_type,
                        filename: a.filename.as_deref(),
                        parent: Some(id),
                        status: "stored",
                        error: None,
                    },
                    at,
                ));
                rows.push(Statement {
                    sql: "INSERT OR IGNORE INTO sightings (item_id, source, account, locator, seen_at, extra) \
                          VALUES (?, 'email_attachment', NULL, ?, ?, ?)".into(),
                    params: vec![
                        json!(a.content.sha256),
                        json!(format!("{id}:{index}")),
                        json!(at),
                        json!(json!({ "parent": id, "index": index, "filename": a.filename }).to_string()),
                    ],
                });
            }
        }
        // A parser bug never bounces mail: the bytes are kept and can be parsed again.
        Err(e) => rows.push(item_row(
            Item {
                id,
                size: content.size,
                media_type: "message/rfc822",
                filename: None,
                parent: None,
                status: "failed",
                error: Some(e.to_string()),
            },
            at,
        )),
    }
    Ok((duplicate, auth, rows))
}

/// Commits one historical message and all its attachments without invoking delivery or forwarding.
pub async fn import<B: Bindings>(
    b: &B,
    raw: &[u8],
    source: Source,
    at: &str,
) -> Result<(String, bool), &'static str> {
    let Source::Imap {
        server,
        account,
        mailbox,
        uid_validity,
        uid,
    } = &source
    else {
        return Err("unsupported historical source");
    };
    if raw.is_empty() || raw.len() as u64 > datastore::MAX_MESSAGE {
        return Err(TOO_LARGE);
    }
    let fields = [server.as_str(), account.as_str(), mailbox.as_str()];
    if fields
        .iter()
        .any(|s| s.is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
        || *uid_validity == 0
        || *uid == 0
    {
        return Err("invalid IMAP source");
    }
    let mut hasher = Hasher::default();
    hasher.update(raw);
    let content = hasher.finish();
    let locator = json!([server, account, mailbox, uid_validity, uid]).to_string();
    let account = account.clone();
    let extra = serde_json::to_string(&source).map_err(|_| NOT_STORED)?;
    let (duplicate, _, mut rows) = store_message(b, raw, &content, source, at).await?;
    rows.push(Statement {
        sql: "INSERT OR IGNORE INTO sightings (item_id, source, account, locator, seen_at, extra) \
              VALUES (?, ?, ?, ?, ?, ?)"
            .into(),
        params: vec![
            json!(content.sha256),
            json!("imap"),
            json!(account),
            json!(locator),
            json!(at),
            json!(extra),
        ],
    });
    b.run(rows).await.map_err(|_| NOT_STORED)?;
    Ok((content.sha256, duplicate))
}

/// Receives one message: configuration, mailbox, bytes, rows, forward decision. Anything that
/// keeps it from being stored rejects it; the forward itself is the caller's.
pub async fn receive<B: Bindings>(b: &B, m: &Incoming<'_>) -> Verdict {
    let config = match b.config().await {
        Ok(Some(c)) => c,
        Ok(None) => return Verdict::Reject(NOT_SET_UP.into()),
        Err(_) => return Verdict::Reject(NOT_STORED.into()),
    };
    if m.content.size > datastore::MAX_MESSAGE {
        return Verdict::Reject(TOO_LARGE.into());
    }
    let mailbox = config.mailbox_for(m.envelope_to);
    let id = m.content.sha256.clone();
    let at = m.received_at.as_str();
    let source = Source::EmailRouting {
        envelope_from: m.envelope_from.to_string(),
        envelope_to: m.envelope_to.to_string(),
    };
    let (duplicate, auth, mut rows) = match store_message(b, m.raw, &m.content, source, at).await {
        Ok(stored) => stored,
        Err(reason) => return Verdict::Reject(reason.into()),
    };
    rows.push(sighting_row(
        &id,
        m.envelope_to,
        at,
        &mailbox,
        auth.as_ref(),
    ));
    if b.run(rows).await.is_err() {
        return Verdict::Reject(NOT_STORED.into());
    }
    let looped = config.looped(&m.loop_marks);
    let forward = if looped {
        None
    } else {
        config.forward_for(&mailbox)
    };
    debug_assert!(mailbox == CATCH_ALL || config.addresses.iter().any(|a| a.local == mailbox));
    Verdict::Stored(Stored {
        instance: config.instance.clone(),
        item_id: id,
        size: m.content.size,
        mailbox,
        duplicate,
        forward,
        looped,
    })
}

//! The lite tier of an instance's unified data store (docs/plans/email.md): the names of its
//! resources, its D1 migrations (the record rows `items`, `sightings` and `emails` with exactly
//! docs/plans/ingest.md decision 3's columns), the email handler's configuration and mailbox
//! rule, and the signed read ops core sends to the data Worker with their SQL.
//!
//! Pure: no Worker runtime, no network. Core (the plan, the `email_*` tools), the data Worker
//! and the smoke suite all use it, so each side reads one definition.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::index::Statement;

/// The tier the lite store is: `[capabilities.data] tier = "lite"`.
pub const TIER_LITE: &str = "lite";
/// The tiers a record may name today.
pub const TIERS: &[&str] = &[TIER_LITE];
/// The path of the signed read ops on the data Worker.
pub const CALL_PATH: &str = "/data/call";
/// The mailbox of every address the record does not list.
pub const CATCH_ALL: &str = "catch-all";
/// The header every forward carries, naming the instance, so a message that comes back is never
/// forwarded again.
pub const LOOP_HEADER: &str = "X-Pigeon-Data";
/// The `sightings.source` of a message received by Email Routing.
pub const SOURCE_EMAIL_ROUTING: &str = "email_routing";
/// The provenance type of each uploaded historical IMAP message.
pub const SOURCE_IMAP: &str = "imap";
/// The largest message Email Routing delivers, in bytes (25 MiB).
pub const MAX_MESSAGE: u64 = 25 * 1024 * 1024;
/// The prefixes of the bucket the read ops may list or read.
pub const READABLE_PREFIXES: &[&str] = &["blobs/", "derived/"];
/// A page of `list` by default, and at most.
pub const LIST_DEFAULT: u32 = 50;
pub const LIST_MAX: u32 = 200;
/// The largest object the `object` op answers inline, in bytes.
pub const OBJECT_INLINE_MAX: u64 = 1024 * 1024;
/// The D1 key of the email handler's configuration (`config` table).
pub const EMAIL_CONFIG_KEY: &str = "email";

/// The name shared by the instance's data bucket, D1 database and data Worker:
/// `{instance}-data`, and `pigeon-data` for the home instance (docs/plans/data-worker.md,
/// decision 1). Any `/` or `.` of the id becomes `-`.
pub fn store_name(instance: &str, home: bool) -> String {
    if home {
        return "pigeon-data".to_string();
    }
    format!("{}-data", instance.replace(['/', '.'], "-"))
}

/// The immutable B2 name for an R2 key and exact byte version. B2 permits new versions of a
/// locked key, so names include both hashes. A retry can only write the same bytes at this name.
pub fn backup_object_key(source: &str, sha: &str) -> String {
    let key_hash = sha256_hex(source.as_bytes());
    format!("backup/v1/{key_hash}/{sha}")
}

fn backup_xml_field<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    Some(xml.split_once(&open)?.1.split_once(&close)?.0.trim())
}

/// A bucket ACL must identify its owner as the only grantee with full control.
pub fn backup_is_private(xml: &str) -> bool {
    let owner = backup_xml_field(xml, "Owner").and_then(|owner| backup_xml_field(owner, "ID"));
    let Some(list) = backup_xml_field(xml, "AccessControlList") else {
        return false;
    };
    let Some(grant) = backup_xml_field(list, "Grant") else {
        return false;
    };
    list.matches("<Grant>").count() == 1
        && owner.is_some_and(|id| !id.is_empty() && backup_xml_field(grant, "ID") == Some(id))
        && backup_xml_field(grant, "Permission") == Some("FULL_CONTROL")
}

/// Restrict the backup writer to one bucket, read/write of files and read-only inspection.
pub fn backup_key_scope(allowed: &Value, bucket: &str) -> bool {
    let Some(buckets) = allowed["buckets"].as_array() else {
        return false;
    };
    let [bound] = buckets.as_slice() else {
        return false;
    };
    const REQUIRED: &[&str] = &[
        "listBuckets",
        "listFiles",
        "readBuckets",
        "readFiles",
        "writeFiles",
        "readBucketRetentions",
        "readFileRetentions",
    ];
    let Some(caps) = allowed["capabilities"].as_array() else {
        return false;
    };
    bound["name"].as_str() == Some(bucket)
        && (allowed["namePrefix"].is_null()
            || allowed["namePrefix"]
                .as_str()
                .is_some_and(|p| "backup/v1/".starts_with(p)))
        && REQUIRED
            .iter()
            .all(|cap| caps.iter().any(|c| c.as_str() == Some(cap)))
        && caps.iter().all(|c| {
            c.as_str().is_some_and(|cap| {
                REQUIRED.contains(&cap)
                    || matches!(cap, "listAllBucketNames" | "readBucketEncryption")
            })
        })
}

/// B2 S3 GetObjectLockConfiguration must say the bucket has enabled nonzero default Governance.
pub fn backup_governance(xml: &str) -> bool {
    backup_xml_field(xml, "ObjectLockEnabled") == Some("Enabled")
        && backup_xml_field(xml, "DefaultRetention").is_some_and(|default| {
            backup_xml_field(default, "Mode") == Some("GOVERNANCE")
                && ["Days", "Years"].into_iter().any(|unit| {
                    backup_xml_field(default, unit)
                        .and_then(|n| n.parse::<u32>().ok())
                        .is_some_and(|period| period > 0)
                })
        })
}

/// The expiration reported by GetObjectRetention, after checking its retention mode.
pub fn backup_retention(xml: &str) -> Option<&str> {
    if matches!(
        backup_xml_field(xml, "Mode"),
        Some("GOVERNANCE" | "COMPLIANCE")
    ) {
        backup_xml_field(xml, "RetainUntilDate")
    } else {
        None
    }
}

/// Number of retries before the backup queue sends a failed message to its dead-letter queue.
pub const BACKUP_MAX_RETRIES: u32 = 5;

/// The backup queue of a store: `{store}-backup`.
pub fn backup_queue(store: &str) -> String {
    format!("{store}-backup")
}

/// The dead-letter queue of the backup queue.
pub fn backup_dlq(store: &str) -> String {
    format!("{store}-backup-dlq")
}

/// One D1 migration of the lite tier, applied once, in order, by the plan's `Migrate` step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// Its id, recorded in `migrations` once applied.
    pub id: &'static str,
    /// Its statements, separated by `;`.
    pub sql: &'static str,
}

/// The table that records which migrations ran. Made before any migration.
pub const MIGRATIONS_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS migrations (id TEXT PRIMARY KEY, at TEXT NOT NULL)";

/// The lite tier's migrations. `0001` is the record: ingest.md decision 3's `items`,
/// `sightings` and `emails`, column for column, lists and structs as JSON text, so a later
/// replay into the Iceberg tables needs no rewrite. `0002` is operational: the handler's
/// configuration and its deliveries (forward outcomes), which are not the record.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        id: "0001_record",
        sql: "\
CREATE TABLE IF NOT EXISTS items (
  item_id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL,
  blake3 TEXT,
  size INTEGER NOT NULL,
  media_type TEXT,
  kind TEXT NOT NULL,
  filename TEXT,
  text_sha256 TEXT,
  parent_id TEXT,
  first_seen TEXT NOT NULL,
  status TEXT NOT NULL,
  error TEXT,
  version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS items_parent ON items (parent_id);
CREATE TABLE IF NOT EXISTS sightings (
  item_id TEXT NOT NULL,
  source TEXT NOT NULL,
  account TEXT,
  locator TEXT NOT NULL,
  seen_at TEXT NOT NULL,
  extra TEXT,
  PRIMARY KEY (item_id, source, locator)
);
CREATE INDEX IF NOT EXISTS sightings_mailbox ON sightings (source, json_extract(extra, '$.mailbox'), seen_at);
CREATE INDEX IF NOT EXISTS sightings_seen ON sightings (source, seen_at);
CREATE TABLE IF NOT EXISTS emails (
  item_id TEXT PRIMARY KEY,
  schema INTEGER NOT NULL,
  fingerprint TEXT,
  message_id TEXT,
  in_reply_to TEXT,
  \"references\" TEXT,
  subject TEXT,
  date TEXT,
  \"from\" TEXT,
  \"to\" TEXT,
  cc TEXT,
  bcc TEXT,
  reply_to TEXT,
  sender TEXT,
  delivered_to TEXT,
  headers TEXT,
  signals TEXT,
  text_ref TEXT,
  html_ref TEXT
);
CREATE INDEX IF NOT EXISTS emails_fingerprint ON emails (fingerprint)",
    },
    Migration {
        id: "0002_operations",
        sql: "\
CREATE TABLE IF NOT EXISTS config (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  digest TEXT NOT NULL,
  at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS email_deliveries (
  item_id TEXT NOT NULL,
  locator TEXT NOT NULL,
  at TEXT NOT NULL,
  mailbox TEXT NOT NULL,
  forward_to TEXT,
  outcome TEXT NOT NULL,
  error TEXT,
  PRIMARY KEY (item_id, locator, at)
);
CREATE INDEX IF NOT EXISTS deliveries_outcome ON email_deliveries (outcome, at)",
    },
    Migration {
        id: "0003_mail_import_lineage",
        sql: "CREATE INDEX IF NOT EXISTS mail_attachment_locator ON sightings(locator) \
              WHERE source = 'email_attachment'",
    },
    Migration {
        id: "0004_backup_progress",
        sql: "CREATE TABLE IF NOT EXISTS backup_progress (id TEXT PRIMARY KEY, body TEXT NOT NULL, lease_owner TEXT, lease_until INTEGER NOT NULL DEFAULT 0)",
    },
];

/// The statements of one migration, then the row that records it.
pub fn migration_statements(m: &Migration, at: &str) -> Vec<Statement> {
    let mut out: Vec<Statement> = m
        .sql
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Statement {
            sql: s.to_string(),
            params: Vec::new(),
        })
        .collect();
    out.push(Statement {
        sql: "INSERT OR IGNORE INTO migrations (id, at) VALUES (?, ?)".into(),
        params: vec![json!(m.id), json!(at)],
    });
    out
}

/// The migrations not in `applied`, in order.
pub fn pending_migrations(applied: &[String]) -> Vec<&'static Migration> {
    MIGRATIONS
        .iter()
        .filter(|m| !applied.iter().any(|a| a == m.id))
        .collect()
}

/// Why the lite migrations must not run on a database: it already holds ingest.md decision
/// 13.4's dedupe index, whose `sightings` table has another shape. `tables` are the database's
/// table names.
pub fn migration_refusal(tables: &[String]) -> Option<String> {
    let index = tables
        .iter()
        .any(|t| t == "blobs" || t == "hints" || t == "applied");
    index.then(|| {
        "this database holds the ingest runner's dedupe index, whose `sightings` table has \
         another shape; the lite record goes in its own database"
            .to_string()
    })
}

// ---- the email handler's configuration ----

/// Which mailboxes forward: the listed addresses only (the default), or every message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardScope {
    #[default]
    Listed,
    All,
}

impl ForwardScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ForwardScope::Listed => "listed",
            ForwardScope::All => "all",
        }
    }
}

/// One listed address: its own mailbox, and where it forwards, if anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    /// The local part, lowercase.
    pub local: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward: Option<String>,
}

/// What the email handler reads from D1 (`config`, key [`EMAIL_CONFIG_KEY`]), written by the
/// plan's `WriteEmailConfig` step from the approved record. Without it the handler rejects
/// every message: the store is off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailConfig {
    /// The instance id, carried in [`LOOP_HEADER`].
    pub instance: String,
    /// The domain mail is received on, lowercase, such as `pigeon.dev`.
    pub domain: String,
    #[serde(default)]
    pub addresses: Vec<Listed>,
    #[serde(default)]
    pub scope: ForwardScope,
    /// The default destination: for a listed address without its own, and for catch-all when
    /// the scope is `all`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_to: Option<String>,
}

impl EmailConfig {
    /// The mailbox of envelope recipient `to`: the listed local part it names on [`Self::domain`]
    /// (lowercased, a `+tag` removed), else [`CATCH_ALL`].
    pub fn mailbox_for(&self, to: &str) -> String {
        let to = to.trim().trim_matches(['<', '>']).to_ascii_lowercase();
        let Some((local, domain)) = to.rsplit_once('@') else {
            return CATCH_ALL.into();
        };
        if domain != self.domain.to_ascii_lowercase() {
            return CATCH_ALL.into();
        }
        let local = local.split('+').next().unwrap_or(local);
        match self.addresses.iter().find(|a| a.local == local) {
            Some(a) => a.local.clone(),
            None => CATCH_ALL.into(),
        }
    }

    /// Where a message in `mailbox` is forwarded, if anywhere: a listed address's own
    /// destination, else the default; catch-all only when the scope is `all`.
    pub fn forward_for(&self, mailbox: &str) -> Option<String> {
        if let Some(a) = self.addresses.iter().find(|a| a.local == mailbox) {
            return a.forward.clone().or_else(|| self.forward_to.clone());
        }
        match self.scope {
            ForwardScope::All => self.forward_to.clone(),
            ForwardScope::Listed => None,
        }
    }

    /// Every destination the configuration forwards to, sorted, once each.
    pub fn destinations(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .addresses
            .iter()
            .filter_map(|a| a.forward.clone())
            .chain(self.forward_to.clone())
            .map(|d| d.to_ascii_lowercase())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Whether a message whose [`LOOP_HEADER`] values are `marks` has already been forwarded by
    /// this instance.
    pub fn looped(&self, marks: &[String]) -> bool {
        marks
            .iter()
            .flat_map(|m| m.split(','))
            .any(|m| m.trim() == self.instance)
    }

    /// The digest the plan compares with the row in D1: SHA-256 of the canonical JSON.
    pub fn digest(&self) -> String {
        sha256_hex(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }
}

/// The lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The statement that writes `config` as the handler's configuration.
pub fn config_statement(config: &EmailConfig, at: &str) -> Statement {
    Statement {
        sql: "INSERT INTO config (key, value, digest, at) VALUES (?, ?, ?, ?) \
              ON CONFLICT (key) DO UPDATE SET value = excluded.value, digest = excluded.digest, \
              at = excluded.at"
            .into(),
        params: vec![
            json!(EMAIL_CONFIG_KEY),
            json!(serde_json::to_string(config).unwrap_or_default()),
            json!(config.digest()),
            json!(at),
        ],
    }
}

/// The query that reads the handler's configuration: one row with `value`, or none.
pub fn config_query() -> Statement {
    Statement {
        sql: "SELECT value, digest FROM config WHERE key = ?".into(),
        params: vec![json!(EMAIL_CONFIG_KEY)],
    }
}

// ---- sender checks ----

/// The SPF, DKIM and DMARC results kept in `sightings.extra.auth`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spf: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dkim: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmarc: Option<String>,
}

/// The authserv-id of the header to trust (docs/plans/email.md, "The flow").
pub const TRUSTED_AUTHSERV: &str = "mx.cloudflare.net";

/// The results of the first `Authentication-Results` value written by
/// [`TRUSTED_AUTHSERV`]; `values` are every such header's value, in order. Values from other
/// servers came with the message and are not trusted. `None` when there is none.
pub fn auth_results(values: &[String]) -> Option<Auth> {
    let trusted = values.iter().find(|v| {
        let v = v.trim_start();
        let id = v.split([';', ' ', '\t', '\r', '\n']).next().unwrap_or("");
        id.eq_ignore_ascii_case(TRUSTED_AUTHSERV)
    })?;
    let mut auth = Auth::default();
    for part in trusted.split(';').skip(1) {
        let part = part.trim();
        let Some((method, rest)) = part.split_once('=') else {
            continue;
        };
        let result = rest
            .split(|c: char| c.is_whitespace() || c == '(')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let slot = match method.trim().to_ascii_lowercase().as_str() {
            "spf" => &mut auth.spf,
            "dkim" => &mut auth.dkim,
            "dmarc" => &mut auth.dmarc,
            _ => continue,
        };
        // The first result of each method counts (a message may carry several DKIM signatures;
        // one pass is enough, so a later pass replaces an earlier other result).
        if slot.is_none() || result == "pass" {
            *slot = Some(result);
        }
    }
    Some(auth)
}

/// The console's sender-check badge: `pass` (DMARC pass), `fail` (DMARC fail), `partial` (SPF
/// or DKIM pass), else `none`.
pub fn sender_check(auth: Option<&Value>) -> &'static str {
    let get = |k: &str| auth.and_then(|a| a[k].as_str()).unwrap_or("");
    match get("dmarc") {
        "pass" => "pass",
        "fail" => "fail",
        _ if get("spf") == "pass" || get("dkim") == "pass" => "partial",
        _ => "none",
    }
}

// ---- the signed read ops ----

/// `list`'s filters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<String>,
    /// RFC 3339; messages seen at or after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// A sender address or domain, matched exactly (lowercased).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The `next` of the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One read op core sends to the data Worker at [`CALL_PATH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DataCall {
    /// Counts and the newest message.
    Status,
    /// Each mailbox with its count and newest time.
    Mailboxes,
    /// A page of messages.
    List(ListQuery),
    /// Imported messages, latest committed sighting first.
    ListImported(ListQuery),
    /// One message: its row, sightings, attachments, and its text and HTML.
    Get { id: String },
    /// A page of object keys under a readable prefix.
    Objects {
        #[serde(default)]
        prefix: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// One object, inline up to [`OBJECT_INLINE_MAX`].
    Object { key: String },
}

/// The signed body: the instance it is for (so one instance's key never answers for
/// another's Worker) and the op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRequest {
    pub instance: String,
    pub call: DataCall,
}

/// Why `call` may not run, before any query: a bad id, key or prefix, a limit out of range.
pub fn call_problem(call: &DataCall) -> Option<String> {
    match call {
        DataCall::Get { id } if !is_sha256(id) => Some("an email id is 64 hex digits".into()),
        DataCall::List(q) | DataCall::ListImported(q)
            if q.limit.is_some_and(|l| l == 0 || l > LIST_MAX) =>
        {
            Some(format!("limit is 1 to {LIST_MAX}"))
        }
        DataCall::List(q)
            if q.cursor
                .as_deref()
                .is_some_and(|c| parse_cursor(c).is_none()) =>
        {
            Some("the cursor is not one this store gave".into())
        }
        DataCall::ListImported(q)
            if q.cursor
                .as_deref()
                .is_some_and(|c| parse_import_cursor(c).is_none()) =>
        {
            Some("the cursor is not one this mail import gave".into())
        }
        DataCall::Objects { prefix, .. } if !prefix.is_empty() && !readable(prefix) => Some(
            format!("only {} may be listed", READABLE_PREFIXES.join(" and ")),
        ),
        DataCall::Object { key } if !readable(key) || key.ends_with('/') => Some(format!(
            "only objects under {} may be read",
            READABLE_PREFIXES.join(" and ")
        )),
        _ => None,
    }
}

/// Whether `key` is under a readable prefix, with no dot segments, backslashes or controls.
pub fn readable(key: &str) -> bool {
    READABLE_PREFIXES.iter().any(|p| key.starts_with(p))
        && !key.contains("//")
        && !key.chars().any(|c| c == '\\' || c.is_control())
        && !key.split('/').any(|s| s == "." || s == "..")
}

/// Whether `s` is 64 lowercase hex digits.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A `list` cursor: the last row's `seen_at` and `item_id`, joined by `|`.
pub fn cursor_of(seen_at: &str, item_id: &str) -> String {
    format!("{seen_at}|{item_id}")
}

fn parse_cursor(c: &str) -> Option<(&str, &str)> {
    let (at, id) = c.split_once('|')?;
    (!at.is_empty() && is_sha256(id)).then_some((at, id))
}

fn parse_import_cursor(c: &str) -> Option<u64> {
    let sequence = c.strip_prefix("imap:")?.parse().ok()?;
    (1..=9_007_199_254_740_991)
        .contains(&sequence)
        .then_some(sequence)
}

/// `status`'s queries, in order: messages, items and the newest sighting; each delivery outcome
/// with its count, newest time and newest recorded reason.
pub fn status_queries() -> Vec<Statement> {
    vec![
        Statement {
            sql: "SELECT COUNT(*) AS messages, COUNT(DISTINCT item_id) AS items, \
                  MAX(seen_at) AS newest FROM sightings WHERE source = ?"
                .into(),
            params: vec![json!(SOURCE_EMAIL_ROUTING)],
        },
        Statement {
            // `last_error`: the newest reason recorded for the outcome (a failed forward's
            // redacted reason from Cloudflare), or null.
            sql: "SELECT d.outcome, COUNT(*) AS n, MAX(d.at) AS last, \
                  (SELECT e.error FROM email_deliveries e WHERE e.outcome = d.outcome \
                  AND e.error IS NOT NULL ORDER BY e.at DESC LIMIT 1) AS last_error \
                  FROM email_deliveries d GROUP BY d.outcome ORDER BY d.outcome"
                .into(),
            params: Vec::new(),
        },
    ]
}

/// `mailboxes`'s query: each mailbox with its count and newest time.
pub fn mailboxes_query() -> Statement {
    Statement {
        sql: "SELECT json_extract(extra, '$.mailbox') AS mailbox, COUNT(*) AS count, \
              MAX(seen_at) AS newest FROM sightings WHERE source = ? \
              GROUP BY json_extract(extra, '$.mailbox') ORDER BY mailbox"
            .into(),
        params: vec![json!(SOURCE_EMAIL_ROUTING)],
    }
}

/// `list`'s query, newest first, one more row than the page so the answer knows there is a
/// next. `query` was checked by [`call_problem`].
pub fn list_query(query: &ListQuery) -> Statement {
    list_query_by_source(query, SOURCE_EMAIL_ROUTING)
}

pub fn imported_list_query(query: &ListQuery) -> Statement {
    list_query_by_source(query, SOURCE_IMAP)
}

fn list_query_by_source(query: &ListQuery, source: &str) -> Statement {
    let mut sql = String::from(
        "SELECT s.item_id, s.locator, s.seen_at, s.extra, s.source, s.account, s.rowid AS seq, \
         e.subject, e.date, e.\"from\", i.size, COALESCE(NULLIF( \
         (SELECT COUNT(*) FROM sightings a WHERE a.source = 'email_attachment' \
          AND a.locator >= s.item_id || ':' AND a.locator < s.item_id || ';'), 0), \
         (SELECT COUNT(*) FROM items a WHERE a.parent_id = s.item_id)) AS attachments \
         FROM sightings s JOIN items i ON i.item_id = s.item_id \
         LEFT JOIN emails e ON e.item_id = s.item_id WHERE s.source = ?",
    );
    let mut params = vec![json!(source)];
    if let Some(m) = &query.mailbox {
        sql.push_str(" AND json_extract(s.extra, '$.mailbox') = ?");
        params.push(json!(m));
    }
    if let Some(since) = &query.since {
        sql.push_str(" AND s.seen_at >= ?");
        params.push(json!(since));
    }
    if let Some(from) = &query.from {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM json_each(e.\"from\") f WHERE \
             json_extract(f.value, '$.normalized') = ? OR json_extract(f.value, '$.domain') = ?)",
        );
        let from = from.trim().to_ascii_lowercase();
        params.push(json!(from));
        params.push(json!(from));
    }
    if source == SOURCE_IMAP {
        if let Some(seq) = query.cursor.as_deref().and_then(parse_import_cursor) {
            sql.push_str(" AND s.rowid < ?");
            params.push(json!(seq));
        }
        sql.push_str(" ORDER BY s.rowid DESC LIMIT ?");
    } else {
        if let Some((at, id)) = query.cursor.as_deref().and_then(parse_cursor) {
            sql.push_str(" AND (s.seen_at < ? OR (s.seen_at = ? AND s.item_id < ?))");
            params.extend([json!(at), json!(at), json!(id)]);
        }
        sql.push_str(" ORDER BY s.seen_at DESC, s.item_id DESC LIMIT ?");
    }
    let limit = query.limit.unwrap_or(LIST_DEFAULT).clamp(1, LIST_MAX);
    params.push(json!(limit + 1));
    Statement { sql, params }
}

/// `list`'s answer from its rows (D1's JSON objects): each message with its sender check and
/// attachment count, and `next` when there is another page.
pub fn list_answer(rows: &[Value], limit: Option<u32>) -> Value {
    let limit = limit.unwrap_or(LIST_DEFAULT).clamp(1, LIST_MAX) as usize;
    let page = &rows[..rows.len().min(limit)];
    let messages: Vec<Value> = page
        .iter()
        .map(|r| {
            let extra = json_text(&r["extra"]);
            let from = json_text(&r["from"]);
            let first = from.get(0);
            let mut message = json!({
                "id": r["item_id"],
                "mailbox": extra["mailbox"],
                "to": r["locator"],
                "seen_at": r["seen_at"],
                "date": r["date"],
                "subject": r["subject"],
                "from": first.map(|f| json!({ "name": f["name"], "address": f["address"] })),
                "size": r["size"],
                "attachments": r["attachments"].as_u64().unwrap_or(0),
                "sender_check": sender_check(extra.get("auth")),
            });
            if r["source"] == SOURCE_IMAP {
                message["source"] = json!(SOURCE_IMAP);
                message["account"] = r["account"].clone();
                message["to"] = Value::Null;
            }
            message
        })
        .collect();
    let next = (rows.len() > limit)
        .then(|| page.last())
        .flatten()
        .map(|r| {
            if r["source"] == SOURCE_IMAP {
                return format!("imap:{}", r["seq"].as_u64().unwrap_or(0));
            }
            cursor_of(
                r["seen_at"].as_str().unwrap_or(""),
                r["item_id"].as_str().unwrap_or(""),
            )
        });
    json!({ "messages": messages, "next": next })
}

/// `get`'s queries, in order: the item, its emails row, its sightings, its attachments.
pub fn get_queries(id: &str) -> Vec<Statement> {
    let one = |sql: &str| Statement {
        sql: sql.into(),
        params: vec![json!(id)],
    };
    vec![
        one("SELECT * FROM items WHERE item_id = ?"),
        one("SELECT * FROM emails WHERE item_id = ?"),
        one("SELECT * FROM sightings WHERE item_id = ? ORDER BY seen_at"),
        Statement {
            sql: "SELECT a.item_id, i.size, i.media_type, i.kind, \
                  json_extract(a.extra, '$.filename') AS filename FROM sightings a \
                  JOIN items i ON i.item_id = a.item_id WHERE a.source = 'email_attachment' \
                  AND a.locator >= ?1 AND a.locator < ?2 UNION ALL \
                  SELECT i.item_id, i.size, i.media_type, i.kind, i.filename FROM items i \
                  WHERE i.parent_id = ?3 AND NOT EXISTS (SELECT 1 FROM sightings a \
                    WHERE a.item_id = i.item_id AND a.source = 'email_attachment' \
                    AND a.locator >= ?1 AND a.locator < ?2) ORDER BY filename"
                .into(),
            params: vec![json!(format!("{id}:")), json!(format!("{id};")), json!(id)],
        },
    ]
}

/// A column holding JSON text, read; `Null` when it is absent or not JSON.
pub fn json_text(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    }
}

/// `get`'s answer from its four row sets and the bodies the Worker read: headers, text, the
/// HTML marked for sandboxed rendering (never to be inlined into a page's own DOM), and the
/// attachment list with each one's key in the store. `None` when there is no such item.
pub fn get_answer(
    item: &[Value],
    email: &[Value],
    sightings: &[Value],
    attachments: &[Value],
    text: Option<String>,
    html: Option<String>,
) -> Option<Value> {
    let item = item.first()?;
    let id = item["item_id"].as_str().unwrap_or_default();
    let e = email.first().cloned().unwrap_or(Value::Null);
    let list = |k: &str| json_text(&e[k]);
    let seen: Vec<Value> = sightings
        .iter()
        .map(|s| {
            let extra = json_text(&s["extra"]);
            let mut sighting = json!({
                "source": s["source"], "to": s["locator"], "seen_at": s["seen_at"],
                "mailbox": extra["mailbox"], "auth": extra["auth"],
                "sender_check": sender_check(extra.get("auth")),
            });
            if s["source"] == SOURCE_IMAP {
                sighting["to"] = Value::Null;
                sighting["provenance"] = extra;
            }
            sighting
        })
        .collect();
    let files: Vec<Value> = attachments
        .iter()
        .map(|a| {
            let sha = a["item_id"].as_str().unwrap_or_default();
            json!({
                "id": sha, "filename": a["filename"], "media_type": a["media_type"],
                "size": a["size"], "key": crate::blob_key(sha),
            })
        })
        .collect();
    Some(json!({
        "id": id,
        "key": crate::blob_key(id),
        "size": item["size"],
        "status": item["status"],
        "error": item["error"],
        "first_seen": item["first_seen"],
        "message_id": e["message_id"],
        "subject": e["subject"],
        "date": e["date"],
        "from": list("from"),
        "to": list("to"),
        "cc": list("cc"),
        "reply_to": list("reply_to"),
        "headers": list("headers"),
        "signals": list("signals"),
        "sightings": seen,
        "text": text,
        "html": html.map(|h| json!({
            "render": "sandboxed",
            "remote_images": "blocked",
            "body": h,
        })),
        "attachments": files,
    }))
}

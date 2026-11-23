//! The dedupe index in D1 (decision 13.4): its schema, and the statements that apply one
//! manifest part, idempotently.

use serde::Serialize;
use serde_json::{Value, json};

use crate::blob_key;
use crate::decide::Verdict;
use crate::manifest::{JobSpec, ManifestLine, SourceSpec, Status};

/// The D1 schema, run once when the database is made. Statements are separated by `;`.
pub const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS blobs (
  sha256 TEXT PRIMARY KEY,
  blake3 TEXT NOT NULL,
  size INTEGER NOT NULL,
  mime TEXT NOT NULL,
  kind TEXT NOT NULL,
  r2_key TEXT NOT NULL,
  simhash TEXT,
  first_run TEXT NOT NULL,
  first_seen_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sightings (
  id TEXT PRIMARY KEY,
  sha256 TEXT NOT NULL,
  account TEXT NOT NULL,
  bucket TEXT NOT NULL,
  key TEXT NOT NULL,
  parent_sha256 TEXT,
  member_path TEXT,
  member_index INTEGER,
  depth INTEGER NOT NULL,
  norm_name TEXT NOT NULL,
  etag TEXT,
  size INTEGER NOT NULL,
  modified TEXT,
  decision TEXT NOT NULL,
  reason TEXT NOT NULL,
  flags TEXT NOT NULL,
  run TEXT NOT NULL,
  seen_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sightings_sha256 ON sightings (sha256);
CREATE INDEX IF NOT EXISTS sightings_name_size ON sightings (norm_name, size);
CREATE TABLE IF NOT EXISTS hints (
  a TEXT NOT NULL,
  b TEXT NOT NULL,
  kind TEXT NOT NULL,
  score REAL,
  run TEXT NOT NULL,
  PRIMARY KEY (a, b, kind)
);
CREATE TABLE IF NOT EXISTS runs (
  run TEXT PRIMARY KEY,
  account TEXT NOT NULL,
  bucket TEXT NOT NULL,
  prefix TEXT NOT NULL,
  host TEXT NOT NULL,
  dry_run INTEGER NOT NULL,
  state TEXT NOT NULL,
  started_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  counters TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS applied (
  part TEXT PRIMARY KEY,
  lines INTEGER NOT NULL,
  at_ms INTEGER NOT NULL
);
";

/// D1 binds at most this many parameters per statement.
pub const MAX_PARAMS: usize = 100;

/// One SQL statement and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// The SQL text, with `?` placeholders.
    pub sql: String,
    /// The values bound to the placeholders, in order.
    pub params: Vec<Value>,
}

/// The statements of [`SCHEMA`], one by one.
pub fn schema_statements() -> Vec<Statement> {
    SCHEMA
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Statement {
            sql: s.to_string(),
            params: Vec::new(),
        })
        .collect()
}

const BLOB_COLS: &str =
    "sha256, blake3, size, mime, kind, r2_key, simhash, first_run, first_seen_ms";
const SIGHTING_COLS: &str = "id, sha256, account, bucket, key, parent_sha256, member_path, \
member_index, depth, norm_name, etag, size, modified, decision, reason, flags, run, seen_ms";
const HINT_COLS: &str = "a, b, kind, score, run";

/// A value's snake-case name, as serde writes it.
fn name<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(Value::String(s)) => s,
        _ => String::new(),
    }
}

/// The run id in a manifest part key `ingest/runs/{run}/manifest/…`; empty if it has none.
fn run_of(part: &str) -> &str {
    part.strip_prefix("ingest/runs/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("")
}

/// An integer as a JSON number, saturating at `i64::MAX` (sizes never get there).
fn int(n: u64) -> Value {
    json!(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Multi-row `INSERT … ON CONFLICT DO NOTHING` statements for `rows` of `width` values each,
/// as many rows per statement as fit under [`MAX_PARAMS`].
fn batches(table: &str, cols: &str, width: usize, rows: Vec<Vec<Value>>) -> Vec<Statement> {
    let per = (MAX_PARAMS / width).max(1);
    let one = format!("({})", vec!["?"; width].join(", "));
    rows.chunks(per)
        .map(|chunk| Statement {
            sql: format!(
                "INSERT INTO {table} ({cols}) VALUES {} ON CONFLICT DO NOTHING",
                vec![one.as_str(); chunk.len()].join(", ")
            ),
            params: chunk.iter().flatten().cloned().collect(),
        })
        .collect()
}

/// The statements that apply manifest part `part` (its R2 key) with `lines` at `now_ms`:
/// first a check-and-mark in `applied` (`INSERT … ON CONFLICT DO NOTHING`), then the
/// `blobs`, `sightings` and `hints` rows, batched under [`MAX_PARAMS`] parameters each. Applying
/// a part twice changes nothing.
pub fn apply_part(part: &str, lines: &[ManifestLine], now_ms: i64) -> Vec<Statement> {
    let run = run_of(part);
    let mut out = vec![Statement {
        sql: "INSERT INTO applied (part, lines, at_ms) VALUES (?, ?, ?) ON CONFLICT DO NOTHING"
            .to_string(),
        params: vec![json!(part), json!(lines.len()), json!(now_ms)],
    }];

    let mut blobs = Vec::new();
    let mut sightings = Vec::new();
    let mut hints = Vec::new();
    for l in lines {
        let stored = matches!(l.verdict, Verdict::Upload | Verdict::Duplicate);
        if stored && !l.sha256.is_empty() {
            let key = l.r2_key.clone().unwrap_or_else(|| blob_key(&l.sha256));
            blobs.push(vec![
                json!(l.sha256),
                json!(l.blake3),
                int(l.size),
                json!(l.mime),
                json!(name(&l.kind)),
                json!(key),
                json!(l.simhash),
                json!(run),
                json!(l.at_ms),
            ]);
        }
        let flags: Vec<String> = l.flags.iter().map(name).collect();
        let m = l.member.as_ref();
        sightings.push(vec![
            json!(l.id),
            json!(l.sha256),
            json!(l.origin.account),
            json!(l.origin.bucket),
            json!(l.origin.key),
            json!(m.map(|m| m.parent_sha256.as_str())),
            json!(m.map(|m| m.path.as_str())),
            json!(m.map(|m| m.index)),
            json!(m.map_or(0, |m| m.depth)),
            json!(l.norm_name),
            json!(l.origin.etag),
            int(l.size),
            json!(l.origin.modified),
            json!(name(&l.verdict)),
            json!(name(&l.reason)),
            json!(flags.join(",")),
            json!(run),
            json!(l.at_ms),
        ]);
        if !l.sha256.is_empty() {
            for h in &l.hints {
                hints.push(vec![
                    json!(l.sha256),
                    json!(h.other),
                    json!(h.kind),
                    json!(h.score),
                    json!(run),
                ]);
            }
        }
    }
    out.extend(batches("blobs", BLOB_COLS, 9, blobs));
    out.extend(batches("sightings", SIGHTING_COLS, 18, sightings));
    out.extend(batches("hints", HINT_COLS, 5, hints));
    out
}

/// The statement that records a run from its job and its `status.json`: inserts the row, or
/// updates its state, time and counters. `host` is where it runs, such as `cloudflare`.
pub fn apply_run(job: &JobSpec, status: &Status, host: &str) -> Statement {
    let (account, bucket, prefix) = match &job.source {
        SourceSpec::S3 {
            account,
            bucket,
            prefix,
            ..
        } => (account.as_str(), bucket.as_str(), prefix.as_str()),
        SourceSpec::Local { account, root } => (account.as_str(), "", root.as_str()),
    };
    Statement {
        sql: "INSERT INTO runs (run, account, bucket, prefix, host, dry_run, state, started_ms, \
updated_ms, counters) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (run) DO UPDATE SET \
state = excluded.state, updated_ms = excluded.updated_ms, counters = excluded.counters"
            .to_string(),
        params: vec![
            json!(status.run),
            json!(account),
            json!(bucket),
            json!(prefix),
            json!(host),
            json!(i64::from(status.dry_run)),
            json!(name(&status.state)),
            json!(status.started_ms),
            json!(status.updated_ms),
            json!(serde_json::to_string(&status.counters).unwrap_or_default()),
        ],
    }
}

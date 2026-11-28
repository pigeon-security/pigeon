//! The dedupe index in D1 (decision 13.4): its schema, the statements that apply one manifest
//! part idempotently, the repair of an index the old code made, and the lookup `head-many` reads
//! (docs/plans/ingest-index-dry-run.md).
//!
//! An `index_blobs` row is proof that R2 holds the blob: only a live run's `upload` and
//! `duplicate` lines make one. A dry run's part indexes nothing but its `index_applied` mark.

use serde::Serialize;
use serde_json::{Value, json};

use crate::blob_key;
use crate::decide::Verdict;
use crate::manifest::{JobSpec, ManifestLine, SourceSpec, Status};

/// The D1 schema: the lite store's migration `0007_ingest_index` (`datastore::MIGRATIONS`).
/// The tables are prefixed `index_` so they live beside the lite record's own `sightings`
/// (email.md, "D1 table names"). Statements are separated by `;`.
pub const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS index_blobs (
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
CREATE TABLE IF NOT EXISTS index_sightings (
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
CREATE INDEX IF NOT EXISTS index_sightings_sha256 ON index_sightings (sha256);
CREATE INDEX IF NOT EXISTS index_sightings_name_size ON index_sightings (norm_name, size);
CREATE TABLE IF NOT EXISTS index_hints (
  a TEXT NOT NULL,
  b TEXT NOT NULL,
  kind TEXT NOT NULL,
  score REAL,
  run TEXT NOT NULL,
  PRIMARY KEY (a, b, kind)
);
CREATE TABLE IF NOT EXISTS index_runs (
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
CREATE TABLE IF NOT EXISTS index_applied (
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

/// Whether a line of a **live** run proves R2 holds its blob: `upload` (the PUT succeeded) or
/// `duplicate` (HEAD or a create-only `Exists` saw it, or the run's first sighting stored it: a
/// failed upload is forgotten, so the next sighting becomes canonical). Never true of a dry run's
/// lines, whose first sightings stored nothing.
pub fn proves_stored(l: &ManifestLine) -> bool {
    matches!(l.verdict, Verdict::Upload | Verdict::Duplicate) && !l.sha256.is_empty()
}

/// The statements that apply manifest part `part` (its R2 key) with `lines` at `now_ms`:
/// first a check-and-mark in `index_applied` (`INSERT … ON CONFLICT DO NOTHING`), then, for a
/// live run only, the `index_blobs` rows of [`proves_stored`] lines and the `index_sightings` and
/// `index_hints` rows of every line (duplicates included, each pointing at the canonical blob by
/// SHA-256), batched under [`MAX_PARAMS`] parameters each. `dry_run` comes from the run's signed
/// grant or its core-written `job.json`, never from the lines; a dry run's part indexes nothing
/// but its mark (its manifest and report stay in R2). Applying a part twice changes nothing.
pub fn apply_part(
    part: &str,
    lines: &[ManifestLine],
    dry_run: bool,
    now_ms: i64,
) -> Vec<Statement> {
    let run = run_of(part);
    let mut out = vec![Statement {
        sql: "INSERT INTO index_applied (part, lines, at_ms) VALUES (?, ?, ?) \
ON CONFLICT DO NOTHING"
            .to_string(),
        params: vec![json!(part), json!(lines.len()), json!(now_ms)],
    }];
    if dry_run {
        return out;
    }

    let mut blobs = Vec::new();
    let mut sightings = Vec::new();
    let mut hints = Vec::new();
    for l in lines {
        if proves_stored(l) {
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
    out.extend(batches("index_blobs", BLOB_COLS, 9, blobs));
    out.extend(batches("index_sightings", SIGHTING_COLS, 18, sightings));
    out.extend(batches("index_hints", HINT_COLS, 5, hints));
    out
}

/// The names of an index's tables: the lite store's prefixed ones ([`SCHEMA`]), or a standalone
/// index made from the old unprefixed schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tables {
    pub blobs: &'static str,
    pub sightings: &'static str,
    pub hints: &'static str,
}

impl Tables {
    /// The lite store's tables, [`SCHEMA`].
    pub const LITE: Tables = Tables {
        blobs: "index_blobs",
        sightings: "index_sightings",
        hints: "index_hints",
    };
    /// A standalone index from before the prefix (ingest.md 13.4 as first written).
    pub const LEGACY: Tables = Tables {
        blobs: "blobs",
        sightings: "sightings",
        hints: "hints",
    };
}

/// The SQL query "some sighting `s` proves R2 held the bytes of blob row `b`": an `upload`, or
/// a `duplicate` that HEAD or a create-only `Exists` confirmed (`in_r2`, in any run). A
/// `duplicate` seen in run alone proves nothing: in a dry run its first sighting stored nothing.
fn proof(t: &Tables, b: &str) -> String {
    format!(
        "SELECT 1 FROM {sightings} s WHERE s.sha256 = {b}.sha256 AND (s.decision = 'upload' \
OR (s.decision = 'duplicate' AND s.reason = 'in_r2'))",
        sightings = t.sightings
    )
}

/// The repair's candidates: `sha256` and `r2_key` of every blob row no sighting proves. The
/// caller HEADs each key in R2 and passes those R2 holds no object for to [`cleanup`].
pub fn cleanup_candidates(t: &Tables) -> Statement {
    Statement {
        sql: format!(
            "SELECT b.sha256, b.r2_key FROM {blobs} b WHERE NOT EXISTS ({p})",
            blobs = t.blobs,
            p = proof(t, "b")
        ),
        params: Vec::new(),
    }
}

/// The repair of an index the old code made (docs/plans/ingest-index-dry-run.md), idempotent:
///
/// 1. the sightings and hints of each run in `dry_runs` (runs whose job is `dry_run`) go;
/// 2. a blob whose `first_run` has no proving sighting of it, while another run has one, is
///    re-marked with the earliest proving sighting's run and time;
/// 3. a blob row in `absent` (candidates R2 answered no object for) that still no sighting
///    proves is deleted. Nothing else is: a row whose blob R2 holds is never deleted.
pub fn cleanup(t: &Tables, dry_runs: &[String], absent: &[String]) -> Vec<Statement> {
    let mut out = Vec::new();
    for run in dry_runs {
        for table in [t.sightings, t.hints] {
            out.push(Statement {
                sql: format!("DELETE FROM {table} WHERE run = ?"),
                params: vec![json!(run)],
            });
        }
    }
    let earliest = |what: &str| {
        format!(
            "SELECT s.{what} FROM {sightings} s WHERE s.sha256 = {blobs}.sha256 AND \
(s.decision = 'upload' OR (s.decision = 'duplicate' AND s.reason = 'in_r2')) \
ORDER BY s.seen_ms, s.run LIMIT 1",
            sightings = t.sightings,
            blobs = t.blobs
        )
    };
    out.push(Statement {
        sql: format!(
            "UPDATE {blobs} SET first_run = ({run}), first_seen_ms = ({seen}) \
WHERE NOT EXISTS ({p} AND s.run = {blobs}.first_run) AND EXISTS ({p})",
            blobs = t.blobs,
            run = earliest("run"),
            seen = earliest("seen_ms"),
            p = proof(t, t.blobs),
        ),
        params: Vec::new(),
    });
    for chunk in absent.chunks(MAX_PARAMS) {
        out.push(Statement {
            sql: format!(
                "DELETE FROM {blobs} WHERE sha256 IN ({marks}) AND NOT EXISTS ({p})",
                blobs = t.blobs,
                marks = vec!["?"; chunk.len()].join(", "),
                p = proof(t, t.blobs)
            ),
            params: chunk.iter().map(|sha| json!(sha)).collect(),
        });
    }
    out
}

/// What `head-many` answers from first: `sha256, size, blake3` of the `index_blobs` rows among
/// `shas`, as many per statement as fit under [`MAX_PARAMS`].
pub fn known_blobs(shas: &[String]) -> Vec<Statement> {
    shas.chunks(MAX_PARAMS)
        .map(|chunk| Statement {
            sql: format!(
                "SELECT sha256, size, blake3 FROM index_blobs WHERE sha256 IN ({})",
                vec!["?"; chunk.len()].join(", ")
            ),
            params: chunk.iter().map(|sha| json!(sha)).collect(),
        })
        .collect()
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
        sql: "INSERT INTO index_runs (run, account, bucket, prefix, host, dry_run, state, started_ms, \
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

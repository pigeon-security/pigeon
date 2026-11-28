//! The ingest runner's shared library: decide, for each file, whether R2 already holds it with
//! certainty, and record what was seen.
//!
//! This crate is pure: it has no Worker runtime and opens no network connection. It reads only
//! through the [`engine::Source`], [`engine::Dest`] and [`engine::Spool`] traits its callers
//! implement: the runner over HTTPS and a spool directory, the smoke suite in memory. It builds
//! for WASM and natively. The design is decision 13 of docs/plans/ingest.md.

pub mod archive;
pub mod audit;
pub mod backblaze;
pub mod backup_digest;
pub mod backup_inventory;
pub mod container_job;
pub mod datastore;
pub mod decide;
pub mod engine;
pub mod estimate;
pub mod immutable_multipart;
pub mod index;
pub mod managed_destination;
pub mod manifest;
pub mod metrics;
pub mod protocol;

use std::fmt;

/// The R2 key of a blob with this SHA-256 (64 lowercase hex): `blobs/sha256/{2}/{2}/{64}`, the
/// same scheme as `pigeon_email::ContentRef::key`.
pub fn blob_key(sha256: &str) -> String {
    let a = sha256.get(0..2).unwrap_or("00");
    let b = sha256.get(2..4).unwrap_or("00");
    format!("blobs/sha256/{a}/{b}/{sha256}")
}

/// What went wrong in a run. Every error is returned; nothing panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The source refused or failed (listing or reading).
    Source(String),
    /// The destination refused or failed (HEAD, PUT, GET).
    Dest(String),
    /// Local reading, writing or spooling failed.
    Io(String),
    /// An archive could not be read.
    Archive(String),
    /// The job or a setting is not valid.
    Config(String),
    /// A part of this library that is not built yet.
    NotBuilt(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Source(s) => write!(f, "source: {s}"),
            Error::Dest(s) => write!(f, "destination: {s}"),
            Error::Io(s) => write!(f, "local i/o: {s}"),
            Error::Archive(s) => write!(f, "archive: {s}"),
            Error::Config(s) => write!(f, "job: {s}"),
            Error::NotBuilt(s) => write!(f, "not built yet: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

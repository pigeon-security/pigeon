//! The ingest runner: the S3 source, the local-folder source, the R2 destination and the spool
//! directory that drive [`pigeon_ingest::engine::run`] in a container next to the data
//! (docs/plans/ingest.md, decision 13). Native only: on WASM this crate is an empty stub, so the
//! workspace's WASM checks pass.

pub use pigeon_ingest;

#[cfg(not(target_arch = "wasm32"))]
mod app;
#[cfg(not(target_arch = "wasm32"))]
mod client;
#[cfg(not(target_arch = "wasm32"))]
mod local;
#[cfg(not(target_arch = "wasm32"))]
mod r2;
#[cfg(not(target_arch = "wasm32"))]
mod s3_source;
#[cfg(not(target_arch = "wasm32"))]
mod spool;

#[cfg(not(target_arch = "wasm32"))]
pub use app::{SystemClock, run_job};
#[cfg(not(target_arch = "wasm32"))]
pub use local::LocalSource;
#[cfg(not(target_arch = "wasm32"))]
pub use r2::R2Dest;
#[cfg(not(target_arch = "wasm32"))]
pub use s3_source::S3Source;
#[cfg(not(target_arch = "wasm32"))]
pub use spool::DirSpool;

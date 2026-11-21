//! The signed POST the gateway sends to a job's `notify` URL on each state change, and which URLs
//! it may be sent to: the platform's shared scheme (`crate::notify`), which model jobs use too.
//! Each job has its own signing key, handed to the caller once.

pub use crate::notify::{HEADER, MAX_URL, check_url, signature, verify};

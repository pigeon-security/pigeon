//! What the harness can do. Core calls in with what each capability needs; nothing here reads
//! the Worker's environment.

pub mod cloudflare;
pub mod domain;
pub mod secrets_store;
pub mod subdomain;
pub mod tokens;
pub mod zero_trust;

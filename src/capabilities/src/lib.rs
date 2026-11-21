//! What the harness can do. Core hands each capability an [`Account`]; a capability mints the
//! token it needs from it, revokes it after, and calls the other capabilities it builds on.
//! Nothing here reads the Worker's environment.

pub mod artifacts;
pub mod cloudflare;
mod custom_domain;
pub mod design;
pub mod domain;
pub mod secrets_store;
pub mod subdomain;
pub mod tokens;
pub mod zero_trust;

/// The Cloudflare account a capability acts on.
pub struct Account {
    /// The account's ID.
    pub id: String,
    /// The parent token, which mints every other.
    pub parent: String,
    /// The account's Secrets Store ID.
    pub secrets_store: String,
}

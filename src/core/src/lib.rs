//! pigeon's building blocks. Each wraps one Cloudflare API or one protocol and knows nothing of
//! pigeon itself; capabilities build features from them. A block that calls Cloudflare mints
//! its own short-lived token from the [`Account`]'s parent token and revokes it after.

pub mod access;
pub mod ai_gateway;
pub mod artifacts;
pub mod buildkite;
pub mod buildkite_hook;
pub mod cloudflare;
pub mod containers;
pub mod custom_domain;
pub mod design;
pub mod durable;
pub mod git;
pub mod gitobj;
pub mod identity;
pub mod mcp;
pub mod oauth;
pub mod oidc;
pub mod pack;
pub mod queues;
pub mod secrets_store;
pub mod sign;
pub mod tokens;
pub mod webauthn;
pub mod workers;
pub mod zones;

/// The Cloudflare account a block acts on. Cloned only to finish work after an answer has
/// gone; it has no `Debug`, so the parent token can't be printed by accident.
#[derive(Clone)]
pub struct Account {
    /// The account's ID.
    pub id: String,
    /// The parent token, which mints every other.
    pub parent: String,
    /// The account's Secrets Store ID.
    pub secrets_store: String,
}

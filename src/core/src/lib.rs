//! pigeon's building blocks. Each wraps one Cloudflare API or one protocol and knows nothing of
//! pigeon itself; capabilities build features from them. A block that calls Cloudflare mints
//! its own short-lived token from the [`Account`]'s parent token and revokes it after.

pub mod access;
pub mod ai_gateway;
pub mod approval;
pub mod artifacts;
pub mod blog;
pub mod buildkite;
pub mod buildkite_hook;
pub mod buildkite_setup;
pub mod cloudflare;
pub mod cloudflare_oauth;
pub mod containers;
pub mod crypto;
pub mod custom_domain;
pub mod design;
pub mod durable;
pub mod git;
pub mod github_app;
pub mod gitobj;
pub mod identity;
pub mod lanes;
pub mod mcp;
pub mod mcp_portal;
pub mod merge3;
pub mod oauth;
pub mod oidc;
pub mod pack;
pub mod policy;
pub mod queues;
pub mod r2;
pub mod rebase;
pub mod registry;
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
    /// For an account linked to pigeon (not its own): the access token it was granted, used as
    /// it is for every call instead of a token minted from `parent`, which is then empty. Never
    /// printed: `Account` has no `Debug`.
    pub bearer: Option<String>,
}

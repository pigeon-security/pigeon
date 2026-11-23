//! pigeon's building blocks. Each wraps one Cloudflare API or one protocol and knows nothing of
//! pigeon itself; capabilities build features from them. A block that calls Cloudflare mints
//! its own short-lived token from the [`Account`]'s parent token and revokes it after.

pub mod access;
pub mod ai_gateway;
pub mod apns;
pub mod apple;
pub mod approval;
pub mod artifacts;
pub mod attribution;
pub mod buildkite;
pub mod buildkite_hook;
pub mod buildkite_setup;
pub mod changelog;
pub mod cloudflare;
pub mod cloudflare_oauth;
pub mod containers;
pub mod crypto;
pub mod custom_domain;
pub mod design;
pub mod durable;
pub mod expiry;
pub mod git;
pub mod github_app;
pub mod gitobj;
pub mod home;
pub mod identity;
pub mod job_token;
pub mod lanes;
pub mod mcp;
pub mod mcp_portal;
pub mod merge3;
pub mod model_prices;
pub mod notify;
pub mod oauth;
pub mod oidc;
pub mod pack;
pub mod policy;
pub mod policy_file;
pub mod proxy_caller;
pub mod queues;
pub mod r2;
pub mod rebase;
pub mod registry;
pub mod s3;
pub mod secrets_store;
pub mod sign;
pub mod tarball;
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

/// The account `env` acts on, with the parent token read from Secrets Store binding `parent` and
/// the vars `CF_ACCOUNT_ID` and `CF_SECRETS_STORE_ID`: for a Worker that holds the parent token.
pub async fn account_from_env(env: &worker::Env, parent: &str) -> worker::Result<Account> {
    let token = secrets_store::read(&env.secret_store(parent)?)
        .await?
        .ok_or_else(|| {
            worker::Error::RustError(format!("`{parent}` is not set in the Secrets Store"))
        })?;
    Ok(Account {
        id: env.var("CF_ACCOUNT_ID")?.to_string(),
        parent: token,
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
        bearer: None,
    })
}

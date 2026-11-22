//! pigeon's proxies on the proxy host: the git proxy (`/git/`, [`git_proxy`]) and the Cloudflare
//! API proxy (`/cf/`, [`cf_proxy`]), with the proxy credential's checks and the audit line
//! ([`proxy`]). Two Workers serve them: core (`pigeon-platform`), and the proxy Worker
//! (`pigeon-proxy`, docs/plans/workers-split.md, phase 6). The request handling is the same
//! code in both; what differs is the [`Host`] each passes in, which answers who a credential
//! belongs to and hands out the account to mint with. Core's host reads its own Directory,
//! scheduler and Lander and holds the parent token; the proxy Worker's asks core over its
//! `PLATFORM` binding and never holds the parent token.
//!
//! This crate never depends on pigeon-capabilities, so a change to core's capabilities never
//! changes the proxy Worker's binary.

pub mod cache;
pub mod cf_proxy;
pub mod git_proxy;
pub mod proxy;

use std::future::Future;
use std::pin::Pin;

use pigeon_core::Account;

pub use pigeon_core::job_token::Grant;
pub use pigeon_core::proxy_caller::ProxyCaller;

/// An answer from a [`Host`], in flight.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = worker::Result<T>> + 'a>>;

/// What a proxy needs a Cloudflare token for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Git tokens for the source repository in Artifacts: minting them for a request, revoking
    /// them after, and reading a branch's history.
    Git,
    /// The calls `wrangler deploy` makes through `/cf/`, and the lookup of a Worker's container
    /// namespaces.
    Deploy,
}

impl Purpose {
    /// The purpose's name in core's fixed table of scoped credentials
    /// (`split::purpose_permissions`).
    pub fn scoped(self) -> &'static str {
        match self {
            Purpose::Git => "artifacts",
            Purpose::Deploy => "cf_deploy",
        }
    }
}

/// What the proxies need from the Worker serving them.
pub trait Host {
    /// The account the proxies act on, by id: the `{account}` every allowed `/cf/` call names.
    fn account_id(&self) -> String;

    /// The holder of proxy credential `presented` (the whole bearer value, `pgn_px_…`), when it
    /// is live; `None` when it is unknown or has expired.
    fn verify<'a>(&'a self, presented: &'a str) -> Pending<'a, Option<ProxyCaller>>;

    /// The grant of CI job token `presented`, when it is live and its grant is for repository
    /// `repo` (`namespace/name`); `None` otherwise.
    fn job_grant<'a>(&'a self, presented: &'a str, repo: &'a str) -> Pending<'a, Option<Grant>>;

    /// The caller CI job token `presented` stands for at `/cf/`, with the one Worker script it
    /// may deploy, when it is the deploy job of the landing deploying its commit of `repo`;
    /// `None` otherwise.
    fn deploy_grant<'a>(
        &'a self,
        presented: &'a str,
        repo: &'a str,
    ) -> Pending<'a, Option<ProxyCaller>>;

    /// An account to act for `purpose` with: one holding the parent token, from which each
    /// request mints its own token, or one whose `bearer` is a scoped token core minted for
    /// the purpose, used as it is.
    fn account<'a>(&'a self, purpose: Purpose) -> Pending<'a, Account>;
}

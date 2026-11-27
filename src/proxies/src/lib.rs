//! pigeon's proxies on the proxy host: the git proxy (`/git/`, [`git_proxy`]) and the Cloudflare
//! API proxy (`/cf/`, [`cf_proxy`]), with the proxy credential's checks and the audit line
//! ([`proxy`]), and the deploy proxy's remembered Worker lookups ([`lookup`]). Two Workers
//! serve them: core (`pigeon-platform`), and the proxy Worker
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
pub mod lookup;
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

    /// Forgets the scoped `token` [`Host::account`] handed out for `purpose`, because Cloudflare
    /// refused it; the next [`Host::account`] asks for another. A host that keeps none does
    /// nothing.
    fn forget(&self, _purpose: Purpose, _token: &str) {}

    /// The R2 bucket the Worker serving the proxies binds as `binding`: an artifact lane's
    /// bucket (`cf_proxy::ARTIFACT_BINDINGS`), whose objects `/cf/` reads and writes through the
    /// binding, with no Cloudflare token and none of the REST API's rate limit. An error when
    /// the Worker does not bind it.
    fn bucket(&self, binding: &str) -> worker::Result<worker::Bucket>;
}

/// Whether `e` is Cloudflare's API refusing the token it was called with (status 401), as
/// `pigeon_core::cloudflare::call` words it.
pub fn unauthorized(e: &worker::Error) -> bool {
    e.to_string().contains(": 401:")
}

/// Runs `work` with the account `account` answers, and answers that account with the result.
/// When `work` fails with a 401 on an account holding a scoped token (`bearer`), the token was
/// revoked or never became valid: `forget` drops it, and `work` runs once more on the account
/// `account` answers next. Never more than twice; any other error is returned as it is.
///
/// Without it, one isolate of the proxy Worker that kept a token core revoked early (core's
/// token ledger revokes the leases it held when it restarts) refuses every git fetch until the
/// token would have expired, up to fifteen minutes, and every CI job dispatched meanwhile loses
/// its runner before it reports (docs/plans/ci-container-starts.md).
pub async fn with_renewal<T, A, AF, F, W, WF>(
    account: A,
    forget: F,
    work: W,
) -> worker::Result<(Account, T)>
where
    A: FnMut() -> AF,
    AF: Future<Output = worker::Result<Account>>,
    F: FnMut(&str),
    W: FnMut(Account) -> WF,
    WF: Future<Output = worker::Result<T>>,
{
    with_renewal_if(
        account,
        forget,
        work,
        |r: &worker::Result<T>| matches!(r, Err(e) if unauthorized(e)),
    )
    .await
}

/// [`with_renewal`], with `refused` saying which results mean Cloudflare refused the token: for
/// work that answers Cloudflare's 401 as a value (the deploy proxy passes the API's answer on
/// to wrangler) rather than as an error. The second attempt's result is answered as it is,
/// refused or not.
pub async fn with_renewal_if<T, A, AF, F, W, WF, R>(
    mut account: A,
    mut forget: F,
    mut work: W,
    refused: R,
) -> worker::Result<(Account, T)>
where
    A: FnMut() -> AF,
    AF: Future<Output = worker::Result<Account>>,
    F: FnMut(&str),
    W: FnMut(Account) -> WF,
    WF: Future<Output = worker::Result<T>>,
    R: Fn(&worker::Result<T>) -> bool,
{
    let first = account().await?;
    let result = work(first.clone()).await;
    match first.bearer.as_deref() {
        Some(token) if refused(&result) => {
            forget(token);
            let second = account().await?;
            let v = work(second.clone()).await?;
            Ok((second, v))
        }
        _ => result.map(|v| (first, v)),
    }
}

/// [`with_renewal`] with `host`'s account for `purpose`.
pub async fn with_host_account<T, W, WF>(
    host: &dyn Host,
    purpose: Purpose,
    work: W,
) -> worker::Result<(Account, T)>
where
    W: FnMut(Account) -> WF,
    WF: Future<Output = worker::Result<T>>,
{
    with_renewal(
        || host.account(purpose),
        |token| host.forget(purpose, token),
        work,
    )
    .await
}

/// [`with_renewal_if`] with `host`'s account for `purpose`.
pub async fn with_host_account_if<T, W, WF, R>(
    host: &dyn Host,
    purpose: Purpose,
    work: W,
    refused: R,
) -> worker::Result<(Account, T)>
where
    W: FnMut(Account) -> WF,
    WF: Future<Output = worker::Result<T>>,
    R: Fn(&worker::Result<T>) -> bool,
{
    with_renewal_if(
        || host.account(purpose),
        |token| host.forget(purpose, token),
        work,
        refused,
    )
    .await
}

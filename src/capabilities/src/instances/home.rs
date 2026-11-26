//! The home instance's credential: a short-lived token of purpose [`PURPOSE`] (`instance_home`),
//! minted from the parent token for one plan or one apply and revoked after it, whether the work
//! succeeded or not. It holds R2 write (the landing bucket) and the zone's custom-domain
//! permissions, never Workers-scripts write: the platform's Workers are the Lander's.
//!
//! The token's value lives only in the [`Account`] handed to the work, as its bearer, so every
//! core helper uses it as it is and mints nothing else from the parent. It is never stored,
//! logged or returned.

use std::future::Future;

use pigeon_core::Account;
use pigeon_core::tokens::{MintRequest, Policy, Scope};
use worker::{Error, Result};

/// The credential purpose of the home instance's plans and applies, in
/// [`crate::split::purpose_permissions`].
pub const PURPOSE: &str = "instance_home";

/// How long the home token lives, in seconds, if revoking it ever fails.
pub const TTL_SECS: u64 = 600;

/// The name the token is minted under, `pigeon:instance-home:<time>`.
const TOKEN_PURPOSE: &str = "instance-home";

/// What the person does when the home token cannot be had.
pub const PERSON_STEPS: &str = "Nothing was changed. The person confirms that `instance_home` \
     is a credential purpose of the deployed platform (it lands through the gate), and that the \
     parent token holds its permissions (Workers R2 Storage Write on the account; Zone Read on \
     the platform zone); then \
     plan again.";

fn bad(why: String) -> Error {
    Error::RustError(why)
}

/// Whether permission `name` is granted on the account rather than on the zone: R2's.
fn account_wide(name: &str) -> bool {
    name.starts_with("Workers R2 Storage")
}

/// The policies of the home token, from `permissions` (what [`PURPOSE`] brings, `None` when this
/// platform does not know it yet): R2 on the account, the rest on zone `zone_id` alone. Refuses
/// a purpose that is missing, empty, or would bring any Workers-scripts permission. Pure.
pub fn policies(
    permissions: Option<&[&str]>,
    zone_id: &str,
) -> std::result::Result<Vec<Policy>, String> {
    let Some(permissions) = permissions.filter(|p| !p.is_empty()) else {
        return Err(format!(
            "`{PURPOSE}` is not a credential purpose of this platform yet. {PERSON_STEPS}"
        ));
    };
    if let Some(p) = permissions
        .iter()
        .find(|p| p.starts_with("Workers Scripts"))
    {
        return Err(format!(
            "`{PURPOSE}` brings {p}, which the home instance never holds; nothing was changed"
        ));
    }
    if zone_id.is_empty() {
        return Err(format!(
            "the platform zone was not found, so `{PURPOSE}` cannot be scoped to it. {PERSON_STEPS}"
        ));
    }
    let account: Vec<&str> = permissions
        .iter()
        .copied()
        .filter(|p| account_wide(p))
        .collect();
    let zone: Vec<&str> = permissions
        .iter()
        .copied()
        .filter(|p| !account_wide(p))
        .collect();
    let mut out = Vec::new();
    if !account.is_empty() {
        out.push(Policy::new(Scope::Account, &account));
    }
    if !zone.is_empty() {
        out.push(Policy::new(Scope::Zones(vec![zone_id.to_string()]), &zone));
    }
    Ok(out)
}

/// What mints and revokes the home token: the home account's token ledger in production, a
/// double in the smoke tests.
#[allow(async_fn_in_trait)] // Workers futures are not `Send`; nothing here needs it.
pub trait Minter {
    /// Mints a token named for `purpose` holding `policies`, living `ttl_secs`: its id and value.
    async fn mint(
        &self,
        purpose: &str,
        policies: Vec<Policy>,
        ttl_secs: u64,
    ) -> Result<(String, String)>;
    /// Revokes token `id`.
    async fn revoke(&self, id: &str) -> Result<()>;
}

/// The platform's own account mints through its token ledger, as every scoped token does
/// (`pigeon_core::tokens::Scoped::open_for`).
impl Minter for Account {
    async fn mint(
        &self,
        purpose: &str,
        policies: Vec<Policy>,
        ttl_secs: u64,
    ) -> Result<(String, String)> {
        let request = MintRequest {
            ttl_secs,
            ..MintRequest::new(purpose, policies)
        };
        let minted = pigeon_core::tokens::mint_for(self, &request).await?;
        Ok((minted.id, minted.value))
    }

    async fn revoke(&self, id: &str) -> Result<()> {
        pigeon_core::tokens::revoke_for(self, id).await
    }
}

/// Runs `work` with the home account `account_id` (Secrets Store `secrets_store`) whose bearer is
/// a token of purpose [`PURPOSE`] minted by `minter`, then revokes the token, whether `work`
/// succeeded or not; `work`'s own error comes first. Refuses before minting, and so before any
/// work, when the purpose is unknown (`permissions` is `None`) or the parent token cannot mint
/// it, naming what the person does.
pub async fn with_token<M, F, Fut, T>(
    minter: &M,
    permissions: Option<&[&str]>,
    zone_id: &str,
    account_id: &str,
    secrets_store: &str,
    work: F,
) -> Result<T>
where
    M: Minter,
    F: FnOnce(Account) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let policies = policies(permissions, zone_id).map_err(bad)?;
    let (id, value) = minter
        .mint(TOKEN_PURPOSE, policies, TTL_SECS)
        .await
        .map_err(|e| {
            bad(format!(
                "the parent token could not mint the `{PURPOSE}` token ({e}). {PERSON_STEPS}"
            ))
        })?;
    let account = Account {
        id: account_id.to_string(),
        parent: String::new(),
        secrets_store: secrets_store.to_string(),
        bearer: Some(value),
        token_ledger: None,
    };
    let result = work(account).await;
    let revoked = minter.revoke(&id).await;
    let value = result?;
    revoked.map_err(|e| {
        bad(format!(
            "the work was done, but the `{PURPOSE}` token was not revoked ({e}); it expires on \
             its own within {TTL_SECS} s"
        ))
    })?;
    Ok(value)
}

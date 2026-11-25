//! Operation-specific callbacks for a Sites Worker with verified publishing context.

use pigeon_contract::{Credential, SitesScopeRequest};
use serde_json::Value;
use worker::{Env, Error, Result};

use crate::instances::{
    self,
    record::{AccountRef, Instance, RepoRef},
};

/// Verify the approved record and exact publishing binding before any credential work.
pub fn approved_record(record: &Instance, request: &SitesScopeRequest) -> Result<Value> {
    if record.id != request.instance() {
        return Err(Error::RustError(
            "publishing instance differs from the approved record".into(),
        ));
    }
    if !matches!(
        record.account,
        AccountRef::Linked {
            account_id: Some(_)
        }
    ) || !matches!(record.repo, Some(RepoRef::Github { .. }))
    {
        return Err(Error::RustError(
            "publishing needs a GitHub repository and a linked account".into(),
        ));
    }
    let value = serde_json::to_value(record)?;
    pigeon_core::site_publish::site(&value).map_err(Error::RustError)?;
    if request
        .binding()
        .is_some_and(|binding| binding != pigeon_core::oauth::hash(&value.to_string()))
    {
        return Err(Error::RustError(
            "the approved publishing record changed".into(),
        ));
    }
    Ok(value)
}

async fn read(env: &Env, request: &SitesScopeRequest) -> Result<Instance> {
    instances::one(env, request.instance())
        .await?
        .ok_or_else(|| Error::RustError("unknown publishing instance".into()))
}

/// Resolve an approved record or one of the existing publishing grants. Only core's
/// verified instance-scoped callback dispatcher calls this.
pub(crate) async fn handle(env: &Env, request: &SitesScopeRequest) -> Result<Value> {
    let record = read(env, request).await?;
    let value = approved_record(&record, request)?;
    let result = match request {
        SitesScopeRequest::Record { .. } => return Ok(value),
        SitesScopeRequest::Linked { instance, .. } => {
            let AccountRef::Linked {
                account_id: Some(account),
            } = record.account
            else {
                unreachable!("approved_record checked the account kind")
            };
            crate::split::linked_credential(env, instance, Some(&account)).await?
        }
        SitesScopeRequest::Github { .. } => {
            let Some(RepoRef::Github {
                full_name,
                installation,
            }) = record.repo
            else {
                unreachable!("approved_record checked the repository kind")
            };
            let Some(installation) = installation else {
                return Ok(Value::Null);
            };
            let (token, _) = crate::github::installation_token_for(
                env,
                &full_name,
                installation,
                &[("contents", "read")],
            )
            .await?;
            serde_json::to_value(Credential {
                token,
                account_id: String::new(),
                expires_at: worker::Date::now().as_millis() as i64 + 10 * 60_000,
                token_id: None,
            })?
        }
    };
    // A provider request can take long enough for an approval to change. Never hand a
    // credential out when the exact approved publishing record no longer matches.
    approved_record(&read(env, request).await?, request)?;
    Ok(result)
}

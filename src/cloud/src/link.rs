//! The Worker's link to core over its `PLATFORM` binding: asking core an op as the caller core
//! signed, in the way the model gateway asks for a credential.

use pigeon_contract::{Answer, Credential, CredentialRequest, Signed};
use pigeon_core::Account;
use pigeon_core::webauthn::random;
use serde_json::Value;
use worker::{Env, Error, Result};

/// The Service Binding to core.
const PLATFORM: &str = "PLATFORM";

/// Core, as one caller reaches it.
pub struct Link<'a> {
    /// The Worker's environment.
    pub env: &'a Env,
    /// The caller core signed for this request.
    pub caller: &'a Signed,
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

impl Link<'_> {
    /// Asks core op `op` with `body`, signed for the caller.
    pub async fn ask(&self, op: &str, body: &Value) -> Result<Answer> {
        let platform = self.env.service(PLATFORM)?;
        // This Worker's own key (CALLER_KEY_OWN), never the shared one: core then knows which
        // Worker asked (docs/plans/caller-keys.md, step 3).
        let key = pigeon_contract::own_key(self.env).await?;
        let nonce = random()?;
        pigeon_contract::ask(
            &platform,
            "https://core",
            op,
            body,
            Some((&key, &self.caller.caller)),
            &nonce,
        )
        .await
    }

    /// An account acting with a token core minted for `purposes` (`credential`, kind `scoped`).
    pub async fn account(&self, purposes: &[&str]) -> Result<Account> {
        let request = CredentialRequest {
            kind: "scoped".into(),
            instance: None,
            scopes: purposes.iter().map(|p| p.to_string()).collect(),
        };
        let body = serde_json::to_value(&request)?;
        match self.ask("credential", &body).await? {
            Answer::Ok(value) => {
                let given: Credential = serde_json::from_value(value)
                    .map_err(|e| bad(format!("core's credential does not parse: {e}")))?;
                Ok(Account {
                    id: given.account_id,
                    parent: String::new(),
                    secrets_store: self.env.var("CF_SECRETS_STORE_ID")?.to_string(),
                    bearer: Some(given.token),
                    token_ledger: None,
                })
            }
            Answer::Error(f) => Err(bad(format!("core gave no credential: {}", f.message))),
        }
    }
}

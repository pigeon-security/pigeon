//! The account's Secrets Store. A Worker binding reads one bound secret; every other operation
//! goes through the API on a token minted for it and revoked after.

use serde::Deserialize;
use serde_json::json;
use worker::{Error, Method, Result, SecretStore};

use crate::cloudflare::call;
use crate::tokens::{self, MintRequest, Minted, Policy, Scope};

/// The permission a writer's token holds, and nothing else.
pub const WRITE_PERMISSION: &str = "Secrets Store Write";

/// Reads the secret behind `binding`.
pub async fn read(binding: &SecretStore) -> Result<Option<String>> {
    binding.get().await
}

/// A secret as listed: never its value.
#[derive(Debug, Deserialize)]
pub struct Secret {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
}

/// Writes to `store` on a minted token. Call [`Writer::finish`] to revoke the token.
pub struct Writer {
    parent: String,
    account: String,
    store: String,
    token: Minted,
}

impl Writer {
    /// Mints a [`WRITE_PERMISSION`] token, as `parent`, for writing to `store`.
    pub async fn open(parent: &str, account: &str, store: &str) -> Result<Self> {
        let request = MintRequest::new(
            "secrets-store",
            vec![Policy::new(Scope::Account, &[WRITE_PERMISSION])],
        );
        let token = tokens::mint(parent, account, &request).await?;
        Ok(Self {
            parent: parent.into(),
            account: account.into(),
            store: store.into(),
            token,
        })
    }

    fn path(&self) -> String {
        format!(
            "/accounts/{}/secrets_store/stores/{}/secrets",
            self.account, self.store
        )
    }

    /// Every secret in the store.
    pub async fn list(&self) -> Result<Vec<Secret>> {
        let v = call(&self.token.value, Method::Get, &self.path(), None).await?;
        Ok(serde_json::from_value(v)?)
    }

    /// Creates secret `name`, readable by Workers; answers its id.
    pub async fn create(&self, name: &str, value: &str, comment: &str) -> Result<String> {
        let body =
            json!([{ "name": name, "value": value, "scopes": ["workers"], "comment": comment }]);
        let v = call(&self.token.value, Method::Post, &self.path(), Some(&body)).await?;
        v[0]["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::RustError(format!("secret `{name}` came back without its id")))
    }

    /// Replaces secret `id`'s value.
    pub async fn update(&self, id: &str, value: &str) -> Result<()> {
        let body = json!({ "value": value, "scopes": ["workers"] });
        call(
            &self.token.value,
            Method::Patch,
            &format!("{}/{id}", self.path()),
            Some(&body),
        )
        .await?;
        Ok(())
    }

    /// Deletes secret `id`.
    pub async fn delete(&self, id: &str) -> Result<()> {
        call(
            &self.token.value,
            Method::Delete,
            &format!("{}/{id}", self.path()),
            None,
        )
        .await?;
        Ok(())
    }

    /// Sets secret `name` to `value`, creating it or replacing its value.
    pub async fn put(&self, name: &str, value: &str, comment: &str) -> Result<()> {
        match self.list().await?.into_iter().find(|s| s.name == name) {
            Some(secret) => self.update(&secret.id, value).await,
            None => self.create(name, value, comment).await.map(|_| ()),
        }
    }

    /// Revokes the writer's token.
    pub async fn finish(self) -> Result<()> {
        tokens::revoke(&self.parent, &self.account, &self.token.id).await
    }
}

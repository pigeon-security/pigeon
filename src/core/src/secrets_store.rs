//! The account's Secrets Store. A Worker binding reads one bound secret; every other operation
//! goes through the API on a token minted for it and revoked after.

use serde::Deserialize;
use serde_json::json;
use worker::{Error, Method, Result, SecretStore};

use crate::Account;
use crate::cloudflare::{call, call_fresh};
use crate::tokens::{Policy, Scope, Scoped};

/// The scope a secret needs for a Worker to bind it.
pub const WORKERS: &str = "workers";

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
    /// When its value last changed, as Cloudflare reports it.
    #[serde(default)]
    pub modified: Option<String>,
}

/// Writes to the account's store on a minted token. Call [`Writer::finish`] to revoke it.
pub struct Writer<'a> {
    account: &'a Account,
    token: Scoped<'a>,
}

/// Secrets asked for per page when listing the store.
pub const PER_PAGE: usize = 100;
/// A bound on how many pages [`Writer::list`] reads.
const MAX_PAGES: usize = 20;

/// Whether a listing page holding `n` secrets may have a page after it.
pub fn more_pages(n: usize) -> bool {
    n >= PER_PAGE
}

impl<'a> Writer<'a> {
    /// Mints a [`WRITE_PERMISSION`] token for writing to `account`'s store.
    pub async fn open(account: &'a Account) -> Result<Self> {
        let policies = vec![Policy::new(Scope::Account, &[WRITE_PERMISSION])];
        let token = Scoped::open(account, "secrets-store", policies).await?;
        Ok(Self { account, token })
    }

    fn path(&self) -> String {
        format!(
            "/accounts/{}/secrets_store/stores/{}/secrets",
            self.account.id, self.account.secrets_store
        )
    }

    /// Every secret in the store, across all of the API's pages (it answers 20 per page by
    /// default, so a single read misses secrets once the store holds more).
    pub async fn list(&self) -> Result<Vec<Secret>> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let path = format!("{}?per_page={PER_PAGE}&page={page}", self.path());
            // The token was minted moments ago; the store may not know it yet.
            let v = call_fresh(self.token.value(), Method::Get, &path, None).await?;
            let batch: Vec<Secret> = serde_json::from_value(v)?;
            let n = batch.len();
            all.extend(batch);
            if !more_pages(n) {
                return Ok(all);
            }
        }
        Err(Error::RustError(format!(
            "the Secrets Store holds more than {} secrets",
            MAX_PAGES * PER_PAGE
        )))
    }

    /// Creates secret `name`, readable by the services in `scopes`; answers its id.
    pub async fn create(
        &self,
        name: &str,
        value: &str,
        comment: &str,
        scopes: &[&str],
    ) -> Result<String> {
        let body = json!([{ "name": name, "value": value, "scopes": scopes, "comment": comment }]);
        let v = call(self.token.value(), Method::Post, &self.path(), Some(&body)).await?;
        v[0]["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| Error::RustError(format!("secret `{name}` came back without its id")))
    }

    /// Replaces secret `id`'s value, readable by the services in `scopes`.
    pub async fn update(&self, id: &str, value: &str, scopes: &[&str]) -> Result<()> {
        let body = json!({ "value": value, "scopes": scopes });
        let path = format!("{}/{id}", self.path());
        call(self.token.value(), Method::Patch, &path, Some(&body)).await?;
        Ok(())
    }

    /// Deletes secret `id`.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let path = format!("{}/{id}", self.path());
        call(self.token.value(), Method::Delete, &path, None).await?;
        Ok(())
    }

    /// Sets secret `name` to `value` for Workers, creating it or replacing its value.
    pub async fn put(&self, name: &str, value: &str, comment: &str) -> Result<()> {
        self.put_scoped(name, value, comment, &[WORKERS])
            .await
            .map(|_| ())
    }

    /// Sets secret `name` to `value` for the services in `scopes`, creating it or replacing its
    /// value; answers its id, which a replaced secret keeps.
    pub async fn put_scoped(
        &self,
        name: &str,
        value: &str,
        comment: &str,
        scopes: &[&str],
    ) -> Result<String> {
        match self.list().await?.into_iter().find(|s| s.name == name) {
            Some(secret) => self
                .update(&secret.id, value, scopes)
                .await
                .map(|()| secret.id),
            None => self.create(name, value, comment, scopes).await,
        }
    }

    /// Revokes the writer's token, then answers `result`.
    pub async fn finish<T>(self, result: Result<T>) -> Result<T> {
        self.token.finish(result).await
    }
}

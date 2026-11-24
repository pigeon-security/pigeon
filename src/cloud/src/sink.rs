//! Where a new key's secret goes: a Secrets Store secret named by the caller. The secret is
//! never in an answer, an error or a log line.

use pigeon_core::secrets_store::{self, Writer};

use crate::link::Link;
use crate::transport::Fut;

/// Writes secrets. Core's token and the Secrets Store back it in the Worker; the smoke suite
/// stubs it.
pub trait Sink {
    /// Whether secret `slot` exists. An error means the sink cannot be written to, so a plan
    /// asks this before it creates anything.
    fn exists<'a>(&'a self, slot: &'a str) -> Fut<'a, bool>;

    /// Writes `value` to secret `slot`, readable by Workers, with `comment`.
    fn write<'a>(&'a self, slot: &'a str, value: &'a str, comment: &'a str) -> Fut<'a, ()>;
}

/// The Secrets Store, written on a `secrets_store` token core mints for the call and revokes
/// after.
pub struct CoreSink<'a>(pub Link<'a>);

impl Sink for CoreSink<'_> {
    fn exists<'a>(&'a self, slot: &'a str) -> Fut<'a, bool> {
        Box::pin(async move {
            let account = self.0.account(&["secrets_store"]).await?;
            let writer = Writer::open(&account).await?;
            let listed = writer.list().await;
            let names = writer.finish(listed).await?;
            Ok(names.iter().any(|s| s.name == slot))
        })
    }

    fn write<'a>(&'a self, slot: &'a str, value: &'a str, comment: &'a str) -> Fut<'a, ()> {
        Box::pin(async move {
            let account = self.0.account(&["secrets_store"]).await?;
            let writer = Writer::open(&account).await?;
            let put = writer
                .put_scoped(slot, value, comment, &[secrets_store::WORKERS])
                .await
                .map(|_| ());
            writer.finish(put).await
        })
    }
}

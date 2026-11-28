//! Core sends signed compute operations to the approved instance Data Worker.

use pigeon_cloud::digitalocean_compute::{Command, PATH, Pin, Request};
use pigeon_ingest::managed_destination;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use worker::{Date, Env, Result};

use crate::{data_store, ingest::net, instances::record::Instance, users::directory};

/// The job keeps the selected generation and origin until every resource is gone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Broker {
    pub pin: Pin,
    pub origin: String,
}

fn bad(message: &'static str) -> worker::Error {
    worker::Error::RustError(message.into())
}

impl Broker {
    /// Confirms the approved team and provisioned key inside the linked Data Worker.
    pub async fn check(env: &Env, record: &Instance, pin: Pin, region: &str) -> Result<Self> {
        if crate::catalogue::is_home(record) || data_store::inline(record) {
            return Err(bad(
                "instance-owned DigitalOcean compute requires a linked standalone Data Worker",
            ));
        }
        if super::team(record) != Some(pin.team.as_str()) || pin.binding(&record.id).is_none() {
            return Err(bad(
                "the compute generation differs from the approved DigitalOcean team",
            ));
        }
        let (origin, key) = data_store::reach(env, record).await?;
        let broker = Self { pin, origin };
        let reply: Pin = broker
            .send(
                &key,
                Command::Check {
                    region: region.into(),
                },
            )
            .await?;
        if reply != broker.pin {
            return Err(bad(
                "the Data Worker did not confirm the approved compute generation",
            ));
        }
        Ok(broker)
    }

    /// Routes cleanup using the job's immutable origin and credential generation even if Data
    /// Ingest was removed from the record. Reapplying the same Worker may rotate its signing key.
    pub async fn command<T: DeserializeOwned>(
        &self,
        env: &Env,
        instance: &str,
        operation: Command,
    ) -> Result<T> {
        let origin = directory(
            env,
            "setting.get",
            json!({"key": data_store::url_setting(instance)}),
        )
        .await?;
        if origin.as_str() != Some(&self.origin) {
            return Err(bad("the approved DigitalOcean Data Worker origin changed"));
        }
        let key = directory(
            env,
            "setting.get",
            json!({"key": data_store::key_setting(instance)}),
        )
        .await?;
        let key = key
            .as_str()
            .ok_or_else(|| bad("the approved DigitalOcean Data Worker key is unavailable"))?;
        self.send(key, operation).await
    }

    async fn send<T: DeserializeOwned>(&self, key: &str, command: Command) -> Result<T> {
        if !managed_destination::valid_origin(&self.origin) {
            return Err(bad(
                "the approved DigitalOcean Data Worker origin is invalid",
            ));
        }
        let body = serde_json::to_value(Request {
            pin: self.pin.clone(),
            command,
        })?;
        let response = net::data_post(
            &self.origin,
            PATH,
            key,
            Date::now().as_millis() as i64,
            &body,
        )
        .await
        .map_err(|_| {
            bad("the approved DigitalOcean Data Worker could not perform the compute operation")
        })?;
        serde_json::from_str(&response).map_err(|_| {
            bad("the approved DigitalOcean Data Worker returned an invalid compute result")
        })
    }
}

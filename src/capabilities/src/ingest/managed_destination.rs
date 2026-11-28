//! Review the Data Worker destination before issuing an ingest approval.

use pigeon_ingest::{managed_destination as wire, manifest::JobSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Result};

use super::{bad, net, now_ms};
use crate::{data_store, instances::record::Instance};

pub(super) struct Binding {
    pub origin: String,
    /// The Data Worker's multipart publisher is configured, so objects over 16 MiB can be
    /// written.
    pub multipart: bool,
    instance: String,
    key: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Reviewed {
    pub origin: String,
    pub max_bytes: u64,
    pub max_objects: u64,
}

impl Binding {
    pub fn review(&self, source_bytes: u64, source_objects: u64, dry_run: bool) -> Reviewed {
        let (max_bytes, max_objects) = wire::run_budget(source_bytes, source_objects, dry_run);
        Reviewed {
            origin: self.origin.clone(),
            max_bytes,
            max_objects,
        }
    }

    pub fn issue(
        &self,
        job: &JobSpec,
        time: i64,
        deadline: i64,
        reviewed: &Reviewed,
    ) -> Result<wire::Destination> {
        if self.origin != reviewed.origin || job.instance != self.instance {
            return Err(bad("the managed destination changed; plan again"));
        }
        let grant = wire::Grant {
            instance: self.instance.clone(),
            run: job.run.clone(),
            issued_ms: time,
            expires_ms: deadline.saturating_add(5 * 60_000),
            dry_run: job.dry_run,
            max_bytes: reviewed.max_bytes,
            max_objects: reviewed.max_objects,
        };
        let grant = wire::issue(&grant, self.key.as_bytes(), time)
            .ok_or_else(|| bad("invalid approved destination grant bounds"))?;
        Ok(wire::Destination {
            origin: self.origin.clone(),
            grant,
        })
    }
}

pub(super) async fn ready(env: &Env, record: &Instance) -> Result<Binding> {
    if data_store::inline(record) {
        return Err(bad("Managed ingest requires a standalone Data Worker."));
    }
    let (origin, key) = data_store::reach(env, record).await?;
    if !wire::valid_origin(&origin) {
        return Err(bad("The Data Worker has no approved HTTPS origin."));
    }
    let reply = net::data_post(&origin, wire::READY_PATH, &key, now_ms(), &json!({})).await
        .map_err(|_| bad("The Data Worker does not have the managed destination. Apply its reviewed instance plan first."))?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["version"] != 1 || reply["instance"] != record.id {
        return Err(bad(
            "The managed destination does not match this Data instance.",
        ));
    }
    Ok(Binding {
        multipart: reply["multipart_configured"].as_bool().unwrap_or(false),
        origin,
        instance: record.id.clone(),
        key,
    })
}

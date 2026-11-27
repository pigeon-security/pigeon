//! Container provisioning for an instance-owned Data Worker. The approved record pins the
//! target account and image. It uses the same metadata and API calls as Wrangler 4.166.0.

use pigeon_core::cloudflare::call;
use pigeon_ingest::container_job::Runtime;
use serde_json::{Value, json};
use std::time::Duration;
use worker::{Delay, Method, Result};

use crate::catalogue::Placement;

use super::data_apply::{bad, get};
use super::record::{AccountRef, Instance};

pub const SCOPE: &str = "data-ingest-backblaze-runtime";
const CLASS: &str = "IngestJob";
const BINDING: &str = "INGEST_JOB";
const MIGRATION: &str = "data-ingest-container-v1";

pub fn configured(record: &Instance) -> Option<&Runtime> {
    crate::ingest::backblaze::source(record)?.runtime.as_ref()
}

pub fn field_problems(record: &Instance, runtime: &Runtime) -> Option<&'static str> {
    if crate::data_store::placement(record) != Placement::OwnWorker {
        return Some("Backblaze ingest containers require Data in its own Worker.");
    }
    match &record.account {
        AccountRef::Linked {
            account_id: Some(account),
        } if runtime.valid_for(account) => None,
        _ => Some(
            "Pin the Backblaze runner to a full Git revision and digest in this linked account's registry.",
        ),
    }
}

pub fn application_name(worker: &str) -> String {
    const SUFFIX: &str = "-ingest";
    const MAX_NAME: usize = 63;
    // Worker names are validated ASCII. A long worker name uses a hashed suffix so distinct
    // names cannot truncate to the same container app name.
    if worker.len() + SUFFIX.len() <= MAX_NAME {
        return format!("{worker}{SUFFIX}");
    }
    let digest = pigeon_core::sign::hex(pigeon_core::oauth::hash(worker).as_bytes());
    let prefix = &worker[..worker.floor_char_boundary(39)];
    format!("{prefix}-{}{SUFFIX}", &digest[..16])
}

/// Add the versioned Container metadata, the named DO binding and the first SQLite
/// migration to Data's ordinary Worker upload. Leave other Worker bindings intact.
pub fn bind(
    metadata: &mut Value,
    worker: &str,
    origin: &str,
    runtime: &Runtime,
    migration: Option<Value>,
) {
    let bindings = metadata["bindings"]
        .as_array_mut()
        .expect("Data Worker bindings");
    bindings.extend([
        json!({"type": "durable_object_namespace", "name": BINDING, "class_name": CLASS}),
        json!({"type": "plain_text", "name": "INGEST_IMAGE_REVISION", "text": runtime.revision}),
        json!({"type": "plain_text", "name": "PIGEON_PLATFORM_ORIGIN", "text": origin}),
    ]);
    metadata["containers"] = json!([{
        "name": application_name(worker),
        "class_name": CLASS,
        "images": {"ingest": runtime.image},
    }]);
    if let Some(migration) = migration {
        metadata["migrations"] = migration;
    }
}

fn migration_payload(prior: Option<&str>) -> Value {
    let mut migration = json!({
        "new_tag": MIGRATION,
        "steps": [{"new_sqlite_classes": [CLASS]}],
    });
    if let Some(prior) = prior {
        migration["old_tag"] = json!(prior);
    }
    migration
}

async fn namespace(token: &str, account: &str, worker: &str) -> Result<Option<String>> {
    const PAGE: usize = 1000;
    for page in 1..=5 {
        let found = call(
            token,
            Method::Get,
            &format!(
                "/accounts/{account}/workers/durable_objects/namespaces?per_page={PAGE}&page={page}"
            ),
            None,
        )
        .await?;
        let rows = found
            .as_array()
            .ok_or_else(|| bad("Cloudflare returned no Durable Object list"))?;
        let matched = rows.iter().find(|row| {
            row["script"] == worker
                && row["class"] == CLASS
                && row["preview"].is_null()
                && row["dispatch_namespace"].is_null()
        });
        if let Some(row) = matched {
            if row["use_sqlite"] != true {
                return Err(bad(
                    "The Data Worker ingest class must use SQLite-backed Durable Objects.",
                ));
            }
            let id = row["id"]
                .as_str()
                .filter(|id| picker_id(id))
                .ok_or_else(|| bad("Cloudflare returned no valid ingest class namespace"))?;
            return Ok(Some(id.to_string()));
        }
        if rows.len() < PAGE {
            return Ok(None);
        }
    }
    Err(bad(
        "Too many Durable Object namespace pages to check the Data Worker safely.",
    ))
}

fn picker_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

async fn application(
    token: &str,
    account: &str,
    worker: &str,
    namespace: &str,
) -> Result<Option<Value>> {
    let app = get(
        token,
        &format!("/accounts/{account}/containers/applications/{namespace}"),
    )
    .await?;
    if let Some(app) = &app
        && (app["id"] != namespace
            || app["durable_objects"]["namespace_id"] != namespace
            || app["name"] != application_name(worker)
            || app["scheduling_policy"] != "durable_object")
    {
        return Err(bad(
            "The existing Container application's identity does not match the Data Worker plan.",
        ));
    }
    Ok(app)
}

pub async fn installed(token: &str, account: &str, worker: &str) -> Result<bool> {
    let Some(namespace) = namespace(token, account, worker).await? else {
        return Ok(false);
    };
    Ok(application(token, account, worker, &namespace)
        .await?
        .is_some())
}

/// Prepare the image on Cloudflare before modifying the Data Worker's deployment. The image
/// must be published by an account owner or release automation before the instance plan applies.
pub async fn prepare(
    token: &str,
    account: &str,
    worker: &str,
    runtime: &Runtime,
) -> Result<Option<Value>> {
    if !runtime.valid_for(account) {
        return Err(bad(
            "The approved runner is not pinned in this account's registry.",
        ));
    }
    let reply = call(
        token,
        Method::Post,
        &format!("/accounts/{account}/containers/image-preparations"),
        Some(&json!({"image": runtime.image})),
    )
    .await?;
    match reply["status"].as_str() {
        Some("ready") => (),
        Some("pending") => {
            return Err(bad(
                "Cloudflare is preparing the pinned image. Replan once it is ready.",
            ));
        }
        _ => return Err(bad("Cloudflare could not prepare the pinned ingest image.")),
    }
    let existing = namespace(token, account, worker).await?;
    if existing.is_some() {
        return Ok(None);
    }
    let service = get(
        token,
        &format!("/accounts/{account}/workers/services/{worker}"),
    )
    .await?;
    let prior = service
        .as_ref()
        .and_then(|value| value["default_environment"]["script"]["migration_tag"].as_str());
    if prior == Some(MIGRATION) {
        return Err(bad(
            "The Data Worker lists the migration but not the new namespace; retry the instance plan.",
        ));
    }
    Ok(Some(migration_payload(prior)))
}

pub async fn attach(token: &str, account: &str, worker: &str) -> Result<()> {
    let mut found = namespace(token, account, worker).await?;
    for millis in [250, 500, 1000] {
        if found.is_some() {
            break;
        }
        Delay::from(Duration::from_millis(millis)).await;
        found = namespace(token, account, worker).await?;
    }
    let namespace = found.ok_or_else(|| {
        bad("The Data Worker's new ingest namespace is not visible yet. Retry the instance plan.")
    })?;
    if application(token, account, worker, &namespace)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let created = call(
        token,
        Method::Post,
        &format!("/accounts/{account}/containers/applications"),
        Some(&json!({
            "name": application_name(worker),
            "scheduling_policy": "durable_object",
            "durable_objects": {"namespace_id": namespace},
            "observability": {"logs": {"enabled": false}},
        })),
    )
    .await;
    // A provider can create the app even when the response is lost. Read its exact identity
    // before treating either a successful or an ambiguous result as attached.
    if application(token, account, worker, &namespace)
        .await?
        .is_some()
    {
        return Ok(());
    }
    created?;
    Err(bad(
        "Cloudflare did not confirm the new ingest Container application.",
    ))
}

//! Instance-owned Scaleway compute broker. Its provider credential is never returned.
use pigeon_cloud::scaleway_ingest;
use pigeon_core::secrets_store;
use pigeon_ingest::{
    protocol,
    scaleway::{self, BROKER_PATH, BrokerRun, BrokerStatus, ComputeCommand, ComputePin},
};
use serde_json::json;
use worker::{Env, Request, Response, Result};

use crate::{fail, json_answer, managed_dest::bounded_body, now_ms};

async fn credential(env: &Env, pin: &ComputePin) -> Result<String> {
    let instance = env.var("INSTANCE")?.to_string();
    let binding = pin
        .binding(&instance)
        .ok_or_else(|| fail("invalid Scaleway credential pin"))?;
    let store = env
        .secret_store(&binding)
        .map_err(|_| fail("the selected Scaleway credential generation is not bound"))?;
    let value = secrets_store::read(&store)
        .await?
        .ok_or_else(|| fail("the selected Scaleway credential is unavailable"))?;
    let (project, key) = scaleway::scaleway_parts(&value).map_err(fail)?;
    if project != pin.project {
        return Err(fail("the Scaleway credential does not match its project"));
    }
    Ok(key)
}

pub async fn fetch(req: &mut Request, env: &Env) -> Result<Response> {
    let key = env.secret("DATA_KEY")?.to_string();
    let signature = req.headers().get(protocol::SIGNATURE_HEADER)?;
    let Some(body) = bounded_body(req, 4096).await? else {
        return json_answer(413, json!({"error": "compute request too large"}));
    };
    let signed = signature.is_some_and(|signature| {
        protocol::verify(
            &signature,
            "POST",
            BROKER_PATH,
            &body,
            key.as_bytes(),
            now_ms(),
        )
        .is_ok()
    });
    if !signed {
        return json_answer(401, json!({"error": "unauthorized"}));
    }
    let Ok(command) = serde_json::from_slice(&body) else {
        return json_answer(400, json!({"error": "invalid Scaleway compute command"}));
    };
    match command {
        ComputeCommand::Check { pin, region } => check(env, pin, &region).await,
        ComputeCommand::Status { run } => observe(env, &run, false).await,
        ComputeCommand::Cleanup { run } => observe(env, &run, true).await,
    }
}

async fn check(env: &Env, pin: ComputePin, region: &str) -> Result<Response> {
    if !scaleway::valid_region(region) {
        return json_answer(400, json!({"error": "invalid Scaleway region"}));
    }
    let Ok(secret) = credential(env, &pin).await else {
        return json_answer(
            409,
            json!({"error": "the approved Scaleway credential is not bound"}),
        );
    };
    if scaleway_ingest::check_project(&secret, &scaleway::zone(region), &pin.project)
        .await
        .is_err()
    {
        return json_answer(
            409,
            json!({"error": "the credential cannot list the Scaleway project"}),
        );
    }
    json_answer(
        200,
        json!({"project": pin.project, "generation": pin.generation}),
    )
}

async fn observe(env: &Env, run: &str, cleanup: bool) -> Result<Response> {
    if !pigeon_ingest::container_job::valid_run(run) {
        return json_answer(400, json!({"error": "invalid Scaleway run"}));
    }
    let Some(object) = env
        .bucket("DATA")?
        .get(scaleway::broker_run_key(run))
        .execute()
        .await?
    else {
        return json_answer(
            404,
            json!({"error": "Scaleway run is not owned by this Data Worker"}),
        );
    };
    if object.size() > 4096 {
        return Err(fail("invalid Scaleway ownership record"));
    }
    let bytes = object
        .body()
        .ok_or_else(|| fail("missing Scaleway ownership record"))?
        .bytes()
        .await?;
    let mut owned: BrokerRun = serde_json::from_slice(&bytes)?;
    if owned.run != run
        || !scaleway::valid_region(&owned.region)
        || owned
            .server
            .as_deref()
            .is_some_and(|id| !scaleway::valid_project(id))
        || owned.volumes.iter().any(|id| !scaleway::valid_project(id))
    {
        return Err(fail("invalid Scaleway ownership record"));
    }
    let secret = credential(env, &owned.pin).await?;
    let zone = scaleway::zone(&owned.region);
    if owned.server.is_none()
        && let Some(found) =
            scaleway_ingest::scw_find_created(&secret, &zone, &owned.pin.project, run).await?
    {
        owned.server = Some(found.server);
        owned.volumes = found.volumes;
    }
    let state = match &owned.server {
        Some(server) => scaleway_ingest::scw_state(&secret, &zone, server).await?,
        None => None,
    };
    // An ambiguous create can take time to appear. Without an observed server ID,
    // confirm absence only after the original deadline.
    let gone = if cleanup && (owned.server.is_some() || now_ms() >= owned.deadline_ms) {
        scaleway_ingest::cleanup(
            &secret,
            &zone,
            &owned.pin.project,
            run,
            owned.server.as_deref(),
            &mut owned.volumes,
        )
        .await?
    } else {
        false
    };
    json_answer(
        200,
        serde_json::to_value(BrokerStatus {
            run: owned.run,
            server: owned.server,
            state,
            volumes: owned.volumes,
            gone,
        })?,
    )
}

//! Instance-owned Scaleway compute broker. Its provider credential is never returned.
use pigeon_cloud::scaleway_ingest;
use pigeon_core::secrets_store;
use pigeon_ingest::{
    protocol,
    scaleway::{self, BROKER_PATH, ComputeCommand, ComputePin},
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
    let Ok(ComputeCommand::Check { pin, region }) = serde_json::from_slice(&body) else {
        return json_answer(400, json!({"error": "invalid Scaleway compute command"}));
    };
    if !scaleway::valid_region(&region) {
        return json_answer(400, json!({"error": "invalid Scaleway region"}));
    }
    let Ok(secret) = credential(env, &pin).await else {
        return json_answer(
            409,
            json!({"error": "the approved Scaleway credential is not bound"}),
        );
    };
    if scaleway_ingest::check_project(&secret, &scaleway::zone(&region), &pin.project)
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

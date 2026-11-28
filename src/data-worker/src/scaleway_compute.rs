//! Instance-owned Scaleway compute broker. Its provider credential is never returned.
use pigeon_cloud::scaleway_ingest;
use pigeon_core::secrets_store;
use pigeon_ingest::{
    protocol,
    scaleway::{
        self, BROKER_PATH, BrokerLaunch, BrokerRun, BrokerStatus, ComputeCommand, ComputePin,
    },
};
use serde_json::json;
use worker::{Conditional, Env, Request, Response, Result};

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
    let Some(body) = bounded_body(req, 12 * 1024).await? else {
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
        ComputeCommand::Launch { launch: request } => {
            let origin = req.url()?.origin().ascii_serialization();
            launch(env, *request, &origin, key.as_bytes()).await
        }
        ComputeCommand::Status { run } => observe(env, &run, None).await,
        ComputeCommand::Cleanup {
            run,
            absent_after_ms,
        } => observe(env, &run, Some(absent_after_ms)).await,
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

async fn launch(env: &Env, request: BrokerLaunch, origin: &str, key: &[u8]) -> Result<Response> {
    use pigeon_ingest::{
        container_job, managed_destination, manifest::SourceSpec, source_credentials,
    };

    let instance = env.var("INSTANCE")?.to_string();
    let now = now_ms();
    let job = &request.job;
    let run = &job.run;
    if job.instance != instance
        || !container_job::valid_run(run)
        || !container_job::valid_revision(&job.commit)
        || !job.keep_archives
        || !scaleway::valid_region(&request.region)
        || !managed_destination::valid_origin(origin)
        || request.source.origin != origin
        || request.destination.origin != origin
        || request.deadline_ms <= now
        || request.deadline_ms.saturating_sub(now) > 24 * 60 * 60_000
    {
        return Err(fail("invalid owned Scaleway launch scope"));
    }
    let source = source_credentials::authenticate(&request.source.grant, key, &instance, now)
        .filter(|grant| grant.run == *run && grant.expires_ms == request.deadline_ms)
        .ok_or_else(|| fail("the Scaleway source grant does not match its run"))?;
    managed_destination::verify(&request.destination.grant, key, &instance, now)
        .filter(|grant| {
            grant.run == *run
                && grant.dry_run == job.dry_run
                && grant.expires_ms >= request.deadline_ms
        })
        .ok_or_else(|| fail("the Scaleway destination grant does not match its run"))?;
    let (bound, _) = crate::source_credentials::open(env, &source.reference).await?;
    let SourceSpec::S3 {
        endpoint,
        region,
        bucket,
        prefix,
        ..
    } = &job.source
    else {
        return Err(fail("Scaleway needs an approved bucket source"));
    };
    if bound.provider != "scaleway"
        || bound.owner.as_deref() != Some(request.pin.project.as_str())
        || region != &request.region
        || region != &bound.region
        || bucket != &bound.bucket
        || bound.endpoint().as_ref() != Some(endpoint)
        || prefix != &source.prefix
        || !source.permits(&bound, prefix)
    {
        return Err(fail("the approved Scaleway source scope changed"));
    }
    let secret = credential(env, &request.pin).await?;
    let zone = scaleway::zone(&request.region);
    let data = env.bucket("DATA")?;
    let marker = scaleway::broker_run_key(run);
    let mut owned = BrokerRun {
        run: run.clone(),
        pin: request.pin,
        region: request.region,
        deadline_ms: request.deadline_ms,
        server: None,
        volumes: Vec::new(),
    };
    // A unique durable record admits only one provider create, even if delivery is replayed.
    let claimed = data
        .put(&marker, serde_json::to_vec(&owned)?)
        .only_if(Conditional {
            etag_does_not_match: Some("*".into()),
            ..Default::default()
        })
        .execute()
        .await?;
    if claimed.is_none() {
        return json_answer(
            409,
            json!({"error": "this Scaleway run was already admitted"}),
        );
    }
    let resources = scaleway_ingest::scw_create(
        &secret,
        &zone,
        &scaleway::scaleway_server_body(job, &owned.pin.project),
    )
    .await?;
    owned.server = Some(resources.server.clone());
    owned.volumes = resources.volumes;
    data.put(&marker, serde_json::to_vec(&owned)?)
        .execute()
        .await?;
    let scratch = resources
        .scratch_volume
        .as_deref()
        .ok_or_else(|| fail("missing approved Scaleway scratch volume"))?;
    let source_json = serde_json::to_string(&request.source)?;
    let destination_json = serde_json::to_string(&request.destination)?;
    let bootstrap = scaleway::owned_bootstrap(job, scratch, &source_json, &destination_json);
    scaleway_ingest::scw_user_data(&secret, &zone, &resources.server, &bootstrap).await?;
    scaleway_ingest::scw_action(
        &secret,
        &zone,
        &resources.server,
        &scaleway::scaleway_poweron(),
    )
    .await?;
    json_answer(
        200,
        serde_json::to_value(BrokerStatus {
            run: run.clone(),
            server: owned.server,
            state: None,
            volumes: owned.volumes,
            gone: false,
        })?,
    )
}

async fn observe(env: &Env, run: &str, cleanup_after: Option<i64>) -> Result<Response> {
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
            200,
            serde_json::to_value(BrokerStatus {
                run: run.into(),
                server: None,
                state: None,
                volumes: Vec::new(),
                gone: cleanup_after.is_some_and(|deadline| now_ms() >= deadline),
            })?,
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
    let gone =
        if cleanup_after.is_some() && (owned.server.is_some() || now_ms() >= owned.deadline_ms) {
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

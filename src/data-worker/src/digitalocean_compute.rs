//! Executes DigitalOcean operations with a secret held by the instance's account.

use pigeon_cloud::{
    compute::probe_digitalocean,
    digitalocean_compute::{Command, Lookup, PATH, Pin, Request as ComputeRequest},
    digitalocean_ingest as provider,
    transport::FetchTransport,
};
use pigeon_core::secrets_store;
use pigeon_ingest::protocol;
use serde_json::json;
use worker::{Conditional, Env, Request, Response, Result};

use crate::{fail, json_answer, managed_dest::bounded_body, now_ms};

async fn credential(env: &Env, pin: &Pin) -> Result<String> {
    let instance = env.var("INSTANCE")?.to_string();
    let binding = pin
        .binding(&instance)
        .ok_or_else(|| fail("invalid DigitalOcean compute credential pin"))?;
    let store = env
        .secret_store(&binding)
        .map_err(|_| fail("the selected DigitalOcean credential generation is not bound"))?;
    let value = secrets_store::read(&store)
        .await
        .map_err(|_| fail("the selected DigitalOcean credential is unavailable"))?
        .ok_or_else(|| fail("the selected DigitalOcean credential is unavailable"))?;
    probe_digitalocean(&FetchTransport, &pin.team, &value).await?;
    Ok(value)
}

struct Owner<'a> {
    env: &'a Env,
    pin: Pin,
}

impl Owner<'_> {
    fn marker(name: &str, resource: &str) -> String {
        format!("compute/digitalocean/creates/{name}/{resource}")
    }

    fn identity(&self, region: &str, fenced: bool) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&(&self.pin, region, fenced))?)
    }

    async fn put_once(&self, marker: &str, region: &str, fenced: bool) -> Result<bool> {
        let claimed = self
            .env
            .bucket("DATA")?
            .put(marker, self.identity(region, fenced)?)
            .only_if(Conditional {
                etag_does_not_match: Some("*".into()),
                ..Default::default()
            })
            .execute()
            .await?;
        Ok(claimed.is_some())
    }

    async fn attempted(&self, marker: &str, region: &str) -> Result<Option<bool>> {
        let object = self.env.bucket("DATA")?.get(marker).execute().await?;
        let Some(body) = object.as_ref().and_then(|marker| marker.body()) else {
            return Ok(None);
        };
        let value = body.bytes().await?;
        if value == self.identity(region, false)? {
            return Ok(Some(true));
        }
        if value == self.identity(region, true)? {
            return Ok(Some(false));
        }
        Err(fail(
            "the DigitalOcean run belongs to a different credential or region",
        ))
    }

    async fn claim(&self, name: &str, resource: &str, region: &str) -> Result<bool> {
        self.put_once(&Self::marker(name, resource), region, false)
            .await
    }

    async fn require(&self, name: &str, resource: &str, region: &str) -> Result<()> {
        let marker = Self::marker(name, resource);
        if self.attempted(&marker, region).await? != Some(true) {
            return Err(fail("this DigitalOcean create attempt was not recorded"));
        }
        Ok(())
    }

    async fn lookup<T>(
        &self,
        name: &str,
        resource: &str,
        region: &str,
        fence: bool,
        lookup: impl std::future::Future<Output = Result<Option<T>>>,
    ) -> Result<Lookup<T>> {
        let marker = Self::marker(name, resource);
        // A late create cannot claim the same slot once terminal cleanup fences it.
        if fence && self.put_once(&marker, region, true).await? {
            return Ok(Lookup::NeverCreated);
        }
        match self.attempted(&marker, region).await? {
            Some(true) => Ok(lookup.await?.into()),
            Some(false) => Ok(Lookup::NeverCreated),
            None => Ok(Lookup::Unresolved),
        }
    }
}

pub async fn fetch(req: &mut Request, env: &Env) -> Result<Response> {
    let key = env.secret("DATA_KEY")?.to_string();
    let signature = req.headers().get(protocol::SIGNATURE_HEADER)?;
    let Some(body) = bounded_body(req, 48 * 1024).await? else {
        return json_answer(413, json!({ "error": "compute request too large" }));
    };
    if !signature.is_some_and(|signature| {
        protocol::verify(&signature, "POST", PATH, &body, key.as_bytes(), now_ms()).is_ok()
    }) {
        return json_answer(401, json!({ "error": "unauthorized" }));
    }
    let Ok(ComputeRequest { pin, command }) = serde_json::from_slice(&body) else {
        return json_answer(
            400,
            json!({ "error": "invalid DigitalOcean compute command" }),
        );
    };
    let Ok(token) = credential(env, &pin).await else {
        return json_answer(
            409,
            json!({ "error": "the selected credential cannot verify the approved DigitalOcean team" }),
        );
    };
    let owner = Owner { env, pin };
    let response = match command {
        Command::Check { region } => {
            let target = provider::Target::new("00000000000000000000000000000000", &region)?;
            provider::find_volume(&FetchTransport, &token, &target).await?;
            json!(owner.pin)
        }
        Command::CreateVolume { run, region, gib } => {
            let target = provider::Target::new(&run, &region)?;
            let result = if owner.claim(target.name(), "volume", &region).await? {
                provider::create_volume(&FetchTransport, &token, &target, gib).await
            } else {
                provider::Created::Unknown
            };
            json!(result)
        }
        Command::FindVolume { run, region, fence } => {
            let target = provider::Target::new(&run, &region)?;
            json!(
                owner
                    .lookup(
                        target.name(),
                        "volume",
                        &region,
                        fence,
                        provider::find_volume(&FetchTransport, &token, &target),
                    )
                    .await?
            )
        }
        Command::CreateDroplet {
            run,
            region,
            volume,
            script,
        } => {
            let target = provider::Target::new(&run, &region)?;
            owner.require(target.name(), "volume", &region).await?;
            let owned = provider::find_volume(&FetchTransport, &token, &target).await?;
            let result = if owned.as_deref() != Some(volume.as_str()) {
                provider::Created::Rejected(409)
            } else if owner.claim(target.name(), "droplet", &region).await? {
                provider::create_droplet(&FetchTransport, &token, &target, &volume, &script).await
            } else {
                provider::Created::Unknown
            };
            json!(result)
        }
        Command::FindDroplet { run, region, fence } => {
            let target = provider::Target::new(&run, &region)?;
            json!(
                owner
                    .lookup(
                        target.name(),
                        "droplet",
                        &region,
                        fence,
                        provider::find_droplet(&FetchTransport, &token, &target),
                    )
                    .await?
            )
        }
        Command::DropletIp { run, region, id } => {
            let target = provider::Target::new(&run, &region)?;
            owner.require(target.name(), "droplet", &region).await?;
            json!(provider::public_ipv4(&FetchTransport, &token, &target, id).await?)
        }
        Command::RemoveDroplet { run, region, id } => {
            let target = provider::Target::new(&run, &region)?;
            owner.require(target.name(), "droplet", &region).await?;
            json!(provider::remove_droplet(&FetchTransport, &token, &target, id).await?)
        }
        Command::RemoveVolume { run, region, id } => {
            let target = provider::Target::new(&run, &region)?;
            owner.require(target.name(), "volume", &region).await?;
            json!(provider::remove_volume(&FetchTransport, &token, &target, &id).await?)
        }
    };
    json_answer(200, response)
}

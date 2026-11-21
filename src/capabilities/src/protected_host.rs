//! A hostname served by a Worker behind Cloudflare Access, letting in only service tokens: the
//! host's own, kept in the Secrets Store, and guests for callers outside the account, whose
//! secret is answered once, when created, and never stored.

use pigeon_core::access::{ServiceToken, Session};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::Writer;
use pigeon_core::zones::{self, Zone};
use pigeon_core::{Account, custom_domain};
use serde::Serialize;
use serde_json::Value;
use worker::Result;

use crate::{Call, Capability, Context, Footprint, HostFootprint, to_value, unknown};

/// A protected hostname.
pub struct ProtectedHost {
    pub zone: String,
    pub host: String,
    /// The Worker serving it.
    pub worker: String,
    /// The Access application's name, also its policy's.
    pub app: String,
    /// The host's own service token, and the Secrets Store names of its client ID and secret.
    pub service_token: String,
    pub client_id_secret: String,
    pub client_secret_secret: String,
    /// More service tokens let in, by name.
    pub guests: Vec<String>,
    /// A header Access also reads a service token from, as one JSON value.
    pub token_header: Option<String>,
    /// Paths on the host Access lets everyone through, such as `/auth`, for pages that check
    /// callers themselves.
    pub public_paths: Vec<String>,
    /// The tool that sets it up, such as `setup_platform`.
    pub tool: &'static str,
}

/// What setting it up made.
#[derive(Serialize)]
struct Served {
    zone: Zone,
    host: String,
    access_app: String,
    client_id: String,
    secrets: [String; 2],
    guests: Vec<ServiceToken>,
    custom_domain: String,
}

impl ProtectedHost {
    /// In order, so the hostname is never served before Access holds it: the zone is active,
    /// Access protects the host, then the host is attached to the Worker.
    async fn serve(&self, account: &Account) -> Result<Served> {
        let zone = zones::active(account, &self.zone).await?;
        let session = Session::open(account).await?;
        let protected = self.protect(account, &session).await;
        let (access_app, own, guests) = session.finish(protected).await?;
        let custom_domain =
            custom_domain::attach(account, &zone.id, &self.host, &self.worker).await?;
        Ok(Served {
            zone,
            host: self.host.clone(),
            access_app,
            client_id: own.client_id,
            secrets: [
                self.client_id_secret.clone(),
                self.client_secret_secret.clone(),
            ],
            guests,
            custom_domain,
        })
    }

    async fn protect(
        &self,
        account: &Account,
        s: &Session<'_>,
    ) -> Result<(String, ServiceToken, Vec<ServiceToken>)> {
        let own = s.service_token(&self.service_token).await?;
        let writer = Writer::open(account).await?;
        let stored = self.store(&writer, s, &own).await;
        writer.finish(stored).await?;
        let mut ids = vec![own.id.clone()];
        let mut guests = Vec::new();
        for name in &self.guests {
            let guest = s.service_token(name).await?;
            ids.push(guest.id.clone());
            guests.push(guest);
        }
        let policy = s.policy(&self.app, &ids).await?;
        let app = s
            .app(&self.app, &self.host, &policy, self.token_header.as_deref())
            .await?;
        if !self.public_paths.is_empty() {
            let bypass = s.bypass_policy(&format!("{}-public", self.app)).await?;
            for path in &self.public_paths {
                let domain = format!("{}{path}", self.host);
                s.app(&format!("{}{path}", self.app), &domain, &bypass, None)
                    .await?;
            }
        }
        Ok((app, own, guests))
    }

    /// Keeps the host's token in the Secrets Store. A token whose secret the store lacks is
    /// rotated, since its secret cannot be read back.
    async fn store(&self, writer: &Writer<'_>, s: &Session<'_>, own: &ServiceToken) -> Result<()> {
        let comment = format!("Access service token `{}`", self.service_token);
        let secret = match &own.client_secret {
            Some(secret) => Some(secret.clone()),
            None if writer
                .list()
                .await?
                .iter()
                .any(|x| x.name == self.client_secret_secret) =>
            {
                None
            }
            None => Some(s.rotate(&own.id).await?),
        };
        writer
            .put(&self.client_id_secret, &own.client_id, &comment)
            .await?;
        match secret {
            Some(secret) => {
                writer
                    .put(&self.client_secret_secret, &secret, &comment)
                    .await
            }
            None => Ok(()),
        }
    }
}

impl Capability for ProtectedHost {
    fn name(&self) -> &'static str {
        "protected_host"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool::bare(
            self.tool,
            "Make sure the protected hostname is served by its Worker behind Access, admitting \
             only its service tokens.",
        )]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != self.tool {
                return Err(unknown(tool));
            }
            Ok(to_value(self.serve(cx.account).await?))
        })
    }

    fn footprint(&self) -> Footprint {
        let mut access = vec![self.service_token.clone()];
        access.extend(self.guests.iter().cloned());
        access.sort();
        Footprint {
            hostnames: vec![HostFootprint {
                host: self.host.clone(),
                worker: self.worker.clone(),
                by: self.name().into(),
                access,
                public_paths: self.public_paths.clone(),
            }],
            zone: Some(self.zone.clone()),
            secrets: vec![
                self.client_id_secret.clone(),
                self.client_secret_secret.clone(),
            ],
            ..Footprint::default()
        }
    }
}

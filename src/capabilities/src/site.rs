//! The public site on a zone's apex, served by the hosting Worker itself, by host: the
//! architecture page and its model, rendered once per isolate from the sources the Worker was
//! built with, so each deploy serves its own. `site_publish` moves the apex onto this Worker;
//! `site_retire` deletes the Worker that served it before.

use std::sync::OnceLock;

use pigeon_core::access::Session;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::Writer;
use pigeon_core::{Account, custom_domain, design, workers, zones};
use serde_json::{Value, json};
use worker::{Error, Headers, Request, Response, Result};

use crate::architecture::{self, Source};
use crate::{Call, Capability, Context, Footprint, HostFootprint, Hosted, to_value, unknown};

/// The site.
pub struct Site {
    /// The apex, such as `pigeon.dev`.
    pub zone: String,
    /// The Worker serving it: the hosting Worker itself.
    pub worker: String,
    /// A Worker that used to serve the apex, deleted by `site_retire`.
    pub old_worker: Option<String>,
    /// An Access setup the apex had, removed when the site is published.
    pub retire: Option<Retired>,
}

/// Names of an Access setup to remove: its application and policy, its service token, and the
/// Secrets Store names of the token's client ID and secret.
pub struct Retired {
    pub app: String,
    pub service_token: String,
    pub secrets: [String; 2],
}

/// The site's pages, by path.
pub const PAGES: [&str; 2] = ["/", "/architecture.json"];

/// The pages, rendered once per isolate: the HTML and the JSON model.
static RENDERED: OnceLock<(String, String)> = OnceLock::new();

/// Whether `host` is the apex `apex`, ignoring ASCII case.
pub fn is_apex(host: &str, apex: &str) -> bool {
    host.eq_ignore_ascii_case(apex)
}

/// The index in [`PAGES`] of the page answering `method path` on the apex: `GET` or `HEAD` of a
/// page; `None` otherwise.
pub fn page(method: &str, path: &str) -> Option<usize> {
    if method != "GET" && method != "HEAD" {
        return None;
    }
    PAGES.iter().position(|p| *p == path)
}

/// Renders the page and the model from the sources the Worker was built with.
fn render(hosted: &Hosted<'_>) -> Result<(String, String)> {
    let sources: Vec<Source> = hosted
        .sources
        .iter()
        .map(|(path, text)| Source { path, text })
        .collect();
    let model = architecture::describe(&sources, hosted);
    let html = architecture::page(&model);
    let json = serde_json::to_string_pretty(&model)?;
    Ok((html, json))
}

impl Site {
    /// Serves a request to the apex: a page, or 404.
    pub async fn serve(&self, hosted: &Hosted<'_>, req: Request) -> Result<Response> {
        let method = req.method().to_string();
        let Some(index) = page(&method, &req.path()) else {
            return Response::error("not found", 404);
        };
        let rendered = match RENDERED.get() {
            Some(r) => r,
            None => {
                let r = render(hosted)?;
                RENDERED.get_or_init(|| r)
            }
        };
        let (body, content_type) = if index == 0 {
            (&rendered.0, "text/html; charset=utf-8")
        } else {
            (&rendered.1, "application/json")
        };
        let headers = Headers::new();
        headers.set("Content-Type", content_type)?;
        headers.set("Content-Security-Policy", design::CSP)?;
        headers.set("X-Content-Type-Options", "nosniff")?;
        headers.set("Referrer-Policy", "no-referrer")?;
        headers.set("X-Served-By", &self.worker)?;
        let response = if method == "HEAD" {
            Response::empty()?
        } else {
            Response::ok(body.as_str())?
        };
        Ok(response.with_headers(headers))
    }

    /// Moves the apex's custom domain onto this Worker, then retires the old Access setup.
    async fn publish(&self, cx: &Context<'_>) -> Result<Value> {
        let zone = zones::active(cx.account, &self.zone).await?;
        let custom_domain =
            custom_domain::attach(cx.account, &zone.id, &zone.name, &self.worker).await?;
        let retired = match &self.retire {
            Some(r) => Some(retire(cx.account, &zone.name, r).await?),
            None => None,
        };
        Ok(json!({
            "worker": self.worker,
            "host": zone.name,
            "pages": PAGES,
            "custom_domain": custom_domain,
            "retired": retired,
        }))
    }

    /// Deletes Worker `old`, but only once the apex's custom domain is on this Worker.
    async fn retire_worker(&self, cx: &Context<'_>, old: &str) -> Result<Value> {
        let serving = custom_domain::serving(cx.account, &self.zone).await?;
        if serving.as_deref() != Some(self.worker.as_str()) {
            return Err(Error::RustError(format!(
                "{} is served by {serving:?}, not {}: run site_publish first",
                self.zone, self.worker
            )));
        }
        let deleted = workers::delete(cx.account, old).await?;
        Ok(json!({ "old_worker": old, "deleted": deleted, "serving": serving }))
    }
}

/// Takes the apex out from behind Access and removes what `r` names; each is skipped when it
/// is already gone. The application goes first, then every policy holding the token, then the
/// token, then its secrets.
async fn retire(account: &Account, apex: &str, r: &Retired) -> Result<Value> {
    let session = Session::open(account).await?;
    let removed = async {
        let app = session.remove_app(&r.app, apex).await?;
        let token = session.find_service_token(&r.service_token).await?;
        let policies = session.remove_policies(&r.app, token.as_deref()).await?;
        if let Some(id) = &token {
            session.remove_service_token(id).await?;
        }
        Ok(json!({ "access_app": app, "policies": policies, "service_token": token }))
    }
    .await;
    let mut removed = session.finish(removed).await?;
    let writer = Writer::open(account).await?;
    let secrets = async {
        let mut gone = Vec::new();
        for secret in writer.list().await? {
            if r.secrets.contains(&secret.name) {
                writer.delete(&secret.id).await?;
                gone.push(secret.name);
            }
        }
        Ok(gone)
    }
    .await;
    removed["secrets"] = json!(writer.finish(secrets).await?);
    Ok(removed)
}

impl Capability for Site {
    fn name(&self) -> &'static str {
        "site"
    }

    fn tools(&self) -> Vec<Tool> {
        let mut tools = vec![Tool::bare(
            "site_publish",
            "Serve the public site on the zone's apex from this Worker, moving the apex's custom \
             domain onto it, and retire the apex's old Access setup.",
        )];
        if self.old_worker.is_some() {
            tools.push(Tool::bare(
                "site_retire",
                "Delete the Worker that used to serve the apex, once the apex's custom domain is \
                 on this Worker; refuses otherwise.",
            ));
        }
        tools
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match (tool, &self.old_worker) {
                ("site_publish", _) => Ok(to_value(self.publish(cx).await?)),
                ("site_retire", Some(old)) => Ok(to_value(self.retire_worker(cx, old).await?)),
                _ => Err(unknown(tool)),
            }
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            hostnames: vec![HostFootprint {
                host: self.zone.clone(),
                worker: self.worker.clone(),
                by: self.name().into(),
                access: Vec::new(),
                public_paths: Vec::new(),
            }],
            zone: Some(self.zone.clone()),
            ..Footprint::default()
        }
    }
}

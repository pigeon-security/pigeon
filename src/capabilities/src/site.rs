//! The public site on a zone's apex, served by the hosting Worker itself, by host: the
//! architecture page and its model, rendered once per isolate from the sources the Worker was
//! built with, so each deploy serves its own. `site_publish` moves the apex onto this Worker.

use std::sync::OnceLock;

use pigeon_core::mcp::Tool;
use pigeon_core::{custom_domain, design, zones};
use serde_json::{Value, json};
use worker::{Headers, Request, Response, Result};

use crate::architecture;
use crate::{Call, Capability, Context, Footprint, HostFootprint, Hosted, to_value, unknown};

/// The site.
pub struct Site {
    /// The apex, such as `pigeon.dev`.
    pub zone: String,
    /// The Worker serving it: the hosting Worker itself.
    pub worker: String,
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

/// The page and the model, from the architecture the Worker was built with: the same one
/// `architecture_describe` and the console's Architecture tab show.
fn render(hosted: &Hosted<'_>) -> Result<(String, String)> {
    let built = architecture::built(hosted);
    let json = serde_json::to_string_pretty(&built.model)?;
    Ok((built.page.clone(), json))
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

    /// Moves the apex's custom domain onto this Worker. It touches no Access setup.
    async fn publish(&self, cx: &Context<'_>) -> Result<Value> {
        let zone = zones::active(cx.account, &self.zone).await?;
        let custom_domain =
            custom_domain::attach(cx.account, &zone.id, &zone.name, &self.worker).await?;
        Ok(json!({
            "worker": self.worker,
            "host": zone.name,
            "pages": PAGES,
            "custom_domain": custom_domain,
        }))
    }
}

impl Capability for Site {
    fn name(&self) -> &'static str {
        "site"
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool::bare(
            "site_publish",
            "Serve the public site on the zone's apex from this Worker, moving the apex's custom \
             domain onto it. Changes no Access setup.",
        )]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, _args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != "site_publish" {
                return Err(unknown(tool));
            }
            Ok(to_value(self.publish(cx).await?))
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

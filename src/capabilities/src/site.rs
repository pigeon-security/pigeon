//! The public site on a zone's apex, served by the hosting Worker itself, by host: the home
//! page, the architecture page, its model and their images ([`pigeon_core::home`], the same
//! pages pigeon-sites serves), rendered once per isolate from the sources the Worker was built
//! with, so each deploy serves its own. `site_publish` moves the apex onto this Worker.

use std::sync::OnceLock;

use pigeon_core::home::{self, Route};
use pigeon_core::mcp::Tool;
use pigeon_core::{custom_domain, zones};
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
    /// Other Workers the apex may move to, such as `pigeon-sites`.
    pub also: Vec<String>,
    /// Each of those Workers with its service binding's name in this Worker.
    pub bindings: Vec<(String, String)>,
}

/// The site's pages, by path; each image of [`home::Asset`] is served besides.
pub const PAGES: [&str; 3] = ["/", "/architecture", "/architecture.json"];

/// The pages, rendered once per isolate: the home page, the architecture page and the JSON
/// model.
static RENDERED: OnceLock<[String; 3]> = OnceLock::new();

/// Whether `host` is the apex `apex`, ignoring ASCII case.
pub fn is_apex(host: &str, apex: &str) -> bool {
    host.eq_ignore_ascii_case(apex)
}

/// What answers `method path` on the apex: `GET` or `HEAD` of a page or an image; `None`
/// otherwise. `/logo.png` (pigeon-sites' iOS app icon) is not served here.
pub fn page(method: &str, path: &str) -> Option<Route> {
    match home::route(method, path) {
        r @ (Route::Home | Route::Architecture | Route::Json | Route::Asset(_)) => Some(r),
        Route::Logo | Route::NotFound | Route::MethodNotAllowed => None,
    }
}

/// The pages, all from the public view of the architecture the Worker was built with: no
/// Access token names, permissions, repositories or paths. The JSON has `commit` at the top
/// level, as ci/deploy/deploy.sh reads it.
fn render(hosted: &Hosted<'_>) -> Result<[String; 3]> {
    let built = architecture::built(hosted);
    let build = home::Build {
        commit: built.public_model["commit"].as_str().unwrap_or_default(),
        digest: &built.digest,
    };
    let body = json!({
        "commit": built.public_model["commit"],
        "digest": built.digest,
        "model": built.public_model,
    });
    let json = serde_json::to_string_pretty(&body)?;
    Ok([
        home::home_html(Some(build)),
        home::architecture_html(&built.public_svg, build),
        json,
    ])
}

impl Site {
    /// Serves a request to the apex: a page, an image, or 404.
    pub async fn serve(&self, hosted: &Hosted<'_>, req: Request) -> Result<Response> {
        let method = req.method().to_string();
        let Some(route) = page(&method, &req.path()) else {
            return Response::error("not found", 404);
        };
        let rendered = match RENDERED.get() {
            Some(r) => r,
            None => {
                let r = render(hosted)?;
                RENDERED.get_or_init(|| r)
            }
        };
        let html = "text/html; charset=utf-8";
        let (body, content_type, cache): (&[u8], &str, Option<&str>) = match route {
            Route::Asset(a) => (a.bytes(), a.content_type(), Some(home::ASSET_CACHE)),
            Route::Architecture => (rendered[1].as_bytes(), html, None),
            Route::Json => (rendered[2].as_bytes(), "application/json", None),
            _ => (rendered[0].as_bytes(), html, None),
        };
        let headers = Headers::new();
        headers.set("Content-Type", content_type)?;
        headers.set("Content-Security-Policy", home::CSP)?;
        headers.set("X-Content-Type-Options", "nosniff")?;
        headers.set("Referrer-Policy", "no-referrer")?;
        headers.set("X-Served-By", &self.worker)?;
        if let Some(cache) = cache {
            headers.set("Cache-Control", cache)?;
        }
        let response = if method == "HEAD" {
            Response::empty()?
        } else {
            Response::from_bytes(body.to_vec())?
        };
        Ok(response.with_headers(headers))
    }

    /// Moves the apex's custom domain onto `target`. A Worker other than this one is asked for
    /// the three pages first, over its service binding, and the move is refused unless they
    /// answer. It touches no Access setup.
    async fn publish(&self, cx: &Context<'_>, target: &str) -> Result<Value> {
        let mut checked: Vec<String> = Vec::new();
        if target != self.worker {
            let binding = self
                .bindings
                .iter()
                .find(|(w, _)| w == target)
                .map(|(_, b)| b.as_str())
                .ok_or_else(|| {
                    worker::Error::RustError(format!("no service binding to Worker `{target}`"))
                })?;
            let fetcher = cx.env.service(binding)?;
            for path in PAGES {
                let url = format!("https://{}{path}", self.zone);
                let mut answer = fetcher.fetch(url.clone(), None).await.map_err(|e| {
                    worker::Error::RustError(format!("{target} did not answer {url}: {e}"))
                })?;
                let status = answer.status_code();
                if status != 200 {
                    return Err(worker::Error::RustError(format!(
                        "{target} answered {status} for {url}; not moving {}",
                        self.zone
                    )));
                }
                if path == "/architecture.json" {
                    let body: Value = serde_json::from_str(&answer.text().await?)?;
                    if !body["commit"].as_str().is_some_and(is_commit) {
                        return Err(worker::Error::RustError(format!(
                            "{target}'s {url} names no 40-character commit; not moving {}",
                            self.zone
                        )));
                    }
                }
                checked.push(path.to_string());
            }
        }
        let zone = zones::active(cx.account, &self.zone).await?;
        let custom_domain = custom_domain::attach(cx.account, &zone.id, &zone.name, target).await?;
        Ok(json!({
            "worker": target,
            "host": zone.name,
            "pages": if checked.is_empty() { json!(PAGES) } else { json!(checked) },
            "checked": checked,
            "custom_domain": custom_domain,
        }))
    }
}

/// Whether `s` is a full commit id: 40 lowercase hex digits.
fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Capability for Site {
    fn name(&self) -> &'static str {
        "site"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "site",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        let mut workers = vec![self.worker.clone()];
        workers.extend(self.also.iter().cloned());
        vec![Tool {
            name: "site_publish",
            description: "Serve the public site on the zone's apex from a Worker, moving the \
                          apex's custom domain onto it. Another Worker is checked first: it must \
                          answer /, /architecture and /architecture.json. Changes no Access setup.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "worker": {
                        "type": "string",
                        "enum": workers,
                        "description": "The Worker to serve the apex; this Worker when not given.",
                    }
                }
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != "site_publish" {
                return Err(unknown(tool));
            }
            let target = match args.get("worker").and_then(Value::as_str) {
                None => self.worker.as_str(),
                Some(w) if w == self.worker || self.also.iter().any(|a| a == w) => w,
                Some(w) => {
                    return Err(worker::Error::RustError(format!(
                        "{} cannot be served by `{w}`",
                        self.zone
                    )));
                }
            };
            Ok(to_value(self.publish(cx, target).await?))
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

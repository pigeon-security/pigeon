//! The public site on a zone's apex, served by the hosting Worker itself, by host
//! (docs/plans/platform-self-hosting.md, S1). In order:
//!
//! 1. `/platform*` answers a 302 to the console ([`home::console_redirect`]);
//! 2. the architecture page, its JSON model and the images ([`pigeon_core::home`]) are built
//!    into the Worker, rendered once per isolate from the sources it was built with, so each
//!    deploy serves its own and `/architecture.json` always names the deployed commit;
//! 3. every other `GET` or `HEAD` is the home instance's published landing release, read from
//!    its bucket over the Worker's R2 binding [`HOME_SITE`] (no token), as `site_worker.js`
//!    serves a linked instance's ([`site_publish::live`], [`site_publish::pick`]), with the
//!    release's own 404 page;
//! 4. with no release, a read failure, or a file that is not script-free, today's answer: the
//!    built-in home page for `/` ([`home::home_html`]), 404 otherwise.
//!
//! Every answer carries [`home::CSP`], which allows no script. `site_publish` moves the apex's
//! custom domain onto this Worker.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use pigeon_core::home::{self, Route};
use pigeon_core::mcp::Tool;
use pigeon_core::site_publish::{self, Live};
use pigeon_core::{custom_domain, r2, zones};
use serde_json::{Value, json};
use worker::{Env, Headers, Request, Response, Result, console_error};

use crate::architecture;
use crate::{Call, Capability, Context, Footprint, HostFootprint, Hosted, to_value, unknown};

/// The R2 binding, in core's wrangler.toml, of the home instance's landing bucket
/// (`pigeon-platform-site`), read only, whose live release the apex serves.
pub const HOME_SITE: &str = "HOME_SITE";

/// How long an isolate keeps what `live` names before reading it again, as `site_worker.js`.
const RECHECK_MS: f64 = 30_000.0;

thread_local! {
    /// When `live` was last read, and the release it named then.
    static LIVE: RefCell<Option<(f64, Option<Rc<Live>>)>> = const { RefCell::new(None) };
}

/// The site.
pub struct Site {
    /// The apex, such as `pigeon.dev`.
    pub zone: String,
    /// The Worker serving it: the hosting Worker itself.
    pub worker: String,
    /// Other Workers the apex may move to; none once this Worker serves the release.
    pub also: Vec<String>,
    /// Each of those Workers with its service binding's name in this Worker.
    pub bindings: Vec<(String, String)>,
    /// The R2 binding whose live release the apex serves ([`HOME_SITE`]); `None` serves the
    /// built-in pages only.
    pub release: Option<String>,
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

/// Who answers a request to the apex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Apex {
    /// `/platform*`: a 302 to this URL of the console.
    Console(String),
    /// Built into the Worker, answered before any release: the architecture page, its JSON and
    /// the images.
    Built(Route),
    /// Any other `GET` or `HEAD`: the published release; failing that, the built-in home page
    /// for `/` and 404 otherwise.
    Release,
    /// Anything else: 404.
    NotFound,
}

/// Who answers `method path?query` on the apex, in order: the console redirect, the built-in
/// dynamic routes, then the release. Pure.
pub fn apex(method: &str, path: &str, query: Option<&str>) -> Apex {
    if let Some(to) = home::console_redirect(path, query) {
        return Apex::Console(to);
    }
    match home::route(method, path) {
        r @ (Route::Architecture | Route::Json | Route::Asset(_)) => Apex::Built(r),
        _ if method == "GET" || method == "HEAD" => Apex::Release,
        _ => Apex::NotFound,
    }
}

/// The headers every apex answer carries, with its `Content-Type` and `Cache-Control`: the
/// script-free [`home::CSP`] among them.
pub fn headers(
    content_type: &str,
    cache: Option<&str>,
    worker: &str,
) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("Content-Type", content_type.to_string()),
        ("Content-Security-Policy", home::CSP.to_string()),
        ("X-Content-Type-Options", "nosniff".to_string()),
        ("Referrer-Policy", "no-referrer".to_string()),
        ("X-Served-By", worker.to_string()),
    ];
    if let Some(c) = cache {
        h.push(("Cache-Control", c.to_string()));
    }
    h
}

/// What the live release answers, before any blob is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromRelease<'a> {
    /// The file `blob`, with `status` (200, or 404 for the release's 404 page) and `headers`.
    File {
        status: u16,
        blob: &'a str,
        content_type: &'a str,
        headers: Vec<(&'static str, String)>,
    },
    /// 304: the client's copy is this file.
    NotModified {
        headers: Vec<(&'static str, String)>,
    },
    /// The release cannot answer: a bad path, no such file and no 404 page, a script, or `/`
    /// missing from it. The built-in answer stands.
    Fallback,
}

/// What `live` answers for the raw (percent-encoded) `path`, as `site_worker.js` does: the file
/// the path resolves to, else the release's 404 page; a 304 when `if_none_match` is the file's
/// ETag. Never a script type, and never a 404 page for `/`. Pure.
pub fn from_release<'a>(
    live: &'a Live,
    path: &str,
    if_none_match: Option<&str>,
    worker: &str,
) -> FromRelease<'a> {
    let Some(decoded) = site_publish::decode_path(path) else {
        return FromRelease::Fallback;
    };
    let Some((status, key, entry)) = site_publish::pick(&live.manifest, &decoded) else {
        return FromRelease::Fallback;
    };
    if (status != 200 && decoded == "/") || site_publish::is_script_type(&entry.content_type) {
        return FromRelease::Fallback;
    }
    let etag = site_publish::etag(entry);
    let mut h = headers(
        &entry.content_type,
        Some(site_publish::cache_control(key)),
        worker,
    );
    h.push(("ETag", etag.clone()));
    h.push(("X-Pigeon-Release", live.release.clone()));
    if let Some(c) = &live.commit {
        h.push(("X-Pigeon-Commit", c.clone()));
    }
    if status == 200 && if_none_match == Some(etag.as_str()) {
        return FromRelease::NotModified { headers: h };
    }
    FromRelease::File {
        status,
        blob: &entry.blob,
        content_type: &entry.content_type,
        headers: h,
    }
}

/// The bucket bound to this Worker, read through [`site_publish::Read`].
struct Bound<'a>(&'a worker::Bucket);

impl site_publish::Read for Bound<'_> {
    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.0.get(key).execute().await? {
            Some(object) => match object.body() {
                Some(body) => Ok(Some(body.bytes().await?)),
                None => Ok(Some(Vec::new())),
            },
            None => Ok(None),
        }
    }
}

/// The live release, read again at most every [`RECHECK_MS`] per isolate. A failed read keeps
/// the release last read, and is logged.
async fn current(bucket: &worker::Bucket) -> Option<Rc<Live>> {
    let now = worker::Date::now().as_millis() as f64;
    let kept = LIVE.with(|l| l.borrow().clone());
    if let Some((at, live)) = &kept
        && now - at < RECHECK_MS
    {
        return live.clone();
    }
    let known = kept.and_then(|(_, l)| l);
    let live = match site_publish::live(&Bound(bucket), known.as_deref()).await {
        Ok(Some(l)) if known.as_deref() == Some(&l) => known,
        Ok(l) => l.map(Rc::new),
        Err(e) => {
            console_error!("apex release: reading live: {e}");
            known
        }
    };
    LIVE.with(|l| *l.borrow_mut() = Some((now, live.clone())));
    live
}

/// Sets each of `h` on a fresh [`Headers`].
fn to_headers(h: &[(&'static str, String)]) -> Result<Headers> {
    let headers = Headers::new();
    for (k, v) in h {
        headers.set(k, v)?;
    }
    Ok(headers)
}

/// `/architecture.json`: the public model, with its `commit` at the top level and first, pretty
/// printed, as ci/deploy/deploy.sh and `site_publish` read it (`"commit": "<40 hex>"`). Pure.
pub fn architecture_json(public_model: &Value, digest: &str) -> Result<String> {
    let body = json!({
        "commit": public_model["commit"],
        "digest": digest,
        "model": public_model,
    });
    Ok(serde_json::to_string_pretty(&body)?)
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
    let json = architecture_json(&built.public_model, &built.digest)?;
    Ok([
        home::home_html(Some(build)),
        home::architecture_html(&built.public_svg, build),
        json,
    ])
}

/// The pages, rendered on the isolate's first need.
fn rendered(hosted: &Hosted<'_>) -> Result<&'static [String; 3]> {
    Ok(match RENDERED.get() {
        Some(r) => r,
        None => {
            let r = render(hosted)?;
            RENDERED.get_or_init(|| r)
        }
    })
}

impl Site {
    /// Serves a request to the apex, in the order of [`apex`].
    pub async fn serve(&self, env: &Env, hosted: &Hosted<'_>, req: Request) -> Result<Response> {
        let method = req.method().to_string();
        let head = method == "HEAD";
        let url = req.url()?;
        match apex(&method, url.path(), url.query()) {
            Apex::Console(to) => {
                let headers = Headers::new();
                headers.set("Location", &to)?;
                headers.set("Cache-Control", "no-store")?;
                headers.set("Referrer-Policy", "no-referrer")?;
                headers.set("X-Served-By", &self.worker)?;
                Ok(Response::empty()?.with_status(302).with_headers(headers))
            }
            Apex::NotFound => Response::error("not found", 404),
            Apex::Built(route) => self.built(hosted, route, head),
            Apex::Release => {
                let inm = req.headers().get("If-None-Match")?;
                match self
                    .release_answer(env, url.path(), inm.as_deref(), head)
                    .await
                {
                    Ok(Some(r)) => return Ok(r),
                    Ok(None) => {}
                    Err(e) => console_error!("apex release: {} {e}", url.path()),
                }
                if url.path() == "/" {
                    self.built(hosted, Route::Home, head)
                } else {
                    Response::error("not found", 404)
                }
            }
        }
    }

    /// A page or image built into the Worker.
    fn built(&self, hosted: &Hosted<'_>, route: Route, head: bool) -> Result<Response> {
        let rendered = rendered(hosted)?;
        let html = "text/html; charset=utf-8";
        let (body, content_type, cache): (&[u8], &str, Option<&str>) = match route {
            Route::Asset(a) => (a.bytes(), a.content_type(), Some(home::ASSET_CACHE)),
            Route::Architecture => (rendered[1].as_bytes(), html, None),
            Route::Json => (rendered[2].as_bytes(), "application/json", None),
            _ => (rendered[0].as_bytes(), html, None),
        };
        let response = if head {
            Response::empty()?
        } else {
            Response::from_bytes(body.to_vec())?
        };
        Ok(response.with_headers(to_headers(&headers(content_type, cache, &self.worker))?))
    }

    /// The live release's answer for `path`, or `None` when it has none ([`FromRelease::Fallback`],
    /// no binding, nothing live, a missing blob, or a file that is not script-free). Pages and
    /// other text are read whole and checked; images and fonts are streamed.
    async fn release_answer(
        &self,
        env: &Env,
        path: &str,
        if_none_match: Option<&str>,
        head: bool,
    ) -> Result<Option<Response>> {
        let Some(binding) = &self.release else {
            return Ok(None);
        };
        let Ok(bucket) = env.bucket(binding) else {
            return Ok(None);
        };
        let Some(live) = current(&bucket).await else {
            return Ok(None);
        };
        let (status, blob, content_type, h) =
            match from_release(&live, path, if_none_match, &self.worker) {
                FromRelease::Fallback => return Ok(None),
                FromRelease::NotModified { headers } => {
                    return Ok(Some(
                        Response::empty()?
                            .with_status(304)
                            .with_headers(to_headers(&headers)?),
                    ));
                }
                FromRelease::File {
                    status,
                    blob,
                    content_type,
                    headers,
                } => (status, blob, content_type, headers),
            };
        let Some(object) = bucket.get(r2::blob_key(blob)).execute().await? else {
            console_error!("apex release {}: blob {blob} is missing", live.release);
            return Ok(None);
        };
        let Some(body) = object.body() else {
            return Ok(None);
        };
        let response = if site_publish::needs_scan(content_type) {
            let bytes = body.bytes().await?;
            if !site_publish::scriptless(content_type, &bytes) {
                console_error!("apex release {}: {path} is not script-free", live.release);
                return Ok(None);
            }
            if head {
                Response::empty()?
            } else {
                Response::from_bytes(bytes)?
            }
        } else if head {
            Response::empty()?
        } else {
            Response::from_body(body.response_body()?)?
        };
        Ok(Some(
            response.with_status(status).with_headers(to_headers(&h)?),
        ))
    }

    /// The release the apex serves now, read over the binding: its id and commit, or `None`
    /// (the built-in home page answers `/`).
    async fn serving(&self, env: &Env) -> Option<Rc<Live>> {
        let bucket = env.bucket(self.release.as_deref()?).ok()?;
        current(&bucket).await
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
        // This Worker: its own architecture model must name the commit deploy.sh waits for, and
        // the answer says which release `/` will serve (none: the built-in home page).
        let mut release = Value::Null;
        if target == self.worker {
            let json: Value = serde_json::from_str(&rendered(cx.hosted)?[2])?;
            if !json["commit"].as_str().is_some_and(is_commit) {
                return Err(worker::Error::RustError(format!(
                    "this Worker's /architecture.json names no 40-character commit; not moving {}",
                    self.zone
                )));
            }
            if let Some(live) = self.serving(cx.env).await {
                release = json!({ "release": live.release, "commit": live.commit });
            }
        }
        let zone = zones::active(cx.account, &self.zone).await?;
        let custom_domain = custom_domain::attach(cx.account, &zone.id, &zone.name, target).await?;
        Ok(json!({
            "worker": target,
            "host": zone.name,
            "pages": if checked.is_empty() { json!(PAGES) } else { json!(checked) },
            "checked": checked,
            "release": release,
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
                          apex's custom domain onto it. This Worker serves the published landing \
                          release, with its built-in /architecture, /architecture.json and \
                          images; another Worker is checked first: it must answer /, \
                          /architecture and /architecture.json. Changes no Access setup.",
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

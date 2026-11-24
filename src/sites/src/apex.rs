//! The pigeon.dev apex: the home page, the architecture page and its JSON, and the logo. The
//! pages are pure ([`pigeon_core::home`]). The architecture comes from core
//! (`architecture_describe`, public), asked as this Worker itself and kept for 30 seconds per
//! isolate: short, because the deploy script polls for it after a core deploy. No credential
//! is on this path.

use std::cell::RefCell;

use pigeon_contract::Answer;
use pigeon_core::home::{self, Route};
use serde_json::{Value, json};
use worker::{Env, Headers, Request, Response, Result, console_error};

/// How long core's answer is kept.
const KEEP_MS: f64 = 30_000.0;

/// The logo: the app icon, as a PNG.
const LOGO: &[u8] = include_bytes!("../assets/logo.png");

const HTML: &str = "text/html; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";
const JSON: &str = "application/json";
/// Cache-Control of the pages: short, so a core deploy shows soon.
const SHORT: &str = "public, max-age=30";

/// What core says about the architecture.
#[derive(Clone)]
struct Architecture {
    commit: String,
    digest: String,
    model: Value,
    drawing: String,
}

impl Architecture {
    fn build(&self) -> home::Build<'_> {
        home::Build {
            commit: &self.commit,
            digest: &self.digest,
        }
    }
}

thread_local! {
    static KEPT: RefCell<Option<(f64, Architecture)>> = const { RefCell::new(None) };
}

/// The architecture from core's answer; a missing field is an error.
fn parse(v: &Value) -> std::result::Result<Architecture, String> {
    let text = |k: &str| {
        v[k].as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("core's architecture answer has no `{k}`"))
    };
    if !v["model"].is_object() {
        return Err("core's architecture answer has no `model`".into());
    }
    Ok(Architecture {
        commit: text("commit")?,
        digest: text("digest")?,
        model: v["model"].clone(),
        drawing: text("drawing")?,
    })
}

/// The architecture, from the isolate's copy or from core.
async fn architecture(env: &Env, key: &[u8], worker: &str) -> Result<Architecture> {
    let now = worker::Date::now().as_millis() as f64;
    let kept = KEPT.with(|k| {
        k.borrow()
            .as_ref()
            .filter(|(at, _)| now - at < KEEP_MS)
            .map(|(_, a)| a.clone())
    });
    if let Some(a) = kept {
        return Ok(a);
    }
    let nonce = pigeon_core::webauthn::random()?;
    let answer = pigeon_contract::ask(
        &env.service("PLATFORM")?,
        "https://core",
        "dispatch",
        &json!({ "tool": "architecture_describe", "args": { "public": true, "svg": true } }),
        Some((key, &crate::public::itself(worker))),
        &nonce,
    )
    .await?;
    let a = match answer {
        Answer::Ok(v) => parse(&v).map_err(worker::Error::RustError)?,
        Answer::Error(f) => {
            return Err(worker::Error::RustError(format!(
                "core's architecture_describe: {}",
                f.message
            )));
        }
    };
    KEPT.with(|k| *k.borrow_mut() = Some((now, a.clone())));
    Ok(a)
}

/// A response with the headers every apex page carries. The body is dropped for HEAD.
fn reply(status: u16, kind: &str, cache: &str, body: Vec<u8>, head: bool) -> Result<Response> {
    let h = Headers::new();
    h.set("Content-Type", kind)?;
    h.set("Content-Security-Policy", home::CSP)?;
    h.set("X-Content-Type-Options", "nosniff")?;
    h.set("Referrer-Policy", "no-referrer")?;
    h.set("X-Served-By", "pigeon-sites")?;
    h.set("Cache-Control", cache)?;
    let body = if head { Vec::new() } else { body };
    Ok(Response::from_bytes(body)?
        .with_status(status)
        .with_headers(h))
}

/// Serves `req` for the apex host.
pub async fn serve(req: Request, env: &Env, key: &[u8], worker: &str) -> Result<Response> {
    let method = req.method().to_string();
    let head = method == "HEAD";
    let path = req.url()?.path().to_string();
    match home::route(&method, &path) {
        Route::NotFound => reply(404, TEXT, "no-store", b"not found".to_vec(), head),
        Route::MethodNotAllowed => {
            let r = reply(405, TEXT, "no-store", b"method not allowed".to_vec(), head)?;
            r.headers().set("Allow", "GET, HEAD")?;
            Ok(r)
        }
        Route::Home => {
            let a = architecture(env, key, worker).await;
            let cache = if a.is_ok() { SHORT } else { "no-store" };
            let page = home::home_html(a.as_ref().ok().map(Architecture::build));
            reply(200, HTML, cache, page.into_bytes(), head)
        }
        Route::Logo => reply(
            200,
            "image/png",
            "public, max-age=86400",
            LOGO.to_vec(),
            head,
        ),
        Route::Architecture => match architecture(env, key, worker).await {
            Ok(a) if a.drawing.starts_with("<svg ") => {
                let page = home::architecture_html(&a.drawing, a.build());
                reply(200, HTML, SHORT, page.into_bytes(), head)
            }
            Ok(a) => {
                // An older core can still answer the overview and JSON during a rollout.
                let page = home::unavailable_html(Some(a.build()));
                reply(503, HTML, "no-store", page.into_bytes(), head)
            }
            Err(e) => {
                console_error!("apex architecture: {e}");
                let page = home::unavailable_html(None);
                reply(503, HTML, "no-store", page.into_bytes(), head)
            }
        },
        Route::Json => match architecture(env, key, worker).await {
            Ok(a) => {
                let body = serde_json::to_string_pretty(
                    &json!({ "commit": a.commit, "digest": a.digest, "model": a.model }),
                )
                .map_err(|e| worker::Error::RustError(e.to_string()))?;
                reply(200, JSON, SHORT, body.into_bytes(), head)
            }
            Err(e) => {
                console_error!("apex architecture.json: {e}");
                let body = json!({ "error": "the architecture is not available" }).to_string();
                reply(503, JSON, "no-store", body.into_bytes(), head)
            }
        },
    }
}

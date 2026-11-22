//! Cloudflare Containers, as a Durable Object holds one: start it from a named image at an
//! instance size, see whether it runs, wait for it to end, stop it. Reached through the JS API
//! (`ctx.container`), since workers-rs's typed start options lack `image` and `instance`, which
//! the `durable_object` scheduling policy needs. Errors carry the JS error's message, never the
//! options, so an environment value never reaches a log.

use worker::State;
use worker::js_sys::{Array, Function, Object, Promise, Reflect};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;

/// What to start.
pub struct Start<'a> {
    /// A key of `ctx.container.images`, such as `base`.
    pub image: &'a str,
    /// Such as `standard-3`.
    pub instance: &'a str,
    pub env: &'a [(&'a str, &'a str)],
    /// The command to run, program first.
    pub entrypoint: &'a [&'a str],
    /// Whether it may reach the internet.
    pub internet: bool,
}

/// How a container ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Its exit code; 0 when it ended cleanly.
    Code(i64),
    /// It ended without an exit code, for this reason.
    Failed(String),
}

fn fail(reason: impl Into<String>) -> worker::Error {
    worker::Error::RustError(reason.into())
}

/// A JS error's `message`.
fn message(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if !err.is_object() {
        return "unknown error".to_string();
    }
    Reflect::get(err, &"message".into())
        .ok()
        .and_then(|m| m.as_string())
        .unwrap_or_else(|| "unknown error".to_string())
}

/// `ctx.container`, when the object has one.
fn container(state: &State) -> Option<JsValue> {
    let raw: &JsValue = state.as_raw();
    let value = Reflect::get(raw, &"container".into()).ok()?;
    if value.is_undefined() || value.is_null() {
        None
    } else {
        Some(value)
    }
}

/// The container's method `name`.
fn method(container: &JsValue, name: &str) -> worker::Result<Function> {
    Reflect::get(container, &name.into())
        .map_err(|e| fail(format!("container {name} unavailable: {}", message(&e))))?
        .dyn_into::<Function>()
        .map_err(|_| fail(format!("container {name} is not a function")))
}

fn set(target: &JsValue, key: &str, value: &JsValue) -> worker::Result<()> {
    Reflect::set(target, &key.into(), value)
        .map(|_| ())
        .map_err(|e| {
            fail(format!(
                "could not build container options: {}",
                message(&e)
            ))
        })
}

/// Whether the object has a container at all.
pub fn present(state: &State) -> bool {
    container(state).is_some()
}

/// Starts the container: an error when there is none, the image name is unknown, or the start
/// is refused.
pub fn start(state: &State, s: &Start) -> worker::Result<()> {
    let container = container(state).ok_or_else(|| fail("this object has no container"))?;
    let images = Reflect::get(&container, &"images".into())
        .map_err(|e| fail(format!("container images unavailable: {}", message(&e))))?;
    if !images.is_object() {
        return Err(fail("container images unavailable"));
    }
    let image = Reflect::get(&images, &s.image.into())
        .ok()
        .and_then(|v| v.as_string())
        .ok_or_else(|| fail(format!("no container image named `{}`", s.image)))?;
    let env = Object::new();
    for (name, value) in s.env {
        set(&env, name, &JsValue::from_str(value))?;
    }
    let entrypoint = Array::new();
    for part in s.entrypoint {
        entrypoint.push(&JsValue::from_str(part));
    }
    let options = Object::new();
    set(&options, "image", &JsValue::from_str(&image))?;
    set(&options, "instance", &JsValue::from_str(s.instance))?;
    set(&options, "env", &env)?;
    set(&options, "entrypoint", &entrypoint)?;
    set(&options, "enableInternet", &JsValue::from_bool(s.internet))?;
    method(&container, "start")?
        .call1(&container, &options)
        .map(|_| ())
        .map_err(|e| fail(format!("container start failed: {}", message(&e))))
}

/// The longest a container may outlive its object's last activity, Cloudflare's ceiling.
pub const MAX_KEEP_MS: i64 = 6 * 3_600_000;

/// Keeps the container running for `ms` (clamped to 1 ms .. [`MAX_KEEP_MS`]) after its object
/// last handled anything. Without it, Cloudflare stops the container shortly after the object
/// goes idle, which kills any job longer than a few seconds. An object that restarts starts
/// without one, so it is set after each start and again whenever the object wakes.
pub async fn keep_for(state: &State, ms: i64) -> worker::Result<()> {
    match ask_keep(state, ms)? {
        Some(answer) => answer.await,
        None => Ok(()),
    }
}

/// [`keep_for`], asked now: the timeout is requested before this returns, and the returned
/// future, if any, is its answer. For an object's constructor, which cannot wait.
pub fn ask_keep(
    state: &State,
    ms: i64,
) -> worker::Result<Option<impl std::future::Future<Output = worker::Result<()>> + 'static>> {
    let container = container(state).ok_or_else(|| fail("this object has no container"))?;
    let ms = ms.clamp(1, MAX_KEEP_MS);
    let result = method(&container, "setInactivityTimeout")?
        .call1(&container, &JsValue::from_f64(ms as f64))
        .map_err(|e| {
            fail(format!(
                "container inactivity timeout failed: {}",
                message(&e)
            ))
        })?;
    Ok(result.dyn_into::<Promise>().ok().map(|promise| async move {
        JsFuture::from(promise).await.map(|_| ()).map_err(|e| {
            fail(format!(
                "container inactivity timeout failed: {}",
                message(&e)
            ))
        })
    }))
}

/// Whether it is running; false when there is none.
pub fn running(state: &State) -> bool {
    container(state)
        .and_then(|c| Reflect::get(&c, &"running".into()).ok())
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Waits for it to end. An error when this instance has no container, as after a restart.
pub async fn monitor(state: &State) -> worker::Result<Exit> {
    let container = container(state).ok_or_else(|| fail("this object has no container"))?;
    let promise = method(&container, "monitor")?
        .call0(&container)
        .map_err(|e| fail(format!("container monitor failed: {}", message(&e))))?
        .dyn_into::<Promise>()
        .map_err(|_| fail("container monitor did not return a promise"))?;
    match JsFuture::from(promise).await {
        Ok(_) => Ok(Exit::Code(0)),
        Err(e) => {
            let code = if e.is_object() {
                Reflect::get(&e, &"exitCode".into())
                    .ok()
                    .and_then(|v| v.as_f64())
            } else {
                None
            };
            Ok(match code {
                Some(code) => Exit::Code(code as i64),
                None => Exit::Failed(message(&e)),
            })
        }
    }
}

/// Stops it; one that is not running is left alone.
pub async fn destroy(state: &State, reason: &str) -> worker::Result<()> {
    if !running(state) {
        return Ok(());
    }
    let Some(container) = container(state) else {
        return Ok(());
    };
    let result = method(&container, "destroy")?
        .call1(&container, &JsValue::from_str(reason))
        .map_err(|e| fail(format!("container destroy failed: {}", message(&e))))?;
    if let Ok(promise) = result.dyn_into::<Promise>() {
        JsFuture::from(promise)
            .await
            .map_err(|e| fail(format!("container destroy failed: {}", message(&e))))?;
    }
    Ok(())
}

/// Posts `body` (JSON text) to `path` on the container's `port`, with `headers`; answers the
/// status and the body as text. For a server the container runs, such as an AI agent sandbox's
/// (`docs/plans/ai-agents.md`). An error when there is no container or the port cannot be reached.
pub async fn post(
    state: &State,
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> worker::Result<(u16, String)> {
    let container = container(state).ok_or_else(|| fail("this object has no container"))?;
    let tcp = method(&container, "getTcpPort")?
        .call1(&container, &JsValue::from_f64(f64::from(port)))
        .map_err(|e| {
            fail(format!(
                "container port {port} unavailable: {}",
                message(&e)
            ))
        })?;
    let h = Object::new();
    for (name, value) in headers {
        set(&h, name, &JsValue::from_str(value))?;
    }
    set(&h, "Content-Type", &JsValue::from_str("application/json"))?;
    let init = Object::new();
    set(&init, "method", &JsValue::from_str("POST"))?;
    set(&init, "headers", &h)?;
    set(&init, "body", &JsValue::from_str(body))?;
    let url = format!("http://container{path}");
    let promise = method(&tcp, "fetch")?
        .call2(&tcp, &JsValue::from_str(&url), &init)
        .map_err(|e| fail(format!("container fetch failed: {}", message(&e))))?
        .dyn_into::<Promise>()
        .map_err(|_| fail("container fetch did not return a promise"))?;
    let response = JsFuture::from(promise)
        .await
        .map_err(|e| fail(format!("container fetch failed: {}", message(&e))))?;
    let status = Reflect::get(&response, &"status".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u16;
    let text = method(&response, "text")?
        .call0(&response)
        .map_err(|e| fail(format!("container answer unreadable: {}", message(&e))))?
        .dyn_into::<Promise>()
        .map_err(|_| fail("container answer unreadable"))?;
    let text = JsFuture::from(text)
        .await
        .map_err(|e| fail(format!("container answer unreadable: {}", message(&e))))?
        .as_string()
        .unwrap_or_default();
    Ok((status, text))
}

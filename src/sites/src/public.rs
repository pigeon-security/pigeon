//! The platform subdomains pigeon-sites serves by host, such as `noisypigeon.pigeon.dev`: each
//! one an approved instance plan attached here as a Workers Custom Domain. A request for one is
//! passed through, GET and HEAD only, to the instance Worker's workers.dev address, and its answer
//! kept in the edge cache under the instance Worker's own Cache-Control. Which hosts exist, and
//! their upstreams, come from core (`instance_subdomains`), asked as this Worker itself and kept
//! for five minutes per isolate. No credential is on this path. Any other public host is 404.

use std::cell::RefCell;
use std::collections::BTreeMap;

use pigeon_contract::Answer;
use serde_json::{Value, json};
use worker::{Cache, Env, Fetch, Headers, Method, Request, RequestInit, Response, Result};

/// How long the host → upstream map is kept.
const MAP_MS: f64 = 5.0 * 60_000.0;

thread_local! {
    static MAP: RefCell<Option<(f64, BTreeMap<String, String>)>> = const { RefCell::new(None) };
}

/// The caller this Worker signs as when it asks core: itself, a service, acting for no one.
fn itself(worker: &str) -> Value {
    json!({
        "user": { "id": format!("worker:{worker}"), "kind": "service", "name": worker, "created_at": 0 },
        "acting_for": null,
    })
}

/// The host → upstream map from core's `instance_subdomains` answer. Pure: only https
/// workers.dev upstreams are kept, and hosts are lowercased.
pub fn map_of(listed: &Value) -> BTreeMap<String, String> {
    listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let host = e["host"].as_str()?.to_ascii_lowercase();
            let up = e["upstream"].as_str()?;
            (up.starts_with("https://") && up.ends_with(".workers.dev"))
                .then(|| (host, up.to_string()))
        })
        .collect()
}

async fn upstream_of(env: &Env, key: &[u8], worker: &str, host: &str) -> Result<Option<String>> {
    let now = worker::Date::now().as_millis() as f64;
    let kept = MAP.with(|m| {
        m.borrow()
            .as_ref()
            .filter(|(at, _)| now - at < MAP_MS)
            .map(|(_, map)| map.get(host).cloned())
    });
    if let Some(found) = kept {
        return Ok(found);
    }
    let nonce = pigeon_core::webauthn::random()?;
    let answer = pigeon_contract::ask(
        &env.service("PLATFORM")?,
        "https://core",
        "dispatch",
        &json!({ "tool": "instance_subdomains" }),
        Some((key, &itself(worker))),
        &nonce,
    )
    .await?;
    let map = match answer {
        Answer::Ok(v) => map_of(&v),
        Answer::Error(f) => {
            return Err(worker::Error::RustError(format!(
                "core's instance_subdomains: {}",
                f.message
            )));
        }
    };
    let found = map.get(host).cloned();
    MAP.with(|m| *m.borrow_mut() = Some((now, map)));
    Ok(found)
}

/// Serves `req` for public host `host`.
pub async fn serve(
    req: Request,
    env: &Env,
    key: &[u8],
    worker: &str,
    host: &str,
) -> Result<Response> {
    let Some(upstream) = upstream_of(env, key, worker, &host.to_ascii_lowercase()).await? else {
        return Response::error("not found", 404);
    };
    let method = req.method();
    if method != Method::Get && method != Method::Head {
        return Response::error("method not allowed", 405);
    }
    let url = req.url()?;
    let cache = Cache::default();
    let cache_key = url.to_string();
    if method == Method::Get
        && let Some(hit) = cache.get(&cache_key, false).await?
    {
        return Ok(hit);
    }
    let target = match url.query() {
        Some(q) => format!("{upstream}{}?{q}", url.path()),
        None => format!("{upstream}{}", url.path()),
    };
    let headers = Headers::new();
    for name in ["Accept", "Accept-Encoding"] {
        if let Some(v) = req.headers().get(name)? {
            headers.set(name, &v)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(method.clone()).with_headers(headers);
    let mut answer = Fetch::Request(Request::new_with_init(&target, &init)?)
        .send()
        .await?;
    if method == Method::Get && answer.status_code() == 200 {
        cache.put(&cache_key, answer.cloned()?).await?;
    }
    Ok(answer)
}

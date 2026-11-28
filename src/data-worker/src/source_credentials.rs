//! Read named source secrets only in the account whose Data Worker binds them.

use pigeon_core::s3;
use pigeon_ingest::{
    protocol,
    source_credentials::{self as wire, Bound, Reference, SOURCES_BINDING, Verify},
};
use serde_json::json;
use worker::{Env, Method, Request, Response, Result};

use crate::fail;

pub(crate) async fn open(env: &Env, reference: &Reference) -> Result<(Bound, s3::Keys)> {
    let config = env.var(SOURCES_BINDING)?.to_string();
    let sources: Vec<Bound> = serde_json::from_str(&config)?;
    let Some(bound) = sources
        .into_iter()
        .find(|bound| &bound.reference == reference)
    else {
        return Err(fail("the reviewed source generation is not bound here"));
    };
    let value = env
        .secret_store(&bound.slot)?
        .get()
        .await?
        .ok_or_else(|| fail("the source key is not set in the owning account"))?;
    let keys = s3::parse_keys(&value).map_err(|_| fail("the source credential is invalid"))?;
    Ok((bound, keys))
}

/// Verify read access with one bounded listing. The result never includes object names or keys.
pub(crate) async fn verify(req: &mut Request, env: &Env) -> Result<Response> {
    let Some(signature) = req.headers().get(protocol::SIGNATURE_HEADER)? else {
        return Response::error("not found", 404);
    };
    let Some(body) = crate::managed_dest::bounded_body(req, 4_096).await? else {
        return Response::error("invalid source request length", 400);
    };
    let key = env.secret("DATA_KEY")?.to_string();
    if protocol::verify(
        &signature,
        "POST",
        wire::READY_PATH,
        &body,
        key.as_bytes(),
        crate::now_ms(),
    )
    .is_err()
    {
        return Response::error("not found", 404);
    }
    let request: Verify = serde_json::from_slice(&body)?;
    if request.instance != env.var("INSTANCE")?.to_string() {
        return Response::error("not found", 404);
    }
    let (bound, keys) = match open(env, &request.reference).await {
        Ok(source) => source,
        Err(_) => return Response::error("the reviewed source credential is unavailable", 409),
    };
    let Some(base) = bound.endpoint() else {
        return Response::error("the approved source provider is invalid", 409);
    };
    if bound.provider == "backblaze"
        && s3::verify_backblaze_read_key(&base, &bound.bucket, &bound.prefix, &keys)
            .await
            .is_err()
    {
        return Response::error(
            "Backblaze must confirm a read-only key for the approved bucket and prefix",
            422,
        );
    }
    let endpoint = s3::Endpoint {
        base,
        region: bound.region,
    };
    let path = s3::object_path(&bound.bucket, "");
    let is_sample = request.prefix.is_some();
    let prefix = request.prefix.as_deref().unwrap_or(&bound.prefix);
    if !prefix.starts_with(&bound.prefix)
        || prefix.len() > 1024
        || prefix.chars().any(char::is_control)
    {
        return Response::error("the requested prefix is outside the approved source", 400);
    }
    let query = s3::list_objects_query(
        Some(prefix).filter(|prefix| !prefix.is_empty()),
        None,
        if is_sample { 1000 } else { 1 },
        None,
    );
    let result = list(&endpoint, &path, &query, &keys).await;
    match result {
        Ok(mut listing) if is_sample => {
            // The planners need only sizes, count, and whether the page was truncated.
            for object in &mut listing.objects {
                object.key.clear();
                object.etag.clear();
                object.last_modified.clear();
            }
            listing.prefixes.clear();
            listing.next = listing.next.map(|_| "more".into());
            Response::from_json(&listing)
        }
        Ok(_) => Response::from_json(
            &json!({ "instance": request.instance, "source_pin": request.reference, "read_access": true, "runner_read": 1 }),
        ),
        Err(_) => Response::error("the provider did not allow the bounded listing", 422),
    }
}

async fn list(
    endpoint: &s3::Endpoint,
    path: &str,
    query: &[(String, String)],
    keys: &s3::Keys,
) -> Result<s3::Listing> {
    use worker::{Fetch, Headers, Request, RequestInit, RequestRedirect};
    let headers = Headers::new();
    let date = s3::amz_date(worker::js_sys::Date::now());
    let signed = s3::sign(
        "GET",
        endpoint,
        path,
        query,
        &[],
        s3::EMPTY_SHA256,
        keys,
        &date,
    );
    for (name, value) in signed.iter().filter(|(name, _)| name.as_str() != "host") {
        headers.set(name, value)?;
    }
    let encoded = query
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                s3::uri_encode(key, true),
                s3::uri_encode(value, true)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{}{path}?{encoded}", endpoint.base);
    let mut init = RequestInit::new();
    init.with_method(Method::Get)
        .with_headers(headers)
        .with_redirect(RequestRedirect::Manual);
    let mut response = Fetch::Request(Request::new_with_init(&url, &init)?)
        .send()
        .await?;
    if response.status_code() != 200 {
        return Err(fail("the provider refused the listing"));
    }
    s3::parse_list_objects(&response.text().await?)
}

/// Serve a runner's approved source in the owning account. Sign only a single object URL
/// at a time. The object bytes go directly from the provider to the runner.
pub(crate) async fn runner_read(req: &mut Request, env: &Env) -> Result<Response> {
    let Some(auth) = req.headers().get("authorization")? else {
        return Response::error("not found", 404);
    };
    let Some(token) = auth.strip_prefix("Bearer ") else {
        return Response::error("not found", 404);
    };
    let now = crate::now_ms();
    let instance = env.var("INSTANCE")?.to_string();
    let key = env.secret("DATA_KEY")?.to_string();
    let Some(grant) = wire::authenticate(token, key.as_bytes(), &instance, now) else {
        return Response::error("not found", 404);
    };
    let Some(body) = crate::managed_dest::bounded_body(req, wire::REQUEST_MAX as u64).await? else {
        return Response::error("invalid source request length", 400);
    };
    let Ok(read) = serde_json::from_slice::<wire::Read>(&body) else {
        return Response::error("invalid source read", 400);
    };
    let (bound, keys) = match open(env, &grant.reference).await {
        Ok(source) => source,
        Err(_) => return Response::error("the approved source generation is unavailable", 409),
    };
    if !grant.permits(&bound, &grant.prefix) {
        return Response::error("source scope changed", 409);
    }
    let Some(base) = bound.endpoint() else {
        return Response::error("invalid source endpoint", 409);
    };
    let endpoint = s3::Endpoint {
        base,
        region: bound.region.clone(),
    };
    match read {
        wire::Read::List { after, cursor } => {
            if after
                .as_deref()
                .is_some_and(|key| !grant.permits(&bound, key))
            {
                return Response::error("listing start falls outside the approved prefix", 400);
            }
            let mut query =
                s3::list_objects_query(Some(&grant.prefix), None, 1000, cursor.as_deref());
            if let (Some(after), None) = (after, cursor) {
                query.push(("start-after".into(), after));
            }
            let path = s3::object_path(&bound.bucket, "");
            match list(&endpoint, &path, &query, &keys).await {
                Ok(page)
                    if page
                        .objects
                        .iter()
                        .all(|object| object.key.starts_with(&grant.prefix)) =>
                {
                    match serde_json::to_vec(&page) {
                        Ok(bytes) if bytes.len() <= wire::RESPONSE_MAX => {
                            let headers = worker::Headers::new();
                            headers.set("content-type", "application/json")?;
                            headers.set("cache-control", "no-store")?;
                            Ok(Response::from_bytes(bytes)?.with_headers(headers))
                        }
                        _ => Response::error("source page exceeded its limit", 502),
                    }
                }
                _ => Response::error("the provider did not return a scoped listing", 502),
            }
        }
        wire::Read::Get { key } if !key.is_empty() && grant.permits(&bound, &key) => {
            let Some(lifetime) = grant.get_lifetime(crate::now_ms()) else {
                return Response::error("source grant expired", 401);
            };
            let path = s3::object_path(&bound.bucket, &key);
            let url = s3::presign_get(
                &endpoint,
                &path,
                &keys,
                &s3::amz_date(worker::js_sys::Date::now()),
                lifetime,
            )?;
            let mut response = Response::from_json(&wire::Download { url })?;
            response.headers_mut().set("cache-control", "no-store")?;
            Ok(response)
        }
        _ => Response::error("object is outside the approved source prefix", 403),
    }
}

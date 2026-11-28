//! Read named source secrets only in the account whose Data Worker binds them.

use pigeon_core::s3;
use pigeon_ingest::{
    protocol,
    source_credentials::{Bound, READY_PATH, Reference, SOURCES_BINDING, Verify},
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
        READY_PATH,
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
            &json!({ "instance": request.instance, "source_pin": request.reference, "read_access": true }),
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

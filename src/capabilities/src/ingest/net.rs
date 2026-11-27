//! The network calls of an ingest run: DigitalOcean, Scaleway and the data Worker. Each call
//! answers the status and body, and an error text that never holds a token or a key.

use pigeon_core::sign;
use pigeon_ingest::protocol;
use serde_json::{Value, json};
use worker::wasm_bindgen::JsValue;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use super::hosts;

/// The most of a provider's answer put in an error, in characters.
const SNIPPET: usize = 300;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// One HTTPS call. `body` is a content type and text. Answers the status and the body text.
async fn http(
    method: Method,
    url: &str,
    headers: &[(&str, String)],
    body: Option<(&str, String)>,
) -> Result<(u16, String)> {
    let h = Headers::new();
    for (k, v) in headers {
        h.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(method)
        .with_redirect(worker::RequestRedirect::Manual);
    if let Some((content_type, text)) = body {
        h.set("content-type", content_type)?;
        init.with_body(Some(JsValue::from_str(&text)));
    }
    init.with_headers(h);
    let mut res = Fetch::Request(Request::new_with_init(url, &init)?)
        .send()
        .await?;
    let status = res.status_code();
    let text = res.text().await.unwrap_or_default();
    Ok((status, text))
}

/// A short, single-line piece of a provider's answer, for an error.
fn snippet(text: &str) -> String {
    text.chars()
        .take(SNIPPET)
        .collect::<String>()
        .replace('\n', " ")
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn failed(what: &str, status: u16, text: &str) -> Error {
    bad(format!("{what} failed ({status}): {}", snippet(text)))
}

// ---- DigitalOcean ----

const DO_API: &str = "https://api.digitalocean.com/v2";

fn do_auth(token: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {token}"))]
}

/// `GET /v2/account`: whether the token works. Answers the account's status text.
pub async fn do_account(token: &str) -> Result<String> {
    let (s, t) = http(
        Method::Get,
        &format!("{DO_API}/account"),
        &do_auth(token),
        None,
    )
    .await?;
    if s != 200 {
        return Err(failed("DigitalOcean's account check", s, &t));
    }
    Ok(parse(&t)["account"]["status"]
        .as_str()
        .unwrap_or("active")
        .to_string())
}

/// `POST /v2/droplets`: creates the Droplet and answers its id.
pub async fn do_create(token: &str, body: &Value) -> Result<String> {
    let (s, t) = http(
        Method::Post,
        &format!("{DO_API}/droplets"),
        &do_auth(token),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if !(200..300).contains(&s) {
        return Err(failed("creating the Droplet", s, &t));
    }
    parse(&t)["droplet"]["id"]
        .as_u64()
        .map(|n| n.to_string())
        .ok_or_else(|| bad("DigitalOcean answered no Droplet id"))
}

/// `GET /v2/droplets/{id}`: the Droplet's status, or none when it is gone.
pub async fn do_state(token: &str, id: &str) -> Result<Option<String>> {
    let (s, t) = http(
        Method::Get,
        &format!("{DO_API}/droplets/{id}"),
        &do_auth(token),
        None,
    )
    .await?;
    match s {
        404 => Ok(None),
        200 => Ok(Some(
            parse(&t)["droplet"]["status"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
        )),
        _ => Err(failed("reading the Droplet", s, &t)),
    }
}

/// `DELETE /v2/droplets/{id}`. A Droplet already gone is fine.
pub async fn do_delete(token: &str, id: &str) -> Result<()> {
    let (s, t) = http(
        Method::Delete,
        &format!("{DO_API}/droplets/{id}"),
        &do_auth(token),
        None,
    )
    .await?;
    if matches!(s, 200..=299 | 404) {
        Ok(())
    } else {
        Err(failed("deleting the Droplet", s, &t))
    }
}

// ---- Scaleway ----

const SCW_API: &str = "https://api.scaleway.com";

fn scw_auth(secret: &str) -> Vec<(&'static str, String)> {
    vec![("x-auth-token", secret.to_string())]
}

/// Lists one server in `zone`: whether the key works.
pub async fn scw_check(secret: &str, zone: &str) -> Result<()> {
    let url = format!("{SCW_API}{}?per_page=1", hosts::scaleway_servers_path(zone));
    let (s, t) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    if s == 200 {
        Ok(())
    } else {
        Err(failed("Scaleway's server list", s, &t))
    }
}

/// Provider resources created in the one server request.
pub struct ScalewayCreated {
    pub server: String,
    pub volumes: Vec<String>,
    pub scratch_volume: Option<String>,
}

pub(super) fn scw_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Creates the server and answers the ids of its owned resources.
pub async fn scw_create(secret: &str, zone: &str, body: &Value) -> Result<ScalewayCreated> {
    let url = format!("{SCW_API}{}", hosts::scaleway_servers_path(zone));
    let (s, t) = http(
        Method::Post,
        &url,
        &scw_auth(secret),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if !(200..300).contains(&s) {
        return Err(failed("creating the server", s, &t));
    }
    scw_resources(&parse(&t)["server"])
}

fn scw_resources(server: &Value) -> Result<ScalewayCreated> {
    let id = server["id"]
        .as_str()
        .filter(|id| scw_uuid(id))
        .ok_or_else(|| bad("Scaleway answered no valid server id"))?;
    let volumes: Vec<String> = ["0", "1"]
        .into_iter()
        .filter_map(|slot| server["volumes"][slot]["id"].as_str())
        .filter(|id| scw_uuid(id))
        .map(str::to_string)
        .collect();
    let scratch = &server["volumes"]["1"];
    let scratch_attached = volumes.len() == 2
        && volumes[0] != volumes[1]
        && scratch["size"] == hosts::SCW_VOLUME_BYTES
        && scratch["volume_type"] == "sbs_volume"
        && scratch["boot"] == false;
    let scratch_volume = scratch_attached.then(|| volumes[1].clone());
    Ok(ScalewayCreated {
        server: id.to_string(),
        volumes,
        scratch_volume,
    })
}

/// Recover the exact run-owned Instance after an ambiguous create response.
pub async fn scw_find_created(
    secret: &str,
    zone: &str,
    project: &str,
    run: &str,
) -> Result<Option<ScalewayCreated>> {
    if !scw_uuid(project) {
        return Err(bad("the approved Scaleway project id is invalid"));
    }
    let tag = hosts::scaleway_run_tag(run);
    let name = hosts::vm_name(run);
    let path = hosts::scaleway_servers_path(zone);
    let mut found: Option<ScalewayCreated> = None;
    // Do not rely on the provider's default state filter. In particular, an
    // interrupted create can leave a stopped VM before the first power-on.
    for state in [
        "stopped",
        "stopped%20in%20place",
        "running",
        "starting",
        "stopping",
        "locked",
    ] {
        let url = format!(
            "{SCW_API}{path}?project={project}&tags={tag}&name={name}&state={state}&per_page=2"
        );
        let (status, body) = http(Method::Get, &url, &scw_auth(secret), None).await?;
        if status != 200 {
            return Err(bad(format!(
                "listing the Scaleway job resources failed ({status})"
            )));
        }
        let response = parse(&body);
        if response["total_count"]
            .as_u64()
            .is_some_and(|count| count > 2)
        {
            return Err(bad("the Scaleway recovery lookup is ambiguous"));
        }
        let candidates = response["servers"]
            .as_array()
            .ok_or_else(|| bad("Scaleway returned no server list"))?;
        for server in candidates.iter().filter(|server| {
            server["project"] == project
                && server["zone"] == zone
                && server["name"] == name
                && server["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|value| value == &tag))
        }) {
            let candidate = scw_resources(server)?;
            if found
                .as_ref()
                .is_some_and(|known| known.server != candidate.server)
            {
                return Err(bad("more than one Scaleway server matches this run"));
            }
            found = Some(candidate);
        }
    }
    Ok(found)
}

/// Sets the server's cloud-init user data.
pub async fn scw_user_data(secret: &str, zone: &str, id: &str, user_data: &str) -> Result<()> {
    let url = format!("{SCW_API}{}", hosts::scaleway_user_data_path(zone, id));
    let (s, _) = http(
        Method::Patch,
        &url,
        &scw_auth(secret),
        Some(("text/plain", user_data.to_string())),
    )
    .await?;
    if (200..300).contains(&s) {
        Ok(())
    } else {
        Err(bad(format!("setting the server's user data failed ({s})")))
    }
}

/// Posts an action body (`poweron` or `terminate`). A server already gone is fine for terminate.
pub async fn scw_action(secret: &str, zone: &str, id: &str, body: &Value) -> Result<()> {
    let url = format!("{SCW_API}{}", hosts::scaleway_action_path(zone, id));
    let (s, t) = http(
        Method::Post,
        &url,
        &scw_auth(secret),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if (200..300).contains(&s) || (s == 404 && body["action"] == "terminate") {
        Ok(())
    } else {
        Err(failed("the server action", s, &t))
    }
}

/// The server's state, or none when it is gone.
pub async fn scw_state(secret: &str, zone: &str, id: &str) -> Result<Option<String>> {
    let url = format!("{SCW_API}{}/{id}", hosts::scaleway_servers_path(zone));
    let (s, t) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    match s {
        404 => Ok(None),
        200 => Ok(Some(
            parse(&t)["server"]["state"]
                .as_str()
                .unwrap_or("unknown")
                .to_string(),
        )),
        _ => Err(failed("reading the server", s, &t)),
    }
}

/// Requests deletion of a stopped server without restarting it.
pub async fn scw_delete_server(secret: &str, zone: &str, id: &str) -> Result<()> {
    let url = format!("{SCW_API}{}/{id}", hosts::scaleway_servers_path(zone));
    let (status, _) = http(Method::Delete, &url, &scw_auth(secret), None).await?;
    match status {
        200..=299 | 404 => Ok(()),
        _ => Err(bad(format!(
            "deleting the Scaleway server failed ({status})"
        ))),
    }
}

/// Look up exactly the named volume from the approved project and zone.
pub async fn scw_find_volume(
    secret: &str,
    zone: &str,
    project: &str,
    name: &str,
) -> Result<Option<String>> {
    if !scw_uuid(project) || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return Err(bad("invalid Scaleway volume recovery scope"));
    }
    let url = format!(
        "{SCW_API}/block/v1/zones/{zone}/volumes?project_id={project}&name={name}&page_size=2"
    );
    let (status, body) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    if status != 200 {
        return Err(bad(format!(
            "listing the Scaleway job volumes failed ({status})"
        )));
    }
    let result = parse(&body);
    let volumes = result["volumes"]
        .as_array()
        .ok_or_else(|| bad("Scaleway returned no volume list"))?;
    if result["total_count"]
        .as_u64()
        .is_some_and(|count| count > 2)
    {
        return Err(bad("the Scaleway volume lookup is ambiguous"));
    }
    let mut matches = volumes.iter().filter(|volume| {
        volume["name"] == name && volume["project_id"] == project && volume["zone"] == zone
    });
    let Some(volume) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(bad(
            "more than one Scaleway volume matched a run-owned name",
        ));
    }
    let id = volume["id"]
        .as_str()
        .filter(|id| scw_uuid(id))
        .ok_or_else(|| bad("Scaleway returned no valid volume id"))?;
    Ok(Some(id.to_string()))
}

/// Deletes a detached project-owned block volume. Success means absence was observed.
pub async fn scw_remove_volume(secret: &str, zone: &str, id: &str) -> Result<bool> {
    if !scw_uuid(id) {
        return Err(bad("the saved Scaleway volume id is invalid"));
    }
    let url = format!("{SCW_API}/block/v1/zones/{zone}/volumes/{id}");
    let (status, _) = http(Method::Delete, &url, &scw_auth(secret), None).await?;
    if status == 404 {
        return Ok(true);
    }
    if !(200..300).contains(&status) {
        return Err(bad(format!(
            "deleting the Scaleway block volume failed ({status})"
        )));
    }
    let (status, _) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    match status {
        404 => Ok(true),
        200 => Ok(false),
        _ => Err(bad(format!(
            "checking the Scaleway block volume failed ({status})"
        ))),
    }
}

// ---- the data Worker ----

/// Posts `body` to the data Worker at `base` + `path`, signed with `key` as
/// `protocol::signing_message` says. Answers the body text.
pub async fn data_post(
    base: &str,
    path: &str,
    key: &str,
    at_ms: i64,
    body: &Value,
) -> Result<String> {
    let text = body.to_string();
    let message = protocol::signing_message("POST", path, at_ms, text.as_bytes());
    let mac = sign::hex(&sign::hmac_sha256(key.as_bytes(), message.as_bytes()));
    let header = format!("t={at_ms},v1={mac}");
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let (s, t) = http(
        Method::Post,
        &url,
        &[(protocol::SIGNATURE_HEADER, header)],
        Some(("application/json", text)),
    )
    .await?;
    if (200..300).contains(&s) {
        Ok(t)
    } else {
        Err(bad(format!("the data Worker rejected the request ({s})")))
    }
}

/// A one-line note for a host that has no VM state to read.
pub fn cloudflare_note() -> Value {
    json!("runs in the instance's Cloudflare Container; see status.json for its progress")
}

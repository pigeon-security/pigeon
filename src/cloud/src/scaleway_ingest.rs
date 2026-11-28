//! Scaleway ingest provider operations shared by home and instance Workers.

use pigeon_ingest::scaleway as hosts;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::wasm_bindgen::JsValue;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// One HTTPS call. `body` is a content type and text. Answers the status and the body text.
pub async fn http(
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

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn failed(what: &str, status: u16) -> Error {
    bad(format!("{what} failed ({status})"))
}

// ---- Scaleway ----

const SCW_API: &str = "https://api.scaleway.com";

fn scw_auth(secret: &str) -> Vec<(&'static str, String)> {
    vec![("x-auth-token", secret.to_string())]
}

/// Lists one server in `zone`: whether the key works.
pub async fn scw_check(secret: &str, zone: &str) -> Result<()> {
    let url = format!("{SCW_API}{}?per_page=1", hosts::scaleway_servers_path(zone));
    let (s, _) = http(Method::Get, &url, &scw_auth(secret), None).await?;
    if s == 200 {
        Ok(())
    } else {
        Err(failed("Scaleway's server list", s))
    }
}

/// Provider resources created in the one server request.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScalewayCreated {
    pub server: String,
    pub volumes: Vec<String>,
    pub scratch_volume: Option<String>,
}

pub use hosts::valid_project as scw_uuid;

/// Test whether the key can list servers in the exact project and region.
pub async fn check_project(secret: &str, zone: &str, project: &str) -> Result<()> {
    let path = hosts::scaleway_servers_path(zone);
    let (status, _) = http(
        Method::Get,
        &format!("{SCW_API}{path}?project={project}&per_page=1"),
        &scw_auth(secret),
        None,
    )
    .await?;
    if status == 200 {
        Ok(())
    } else {
        Err(failed("Scaleway project check", status))
    }
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
        return Err(failed("creating the server", s));
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
    let (s, _) = http(
        Method::Post,
        &url,
        &scw_auth(secret),
        Some(("application/json", body.to_string())),
    )
    .await?;
    if (200..300).contains(&s) || (s == 404 && body["action"] == "terminate") {
        Ok(())
    } else {
        Err(failed("the server action", s))
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
        _ => Err(failed("reading the server", s)),
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
/// Request removal of the server and confirm its volumes are all gone.
pub async fn cleanup(
    secret: &str,
    zone: &str,
    project: &str,
    run: &str,
    server: Option<&str>,
    volumes: &mut Vec<String>,
) -> Result<bool> {
    if let Some(id) = server {
        match scw_state(secret, zone, id).await?.as_deref() {
            None => {}
            Some("stopped") => {
                scw_delete_server(secret, zone, id).await?;
                return Ok(false);
            }
            Some("running" | "stopped in place") => {
                scw_action(secret, zone, id, &hosts::scaleway_poweroff()).await?;
                return Ok(false);
            }
            Some(_) => return Ok(false),
        }
    }
    // A create response can omit its asynchronous block IDs. Before declaring
    // cleanup complete, also query both deterministic names within the project.
    for name in hosts::scaleway_volume_names(run) {
        if let Some(volume) = scw_find_volume(secret, zone, project, &name).await?
            && !volumes.contains(&volume)
        {
            volumes.push(volume);
        }
    }
    let mut remaining = Vec::new();
    for volume in volumes.iter() {
        if !scw_remove_volume(secret, zone, volume).await? {
            remaining.push(volume.clone());
        }
    }
    *volumes = remaining;
    Ok(volumes.is_empty())
}

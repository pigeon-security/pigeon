//! Change one exact OAuth redirect URI on the Access application behind the MCP Server Portal
//! (docs/plans/mcp-portal.md, "Phase 6"). The functions at the top are pure: they check a URI,
//! summarise the application, plan one change and compare two reads. The two handlers at the
//! bottom read and write through an Access [`Session`] and ask a person's approval first.

use pigeon_core::access::Session;
use serde_json::{Map, Value, json};
use worker::{Error, Result};

use crate::Context;

/// The approval kind of a redirect change.
pub const KIND: &str = "portal_oauth_redirects";

/// How long a plan waits for its approval: 30 minutes.
const PENDING_MS: i64 = 30 * 60 * 1000;

/// The longest URI accepted, in bytes.
const MAX_URI: usize = 2000;

/// Fields Cloudflare sets itself and refuses or ignores in a PUT.
const DROPPED: [&str; 4] = ["id", "aud", "created_at", "updated_at"];

/// One planned change: the PUT body (`None` when nothing would change) and the `allowed_uris`
/// before and after.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    /// The body for the PUT, or `None` when nothing would change.
    pub body: Option<Value>,
    /// The `allowed_uris` as read.
    pub before: Vec<String>,
    /// The `allowed_uris` the change leaves.
    pub after: Vec<String>,
}

/// Accepts one exact `https://host/...` URI and answers it; otherwise says why not.
pub fn check_uri(uri: &str) -> Result<String, String> {
    if uri.len() > MAX_URI {
        return Err(format!("the URI is longer than {MAX_URI} bytes"));
    }
    if uri.contains('*') {
        return Err("a wildcard `*` is never allowed: give one exact URI".into());
    }
    if uri.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URI holds whitespace or a control character".into());
    }
    let lower = uri.to_ascii_lowercase();
    if lower.contains("%3a") || lower.contains("%2f") {
        return Err("the URI looks percent-encoded (`%3A` or `%2F`): decode it first".into());
    }
    if lower.starts_with("http://") {
        return Err(
            "`http://` is refused: loopback and localhost are already allowed by flags".into(),
        );
    }
    let Some(rest) = uri.strip_prefix("https://") else {
        return Err("the URI must start with `https://`".into());
    };
    if uri.contains('#') {
        return Err("the URI must not have a fragment (`#`)".into());
    }
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("the URI must not have user info (`user@`)".into());
    }
    if authority.is_empty() || authority.starts_with(':') {
        return Err("the URI has no host".into());
    }
    Ok(uri.to_string())
}

/// The application as it may be shown: id, name, type, domain, `oauth_configuration` and the
/// policies' id, name and precedence. Never a policy's rules, which hold personal emails.
pub fn summary(app: &Value) -> Value {
    let policies: Vec<Value> = app["policies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| json!({ "id": p["id"], "name": p["name"], "precedence": p["precedence"] }))
        .collect();
    json!({
        "id": app["id"],
        "name": app["name"],
        "type": app["type"],
        "domain": app["domain"],
        "oauth_configuration": app["oauth_configuration"],
        "policies": policies,
    })
}

/// The `allowed_uris` of an application, or why they cannot be read.
fn allowed(app: &Value) -> Result<Vec<String>, String> {
    match &app["oauth_configuration"]["dynamic_client_registration"]["allowed_uris"] {
        Value::Null => Ok(Vec::new()),
        Value::Array(a) => a
            .iter()
            .map(|u| {
                u.as_str()
                    .map(String::from)
                    .ok_or_else(|| "an allowed URI is not a string".to_string())
            })
            .collect(),
        _ => Err("`allowed_uris` is not a list".into()),
    }
}

/// Plans `action` (`add` or `remove`) of `uri` on `app`. Refuses an application whose Managed
/// OAuth is explicitly off or whose dynamic client registration is not on, or whose policies
/// are not all reusable links.
/// Only `allowed_uris` changes in the body; policies become `{id, precedence}` links.
pub fn plan(app: &Value, action: &str, uri: &str) -> Result<Planned, String> {
    if action != "add" && action != "remove" {
        return Err(format!("unknown action `{action}`: use add or remove"));
    }
    let oauth = &app["oauth_configuration"];
    if !oauth.is_object() {
        return Err("the application has no oauth_configuration".into());
    }
    // Cloudflare treats an omitted `enabled` as true, so only an explicit false is off.
    if oauth["enabled"] == json!(false) {
        return Err("Managed OAuth is not enabled on the application".into());
    }
    if oauth["dynamic_client_registration"]["enabled"] != json!(true) {
        return Err("dynamic client registration is not enabled on the application".into());
    }
    let mut links = Vec::new();
    for (i, p) in app["policies"].as_array().into_iter().flatten().enumerate() {
        let Some(id) = p["id"].as_str() else {
            return Err("a policy of the application has no id".into());
        };
        if p["reusable"] != json!(true) {
            return Err(format!("policy {id} is not a reusable policy"));
        }
        let precedence = p["precedence"].as_i64().unwrap_or(i as i64 + 1);
        links.push(json!({ "id": id, "precedence": precedence }));
    }
    let before = allowed(app)?;
    let mut after = before.clone();
    if action == "add" {
        if !after.iter().any(|u| u == uri) {
            after.push(uri.to_string());
        }
    } else {
        after.retain(|u| u != uri);
    }
    if after == before {
        return Ok(Planned {
            body: None,
            before,
            after,
        });
    }
    let mut body = app.clone();
    if let Some(map) = body.as_object_mut() {
        for k in DROPPED {
            map.remove(k);
        }
        map.insert("policies".into(), Value::Array(links));
    }
    body["oauth_configuration"]["dynamic_client_registration"]["allowed_uris"] = json!(after);
    Ok(Planned {
        body: Some(body),
        before,
        after,
    })
}

/// The policy ids of an application, in order.
fn policy_ids(app: &Value) -> Vec<&Value> {
    app["policies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| &p["id"])
        .collect()
}

/// Names the top-level fields and `oauth_configuration` sub-fields that differ between two reads
/// of the application. `allowed_uris` and `updated_at` are ignored; policies count by id and
/// order only.
pub fn differences(before: &Value, after: &Value) -> Vec<String> {
    let empty = Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        match k.as_str() {
            "updated_at" => {}
            "policies" => {
                if policy_ids(before) != policy_ids(after) {
                    out.push("policies".to_string());
                }
            }
            "oauth_configuration" => out.extend(oauth_differences(&b[k], &a[k])),
            _ => {
                if b.get(k) != a.get(k) {
                    out.push(k.clone());
                }
            }
        }
    }
    out
}

/// The differing sub-fields of two `oauth_configuration` values, named with their path.
fn oauth_differences(before: &Value, after: &Value) -> Vec<String> {
    let strip = |v: &Value| {
        let mut v = v.clone();
        if let Some(d) = v
            .get_mut("dynamic_client_registration")
            .and_then(Value::as_object_mut)
        {
            d.remove("allowed_uris");
        }
        v
    };
    let (b, a) = (strip(before), strip(after));
    let empty = Map::new();
    let (bm, am) = (
        b.as_object().unwrap_or(&empty),
        a.as_object().unwrap_or(&empty),
    );
    let mut keys: Vec<&String> = bm.keys().chain(am.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        if k == "dynamic_client_registration" {
            let (bd, ad) = (
                bm.get(k).and_then(Value::as_object).unwrap_or(&empty),
                am.get(k).and_then(Value::as_object).unwrap_or(&empty),
            );
            let mut sub: Vec<&String> = bd.keys().chain(ad.keys()).collect();
            sub.sort();
            sub.dedup();
            for s in sub {
                if bd.get(s) != ad.get(s) {
                    out.push(format!("oauth_configuration.{k}.{s}"));
                }
            }
        } else if bm.get(k) != am.get(k) {
            out.push(format!("oauth_configuration.{k}"));
        }
    }
    out
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// What the approval covers: the app, the action, the URI and the list before and after.
fn payload(app_id: &str, action: &str, uri: &str, planned: &Planned) -> Value {
    json!({
        "app_id": app_id,
        "action": action,
        "uri": uri,
        "before": planned.before,
        "after": planned.after,
    })
}

/// Reads the portal application through a session that is always finished.
async fn read_app(cx: &Context<'_>, host: &str) -> Result<Value> {
    let session = Session::open_apps(cx.account, false).await?;
    let result = session.portal_app(host).await;
    session.finish(result).await
}

/// `portal_oauth_redirects`: lists the redirect URIs, or plans an add or remove and creates an
/// approval for it. Nothing changes at Cloudflare.
pub async fn redirects(cx: &Context<'_>, host: &str, args: &Value) -> Result<Value> {
    let action = crate::arg(args, "action")?;
    if action == "list" {
        return Ok(summary(&read_app(cx, host).await?));
    }
    if action != "add" && action != "remove" {
        return Err(bad("`action` must be list, add or remove"));
    }
    let uri = check_uri(crate::arg(args, "uri")?).map_err(bad)?;
    let caller = cx
        .caller
        .ok_or_else(|| bad("planning needs a known caller"))?;
    let person = caller
        .acting_for
        .as_ref()
        .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
    let app = read_app(cx, host).await?;
    let planned = plan(&app, action, &uri).map_err(bad)?;
    let Some(_) = planned.body else {
        return Ok(json!({
            "applies": false,
            "allowed_uris": planned.after,
            "note": "Nothing to change: the list already is as asked.",
        }));
    };
    let app_id = app["id"].as_str().unwrap_or_default();
    let payload = payload(app_id, action, &uri, &planned);
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest = pigeon_core::approval::payload_digest(&kind, host, &payload);
    let lines = vec![format!("{action} {uri} on {host}")];
    let made = crate::users::directory(
        cx.env,
        "approval.create",
        json!({
            "kind": kind.name(),
            "subject": host,
            "digest": digest,
            "summary": { "plan": lines },
            "payload": payload,
            "caller_id": caller.user.id,
            "approver_id": person.id,
            "pending_ms": PENDING_MS,
        }),
    )
    .await?;
    Ok(json!({
        "plan": lines,
        "before": planned.before,
        "after": planned.after,
        "applies": true,
        "approval": made["record"]["id"],
        "waits_until_ms": made["record"]["expires_ms"],
        "note": "Nothing changes until the person approves this plan on the console's \
                 Approvals tab; then portal_oauth_redirects_apply {plan: <this approval>} \
                 makes the change once.",
    }))
}

/// `portal_oauth_redirects_apply`: re-reads the application, checks it still gives the approved
/// plan, spends the approval, PUTs the change and reads the application again.
pub async fn apply(cx: &Context<'_>, host: &str, args: &Value) -> Result<Value> {
    let caller = cx
        .caller
        .ok_or_else(|| bad("applying needs a known caller"))?;
    let approval_id = crate::arg(args, "plan")?;
    let got = crate::users::directory(cx.env, "approval.get", json!({ "id": approval_id })).await?;
    if got["kind"] != KIND {
        return Err(bad(format!(
            "there is no portal redirect approval {approval_id}"
        )));
    }
    let action = got["payload"]["action"].as_str().unwrap_or_default();
    let uri = check_uri(got["payload"]["uri"].as_str().unwrap_or_default()).map_err(bad)?;
    let session = Session::open_apps(cx.account, true).await?;
    let result = apply_with(cx, &session, host, caller, approval_id, action, &uri).await;
    session.finish(result).await
}

/// The steps of [`apply`] that need the open session.
async fn apply_with(
    cx: &Context<'_>,
    session: &Session<'_>,
    host: &str,
    caller: &crate::users::Caller,
    approval_id: &str,
    action: &str,
    uri: &str,
) -> Result<Value> {
    let app = session.portal_app(host).await?;
    let planned = plan(&app, action, uri).map_err(bad)?;
    let app_id = app["id"].as_str().unwrap_or_default().to_string();
    let kind = pigeon_core::approval::Kind::Other(KIND.into());
    let digest = pigeon_core::approval::payload_digest(
        &kind,
        host,
        &payload(&app_id, action, uri, &planned),
    );
    let taken = crate::users::directory(
        cx.env,
        "approval.take",
        json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
    )
    .await?;
    if let Some(why) = taken["refused"].as_str() {
        return Err(bad(format!(
            "the plan does not run: {why}. If the application changed since it was approved, plan again."
        )));
    }
    let Some(body) = planned.body else {
        return Ok(json!({ "applied": false, "allowed_uris": planned.after, "differences": [] }));
    };
    session.put_app(&app_id, &body).await?;
    let now = session.portal_app(host).await?;
    let diff = differences(&app, &now);
    let mut out = json!({
        "applied": true,
        "allowed_uris": allowed(&now).map_err(bad)?,
        "differences": diff,
    });
    if !diff.is_empty() {
        out["warning"] = json!(
            "Other fields of the application differ after the change: check the Cloudflare \
             dashboard."
        );
    }
    Ok(out)
}

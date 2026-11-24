//! Scaleway changes: plan and apply for API keys and buckets. A plan reads Scaleway, checks the
//! arguments and changes nothing. An apply makes the change the plan describes, reads Scaleway
//! again and says how that differs from what the plan expected.

use pigeon_core::s3;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::iam;
use crate::provider::{Action, Plan, differences};
use crate::s3api;
use crate::scaleway::{self, Api, PERMISSION_SETS, arg_num, is_access_key, is_uuid, need_str};

/// The most days a new key may live.
pub const MAX_DAYS: u64 = 365;
/// How many days a new key lives when the plan does not say.
pub const DEFAULT_DAYS: u64 = 30;
const DAY_MS: i64 = 86_400_000;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Plans a change of `kind`.
pub async fn plan(api: &Api<'_>, kind: &str, action: Action, args: &Value) -> Result<Plan> {
    match (kind, action) {
        ("tokens", Action::Create) => plan_token_create(api, args).await,
        ("tokens", Action::Delete) => plan_token_delete(api, args).await,
        ("buckets", Action::Create) => plan_bucket_create(api, args).await,
        ("buckets", Action::Delete) => plan_bucket_delete(api, args).await,
        _ => Err(bad(format!("Scaleway does not {} {kind}", action.name()))),
    }
}

/// Makes the change `plan` describes.
pub async fn apply(api: &Api<'_>, plan: &Plan) -> Result<Value> {
    match (plan.kind.as_str(), plan.action) {
        ("tokens", Action::Create) => apply_token_create(api, plan).await,
        ("tokens", Action::Delete) => apply_token_delete(api, plan).await,
        ("buckets", Action::Create) => apply_bucket_create(api, plan).await,
        ("buckets", Action::Delete) => apply_bucket_delete(api, plan).await,
        _ => Err(bad("the plan is not one this gateway can apply")),
    }
}

/// A plan of Scaleway's.
fn make(kind: &str, action: Action, args: Value, before: Value, lines: Vec<String>) -> Plan {
    Plan {
        provider: scaleway::NAME.into(),
        kind: kind.into(),
        action,
        args,
        before,
        lines,
    }
}

// ---- API keys ----

/// The string array argument `key`, when given.
fn strings(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
            .map(Some)
            .ok_or_else(|| bad(format!("`{key}` must be a list of strings"))),
        _ => Err(bad(format!("`{key}` must be a list of strings"))),
    }
}

/// Whether `slot` may name a new secret: `CLOUD_` and 1 to 56 of `A-Z`, `0-9`, `_`.
pub fn check_slot(slot: &str) -> Result<()> {
    let rest = slot.strip_prefix("CLOUD_").unwrap_or_default();
    let ok = (1..=56).contains(&rest.len())
        && rest
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        return Err(bad(
            "`slot` is `CLOUD_` and 1 to 56 of A-Z, 0-9 and `_`, such as CLOUD_SCW_BACKUPS",
        ));
    }
    if scaleway::SLOTS.contains(&slot) {
        return Err(bad("`slot` may not be one of the gateway's own secrets"));
    }
    Ok(())
}

/// The normalised arguments of a key to create, or why they are refused.
pub fn token_create_args(args: &Value, default_project: &str) -> Result<Value> {
    if !args["organization_id"].is_null() || !args["organization"].is_null() {
        return Err(bad(
            "an organization-wide rule is refused: name `project_ids` instead",
        ));
    }
    let name = need_str(args, "name")?;
    if !(1..=50).contains(&name.len())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(bad("`name` is 1 to 50 of a-z, 0-9 and `-`"));
    }
    let mut sets = strings(args, "permission_sets")?.unwrap_or_default();
    if sets.is_empty() {
        return Err(bad("`permission_sets` needs at least one"));
    }
    if let Some(other) = sets.iter().find(|s| !PERMISSION_SETS.contains(&s.as_str())) {
        return Err(bad(format!(
            "permission set `{other}` is not allowed: the gateway grants only {}",
            PERMISSION_SETS.join(", ")
        )));
    }
    sets.sort();
    sets.dedup();
    let mut projects = strings(args, "project_ids")?.unwrap_or_default();
    if projects.is_empty() {
        projects.push(default_project.to_string());
    }
    if let Some(other) = projects.iter().find(|p| !is_uuid(p)) {
        return Err(bad(format!("`{other}` is not a project ID (a UUID)")));
    }
    let mut seen = Vec::new();
    projects.retain(|p| {
        let new = !seen.contains(p);
        seen.push(p.clone());
        new
    });
    let days = arg_num(args, "expires_days", DEFAULT_DAYS, 1, MAX_DAYS)?;
    let slot = need_str(args, "slot")?;
    check_slot(slot)?;
    Ok(json!({
        "name": name,
        "permission_sets": sets,
        "project_ids": projects,
        "expires_days": days,
        "slot": slot,
    }))
}

async fn plan_token_create(api: &Api<'_>, args: &Value) -> Result<Plan> {
    let args = token_create_args(args, &api.creds.project)?;
    let name = args["name"].as_str().unwrap_or_default();
    let slot = args["slot"].as_str().unwrap_or_default();
    let app_name = format!("pigeon-{name}");
    // No overwrite in this cut. An error here also says the sink cannot be written to.
    let exists = api
        .ports
        .sink
        .exists(slot)
        .await
        .map_err(|e| api.err(format!("the Secrets Store cannot be checked: {e}"), &[]))?;
    if exists {
        return Err(bad(format!(
            "secret `{slot}` already exists: choose another slot"
        )));
    }
    let org = api.creds.org.as_str();
    let found = api
        .iam(
            "GET",
            "/applications",
            &[("organization_id", org), ("name", app_name.as_str())],
            None,
        )
        .await?;
    if iam::parse_applications(&found)?
        .iter()
        .any(|a| a.name == app_name)
    {
        return Err(bad(format!(
            "an application named `{app_name}` already exists"
        )));
    }
    let list = |key: &str| {
        args[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let lines = vec![
        format!("Create IAM application `{app_name}`, tagged `pigeon`, in organization {org}."),
        format!(
            "Give it one policy: {} on project(s) {}.",
            list("permission_sets"),
            list("project_ids")
        ),
        format!(
            "Create an API key for it that expires in {} days; its default project is the first.",
            args["expires_days"]
        ),
        format!(
            "Write ACCESS_KEY:SECRET_KEY to Secrets Store secret `{slot}` (readable by Workers). \
             The secret key is never shown."
        ),
    ];
    let before = json!({ "application_exists": false, "slot_exists": false });
    Ok(make("tokens", Action::Create, args, before, lines))
}

async fn plan_token_delete(api: &Api<'_>, args: &Value) -> Result<Plan> {
    let access = need_str(args, "access_key")?;
    if !is_access_key(access) {
        return Err(bad("`access_key` is `SCW` and 17 capitals or digits"));
    }
    if access == api.creds.access {
        return Err(bad(
            "the gateway's own API key cannot be deleted through the gateway",
        ));
    }
    let with_app = match &args["with_application"] {
        Value::Null => false,
        Value::Bool(b) => *b,
        _ => return Err(bad("`with_application` must be true or false")),
    };
    let key = api
        .key(access)
        .await?
        .ok_or_else(|| bad("Scaleway has no such API key"))?;
    if !key.deletable {
        return Err(bad("Scaleway does not allow deleting this API key"));
    }
    let mut before = json!({
        "access_key": key.access_key,
        "application_id": key.application_id,
        "description": key.description,
        "expires_at": key.expires_at,
        "application": null,
        "policies": [],
    });
    let mut lines = vec![format!("Delete API key {access} ({}).", key.description)];
    if with_app {
        let app_id = key
            .application_id
            .as_deref()
            .ok_or_else(|| bad("the key has no application to delete"))?;
        let path = format!("/applications/{}", s3::uri_encode(app_id, true));
        let app = api.iam("GET", &path, &[], None).await?;
        let app = iam::parse_application(&app)?;
        if !app.tags.iter().any(|t| t == "pigeon") {
            return Err(bad(format!(
                "application `{}` is not tagged `pigeon`: the gateway deletes only its own",
                app.name
            )));
        }
        let org = api.creds.org.as_str();
        let keys = api
            .iam(
                "GET",
                "/api-keys",
                &[
                    ("organization_id", org),
                    ("application_id", app_id),
                    ("page_size", "100"),
                ],
                None,
            )
            .await?;
        let (others, _) = iam::parse_keys(&keys)?;
        if others.iter().any(|k| k.access_key != access) {
            return Err(bad(format!(
                "application `{}` holds another API key: delete only this key, or the others first",
                app.name
            )));
        }
        let policies = api
            .iam(
                "GET",
                "/policies",
                &[
                    ("organization_id", org),
                    ("application_ids", app_id),
                    ("page_size", "100"),
                ],
                None,
            )
            .await?;
        let mut ids: Vec<String> = iam::parse_policies(&policies)?
            .into_iter()
            .filter(|p| p.application_id.as_deref() == Some(app_id))
            .map(|p| p.id)
            .collect();
        ids.sort();
        lines.push(format!(
            "Delete its {} polic{} and application `{}`.",
            ids.len(),
            if ids.len() == 1 { "y" } else { "ies" },
            app.name
        ));
        before["application"] = app.to_json();
        before["policies"] = json!(ids);
    }
    let args = json!({ "access_key": access, "with_application": with_app });
    Ok(make("tokens", Action::Delete, args, before, lines))
}

/// `ms` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn rfc3339(ms: i64) -> String {
    let d = s3::amz_date(ms as f64);
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &d[9..11],
        &d[11..13],
        &d[13..15]
    )
}

/// Deletes IAM objects the apply made, in order, ignoring errors: a clean-up after a failure.
async fn clean_up(api: &Api<'_>, paths: &[String]) {
    for path in paths {
        let _ = api.iam("DELETE", path, &[], None).await;
    }
}

async fn apply_token_create(api: &Api<'_>, plan: &Plan) -> Result<Value> {
    let a = &plan.args;
    let text = |k: &str| a[k].as_str().unwrap_or_default().to_string();
    let (name, slot) = (text("name"), text("slot"));
    let org = api.creds.org.clone();
    let app_name = format!("pigeon-{name}");
    let projects = a["project_ids"].clone();
    let project = projects[0].as_str().unwrap_or_default().to_string();
    let days = a["expires_days"].as_u64().unwrap_or(DEFAULT_DAYS);

    // Nothing is created before the slot is known to be writable.
    let exists = api
        .ports
        .sink
        .exists(&slot)
        .await
        .map_err(|e| api.err(format!("the Secrets Store cannot be written: {e}"), &[]))?;
    if exists {
        return Err(bad(format!("secret `{slot}` already exists: plan again")));
    }

    let body = json!({
        "name": app_name,
        "organization_id": org,
        "description": "made by pigeon's cloud gateway",
        "tags": ["pigeon"],
    });
    let app = iam::parse_id(&api.iam("POST", "/applications", &[], Some(&body)).await?)?;
    let app_path = format!("/applications/{app}");

    let body = json!({
        "name": app_name,
        "description": "made by pigeon's cloud gateway",
        "organization_id": org,
        "rules": [{ "permission_set_names": a["permission_sets"], "project_ids": projects }],
        "tags": ["pigeon"],
        "application_id": app,
    });
    let policy = match api.iam("POST", "/policies", &[], Some(&body)).await {
        Ok(v) => iam::parse_id(&v)?,
        Err(e) => {
            clean_up(api, &[app_path]).await;
            return Err(e);
        }
    };
    let policy_path = format!("/policies/{policy}");

    let expires_at = rfc3339(api.ports.now_ms + days as i64 * DAY_MS);
    let body = json!({
        "application_id": app,
        "expires_at": expires_at,
        "default_project_id": project,
        "description": format!("pigeon cloud gateway: {name}"),
    });
    let made = match api.iam("POST", "/api-keys", &[], Some(&body)).await {
        Ok(v) => iam::parse_new_key(&v),
        Err(e) => Err(e),
    };
    let made = match made {
        Ok(m) => m,
        Err(e) => {
            clean_up(api, &[policy_path, app_path]).await;
            return Err(e);
        }
    };

    let access = made.key.access_key.clone();
    let value = format!("{access}:{}", made.secret.expose());
    let who = api.ports.caller["user"]["name"]
        .as_str()
        .unwrap_or("someone");
    let comment =
        format!("Scaleway API key of application {app_name}, made by the cloud gateway for {who}");
    if let Err(e) = api.ports.sink.write(&slot, &value, &comment).await {
        let why = api.err(
            format!("writing secret `{slot}` failed: {e}"),
            &[made.secret.expose(), value.as_str()],
        );
        let key_path = format!("/api-keys/{access}");
        let gone = api.iam("DELETE", &key_path, &[], None).await.is_ok();
        clean_up(api, &[policy_path, app_path]).await;
        return Err(if gone {
            bad(format!("{why}; the new API key was deleted"))
        } else {
            bad(format!(
                "{why}; deleting the new API key failed too: delete the key of application \
                 `{app_name}` by hand"
            ))
        });
    }

    // The change is made and the secret written: a failed look afterwards is a difference, not an
    // error that hides the new key.
    let seen = api.key(&access).await.ok().flatten();
    let expected = json!({ "application_id": app, "default_project_id": project });
    let found = seen.map_or(Value::Null, |k| {
        json!({ "application_id": k.application_id, "default_project_id": k.default_project_id })
    });
    Ok(json!({
        "created": {
            "slot": slot,
            "access_key": access,
            "application": { "id": app, "name": app_name },
            "policy": policy,
            "expires_at": expires_at,
        },
        "note": format!("the secret key is in secret `{slot}` as ACCESS_KEY:SECRET_KEY; it is never shown"),
        "differences": differences(&expected, &found),
    }))
}

async fn apply_token_delete(api: &Api<'_>, plan: &Plan) -> Result<Value> {
    let access = plan.args["access_key"].as_str().unwrap_or_default();
    if access == api.creds.access {
        return Err(bad(
            "the gateway's own API key cannot be deleted through the gateway",
        ));
    }
    let with_app = plan.args["with_application"].as_bool().unwrap_or(false);
    api.iam("DELETE", &format!("/api-keys/{access}"), &[], None)
        .await?;
    let mut expected = json!({ "key_exists": false });
    let mut found = json!({ "key_exists": api.key(access).await?.is_some() });
    if with_app {
        let app_id = plan.before["application_id"].as_str().unwrap_or_default();
        for id in plan.before["policies"].as_array().into_iter().flatten() {
            let id = id.as_str().unwrap_or_default();
            api.iam("DELETE", &format!("/policies/{id}"), &[], None)
                .await?;
        }
        let path = format!("/applications/{app_id}");
        api.iam("DELETE", &path, &[], None).await?;
        expected["application_exists"] = json!(false);
        found["application_exists"] = json!(api.iam_maybe("GET", &path).await?.is_some());
    }
    Ok(json!({
        "deleted": { "access_key": access, "with_application": with_app },
        "differences": differences(&expected, &found),
    }))
}

// ---- buckets ----

/// The region and bucket a bucket plan names, checked.
fn bucket_args(args: &Value) -> Result<(String, String)> {
    let region = s3api::check_region(need_str(args, "region")?)?.to_string();
    let bucket = need_str(args, "bucket")?.to_string();
    s3api::check_bucket(&bucket)?;
    Ok((region, bucket))
}

async fn plan_bucket_create(api: &Api<'_>, args: &Value) -> Result<Plan> {
    let (region, bucket) = bucket_args(args)?;
    if api.bucket_exists(&region, &bucket).await? {
        return Err(bad(format!("bucket `{bucket}` already exists in {region}")));
    }
    let lines = vec![
        format!("Create bucket `{bucket}` in {region}, private (`x-amz-acl: private`)."),
        "It lands in the gateway key's preferred project: Scaleway's S3 API takes no project."
            .into(),
    ];
    let args = json!({ "region": region, "bucket": bucket });
    Ok(make(
        "buckets",
        Action::Create,
        args,
        json!({ "exists": false }),
        lines,
    ))
}

async fn plan_bucket_delete(api: &Api<'_>, args: &Value) -> Result<Plan> {
    let (region, bucket) = bucket_args(args)?;
    if !api.bucket_exists(&region, &bucket).await? {
        return Err(bad(format!("bucket `{bucket}` does not exist in {region}")));
    }
    if !api.bucket_empty(&region, &bucket).await? {
        return Err(bad(format!(
            "bucket `{bucket}` is not empty (objects, old versions or delete markers): empty it first"
        )));
    }
    let lines = vec![format!("Delete the empty bucket `{bucket}` in {region}.")];
    let args = json!({ "region": region, "bucket": bucket });
    Ok(make(
        "buckets",
        Action::Delete,
        args,
        json!({ "exists": true, "empty": true }),
        lines,
    ))
}

async fn apply_bucket_create(api: &Api<'_>, plan: &Plan) -> Result<Value> {
    let (region, bucket) = bucket_args(&plan.args)?;
    let acl = [("x-amz-acl".to_string(), "private".to_string())];
    api.s3("PUT", &region, &s3::object_path(&bucket, ""), &[], &acl)
        .await?;
    let exists = api.bucket_exists(&region, &bucket).await?;
    Ok(json!({
        "created": { "region": region, "bucket": bucket, "acl": "private" },
        "differences": differences(&json!({ "exists": true }), &json!({ "exists": exists })),
    }))
}

async fn apply_bucket_delete(api: &Api<'_>, plan: &Plan) -> Result<Value> {
    let (region, bucket) = bucket_args(&plan.args)?;
    // Empty when planned and empty again now: the last look before the delete.
    if !api.bucket_empty(&region, &bucket).await? {
        return Err(bad(format!(
            "bucket `{bucket}` is no longer empty: plan again"
        )));
    }
    api.s3("DELETE", &region, &s3::object_path(&bucket, ""), &[], &[])
        .await?;
    let exists = api.bucket_exists(&region, &bucket).await?;
    Ok(json!({
        "deleted": { "region": region, "bucket": bucket },
        "differences": differences(&json!({ "exists": false }), &json!({ "exists": exists })),
    }))
}

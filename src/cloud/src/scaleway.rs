//! Scaleway as a provider: IAM API keys (with their applications and policies) and Object
//! Storage buckets and objects. This file holds the slots, the survey of them, the calls to
//! Scaleway and the reads. The changes (plan and apply) are in [`crate::changes`].

use pigeon_contract::SlotSpec;
use pigeon_core::expiry::{self, Expiry};
use pigeon_core::s3::{self, Keys};
use pigeon_core::secrets_store;
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::changes;
use crate::iam::{self, SecretKey};
use crate::provider::{Action, Plan, Ports, Provider};
use crate::redact::redact;
use crate::s3api;
use crate::slots::SlotState;
use crate::transport::{Fut, HOSTS, HttpRequest, HttpResponse};

/// The provider's name.
pub const NAME: &str = "scaleway";
/// The gateway's API access key.
pub const ACCESS_KEY: &str = "SCW_ACCESS_KEY";
/// The gateway's API secret key.
pub const SECRET_KEY: &str = "SCW_SECRET_KEY";
/// The organization the gateway works in.
pub const ORGANIZATION_ID: &str = "SCW_DEFAULT_ORGANIZATION_ID";
/// The project whose buckets the gateway manages.
pub const PROJECT_ID: &str = "SCW_DEFAULT_PROJECT_ID";
/// The four slots, in the order messages list them.
pub const SLOTS: [&str; 4] = [ACCESS_KEY, SECRET_KEY, ORGANIZATION_ID, PROJECT_ID];

/// The permission sets a plan may grant: Object Storage's (names checked against Scaleway's
/// permission set reference). The gateway's own key manages IAM, so without this list a plan
/// could grant IAM or organization rights.
pub const PERMISSION_SETS: [&str; 8] = [
    "ObjectStorageFullAccess",
    "ObjectStorageReadOnly",
    "ObjectStorageBucketsRead",
    "ObjectStorageBucketsWrite",
    "ObjectStorageBucketsDelete",
    "ObjectStorageObjectsRead",
    "ObjectStorageObjectsWrite",
    "ObjectStorageObjectsDelete",
];

/// Whether `s` is an access key: `SCW` and 17 of `A-Z0-9`.
pub fn is_access_key(s: &str) -> bool {
    s.len() == 20
        && s.starts_with("SCW")
        && s[3..]
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Whether `s` is a UUID in its usual 8-4-4-4-12 form.
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Checks a value for slot `name` by its form alone; the answer never holds the value.
pub fn check_secret(name: &str, value: &str) -> Result<String> {
    let value = value.trim();
    let (ok, shape) = match name {
        ACCESS_KEY => (is_access_key(value), "`SCW` and 17 capitals or digits"),
        SECRET_KEY | ORGANIZATION_ID | PROJECT_ID => (is_uuid(value), "a UUID"),
        _ => {
            return Err(Error::RustError(format!(
                "the cloud gateway has no secret {name}"
            )));
        }
    };
    if ok {
        Ok(format!(
            "{name} has the form of a Scaleway value (the form only is checked)"
        ))
    } else {
        Err(Error::RustError(format!("{name} must be {shape}")))
    }
}

/// The note a set answers: the Worker reads its secrets through bindings that land later.
pub const SET_NOTE: &str = "the cloud gateway reads it through its binding, once bound \
     (docs/plans/cloud-gateway.md, phase 6)";

/// One slot as the survey found it.
pub struct SlotView {
    /// The slot's name.
    pub name: &'static str,
    /// Whether the Worker has a binding for it.
    pub bound: bool,
    /// Whether it holds a value.
    pub set: bool,
    /// The value, when the Worker could read it.
    value: Option<String>,
}

/// The four slots as found: bound, set, and the values the Worker can read.
pub struct Survey {
    /// One entry per slot, in [`SLOTS`] order.
    pub slots: Vec<SlotView>,
}

impl Survey {
    /// Reads the four slots. A slot without a binding cannot be read, so the sink says whether
    /// it holds a value: its names are listed once, and only when a slot needs it. When the sink
    /// fails the slot counts as not set.
    pub async fn take(ports: &Ports<'_>) -> Result<Survey> {
        let mut slots = Vec::new();
        // None: not asked yet. Some(None): asked and failed.
        let mut listed: Option<Option<Vec<String>>> = None;
        for name in SLOTS {
            let view = match ports.slots.read(name).await? {
                SlotState::Set(v) => SlotView {
                    name,
                    bound: true,
                    set: true,
                    value: Some(v),
                },
                SlotState::Unset => SlotView {
                    name,
                    bound: true,
                    set: false,
                    value: None,
                },
                SlotState::Unbound => {
                    if listed.is_none() {
                        listed = Some(ports.sink.names().await.ok());
                    }
                    let set = listed
                        .as_ref()
                        .and_then(Option::as_ref)
                        .is_some_and(|names| names.iter().any(|n| n == name));
                    SlotView {
                        name,
                        bound: false,
                        set,
                        value: None,
                    }
                }
            };
            slots.push(view);
        }
        Ok(Survey { slots })
    }

    /// Why Scaleway cannot be used, or none when it can.
    pub fn why(&self) -> Option<String> {
        let list = |keep: &dyn Fn(&SlotView) -> bool| {
            self.slots
                .iter()
                .filter(|s| keep(s))
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let missing = list(&|s| !s.set);
        if !missing.is_empty() {
            return Some(format!(
                "Scaleway is not set up: set {missing} on the app's Access page (Credentials)"
            ));
        }
        let unbound = list(&|s| !s.bound);
        if !unbound.is_empty() {
            return Some(format!(
                "Scaleway is not set up: {unbound} are set but not bound yet: ask an agent for \
                 docs/plans/cloud-gateway.md phase 6"
            ));
        }
        None
    }

    /// The credentials, when every slot is bound and set.
    pub fn creds(&self) -> Option<Creds> {
        let value = |name: &str| {
            self.slots
                .iter()
                .find(|s| s.name == name)
                .and_then(|s| s.value.clone())
        };
        Some(Creds {
            access: value(ACCESS_KEY)?,
            secret: SecretKey::new(value(SECRET_KEY)?),
            org: value(ORGANIZATION_ID)?,
            project: value(PROJECT_ID)?,
        })
    }
}

/// The gateway's own Scaleway credentials. `Debug` hides the secret key.
#[derive(Debug, Clone)]
pub struct Creds {
    /// The API access key.
    pub access: String,
    /// The API secret key.
    pub secret: SecretKey,
    /// The organization ID.
    pub org: String,
    /// The default project ID.
    pub project: String,
}

/// Calls to Scaleway with the gateway's credentials. Every error leaves here redacted.
pub struct Api<'a> {
    /// What the call goes through.
    pub ports: &'a Ports<'a>,
    /// The credentials it uses.
    pub creds: &'a Creds,
}

impl Api<'_> {
    /// An error with `text` redacted of the gateway's keys and of `extra`.
    pub fn err(&self, text: impl AsRef<str>, extra: &[&str]) -> Error {
        let mut known = vec![self.creds.access.as_str(), self.creds.secret.expose()];
        known.extend_from_slice(extra);
        Error::RustError(redact(text.as_ref(), &known))
    }

    async fn send(&self, req: HttpRequest) -> Result<HttpResponse> {
        self.ports
            .transport
            .send(req)
            .await
            .map_err(|e| self.err(e.to_string(), &[]))
    }

    /// An IAM call: its status and body, whatever the status.
    async fn iam_raw(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<(u16, Value)> {
        let req = iam::request(method, path, query, body, &self.creds.secret);
        let res = self.send(req).await?;
        if res.status >= 400 {
            return Ok((
                res.status,
                json!({ "message": iam::error_message(&res.body) }),
            ));
        }
        if res.body.trim().is_empty() {
            return Ok((res.status, Value::Null));
        }
        let value = serde_json::from_str(&res.body)
            .map_err(|_| self.err("Scaleway's answer is not JSON", &[]))?;
        Ok((res.status, value))
    }

    /// An IAM call that must work.
    pub async fn iam(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let (status, value) = self.iam_raw(method, path, query, body).await?;
        if status >= 400 {
            let message = value["message"].as_str().unwrap_or_default();
            return Err(self.err(format!("Scaleway {status}: {message}"), &[]));
        }
        Ok(value)
    }

    /// An IAM call that may find nothing: a 404 answers none.
    pub async fn iam_maybe(&self, method: &str, path: &str) -> Result<Option<Value>> {
        let (status, value) = self.iam_raw(method, path, &[], None).await?;
        match status {
            404 => Ok(None),
            s if s >= 400 => {
                let message = value["message"].as_str().unwrap_or_default();
                Err(self.err(format!("Scaleway {s}: {message}"), &[]))
            }
            _ => Ok(Some(value)),
        }
    }

    /// A signed S3 call to `region`. A status of 400 or more is an error from S3's message.
    pub async fn s3(
        &self,
        method: &str,
        region: &str,
        path: &str,
        query: &[(String, String)],
        headers: &[(String, String)],
    ) -> Result<HttpResponse> {
        let keys = Keys {
            access_key_id: self.creds.access.clone(),
            secret_access_key: self.creds.secret.expose().to_string(),
        };
        let req = s3api::request(
            method,
            region,
            path,
            query,
            headers,
            &keys,
            self.ports.now_ms,
        )?;
        let res = self.send(req).await?;
        if res.status >= 400 {
            return Err(self.err(s3::parse_error(res.status, &res.body), &[]));
        }
        Ok(res)
    }

    /// The API key with `access_key`, or none.
    pub async fn key(&self, access_key: &str) -> Result<Option<iam::ApiKey>> {
        let path = format!("/api-keys/{}", s3::uri_encode(access_key, true));
        self.iam_maybe("GET", &path)
            .await?
            .map(|v| iam::parse_key(&v))
            .transpose()
    }

    /// The buckets in `region`.
    pub async fn buckets(&self, region: &str) -> Result<Vec<s3::Bucket>> {
        let res = self.s3("GET", region, "/", &[], &[]).await?;
        s3::parse_list_buckets(&res.body).map_err(|e| self.err(e.to_string(), &[]))
    }

    /// Whether bucket `bucket` is among those in `region`.
    pub async fn bucket_exists(&self, region: &str, bucket: &str) -> Result<bool> {
        Ok(self.buckets(region).await?.iter().any(|b| b.name == bucket))
    }

    /// Whether `bucket` holds no object, no old version and no delete marker.
    pub async fn bucket_empty(&self, region: &str, bucket: &str) -> Result<bool> {
        let path = s3::object_path(bucket, "");
        let res = self
            .s3("GET", region, &path, &s3api::versions_query(), &[])
            .await?;
        s3api::versions_empty(&res.body).map_err(|e| self.err(e.to_string(), &[]))
    }
}

/// Scaleway, as a [`Provider`].
pub struct Scaleway;

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// The string argument `key`, when given.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.as_str())),
        _ => Err(bad(format!("`{key}` must be a string"))),
    }
}

/// The string argument `key`, which must be given.
pub fn need_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    arg_str(args, key)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("`{key}` is required")))
}

/// The whole number argument `key` within `min..=max`, or `default` when not given.
pub fn arg_num(args: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    match &args[key] {
        Value::Null => Ok(default),
        v => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| {
                bad(format!(
                    "`{key}` must be a whole number from {min} to {max}"
                ))
            }),
    }
}

impl Scaleway {
    /// The credentials, or why not: the message every tool answers while Scaleway is not set up.
    async fn creds(&self, ports: &Ports<'_>) -> Result<Creds> {
        let survey = Survey::take(ports).await?;
        match survey.why() {
            Some(why) => Err(bad(why)),
            None => survey.creds().ok_or_else(|| bad("Scaleway is not set up")),
        }
    }

    async fn do_status(&self, ports: &Ports<'_>) -> Result<Value> {
        let survey = Survey::take(ports).await?;
        let why = survey.why();
        let check = match survey.creds() {
            Some(creds) => {
                let api = Api {
                    ports,
                    creds: &creds,
                };
                match api.key(&creds.access).await {
                    Ok(Some(k)) => json!({
                        "access_key": k.access_key,
                        "description": k.description,
                        "expires_at": k.expires_at,
                        "default_project_id": k.default_project_id,
                    }),
                    Ok(None) => json!({ "error": "Scaleway does not know the gateway's key" }),
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
            None => Value::Null,
        };
        Ok(json!({
            "provider": NAME,
            "ready": why.is_none(),
            "slots": survey.slots.iter()
                .map(|s| json!({ "name": s.name, "bound": s.bound, "set": s.set }))
                .collect::<Vec<_>>(),
            "why": why,
            "hosts": HOSTS,
            "check": check,
        }))
    }

    async fn do_list(&self, ports: &Ports<'_>, kind: &str, args: &Value) -> Result<Value> {
        let creds = self.creds(ports).await?;
        let api = Api {
            ports,
            creds: &creds,
        };
        match kind {
            "tokens" => tokens_list(&api, args).await,
            "buckets" => buckets_list(&api, args).await,
            "objects" => objects_list(&api, args).await,
            "object" => object_head(&api, args).await,
            other => Err(bad(format!("Scaleway has no resource kind `{other}`"))),
        }
    }

    async fn do_plan(
        &self,
        ports: &Ports<'_>,
        kind: &str,
        action: Action,
        args: &Value,
    ) -> Result<Plan> {
        let creds = self.creds(ports).await?;
        let api = Api {
            ports,
            creds: &creds,
        };
        changes::plan(&api, kind, action, args).await
    }

    async fn do_apply(&self, ports: &Ports<'_>, plan: &Plan) -> Result<Value> {
        let creds = self.creds(ports).await?;
        let api = Api {
            ports,
            creds: &creds,
        };
        changes::apply(&api, plan).await
    }
}

/// A slot's spec: scope `workers`, with where to get its value.
fn slot(name: &str, purpose: &str) -> SlotSpec {
    SlotSpec {
        name: name.into(),
        purpose: purpose.into(),
        scopes: vec![secrets_store::WORKERS.to_string()],
    }
}

impl Provider for Scaleway {
    fn name(&self) -> &'static str {
        NAME
    }

    fn slots(&self) -> Vec<SlotSpec> {
        vec![
            slot(
                ACCESS_KEY,
                "The access key (`SCW` and 17 more) of the Scaleway API key the cloud gateway \
                 uses: made for an IAM application `pigeon-cloud-gateway` that may manage IAM \
                 and the project's Object Storage (Scaleway console: IAM, Applications, API \
                 keys).",
            ),
            slot(
                SECRET_KEY,
                "The secret key (a UUID, shown once) of the same API key.",
            ),
            slot(
                ORGANIZATION_ID,
                "The Scaleway organization ID (console: Organization, Settings).",
            ),
            slot(
                PROJECT_ID,
                "The ID of the Scaleway project whose buckets pigeon manages (console: \
                 Project, Settings).",
            ),
        ]
    }

    fn kinds(&self) -> &'static [&'static str] {
        &["tokens", "buckets", "objects", "object"]
    }

    fn ready<'a>(&'a self, ports: &'a Ports<'a>) -> Fut<'a, ()> {
        Box::pin(async move { self.creds(ports).await.map(|_| ()) })
    }

    fn status<'a>(&'a self, ports: &'a Ports<'a>) -> Fut<'a, Value> {
        Box::pin(self.do_status(ports))
    }

    fn list<'a>(&'a self, ports: &'a Ports<'a>, kind: &'a str, args: &'a Value) -> Fut<'a, Value> {
        Box::pin(self.do_list(ports, kind, args))
    }

    fn plan<'a>(
        &'a self,
        ports: &'a Ports<'a>,
        kind: &'a str,
        action: Action,
        args: &'a Value,
    ) -> Fut<'a, Plan> {
        Box::pin(self.do_plan(ports, kind, action, args))
    }

    fn apply<'a>(&'a self, ports: &'a Ports<'a>, plan: &'a Plan) -> Fut<'a, Value> {
        Box::pin(self.do_apply(ports, plan))
    }
}

/// When the gateway's own key expires, as Scaleway says: unknown unless both of its keys are
/// readable.
pub async fn key_expiry(ports: &Ports<'_>, name: &str) -> Expiry {
    let unknown = |why: &str| Expiry::Unknown { why: why.into() };
    if name != ACCESS_KEY && name != SECRET_KEY {
        return unknown("the cloud gateway dates only its API key");
    }
    let mut values = Vec::new();
    for slot in [ACCESS_KEY, SECRET_KEY] {
        match ports.slots.read(slot).await {
            Ok(SlotState::Set(v)) => values.push(v),
            _ => return unknown("the key is not readable by the Worker yet"),
        }
    }
    let creds = Creds {
        access: values[0].clone(),
        secret: SecretKey::new(values[1].clone()),
        org: String::new(),
        project: String::new(),
    };
    let api = Api {
        ports,
        creds: &creds,
    };
    match api
        .iam_raw("GET", &format!("/api-keys/{}", creds.access), &[], None)
        .await
    {
        Ok((401 | 403, _)) => Expiry::Refused {
            why: "Scaleway refuses the API key (401/403): it has expired or been revoked".into(),
        },
        Ok((404, _)) => unknown("Scaleway does not know the API key"),
        Ok((s, _)) if s >= 400 => unknown(&format!("the check failed: Scaleway answered {s}")),
        Ok((_, v)) => expiry::from_field(v["expires_at"].as_str()),
        Err(e) => unknown(&format!("the check failed: {e}")),
    }
}

/// API keys of the organization, with their application and policies.
async fn tokens_list(api: &Api<'_>, args: &Value) -> Result<Value> {
    let page = arg_num(args, "page", 1, 1, 10_000)?.to_string();
    let size = arg_num(args, "page_size", 50, 1, 100)?.to_string();
    let org = api.creds.org.as_str();
    let mut query = vec![
        ("organization_id", org),
        ("order_by", "created_at_asc"),
        ("page", page.as_str()),
        ("page_size", size.as_str()),
    ];
    if let Some(app) = arg_str(args, "application_id")? {
        query.push(("application_id", app));
    }
    let listed = api.iam("GET", "/api-keys", &query, None).await?;
    let (keys, total) = iam::parse_keys(&listed)?;
    let by_org = [("organization_id", org), ("page_size", "100")];
    let apps = iam::parse_applications(&api.iam("GET", "/applications", &by_org, None).await?)?;
    let policies = iam::parse_policies(&api.iam("GET", "/policies", &by_org, None).await?)?;
    let api_keys: Vec<Value> = keys
        .iter()
        .map(|k| {
            let mut out = k.to_json();
            let app = k
                .application_id
                .as_ref()
                .and_then(|id| apps.iter().find(|a| &a.id == id));
            out["application"] = app.map_or(Value::Null, iam::Application::to_json);
            out["policies"] = Value::Array(
                policies
                    .iter()
                    .filter(|p| p.application_id.is_some() && p.application_id == k.application_id)
                    .map(iam::Policy::to_json)
                    .collect(),
            );
            out
        })
        .collect();
    Ok(json!({
        "api_keys": api_keys,
        "total_count": total,
        "page": page.parse::<u64>().unwrap_or(1),
        "page_size": size.parse::<u64>().unwrap_or(50),
    }))
}

/// The regions to ask: the one named, or all three.
fn regions(args: &Value) -> Result<Vec<&str>> {
    match arg_str(args, "region")? {
        Some(r) => Ok(vec![s3api::check_region(r)?]),
        None => Ok(s3api::REGIONS.to_vec()),
    }
}

/// Buckets in one region or in all three. With no region named, a region that fails is
/// reported beside the others.
async fn buckets_list(api: &Api<'_>, args: &Value) -> Result<Value> {
    let named = arg_str(args, "region")?.is_some();
    let mut buckets = Vec::new();
    let mut errors = Vec::new();
    for region in regions(args)? {
        match api.buckets(region).await {
            Ok(found) => buckets.extend(
                found
                    .into_iter()
                    .map(|b| json!({ "region": region, "name": b.name, "created": b.created })),
            ),
            Err(e) if named => return Err(e),
            Err(e) => errors.push(json!({ "region": region, "error": e.to_string() })),
        }
    }
    Ok(json!({ "buckets": buckets, "errors": errors }))
}

/// One page of a bucket's objects.
async fn objects_list(api: &Api<'_>, args: &Value) -> Result<Value> {
    let region = s3api::check_region(need_str(args, "region")?)?;
    let bucket = need_str(args, "bucket")?;
    s3api::check_bucket(bucket)?;
    let max = arg_num(args, "max_keys", 100, 1, 1000)? as u32;
    let query = s3::list_objects_query(
        arg_str(args, "prefix")?,
        arg_str(args, "delimiter")?,
        max,
        arg_str(args, "token")?,
    );
    let res = api
        .s3("GET", region, &s3::object_path(bucket, ""), &query, &[])
        .await?;
    let page = s3::parse_list_objects(&res.body).map_err(|e| api.err(e.to_string(), &[]))?;
    Ok(json!({
        "region": region,
        "bucket": bucket,
        "objects": page.objects,
        "prefixes": page.prefixes,
        "next_token": page.next,
    }))
}

/// One object's size, type and time, without its content.
async fn object_head(api: &Api<'_>, args: &Value) -> Result<Value> {
    let region = s3api::check_region(need_str(args, "region")?)?;
    let bucket = need_str(args, "bucket")?;
    s3api::check_bucket(bucket)?;
    let key = need_str(args, "key")?;
    if key.len() > 1024 {
        return Err(bad("`key` is at most 1024 bytes"));
    }
    let res = api
        .s3("HEAD", region, &s3::object_path(bucket, key), &[], &[])
        .await?;
    let mut out = s3api::head_view(bucket, key, &res);
    out["region"] = json!(region);
    Ok(out)
}

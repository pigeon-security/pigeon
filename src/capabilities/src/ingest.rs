//! The `data-ingest` capability (decision 13 of docs/plans/ingest.md): plan, start, watch, report on
//! and stop a run that copies only what R2 does not already hold.
//!
//! It is latent. Nothing starts without a person's passkey approval of an exact plan, and no
//! plan applies until the credentials it needs are configured. Planning uses source credentials
//! to sample the collection. Credentials are never logged or included in a response.
//!
//! The commit a run is built from is the one this Worker was built from (`PIGEON_COMMIT` at
//! build time). A local build has none, so `data_ingest_plan` takes an optional full `commit`, which
//! also overrides the built-in one.

pub mod backblaze;
pub mod connections;
pub mod digitalocean;
pub mod digitalocean_compute;
pub mod hosts;
mod managed_destination;
pub mod multipart;
pub mod net;
pub mod scaleway;
pub mod scaleway_compute;
pub mod scaleway_source;
pub mod throughput;

pub const NAME: &str = "data-ingest";
const INGEST_VIEWS: &[crate::instances::tools::View] = &[crate::instances::tools::View {
    area: "ingest",
    id: "runs",
    title: "Runs",
    order: 10,
    since: None,
}];

const DATA_TOOLS: &[crate::instances::tools::Registration] = &[
    crate::instances::tools::Registration {
        name: connections::LIST,
        description: None,
        arguments: &["compute_preview"],
    },
    crate::instances::tools::Registration {
        name: connections::VERIFY,
        description: None,
        arguments: &["connection"],
    },
    crate::instances::tools::Registration {
        name: "data_ingest_plan",
        description: Some("Plan an ingest using the approved source and private Data destination."),
        arguments: &[
            "source",
            "connection",
            "prefix",
            "size_bytes",
            "objects",
            "continue",
            "only",
            "max_live_bytes",
            "shards",
            "dry_run",
            "keep_archives",
            "fingerprints",
            "commit",
            "scratch_gib",
            "max_minutes",
            "archive_inflated_gib",
            "archive_member_gib",
        ],
    },
    crate::instances::tools::Registration {
        name: multipart::CHECK,
        description: Some("Check large-file uploads: run the Data Worker's multipart canary once."),
        arguments: &[],
    },
    crate::instances::tools::Registration {
        name: "data_ingest_start",
        description: Some("Start the approved ingest plan for this instance."),
        arguments: &["plan"],
    },
    crate::instances::tools::Registration {
        name: "data_ingest_report",
        description: Some("Read an ingestion report owned by this instance."),
        arguments: &["run"],
    },
    crate::instances::tools::Registration {
        name: "data_ingest_status",
        description: Some("Read ingest progress and clean up a finished legacy run."),
        arguments: &["run"],
    },
    crate::instances::tools::Registration {
        name: "data_ingest_stop",
        description: Some("Stop a run owned by this instance."),
        arguments: &["run"],
    },
];

fn data_enabled(record: &instances::record::Instance) -> bool {
    record.capabilities.contains_key(NAME) && crate::data_store::enabled(record)
}

pub fn data_instance_tools(
    instance: &instances::record::Instance,
) -> serde_json::Result<crate::instances::tools::Enabled> {
    Ok(crate::instances::tools::Enabled::when(
        data_enabled(instance),
        DATA_TOOLS,
        INGEST_VIEWS,
    ))
}

/// The source and destination a legacy run (a declared [`Target`], not `data-ingest`) copies.
pub struct Bindings<'a> {
    pub source: &'a str,
    pub bucket: &'a str,
    pub prefix: &'a str,
    pub destination_bucket: &'a str,
}

use pigeon_core::mcp::Tool;
use pigeon_core::r2::{self, Bucket};
use pigeon_core::s3::{self, Endpoint};
use pigeon_core::secrets_store;
use pigeon_ingest::estimate::{self, Host};
use pigeon_ingest::manifest::{self, DestSpec, JobSpec, SourceSpec};
use pigeon_ingest::protocol::{self, StartRequest, StopRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::{Date, Math};
use worker::{Env, Error, Method, Result};

use crate::data_read::{self, Account, Provider};
use crate::instances::{self, record::AccountRef};
use crate::passkeys::Passkeys;
use crate::users::directory;
use crate::{Call, Capability, Context, Slot, arg, to_value, unknown};

/// The approval kind of a run.
pub const KIND: &str = "ingest_run";
/// How long a plan waits for approval: 24 hours.
pub const PENDING_MS: i64 = 24 * 3_600_000;
/// How long a run's record is kept: a century.
const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;
/// The most rows of a list `data_ingest_report` answers.
pub const REPORT_ROWS: usize = 200;
/// The share of bytes a plan assumes unique (decision 13.7).
const UNIQUE_SHARE: f64 = 0.3;

/// An instance that takes ingest, declared in code.
#[derive(Debug, Clone)]
pub struct Target {
    /// The instance id, such as `willowgraysen`.
    pub instance: String,
    /// Its R2 bucket for the data: `{instance}-data` unless given.
    pub bucket: String,
}

impl Target {
    /// A target with the default bucket `{instance}-data`.
    pub fn new(instance: &str) -> Self {
        Self {
            instance: instance.into(),
            bucket: format!("{instance}-data"),
        }
    }
}

/// The ingest capability.
pub struct Ingest {
    /// The instances that take ingest.
    pub targets: Vec<Target>,
    /// The declared storage accounts, which are the sources.
    pub accounts: Vec<Account>,
    /// Gives the platform's host, for linked accounts.
    pub passkeys: Passkeys,
}

/// What pigeon keeps of a run it started, in the Directory as `ingest:run:{run}`. No key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// The run id.
    pub run: String,
    /// The instance.
    pub instance: String,
    /// `digitalocean`, `scaleway` or `cloudflare`.
    pub host: String,
    /// The source account id.
    pub source: String,
    /// The source bucket.
    pub bucket: String,
    /// The provider region of the host.
    pub region: String,
    /// The VM's id at its provider, once there is one.
    pub vm: Option<String>,
    /// Extra Scaleway resources created with the server. Older runs have no snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scaleway_volumes: Vec<String>,
    /// Cleanup and audit state for managed Scaleway runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaleway: Option<scaleway::Lifecycle>,
    /// Whether the VM was deleted (or the Container stopped).
    pub vm_deleted: bool,
    /// The bound R2 destination for new runs. Legacy rows have no snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<DestSpec>,
    /// The data Worker used for this run, for safe cleanup of Cloudflare runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_worker: Option<String>,
    /// The approval that allowed it.
    pub approval: String,
    /// The commit the runner was built from.
    pub commit: String,
    /// Whether it is a dry run.
    pub dry_run: bool,
    /// When it was started, in unix milliseconds.
    pub started_ms: i64,
}

/// Runtime provisioner values that must remain the same from plan to start.
fn execution_binding(host: Host, worker: Option<&str>, host_key: Option<&str>) -> Result<Value> {
    match host {
        Host::Cloudflare => {
            let url = worker.ok_or_else(|| bad("the data Worker's URL is not in the Directory"))?;
            let parsed = worker::Url::parse(url)
                .map_err(|_| bad("the data Worker has no valid HTTPS origin"))?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || url.ends_with('/')
            {
                return Err(bad(
                    "the data Worker must have an HTTPS origin with no trailing slash",
                ));
            }
            Ok(json!({ "data_worker": url }))
        }
        Host::Scaleway => {
            let key = host_key.ok_or_else(|| bad("the Scaleway provisioning key is missing"))?;
            let (project, _) = hosts::scaleway_parts(key).map_err(bad)?;
            Ok(json!({ "project": project }))
        }
        Host::DigitalOcean => Ok(json!({ "host": "digitalocean" })),
        Host::Local => Err(bad("a local run is not started from here")),
    }
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

pub(crate) fn run_key(run: &str) -> String {
    format!("ingest:run:{run}")
}

fn index_key(instance: &str) -> String {
    format!("ingest:runs:{instance}")
}

fn now_ms() -> i64 {
    Date::now() as i64
}

/// Today as `yyyymmdd`, UTC.
fn today() -> String {
    let d = Date::new_0();
    format!(
        "{:04}{:02}{:02}",
        d.get_utc_full_year(),
        d.get_utc_month() + 1,
        d.get_utc_date()
    )
}

fn nonce() -> u32 {
    (Math::random() * 16_777_216.0) as u32
}

async fn setting(env: &Env, key: &str) -> Result<Option<String>> {
    Ok(directory(env, "setting.get", json!({ "key": key }))
        .await?
        .as_str()
        .map(str::to_string))
}

async fn keep(env: &Env, key: &str, value: &str) -> Result<()> {
    directory(
        env,
        "setting.put",
        json!({ "key": key, "value": value, "ttl_ms": KEPT_MS }),
    )
    .await?;
    Ok(())
}

async fn secret_value(env: &Env, name: &str) -> Option<String> {
    let store = env.secret_store(name).ok()?;
    secrets_store::read(&store)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

async fn save_run(env: &Env, rec: &RunRecord) -> Result<()> {
    keep(env, &run_key(&rec.run), &serde_json::to_string(rec)?).await
}

async fn runs_of(env: &Env, instance: &str) -> Result<Vec<String>> {
    Ok(match setting(env, &index_key(instance)).await? {
        Some(text) => serde_json::from_str(&text)?,
        None => Vec::new(),
    })
}

/// The records of `instance`'s runs, oldest first. It only reads: unlike `data_ingest_status` for
/// one run, it never deletes a VM, so the console lists runs with it (`app_ingest_runs`).
pub async fn runs(env: &Env, instance: &str) -> Result<Vec<RunRecord>> {
    let mut out = Vec::new();
    for id in runs_of(env, instance).await? {
        if let Some(text) = setting(env, &run_key(&id)).await? {
            let record: RunRecord = serde_json::from_str(&text)?;
            if record.instance == instance {
                out.push(record);
            }
        }
    }
    out.extend(digitalocean::runs(env, instance).await?);
    out.sort_by_key(|record| record.started_ms);
    Ok(out)
}

async fn run_of(env: &Env, instance: &str, run: &str) -> Result<RunRecord> {
    let text = setting(env, &run_key(run)).await?.ok_or_else(|| {
        bad(format!(
            "pigeon has no record of run `{run}`; it only acts on runs it started"
        ))
    })?;
    let rec: RunRecord = serde_json::from_str(&text)?;
    if rec.instance != instance {
        return Err(bad(format!("run `{run}` belongs to another instance")));
    }
    Ok(rec)
}

fn tool(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
) -> Tool {
    Tool {
        name,
        description,
        input_schema: json!({ "type": "object", "properties": properties, "required": required }),
    }
}

fn bucket_ok(name: &str) -> bool {
    (3..=63).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

/// The instance's Cloudflare account id, when its record says it is linked and knows it.
fn linked_id(i: &instances::record::Instance) -> Option<String> {
    match &i.account {
        AccountRef::Linked {
            account_id: Some(id),
        } => Some(id.clone()),
        _ => None,
    }
}

/// The plan is usable only while its stored destination and source match today's bindings.
pub fn check_job_binding(
    job: &JobSpec,
    account_id: &str,
    destination_bucket: &str,
    source: &Account,
) -> std::result::Result<(), String> {
    if job.dest.endpoint != hosts::r2_endpoint(account_id) || job.dest.bucket != destination_bucket
    {
        return Err("the ingest destination changed since planning; plan again".into());
    }
    if !matches!(&job.source, SourceSpec::S3 { account, endpoint, region, bucket, .. }
        if account == &source.id() && endpoint == &source.endpoint && region == &source.region
            && bucket_ok(bucket) && data_read::bucket_allowed(source, bucket))
    {
        return Err("the ingest source is no longer approved; plan again".into());
    }
    Ok(())
}

/// Caps every list of a report at [`REPORT_ROWS`] rows, noting the real counts.
pub fn cap_report(report: &mut Value) {
    let Some(map) = report.as_object_mut() else {
        return;
    };
    let mut totals = serde_json::Map::new();
    for (k, v) in map.iter_mut() {
        if let Some(rows) = v.as_array_mut()
            && rows.len() > REPORT_ROWS
        {
            totals.insert(k.clone(), json!(rows.len()));
            rows.truncate(REPORT_ROWS);
        }
    }
    if !totals.is_empty() {
        map.insert("truncated_from".into(), Value::Object(totals));
    }
}

/// The caller can narrow the approved S3 prefix. It cannot choose a provider, endpoint, bucket
/// or destination when running with the data-ingest capability.
pub fn spaces_prefix<'a>(
    approved: &'a str,
    args: &'a Value,
) -> std::result::Result<&'a str, String> {
    let Some(value) = args.get("prefix") else {
        return Ok(approved);
    };
    match value.as_str() {
        Some(prefix) if crate::digitalocean::spaces::allows_prefix(approved, prefix) => Ok(prefix),
        _ => Err("The ingest prefix must be within the approved Spaces prefix.".into()),
    }
}

impl Ingest {
    fn target(&self, instance: &str) -> Result<&Target> {
        self.targets
            .iter()
            .find(|t| t.instance == instance)
            .ok_or_else(|| {
                let known: Vec<&str> = self.targets.iter().map(|t| t.instance.as_str()).collect();
                bad(format!(
                    "instance `{instance}` does not take ingest; declared: {}",
                    known.join(", ")
                ))
            })
    }

    fn bindings<'a>(
        &'a self,
        record: &'a instances::record::Instance,
        args: &'a Value,
    ) -> std::result::Result<Bindings<'a>, String> {
        let target = self.target(&record.id).map_err(|error| error.to_string())?;
        Ok(Bindings {
            source: arg(args, "source").map_err(|error| error.to_string())?,
            bucket: arg(args, "bucket").map_err(|error| error.to_string())?,
            prefix: args["prefix"].as_str().unwrap_or(""),
            destination_bucket: &target.bucket,
        })
    }

    fn destination<'a>(&'a self, record: &'a instances::record::Instance) -> Result<&'a str> {
        Ok(&self.target(&record.id)?.bucket)
    }

    async fn checked_link(
        &self,
        env: &Env,
        record: &instances::record::Instance,
    ) -> Result<pigeon_core::Account> {
        let id =
            linked_id(record).ok_or_else(|| bad("the approved instance has no linked account"))?;
        let linked = crate::linked::account_for(env, self.host_name(), &record.id).await?;
        if linked.id != id {
            return Err(bad(
                "the linked account does not match the approved ingest account",
            ));
        }
        Ok(linked)
    }

    async fn check_current(env: &Env, record: &instances::record::Instance) -> Result<()> {
        if instances::one(env, &record.id).await?.as_ref() != Some(record) {
            return Err(bad(
                "the approved ingest settings changed while starting; plan again",
            ));
        }
        Ok(())
    }

    fn host_name(&self) -> &str {
        &self.passkeys.host
    }

    /// The commit to build: the argument, else the Worker's own build commit.
    fn commit(args: &Value) -> std::result::Result<String, String> {
        let given = args["commit"].as_str().map(str::to_string);
        let built = option_env!("PIGEON_COMMIT")
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        match given.or(built) {
            Some(c) if hosts::is_commit(&c) => Ok(c),
            Some(_) => Err("`commit` must be a full 40-character lowercase hex commit id".into()),
            None => Err("this Worker has no build commit: pass `commit` (a full 40-character commit id on GitHub) to data_ingest_plan".into()),
        }
    }

    /// `data_ingest_plan`: checks, a sample, an estimate and, when nothing is missing, an approval.
    async fn plan(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        if throughput::only_arg(args)?
            && args["connection"].is_null()
            && args["source"] != "backblaze"
        {
            return Err(bad("only \"too_large\" is for Backblaze connections."));
        }
        let caller = cx
            .caller
            .ok_or_else(|| bad("planning needs a known caller"))?;
        let person = caller
            .acting_for
            .as_ref()
            .ok_or_else(|| bad("the caller acts for no person who could approve this"))?;
        let instance = arg(args, "instance")?;
        let env = cx.env;
        let Some(record) = instances::one(env, instance).await? else {
            return Ok(unready(
                vec![format!(
                    "There is no approved record for instance `{instance}`."
                )],
                Value::Null,
                Value::Null,
            ));
        };
        let connection = args
            .get("connection")
            .map(|value| {
                value
                    .as_str()
                    .filter(|name| connections::valid_name(name))
                    .ok_or_else(|| bad("Name an approved bucket connection."))
            })
            .transpose()?;
        let named_scaleway = match connection {
            Some(name) if record.capabilities.contains_key(NAME) => {
                let binding = connections::binding(&record, name)?;
                if args.get("bucket").is_some()
                    || args
                        .get("source")
                        .is_some_and(|source| source.as_str() != Some(binding.provider()))
                {
                    return Err(bad(
                        "The approved connection fixes the provider and bucket.",
                    ));
                }
                if throughput::only_arg(args)?
                    && !matches!(binding, connections::Binding::Backblaze(_))
                {
                    return Err(bad("only \"too_large\" is for Backblaze connections."));
                }
                match binding {
                    connections::Binding::Scaleway(config) => Some(config),
                    connections::Binding::DigitalOcean(_) => {
                        return digitalocean::plan(cx, self.host_name(), &record, args).await;
                    }
                    connections::Binding::Backblaze(_) => {
                        return backblaze::plan(self, cx, &record, args).await;
                    }
                }
            }
            Some(_) => return Err(bad("enable Data Ingest before using a named connection")),
            None => None,
        };
        let managed = if record.capabilities.contains_key(NAME) {
            if named_scaleway.is_none() && args["source"] == "backblaze" {
                return backblaze::plan(self, cx, &record, args).await;
            }
            if throughput::only_arg(args)? {
                return Err(bad("only \"too_large\" is for Backblaze connections."));
            }
            if named_scaleway.is_none() && args["source"] != "scaleway" {
                return digitalocean::plan(cx, self.host_name(), &record, args).await;
            }
            if !data_enabled(&record) {
                return Ok(unready(
                    vec!["Enable Data on this instance first.".into()],
                    Value::Null,
                    Value::Null,
                ));
            }
            if [
                "scratch_gib",
                "max_minutes",
                "archive_inflated_gib",
                "archive_member_gib",
            ]
            .iter()
            .any(|key| args.get(key).is_some())
            {
                return Ok(unready(
                    vec!["Scaleway uses a fixed scratch size, runtime limit and archive limits. Omit the DigitalOcean-specific overrides.".into()],
                    Value::Null,
                    Value::Null,
                ));
            }
            let Some(config) = named_scaleway.or_else(|| scaleway_source::binding(&record)) else {
                return Ok(unready(
                    vec!["Approve this instance's Scaleway source binding first.".into()],
                    Value::Null,
                    Value::Null,
                ));
            };
            if let Err(error) = config.validate() {
                return Ok(unready(vec![error], Value::Null, Value::Null));
            }
            Some(config)
        } else {
            None
        };
        let managed_account = managed.map(|config| config.account(instance));
        let managed_id = managed_account.as_ref().map(Account::id);
        let store_bucket = crate::data_store::store_name(&record);
        let bindings = if let (Some(config), Some(source)) = (managed, managed_id.as_deref()) {
            match config.prefix(args) {
                Ok(prefix) => Bindings {
                    source,
                    bucket: &config.bucket,
                    prefix,
                    destination_bucket: &store_bucket,
                },
                Err(error) => return Ok(unready(vec![error], Value::Null, Value::Null)),
            }
        } else {
            match self.bindings(&record, args) {
                Ok(bindings) => bindings,
                Err(error) => return Ok(unready(vec![error], Value::Null, Value::Null)),
            }
        };
        let linked = match self.checked_link(env, &record).await {
            Ok(linked) => linked,
            Err(error) => return Ok(unready(vec![error.to_string()], Value::Null, Value::Null)),
        };
        let Bindings {
            source,
            bucket,
            prefix,
            destination_bucket,
        } = bindings;
        let continued = match args.get("continue").filter(|v| !v.is_null()) {
            Some(prior) => {
                let prior = prior
                    .as_str()
                    .filter(|run| pigeon_ingest::container_job::valid_run(run))
                    .ok_or_else(|| bad("continue must be a run id"))?;
                Some(throughput::continuation(&linked, destination_bucket, instance, prior).await?)
            }
            None => None,
        };
        let flag = |name: &str, default: bool| args[name].as_bool().unwrap_or(default);
        let (dry_run, keep_archives, fingerprints) = (
            flag(
                "dry_run",
                continued.as_ref().is_none_or(|(prior, _)| prior.dry_run),
            ),
            flag("keep_archives", true),
            flag("fingerprints", false),
        );
        if connection.is_some() && !keep_archives {
            return Err(bad("A named Scaleway run must retain archive containers."));
        }
        let mut missing = Vec::new();

        // The source account and its keys.
        let account = match managed_account
            .as_ref()
            .map_or_else(|| data_read::find(&self.accounts, source), Ok)
        {
            Ok(a) if bucket_ok(bucket) && data_read::bucket_allowed(a, bucket) => Some(a),
            Ok(a) => {
                missing.push(format!(
                    "Bucket `{bucket}` is not allowed for `{}`. Ask an agent to add it to the account's buckets in `data_read_accounts()`.",
                    a.id()
                ));
                None
            }
            Err(e) => {
                missing.push(format!(
                    "{e}. Ask an agent to declare the account in `data_read_accounts()` in src/platform/src/settings.rs."
                ));
                None
            }
        };
        if account.is_some_and(|a| a.provider == Provider::Scaleway) && connection.is_none() {
            return Ok(unready(
                vec!["Choose an approved named Scaleway bucket connection with an owning-account source and compute key.".into()],
                Value::Null, Value::Null,
            ));
        }
        let mut source_keys = None;
        let mut source_pin = None;
        if let Some(a) = account {
            if let Some(name) = connection {
                source_pin = connections::owned_reference(&record, name)?;
                if source_pin.is_none() {
                    missing.push("Register this source key in the linked account and apply the Data Worker plan.".into());
                }
            } else {
                let source_slot = a.binding();
                match secret_value(env, &source_slot).await {
                    Some(v) => source_keys = s3::parse_keys(&v).ok(),
                    None => missing.push(format!("Secret {source_slot} is not bound.")),
                }
            }
            if a.provider == Provider::Backblaze
                && setting(env, &format!("data_key:{instance}"))
                    .await?
                    .is_none()
            {
                missing.push(format!(
                    "The data Worker for `{instance}` has no key in the Directory (`data_key:{instance}`). Deploy the data Worker first."
                ));
            }
        }
        if connection.is_none() && secret_value(env, &hosts::r2_slot(instance)).await.is_none() {
            missing.push(format!(
                "Secret `{}` is not bound. In {}'s Cloudflare dashboard make an R2 API token for bucket `{}` only, Object Read & Write, set it on the app's Access page (Credentials), then ask an agent to bind it.",
                hosts::r2_slot(instance),
                instance,
                destination_bucket
            ));
        }
        let commit = match Self::commit(args) {
            Ok(c) => Some(c),
            Err(e) => {
                missing.push(e);
                None
            }
        };

        // A sample of the source and the estimate, when the source can be read.
        let mut sample = Value::Null;
        let mut est = Value::Null;
        let mut sampled_listing: Option<s3::Listing> = None;
        if let Some(a) = account {
            let listing = if let Some(pin) = source_pin.as_ref() {
                Some(connections::owned_sample(env, &record, pin, prefix).await)
            } else if let Some(keys) = source_keys.as_ref() {
                let query = s3::list_objects_query(
                    Some(prefix).filter(|p| !p.is_empty()),
                    None,
                    1000,
                    None,
                );
                let endpoint = Endpoint {
                    base: a.endpoint.clone(),
                    region: a.region.clone(),
                };
                Some(
                    async {
                        let mut response = s3::send(
                            Method::Get,
                            &endpoint,
                            &s3::object_path(bucket, ""),
                            &query,
                            &[],
                            keys,
                        )
                        .await?;
                        s3::parse_list_objects(&response.text().await?)
                    }
                    .await,
                )
            } else {
                None
            };
            if let Some(listing) = listing {
                match listing {
                    Ok(listing) => {
                        let count = listing.objects.len() as u64;
                        let bytes = listing.objects.iter().try_fold(0_u64, |sum, object| {
                            sum.checked_add(object.size)
                                .ok_or_else(|| bad("source byte count overflow"))
                        })?;
                        let more = listing.next.is_some();
                        // The declared totals count for every source, a named connection
                        // included.
                        let (total_bytes, total_objects, extrapolated) = estimate::totals(
                            count,
                            bytes,
                            more,
                            args["size_bytes"].as_u64(),
                            throughput::objects_arg(args)?,
                        );
                        sampled_listing = Some(listing.clone());
                        sample = json!({
                            "objects": count, "bytes": bytes, "first_page_only": more,
                            "note": if more && args["size_bytes"].is_null() {
                                "Only the first page was listed, so the estimate covers that page. Pass `size_bytes` (the whole size) for a whole-run estimate."
                            } else { "" },
                        });
                        let host = hosts::host_for(a.provider);
                        let measured = match host {
                            Host::Scaleway => {
                                throughput::measured(
                                    env,
                                    &linked,
                                    destination_bucket,
                                    instance,
                                    "scaleway",
                                )
                                .await
                            }
                            _ => None,
                        };
                        est = to_value(throughput::plan_estimate(
                            host,
                            total_bytes,
                            total_objects,
                            extrapolated,
                            measured,
                        ));
                    }
                    Err(e) => missing.push(format!(
                        "The source could not be listed with the bound key: {e}"
                    )),
                }
            }
        }

        let (Some(a), Some(commit)) = (account, commit.as_ref()) else {
            return Ok(unready(missing, est, sample));
        };
        if !missing.is_empty() {
            return Ok(unready(missing, est, sample));
        }
        let mut job = hosts::job_spec(&hosts::JobInputs {
            instance,
            account: a,
            bucket,
            prefix,
            dest_bucket: destination_bucket,
            account_id: &linked.id,
            dry_run,
            keep_archives,
            fingerprints,
            commit,
            day: &today(),
            nonce: nonce(),
        });
        let host = hosts::host_for(a.provider);
        if let Ok(planned) = serde_json::from_value::<estimate::Estimate>(est.clone())
            && host == Host::Scaleway
        {
            job.max_minutes = Some(throughput::deadline(host, &planned, None)?);
        }
        if let Some((prior, resume)) = &continued {
            if prior.source != job.source || prior.dry_run != job.dry_run {
                return Err(bad(
                    "a continuation keeps the continued run's source, prefix and dry-run setting; plan a new run instead",
                ));
            }
            job.resume = Some(resume.clone());
        }
        let data_worker = if host == Host::Cloudflare {
            setting(env, &format!("data_url:{instance}")).await?
        } else {
            None
        };
        let broker = if let Some(config) = managed {
            match scaleway_compute::ready(env, &record, &config.project, &a.region).await {
                Ok(broker) => Some(broker),
                Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
            }
        } else {
            None
        };
        let mut execution = match broker.as_ref() {
            Some((pin, origin)) => {
                json!({"project": pin.project, "compute": pin, "compute_origin": origin})
            }
            None => match execution_binding(host, data_worker.as_deref(), None) {
                Ok(execution) => execution,
                Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
            },
        };
        if let Some(config) = managed {
            if execution["project"] != config.project {
                return Ok(unready(
                    vec!["The Scaleway compute credential belongs to a different project.".into()],
                    est,
                    sample,
                ));
            }
            execution["scaleway_source"] = to_value(config);
        }
        if host == Host::Scaleway {
            match scaleway::audit_bucket(env, self.host_name(), &record).await {
                Ok(bucket) if bucket != destination_bucket => {
                    execution["jobs_audit_bucket"] = json!(bucket);
                    execution["max_runtime_ms"] = json!(scaleway::MAX_RUN);
                }
                Ok(_) => {
                    return Ok(unready(
                        vec!["Scaleway ingest needs a Jobs bucket separate from Data.".into()],
                        est,
                        sample,
                    ));
                }
                Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
            }
        }
        let mut live_notes = Vec::new();
        let request = if let Some(pin) = source_pin {
            let destination = match managed_destination::ready(env, &record).await {
                Ok(binding) => binding,
                Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
            };
            let estimate: estimate::Estimate = serde_json::from_value(est.clone())?;
            if let Some(listing) = &sampled_listing {
                match throughput::live(args, dry_run, listing, destination.multipart)? {
                    Ok(allowed) => {
                        job.skip_over = allowed.skip_over;
                        live_notes = allowed.notes;
                    }
                    Err(why) => return Ok(unready(vec![why], est, sample)),
                }
            }
            let origin = match connections::owned_origin(env, &record, &pin).await {
                Ok(origin) => origin,
                Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
            };
            execution["source_origin"] = json!(origin);
            execution["connection"] = to_value(&pin);
            execution["managed_destination"] =
                to_value(destination.review(estimate.bytes, estimate.objects, dry_run));
            hosts::scaleway_broker_host_request(&job, &a.region)
        } else {
            hosts::host_request(host, &job, &a.region)
        };
        let bucket_here = r2::bucket_exists(
            &linked,
            &Bucket {
                name: destination_bucket.to_string(),
                jurisdiction: None,
            },
        )
        .await?;
        if managed.is_some() && !bucket_here {
            return Ok(unready(
                vec!["Apply the approved Data instance plan to provision the destination bucket first.".into()],
                est,
                sample,
            ));
        }
        if managed.is_some() {
            r2::require_private_bucket(
                &linked,
                &Bucket {
                    name: destination_bucket.into(),
                    jurisdiction: None,
                },
            )
            .await?;
        }
        let mut lines = Vec::new();
        if let Some(resume) = &job.resume {
            lines.push(throughput::continue_line(resume));
        }
        if let Ok(planned) = serde_json::from_value::<estimate::Estimate>(est.clone()) {
            lines.push(throughput::time_line(&planned));
        }
        lines.extend(live_notes.iter().cloned());
        lines.push(if bucket_here {
            format!(
                "R2 bucket `{}` already exists in `{instance}`'s account.",
                destination_bucket
            )
        } else {
            format!(
                "Create R2 bucket `{}` in `{instance}`'s account.",
                destination_bucket
            )
        });
        lines.push(format!(
            "Write `{}` to the bucket.",
            manifest::job_key(&job.run)
        ));
        lines.push(match host {
            Host::DigitalOcean => format!(
                "Create a {} Droplet in {} that builds the runner at commit {commit}, runs it once and powers off.",
                hosts::DO_SIZE, a.region
            ),
            Host::Scaleway => format!(
                "Create a {} Instance in {} with dedicated 200 GB scratch storage, build the container at commit {commit}, run it once and power off.",
                hosts::SCW_TYPE,
                hosts::zone(&a.region)
            ),
            _ => format!("Ask `{instance}`'s data Worker to run the runner as a Cloudflare Container (commit {commit})."),
        });
        lines.push(if dry_run {
            "Dry run: files are read and decided on; nothing is written under blobs/ or quarantine/.".into()
        } else {
            "Real run: files R2 does not hold are copied into the bucket.".to_string()
        });
        if let Some(review) = execution.get("managed_destination") {
            lines.push(format!(
                "Use a Data Worker run grant limited to {} write bytes, {} write attempts and 16 MiB per atomic payload.",
                review["max_bytes"], review["max_objects"]
            ));
        }
        let mut payload = hosts::approval_payload(&job, &request);
        payload["execution"] = execution.clone();
        let kind = pigeon_core::approval::Kind::Other(KIND.into());
        let digest = pigeon_core::approval::payload_digest(&kind, &job.run, &payload);
        let made = directory(
            env,
            "approval.create",
            json!({
                "kind": kind.name(),
                "subject": job.run,
                "digest": digest,
                "summary": { "plan": lines, "estimate": est, "execution": execution },
                "payload": payload,
                "caller_id": caller.user.id,
                "approver_id": person.id,
                "pending_ms": PENDING_MS,
            }),
        )
        .await?;
        Ok(json!({
            "applies": true,
            "missing": [],
            "plan": lines,
            "job": job,
            "host_request": request,
            "execution": execution,
            "estimate": est,
            "sample": sample,
            "approval": made["record"]["id"],
            "waits_until_ms": made["record"]["expires_ms"],
            "note": "Nothing starts until the person approves this plan on the console's Approvals tab; then the platform starts it once, as you (data_ingest_start). approvals_status {id: <this approval>} says how it went.",
        }))
    }

    /// `data_ingest_start`: takes the approval, writes the job, starts the host, records the run.
    async fn start(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("starting needs a known caller"))?;
        let env = cx.env;
        let approval_id = arg(args, "plan")?;
        let got = directory(env, "approval.get", json!({ "id": approval_id })).await?;
        if got["kind"] != KIND {
            return Err(bad(format!("there is no ingest approval {approval_id}")));
        }
        if digitalocean::is_plan(&got["payload"]) {
            return digitalocean::start(cx, self.host_name(), approval_id, &got, args).await;
        }
        if backblaze::managed(&got["payload"]) {
            return backblaze::start(self, cx, args, approval_id, &got).await;
        }
        let job: JobSpec = serde_json::from_value(got["payload"]["job"].clone())
            .map_err(|_| bad("the approval holds no readable job"))?;
        if args
            .get("instance")
            .is_some_and(|value| value.as_str() != Some(&job.instance))
        {
            return Err(bad("the ingest approval belongs to a different instance"));
        }
        if got["subject"].as_str().is_some_and(|s| s != job.run) {
            return Err(bad("the approval's run does not match its job"));
        }
        let SourceSpec::S3 {
            account: source_id,
            region,
            bucket: source_bucket,
            prefix,
            ..
        } = &job.source
        else {
            return Err(bad("only S3 sources start from here"));
        };
        let record = instances::one(env, &job.instance)
            .await?
            .ok_or_else(|| bad("the approved ingest instance no longer exists"))?;
        let source_args = json!({ "source": source_id, "bucket": source_bucket, "prefix": prefix });
        let approved_execution = &got["payload"]["execution"];
        let named_destination = match (
            approved_execution.get("connection"),
            approved_execution.get("managed_destination"),
        ) {
            (Some(pin), Some(review)) if approved_execution.get("scaleway_source").is_some() => {
                let pin: connections::Pin = serde_json::from_value(pin.clone())?;
                let review: managed_destination::Reviewed = serde_json::from_value(review.clone())?;
                Some((pin, review))
            }
            (None, None) => None,
            _ => {
                return Err(bad(
                    "the Scaleway approval has incomplete managed bindings; plan again",
                ));
            }
        };
        let managed = if approved_execution.get("scaleway_source").is_some() {
            if !data_enabled(&record) {
                return Err(bad("Data Ingest or Data has been disabled; plan again"));
            }
            let config = if let Some((pin, _)) = named_destination.as_ref() {
                let connections::Binding::Scaleway(config) =
                    connections::binding(&record, &pin.name)?
                else {
                    return Err(bad(
                        "the approved connection is no longer a Scaleway source; plan again",
                    ));
                };
                config
            } else {
                scaleway_source::binding(&record)
                    .ok_or_else(|| bad("the approved Scaleway source was removed; plan again"))?
            };
            Some(config)
        } else {
            None
        };
        let managed_account = managed.map(|config| config.account(&record.id));
        let store_bucket = crate::data_store::store_name(&record);
        let destination_bucket = if let Some(config) = managed {
            config.prefix(&source_args).map_err(bad)?;
            store_bucket.as_str()
        } else {
            self.bindings(&record, &source_args)
                .map_err(bad)?
                .destination_bucket
        };
        let account = managed_account
            .as_ref()
            .map_or_else(|| data_read::find(&self.accounts, source_id), Ok)?;
        let account_id =
            linked_id(&record).ok_or_else(|| bad("the approved instance has no linked account"))?;
        check_job_binding(&job, &account_id, destination_bucket, account).map_err(bad)?;
        let linked = self.checked_link(env, &record).await?;
        let host = hosts::host_for(account.provider);
        let broker = if host == Host::Scaleway {
            let config =
                managed.ok_or_else(|| bad("plan with an owning-account Scaleway source"))?;
            let (pin, review) = named_destination
                .as_ref()
                .ok_or_else(|| bad("plan with a named Scaleway source"))?;
            let (compute, origin) =
                scaleway_compute::ready(env, &record, &config.project, region).await?;
            let source_origin = approved_execution["source_origin"]
                .as_str()
                .ok_or_else(|| {
                    bad("the Scaleway plan predates the owning-account runner; plan again")
                })?;
            let deadline = now_ms() + scaleway::MAX_RUN;
            let source =
                connections::owned_runner(env, &record, pin, source_origin, &job, deadline).await?;
            let destination = managed_destination::ready(env, &record).await?.issue(
                &job,
                now_ms(),
                deadline,
                review,
            )?;
            Some((
                origin,
                pigeon_ingest::scaleway::BrokerLaunch {
                    pin: compute,
                    region: region.clone(),
                    job: job.clone(),
                    deadline_ms: deadline,
                    source,
                    destination,
                },
            ))
        } else {
            None
        };
        // Read every key before the approval is spent, so a missing one costs nothing.
        let unbound = |name: &str| bad(format!("secret `{name}` is not bound"));
        let source_slot = managed.map_or_else(
            || account.binding(),
            |_| scaleway_source::source_slot(&record.id),
        );
        let source_keys = if broker.is_some() {
            String::new()
        } else {
            let keys = secret_value(env, &source_slot)
                .await
                .ok_or_else(|| unbound(&source_slot))?;
            s3::parse_keys(&keys)?;
            keys
        };
        let dest_keys = if named_destination.is_none() {
            let r2_slot = hosts::r2_slot(&job.instance);
            let keys = secret_value(env, &r2_slot)
                .await
                .ok_or_else(|| unbound(&r2_slot))?;
            s3::parse_keys(&keys)?;
            keys
        } else {
            String::new()
        };
        let compute_slot = if broker.is_some() {
            None
        } else {
            hosts::host_slot(account)
        };
        let host_key = match compute_slot.as_deref() {
            Some(slot) => Some(secret_value(env, slot).await.ok_or_else(|| unbound(slot))?),
            None => None,
        };
        let data = if host == Host::Cloudflare {
            let key = setting(env, &format!("data_key:{}", job.instance))
                .await?
                .ok_or_else(|| {
                    bad("the data Worker has no key: deploy it first (decision 13.9 step 6)")
                })?;
            let url = setting(env, &format!("data_url:{}", job.instance))
                .await?
                .ok_or_else(|| {
                    bad("the data Worker's URL is not in the Directory (`data_url:{instance}`)")
                })?;
            Some((url, key))
        } else {
            None
        };
        // Rebuild what was approved; the digest must still match.
        let started_ms = now_ms();
        let request = if broker.is_some() {
            hosts::scaleway_broker_host_request(&job, region)
        } else {
            hosts::host_request(host, &job, region)
        };
        let kind = pigeon_core::approval::Kind::Other(KIND.into());
        let mut execution = if let Some((origin, launch)) = broker.as_ref() {
            json!({"project": launch.pin.project, "compute": launch.pin, "compute_origin": origin, "source_origin": launch.source.origin})
        } else {
            execution_binding(
                host,
                data.as_ref().map(|(url, _)| url.as_str()),
                host_key.as_deref(),
            )?
        };
        if let Some(config) = managed {
            if execution["project"] != config.project {
                return Err(bad(
                    "the Scaleway compute credential belongs to a different project",
                ));
            }
            execution["scaleway_source"] = to_value(config);
            r2::require_private_bucket(
                &linked,
                &Bucket {
                    name: destination_bucket.into(),
                    jurisdiction: None,
                },
            )
            .await?;
        }
        let scaleway_audit_bucket = if host == Host::Scaleway {
            let bucket = scaleway::audit_bucket(env, self.host_name(), &record).await?;
            if bucket == destination_bucket {
                return Err(bad(
                    "Scaleway ingest needs a Jobs bucket separate from Data",
                ));
            }
            execution["jobs_audit_bucket"] = json!(bucket);
            execution["max_runtime_ms"] = json!(scaleway::MAX_RUN);
            Some(bucket)
        } else {
            None
        };
        if let Some((pin, review)) = named_destination {
            execution["connection"] = to_value(&pin);
            execution["managed_destination"] = to_value(&review);
        }
        let mut payload = hosts::approval_payload(&job, &request);
        // Earlier DigitalOcean plans already named their fixed API destination. Other
        // providers need a new plan if they did not record their configurable target.
        if !got["payload"]["execution"].is_null() || host != Host::DigitalOcean {
            if got["payload"]["execution"] != execution {
                return Err(bad(
                    "the ingest execution target changed or was never bound; plan again",
                ));
            }
            payload["execution"] = execution;
        }
        let digest = pigeon_core::approval::payload_digest(&kind, &job.run, &payload);
        Self::check_current(env, &record).await?;
        if let Some((_, launch)) = broker.as_ref()
            && scaleway_compute::active(env, &record.id, &launch.pin.project)
                .await?
                .as_ref()
                != Some(&launch.pin)
        {
            return Err(bad("the Scaleway compute credential changed; plan again"));
        }
        let taken = directory(
            env,
            "approval.take",
            json!({ "id": approval_id, "digest": digest, "caller_id": caller.user.id }),
        )
        .await?;
        if let Some(why) = taken["refused"].as_str() {
            return Err(bad(format!(
                "the plan does not run: {why}. If anything changed since it was approved, plan again."
            )));
        }
        Self::check_current(env, &record).await?;
        let dest = Bucket {
            name: destination_bucket.to_string(),
            jurisdiction: None,
        };
        let bucket_created = if managed.is_some() {
            false
        } else {
            r2::ensure_bucket(&linked, &dest).await?.created
        };
        r2::put(
            &linked,
            &dest,
            &manifest::job_key(&job.run),
            "application/json",
            serde_json::to_vec(&job)?,
        )
        .await?;
        let mut rec = RunRecord {
            run: job.run.clone(),
            instance: job.instance.clone(),
            host: host_label(host).into(),
            source: source_id.clone(),
            bucket: source_bucket.clone(),
            region: region.clone(),
            vm: None,
            scaleway_volumes: Vec::new(),
            scaleway: None,
            vm_deleted: false,
            destination: Some(job.dest.clone()),
            data_worker: data.as_ref().map(|(url, _)| url.clone()),
            approval: approval_id.to_string(),
            commit: job.commit.clone(),
            dry_run: job.dry_run,
            started_ms,
        };
        let mut runs = runs_of(env, &job.instance).await?;
        runs.push(job.run.clone());
        keep(
            env,
            &index_key(&job.instance),
            &serde_json::to_string(&runs)?,
        )
        .await?;
        save_run(env, &rec).await?;
        Self::check_current(env, &record).await?;
        match host {
            Host::DigitalOcean => {
                let token = host_key.as_deref().unwrap_or_default();
                let body = hosts::droplet_body(
                    &job,
                    region,
                    &hosts::cloud_init(&job, &source_keys, &dest_keys),
                );
                rec.vm = Some(net::do_create(token, &body).await?);
            }
            Host::Scaleway => {
                let (origin, launch) =
                    broker.ok_or_else(|| bad("missing Scaleway broker launch"))?;
                let mut lifecycle = scaleway::Lifecycle::new(
                    linked.id.clone(),
                    launch.pin.project.clone(),
                    String::new(),
                    scaleway_audit_bucket
                        .ok_or_else(|| bad("the approved audit bucket is missing"))?,
                    rec.started_ms,
                );
                lifecycle.use_broker(origin.clone(), launch.deadline_ms);
                rec.scaleway = Some(lifecycle);
                save_run(env, &rec).await?;
                let command = pigeon_ingest::scaleway::ComputeCommand::Launch {
                    launch: Box::new(launch),
                };
                let result = scaleway_compute::command(env, &record, Some(&origin), &command)
                    .await
                    .and_then(|(_, reply)| {
                        serde_json::from_value::<pigeon_ingest::scaleway::BrokerStatus>(reply)
                            .map_err(Into::into)
                    });
                match result {
                    Ok(status) if status.run == rec.run && status.server.is_some() => {
                        rec.vm = status.server;
                        rec.scaleway_volumes = status.volumes;
                        scaleway::mark_started(&mut rec);
                    }
                    _ => {
                        scaleway::mark_start_failed(&mut rec);
                        save_run(env, &rec).await?;
                        return Err(bad(format!(
                            "Scaleway launch could not be confirmed. Follow data_ingest_status for cleanup of {}.",
                            rec.run
                        )));
                    }
                }
            }
            Host::Cloudflare => {
                let (url, key) = data.unwrap_or_default();
                let body = serde_json::to_value(StartRequest {
                    job: job.clone(),
                    source_keys: source_keys.clone(),
                    dest_keys: dest_keys.clone(),
                })?;
                net::data_post(&url, protocol::START_PATH, &key, now_ms(), &body).await?;
            }
            Host::Local => return Err(bad("a local run is not started from here")),
        }
        save_run(env, &rec).await?;
        Ok(json!({
            "run": job.run,
            "host": rec.host,
            "vm": rec.vm,
            "bucket_created": bucket_created,
            "job": manifest::job_key(&job.run),
            "note": "Started. Follow it with data_ingest_status; read data_ingest_report when it is done.",
        }))
    }

    /// The host's key for a run's source account, when the host is a VM.
    async fn host_key(&self, env: &Env, rec: &RunRecord) -> Result<Option<String>> {
        let account = data_read::find(&self.accounts, &rec.source)?;
        match hosts::host_slot(account) {
            None => Ok(None),
            Some(slot) => {
                Ok(Some(secret_value(env, &slot).await.ok_or_else(|| {
                    bad(format!("secret `{slot}` is not bound"))
                })?))
            }
        }
    }

    /// The VM's state, or none when it is gone or there is none.
    async fn vm_state(&self, env: &Env, rec: &RunRecord) -> Result<Option<String>> {
        let (Some(id), false) = (rec.vm.as_deref(), rec.vm_deleted) else {
            return Ok(None);
        };
        if let Some(state) = rec.scaleway.as_ref() {
            if state.uses_broker() {
                return Ok(scaleway::broker_status(env, rec, state, false).await?.state);
            }
            let secret = scaleway::provider_credentials(env, state).await?;
            return net::scw_state(&secret, &hosts::zone(&rec.region), id).await;
        }
        let key = self.host_key(env, rec).await?.unwrap_or_default();
        match rec.host.as_str() {
            "digitalocean" => net::do_state(&key, id).await,
            "scaleway" => {
                let (_, secret) = hosts::scaleway_parts(&key).map_err(bad)?;
                net::scw_state(&secret, &hosts::zone(&rec.region), id).await
            }
            _ => Ok(None),
        }
    }

    /// Deletes the run's VM, or stops its Container. Marks the record.
    async fn remove_vm(&self, env: &Env, rec: &mut RunRecord) -> Result<()> {
        match rec.host.as_str() {
            "cloudflare" => {
                let key = setting(env, &format!("data_key:{}", rec.instance))
                    .await?
                    .ok_or_else(|| bad("the data Worker has no key"))?;
                let url = setting(env, &format!("data_url:{}", rec.instance))
                    .await?
                    .ok_or_else(|| bad("the data Worker's URL is not in the Directory"))?;
                if rec
                    .data_worker
                    .as_ref()
                    .is_some_and(|original| original != &url)
                {
                    return Err(bad(
                        "the data Worker changed; stop this run on its original Worker",
                    ));
                }
                let body = serde_json::to_value(StopRequest {
                    run: rec.run.clone(),
                })?;
                net::data_post(&url, protocol::STOP_PATH, &key, now_ms(), &body).await?;
            }
            host => {
                if let Some(id) = rec.vm.as_deref() {
                    let key = self.host_key(env, rec).await?.unwrap_or_default();
                    if host == "digitalocean" {
                        net::do_delete(&key, id).await?;
                    } else {
                        let (_, secret) = hosts::scaleway_parts(&key).map_err(bad)?;
                        let zone = hosts::zone(&rec.region);
                        match net::scw_state(&secret, &zone, id).await?.as_deref() {
                            None => {}
                            Some("stopped") => {
                                net::scw_delete_server(&secret, &zone, id).await?;
                                return Ok(());
                            }
                            Some("running") => {
                                net::scw_action(&secret, &zone, id, &hosts::scaleway_terminate())
                                    .await?;
                                return Ok(());
                            }
                            Some(_) => return Ok(()),
                        }
                        let mut remaining = Vec::new();
                        for volume in &rec.scaleway_volumes {
                            if !net::scw_remove_volume(&secret, &zone, volume).await? {
                                remaining.push(volume.clone());
                            }
                        }
                        if !remaining.is_empty() {
                            rec.scaleway_volumes = remaining;
                            return save_run(env, rec).await;
                        }
                        rec.scaleway_volumes.clear();
                    }
                }
            }
        }
        rec.vm_deleted = true;
        save_run(env, rec).await
    }

    async fn linked_for(
        &self,
        cx: &Context<'_>,
        run: &RunRecord,
    ) -> Result<(pigeon_core::Account, Bucket)> {
        let record = instances::one(cx.env, &run.instance)
            .await?
            .ok_or_else(|| bad("unknown ingest instance"))?;
        let account = self.checked_link(cx.env, &record).await?;
        let bucket = match (&run.destination, data_enabled(&record)) {
            (Some(bound), true)
                if bucket_ok(&bound.bucket)
                    && bound.endpoint == hosts::r2_endpoint(&account.id) =>
            {
                bound.bucket.clone()
            }
            _ => {
                let bucket = self.destination(&record)?.to_string();
                if run.destination.as_ref().is_some_and(|bound| {
                    bound.bucket != bucket || bound.endpoint != hosts::r2_endpoint(&account.id)
                }) {
                    return Err(bad("the run belongs to a different ingest destination"));
                }
                bucket
            }
        };
        Ok((
            account,
            Bucket {
                name: bucket,
                jurisdiction: None,
            },
        ))
    }

    /// `data_ingest_status`: the instance's runs, or one run's status and host state.
    async fn status(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let env = cx.env;
        let instance = arg(args, "instance")?;
        let Some(run) = args["run"].as_str() else {
            return Ok(json!({ "instance": instance, "runs": runs(env, instance).await? }));
        };
        if let Some(result) =
            digitalocean::read(env, self.host_name(), instance, run, false).await?
        {
            return Ok(result);
        }
        let mut rec = run_of(env, instance, run).await?;
        let (account, bucket) = self.linked_for(cx, &rec).await?;
        let mut status: Value = match r2::get(&account, &bucket, &manifest::status_key(run)).await?
        {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => Value::Null,
        };
        // A sharded run: its shards' statuses, and their sum (docs/plans/ingest-bytes.md).
        let mut shard_statuses = Vec::new();
        if rec.host == "cloudflare"
            && let Some(parts) = backblaze::shard_runs(env, instance, run).await?
        {
            let mut read = Vec::new();
            for part in &parts {
                let one: Option<manifest::Status> =
                    r2::get(&account, &bucket, &manifest::status_key(part))
                        .await?
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                shard_statuses.push(json!({ "run": part, "status": one }));
                read.extend(one);
            }
            if !read.is_empty() {
                let mut all = pigeon_ingest::shard::combined_status(run, &read);
                if read.len() < parts.len() {
                    all.state = manifest::RunState::Running;
                }
                status = serde_json::to_value(all)?;
            }
        }
        let state = status["state"].as_str().unwrap_or("").to_string();
        let vm = if rec.host == "cloudflare" {
            net::cloudflare_note()
        } else {
            json!(self.vm_state(env, &rec).await?)
        };
        let ended = matches!(state.as_str(), "done" | "failed" | "stopped");
        let mut note = String::new();
        if ended
            && !rec.vm_deleted
            && rec.host != "cloudflare"
            && rec.vm.is_some()
            && rec.scaleway.is_none()
        {
            self.remove_vm(env, &mut rec).await?;
            note = if rec.vm_deleted {
                "The run ended, so its VM was deleted."
            } else {
                "The run ended. Scaleway resource cleanup is in progress."
            }
            .into();
        }
        let mut output = json!({ "run": rec, "status": status, "vm_state": vm, "note": note });
        if !shard_statuses.is_empty() {
            output["shards"] = json!(shard_statuses);
        }
        if rec.host == "cloudflare"
            && let Some(managed) = backblaze::status(env, instance, run).await?
        {
            output["jobs"] = managed;
        }
        Ok(output)
    }

    /// `data_ingest_report`: the run's report with long lists capped.
    async fn report(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let instance = arg(args, "instance")?;
        let run = arg(args, "run")?;
        if let Some(result) =
            digitalocean::read(cx.env, self.host_name(), instance, run, true).await?
        {
            return Ok(result);
        }
        let rec = run_of(cx.env, instance, run).await?;
        let (account, bucket) = self.linked_for(cx, &rec).await?;
        // A sharded run's report is its shards' reports combined, once every shard has one.
        if rec.host == "cloudflare"
            && let Some(parts) = backblaze::shard_runs(cx.env, instance, run).await?
        {
            let mut reports = Vec::new();
            let mut missing = Vec::new();
            for part in &parts {
                match r2::get(&account, &bucket, &manifest::report_key(part)).await? {
                    Some(bytes) => {
                        reports.push(serde_json::from_slice::<manifest::Report>(&bytes)?)
                    }
                    None => missing.push(part.clone()),
                }
            }
            if !missing.is_empty() {
                return Ok(json!({
                    "run": run, "report": null, "shards": parts,
                    "note": format!("There is no combined report yet: shards {} have not finished.", missing.join(", ")),
                }));
            }
            let mut report =
                serde_json::to_value(pigeon_ingest::shard::combined_report(run, &reports))?;
            cap_report(&mut report);
            report["shards"] = json!(parts);
            return Ok(report);
        }
        match r2::get(&account, &bucket, &manifest::report_key(run)).await? {
            Some(bytes) => {
                let mut report: Value = serde_json::from_slice(&bytes)?;
                cap_report(&mut report);
                Ok(report)
            }
            None => Ok(
                json!({ "run": run, "report": null, "note": "There is no report yet: the run has not finished." }),
            ),
        }
    }

    /// `data_ingest_stop`: deletes the VM, or asks the data Worker to stop the Container.
    async fn stop(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let instance = arg(args, "instance")?;
        let run = arg(args, "run")?;
        if let Some(result) = digitalocean::cancel(cx.env, instance, run).await? {
            return Ok(result);
        }
        let mut rec = run_of(cx.env, instance, run).await?;
        if rec.host == "cloudflare"
            && let Some(result) = backblaze::stop(cx.env, instance, run).await?
        {
            return Ok(result);
        }
        if rec.vm_deleted {
            return Ok(json!({ "run": run, "note": "Already stopped." }));
        }
        if rec.scaleway.is_some() {
            if !scaleway::request_stop(cx.env, run).await? {
                return Err(bad("Scaleway cleanup state changed while stopping; retry"));
            }
            return Ok(
                json!({"run":run, "note":"Scaleway cleanup requested. Follow data_ingest_status for confirmation."}),
            );
        }
        self.remove_vm(cx.env, &mut rec).await?;
        if !rec.vm_deleted {
            return Ok(json!({ "run": run, "note": "Scaleway resource cleanup is in progress." }));
        }
        Ok(
            json!({ "run": run, "note": "Stopped: the VM is deleted (or the Container told to stop). The files already copied stay in R2." }),
        )
    }

    /// Checks an R2 key with a signed HEAD on the instance's bucket.
    async fn check_r2(&self, cx: &Context<'_>, t: &Target, value: &str) -> Result<Value> {
        let keys = s3::parse_keys(value)?;
        let rec = instances::one(cx.env, &t.instance).await?.ok_or_else(|| {
            bad(format!(
                "instance `{}` has no record yet; propose it on the Instances tab first",
                t.instance
            ))
        })?;
        let bucket = self.destination(&rec)?;
        let id = linked_id(&rec).ok_or_else(|| {
            bad(format!("instance `{}` is not linked yet: press Link Cloudflare account on the Instances tab first", t.instance))
        })?;
        let endpoint = Endpoint {
            base: hosts::r2_endpoint(&id),
            region: "auto".into(),
        };
        s3::send(Method::Head, &endpoint, &s3::object_path(bucket, ""), &[], &[], &keys)
            .await
            .map_err(|e| bad(format!("R2 refused this key on bucket `{}`: {e}. Make sure the bucket exists and the key covers it.", bucket)))?;
        Ok(Value::String(format!(
            "the key reaches bucket `{}`",
            bucket
        )))
    }
}

/// The answer when a plan cannot apply: what is missing, and the estimate if there is one.
fn unready(missing: Vec<String>, estimate: Value, sample: Value) -> Value {
    json!({
        "applies": false,
        "missing": missing,
        "estimate": estimate,
        "sample": sample,
        "note": "Nothing was created. Do what `missing` says, then plan again.",
    })
}

fn host_label(h: Host) -> &'static str {
    match h {
        Host::DigitalOcean => "digitalocean",
        Host::Scaleway => "scaleway",
        Host::Cloudflare => "cloudflare",
        Host::Local => "local",
    }
}

impl Capability for Ingest {
    fn name(&self) -> &'static str {
        NAME
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        use pigeon_core::versions::{Component, Kind};
        vec![Component::began(NAME, Kind::Capability).breaking(
            "2.0.0",
            "Its tools are renamed data_ingest_*, like data_email_* and data_read_*, for the \
             capability at the record's data.ingest.",
            "Call data_ingest_plan, data_ingest_start, data_ingest_status, data_ingest_report \
             and data_ingest_stop instead of ingest_*; runs, their records and their approvals \
             are unchanged.",
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        let instance =
            json!({ "type": "string", "description": "The instance, such as `willowgraysen`." });
        let run = json!({ "type": "string", "description": "A run id from data_ingest_plan or data_ingest_status." });
        vec![
            tool(
                connections::LIST,
                "List this instance's approved bucket connections and credential state.",
                json!({"instance": instance, "compute_preview": {"type": "string", "description": "Check the newly entered Scaleway compute slot for this connection."}}),
                &["instance"],
            ),
            tool(
                connections::VERIFY,
                "Verify read access to a named bucket connection without writing to it.",
                json!({"instance": instance, "connection": {"type": "string"}}),
                &["instance", "connection"],
            ),
            tool(
                "data_ingest_plan",
                "Plan a run that copies a source bucket into an instance's R2 bucket, leaving behind \
                 only files R2 already holds with certainty. Reads only: checks what is set up, \
                 samples the source, estimates time and cost, and asks the person to approve. \
                 Anything missing comes back as `missing`.",
                json!({
                    "instance": instance,
                    "source": { "type": "string", "description": "Provider adapter for Data ingest, or a registered account for a legacy run." },
                    "connection": { "type": "string", "description": "Name of an approved bucket connection." },
                    "bucket": { "type": "string", "description": "The source bucket (a declared legacy target only)." },
                    "prefix": { "type": "string" },
                    "dry_run": { "type": "boolean", "description": "Default true: decide and record, copy nothing." },
                    "keep_archives": { "type": "boolean", "description": "Default true." },
                    "fingerprints": { "type": "boolean", "description": "Default false." },
                    "size_bytes": { "type": "integer", "description": "The source's whole size, for the estimate." },
                    "objects": { "type": "integer", "description": "The source's whole object count (from the provider's console), for the run time: small files make a run file-count-bound." },
                    "max_live_bytes": { "type": "integer", "description": "A live run's cap on declared source bytes (default 10 GB, at most 100 TB). Raising it is the person's call: it is shown in the approval." },
                    "shards": { "type": "integer", "description": "Backblaze on a named connection only: split the run into 1 to 8 containers on disjoint keys (by the hash of each key), under one approval; each shard gets an equal share of the write grant. Continue a stopped shard by its own run id ({run}-s{i})." },
                    "continue": { "type": "string", "description": "The id of a stopped run (by its deadline, a stop or a spent grant) to continue from its checkpoint, with the same source, prefix and dry-run setting. Needs a new approval; the continuation gets a new run id." },
                    "only": { "type": "string", "enum": ["too_large"], "description": "With continue naming an ended live run: a new run that takes only the objects that run skipped as too large (over 16 MiB), once large-file uploads pass data_ingest_multipart_check. Backblaze connections only." },
                    "commit": { "type": "string", "description": "A full commit id to build the runner from; default this Worker's own." },
                    "scratch_gib": { "type": "integer", "description": "Size of the dedicated DigitalOcean scratch volume (default 32 GiB)." },
                    "max_minutes": { "type": "integer", "description": "DigitalOcean only: override the computed deadline (1.5 times the slow estimate plus 10 minutes), 10 to 1440 minutes." },
                    "archive_inflated_gib": { "type": "integer", "description": "Maximum inflated GiB per ZIP tree, bounded by scratch space." },
                    "archive_member_gib": { "type": "integer", "description": "Maximum inflated GiB per ZIP member." },
                }),
                &["instance"],
            ),
            tool(
                multipart::CHECK,
                "Check an instance's large-file uploads (objects over 16 MiB): the Data Worker \
                 writes a generated 40 MiB object through its real multipart path, checks its \
                 SHA-256, that a second create-only completion is refused and that it reads \
                 back, then deletes only that object. Ingest writes large files only after \
                 this passes for the bound credential.",
                json!({ "instance": instance }),
                &["instance"],
            ),
            tool(
                "data_ingest_start",
                "Start an approved ingest plan, once.",
                json!({ "plan": { "type": "string", "description": "The approval id from data_ingest_plan." } }),
                &["plan"],
            ),
            tool(
                "data_ingest_status",
                "An instance's ingest runs, or one run's progress and host state. Deletes the VM of a run that ended.",
                json!({ "instance": instance, "run": run }),
                &["instance"],
            ),
            tool(
                "data_ingest_report",
                "A finished run's report, long lists capped at 200 rows.",
                json!({ "instance": instance, "run": run }),
                &["instance", "run"],
            ),
            tool(
                "data_ingest_stop",
                "Stop a run pigeon started: delete its VM or stop its Container.",
                json!({ "instance": instance, "run": run }),
                &["instance", "run"],
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                connections::LIST | connections::VERIFY => {
                    let instance = arg(args, "instance")?;
                    let record = instances::one(cx.env, instance)
                        .await?
                        .ok_or_else(|| bad("There is no approved instance."))?;
                    if tool == connections::LIST {
                        let mut listed = connections::listings(cx.env, &record).await?;
                        if let Some(rows) = listed["connections"].as_array_mut() {
                            for row in rows {
                                let Some(name) = row["name"].as_str() else {
                                    continue;
                                };
                                let verify = args["compute_preview"].as_str() == Some(name);
                                if scaleway_compute::project_scope(&record, name).is_ok() {
                                    row["compute"] =
                                        scaleway_compute::preview(cx, &record, name, verify)
                                            .await?;
                                } else if digitalocean_compute::team_scope(&record, name).is_ok() {
                                    row["compute"] =
                                        digitalocean_compute::preview(cx, &record, name, verify)
                                            .await?;
                                }
                            }
                        }
                        Ok(listed)
                    } else {
                        connections::verify(cx.env, &record, arg(args, "connection")?).await
                    }
                }
                multipart::CHECK => multipart::check(cx.env, arg(args, "instance")?).await,
                "data_ingest_plan" => self.plan(cx, args).await,
                "data_ingest_start" => self.start(cx, args).await,
                "data_ingest_status" => self.status(cx, args).await,
                "data_ingest_report" => self.report(cx, args).await,
                "data_ingest_stop" => self.stop(cx, args).await,
                other => Err(unknown(other)),
            }
        })
    }

    fn write_check<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            if w.action.id == scaleway_compute::SET {
                return scaleway_compute::check_write(cx, w).await;
            }
            if w.action.id == digitalocean_compute::SET {
                return digitalocean_compute::check_write(cx, w).await;
            }
            if w.action.id == multipart::SET_KEY {
                return multipart::check_write(cx, w).await;
            }
            connections::check_write(cx, w).await
        })
    }

    fn write_make<'a>(
        &'a self,
        cx: &'a Context<'a>,
        w: &'a crate::console::write::Write<'a>,
    ) -> Call<'a> {
        Box::pin(async move {
            if w.action.id == scaleway_compute::SET {
                return scaleway_compute::save(cx, w).await;
            }
            if w.action.id == digitalocean_compute::SET {
                return digitalocean_compute::save(cx, w).await;
            }
            if w.action.id == multipart::SET_KEY {
                return multipart::save(cx, self.host_name(), w).await;
            }
            connections::save(cx, w).await
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        let mut slots: Vec<Slot> = self
            .targets
            .iter()
            .map(|t| Slot {
                name: hosts::r2_slot(&t.instance),
                purpose: format!(
                    "The R2 API token ingest writes with, as `ACCESS_KEY_ID:SECRET`. Make it in {}'s \
                     Cloudflare dashboard (R2, Manage API tokens): Object Read & Write, on bucket `{}` only.",
                    t.instance, t.bucket
                ),
                scopes: &[secrets_store::WORKERS],
            })
            .collect();
        for target in &self.targets {
            slots.extend([
                Slot {
                    name: scaleway_source::source_slot(&target.instance),
                    purpose: format!("Read-only Object Storage key for the approved Scaleway source on {}, as ACCESS_KEY_ID:SECRET.", target.instance),
                    scopes: &[secrets_store::WORKERS],
                },
                Slot {
                    name: scaleway_source::compute_slot(&target.instance),
                    purpose: format!("Scaleway compute credentials on {}, as PROJECT_ID:SECRET, scoped to Instances and Block Storage in the approved project.", target.instance),
                    scopes: &[secrets_store::WORKERS],
                },
            ]);
        }
        slots.extend(self.targets.iter().map(|target| Slot {
            name: backblaze::source_slot(&target.instance),
            purpose: format!(
                "The bucket- and prefix-scoped read-only Backblaze B2 key for {}'s approved ingest source, as `KEY_ID:APPLICATION_KEY`.",
                target.instance
            ),
            scopes: &[secrets_store::WORKERS],
        }));
        for a in &self.accounts {
            let Some(name) = hosts::host_slot(a) else {
                continue;
            };
            let purpose = match a.provider {
                Provider::DigitalOcean => format!(
                    "A DigitalOcean API token that starts and deletes ingest Droplets for `{}`. \
                     Custom scopes: droplet:create, droplet:read, droplet:delete (and the read scopes DigitalOcean adds).",
                    a.id()
                ),
                _ => format!(
                    "`PROJECT_ID:SECRET_KEY` of a Scaleway IAM application that starts and deletes \
                     ingest Instances for `{}`: InstancesFullAccess and BlockStorageFullAccess on that one project only.",
                    a.id()
                ),
            };
            slots.push(Slot {
                name,
                purpose,
                scopes: &[secrets_store::WORKERS],
            });
        }
        slots
    }

    /// Checks an ingest secret live before it is written.
    fn check_secret<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            if let Some(t) = self
                .targets
                .iter()
                .find(|t| hosts::r2_slot(&t.instance) == name)
            {
                return self.check_r2(cx, t, value).await;
            }
            for target in &self.targets {
                let source = name == scaleway_source::source_slot(&target.instance);
                if !source && name != scaleway_source::compute_slot(&target.instance) {
                    continue;
                }
                let record = instances::one(cx.env, &target.instance)
                    .await?
                    .ok_or_else(|| {
                        bad("approve the instance before setting its Scaleway credentials")
                    })?;
                let config = scaleway_source::binding(&record)
                    .ok_or_else(|| bad("approve the Scaleway source binding first"))?;
                config.validate().map_err(bad)?;
                if source {
                    let keys = s3::parse_keys(value)?;
                    let account = config.account(&target.instance);
                    let endpoint = Endpoint {
                        base: account.endpoint,
                        region: account.region,
                    };
                    let prefix = config.prefix.as_str();
                    let query = s3::list_objects_query(
                        Some(prefix).filter(|p| !p.is_empty()),
                        None,
                        1,
                        None,
                    );
                    s3::send(
                        Method::Get,
                        &endpoint,
                        &s3::object_path(&config.bucket, ""),
                        &query,
                        &[],
                        &keys,
                    )
                    .await?;
                    return Ok(json!("The key can list the approved Scaleway prefix."));
                }
                let (project, secret) = hosts::scaleway_parts(value).map_err(bad)?;
                if project != config.project {
                    return Err(bad(
                        "the credential's project differs from the approved project",
                    ));
                }
                net::scw_check(&secret, &hosts::zone(&config.region)).await?;
                return Ok(json!(
                    "The key can list Scaleway Instances in the approved region."
                ));
            }
            if let Some(target) = self
                .targets
                .iter()
                .find(|target| backblaze::source_slot(&target.instance) == name)
            {
                let record = instances::one(cx.env, &target.instance)
                    .await?
                    .ok_or_else(|| {
                        bad("the approved Backblaze ingest instance no longer exists")
                    })?;
                return backblaze::check_key(&record, value).await;
            }
            let account = self
                .accounts
                .iter()
                .find(|a| hosts::host_slot(a).as_deref() == Some(name))
                .ok_or_else(|| bad(format!("ingest has no secret `{name}`")))?;
            match account.provider {
                Provider::DigitalOcean => {
                    let status = net::do_account(value.trim()).await?;
                    Ok(Value::String(format!(
                        "the token works; the account is {status}"
                    )))
                }
                _ => {
                    let (_, secret) = hosts::scaleway_parts(value).map_err(bad)?;
                    net::scw_check(&secret, &hosts::zone(&account.region)).await?;
                    Ok(Value::String(format!(
                        "the key can list servers in {}",
                        hosts::zone(&account.region)
                    )))
                }
            }
        })
    }
}

//! The `ingest` capability (decision 13 of docs/plans/ingest.md): plan, start, watch, report on
//! and stop a run that copies only what R2 does not already hold.
//!
//! It is latent. Nothing starts without a person's passkey approval of an exact plan, and no
//! plan applies until the keys it needs are bound as secrets. Keys are read only inside
//! `ingest_start`, passed to the host and never logged or answered.
//!
//! The commit a run is built from is the one this Worker was built from (`PIGEON_COMMIT` at
//! build time). A local build has none, so `ingest_plan` takes an optional full `commit`, which
//! also overrides the built-in one.

pub mod hosts;
pub mod net;
mod profile;
const SCOPED_TOOLS: &[crate::instances::tools::Registration] = &[
    crate::instances::tools::Registration {
        name: "ingest_plan",
        description: Some("Plan ingestion using this instance's approved source and destination."),
        arguments: &[
            "dry_run",
            "keep_archives",
            "fingerprints",
            "size_bytes",
            "commit",
        ],
    },
    crate::instances::tools::Registration {
        name: "ingest_start",
        description: Some("Start an approved ingestion plan for this instance."),
        arguments: &["plan"],
    },
    crate::instances::tools::Registration {
        name: "ingest_report",
        description: Some("Read an ingestion report owned by this instance."),
        arguments: &["run"],
    },
];

pub fn instance_tools(
    instance: &crate::instances::record::Instance,
) -> serde_json::Result<&'static [crate::instances::tools::Registration]> {
    Ok(if instance.ingest.as_ref().is_some_and(Profile::enabled) {
        SCOPED_TOOLS
    } else {
        &[]
    })
}

pub use profile::{Bindings, Profile};

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

use crate::instances::{self, record::AccountRef};
use crate::passkeys::Passkeys;
use crate::storage::{self, Account, Provider};
use crate::users::directory;
use crate::{Call, Capability, Context, Slot, arg, to_value, unknown};

/// The approval kind of a run.
pub const KIND: &str = "ingest_run";
/// How long a plan waits for approval: 24 hours.
pub const PENDING_MS: i64 = 24 * 3_600_000;
/// How long a run's record is kept: a century.
const KEPT_MS: i64 = 100 * 365 * 24 * 3_600_000;
/// The most rows of a list `ingest_report` answers.
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

fn run_key(run: &str) -> String {
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
            && bucket_ok(bucket) && storage::bucket_allowed(source, bucket))
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
        match &record.ingest {
            Some(profile) => profile.bind(args),
            None => {
                let target = self.target(&record.id).map_err(|error| error.to_string())?;
                Ok(Bindings {
                    source: arg(args, "source").map_err(|error| error.to_string())?,
                    bucket: arg(args, "bucket").map_err(|error| error.to_string())?,
                    prefix: args["prefix"].as_str().unwrap_or(""),
                    destination_bucket: &target.bucket,
                })
            }
        }
    }

    fn destination<'a>(&'a self, record: &'a instances::record::Instance) -> Result<&'a str> {
        match &record.ingest {
            Some(profile) => {
                profile.validate().map_err(bad)?;
                match profile {
                    Profile::Enabled {
                        destination_bucket, ..
                    } => Ok(destination_bucket),
                    Profile::Disabled => Err(bad("ingest is disabled for this instance")),
                }
            }
            None => Ok(&self.target(&record.id)?.bucket),
        }
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
            None => Err("this Worker has no build commit: pass `commit` (a full 40-character commit id on GitHub) to ingest_plan".into()),
        }
    }

    /// `ingest_plan`: checks, a sample, an estimate and, when nothing is missing, an approval.
    async fn plan(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
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
        let bindings = match self.bindings(&record, args) {
            Ok(bindings) => bindings,
            Err(error) => return Ok(unready(vec![error], Value::Null, Value::Null)),
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
        let flag = |name: &str, default: bool| args[name].as_bool().unwrap_or(default);
        let (dry_run, keep_archives, fingerprints) = (
            flag("dry_run", true),
            flag("keep_archives", true),
            flag("fingerprints", false),
        );
        let mut missing = Vec::new();

        // The source account and its keys.
        let account = match storage::find(&self.accounts, source) {
            Ok(a) if bucket_ok(bucket) && storage::bucket_allowed(a, bucket) => Some(a),
            Ok(a) => {
                missing.push(format!(
                    "Bucket `{bucket}` is not allowed for `{}`. Ask an agent to add it to the account's buckets in `storage_accounts()`.",
                    a.id()
                ));
                None
            }
            Err(e) => {
                missing.push(format!(
                    "{e}. Ask an agent to declare the account in `storage_accounts()` in src/platform/src/settings.rs."
                ));
                None
            }
        };
        let mut source_keys = None;
        if let Some(a) = account {
            match secret_value(env, &a.binding()).await {
                Some(v) => source_keys = s3::parse_keys(&v).ok(),
                None => missing.push(format!(
                    "Secret `{}` is not bound. Create a read-only key on {}'s console, set it on the console's Access tab, then ask an agent to bind it in src/platform/wrangler.toml.",
                    a.binding(),
                    a.provider.slug()
                )),
            }
            if let Some(slot) = hosts::host_slot(a)
                && secret_value(env, &slot).await.is_none()
            {
                missing.push(format!(
                    "Secret `{slot}` is not bound. Create the API key for starting a VM on {} (decision 13.9 step 3), set it on the Access tab, then ask an agent to bind it.",
                    a.provider.slug()
                ));
            }
            if a.provider == Provider::Backblaze
                && setting(env, &format!("data_key:{instance}"))
                    .await?
                    .is_none()
            {
                missing.push(format!(
                    "The data Worker for `{instance}` has no key in the Directory (`data_key:{instance}`). Deploy the data Worker first (decision 13.9 step 6)."
                ));
            }
        }
        if secret_value(env, &hosts::r2_slot(instance)).await.is_none() {
            missing.push(format!(
                "Secret `{}` is not bound. In {}'s Cloudflare dashboard make an R2 API token for bucket `{}` only, Object Read & Write, set it on the Access tab, then ask an agent to bind it.",
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
        if let (Some(a), Some(keys)) = (account, source_keys.as_ref()) {
            let query =
                s3::list_objects_query(Some(prefix).filter(|p| !p.is_empty()), None, 1000, None);
            let endpoint = Endpoint {
                base: a.endpoint.clone(),
                region: a.region.clone(),
            };
            match s3::send(
                Method::Get,
                &endpoint,
                &s3::object_path(bucket, ""),
                &query,
                &[],
                keys,
            )
            .await
            {
                Ok(mut res) => {
                    let listing = s3::parse_list_objects(&res.text().await?)?;
                    let count = listing.objects.len() as u64;
                    let bytes: u64 = listing.objects.iter().map(|o| o.size).sum();
                    let more = listing.next.is_some();
                    let (total_bytes, total_objects) = match args["size_bytes"].as_u64() {
                        Some(t) if count > 0 => (t, t * count / bytes.max(1)),
                        Some(t) => (t, 0),
                        None => (bytes, count),
                    };
                    sample = json!({
                        "objects": count, "bytes": bytes, "first_page_only": more,
                        "note": if more && args["size_bytes"].is_null() {
                            "Only the first page was listed, so the estimate covers that page. Pass `size_bytes` (the whole size) for a whole-run estimate."
                        } else { "" },
                    });
                    est = to_value(estimate::estimate(
                        hosts::host_for(a.provider),
                        total_bytes,
                        total_objects,
                        UNIQUE_SHARE,
                    ));
                }
                Err(e) => missing.push(format!(
                    "The source could not be listed with the bound key: {e}"
                )),
            }
        }

        let (Some(a), Some(commit)) = (account, commit.as_ref()) else {
            return Ok(unready(missing, est, sample));
        };
        if !missing.is_empty() {
            return Ok(unready(missing, est, sample));
        }
        let job = hosts::job_spec(&hosts::JobInputs {
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
        let data_worker = if host == Host::Cloudflare {
            setting(env, &format!("data_url:{instance}")).await?
        } else {
            None
        };
        let host_key = if host == Host::Scaleway {
            if let Some(slot) = hosts::host_slot(a) {
                secret_value(env, &slot).await
            } else {
                None
            }
        } else {
            None
        };
        let execution = match execution_binding(host, data_worker.as_deref(), host_key.as_deref()) {
            Ok(execution) => execution,
            Err(error) => return Ok(unready(vec![error.to_string()], est, sample)),
        };
        let request = hosts::host_request(host, &job, &a.region);
        let bucket_here = r2::bucket_exists(
            &linked,
            &Bucket {
                name: destination_bucket.to_string(),
                jurisdiction: None,
            },
        )
        .await?;
        let mut lines = Vec::new();
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
                "Create a {} Instance in {} that builds the runner at commit {commit}, runs it once and powers off.",
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
            "note": "Nothing starts until the person approves this plan on the console's Approvals tab; then ingest_start {plan: <this approval>} starts it once.",
        }))
    }

    /// `ingest_start`: takes the approval, writes the job, starts the host, records the run.
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
        let bindings = self.bindings(&record, &source_args).map_err(bad)?;
        let account = storage::find(&self.accounts, source_id)?;
        let account_id =
            linked_id(&record).ok_or_else(|| bad("the approved instance has no linked account"))?;
        check_job_binding(&job, &account_id, bindings.destination_bucket, account).map_err(bad)?;
        let linked = self.checked_link(env, &record).await?;
        let host = hosts::host_for(account.provider);
        // Read every key before the approval is spent, so a missing one costs nothing.
        let unbound = |name: &str| bad(format!("secret `{name}` is not bound"));
        let source_keys = secret_value(env, &account.binding())
            .await
            .ok_or_else(|| unbound(&account.binding()))?;
        s3::parse_keys(&source_keys)?;
        let r2_slot = hosts::r2_slot(&job.instance);
        let dest_keys = secret_value(env, &r2_slot)
            .await
            .ok_or_else(|| unbound(&r2_slot))?;
        s3::parse_keys(&dest_keys)?;
        let host_key = match hosts::host_slot(account) {
            Some(slot) => Some(
                secret_value(env, &slot)
                    .await
                    .ok_or_else(|| unbound(&slot))?,
            ),
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
        let request = hosts::host_request(host, &job, region);
        let kind = pigeon_core::approval::Kind::Other(KIND.into());
        let execution = execution_binding(
            host,
            data.as_ref().map(|(url, _)| url.as_str()),
            host_key.as_deref(),
        )?;
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
            name: bindings.destination_bucket.to_string(),
            jurisdiction: None,
        };
        let ensured = r2::ensure_bucket(&linked, &dest).await?;
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
            vm_deleted: false,
            destination: Some(job.dest.clone()),
            data_worker: data.as_ref().map(|(url, _)| url.clone()),
            approval: approval_id.to_string(),
            commit: job.commit.clone(),
            dry_run: job.dry_run,
            started_ms: now_ms(),
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
                let (project, secret) =
                    hosts::scaleway_parts(host_key.as_deref().unwrap_or_default()).map_err(bad)?;
                let zone = hosts::zone(region);
                let id =
                    net::scw_create(&secret, &zone, &hosts::scaleway_server_body(&job, &project))
                        .await?;
                rec.vm = Some(id.clone());
                save_run(env, &rec).await?;
                net::scw_user_data(
                    &secret,
                    &zone,
                    &id,
                    &hosts::cloud_init(&job, &source_keys, &dest_keys),
                )
                .await?;
                net::scw_action(&secret, &zone, &id, &hosts::scaleway_poweron()).await?;
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
            "bucket_created": ensured.created,
            "job": manifest::job_key(&job.run),
            "note": "Started. Follow it with ingest_status; read ingest_report when it is done.",
        }))
    }

    /// The host's key for a run's source account, when the host is a VM.
    async fn host_key(&self, env: &Env, rec: &RunRecord) -> Result<Option<String>> {
        let account = storage::find(&self.accounts, &rec.source)?;
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
                        net::scw_action(
                            &secret,
                            &hosts::zone(&rec.region),
                            id,
                            &hosts::scaleway_terminate(),
                        )
                        .await?;
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
        let bucket = self.destination(&record)?.to_string();
        let account = self.checked_link(cx.env, &record).await?;
        if run.destination.as_ref().is_some_and(|bound| {
            bound.bucket != bucket || bound.endpoint != hosts::r2_endpoint(&account.id)
        }) {
            return Err(bad("the run belongs to a different ingest destination"));
        }
        Ok((
            account,
            Bucket {
                name: bucket,
                jurisdiction: None,
            },
        ))
    }

    /// `ingest_status`: the instance's runs, or one run's status and host state.
    async fn status(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let env = cx.env;
        let instance = arg(args, "instance")?;
        let Some(run) = args["run"].as_str() else {
            let mut out = Vec::new();
            for id in runs_of(env, instance).await? {
                if let Some(text) = setting(env, &run_key(&id)).await? {
                    let record: RunRecord = serde_json::from_str(&text)?;
                    if record.instance == instance {
                        out.push(record);
                    }
                }
            }
            return Ok(json!({ "instance": instance, "runs": out }));
        };
        let mut rec = run_of(env, instance, run).await?;
        let (account, bucket) = self.linked_for(cx, &rec).await?;
        let status: Value = match r2::get(&account, &bucket, &manifest::status_key(run)).await? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => Value::Null,
        };
        let state = status["state"].as_str().unwrap_or("").to_string();
        let vm = if rec.host == "cloudflare" {
            net::cloudflare_note()
        } else {
            json!(self.vm_state(env, &rec).await?)
        };
        let ended = matches!(state.as_str(), "done" | "failed" | "stopped");
        let mut note = String::new();
        if ended && !rec.vm_deleted && rec.host != "cloudflare" && rec.vm.is_some() {
            self.remove_vm(env, &mut rec).await?;
            note = "The run ended, so its VM was deleted.".into();
        }
        Ok(json!({
            "run": rec, "status": status, "vm_state": vm, "note": note,
        }))
    }

    /// `ingest_report`: the run's report with long lists capped.
    async fn report(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let instance = arg(args, "instance")?;
        let run = arg(args, "run")?;
        let rec = run_of(cx.env, instance, run).await?;
        let (account, bucket) = self.linked_for(cx, &rec).await?;
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

    /// `ingest_stop`: deletes the VM, or asks the data Worker to stop the Container.
    async fn stop(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let instance = arg(args, "instance")?;
        let run = arg(args, "run")?;
        let mut rec = run_of(cx.env, instance, run).await?;
        if rec.vm_deleted {
            return Ok(json!({ "run": run, "note": "Already stopped." }));
        }
        self.remove_vm(cx.env, &mut rec).await?;
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
        "ingest"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "ingest",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        let instance =
            json!({ "type": "string", "description": "The instance, such as `willowgraysen`." });
        let run = json!({ "type": "string", "description": "A run id from ingest_plan or ingest_status." });
        vec![
            tool(
                "ingest_plan",
                "Plan a run that copies a source bucket into an instance's R2 bucket, leaving behind \
                 only files R2 already holds with certainty. Reads only: checks what is set up, \
                 samples the source, estimates time and cost, and asks the person to approve. \
                 Anything missing comes back as `missing`.",
                json!({
                    "instance": instance,
                    "source": { "type": "string", "description": "A storage account id (required for legacy instances; a profile fixes the source)." },
                    "bucket": { "type": "string", "description": "The source bucket (required for legacy instances; a profile fixes the bucket)." },
                    "prefix": { "type": "string" },
                    "dry_run": { "type": "boolean", "description": "Default true: decide and record, copy nothing." },
                    "keep_archives": { "type": "boolean", "description": "Default true." },
                    "fingerprints": { "type": "boolean", "description": "Default false." },
                    "size_bytes": { "type": "integer", "description": "The source's whole size, for the estimate." },
                    "commit": { "type": "string", "description": "A full commit id to build the runner from; default this Worker's own." },
                }),
                &["instance"],
            ),
            tool(
                "ingest_start",
                "Start an approved ingest plan, once.",
                json!({ "plan": { "type": "string", "description": "The approval id from ingest_plan." } }),
                &["plan"],
            ),
            tool(
                "ingest_status",
                "An instance's ingest runs, or one run's progress and host state. Deletes the VM of a run that ended.",
                json!({ "instance": instance, "run": run }),
                &["instance"],
            ),
            tool(
                "ingest_report",
                "A finished run's report, long lists capped at 200 rows.",
                json!({ "instance": instance, "run": run }),
                &["instance", "run"],
            ),
            tool(
                "ingest_stop",
                "Stop a run pigeon started: delete its VM or stop its Container.",
                json!({ "instance": instance, "run": run }),
                &["instance", "run"],
            ),
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "ingest_plan" => self.plan(cx, args).await,
                "ingest_start" => self.start(cx, args).await,
                "ingest_status" => self.status(cx, args).await,
                "ingest_report" => self.report(cx, args).await,
                "ingest_stop" => self.stop(cx, args).await,
                other => Err(unknown(other)),
            }
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
                     ingest Instances for `{}`: InstancesFullAccess on that one project only.",
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

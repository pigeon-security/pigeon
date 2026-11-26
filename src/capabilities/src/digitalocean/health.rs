//! A bounded DigitalOcean Droplet health check for the approved instance.

use pigeon_cloud::digitalocean_droplet::{
    self as provider, Created, DropletStatus, Observation, Removal,
};
use pigeon_cloud::transport::FetchTransport;
use pigeon_core::{crypto, oauth, sign, webauthn};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Request, Response, Result};

use super::{bad, expected, for_write, team};
use crate::Context;
use crate::console::write::Write;
use crate::instances::record::Instance;
use crate::jobs_audit::{self, ExportStatus};
use crate::users::directory;

pub const STATUS: &str = "digitalocean_droplet_status";
pub const JOBS: &str = "digitalocean_droplet_jobs";
pub const JOB: &str = "digitalocean_droplet_job";
pub const SET_TOKEN: &str = "digitalocean-droplet-token-set";
pub const START: &str = "digitalocean-droplet-start";
pub const CANCEL: &str = "digitalocean-droplet-cancel";
pub const CALLBACK_PATH: &str = "/internal/droplet-health";
const CALLBACK: &str = "https://platform.pigeon.dev/internal/droplet-health";
const COMMIT: &str = "088a2dc05c1782fd8ba9a7678a5e4edd67952cb7";
const REPOSITORY: &str = "https://github.com/pigeon-security/pigeon.git";
const EXPECTED_VERSION: &str = "pigeon-ingest-runner 0.1.0";
const MINUTE: u64 = 60_000;
const MAX_MINUTES: u64 = 30;
const ESTIMATED_USD: f64 = 0.02;
const MAX_EVENTS: usize = 32;
const DEADLINE_FAILURE: &str = "No verified health result before the deadline.";
const CLEANUP_RETRY: &str = "Could not safely reconcile or delete the Droplet. Pigeon will retry; verify the provider credential and check the team dashboard.";

#[doc(hidden)]
pub const MAX_JOBS: usize = 30;
pub(crate) const HISTORY_TTL_MS: i64 = 180 * 24 * 60 * 60 * 1000;

pub(crate) fn history_key(instance: &str) -> String {
    format!("digitalocean-droplet-history:{instance}")
}

pub fn enabled(record: &Instance) -> bool {
    team(record).is_some() && record.capabilities[super::NAME].droplet_health
}

pub fn token_key(instance: &str) -> String {
    format!("digitalocean-droplet-token:{instance}")
}

pub fn job_key(instance: &str) -> String {
    format!("digitalocean-droplet-job:{instance}")
}

fn associated_data(instance: &str, team: &str) -> Vec<u8> {
    format!("pigeon:digitalocean-droplet:v1\0{instance}\0{team}").into_bytes()
}

pub fn plan() -> Value {
    let mut body = json!({
        "region": provider::REGION,
        "size": provider::SIZE,
        "image": provider::IMAGE,
        "commit": COMMIT,
        "repository": REPOSITORY,
        "expected_version": EXPECTED_VERSION,
        "callback": CALLBACK,
        "max_minutes": MAX_MINUTES,
        "estimated_usd": ESTIMATED_USD,
        "cleanup": "delete the owned Droplet after its result or deadline and confirm absence"
    });
    body["bootstrap_policy"] = json!(oauth::hash(include_str!("health.rs")));
    body["fingerprint"] = json!(oauth::hash(&body.to_string()));
    body
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Credential {
    team: String,
    sealed: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Creating,
    Waiting,
    Cleanup,
    Gone,
    Rejected,
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
#[doc(hidden)]
pub enum Stage {
    DropletCreate,
    CloudInit,
    PackageInstall,
    ImageBuild,
    VersionCheck,
    Cleanup,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Requested,
    Started,
    Succeeded,
    Failed,
    Uncertain,
    Retrying,
    Cancelled,
    TimedOut,
    Confirmed,
}

#[derive(Clone, Deserialize, Serialize)]
struct Event {
    at_ms: u64,
    stage: Stage,
    outcome: Outcome,
}

#[derive(Clone, Deserialize, Serialize)]
#[doc(hidden)]
pub struct Job {
    pub(crate) id: String,
    pub(crate) instance: String,
    state: State,
    health: String,
    started_ms: u64,
    deadline_ms: u64,
    next_ms: u64,
    lease_until_ms: u64,
    expected_version: String,
    #[serde(default)]
    created_ms: Option<u64>,
    #[serde(default)]
    confirmed_delete_ms: Option<u64>,
    #[serde(default)]
    provider_observation: Option<(String, u64)>,
    #[serde(default)]
    updated_ms: Option<u64>,
    droplet: Option<u64>,
    credential: Option<Credential>,
    callback_hash: Option<String>,
    error: Option<String>,
    #[serde(default)]
    events: Vec<Event>,
}

impl Job {
    pub fn terminal(&self) -> bool {
        matches!(self.state, State::Gone | State::Rejected)
    }

    fn public(&self) -> Value {
        let provider_state = match self.state {
            State::Creating if self.error.is_some() => "uncertain",
            State::Creating => "creating",
            State::Waiting => "waiting",
            State::Cleanup => "cleanup",
            State::Gone => "absent",
            State::Rejected => "rejected",
        };
        let estimate = self.created_ms.and_then(|start| {
            provider::compute_estimate(
                start,
                self.updated_ms.unwrap_or(self.started_ms),
                self.confirmed_delete_ms,
            )
        });
        json!({
            "id": self.id, "state": self.state, "health": self.health,
            "started_ms": self.started_ms, "deadline_ms": self.deadline_ms,
            "error": self.error,
            "estimated_usd": estimate.as_ref().map(|cost| cost.estimated_usd_micros as f64 / 1_000_000.0),
            "compute_estimate": estimate,
            "provider_state": self.provider_observation.as_ref().map_or(provider_state, |(status, _)| status),
            "provider_observed_ms": self.provider_observation.as_ref().map(|(_, at)| at),
            "stage": self.events.iter().map(|event| event.stage).max(),
            "created_ms": self.created_ms,
            "confirmed_delete_ms": self.confirmed_delete_ms,
        })
    }

    #[doc(hidden)]
    pub fn public_with_events(&self) -> Value {
        let mut summary = self.public();
        summary["events"] = json!(self.events);
        summary
    }

    fn record(&mut self, stage: Stage, outcome: Outcome, time: u64) {
        if self.events.len() < MAX_EVENTS
            && !self
                .events
                .iter()
                .any(|event| event.stage == stage && event.outcome == outcome)
        {
            self.events.push(Event {
                at_ms: time,
                stage,
                outcome,
            });
        }
    }

    fn name(&self) -> String {
        format!("{}{id}", provider::NAME_PREFIX, id = self.id)
    }

    fn close(&mut self, state: State) {
        self.state = state;
        self.credential = None;
        self.callback_hash = None;
        self.next_ms = 0;
    }

    #[doc(hidden)]
    pub fn schedule_cancel(&mut self, run: &str, time: u64) -> bool {
        if self.id != run || self.terminal() || self.state == State::Cleanup {
            return false;
        }
        self.health = "cancelled".into();
        self.error = None;
        self.callback_hash = None;
        self.state = State::Cleanup;
        self.next_ms = time;
        self.updated_ms = Some(time);
        self.record(Stage::Cleanup, Outcome::Cancelled, time);
        true
    }

    #[doc(hidden)]
    pub fn callback(
        &mut self,
        token: &str,
        event: &str,
        stage: Option<Stage>,
        version: Option<&str>,
        time: u64,
    ) -> bool {
        let supplied = oauth::hash(token);
        if self.terminal()
            || time >= self.deadline_ms
            || !self
                .callback_hash
                .as_ref()
                .is_some_and(|hash| sign::equal(hash.as_bytes(), supplied.as_bytes()))
        {
            return false;
        }
        let stage = match (stage, event) {
            (
                Some(
                    stage @ (Stage::CloudInit
                    | Stage::PackageInstall
                    | Stage::ImageBuild
                    | Stage::VersionCheck),
                ),
                _,
            ) => stage,
            // Accept legacy reports from an already-created Droplet during deployment.
            (None, "started") => Stage::CloudInit,
            (None, "result") => Stage::VersionCheck,
            _ => return false,
        };
        // Retries and late in-flight messages cannot move a job back to an older stage.
        if self.events.iter().any(|event| event.stage > stage) {
            return true;
        }
        if self
            .events
            .iter()
            .any(|event| event.stage == stage && event.outcome == Outcome::Succeeded)
        {
            return true;
        }
        match event {
            "started" | "succeeded" if stage != Stage::VersionCheck || event == "started" => {
                let outcome = if event == "started" {
                    Outcome::Started
                } else {
                    Outcome::Succeeded
                };
                self.health = "started".into();
                self.record(stage, outcome, time);
            }
            "failed" | "result" if event != "result" || stage == Stage::VersionCheck => {
                let passed = event == "result" && version == Some(self.expected_version.as_str());
                self.health = if passed { "passed" } else { "failed" }.into();
                self.record(
                    stage,
                    if passed {
                        Outcome::Succeeded
                    } else {
                        Outcome::Failed
                    },
                    time,
                );
                self.callback_hash = None;
                self.state = State::Cleanup;
                self.next_ms = time;
            }
            _ => return false,
        }
        self.updated_ms = Some(time);
        true
    }
}

#[doc(hidden)]
pub fn update_history(mut history: Vec<Value>, job: &Job) -> Vec<Value> {
    let mut snapshot = job.public_with_events();
    if job.terminal() {
        let prior = history
            .iter()
            .find(|row| row["id"].as_str() == Some(job.id.as_str()));
        snapshot["audit_status"] = prior
            .and_then(|row| row.get("audit_status"))
            .cloned()
            .unwrap_or_else(|| json!(ExportStatus::Pending));
        if let Some(retry_at) = prior.and_then(|row| row.get("audit_retry_at")) {
            snapshot["audit_retry_at"] = retry_at.clone();
        }
    }
    history.retain(|row| row["id"].as_str() != Some(job.id.as_str()));
    history.insert(0, snapshot);
    history.truncate(MAX_JOBS);
    history
}

fn now() -> u64 {
    worker::Date::now().as_millis()
}

async fn setting<T: serde::de::DeserializeOwned>(
    env: &Env,
    key: &str,
) -> Result<Option<(String, T)>> {
    let value = directory(env, "setting.get", json!({"key":key})).await?;
    value
        .as_str()
        .map(|text| {
            Ok((
                text.to_string(),
                serde_json::from_str(text)
                    .map_err(|_| bad("stored Droplet health state could not be read"))?,
            ))
        })
        .transpose()
}

async fn replace(env: &Env, prior: &str, job: &Job) -> Result<bool> {
    let answer = directory(
        env,
        "digitalocean.health.replace",
        json!({
            "instance": job.instance, "previous": prior, "value": serde_json::to_string(job)?,
        }),
    )
    .await?;
    Ok(answer == true)
}

pub(super) async fn jobs(env: &Env, instance: &str) -> Result<Value> {
    directory(
        env,
        "digitalocean.health.history",
        json!({"instance":instance}),
    )
    .await
}

pub(super) async fn job(env: &Env, instance: &str, id: &str) -> Result<Value> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(bad("invalid Droplet job ID"));
    }
    let answer = directory(
        env,
        "digitalocean.health.history",
        json!({"instance":instance, "id":id}),
    )
    .await?;
    if answer["job"].is_null() {
        return Err(bad("no such Droplet job on this instance"));
    }
    Ok(answer)
}

pub(super) async fn status(env: &Env, record: &Instance) -> Result<Value> {
    if !enabled(record) {
        return Err(bad("the Droplet health check is disabled on this instance"));
    }
    let credential = match setting::<Credential>(env, &token_key(&record.id)).await? {
        Some((_, configured)) if team(record) == Some(configured.team.as_str()) => "configured",
        Some(_) => "unverified",
        None => "missing",
    };
    let job = setting::<Job>(env, &job_key(&record.id))
        .await?
        .map(|(_, job)| job);
    Ok(json!({
        "instance": record.id,
        "credential": credential,
        "can_start": credential == "configured" && job.as_ref().is_none_or(Job::terminal),
        "plan": plan(),
        "run": job.as_ref().map(Job::public),
    }))
}

pub(super) async fn check_write(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    if !enabled(&record) {
        return Err(bad("the Droplet health check is disabled on this instance"));
    }
    if w.action.id == CANCEL {
        let (_, job) = cancel_target(cx.env, &record, w).await?;
        return Ok(json!({"run":job.public()}));
    }
    if w.action.id != START {
        return Ok(Value::Null);
    }
    let plan = plan();
    if w.change["plan"] != plan["fingerprint"] {
        return Err(bad("the Droplet health plan changed; review it again"));
    }
    if status(cx.env, &record).await?["can_start"] != true {
        return Err(bad(
            "set a provisioning credential and wait for any previous Droplet cleanup",
        ));
    }
    Ok(json!({"plan":plan}))
}

pub(super) async fn save_token(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    if !enabled(&record) {
        return Err(bad("the Droplet health check is disabled on this instance"));
    }
    let team = expected(&record)?;
    let token = w.change["value"]
        .as_str()
        .filter(|s| (1..=512).contains(&s.len()))
        .ok_or_else(|| bad("invalid DigitalOcean provisioning token"))?;
    crate::cloud_scope::verify_digitalocean(&record, token).await?;
    let key = crate::linked::sealing_key(cx.env).await?;
    let sealed = crypto::seal(&key, token.as_bytes(), &associated_data(&record.id, team)).await?;
    let value = serde_json::to_string(&Credential {
        team: team.into(),
        sealed,
    })?;
    directory(
        cx.env,
        "digitalocean.health.token",
        json!({"target":w.target,"value":value}),
    )
    .await?;
    Ok(json!({"instance":record.id,"verified":true}))
}

async fn open(env: &Env, instance: &str, credential: &Credential) -> Result<String> {
    let key = crate::linked::sealing_key(env).await?;
    let value = crypto::open(
        &key,
        &credential.sealed,
        &associated_data(instance, &credential.team),
    )
    .await
    .map_err(|_| bad("the saved DigitalOcean provisioning token could not be opened"))?;
    String::from_utf8(value)
        .map_err(|_| bad("the saved DigitalOcean provisioning token is invalid"))
}

fn script(job: &Job, token: &str) -> String {
    let json_event = |event: &str, stage: Stage, version: Option<&str>| {
        json!({
            "instance": job.instance, "id": job.id, "event": event, "stage": stage, "version": version,
        })
        .to_string()
    };
    let started = |stage| json_event("started", stage, None);
    let succeeded = |stage| json_event("succeeded", stage, None);
    let failed = |stage| json_event("failed", stage, None);
    format!(
        r#"#!/bin/bash
set -eu
report() {{
  curl --silent --fail --max-time 4 --retry 2 --retry-delay 1 --retry-max-time 7 --retry-all-errors -X POST '{CALLBACK}' \
    -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' --data "$1" >/dev/null
}}
report '{cloud_init}' || true
report '{package_start}' || true
if ! timeout 420 bash -c 'apt-get update -qq && apt-get install -y -qq docker.io git'; then
  report '{package_failed}'
  exit 1
fi
report '{package_passed}' || true
report '{build_start}' || true
if ! timeout 1080 docker build -f src/ingest-runner/Dockerfile --build-arg PIGEON_COMMIT={COMMIT} -t pigeon-health '{REPOSITORY}#{COMMIT}'; then
  report '{build_failed}'
  exit 1
fi
report '{build_passed}' || true
report '{version_start}' || true
if output=$(timeout 60 docker run --rm --network none --read-only pigeon-health --version) &&
   [ "$output" = '{version}' ]; then
  report '{passed}'
else
  report '{version_failed}'
  exit 1
fi
"#,
        cloud_init = started(Stage::CloudInit),
        package_start = started(Stage::PackageInstall),
        package_passed = succeeded(Stage::PackageInstall),
        package_failed = failed(Stage::PackageInstall),
        build_start = started(Stage::ImageBuild),
        build_passed = succeeded(Stage::ImageBuild),
        build_failed = failed(Stage::ImageBuild),
        version_start = started(Stage::VersionCheck),
        version_failed = failed(Stage::VersionCheck),
        passed = json_event("result", Stage::VersionCheck, Some(&job.expected_version)),
        version = job.expected_version
    )
}

pub(super) async fn start(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    check_write(cx, w).await?;
    let record = for_write(cx.env, &w.target).await?;
    let (secret_row, credential) = setting::<Credential>(cx.env, &token_key(&record.id))
        .await?
        .filter(|(_, c)| team(&record) == Some(c.team.as_str()))
        .ok_or_else(|| bad("set a provisioning credential for the approved team"))?;
    let pat = open(cx.env, &record.id, &credential).await?;
    crate::cloud_scope::verify_digitalocean(&record, &pat).await?;
    let entropy = webauthn::random()?;
    let id = sign::hex(&entropy.as_bytes()[..16]);
    let callback = webauthn::random()?;
    let at = now();
    let mut job = Job {
        id,
        instance: record.id.clone(),
        state: State::Creating,
        health: "pending".into(),
        started_ms: at,
        deadline_ms: at + MAX_MINUTES * MINUTE,
        next_ms: at + 2 * MINUTE,
        lease_until_ms: 0,
        expected_version: format!("{EXPECTED_VERSION} {COMMIT}"),
        created_ms: None,
        confirmed_delete_ms: None,
        provider_observation: None,
        updated_ms: Some(at),
        droplet: None,
        credential: Some(credential),
        callback_hash: Some(oauth::hash(&callback)),
        error: None,
        events: Vec::new(),
    };
    job.record(Stage::DropletCreate, Outcome::Requested, at);
    let prior = serde_json::to_string(&job)?;
    directory(
        cx.env,
        "digitalocean.health.start",
        json!({
            "target":w.target, "value":prior, "credential_before":secret_row
        }),
    )
    .await?;
    let result =
        provider::create(&FetchTransport, &pat, &job.name(), &script(&job, &callback)).await;
    match result {
        Created::Accepted(id) => {
            job.droplet = Some(id);
            job.created_ms = Some(job.started_ms);
            job.state = State::Waiting;
            job.record(Stage::DropletCreate, Outcome::Succeeded, now());
        }
        Created::Rejected(status) => {
            job.error = Some(format!(
                "DigitalOcean refused the Droplet request (HTTP {status})"
            ));
            job.record(Stage::DropletCreate, Outcome::Failed, now());
            job.close(State::Rejected);
        }
        Created::Unknown => {
            job.error = Some("Droplet creation is uncertain. Pigeon will reconcile the original job without issuing a second create.".into());
            job.record(Stage::DropletCreate, Outcome::Uncertain, now());
        }
    }
    job.updated_ms = Some(now());
    if !replace(cx.env, &prior, &job).await? {
        return status(cx.env, &record).await;
    }
    Ok(json!({"instance":record.id,"run":job.public()}))
}

async fn cancel_target(env: &Env, record: &Instance, w: &Write<'_>) -> Result<(String, Job)> {
    if !enabled(record) {
        return Err(bad("the Droplet health check is disabled on this instance"));
    }
    setting::<Job>(env, &job_key(&record.id))
        .await?
        .filter(|(_, job)| {
            w.change["run"].as_str() == Some(job.id.as_str())
                && !job.terminal()
                && job.state != State::Cleanup
        })
        .ok_or_else(|| bad("that Droplet health job is no longer waiting for cancellation"))
}

pub(super) async fn cancel(cx: &Context<'_>, w: &Write<'_>) -> Result<Value> {
    let record = for_write(cx.env, &w.target).await?;
    let (prior, mut job) = cancel_target(cx.env, &record, w).await?;
    if !job.schedule_cancel(w.change["run"].as_str().unwrap_or_default(), now())
        || !replace(cx.env, &prior, &job).await?
    {
        return Err(bad("the Droplet health job changed; refresh its status"));
    }
    Ok(json!({"instance":record.id,"run":job.public()}))
}

/// Receive an authenticated, job-only health signal. No provider credential crosses this route.
pub async fn callback(env: &Env, mut request: Request) -> Result<Response> {
    let Some(token) = request
        .headers()
        .get("Authorization")?
        .and_then(|s| s.strip_prefix("Bearer ").map(str::to_string))
        .filter(|token| token.len() == 43)
    else {
        return Response::error("unauthorized", 401);
    };
    if request
        .headers()
        .get("Content-Length")?
        .and_then(|value| value.parse::<usize>().ok())
        .is_none_or(|len| !(1..=1024).contains(&len))
    {
        return Response::error("invalid health report size", 400);
    }
    let body: Value = match request.json().await {
        Ok(v) => v,
        Err(_) => return Response::error("invalid health report", 400),
    };
    let Some(instance) = body["instance"].as_str().filter(|i| i.len() <= 64) else {
        return Response::error("unauthorized", 401);
    };
    let Some((prior, mut job)) = setting::<Job>(env, &job_key(instance)).await? else {
        return Response::error("unauthorized", 401);
    };
    let stage: Option<Stage> = match serde_json::from_value(body["stage"].clone()) {
        Ok(stage) => stage,
        Err(_) => return Response::error("invalid health stage", 400),
    };
    if body["id"].as_str() != Some(job.id.as_str())
        || !job.callback(
            &token,
            body["event"].as_str().unwrap_or_default(),
            stage,
            body["version"].as_str(),
            now(),
        )
    {
        return Response::error("unauthorized", 401);
    }
    if !replace(env, &prior, &job).await? {
        return Response::error("retry health report", 409);
    }
    Ok(Response::empty()?.with_status(204))
}

async fn reap_one(env: &Env, job: &mut Job, time: u64) -> Result<()> {
    let credential = job
        .credential
        .as_ref()
        .ok_or_else(|| bad("no provisioning credential for outstanding Droplet cleanup"))?;
    let pat = open(env, &job.instance, credential).await?;
    pigeon_cloud::compute::probe_digitalocean(&FetchTransport, &credential.team, &pat).await?;
    if job.droplet.is_none() {
        job.droplet = provider::reconcile(&FetchTransport, &pat, &job.name()).await?;
        if job.droplet.is_none() {
            job.error = Some("The original Droplet request is not yet resolved.".into());
            return Ok(());
        }
        job.error = None;
        job.record(Stage::DropletCreate, Outcome::Succeeded, time);
        if job.state == State::Creating {
            job.state = State::Waiting;
        }
    }
    job.created_ms.get_or_insert(job.started_ms);
    if time < job.deadline_ms && job.state != State::Cleanup {
        if let Some(id) = job.droplet {
            let observation = provider::observe(&FetchTransport, &pat, &job.name(), id).await?;
            let status = match observation {
                Observation::Absent => "not_visible",
                Observation::Present(DropletStatus::New) => "new",
                Observation::Present(DropletStatus::Active) => "active",
                Observation::Present(DropletStatus::Off) => "off",
                Observation::Present(DropletStatus::Archive) => "archive",
                Observation::Present(DropletStatus::Unknown) => "unknown",
            };
            job.provider_observation = Some((status.into(), now()));
        }
        job.next_ms = (time + MINUTE).min(job.deadline_ms);
        return Ok(());
    }
    if matches!(job.health.as_str(), "pending" | "started") {
        job.health = "failed".into();
        job.error = Some(DEADLINE_FAILURE.into());
        job.record(Stage::Cleanup, Outcome::TimedOut, time);
    }
    job.state = State::Cleanup;
    job.record(Stage::Cleanup, Outcome::Requested, time);
    if let Some(id) = job.droplet {
        let removal = provider::cleanup(&FetchTransport, &pat, &job.name(), id).await?;
        if job.error.as_deref() == Some(CLEANUP_RETRY) {
            job.error = job
                .events
                .iter()
                .any(|event| event.outcome == Outcome::TimedOut)
                .then(|| DEADLINE_FAILURE.into());
        }
        match removal {
            Removal::Absent => {
                let confirmed = now();
                job.provider_observation = Some(("absent".into(), confirmed));
                job.confirmed_delete_ms = Some(confirmed);
                job.record(Stage::Cleanup, Outcome::Confirmed, confirmed);
                job.close(State::Gone);
            }
            Removal::Requested => {}
        }
    }
    Ok(())
}

/// Cron work is bounded and claimed by compare-and-swap. Cleanup remains pending on any failure.
pub async fn reap(env: &Env, host: &str) -> Result<()> {
    let cleanup = reap_jobs(env).await;
    let audit = export_audits(env, host).await;
    cleanup.and(audit)
}

async fn reap_jobs(env: &Env) -> Result<()> {
    let time = now();
    let rows: Vec<String> = serde_json::from_value(
        directory(env, "digitalocean.health.pending", json!({"now":time})).await?,
    )?;
    for prior in rows {
        let mut job: Job = serde_json::from_str(&prior)?;
        job.lease_until_ms = time + 2 * MINUTE;
        let claim = serde_json::to_string(&job)?;
        if !replace(env, &prior, &job).await? {
            continue;
        }
        job.next_ms = time + MINUTE;
        if reap_one(env, &mut job, time).await.is_err() {
            if job.state == State::Cleanup || time >= job.deadline_ms {
                job.record(Stage::Cleanup, Outcome::Retrying, time);
                job.error = Some(CLEANUP_RETRY.into());
            } else {
                job.provider_observation = Some(("unavailable".into(), time));
            }
        }
        job.updated_ms = Some(now());
        job.lease_until_ms = 0;
        replace(env, &claim, &job).await?;
    }
    Ok(())
}

async fn export_audits(env: &Env, host: &str) -> Result<()> {
    let jobs: Vec<Value> = serde_json::from_value(
        directory(env, "digitalocean.health.audit_due", Value::Null).await?,
    )?;
    for row in jobs {
        let (Some(instance), Some(id)) = (row["instance"].as_str(), row["job"]["id"].as_str())
        else {
            continue;
        };
        let args = json!({"instance":instance,"id":id});
        if directory(env, "digitalocean.health.audit_claim", args.clone()).await? != true {
            continue;
        }
        let status = jobs_audit::export(env, host, instance, &row["job"]).await;
        if status != ExportStatus::Pending {
            directory(
                env,
                "digitalocean.health.audit_finish",
                json!({"instance":instance, "id":id, "status":status}),
            )
            .await?;
        }
    }
    Ok(())
}

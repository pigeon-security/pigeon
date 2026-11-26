//! A bounded DigitalOcean Droplet health check for the approved instance.

use pigeon_cloud::digitalocean_droplet::{self as provider, Created, Removal};
use pigeon_cloud::transport::FetchTransport;
use pigeon_core::{crypto, oauth, sign, webauthn};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Request, Response, Result};

use super::{bad, expected, for_write, team};
use crate::Context;
use crate::console::write::Write;
use crate::instances::record::Instance;
use crate::users::directory;

pub const STATUS: &str = "digitalocean_droplet_status";
pub const SET_TOKEN: &str = "digitalocean-droplet-token-set";
pub const START: &str = "digitalocean-droplet-start";
pub const CALLBACK_PATH: &str = "/internal/droplet-health";
const CALLBACK: &str = "https://platform.pigeon.dev/internal/droplet-health";
const COMMIT: &str = "088a2dc05c1782fd8ba9a7678a5e4edd67952cb7";
const REPOSITORY: &str = "https://github.com/pigeon-security/pigeon.git";
const EXPECTED_VERSION: &str = "pigeon-ingest-runner 0.1.0";
const MINUTE: u64 = 60_000;
const MAX_MINUTES: u64 = 30;

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
        "estimated_usd": 0.02,
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

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Job {
    id: String,
    instance: String,
    state: State,
    health: String,
    started_ms: u64,
    deadline_ms: u64,
    next_ms: u64,
    lease_until_ms: u64,
    expected_version: String,
    droplet: Option<u64>,
    credential: Option<Credential>,
    callback_hash: Option<String>,
    error: Option<String>,
}

impl Job {
    pub fn terminal(&self) -> bool {
        matches!(self.state, State::Gone | State::Rejected)
    }

    fn public(&self) -> Value {
        json!({
            "id": self.id, "state": self.state, "health": self.health,
            "started_ms": self.started_ms, "deadline_ms": self.deadline_ms,
            "error": self.error,
        })
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

    fn callback(&mut self, token: &str, event: &str, version: Option<&str>, time: u64) -> bool {
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
        match event {
            "started" if self.health == "pending" => self.health = "started".into(),
            "result" if self.health == "pending" || self.health == "started" => {
                self.health = if version == Some(self.expected_version.as_str()) {
                    "passed"
                } else {
                    "failed"
                }
                .into();
                self.callback_hash = None;
                self.state = State::Cleanup;
                self.next_ms = time;
            }
            "started" if self.health == "started" => {}
            _ => return false,
        }
        true
    }
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
        "setting.replace",
        json!({
            "key": job_key(&job.instance), "previous": prior, "value": serde_json::to_string(job)?,
            "ttl_ms": super::TOKEN_KEPT_MS,
        }),
    )
    .await?;
    Ok(answer == true)
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
    let json_event = |event: &str, version: Option<&str>| {
        json!({
            "instance": job.instance, "id": job.id, "event": event, "version": version,
        })
        .to_string()
    };
    let started = json_event("started", None);
    let passed = json_event("result", Some(&job.expected_version));
    let failed = json_event("result", None);
    format!(
        r#"#!/bin/bash
set -eu
report() {{
  curl --silent --fail --max-time 10 --retry 2 --retry-all-errors -X POST '{CALLBACK}' \
    -H 'Authorization: Bearer {token}' -H 'Content-Type: application/json' --data "$1" >/dev/null
}}
report '{started}' || true
if timeout 1500 bash -c 'apt-get update -qq && apt-get install -y -qq docker.io git && docker build -f src/ingest-runner/Dockerfile --build-arg PIGEON_COMMIT={COMMIT} -t pigeon-health {REPOSITORY}#{COMMIT}' &&
   output=$(timeout 60 docker run --rm --network none --read-only pigeon-health --version) &&
   [ "$output" = '{version}' ]; then
  report '{passed}'
else
  report '{failed}'
fi
"#,
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
        droplet: None,
        credential: Some(credential),
        callback_hash: Some(oauth::hash(&callback)),
        error: None,
    };
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
            job.state = State::Waiting;
        }
        Created::Rejected(status) => {
            job.error = Some(format!("DigitalOcean refused the Droplet request (HTTP {status})"));
            job.close(State::Rejected);
        }
        Created::Unknown => job.error = Some("Droplet creation is uncertain. Pigeon will reconcile the original job without issuing a second create.".into()),
    }
    if !replace(cx.env, &prior, &job).await? {
        return status(cx.env, &record).await;
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
    if body["id"].as_str() != Some(job.id.as_str())
        || !job.callback(
            &token,
            body["event"].as_str().unwrap_or_default(),
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
        if job.state == State::Creating {
            job.state = State::Waiting;
        }
    }
    if time < job.deadline_ms && job.state != State::Cleanup {
        job.next_ms = job.deadline_ms;
        return Ok(());
    }
    if matches!(job.health.as_str(), "pending" | "started") {
        job.health = "failed".into();
        job.error = Some("No verified health result before the deadline.".into());
    }
    job.state = State::Cleanup;
    if let Some(id) = job.droplet {
        match provider::cleanup(&FetchTransport, &pat, &job.name(), id).await? {
            Removal::Absent => job.close(State::Gone),
            Removal::Requested => {}
        }
    }
    Ok(())
}

/// Cron work is bounded and claimed by compare-and-swap. Cleanup remains pending on any failure.
pub async fn reap(env: &Env) -> Result<()> {
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
            job.error = Some("Could not safely reconcile or delete the Droplet. Pigeon will retry; verify the provider credential and check the team dashboard.".into());
        }
        job.lease_until_ms = 0;
        replace(env, &claim, &job).await?;
    }
    Ok(())
}

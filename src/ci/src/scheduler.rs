//! The scheduler: every CI job's record, in one Durable Object with SQLite storage, and the
//! alarm loop that moves jobs along. It dispatches queued jobs to the connector routing picks
//! (waiting for the first-ranked one until the wait limit), applies what runners and webhooks
//! report through the state machine, polls jobs as a fallback and enforces deadlines, retries an
//! infrastructure failure once on the same connector, and fans each change out: the `pigeon-ci-events`
//! queue, the job's signed `notify` POST, and for a final result the commit's git note.
//!
//! Its class is exported by the hosting Worker, which builds it with the connectors from its
//! settings. Between reading a job and writing it back there is never an `await`, so the
//! object's single thread makes each change atomic; a job being dispatched is claimed first, so
//! the alarm and a submit never dispatch it twice.

use pigeon_core::artifacts::{self, Access};
use pigeon_core::durable::{self, Db, opt};
use pigeon_core::git::{self, Remote, Update, ZERO};
use pigeon_core::gitobj;
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{
    Env, Error, Fetch, Headers, Method, Request, RequestInit, Response, Result, SqlStorageValue,
    State, console_error, console_log, js_sys,
};

use super::config::Job;
use super::notes::{self, Line};
use super::route::{self, Availability, Candidate};
use super::state::{self, Next, Report, State as JobState};
use super::token::{self, Grant};
use super::{
    Cx, Dispatched, EVENTS, Held, NewJob, PURPOSE_ARTIFACTS, Run, Seen, Settings, SetupRun,
    account, notify,
};
use pigeon_core::buildkite_setup::{Kind, Owned};

/// The scheduler's schema, one statement per migration, applied in order and never edited.
const MIGRATIONS: [&str; 18] = [
    "CREATE TABLE jobs (id TEXT PRIMARY KEY, repo TEXT NOT NULL, commit_sha TEXT NOT NULL, name TEXT NOT NULL, job TEXT NOT NULL, class TEXT NOT NULL, pin TEXT, task TEXT, caller TEXT, caller_name TEXT, notify_url TEXT, notify_key TEXT, state TEXT NOT NULL, attempt INTEGER NOT NULL, connector TEXT, handle TEXT, url TEXT, excluded TEXT NOT NULL, token_hash TEXT, token_expires_at INTEGER, deadline_at INTEGER, claimed_at INTEGER, next_poll_at INTEGER, created_at INTEGER NOT NULL, queued_at INTEGER NOT NULL, dispatched_at INTEGER, started_at INTEGER, finished_at INTEGER, detail TEXT)",
    "CREATE INDEX jobs_state ON jobs (state)",
    "CREATE INDEX jobs_commit ON jobs (repo, commit_sha)",
    "CREATE UNIQUE INDEX jobs_token ON jobs (token_hash)",
    "CREATE TABLE events (job TEXT NOT NULL, at INTEGER NOT NULL, state TEXT NOT NULL, detail TEXT)",
    "CREATE INDEX events_job ON events (job)",
    "CREATE TABLE outbox (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, job TEXT, repo TEXT, body TEXT, tries INTEGER NOT NULL, next_at INTEGER NOT NULL)",
    "CREATE TABLE notes (repo TEXT NOT NULL, commit_sha TEXT NOT NULL, text TEXT NOT NULL, pushed INTEGER NOT NULL, PRIMARY KEY (repo, commit_sha))",
    "CREATE TABLE pool (connector TEXT PRIMARY KEY, availability TEXT NOT NULL, checked_at INTEGER NOT NULL)",
    "ALTER TABLE jobs ADD COLUMN exit_code INTEGER",
    "ALTER TABLE jobs ADD COLUMN output TEXT",
    "CREATE TABLE setup_owned (connector TEXT NOT NULL, kind TEXT NOT NULL, key TEXT NOT NULL, id TEXT NOT NULL, spec_hash TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (connector, kind, key))",
    "CREATE TABLE setup_keys (connector TEXT PRIMARY KEY, key TEXT NOT NULL, created_at INTEGER NOT NULL)",
    "CREATE TABLE setup_runs (connector TEXT PRIMARY KEY, at INTEGER NOT NULL, applied INTEGER NOT NULL, report TEXT NOT NULL)",
    "CREATE TABLE setup_builds (build TEXT PRIMARY KEY, at INTEGER NOT NULL)",
    "ALTER TABLE jobs ADD COLUMN image TEXT",
    "ALTER TABLE jobs ADD COLUMN instance TEXT",
    "ALTER TABLE jobs ADD COLUMN retry_on TEXT",
];

/// How long the pool's availability is trusted.
const POOL_TTL_MS: i64 = 30_000;
/// How often a live job's connector is asked, whatever its webhooks say.
const POLL_MS: i64 = 120_000;
/// What a deadline allows beyond the job's timeout, for queueing at the connector.
const DEADLINE_GRACE_MS: i64 = 10 * 60_000;
/// How long a claim on a queued job holds, should a dispatch never return.
const CLAIM_MS: i64 = 120_000;
/// How soon the alarm comes back while anything is queued.
const QUEUED_RECHECK_MS: i64 = 15_000;
/// How many times a notify POST and a notes push are tried.
const NOTIFY_TRIES: i64 = 3;
const NOTE_TRIES: i64 = 10;
/// How many times one notes push re-reads the tip after losing a race.
const NOTE_RACE_TRIES: u32 = 3;
/// How long an outbox item being sent is kept from being sent again.
const OUTBOX_LEASE_MS: i64 = 5 * 60_000;

const LIVE: &str = "('dispatched', 'running')";

fn now() -> i64 {
    js_sys::Date::now() as i64
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn opt_i(v: Option<i64>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, Into::into)
}

/// A job as kept.
#[derive(Deserialize, Debug, Clone)]
struct Row {
    id: String,
    instance: Option<String>,
    repo: String,
    commit_sha: String,
    name: String,
    job: String,
    class: String,
    pin: Option<String>,
    task: Option<String>,
    caller: Option<String>,
    caller_name: Option<String>,
    notify_url: Option<String>,
    notify_key: Option<String>,
    state: String,
    attempt: i64,
    connector: Option<String>,
    handle: Option<String>,
    url: Option<String>,
    excluded: String,
    token_expires_at: Option<i64>,
    deadline_at: Option<i64>,
    claimed_at: Option<i64>,
    created_at: i64,
    queued_at: i64,
    dispatched_at: Option<i64>,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    detail: Option<String>,
    exit_code: Option<i64>,
    output: Option<String>,
    /// The runner image recorded at dispatch; `None` where the connector records none.
    image: Option<String>,
    /// The connector a retry stays on ([`retry_on`]); `None` on a first attempt.
    retry_on: Option<String>,
}

impl Row {
    fn state(&self) -> JobState {
        JobState::parse(&self.state).unwrap_or(JobState::Error)
    }

    fn attempt(&self) -> u32 {
        u32::try_from(self.attempt).unwrap_or(state::MAX_ATTEMPTS)
    }

    /// How long the script ran: from the runner's start (or the dispatch) to the end.
    fn duration_ms(&self) -> Option<u64> {
        self.finished_at
            .zip(self.started_at.or(self.dispatched_at))
            .map(|(f, s)| (f - s).max(0) as u64)
    }

    fn excluded(&self) -> Vec<String> {
        serde_json::from_str(&self.excluded).unwrap_or_default()
    }

    fn job(&self) -> Result<Job> {
        serde_json::from_str(&self.job).map_err(|e| bad(&format!("job {}: {e}", self.id)))
    }

    /// The job as callers see it: never its token's hash or its notify key.
    fn public(&self) -> Value {
        json!({
            "id": self.id, "repo": self.repo, "commit": self.commit_sha, "job": self.name,
            "class": self.class, "state": self.state, "attempt": self.attempt,
            "connector": self.connector, "url": self.url, "pin": self.pin, "task": self.task,
            "caller": self.caller, "caller_name": self.caller_name,
            "notify": self.notify_url.is_some(), "detail": self.detail,
            "created_at": self.created_at, "queued_at": self.queued_at,
            "dispatched_at": self.dispatched_at, "started_at": self.started_at,
            "finished_at": self.finished_at, "deadline_at": self.deadline_at,
            "exit_code": self.exit_code, "duration_ms": self.duration_ms(),
            "image": self.image, "retry_on": self.retry_on,
        })
    }
}

/// What dispatching an attempt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The connector took it.
    Dispatched(Dispatched),
    /// It goes back in the queue, to retry on `failed`, the connector that refused it
    /// ([`retry_on`]).
    Requeue { failed: String, why: String },
    /// It ends as an error.
    Fail { why: String },
}

/// What a dispatch's result does to a job on `attempt`, sent to `connector`: a refusal is an
/// infrastructure failure, retried once on the same connector.
pub fn after_dispatch(
    attempt: u32,
    connector: &str,
    result: std::result::Result<Dispatched, String>,
) -> Outcome {
    match result {
        Ok(d) => Outcome::Dispatched(d),
        Err(why) => {
            let report = Report::Infra { why: why.clone() };
            match state::on_report(JobState::Dispatched, attempt, &report) {
                Next::Retry => Outcome::Requeue {
                    failed: connector.to_string(),
                    why: format!("{connector}: {why}"),
                },
                _ => Outcome::Fail {
                    why: format!("{connector}: {why}"),
                },
            }
        }
    }
}

/// Where a retry goes after connector `failed` lost an attempt to an infrastructure failure (a
/// refused dispatch, a container that stopped before reporting), and what the job says: back
/// to `failed`, a fresh runner there, within [`state::MAX_ATTEMPTS`]. Never elsewhere: routing
/// matches hardware, not what a connector's runners carry, so another connector that covers the
/// class may still lack what the job needs (Buildkite's hosted agents have no Rust toolchain
/// and no pigeon base image, so `test` sent there exits 127 at `cargo: not found`). A job that
/// fails (a result, after reporting) is never retried at all.
pub fn retry_on(failed: &str, pin: Option<&str>, why: &str) -> (Option<String>, String) {
    if failed.is_empty() {
        return (None, format!("retrying: {why}"));
    }
    let said = if pin == Some(failed) {
        format!("retrying on {failed}, which it is pinned to: {why}")
    } else {
        format!("retrying on {failed}, a fresh runner: {why}")
    };
    (Some(failed.to_string()), said)
}

/// A commit's note lines from its finished jobs, oldest first: the latest result per job name,
/// so two jobs finishing together each have their line and a rerun replaces its own.
fn note_lines(finished: Vec<Row>) -> Vec<Line> {
    let mut latest: std::collections::BTreeMap<String, Row> = Default::default();
    for r in finished {
        latest.insert(r.name.clone(), r);
    }
    latest
        .into_values()
        .map(|r| Line {
            job: r.name.clone(),
            state: r.state.clone(),
            connector: r.connector.clone(),
            attempt: r.attempt(),
            exit_code: r.exit_code,
            duration_ms: r.duration_ms(),
            url: r.url.clone(),
        })
        .collect()
}

/// What a successful notes push leaves in the outbox.
#[derive(Debug, PartialEq, Eq)]
pub enum AfterPush {
    /// Nothing: every note is pushed.
    Done,
    /// Another push: a note changed while this one was in flight.
    Again,
}

/// What a successful notes push does next, with `unpushed` notes still marked unpushed.
pub fn after_push(unpushed: i64) -> AfterPush {
    if unpushed > 0 {
        AfterPush::Again
    } else {
        AfterPush::Done
    }
}

/// `a` with the gateway's own `limit` on live jobs applied: free slots are the fewer of what
/// the connector says and what the limit leaves.
pub fn with_limit(a: Availability, limit: Option<u32>, live: u32) -> Availability {
    let Some(limit) = limit else { return a };
    let left = limit.saturating_sub(live);
    Availability {
        free: Some(a.free.map_or(left, |f| f.min(left))),
        ..a
    }
}

/// A job's deadline, dispatched at `at_ms` with `timeout_min`.
pub fn deadline(at_ms: i64, timeout_min: u32) -> i64 {
    at_ms + i64::from(timeout_min) * 60_000 + DEADLINE_GRACE_MS
}

/// The scheduler's logic; the hosting Worker's Durable Object class wraps it.
pub struct Scheduler {
    db: Db,
    state: State,
    env: Env,
    settings: Settings,
}

impl Scheduler {
    /// The scheduler, its migrations applied.
    pub fn new(state: State, env: Env, settings: Settings) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self {
            db,
            state,
            env,
            settings,
        })
    }

    /// Answers an operation the capability sent with `durable::call`.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        // A deploy's first request to this build's scheduler (the Lander's polls, a job, a
        // setup call) brings the alarm forward when this build has not applied the setup yet.
        if self.setup_pending().unwrap_or(false) {
            let storage = self.state.storage();
            if storage.get_alarm().await?.is_none_or(|c| c > now() + 1_000) {
                storage.set_alarm(1_000).await?;
            }
        }
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    /// Whether this build has yet to apply the connectors' setup: each deploy applies it once,
    /// here, whichever build the Lander that follows the landing is.
    fn setup_pending(&self) -> Result<bool> {
        if super::BUILD.is_empty() {
            return Ok(false);
        }
        #[derive(Deserialize)]
        struct B {
            #[allow(dead_code)]
            build: String,
        }
        Ok(self
            .db
            .row::<B>(
                "SELECT build FROM setup_builds WHERE build = ?",
                vec![super::BUILD.into()],
            )?
            .is_none())
    }

    /// Notes that this build has applied the setup (even when it failed: the next deploy, or
    /// the next landing's Lander, tries again; nothing loops).
    fn setup_done(&self) -> Result<()> {
        if super::BUILD.is_empty() {
            return Ok(());
        }
        self.db.exec(
            "INSERT OR REPLACE INTO setup_builds (build, at) VALUES (?, ?)",
            vec![super::BUILD.into(), now().into()],
        )
    }

    /// The alarm: dispatch, deadlines, polls, the outbox; then the next alarm.
    pub async fn alarm(&self) -> Result<Response> {
        if self.setup_pending().unwrap_or(false) {
            self.setup_done()?;
            if let Err(e) = self.setup("", true).await {
                console_error!("ci: setup after deploy: {e}");
            }
        }
        if let Err(e) = self.heal() {
            console_error!("ci: queueing notes pushes: {e}");
        }
        if let Err(e) = self.pump().await {
            console_error!("ci: dispatching: {e}");
        }
        if let Err(e) = self.overdue().await {
            console_error!("ci: deadlines and polls: {e}");
        }
        if let Err(e) = self.outbox().await {
            console_error!("ci: outbox: {e}");
        }
        self.reschedule().await?;
        Response::ok("ok")
    }

    fn cx(&self) -> Cx<'_> {
        Cx {
            env: &self.env,
            host: &self.settings.host,
            accounts: &self.settings.accounts,
            caller: None,
            key: None,
        }
    }

    /// What the scheduler holds for connector `name`'s setup.
    fn held(&self, name: &str) -> Result<Held> {
        #[derive(Deserialize)]
        struct O {
            kind: String,
            key: String,
            id: String,
            spec_hash: Option<String>,
        }
        #[derive(Deserialize)]
        struct K {
            key: String,
        }
        let owned: Vec<O> = self.db.rows(
            "SELECT kind, key, id, spec_hash FROM setup_owned WHERE connector = ? ORDER BY rowid",
            vec![name.into()],
        )?;
        let key: Option<K> = self.db.row(
            "SELECT key FROM setup_keys WHERE connector = ?",
            vec![name.into()],
        )?;
        Ok(Held {
            owned: owned
                .into_iter()
                .filter_map(|o| {
                    Some(Owned {
                        kind: Kind::parse(&o.kind)?,
                        key: o.key,
                        id: o.id,
                        spec_hash: o.spec_hash,
                    })
                })
                .collect(),
            hook_key: key.map(|k| k.key),
        })
    }

    /// Runs connector `name`'s setup (each connector's, when empty), applying it when
    /// `apply`, and keeps what it made. Answers each connector's report.
    async fn setup(&self, name: &str, apply: bool) -> Result<Value> {
        let cx = self.cx();
        let mut out = serde_json::Map::new();
        for c in &self.settings.connectors {
            if !name.is_empty() && c.name() != name {
                continue;
            }
            let held = self.held(c.name())?;
            let run = match c.setup(&cx, &held, apply).await {
                Ok(Some(run)) => run,
                Ok(None) => continue,
                Err(e) => SetupRun {
                    report: json!({ "error": e.to_string() }),
                    ..SetupRun::default()
                },
            };
            if let Some(key) = &run.hook_key {
                self.db.exec(
                    "INSERT OR REPLACE INTO setup_keys (connector, key, created_at) VALUES (?, ?, ?)",
                    vec![c.name().into(), key.as_str().into(), now().into()],
                )?;
            }
            for o in &run.record {
                self.db.exec(
                    "INSERT OR REPLACE INTO setup_owned (connector, kind, key, id, spec_hash, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                    vec![
                        c.name().into(),
                        o.kind.name().into(),
                        o.key.as_str().into(),
                        o.id.as_str().into(),
                        o.spec_hash.as_deref().map_or(SqlStorageValue::Null, Into::into),
                        now().into(),
                    ],
                )?;
            }
            if apply {
                self.db.exec(
                    "INSERT OR REPLACE INTO setup_runs (connector, at, applied, report) VALUES (?, ?, 1, ?)",
                    vec![c.name().into(), now().into(), run.report.to_string().into()],
                )?;
            }
            out.insert(c.name().to_string(), run.report);
        }
        Ok(Value::Object(out))
    }

    fn row(&self, id: &str) -> Result<Option<Row>> {
        self.db
            .row("SELECT * FROM jobs WHERE id = ?", vec![id.into()])
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        match op {
            "submit" => {
                let new: NewJob = serde_json::from_value(a.clone())?;
                let id = new.id.clone();
                self.insert(&new)?;
                if let Err(e) = self.pump().await {
                    console_error!("ci: dispatching: {e}");
                }
                self.reschedule().await?;
                let row = self.row(&id)?.ok_or_else(|| bad("the job vanished"))?;
                Ok(row.public())
            }
            "pool" => Ok(self
                .pool()
                .await?
                .into_iter()
                .map(|(name, a)| (name, json!(a)))
                .collect::<serde_json::Map<_, _>>()
                .into()),
            "status" => self.status(s("id"), None),
            "instance_status" => {
                let scope = serde_json::from_value(a["scope"].clone())?;
                let id = a["query"]["id"]
                    .as_str()
                    .ok_or_else(|| bad("`id` is required"))?;
                self.status(id, Some(&scope))
            }
            "instance_jobs" => {
                let scope = serde_json::from_value(a["scope"].clone())?;
                self.jobs(&a["query"], Some(&scope))
            }
            "setup" => {
                // A caller of another build (the old Lander, or a new one meeting this old
                // scheduler) is refused, so stale settings are never applied.
                let want = s("build");
                if !want.is_empty() && !super::BUILD.is_empty() && want != super::BUILD {
                    return Err(bad(&format!(
                        "the CI scheduler is build {}, not {want}",
                        super::BUILD
                    )));
                }
                let apply = a["apply"].as_bool() == Some(true);
                if apply {
                    self.setup_done()?;
                }
                let reports = self.setup(s("connector"), apply).await?;
                #[derive(Deserialize)]
                struct R {
                    connector: String,
                    at: i64,
                    report: String,
                }
                let last: Vec<R> = self
                    .db
                    .rows("SELECT connector, at, report FROM setup_runs", vec![])?;
                let last: serde_json::Map<String, Value> = last
                    .into_iter()
                    .map(|r| {
                        let report = serde_json::from_str(&r.report).unwrap_or(Value::Null);
                        (r.connector, json!({ "at": r.at, "report": report }))
                    })
                    .collect();
                Ok(json!({ "build": super::BUILD, "reports": reports, "last_applied": last }))
            }
            "setup_key" => Ok(json!(self.held(s("connector"))?.hook_key)),
            "reusable" => self.reusable(a),
            "jobs" => self.jobs(a, None),
            "cancel" => self.cancel(s("id")).await,
            "commit" => {
                self.heal()?;
                self.reschedule().await?;
                self.commit(s("repo"), s("commit"))
            }
            "runner" => {
                let report: Report = serde_json::from_value(a["report"].clone())?;
                let Some(row) = self.db.row::<Row>(
                    "SELECT * FROM jobs WHERE id = ? AND token_hash = ?",
                    vec![s("job").into(), s("hash").into()],
                )?
                else {
                    return Ok(Value::Null);
                };
                if !token::allows_job(&self.grant_of(&row), now(), s("job")) {
                    return Ok(Value::Null);
                }
                let detail = a["detail"].as_str().map(String::from);
                if matches!(report, Report::Finished { .. }) && !row.state().terminal() {
                    self.db.exec(
                        "UPDATE jobs SET exit_code = ?, output = ? WHERE id = ?",
                        vec![
                            opt_i(a["exit_code"].as_i64()),
                            opt(a["output"].as_str()),
                            row.id.as_str().into(),
                        ],
                    )?;
                }
                self.apply(&row.id, &report, detail, None).await?;
                self.reschedule().await?;
                let row = self.row(&row.id)?.ok_or_else(|| bad("the job vanished"))?;
                Ok(json!({ "state": row.state }))
            }
            "seen" => {
                let seen: Seen = serde_json::from_value(a["seen"].clone())?;
                let row: Option<Row> = self.db.row(
                    &format!(
                        "SELECT * FROM jobs WHERE connector = ? AND handle = ? AND state IN {LIVE}"
                    ),
                    vec![s("connector").into(), seen.handle.as_str().into()],
                )?;
                if let Some(row) = row {
                    self.apply(&row.id, &seen.report, None, seen.url).await?;
                    self.reschedule().await?;
                }
                Ok(Value::Null)
            }
            "exchange" => {
                // A connector verified a runner's OIDC token for run `handle`: the job must be
                // that run's, live, on that connector. A new job token replaces any earlier one.
                let Some(row) = self.db.row::<Row>(
                    &format!(
                        "SELECT * FROM jobs WHERE id = ? AND connector = ? AND handle = ? AND state IN {LIVE}"
                    ),
                    vec![s("job").into(), s("connector").into(), s("handle").into()],
                )?
                else {
                    return Ok(Value::Null);
                };
                let (value, hash) = token::mint()?;
                self.db.exec(
                    "UPDATE jobs SET token_hash = ?, token_expires_at = deadline_at WHERE id = ?",
                    vec![hash.as_str().into(), row.id.as_str().into()],
                )?;
                Ok(json!(value))
            }
            "grant" => {
                let row: Option<Row> = self.db.row(
                    &format!("SELECT * FROM jobs WHERE token_hash = ? AND state IN {LIVE}"),
                    vec![s("hash").into()],
                )?;
                Ok(match row {
                    Some(r) if r.token_expires_at.is_some_and(|t| now() < t) => {
                        json!(self.grant_of(&r))
                    }
                    _ => Value::Null,
                })
            }
            other => Err(bad(&format!("no operation `{other}`"))),
        }
    }

    fn grant_of(&self, row: &Row) -> Grant {
        Grant {
            job: row.id.clone(),
            repo: row.repo.clone(),
            commit: row.commit_sha.clone(),
            expires_at: row.token_expires_at.unwrap_or(0),
        }
    }

    fn insert(&self, new: &NewJob) -> Result<()> {
        let t = now();
        self.db.exec(
            "INSERT INTO jobs (id, instance, repo, commit_sha, name, job, class, pin, task, caller, caller_name, notify_url, notify_key, state, attempt, excluded, created_at, queued_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', 1, '[]', ?, ?)",
            vec![
                new.id.as_str().into(),
                opt(new.instance.as_deref()),
                new.repo.as_str().into(),
                new.commit.as_str().into(),
                new.name.as_str().into(),
                serde_json::to_string(&new.job)?.into(),
                new.class.as_str().into(),
                opt(new.pin.as_deref()),
                opt(new.task.as_deref()),
                opt(new.caller.as_deref()),
                opt(new.caller_name.as_deref()),
                opt(new.notify_url.as_deref()),
                opt(new.notify_key.as_deref()),
                t.into(),
                t.into(),
            ],
        )?;
        self.changed(&new.id, Some("submitted"))
    }

    /// Each connector's availability, asked again when older than [`POOL_TTL_MS`].
    async fn pool(&self) -> Result<Vec<(String, Availability)>> {
        #[derive(Deserialize)]
        struct Cached {
            connector: String,
            availability: String,
            checked_at: i64,
        }
        let cached: Vec<Cached> = self.db.rows("SELECT * FROM pool", vec![])?;
        let cx = self.cx();
        let mut out = Vec::new();
        for c in &self.settings.connectors {
            let fresh = cached
                .iter()
                .find(|r| r.connector == c.name() && now() - r.checked_at < POOL_TTL_MS)
                .and_then(|r| serde_json::from_str::<Availability>(&r.availability).ok());
            let a = match fresh {
                Some(a) => a,
                None => {
                    let a = match c.available(&cx).await {
                        Ok(a) => a,
                        Err(e) => {
                            console_error!("ci: {} availability: {e}", c.name());
                            Availability {
                                configured: true,
                                healthy: false,
                                free: None,
                                queued: 0,
                            }
                        }
                    };
                    self.db.exec(
                        "INSERT OR REPLACE INTO pool (connector, availability, checked_at) VALUES (?, ?, ?)",
                        vec![c.name().into(), serde_json::to_string(&a)?.into(), now().into()],
                    )?;
                    a
                }
            };
            out.push((
                c.name().to_string(),
                with_limit(a, c.limit(), self.live(c.name())?),
            ));
        }
        Ok(out)
    }

    /// How many of `connector`'s jobs are live.
    fn live(&self, connector: &str) -> Result<u32> {
        #[derive(Deserialize)]
        struct Count {
            n: i64,
        }
        Ok(self
            .db
            .row::<Count>(
                &format!("SELECT COUNT(*) AS n FROM jobs WHERE connector = ? AND state IN {LIVE}"),
                vec![connector.into()],
            )?
            .map_or(0, |c| u32::try_from(c.n).unwrap_or(u32::MAX)))
    }

    /// Dispatches each queued job that has somewhere to go.
    async fn pump(&self) -> Result<()> {
        let queued: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE state = 'queued' ORDER BY queued_at",
            vec![],
        )?;
        if queued.is_empty() {
            return Ok(());
        }
        let mut pool = self.pool().await?;
        for q in queued {
            // Read again and claim with no await between: the claim is atomic.
            let Some(row) = self.row(&q.id)? else {
                continue;
            };
            let t = now();
            if row.state() != JobState::Queued || row.claimed_at.is_some_and(|c| t - c < CLAIM_MS) {
                continue;
            }
            let job = row.job()?;
            let excluded = row.excluded();
            let retry_on = row.retry_on.as_deref().filter(|c| !c.is_empty());
            let offers: Vec<_> = self
                .settings
                .connectors
                .iter()
                .map(|c| c.offers())
                .collect();
            let candidates: Vec<Candidate> = self
                .settings
                .connectors
                .iter()
                .zip(&offers)
                .zip(&pool)
                .enumerate()
                .map(|(i, ((c, offers), (_, a)))| Candidate {
                    name: c.name(),
                    rank: i as u32,
                    offers,
                    availability: *a,
                })
                .collect();
            let ask = route::Ask {
                needs: &job.needs,
                timeout_min: job.timeout_min,
                pin: row.pin.as_deref(),
                exclude: &excluded,
                retry_on,
            };
            let routed = match route::route(&ask, &candidates) {
                Ok(r) => r,
                Err(why) => {
                    let why = why
                        .iter()
                        .map(|(c, r)| format!("{c}: {r}"))
                        .collect::<Vec<_>>()
                        .join("; ");
                    let detail = match retry_on {
                        // An infrastructure failure, said as one: the retry has nowhere it is
                        // known to suit.
                        Some(on) => format!(
                            "infrastructure: {on} lost the last attempt and cannot take the \
                             retry, and no other connector is known to suit the job: {why}"
                        ),
                        None => format!("no connector can run it: {why}"),
                    };
                    self.finish(&row, JobState::Error, Some(&detail))?;
                    continue;
                }
            };
            let fallback = route::fallback_due(row.queued_at, t, self.settings.wait_limit_ms);
            let Some(pick) = route::choose(&routed, fallback) else {
                continue;
            };
            let Some(connector) = self
                .settings
                .connectors
                .iter()
                .find(|c| c.name() == pick.connector)
            else {
                continue;
            };
            self.db.exec(
                "UPDATE jobs SET claimed_at = ? WHERE id = ?",
                vec![t.into(), row.id.as_str().into()],
            )?;
            let deadline_at = deadline(t, job.timeout_min);
            let (token, token_hash) = if connector.takes_token() {
                let (v, h) = token::mint()?;
                (Some(v), Some(h))
            } else {
                (None, None)
            };
            let host = &self.settings.host;
            let run = Run {
                job: row.id.clone(),
                attempt: row.attempt(),
                repo: row.repo.clone(),
                commit: row.commit_sha.clone(),
                name: row.name.clone(),
                script: job.run.clone(),
                class: pick.class,
                timeout_min: job.timeout_min,
                pass_token: job.token,
                token,
                runner_url: format!("https://{host}/ci/runner/{}", row.id),
                git_url: format!("https://{}/git/{}.git", self.settings.proxy_host, row.repo),
            };
            let cx = self.cx();
            let result = connector
                .dispatch(&cx, &run)
                .await
                .map_err(|e| e.to_string());
            drop(run);
            // The job may have been cancelled while the connector was asked.
            let Some(row) = self.row(&row.id)? else {
                continue;
            };
            if row.state() != JobState::Queued {
                if let Ok(d) = &result
                    && let Err(e) = connector.cancel(&cx, &d.handle).await
                {
                    console_error!(
                        "ci: {}: cancelling {} at {}: {e}",
                        row.id,
                        d.handle,
                        connector.name()
                    );
                }
                continue;
            }
            match after_dispatch(row.attempt(), connector.name(), result) {
                Outcome::Dispatched(d) => {
                    let t = now();
                    // Only the containers connector records its image; others record none.
                    let image = (connector.name() == super::containers::NAME)
                        .then(super::reuse::runner_image)
                        .flatten();
                    self.db.exec(
                        "UPDATE jobs SET state = 'dispatched', connector = ?, handle = ?, url = ?, token_hash = ?, token_expires_at = ?, deadline_at = ?, dispatched_at = ?, next_poll_at = ?, claimed_at = NULL, class = ?, image = ? WHERE id = ?",
                        vec![
                            connector.name().into(),
                            d.handle.as_str().into(),
                            opt(d.url.as_deref()),
                            opt(token_hash.as_deref()),
                            opt_i(token_hash.as_ref().map(|_| deadline_at)),
                            deadline_at.into(),
                            t.into(),
                            (t + POLL_MS).into(),
                            pick.class.name().into(),
                            opt(image.as_deref()),
                            row.id.as_str().into(),
                        ],
                    )?;
                    self.changed(&row.id, None)?;
                    if let Some((_, a)) = pool.iter_mut().find(|(n, _)| n == connector.name()) {
                        a.free = a.free.map(|f| f.saturating_sub(1));
                    }
                }
                Outcome::Requeue { failed, why } => self.requeue(&row, &failed, &why)?,
                Outcome::Fail { why } => self.finish(&row, JobState::Error, Some(&why))?,
            }
        }
        Ok(())
    }

    /// Puts a job back in the queue as its next attempt, on `failed` again ([`retry_on`]).
    fn requeue(&self, row: &Row, failed: &str, why: &str) -> Result<()> {
        let (on, said) = retry_on(failed, row.pin.as_deref(), why);
        self.db.exec(
            "UPDATE jobs SET state = 'queued', attempt = attempt + 1, retry_on = ?, connector = NULL, handle = NULL, url = NULL, token_hash = NULL, token_expires_at = NULL, deadline_at = NULL, claimed_at = NULL, next_poll_at = NULL, dispatched_at = NULL, started_at = NULL, image = NULL, queued_at = ?, detail = ? WHERE id = ?",
            vec![
                opt(on.as_deref()),
                now().into(),
                said.as_str().into(),
                row.id.as_str().into(),
            ],
        )?;
        self.changed(&row.id, Some(&said))
    }

    /// Ends a job in terminal state `to`; its token dies with it.
    fn finish(&self, row: &Row, to: JobState, detail: Option<&str>) -> Result<()> {
        if !to.terminal() || !state::allowed(row.state(), to) {
            return Ok(());
        }
        self.db.exec(
            "UPDATE jobs SET state = ?, finished_at = ?, token_hash = NULL, token_expires_at = NULL, claimed_at = NULL, next_poll_at = NULL, detail = COALESCE(?, detail) WHERE id = ?",
            vec![
                to.as_str().into(),
                now().into(),
                opt(detail),
                row.id.as_str().into(),
            ],
        )?;
        self.changed(&row.id, detail)
    }

    /// Applies `report` on job `id` through the state machine.
    async fn apply(
        &self,
        id: &str,
        report: &Report,
        detail: Option<String>,
        url: Option<String>,
    ) -> Result<()> {
        let Some(row) = self.row(id)? else {
            return Ok(());
        };
        if let Some(u) = &url {
            self.db.exec(
                "UPDATE jobs SET url = ? WHERE id = ?",
                vec![u.as_str().into(), id.into()],
            )?;
        }
        match state::on_report(row.state(), row.attempt(), report) {
            Next::Ignore => Ok(()),
            Next::Retry => {
                let why = match report {
                    Report::Infra { why } => why.clone(),
                    _ => "lost".to_string(),
                };
                let connector = row.connector.clone().unwrap_or_default();
                self.requeue(&row, &connector, &why)?;
                // Best effort: the lost run should not go on beside the retry.
                if let (Some(c), Some(h)) = (self.settings.connector(&connector), &row.handle)
                    && let Err(e) = c.cancel(&self.cx(), h).await
                {
                    console_error!("ci: {id}: cancelling {h} at {connector}: {e}");
                }
                Ok(())
            }
            Next::Move(JobState::Running) => {
                self.db.exec(
                    "UPDATE jobs SET state = 'running', started_at = ? WHERE id = ? AND state = 'dispatched'",
                    vec![now().into(), id.into()],
                )?;
                self.changed(id, detail.as_deref())
            }
            Next::Move(to) => self.finish(&row, to, detail.as_deref()),
        }
    }

    /// Records the job's current state as an event and fans it out.
    fn changed(&self, id: &str, detail: Option<&str>) -> Result<()> {
        let row = self.row(id)?.ok_or_else(|| bad("the job vanished"))?;
        let t = now();
        self.db.exec(
            "INSERT INTO events (job, at, state, detail) VALUES (?, ?, ?, ?)",
            vec![id.into(), t.into(), row.state.as_str().into(), opt(detail)],
        )?;
        let event = json!({
            "source": "ci", "job": row.id, "repo": row.repo, "commit": row.commit_sha,
            "name": row.name, "state": row.state, "attempt": row.attempt,
            "connector": row.connector, "url": row.url, "detail": detail, "at": t,
        });
        // The queue send is awaited by the alarm through the outbox, like the rest.
        self.db.exec(
            "INSERT INTO outbox (kind, job, body, tries, next_at) VALUES ('event', ?, ?, 0, ?)",
            vec![id.into(), event.to_string().into(), t.into()],
        )?;
        if row.notify_url.is_some() {
            self.db.exec(
                "INSERT INTO outbox (kind, job, body, tries, next_at) VALUES ('notify', ?, ?, 0, ?)",
                vec![id.into(), event.to_string().into(), t.into()],
            )?;
        }
        if row.state().terminal() {
            self.note(&row.repo, &row.commit_sha)?;
        }
        Ok(())
    }

    /// Rewrites the commit's note from its jobs' final results and queues the push.
    fn note(&self, repo: &str, commit: &str) -> Result<()> {
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE repo = ? AND commit_sha = ? AND finished_at IS NOT NULL ORDER BY finished_at",
            vec![repo.into(), commit.into()],
        )?;
        self.db.exec(
            "INSERT OR REPLACE INTO notes (repo, commit_sha, text, pushed) VALUES (?, ?, ?, 0)",
            vec![
                repo.into(),
                commit.into(),
                notes::text(&note_lines(rows)).into(),
            ],
        )?;
        self.ensure_note_push(repo)
    }

    /// Queues a notes push for `repo` unless one is queued. A push in flight stays queued until
    /// it ends, and on success is queued again while any note is still unpushed, so a note
    /// rewritten during a push is never left behind.
    fn ensure_note_push(&self, repo: &str) -> Result<()> {
        if self.count(
            "SELECT COUNT(*) AS n FROM outbox WHERE kind = 'note' AND repo = ?",
            repo,
        )? == 0
        {
            self.db.exec(
                "INSERT INTO outbox (kind, repo, tries, next_at) VALUES ('note', ?, 0, ?)",
                vec![repo.into(), now().into()],
            )?;
        }
        Ok(())
    }

    /// Queues a push for every repository with a note not yet pushed and none queued: what
    /// catches up a push lost before this fix.
    fn heal(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct Repo {
            repo: String,
        }
        let repos: Vec<Repo> = self
            .db
            .rows("SELECT DISTINCT repo FROM notes WHERE pushed = 0", vec![])?;
        for r in repos {
            self.ensure_note_push(&r.repo)?;
        }
        Ok(())
    }

    /// The number `query` (one `?`, bound to `arg`) counts as `n`.
    fn count(&self, query: &str, arg: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct Count {
            n: i64,
        }
        Ok(self
            .db
            .row::<Count>(query, vec![arg.into()])?
            .map_or(0, |c| c.n))
    }

    /// Live jobs past their deadline, and live jobs due a poll.
    async fn overdue(&self) -> Result<()> {
        let t = now();
        let due: Vec<Row> = self.db.rows(
            &format!("SELECT * FROM jobs WHERE state IN {LIVE} AND (deadline_at <= ? OR next_poll_at <= ?)"),
            vec![t.into(), t.into()],
        )?;
        let cx = self.cx();
        for row in due {
            let (Some(name), Some(handle)) = (&row.connector, &row.handle) else {
                continue;
            };
            let Some(c) = self.settings.connector(name) else {
                continue;
            };
            let last = match c.status(&cx, handle).await {
                Ok(r) => r,
                Err(e) => {
                    console_error!("ci: {}: status at {name}: {e}", row.id);
                    None
                }
            };
            if row.deadline_at.is_some_and(|d| d <= t) {
                let Some(fresh) = self.row(&row.id)? else {
                    continue;
                };
                match state::on_deadline(fresh.state(), last.as_ref()) {
                    Next::Move(JobState::TimedOut) => {
                        if let Err(e) = c.cancel(&cx, handle).await {
                            console_error!("ci: {}: cancelling at {name}: {e}", row.id);
                        }
                        self.finish(&fresh, JobState::TimedOut, Some("past its deadline"))?;
                    }
                    Next::Move(to) => self.finish(&fresh, to, None)?,
                    _ => {}
                }
            } else {
                if let Some(r) = &last {
                    self.apply(&row.id, r, None, None).await?;
                }
                self.db.exec(
                    "UPDATE jobs SET next_poll_at = ? WHERE id = ?",
                    vec![(now() + POLL_MS).into(), row.id.as_str().into()],
                )?;
            }
        }
        Ok(())
    }

    /// Sends what the outbox holds that is due.
    async fn outbox(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct Item {
            id: i64,
            kind: String,
            job: Option<String>,
            repo: Option<String>,
            body: Option<String>,
            tries: i64,
        }
        let due: Vec<Item> = self.db.rows(
            "SELECT id, kind, job, repo, body, tries FROM outbox WHERE next_at <= ? ORDER BY id LIMIT 50",
            vec![now().into()],
        )?;
        for item in due {
            // Leased while it is sent, so nothing else sends it at the same time.
            self.db.exec(
                "UPDATE outbox SET next_at = ? WHERE id = ?",
                vec![(now() + OUTBOX_LEASE_MS).into(), item.id.into()],
            )?;
            let (sent, limit) = match item.kind.as_str() {
                "event" => (self.send_event(item.body.as_deref()).await, 1),
                "notify" => (
                    self.send_notify(item.job.as_deref(), item.body.as_deref())
                        .await,
                    NOTIFY_TRIES,
                ),
                "note" => (
                    self.push_notes(item.repo.as_deref().unwrap_or_default())
                        .await,
                    NOTE_TRIES,
                ),
                _ => (Ok(()), 1),
            };
            let unpushed = match (&sent, item.kind.as_str()) {
                (Ok(()), "note") => self.count(
                    "SELECT COUNT(*) AS n FROM notes WHERE repo = ? AND pushed = 0",
                    item.repo.as_deref().unwrap_or_default(),
                )?,
                _ => 0,
            };
            match sent {
                Ok(()) if after_push(unpushed) == AfterPush::Again => self.db.exec(
                    "UPDATE outbox SET tries = 0, next_at = ? WHERE id = ?",
                    vec![now().into(), item.id.into()],
                )?,
                Ok(()) => self
                    .db
                    .exec("DELETE FROM outbox WHERE id = ?", vec![item.id.into()])?,
                Err(e) if item.tries + 1 >= limit => {
                    console_error!(
                        "ci: {} for {} dropped after {} tries: {e}",
                        item.kind,
                        item.job.or(item.repo).unwrap_or_default(),
                        item.tries + 1
                    );
                    self.db
                        .exec("DELETE FROM outbox WHERE id = ?", vec![item.id.into()])?;
                }
                Err(e) => {
                    console_log!("ci: {} will be tried again: {e}", item.kind);
                    let wait = 30_000 * (1 << item.tries.clamp(0, 6));
                    self.db.exec(
                        "UPDATE outbox SET tries = tries + 1, next_at = ? WHERE id = ?",
                        vec![(now() + wait).into(), item.id.into()],
                    )?;
                }
            }
        }
        Ok(())
    }

    async fn send_event(&self, body: Option<&str>) -> Result<()> {
        let event: Value = serde_json::from_str(body.unwrap_or("null"))?;
        self.env.queue(EVENTS)?.send(event).await
    }

    /// POSTs a change to the job's `notify` URL, signed with the job's key. Logs never hold the
    /// key or the body.
    async fn send_notify(&self, job: Option<&str>, body: Option<&str>) -> Result<()> {
        let Some(row) = job.map(|j| self.row(j)).transpose()?.flatten() else {
            return Ok(());
        };
        let (Some(url), Some(key)) = (&row.notify_url, &row.notify_key) else {
            return Ok(());
        };
        let body = body.unwrap_or("{}");
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        headers.set("User-Agent", "pigeon-ci")?;
        headers.set(
            notify::HEADER,
            &notify::signature(key.as_bytes(), now() / 1000, body.as_bytes()),
        )?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(body.into()));
        let res = Fetch::Request(Request::new_with_init(url, &init)?)
            .send()
            .await?;
        let status = res.status_code();
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(bad(&format!("notify for {} answered {status}", row.id)))
        }
    }

    /// Pushes `repo`'s notes as one notes commit over the ref's current tip.
    async fn push_notes(&self, repo: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct Note {
            commit_sha: String,
            text: String,
        }
        let all: Vec<Note> = self.db.rows(
            "SELECT commit_sha, text FROM notes WHERE repo = ? ORDER BY commit_sha",
            vec![repo.into()],
        )?;
        if all.is_empty() {
            return Ok(());
        }
        let (ns, name) = repo
            .split_once('/')
            .ok_or_else(|| bad(&format!("not a repository: {repo}")))?;
        let account = account(&self.cx(), &[PURPOSE_ARTIFACTS]).await?;
        let found = artifacts::repo(&account, ns, name)
            .await?
            .ok_or_else(|| bad(&format!("no repository {repo}")))?;
        let minted = artifacts::mint(&account, ns, name, Access::Write, 300).await?;
        let remote = Remote {
            url: found.remote,
            authorization: format!("Bearer {}", minted.value),
        };
        let pairs: Vec<(String, String)> = all
            .iter()
            .map(|n| (n.commit_sha.clone(), n.text.clone()))
            .collect();
        let pushed = async {
            let mut last = String::new();
            // Each try reads the ref's tip afresh, so a race lost to another push is retried
            // over the new tip.
            for attempt in 1..=NOTE_RACE_TRIES {
                let refs = git::refs(&remote, "git-receive-pack").await?;
                let tip = refs.refs.get(notes::REF).cloned();
                let (objects, id) =
                    notes::commit(&pairs, tip.as_deref(), now() / 1000).map_err(|e| bad(&e))?;
                let pack = gitobj::pack(&objects);
                let update = Update {
                    name: notes::REF.to_string(),
                    old: tip.clone().unwrap_or_else(|| ZERO.to_string()),
                    new: id,
                };
                let status = git::push(&remote, &refs.caps, &[update], &pack).await?;
                if status.iter().any(|s| s == &format!("ok {}", notes::REF)) {
                    return Ok(());
                }
                last = status.join("; ");
                console_error!(
                    "ci: notes push for {repo} over {} refused (try {attempt}): {last}",
                    tip.as_deref().unwrap_or("nothing")
                );
            }
            Err(bad(&format!("push of {}: {last}", notes::REF)))
        }
        .await;
        if let Some(id) = &minted.id
            && let Err(e) = artifacts::revoke(&account, ns, name, id).await
        {
            console_error!("ci: revoking the notes push token: {e}");
        }
        pushed?;
        for (commit, text) in &pairs {
            self.db.exec(
                "UPDATE notes SET pushed = 1 WHERE repo = ? AND commit_sha = ? AND text = ?",
                vec![repo.into(), commit.as_str().into(), text.as_str().into()],
            )?;
        }
        Ok(())
    }

    /// Sets the alarm for the next thing due, if anything is.
    async fn reschedule(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct At {
            at: Option<i64>,
        }
        let t = now();
        let one = |q: &str| -> Result<Option<i64>> {
            Ok(self.db.row::<At>(q, vec![])?.and_then(|a| a.at))
        };
        let candidates = [
            one("SELECT MIN(queued_at) AS at FROM jobs WHERE state = 'queued'")?
                .map(|_| t + QUEUED_RECHECK_MS),
            one(&format!(
                "SELECT MIN(deadline_at) AS at FROM jobs WHERE state IN {LIVE}"
            ))?,
            one(&format!(
                "SELECT MIN(next_poll_at) AS at FROM jobs WHERE state IN {LIVE}"
            ))?,
            one("SELECT MIN(next_at) AS at FROM outbox")?,
        ];
        match candidates.into_iter().flatten().min() {
            Some(at) => {
                let storage = self.state.storage();
                let current = storage.get_alarm().await?;
                if current.is_none_or(|c| c > at) {
                    storage.set_alarm((at - t).max(0)).await?;
                }
            }
            None => self.state.storage().delete_alarm().await?,
        }
        Ok(())
    }

    fn status(&self, id: &str, scope: Option<&super::scope::Scope>) -> Result<Value> {
        let row = self
            .row(id)?
            .filter(|row| {
                scope
                    .is_none_or(|scope| scope.allows(row.instance.as_deref(), &row.repo, &row.name))
            })
            .ok_or_else(|| bad("no such job"))?;
        let events: Vec<Value> = self.db.rows(
            "SELECT at, state, detail FROM events WHERE job = ? ORDER BY at, rowid",
            vec![id.into()],
        )?;
        let mut out = row.public();
        out["output"] = json!(row.output);
        out["excluded"] = json!(row.excluded());
        out["events"] = json!(events);
        Ok(out)
    }

    fn jobs(&self, a: &Value, scope: Option<&super::scope::Scope>) -> Result<Value> {
        let mut q = String::from("SELECT * FROM jobs WHERE 1 = 1");
        let mut args: Vec<SqlStorageValue> = Vec::new();
        if let Some(scope) = scope {
            q.push_str(
                " AND instance = ? AND repo = ? AND name IN (SELECT value FROM json_each(?))",
            );
            args.push(scope.instance.as_str().into());
            args.push(scope.repo.as_str().into());
            args.push(serde_json::to_string(&scope.jobs)?.into());
        }
        for (key, column) in [
            ("repo", "repo"),
            ("commit", "commit_sha"),
            ("state", "state"),
            ("task", "task"),
        ] {
            if let Some(v) = a[key].as_str() {
                q.push_str(&format!(" AND {column} = ?"));
                args.push(v.into());
            }
        }
        if let Some(v) = a["caller"].as_str() {
            q.push_str(" AND (caller = ? OR caller_name = ?)");
            args.push(v.into());
            args.push(v.into());
        }
        let limit = a["limit"].as_i64().unwrap_or(50).clamp(1, 200);
        q.push_str(&format!(" ORDER BY created_at DESC LIMIT {limit}"));
        let rows: Vec<Row> = self.db.rows(&q, args)?;
        Ok(json!({ "jobs": rows.iter().map(Row::public).collect::<Vec<_>>() }))
    }

    /// The newest job that may stand in for a new one ([`super::reuse::reusable`]), or `{}`.
    fn reusable(&self, a: &Value) -> Result<Value> {
        let text = |k: &str| a[k].as_str().unwrap_or_default().to_string();
        let want = super::reuse::Want {
            repo: text("repo"),
            commit: text("commit"),
            name: text("name"),
            image: text("image"),
        };
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE repo = ? AND commit_sha = ? AND name = ? AND instance IS ? AND state = 'succeeded' AND image IS NOT NULL ORDER BY finished_at DESC, created_at DESC LIMIT 50",
            vec![
                want.repo.as_str().into(),
                want.commit.as_str().into(),
                want.name.as_str().into(),
                opt(a["instance"].as_str()),
            ],
        )?;
        let candidates: Vec<super::reuse::Candidate> = rows
            .iter()
            .map(|r| super::reuse::Candidate {
                id: r.id.clone(),
                repo: r.repo.clone(),
                commit: r.commit_sha.clone(),
                name: r.name.clone(),
                state: r.state.clone(),
                image: r.image.clone(),
            })
            .collect();
        Ok(match super::reuse::reusable(&candidates, &want) {
            Some(c) => json!({ "id": c.id }),
            None => json!({}),
        })
    }

    async fn cancel(&self, id: &str) -> Result<Value> {
        let row = self.row(id)?.ok_or_else(|| bad("no such job"))?;
        if state::on_cancel(row.state()) == Next::Ignore {
            return Ok(json!({ "id": id, "state": row.state, "note": "it had already finished" }));
        }
        if let (Some(name), Some(handle)) = (&row.connector, &row.handle)
            && let Some(c) = self.settings.connector(name)
            && let Err(e) = c.cancel(&self.cx(), handle).await
        {
            console_error!("ci: {id}: cancelling at {name}: {e}");
        }
        let Some(fresh) = self.row(id)? else {
            return Err(bad("no such job"));
        };
        self.finish(&fresh, JobState::Cancelled, Some("cancelled by a caller"))?;
        self.reschedule().await?;
        let row = self.row(id)?.ok_or_else(|| bad("no such job"))?;
        Ok(row.public())
    }

    fn commit(&self, repo: &str, commit: &str) -> Result<Value> {
        #[derive(Deserialize)]
        struct Note {
            text: String,
            pushed: i64,
        }
        let note: Option<Note> = self.db.row(
            "SELECT text, pushed FROM notes WHERE repo = ? AND commit_sha = ?",
            vec![repo.into(), commit.into()],
        )?;
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE repo = ? AND commit_sha = ? ORDER BY created_at DESC",
            vec![repo.into(), commit.into()],
        )?;
        Ok(json!({
            "repo": repo, "commit": commit, "ref": notes::REF,
            "note": note.as_ref().map(|n| &n.text),
            "pushed": note.as_ref().is_some_and(|n| n.pushed == 1),
            "jobs": rows.iter().map(Row::public).collect::<Vec<_>>(),
        }))
    }
}

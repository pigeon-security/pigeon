//! The scheduler: every CI job's record, in one Durable Object with SQLite storage, and the
//! alarm loop that moves jobs along. It dispatches queued jobs to the connector routing picks
//! (waiting for the first-ranked one until the wait limit), applies what runners and webhooks
//! report through the state machine, polls jobs as a fallback and enforces deadlines, retries an
//! infrastructure failure once elsewhere, and fans each change out: the `pigeon-ci-events`
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
use pigeon_core::{Account, secrets_store};
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
use super::{Cx, Dispatched, EVENTS, NewJob, Run, Seen, Settings, notify};

/// The scheduler's schema, one statement per migration, applied in order and never edited.
const MIGRATIONS: [&str; 11] = [
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
        })
    }
}

/// What dispatching an attempt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The connector took it.
    Dispatched(Dispatched),
    /// It goes back in the queue, `exclude`d from the connector that failed it.
    Requeue { exclude: String, why: String },
    /// It ends as an error.
    Fail { why: String },
}

/// What a dispatch's result does to a job on `attempt`, sent to `connector`: a refusal is an
/// infrastructure failure, retried once elsewhere.
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
                    exclude: connector.to_string(),
                    why: format!("{connector}: {why}"),
                },
                _ => Outcome::Fail {
                    why: format!("{connector}: {why}"),
                },
            }
        }
    }
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

/// The account a git token is minted on, for the notes push and for reading `ci.toml`.
pub(crate) async fn account(env: &Env, parent: &str) -> Result<Account> {
    let parent_token = secrets_store::read(&env.secret_store(parent)?)
        .await?
        .ok_or_else(|| bad(&format!("`{parent}` is not set in the Secrets Store")))?;
    Ok(Account {
        id: env.var("CF_ACCOUNT_ID")?.to_string(),
        parent: parent_token,
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
    })
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
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    /// The alarm: dispatch, deadlines, polls, the outbox; then the next alarm.
    pub async fn alarm(&self) -> Result<Response> {
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
        }
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
            "status" => {
                let row = self.row(s("id"))?.ok_or_else(|| bad("no such job"))?;
                let events: Vec<Value> = self.db.rows(
                    "SELECT at, state, detail FROM events WHERE job = ? ORDER BY at, rowid",
                    vec![s("id").into()],
                )?;
                let mut out = row.public();
                out["output"] = json!(row.output);
                out["excluded"] = json!(row.excluded());
                out["events"] = json!(events);
                Ok(out)
            }
            "jobs" => self.jobs(a),
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
            "INSERT INTO jobs (id, repo, commit_sha, name, job, class, pin, task, caller, caller_name, notify_url, notify_key, state, attempt, excluded, created_at, queued_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'queued', 1, '[]', ?, ?)",
            vec![
                new.id.as_str().into(),
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
            };
            let routed = match route::route(&ask, &candidates) {
                Ok(r) => r,
                Err(why) => {
                    let why = why
                        .iter()
                        .map(|(c, r)| format!("{c}: {r}"))
                        .collect::<Vec<_>>()
                        .join("; ");
                    self.finish(
                        &row,
                        JobState::Error,
                        Some(&format!("no connector can run it: {why}")),
                    )?;
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
                    self.db.exec(
                        "UPDATE jobs SET state = 'dispatched', connector = ?, handle = ?, url = ?, token_hash = ?, token_expires_at = ?, deadline_at = ?, dispatched_at = ?, next_poll_at = ?, claimed_at = NULL, class = ? WHERE id = ?",
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
                            row.id.as_str().into(),
                        ],
                    )?;
                    self.changed(&row.id, None)?;
                    if let Some((_, a)) = pool.iter_mut().find(|(n, _)| n == connector.name()) {
                        a.free = a.free.map(|f| f.saturating_sub(1));
                    }
                }
                Outcome::Requeue { exclude, why } => self.requeue(&row, &exclude, &why)?,
                Outcome::Fail { why } => self.finish(&row, JobState::Error, Some(&why))?,
            }
        }
        Ok(())
    }

    /// Puts a job back in the queue as its next attempt, away from `exclude`.
    fn requeue(&self, row: &Row, exclude: &str, why: &str) -> Result<()> {
        let mut excluded = row.excluded();
        if !excluded.iter().any(|e| e == exclude) {
            excluded.push(exclude.to_string());
        }
        self.db.exec(
            "UPDATE jobs SET state = 'queued', attempt = attempt + 1, excluded = ?, connector = NULL, handle = NULL, url = NULL, token_hash = NULL, token_expires_at = NULL, deadline_at = NULL, claimed_at = NULL, next_poll_at = NULL, dispatched_at = NULL, started_at = NULL, queued_at = ?, detail = ? WHERE id = ?",
            vec![
                serde_json::to_string(&excluded)?.into(),
                now().into(),
                format!("retrying elsewhere: {why}").into(),
                row.id.as_str().into(),
            ],
        )?;
        self.changed(&row.id, Some(&format!("retrying elsewhere: {why}")))
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
        let account = account(&self.env, &self.settings.parent).await?;
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

    fn jobs(&self, a: &Value) -> Result<Value> {
        let mut q = String::from("SELECT * FROM jobs WHERE 1 = 1");
        let mut args: Vec<SqlStorageValue> = Vec::new();
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context as TaskContext, Poll, Waker};

    use pigeon_core::sign;
    use worker::wasm_bindgen::{JsCast, JsValue};

    use super::super::class::class_for;
    use super::super::class::{Class, Offered, Size};
    use super::super::config::{Arch, Needs, Os};
    use super::super::{Connector, Hook, Pending};
    use super::*;

    /// Runs a future that is ready at once, as the fake's are.
    fn ready<T>(f: impl Future<Output = T>) -> T {
        let mut f = pin!(f);
        match f.as_mut().poll(&mut TaskContext::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("the fake connector never waits"),
        }
    }

    /// A connector that answers from a script, for tests.
    struct Fake {
        name: &'static str,
        availability: Availability,
        dispatches: RefCell<Vec<std::result::Result<Dispatched, String>>>,
        secret: &'static str,
    }

    impl Fake {
        fn new(
            name: &'static str,
            dispatches: Vec<std::result::Result<Dispatched, String>>,
        ) -> Self {
            Fake {
                name,
                availability: Availability {
                    configured: true,
                    healthy: true,
                    free: Some(1),
                    queued: 0,
                },
                dispatches: RefCell::new(dispatches),
                secret: "hook-secret",
            }
        }
    }

    impl Connector for Fake {
        fn name(&self) -> &'static str {
            self.name
        }

        fn offers(&self) -> Vec<Offered> {
            vec![Offered {
                class: Class {
                    os: Os::Linux,
                    arch: Arch::Amd64,
                    size: Size::Small,
                    docker: false,
                    gpu: false,
                },
                cpu: 2,
                memory_gb: 8,
                disk_gb: 16,
                max_timeout_min: 60,
                network_none: false,
            }]
        }

        fn available<'a>(&'a self, _cx: &'a Cx<'a>) -> Pending<'a, Availability> {
            Box::pin(async { Ok(self.availability) })
        }

        fn dispatch<'a>(&'a self, _cx: &'a Cx<'a>, run: &'a Run) -> Pending<'a, Dispatched> {
            Box::pin(async move {
                let next = self.dispatches.borrow_mut().remove(0);
                next.map(|d| Dispatched {
                    handle: format!("{}-{}", d.handle, run.attempt),
                    url: d.url,
                })
                .map_err(|e| bad(&e))
            })
        }

        fn status<'a>(&'a self, _cx: &'a Cx<'a>, _h: &'a str) -> Pending<'a, Option<Report>> {
            Box::pin(async { Ok(None) })
        }

        fn cancel<'a>(&'a self, _cx: &'a Cx<'a>, _h: &'a str) -> Pending<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn webhook<'a>(&'a self, _cx: &'a Cx<'a>, hook: &'a Hook) -> Pending<'a, Option<Seen>> {
            Box::pin(async move {
                let given = hook.header("x-fake-token").unwrap_or_default();
                if !sign::equal(given.as_bytes(), self.secret.as_bytes()) {
                    return Err(bad("the token does not match"));
                }
                let v: Value = serde_json::from_slice(&hook.body)?;
                Ok(Some(Seen {
                    handle: v["handle"].as_str().unwrap_or_default().into(),
                    report: Report::Finished {
                        passed: v["passed"].as_bool().unwrap_or(false),
                    },
                    url: None,
                }))
            })
        }
    }

    /// An `Env` handle that is never used: the fake ignores it.
    fn with_cx<T>(f: impl FnOnce(&Cx<'_>) -> T) -> T {
        let env = std::mem::ManuallyDrop::new(JsValue::NULL.unchecked_into::<Env>());
        f(&Cx {
            env: &env,
            host: "platform.example.com",
        })
    }

    fn run(attempt: u32) -> Run {
        Run {
            job: "j".into(),
            attempt,
            repo: "ns/r".into(),
            commit: "a".repeat(40),
            name: "test".into(),
            script: "ci/test.sh".into(),
            class: class_for(&Needs::default()).unwrap(),
            timeout_min: 30,
            token: None,
            runner_url: String::new(),
            git_url: String::new(),
        }
    }

    fn ok(handle: &str) -> std::result::Result<Dispatched, String> {
        Ok(Dispatched {
            handle: handle.into(),
            url: None,
        })
    }

    /// Routes a default job over `pool` (in order), leaving out `exclude`.
    fn routed_to(pool: &[&Fake], exclude: &[String]) -> Option<String> {
        with_cx(|cx| {
            let offers: Vec<Vec<Offered>> = pool.iter().map(|f| f.offers()).collect();
            let candidates: Vec<Candidate> = pool
                .iter()
                .zip(&offers)
                .enumerate()
                .map(|(i, (f, o))| Candidate {
                    name: f.name(),
                    rank: i as u32,
                    offers: o,
                    availability: ready(f.available(cx)).unwrap(),
                })
                .collect();
            let needs = Needs::default();
            let ask = route::Ask {
                needs: &needs,
                timeout_min: 30,
                pin: None,
                exclude,
            };
            route::route(&ask, &candidates)
                .ok()
                .and_then(|r| route::choose(&r, false).map(|p| p.connector.clone()))
        })
    }

    fn dispatch(f: &Fake, attempt: u32) -> Outcome {
        with_cx(|cx| {
            let r = run(attempt);
            let result = ready(f.dispatch(cx, &r)).map_err(|e| e.to_string());
            after_dispatch(attempt, f.name(), result)
        })
    }

    #[test]
    fn a_job_goes_to_the_first_connector_and_is_dispatched() {
        let a = Fake::new("a", vec![ok("run")]);
        let b = Fake::new("b", vec![]);
        assert_eq!(routed_to(&[&a, &b], &[]).as_deref(), Some("a"));
        assert_eq!(
            dispatch(&a, 1),
            Outcome::Dispatched(Dispatched {
                handle: "run-1".into(),
                url: None
            })
        );
    }

    #[test]
    fn a_refused_dispatch_retries_once_on_another_connector_then_errors() {
        let a = Fake::new("a", vec![Err("no capacity".into())]);
        let b = Fake::new("b", vec![Err("lost".into())]);
        let Outcome::Requeue { exclude, why } = dispatch(&a, 1) else {
            panic!("a first refusal is retried");
        };
        assert_eq!(exclude, "a");
        assert!(why.contains("no capacity"));
        let next = routed_to(&[&a, &b], &[exclude]);
        assert_eq!(next.as_deref(), Some("b"));
        assert_eq!(
            dispatch(&b, 2),
            Outcome::Fail {
                why: "b: lost".into()
            }
        );
        assert_eq!(routed_to(&[&a, &b], &["a".into(), "b".into()]), None);
    }

    #[test]
    fn a_full_first_connector_holds_the_job_until_the_fallback() {
        let mut a = Fake::new("a", vec![]);
        a.availability.free = Some(0);
        let b = Fake::new("b", vec![ok("x")]);
        assert_eq!(routed_to(&[&a, &b], &[]), None);
        with_cx(|cx| {
            let offers = [a.offers(), b.offers()];
            let candidates = [
                Candidate {
                    name: "a",
                    rank: 0,
                    offers: &offers[0],
                    availability: ready(a.available(cx)).unwrap(),
                },
                Candidate {
                    name: "b",
                    rank: 1,
                    offers: &offers[1],
                    availability: ready(b.available(cx)).unwrap(),
                },
            ];
            let needs = Needs::default();
            let ask = route::Ask {
                needs: &needs,
                timeout_min: 30,
                pin: None,
                exclude: &[],
            };
            let r = route::route(&ask, &candidates).unwrap();
            assert!(route::fallback_due(0, 300_000, 300_000));
            assert_eq!(route::choose(&r, true).unwrap().connector, "b");
        });
    }

    #[test]
    fn a_webhook_is_verified_before_it_is_read_and_a_failure_is_final() {
        let a = Fake::new("a", vec![]);
        let body = br#"{"handle":"run-1","passed":false}"#.to_vec();
        let hook = |token: &str| Hook {
            headers: vec![("x-fake-token".into(), token.into())],
            body: body.clone(),
        };
        with_cx(|cx| {
            assert!(ready(a.webhook(cx, &hook("wrong"))).is_err());
            let unsigned = Hook {
                headers: vec![],
                body: b"not even json".to_vec(),
            };
            assert!(ready(a.webhook(cx, &unsigned)).is_err());
            let seen = ready(a.webhook(cx, &hook("hook-secret"))).unwrap().unwrap();
            assert_eq!(seen.handle, "run-1");
            assert_eq!(
                state::on_report(JobState::Running, 1, &seen.report),
                Next::Move(JobState::Failed)
            );
        });
    }

    fn finished(name: &str, state: &str, at: i64, exit: i64) -> Row {
        Row {
            id: format!("{name}-{at}"),
            repo: "ns/r".into(),
            commit_sha: "a".repeat(40),
            name: name.into(),
            job: "{}".into(),
            class: "linux-amd64-small".into(),
            pin: None,
            task: None,
            caller: None,
            caller_name: None,
            notify_url: None,
            notify_key: None,
            state: state.into(),
            attempt: 1,
            connector: Some("containers".into()),
            handle: None,
            url: None,
            excluded: "[]".into(),
            token_expires_at: None,
            deadline_at: None,
            claimed_at: None,
            created_at: 0,
            queued_at: 0,
            dispatched_at: Some(at - 200),
            started_at: Some(at - 100),
            finished_at: Some(at),
            detail: None,
            exit_code: Some(exit),
            output: None,
        }
    }

    #[test]
    fn two_jobs_finishing_together_on_one_commit_each_keep_their_line() {
        // smoke finishes and its push goes out; smoke-fail finishes 7 s later on that commit.
        let first = notes::text(&note_lines(vec![finished("smoke", "succeeded", 1_000, 0)]));
        let second = notes::text(&note_lines(vec![
            finished("smoke", "succeeded", 1_000, 0),
            finished("smoke-fail", "failed", 8_000, 1),
        ]));
        assert_eq!(first.lines().count(), 1);
        assert_eq!(second.lines().count(), 2);
        assert!(
            second.contains("smoke succeeded connector=containers attempt=1 exit=0 duration=100ms")
        );
        assert!(
            second
                .contains("smoke-fail failed connector=containers attempt=1 exit=1 duration=100ms")
        );
        // The first push marks only the text it pushed, so the rewritten note stays unpushed
        // and the queued push goes again instead of being dropped.
        assert_ne!(first, second);
        assert_eq!(after_push(1), AfterPush::Again);
        assert_eq!(after_push(0), AfterPush::Done);
    }

    #[test]
    fn a_rerun_replaces_its_own_line() {
        let lines = note_lines(vec![
            finished("smoke", "failed", 1_000, 1),
            finished("smoke", "succeeded", 2_000, 0),
        ]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].state, "succeeded");
    }

    #[test]
    fn the_gateways_limit_caps_free_slots() {
        let a = Availability {
            configured: true,
            healthy: true,
            free: None,
            queued: 0,
        };
        assert_eq!(with_limit(a, None, 9).free, None);
        assert_eq!(with_limit(a, Some(4), 1).free, Some(3));
        assert_eq!(with_limit(a, Some(4), 6).free, Some(0));
        let some = Availability { free: Some(2), ..a };
        assert_eq!(with_limit(some, Some(4), 1).free, Some(2));
    }

    #[test]
    fn a_deadline_allows_the_timeout_and_ten_minutes() {
        assert_eq!(deadline(1_000, 30), 1_000 + 40 * 60_000);
    }

    #[test]
    fn the_job_token_is_never_serialised_with_a_run() {
        let mut r = run(1);
        r.token = Some("pgn_ci_secret".into());
        let v = serde_json::to_string(&r).unwrap();
        assert!(!v.contains("pgn_ci_secret"));
    }
}

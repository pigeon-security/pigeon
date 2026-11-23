//! The runs' Durable Object class, `AiAgentRuns`, in two roles, as `ModelJobs` is. Object `index`
//! keeps a row per run (what the tools, the console and the parent read), the slots runs take,
//! and the pause flag. Each run lives in its own object, `run:{id}`: its messages, its tool calls
//! and where it is. Its alarm does one step at a time, as [`run::next`] decides, and sets the
//! next alarm; every step is recorded before the next, so a deploy or an eviction costs a step
//! at most.
//!
//! A run reaches pigeon only as its caller (the person's delegate, with the run's id), through
//! core's `dispatch` over the `PLATFORM` binding: model turns are `models_submit` jobs in the
//! `ai-agents` lane, CI and the policy are pigeon tools, and every call passes pigeon's gate. Its
//! sandbox is a `Sandbox` object of its own ([`crate::sandbox`]).

use pigeon_contract::Answer;
use pigeon_core::durable::{self, Db, opt};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result, SqlStorageValue, State as ObjectState};

use crate::budget::{self, Budget, Next, Spent};
use crate::run::{self, Action, Call, Job, Recovery, State, View, Wait};
use crate::tools::{self, Kind};
use crate::transcript::{self, Spec};
use crate::{Role, prompts, sandbox};

/// The class's binding.
pub const BINDING: &str = "AI_AGENT_RUNS";
/// The one index object.
pub const INDEX: &str = "index";
/// The Secrets Store binding of the key callers are signed with.
pub const CALLER_KEY: &str = "CALLER_KEY";
/// The service binding to core.
pub const PLATFORM: &str = "PLATFORM";

/// How many gateway runs may be live at once (preparing, running or waiting), of every role
/// together: the one ceiling on runs and so on sandboxes, one per run. There is no per-role cap;
/// a planner's executors are bounded by its `executors` budget, and `ai_agents_pause` and
/// `ai_agents_stop` stay the kill switch.
pub const MAX_LIVE: u32 = 32;
/// How many live slots a planner may not take: planners start only while more than this many
/// are free, so executors always have room and a planner never waits forever on its own.
pub const EXECUTOR_RESERVE: u32 = 8;
/// How many runs may wait, queued, beyond the live ones.
pub const MAX_QUEUED: u32 = 20;

/// Each object's schema, one statement per migration, applied in order and never edited. The
/// index uses `runs` and `flags`; a run object uses `run`, `messages` and `calls`.
const MIGRATIONS: [&str; 9] = [
    "CREATE TABLE runs (id TEXT PRIMARY KEY, role TEXT NOT NULL, runtime TEXT NOT NULL, state TEXT NOT NULL, parent_run TEXT, session_id TEXT, caller TEXT NOT NULL, by_id TEXT, for_id TEXT, brief TEXT NOT NULL, base TEXT, branch TEXT, budget TEXT NOT NULL, spent TEXT NOT NULL, step TEXT, waiting TEXT, report TEXT, created_at INTEGER NOT NULL, started_at INTEGER, ended_at INTEGER, stop_asked INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE flags (k TEXT PRIMARY KEY, v TEXT NOT NULL)",
    "CREATE TABLE run (id TEXT PRIMARY KEY, role TEXT NOT NULL, caller TEXT NOT NULL, brief TEXT NOT NULL, base TEXT NOT NULL, branch TEXT NOT NULL, budget TEXT NOT NULL, spent TEXT NOT NULL, state TEXT NOT NULL, started_at INTEGER NOT NULL, model_index INTEGER NOT NULL DEFAULT 0, fallback_left INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0, job_id TEXT, job_kind TEXT, job_state TEXT, job_at INTEGER, job_error TEXT, last_usage TEXT, stop_reason TEXT, reported TEXT, report TEXT, nudges INTEGER NOT NULL DEFAULT 0, waiting TEXT, prepare_tries INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE messages (n INTEGER PRIMARY KEY, body TEXT NOT NULL)",
    "CREATE TABLE calls (n INTEGER PRIMARY KEY, id TEXT NOT NULL, name TEXT NOT NULL, input TEXT NOT NULL, started INTEGER NOT NULL DEFAULT 0, done INTEGER NOT NULL DEFAULT 0, result TEXT, is_error INTEGER NOT NULL DEFAULT 0)",
    "ALTER TABLE runs ADD COLUMN assumed TEXT",
    // ai_agents_extend: who raised a run's budget, by how much, when.
    "ALTER TABLE runs ADD COLUMN extended TEXT",
    // ai_agents_note: notes waiting for the run's next turn.
    "ALTER TABLE run ADD COLUMN notes TEXT",
    // The last turn (report only) before the cost budget runs out has begun.
    "ALTER TABLE run ADD COLUMN last_turn INTEGER NOT NULL DEFAULT 0",
];

fn now() -> i64 {
    worker::js_sys::Date::now() as i64
}

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

fn opt_i(v: Option<i64>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, Into::into)
}

/// Whether a queued run of `role` may take a live slot while `live` runs of every role are
/// live; `oldest` is whether it is the oldest queued run of its role (planners queue among
/// planners, executors of either kind among themselves). An executor needs a slot under
/// [`MAX_LIVE`]; a planner also leaves [`EXECUTOR_RESERVE`] of them free.
pub fn may_start(role: Role, live: u32, oldest: bool) -> bool {
    let cap = match role {
        Role::Planner => MAX_LIVE.saturating_sub(EXECUTOR_RESERVE),
        Role::Executor | Role::Mechanical => MAX_LIVE,
    };
    oldest && live < cap
}

/// Calls `op` on object `name`.
pub async fn call(env: &Env, name: &str, op: &str, args: &Value) -> Result<Value> {
    durable::call(env, BINDING, name, op, args).await
}

/// The key callers are signed with: read in one place, so a per-Worker key drops in later.
pub async fn caller_key(env: &Env) -> Result<String> {
    let binding = env
        .secret_store(CALLER_KEY)
        .map_err(|_| bad("pigeon-ai-agents has no caller key: CALLER_KEY is not bound"))?;
    pigeon_core::secrets_store::read(&binding)
        .await?
        .ok_or_else(|| bad("pigeon-ai-agents has no caller key: CALLER_KEY is not set"))
}

/// Runs pigeon tool `tool` with `args` through core's gate, as `caller` (its JSON). A tool's
/// refusal comes back as `Err` with core's words, which a model can read.
pub async fn dispatch(env: &Env, caller: &Value, tool: &str, args: &Value) -> Result<Value> {
    let key = caller_key(env).await?;
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service(PLATFORM)?;
    match pigeon_contract::ask(
        &platform,
        "https://core",
        "dispatch",
        &json!({ "tool": tool, "args": args }),
        Some((key.as_bytes(), caller)),
        &nonce,
    )
    .await?
    {
        Answer::Ok(v) => Ok(v),
        Answer::Error(f) => Err(bad(f.message)),
    }
}

/// A run's row in the index.
#[derive(Deserialize, Debug)]
struct Row {
    id: String,
    role: String,
    runtime: String,
    state: String,
    parent_run: Option<String>,
    session_id: Option<String>,
    by_id: Option<String>,
    for_id: Option<String>,
    brief: String,
    base: Option<String>,
    branch: Option<String>,
    budget: String,
    spent: String,
    step: Option<String>,
    waiting: Option<String>,
    report: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    stop_asked: i64,
    assumed: Option<String>,
    extended: Option<String>,
}

impl Row {
    /// What `ai_agents_status` shows: never the caller's JSON or the messages.
    fn public(&self) -> Value {
        let parsed = |s: &Option<String>| -> Value {
            s.as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null)
        };
        let spent: Spent = serde_json::from_str(&self.spent).unwrap_or_default();
        let budget: Option<Budget> = serde_json::from_str(&self.budget).ok();
        let brief: String = self.brief.chars().take(300).collect();
        json!({
            "run": self.id,
            "role": self.role,
            "runtime": self.runtime,
            "state": self.state,
            "parent_run": self.parent_run,
            "session_id": self.session_id,
            "brief": brief,
            "base": self.base,
            "branch": self.branch,
            "step": self.step,
            "waiting": parsed(&self.waiting),
            "report": self.report,
            "spent": {
                "turns": spent.turns,
                "cost": budget::money(spent.cost_micros),
                "sandbox_min": spent.sandbox_ms / 60_000,
            },
            "budget": budget.map(|b| json!({
                "turns": b.turns,
                "cost": budget::money(b.cost_micros),
                "wall_min": b.wall_ms / 60_000,
                "sandbox_min": b.sandbox_ms / 60_000,
                "executors": b.executors,
            })),
            "cost_tag": format!("ai-agent/{}/", self.id),
            "created_at": self.created_at,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "stop_asked": self.stop_asked != 0,
            "assumed": parsed(&self.assumed),
            "extended": parsed(&self.extended),
        })
    }
}

/// The questions in `rows` that went unanswered after `since` (ms), each with its run.
fn assumed_since(rows: &[Row], since: i64) -> Vec<Value> {
    rows.iter()
        .flat_map(|r| {
            let list: Vec<Value> = r
                .assumed
                .as_deref()
                .and_then(|v| serde_json::from_str(v).ok())
                .unwrap_or_default();
            list.into_iter()
                .filter(|q| q["at"].as_i64().is_some_and(|t| t > since))
                .map(|q| json!({ "run": r.id, "question": q["question"], "at": q["at"] }))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A run object's one row.
#[derive(Deserialize, Debug, Clone)]
struct RunRow {
    id: String,
    role: String,
    caller: String,
    base: String,
    branch: String,
    budget: String,
    spent: String,
    state: String,
    started_at: i64,
    model_index: i64,
    fallback_left: i64,
    failures: i64,
    job_id: Option<String>,
    job_kind: Option<String>,
    job_state: Option<String>,
    job_at: Option<i64>,
    job_error: Option<String>,
    last_usage: Option<String>,
    stop_reason: Option<String>,
    reported: Option<String>,
    report: Option<String>,
    nudges: i64,
    waiting: Option<String>,
    prepare_tries: i64,
    notes: Option<String>,
    last_turn: i64,
}

/// One stored tool call.
#[derive(Deserialize, Debug, Clone)]
struct CallRow {
    n: i64,
    id: String,
    name: String,
    input: String,
    started: i64,
    done: i64,
    result: Option<String>,
    is_error: i64,
}

/// The objects' logic; the hosting Worker's Durable Object class wraps it.
pub struct AiAgentRuns {
    db: Db,
    state: ObjectState,
    env: Env,
}

impl AiAgentRuns {
    /// The objects' logic, its migrations applied.
    pub fn new(state: ObjectState, env: Env) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self { db, state, env })
    }

    /// Answers one operation from the service or another object.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    /// The alarm: a run object takes its next step.
    pub async fn alarm(&self) -> Result<Response> {
        if let Some(row) = self.run_row()?
            && let Err(e) = self.step(row).await
        {
            worker::console_error!("agents: a step failed: {e}");
            // Try again soon; a step that keeps failing is bounded by the run's wall time.
            self.state.storage().set_alarm(30_000).await?;
        }
        Response::ok("ok")
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default().to_string();
        match op {
            // ---- the index ----
            "create" => self.create(a).await,
            "list" => self.list(a),
            "section" => Ok(json!({
                "paused": self.flag("paused")?,
                "runs": self.section_rows(&s("reader"))?,
            })),
            "get" => self.get(&s("run"), &s("reader")),
            // For a run object, about itself: no reader check.
            "get_any" => Ok(self
                .row(&s("run"))?
                .map(|r| r.public())
                .unwrap_or(Value::Null)),
            "update" => self.update(a),
            "slot" => self.slot(&s("run")),
            "inbox" => self.inbox(&s("reader"), a["since"].as_i64().unwrap_or(0)),
            "relay_answer" => {
                let id = s("run");
                self.get(&id, &s("reader"))?;
                call(
                    &self.env,
                    &format!("run:{id}"),
                    "answer",
                    &json!({ "answer": a["answer"] }),
                )
                .await
            }
            "report" => self.report(a),
            "extend" => self.extend(a).await,
            "note" => {
                let id = s("run");
                let row = self
                    .row(&id)?
                    .filter(|r| Self::may_read(r, &s("reader")))
                    .ok_or_else(|| bad(format!("no run {id}, or not yours")))?;
                if row.ended_at.is_some() || row.runtime != "gateway" {
                    return Err(bad(format!("run {id} is not a live gateway run")));
                }
                call(
                    &self.env,
                    &format!("run:{id}"),
                    "queue_note",
                    &json!({ "text": a["text"] }),
                )
                .await
            }
            "assume" => {
                let id = s("run");
                let row = self.row(&id)?.ok_or_else(|| bad("no such run"))?;
                let mut list: Vec<Value> = row
                    .assumed
                    .as_deref()
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or_default();
                list.push(json!({ "question": a["question"], "at": now() }));
                self.db.exec(
                    "UPDATE runs SET assumed = ? WHERE id = ?",
                    vec![Value::Array(list).to_string().into(), id.as_str().into()],
                )?;
                Ok(Value::Null)
            }
            "handoff" => {
                let id = s("run");
                let row = self
                    .row(&id)?
                    .filter(|r| Self::may_read(r, &s("reader")))
                    .ok_or_else(|| bad(format!("no run {id}, or not yours")))?;
                if row.runtime != "gateway" {
                    return Err(bad("only a gateway run is handed to the Claude chat"));
                }
                let live = row.ended_at.is_none();
                if live {
                    self.db.exec(
                        "UPDATE runs SET stop_asked = 1, report = COALESCE(report, 'handed to the Claude chat') WHERE id = ?",
                        vec![id.as_str().into()],
                    )?;
                    call(&self.env, &format!("run:{id}"), "halt", &json!({})).await?;
                }
                Ok(handoff_packet(
                    &row.public(),
                    &row.brief,
                    row.base.as_deref(),
                ))
            }
            "children" => {
                let rows: Vec<Row> = self.db.rows(
                    "SELECT * FROM runs WHERE parent_run = ? ORDER BY created_at",
                    vec![s("run").into()],
                )?;
                Ok(json!(rows.iter().map(Row::public).collect::<Vec<_>>()))
            }
            "stop_child" => {
                let id = s("run");
                let row = self
                    .row(&id)?
                    .filter(|r| r.parent_run.as_deref() == Some(s("parent").as_str()))
                    .ok_or_else(|| bad(format!("no executor {id} of yours")))?;
                self.db.exec(
                    "UPDATE runs SET stop_asked = 1 WHERE id = ?",
                    vec![row.id.as_str().into()],
                )?;
                call(&self.env, &format!("run:{id}"), "halt", &json!({})).await?;
                Ok(json!({ "stopping": [id] }))
            }
            "stop" => self.stop(a).await,
            "pause" => self.set_flag("paused", a["paused"].as_bool().unwrap_or(true)),
            "paused" => Ok(json!(self.flag("paused")?)),
            // ---- a run object ----
            "begin" => self.begin(a).await,
            "answer" => {
                let row = self.run_row()?.ok_or_else(|| bad("no run here"))?;
                let waiting: Option<Wait> = row
                    .waiting
                    .as_deref()
                    .and_then(|w| serde_json::from_str(w).ok());
                let Some(Wait::Answer { call: call_id, .. }) = waiting else {
                    return Err(bad(
                        "the run is not waiting for an answer: after 5 minutes it went on with \
                         the conventional choice (listed under `assumed` in ai_agents_inbox and \
                         in its report); stop it or start a new run if that choice is wrong",
                    ));
                };
                let text = s("answer");
                if text.trim().is_empty() {
                    return Err(bad("`answer` is required"));
                }
                self.answered(&row, &call_id, &format!("The person answered: {text}"))
                    .await?;
                Ok(json!({ "answered": true }))
            }
            "set_budget" => {
                self.set("budget = ?", vec![a["budget"].to_string().into()])?;
                Ok(Value::Null)
            }
            "queue_note" => {
                let row = self.run_row()?.ok_or_else(|| bad("no run here"))?;
                let mut notes: Vec<String> = row
                    .notes
                    .as_deref()
                    .and_then(|n| serde_json::from_str(n).ok())
                    .unwrap_or_default();
                notes.push(s("text"));
                self.set("notes = ?", vec![json!(notes).to_string().into()])?;
                Ok(json!({ "queued": notes.len() }))
            }
            "halt" => {
                self.db.exec("UPDATE run SET waiting = NULL", vec![])?;
                self.state.storage().set_alarm(0).await?;
                Ok(Value::Null)
            }
            other => Err(bad(format!("no operation `{other}`"))),
        }
    }

    // ---- the index ----

    fn flag(&self, k: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct F {
            v: String,
        }
        Ok(self
            .db
            .row::<F>("SELECT v FROM flags WHERE k = ?", vec![k.into()])?
            .is_some_and(|f| f.v == "1"))
    }

    fn set_flag(&self, k: &str, on: bool) -> Result<Value> {
        self.db.exec(
            "INSERT INTO flags (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v",
            vec![k.into(), if on { "1" } else { "0" }.into()],
        )?;
        Ok(json!({ k: on }))
    }

    fn row(&self, id: &str) -> Result<Option<Row>> {
        self.db
            .row("SELECT * FROM runs WHERE id = ?", vec![id.into()])
    }

    fn count(&self, states: &str, roles: &str) -> Result<u32> {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n: Option<N> = self.db.row(
            &format!(
                "SELECT COUNT(*) AS n FROM runs WHERE runtime = 'gateway' AND state IN {states} AND role IN {roles}"
            ),
            vec![],
        )?;
        Ok(n.map_or(0, |n| n.n.max(0) as u32))
    }

    /// Records a new run, queued, and starts its object.
    async fn create(&self, a: &Value) -> Result<Value> {
        let queued = self.count("('queued')", "('planner', 'executor', 'mechanical')")?;
        if queued >= MAX_QUEUED {
            return Err(bad(format!(
                "{queued} runs are already queued, the most there may be; try again later"
            )));
        }
        let t = now();
        let id = a["run"].as_str().ok_or_else(|| bad("`run` is required"))?;
        let runtime = a["runtime"].as_str().unwrap_or("gateway");
        self.db.exec(
            "INSERT INTO runs (id, role, runtime, state, parent_run, session_id, caller, by_id, for_id, brief, base, branch, budget, spent, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                id.into(),
                a["role"].as_str().unwrap_or("executor").into(),
                runtime.into(),
                if runtime == "gateway" { "queued" } else { "running" }.into(),
                opt(a["parent_run"].as_str()),
                opt(a["session_id"].as_str()),
                a["caller"].to_string().into(),
                opt(a["caller"]["user"]["id"].as_str()),
                opt(a["caller"]["acting_for"]["id"].as_str()),
                a["brief"].as_str().unwrap_or_default().into(),
                opt(a["base"].as_str()),
                opt(a["branch"].as_str()),
                a["budget"].to_string().into(),
                serde_json::to_string(&Spent::default())?.into(),
                t.into(),
            ],
        )?;
        if runtime == "gateway" {
            call(&self.env, &format!("run:{id}"), "begin", a).await?;
        }
        Ok(self.row(id)?.map(|r| r.public()).unwrap_or(Value::Null))
    }

    /// What waits for the person, and what ended since `since` (ms): open questions and passkey
    /// waits, and finished runs with their reports, for the parent session to relay.
    fn inbox(&self, reader: &str, since: i64) -> Result<Value> {
        let waiting: Vec<Row> = self.db.rows(
            "SELECT * FROM runs WHERE state = 'waiting' AND waiting IS NOT NULL AND (by_id = ? OR for_id = ?) ORDER BY created_at",
            vec![reader.into(), reader.into()],
        )?;
        let ended: Vec<Row> = self.db.rows(
            "SELECT * FROM runs WHERE ended_at IS NOT NULL AND ended_at > ? AND (by_id = ? OR for_id = ?) ORDER BY ended_at LIMIT 50",
            vec![since.into(), reader.into(), reader.into()],
        )?;
        let cursor = ended
            .iter()
            .filter_map(|r| r.ended_at)
            .max()
            .unwrap_or(since);
        // Questions that went unanswered since the cursor: what each run assumed instead.
        let with_assumed: Vec<Row> = self.db.rows(
            "SELECT * FROM runs WHERE assumed IS NOT NULL AND (by_id = ? OR for_id = ?) ORDER BY created_at DESC LIMIT 50",
            vec![reader.into(), reader.into()],
        )?;
        let assumed = assumed_since(&with_assumed, since);
        let cursor = assumed
            .iter()
            .filter_map(|q| q["at"].as_i64())
            .fold(cursor, i64::max);
        Ok(json!({
            "assumed": assumed,
            "waiting": waiting.iter().map(Row::public).collect::<Vec<_>>(),
            "ended": ended.iter().map(Row::public).collect::<Vec<_>>(),
            "next_since": cursor,
        }))
    }

    /// A Claude Code session registered as a run reports how it ended.
    fn report(&self, a: &Value) -> Result<Value> {
        let id = a["run"].as_str().unwrap_or_default();
        let reader = a["reader"].as_str().unwrap_or_default();
        let row = self
            .row(id)?
            .filter(|r| Self::may_read(r, reader))
            .ok_or_else(|| bad(format!("no run {id}, or not yours")))?;
        if row.runtime != "claude_code" {
            return Err(bad(
                "a gateway run reports by itself; ai_agents_report is for Claude Code sessions",
            ));
        }
        let state = match a["state"].as_str() {
            Some(s @ ("running" | "done" | "failed")) => s,
            _ => return Err(bad("`state` must be running, done or failed")),
        };
        let ended = state != "running";
        self.db.exec(
            "UPDATE runs SET state = ?, report = COALESCE(?, report), branch = COALESCE(?, branch), step = COALESCE(?, step), ended_at = CASE WHEN ? THEN ? ELSE ended_at END WHERE id = ?",
            vec![
                state.into(),
                opt(a["report"].as_str()),
                opt(a["branch"].as_str()),
                opt(a["step"].as_str()),
                i64::from(ended).into(),
                now().into(),
                id.into(),
            ],
        )?;
        Ok(self.row(id)?.map(|r| r.public()).unwrap_or(Value::Null))
    }

    /// The runs for the console's AI Agents section: live ones, then the last ended.
    pub fn section_rows(&self, reader: &str) -> Result<Vec<Value>> {
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM runs WHERE by_id = ? OR for_id = ? ORDER BY CASE WHEN ended_at IS NULL THEN 0 ELSE 1 END, created_at DESC LIMIT 30",
            vec![reader.into(), reader.into()],
        )?;
        Ok(rows.iter().map(Row::public).collect())
    }

    /// Whether `reader` (a user id) may see or act on a run: the caller who started it, or the
    /// person it acts for.
    fn may_read(row: &Row, reader: &str) -> bool {
        !reader.is_empty()
            && (row.by_id.as_deref() == Some(reader) || row.for_id.as_deref() == Some(reader))
    }

    fn get(&self, id: &str, reader: &str) -> Result<Value> {
        self.row(id)?
            .filter(|r| Self::may_read(r, reader))
            .map(|r| r.public())
            .ok_or_else(|| bad(format!("no run {id}, or not yours")))
    }

    fn list(&self, a: &Value) -> Result<Value> {
        let reader = a["reader"].as_str().unwrap_or_default();
        let limit = a["limit"].as_i64().unwrap_or(20).clamp(1, 100);
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM runs WHERE by_id = ? OR for_id = ? ORDER BY created_at DESC LIMIT ?",
            vec![reader.into(), reader.into(), limit.into()],
        )?;
        Ok(json!({
            "paused": self.flag("paused")?,
            "runs": rows.iter().map(Row::public).collect::<Vec<_>>(),
        }))
    }

    /// `ai_agents_extend`: raises a live run's budget, in its object and here, and keeps who
    /// asked and by how much in the run's `extended` list.
    async fn extend(&self, a: &Value) -> Result<Value> {
        let id = a["run"].as_str().unwrap_or_default();
        let reader = a["reader"].as_str().unwrap_or_default();
        let row = self
            .row(id)?
            .filter(|r| Self::may_read(r, reader))
            .ok_or_else(|| bad(format!("no run {id}, or not yours")))?;
        if row.ended_at.is_some() || row.runtime != "gateway" {
            return Err(bad(format!("run {id} is not a live gateway run")));
        }
        let role = role_named(&row.role);
        let current: Budget =
            serde_json::from_str(&row.budget).unwrap_or_else(|_| Budget::default_for(role));
        let (next, added) = budget::extend(role, &current, &a["add"]).map_err(bad)?;
        call(
            &self.env,
            &format!("run:{id}"),
            "set_budget",
            &json!({ "budget": next }),
        )
        .await?;
        let mut list: Vec<Value> = row
            .extended
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
            .unwrap_or_default();
        list.push(json!({ "by": reader, "added": added, "at": now() }));
        self.db.exec(
            "UPDATE runs SET budget = ?, extended = ? WHERE id = ?",
            vec![
                serde_json::to_string(&next)?.into(),
                Value::Array(list).to_string().into(),
                id.into(),
            ],
        )?;
        worker::console_log!("agents: run {id}'s budget extended by {reader}: {added}");
        self.get(id, reader)
    }

    /// A run object reports where it is.
    fn update(&self, a: &Value) -> Result<Value> {
        let id = a["run"].as_str().unwrap_or_default();
        let ended = a["state"]
            .as_str()
            .and_then(State::named)
            .is_some_and(State::terminal);
        self.db.exec(
            "UPDATE runs SET state = COALESCE(?, state), step = ?, spent = COALESCE(?, spent), waiting = ?, report = COALESCE(?, report), branch = COALESCE(?, branch), started_at = COALESCE(started_at, ?), ended_at = CASE WHEN ? THEN ? ELSE ended_at END WHERE id = ?",
            vec![
                opt(a["state"].as_str()),
                opt(a["step"].as_str()),
                opt(a.get("spent").filter(|s| s.is_object()).map(Value::to_string).as_deref()),
                opt(a.get("waiting").filter(|w| w.is_object()).map(Value::to_string).as_deref()),
                opt(a["report"].as_str()),
                opt(a["branch"].as_str()),
                opt_i(a["started_at"].as_i64()),
                i64::from(ended).into(),
                now().into(),
                id.into(),
            ],
        )?;
        Ok(Value::Null)
    }

    /// Whether run `id` may take a live slot now: it already holds one, or it is the oldest
    /// queued run of its role, there is room under [`MAX_LIVE`] ([`may_start`]) and the runs are
    /// not paused.
    fn slot(&self, id: &str) -> Result<Value> {
        let Some(row) = self.row(id)? else {
            return Ok(json!({ "ok": false, "why": "no such run" }));
        };
        if row.state != "queued" {
            return Ok(json!({ "ok": true }));
        }
        if self.flag("paused")? {
            return Ok(json!({ "ok": false, "why": "every run is paused" }));
        }
        let planner = row.role == "planner";
        let roles = if planner {
            "('planner')"
        } else {
            "('executor', 'mechanical')"
        };
        let live = self.count(
            "('preparing', 'running', 'waiting')",
            "('planner', 'executor', 'mechanical')",
        )?;
        // The oldest queued run of the role goes first.
        #[derive(Deserialize)]
        struct First {
            id: String,
        }
        let first: Option<First> = self.db.row(
            &format!(
                "SELECT id FROM runs WHERE runtime = 'gateway' AND state = 'queued' AND role IN {roles} ORDER BY created_at, id LIMIT 1"
            ),
            vec![],
        )?;
        let role = if planner {
            Role::Planner
        } else {
            Role::Executor
        };
        let ok = may_start(role, live, first.is_some_and(|f| f.id == id));
        if ok {
            self.db.exec(
                "UPDATE runs SET state = 'preparing' WHERE id = ? AND state = 'queued'",
                vec![id.into()],
            )?;
        }
        Ok(json!({ "ok": ok, "why": if ok { "" } else { "waiting for a free slot" } }))
    }

    /// Asks run `run` (or every live run, with `all`) to stop; a planner's executors stop too.
    async fn stop(&self, a: &Value) -> Result<Value> {
        let reader = a["reader"].as_str().unwrap_or_default();
        let ids: Vec<String> = if a["all"].as_bool() == Some(true) {
            #[derive(Deserialize)]
            struct Id {
                id: String,
            }
            self.db
                .rows::<Id>(
                    "SELECT id FROM runs WHERE state IN ('queued', 'preparing', 'running', 'waiting') AND (by_id = ? OR for_id = ?)",
                    vec![reader.into(), reader.into()],
                )?
                .into_iter()
                .map(|r| r.id)
                .collect()
        } else {
            let id = a["run"].as_str().unwrap_or_default();
            let row = self
                .row(id)?
                .filter(|r| Self::may_read(r, reader))
                .ok_or_else(|| bad(format!("no run {id}, or not yours")))?;
            let mut ids = vec![row.id];
            #[derive(Deserialize)]
            struct Id {
                id: String,
            }
            ids.extend(
                self.db
                    .rows::<Id>(
                        "SELECT id FROM runs WHERE parent_run = ? AND state IN ('queued', 'preparing', 'running', 'waiting')",
                        vec![id.into()],
                    )?
                    .into_iter()
                    .map(|r| r.id),
            );
            ids
        };
        for id in &ids {
            self.db.exec(
                "UPDATE runs SET stop_asked = 1 WHERE id = ?",
                vec![id.as_str().into()],
            )?;
            // The run object stops at its next step, which this brings forward.
            if let Err(e) = call(&self.env, &format!("run:{id}"), "halt", &json!({})).await {
                worker::console_error!("agents: halting run {id}: {e}");
            }
        }
        Ok(json!({ "stopping": ids }))
    }

    // ---- a run object ----

    fn run_row(&self) -> Result<Option<RunRow>> {
        self.db.row("SELECT * FROM run LIMIT 1", vec![])
    }

    /// Takes on a new run: its row and its brief as the first message; the first step at once.
    async fn begin(&self, a: &Value) -> Result<Value> {
        if self.run_row()?.is_some() {
            return Ok(Value::Null);
        }
        let id = a["run"].as_str().ok_or_else(|| bad("`run` is required"))?;
        self.db.exec(
            "INSERT INTO run (id, role, caller, brief, base, branch, budget, spent, state, started_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'queued', ?)",
            vec![
                id.into(),
                a["role"].as_str().unwrap_or("executor").into(),
                a["caller"].to_string().into(),
                a["brief"].as_str().unwrap_or_default().into(),
                a["base"].as_str().unwrap_or("main").into(),
                a["branch"].as_str().unwrap_or_default().into(),
                a["budget"].to_string().into(),
                serde_json::to_string(&Spent::default())?.into(),
                now().into(),
            ],
        )?;
        self.push_message(&json!({ "role": "user", "content": prompts::first_message(a) }))?;
        self.state.storage().set_alarm(0).await?;
        Ok(Value::Null)
    }

    fn messages(&self) -> Result<Vec<Value>> {
        #[derive(Deserialize)]
        struct M {
            body: String,
        }
        Ok(self
            .db
            .rows::<M>("SELECT body FROM messages ORDER BY n", vec![])?
            .into_iter()
            .filter_map(|m| serde_json::from_str(&m.body).ok())
            .collect())
    }

    /// Whether the last message is the user's.
    fn user_last(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct M {
            body: String,
        }
        Ok(self
            .db
            .row::<M>("SELECT body FROM messages ORDER BY n DESC LIMIT 1", vec![])?
            .and_then(|m| serde_json::from_str::<Value>(&m.body).ok())
            .is_some_and(|m| m["role"] == "user"))
    }

    /// The text of the model's last message, if it is the model's and has any.
    fn last_text(&self) -> Result<Option<String>> {
        let messages = self.messages()?;
        Ok(messages
            .last()
            .filter(|m| m["role"] == "assistant")
            .and_then(|m| m["content"].as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|t| !t.trim().is_empty()))
    }

    /// Removes the thinking blocks from every stored assistant message, for good: the provider
    /// refused their signatures.
    fn strip_stored_thinking(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct M {
            n: i64,
            body: String,
        }
        let rows = self
            .db
            .rows::<M>("SELECT n, body FROM messages ORDER BY n", vec![])?;
        for m in rows {
            let Ok(body) = serde_json::from_str::<Value>(&m.body) else {
                continue;
            };
            let stripped = transcript::strip_thinking(std::slice::from_ref(&body));
            if let Some(new) = stripped.first().filter(|new| **new != body) {
                self.db.exec(
                    "UPDATE messages SET body = ? WHERE n = ?",
                    vec![new.to_string().into(), m.n.into()],
                )?;
            }
        }
        Ok(())
    }

    /// Puts the notes queued by `ai_agents_note` in the transcript, before a turn.
    fn take_notes(&self) -> Result<()> {
        // Read now, not from the step's row: a note may have come in since.
        let Some(row) = self.run_row()? else {
            return Ok(());
        };
        let notes: Vec<String> = row
            .notes
            .as_deref()
            .and_then(|n| serde_json::from_str(n).ok())
            .unwrap_or_default();
        if notes.is_empty() {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct M {
            n: i64,
            body: String,
        }
        let last = self.db.row::<M>(
            "SELECT n, body FROM messages ORDER BY n DESC LIMIT 1",
            vec![],
        )?;
        let body = last
            .as_ref()
            .and_then(|m| serde_json::from_str::<Value>(&m.body).ok());
        let (message, replaces) = transcript::with_notes(body.as_ref(), &notes);
        match (replaces, last) {
            (true, Some(m)) => self.db.exec(
                "UPDATE messages SET body = ? WHERE n = ?",
                vec![message.to_string().into(), m.n.into()],
            )?,
            _ => self.push_message(&message)?,
        }
        self.set("notes = NULL", vec![])
    }

    /// Adds `text` to the last user message (or a new one), the way notes are added, so the
    /// roles keep alternating.
    fn add_last_turn_text(&self, text: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct M {
            n: i64,
            body: String,
        }
        let last = self.db.row::<M>(
            "SELECT n, body FROM messages ORDER BY n DESC LIMIT 1",
            vec![],
        )?;
        let body = last
            .as_ref()
            .and_then(|m| serde_json::from_str::<Value>(&m.body).ok());
        let (mut message, replaces) = transcript::with_notes(body.as_ref(), &[]);
        if let Some(blocks) = message["content"].as_array_mut() {
            blocks.push(json!({"type": "text", "text": text}));
        }
        match (replaces, last) {
            (true, Some(m)) => self.db.exec(
                "UPDATE messages SET body = ? WHERE n = ?",
                vec![message.to_string().into(), m.n.into()],
            ),
            _ => self.push_message(&message),
        }
    }

    fn push_message(&self, m: &Value) -> Result<()> {
        self.db.exec(
            "INSERT INTO messages (n, body) VALUES ((SELECT COALESCE(MAX(n), 0) + 1 FROM messages), ?)",
            vec![m.to_string().into()],
        )
    }

    fn calls(&self) -> Result<Vec<CallRow>> {
        self.db.rows("SELECT * FROM calls ORDER BY n", vec![])
    }

    fn set(&self, assignments: &str, args: Vec<SqlStorageValue>) -> Result<()> {
        self.db.exec(&format!("UPDATE run SET {assignments}"), args)
    }

    /// Tells the index where the run is.
    async fn tell(&self, row: &RunRow, step: &str, extra: Value) -> Result<()> {
        let mut a = json!({
            "run": row.id,
            "state": row.state,
            "step": step,
            "spent": serde_json::from_str::<Value>(&row.spent).unwrap_or(Value::Null),
            "branch": row.branch,
            "started_at": row.started_at,
        });
        if let (Some(o), Some(e)) = (a.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        call(&self.env, INDEX, "update", &a).await.map(|_| ())
    }

    /// One step of the run, then the next alarm.
    async fn step(&self, row: RunRow) -> Result<()> {
        let state = State::named(&row.state).unwrap_or(State::Failed);
        if state.terminal() {
            return Ok(());
        }
        let role = role_named(&row.role);
        let calls = self.calls()?;
        let view_calls: Vec<Call> = calls
            .iter()
            .map(|c| Call {
                id: c.id.clone(),
                name: c.name.clone(),
                started: c.started != 0,
                done: c.done != 0,
            })
            .collect();
        let waiting: Option<Wait> = row
            .waiting
            .as_deref()
            .and_then(|w| serde_json::from_str(w).ok());
        let spent: Spent = serde_json::from_str(&row.spent).unwrap_or_default();
        let index_stop = self.index_stop(&row.id).await;
        let paused = call(&self.env, INDEX, "paused", &json!({}))
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let job = row.job_id.as_ref().map(|_| {
            let j = match row.job_state.as_deref() {
                Some("succeeded") => Job::Succeeded,
                Some(s) if s != "queued" && s != "running" => Job::Ended,
                _ => Job::Pending,
            };
            (j, now() - row.job_at.unwrap_or_else(now))
        });
        let view = View {
            state,
            stop_asked: index_stop,
            paused,
            job,
            calls: &view_calls,
            turns: spent.turns,
            stop_reason: row.stop_reason.as_deref(),
            reported: row.reported.as_deref().and_then(State::named),
            nudges: row.nudges as u32,
            user_last: self.user_last()?,
            last_turn: row.last_turn != 0,
            waiting: waiting.as_ref(),
        };
        let action = run::next(&view);
        let budget: Budget =
            serde_json::from_str(&row.budget).unwrap_or_else(|_| Budget::default_for(role));
        // The wall clock applies to every step.
        if !matches!(action, Action::Idle | Action::End(_))
            && let Err(over) = budget::check(&budget, &spent, row.started_at, now(), Next::Tool)
        {
            return self.end(&row, State::OverBudget, &over.why(&budget)).await;
        }
        let storage = self.state.storage();
        match action {
            Action::Idle => Ok(()),
            Action::End(to) => {
                let why = match to {
                    State::Stopped => "stopped by the person or its planner".to_string(),
                    State::Failed if row.report.is_none() => {
                        "the model stopped twice without reporting".to_string()
                    }
                    State::OverBudget if row.last_turn != 0 && row.report.is_none() => {
                        self.last_text()?.unwrap_or_else(|| {
                            "the run spent 90% of its cost budget and its last turn did not \
                             report"
                                .to_string()
                        })
                    }
                    _ => row.report.clone().unwrap_or_default(),
                };
                self.end(&row, to, &why).await
            }
            Action::Check { after_ms } => match &waiting {
                Some(Wait::Approval { id, call: call_id }) => {
                    if !self.check_approval(&row, id, call_id).await? {
                        storage.set_alarm(after_ms).await?;
                    }
                    Ok(())
                }
                Some(Wait::Answer {
                    call: call_id,
                    question,
                    since,
                    ..
                }) if now() - since >= run::ANSWER_PATIENCE_MS => {
                    // The question goes in the inbox as assumed, so the person sees what the
                    // run went on without.
                    call(
                        &self.env,
                        INDEX,
                        "assume",
                        &json!({ "run": row.id, "question": question }),
                    )
                    .await?;
                    self.answered(&row, call_id, &run::unanswered(question))
                        .await
                }
                Some(Wait::Executors { since }) => {
                    let children =
                        call(&self.env, INDEX, "children", &json!({ "run": row.id })).await?;
                    if ended_since(&children, *since) || live_children(&children) == 0 {
                        let summary = children_summary(&children, *since);
                        let waiting_call = self
                            .calls()?
                            .into_iter()
                            .find(|c| c.name == "wait_executors" && c.done == 0);
                        match waiting_call {
                            Some(c) => self.answered(&row, &c.id, &summary).await,
                            None => {
                                self.set("state = 'running', waiting = NULL", vec![])?;
                                storage.set_alarm(0).await
                            }
                        }
                    } else {
                        storage.set_alarm(after_ms).await
                    }
                }
                // The provider's wait is over: back to work, and the next turn tries again.
                Some(Wait::Provider { since }) if now() - since < run::PROVIDER_PATIENCE_MS => {
                    self.set("state = 'running', waiting = NULL", vec![])?;
                    storage.set_alarm(0).await
                }
                Some(Wait::Provider { .. }) => {
                    self.end(&row, State::Failed, "the model provider failed for an hour")
                        .await
                }
                _ => storage.set_alarm(after_ms).await,
            },
            Action::Prepare => self.prepare(&row).await,
            Action::Turn => self.turn(&row, role, &budget, &spent).await,
            Action::Poll { after_ms } => {
                self.poll(&row).await?;
                storage.set_alarm(after_ms).await
            }
            Action::Read => self.read(&row, &spent).await,
            Action::Retry => self.retry(&row, role).await,
            Action::Call { index } => {
                self.call_tool(&row, role, &calls[index], &budget, &spent)
                    .await
            }
            Action::Interrupted { index } => {
                self.finish_call(
                    calls[index].n,
                    "interrupted: the run restarted while this call ran, so its effect is \
                     unknown; check before trying again",
                    true,
                )?;
                storage.set_alarm(0).await
            }
            Action::Nudge => {
                self.push_message(&json!({ "role": "user", "content": prompts::NUDGE }))?;
                self.set("nudges = nudges + 1, stop_reason = NULL", vec![])?;
                storage.set_alarm(0).await
            }
        }
    }

    async fn index_stop(&self, id: &str) -> bool {
        #[derive(Deserialize)]
        struct S {
            stop_asked: bool,
        }
        call(&self.env, INDEX, "get_any", &json!({ "run": id }))
            .await
            .ok()
            .and_then(|v| serde_json::from_value::<S>(v).ok())
            .is_some_and(|s| s.stop_asked)
    }

    /// Ends the run: its sandbox goes, the index learns how it ended.
    async fn end(&self, row: &RunRow, to: State, why: &str) -> Result<()> {
        if let Some(job) = &row.job_id {
            let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
            let _ = dispatch(&self.env, &caller, "models_cancel", &json!({ "id": job })).await;
        }
        if let Err(e) = sandbox::call(&self.env, &row.id, "destroy", &json!({})).await {
            worker::console_error!("agents: destroying run {}'s sandbox: {e}", row.id);
        }
        self.set(
            "state = ?, job_id = NULL, waiting = NULL",
            vec![to.name().into()],
        )?;
        let mut ended = row.clone();
        ended.state = to.name().into();
        let report = row.report.clone().unwrap_or_else(|| why.to_string());
        self.tell(
            &ended,
            &format!("ended: {}", to.name()),
            json!({ "report": report }),
        )
        .await
    }

    /// Takes a live slot, starts the sandbox and checks out the run's branch.
    async fn prepare(&self, row: &RunRow) -> Result<()> {
        let storage = self.state.storage();
        let slot = call(&self.env, INDEX, "slot", &json!({ "run": row.id })).await?;
        if slot["ok"] != true {
            return storage.set_alarm(30_000).await;
        }
        if row.state == "queued" {
            self.set("state = 'preparing'", vec![])?;
        }
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        let prepared = sandbox::call(
            &self.env,
            &row.id,
            "prepare",
            &json!({
                "run": row.id,
                "role": row.role,
                "base": row.base,
                "branch": row.branch,
                "caller": caller,
            }),
        )
        .await;
        match prepared {
            Ok(_) => {
                self.set("state = 'running'", vec![])?;
                let mut r = row.clone();
                r.state = "running".into();
                self.tell(&r, "checked out; thinking", Value::Null).await?;
                storage.set_alarm(0).await
            }
            Err(e) if row.prepare_tries < 3 => {
                worker::console_error!("agents: preparing run {}: {e}", row.id);
                self.set("prepare_tries = prepare_tries + 1", vec![])?;
                storage.set_alarm(30_000).await
            }
            Err(e) => {
                self.end(
                    row,
                    State::Failed,
                    &format!("its sandbox could not start: {e}"),
                )
                .await
            }
        }
    }

    /// Sends the run's next turn as a model job.
    async fn turn(&self, row: &RunRow, role: Role, budget: &Budget, spent: &Spent) -> Result<()> {
        let calls = self.calls()?;
        if !calls.is_empty() {
            // The last turn's calls are all answered: their results go in, in the model's order.
            let results: Vec<(String, String, bool)> = calls
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        c.result.clone().unwrap_or_default(),
                        c.is_error != 0,
                    )
                })
                .collect();
            self.push_message(&transcript::results_message(&results))?;
            self.db.exec("DELETE FROM calls", vec![])?;
        }
        self.take_notes()?;
        let models = role.models();
        let index = (row.model_index.max(0) as usize).min(models.len() - 1);
        let model = models[index];
        let usage: Value = row
            .last_usage
            .as_deref()
            .and_then(|u| serde_json::from_str(u).ok())
            .unwrap_or(Value::Null);
        let estimate = budget::estimate_micros(
            model,
            transcript::input_tokens(&usage).max(20_000),
            transcript::max_tokens(role),
        );
        // The last turn: report only. The 10% left of the cost budget is kept for it.
        let last = row.last_turn != 0 || budget::last_turn_due(budget, spent, estimate);
        if last && row.last_turn == 0 {
            self.add_last_turn_text(&prompts::last_turn(spent.cost_micros, budget.cost_micros))?;
            self.set("last_turn = 1", vec![])?;
        }
        let mut messages = self.messages()?;
        if index > 0 {
            messages = transcript::strip_thinking(&messages);
        }
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        // A long context is summarised first, on a cheap model. Never on the last turn.
        let (request, kind) = match (
            !last && transcript::needs_compaction(&usage),
            transcript::cut_point(&messages, 20),
        ) {
            (true, Some(at)) => (
                transcript::summary_request("claude-haiku-5-5", &messages[..at]),
                "summary",
            ),
            _ => {
                let defs = tools::definitions(role, &prompts::pigeon_tools());
                let system = prompts::system(role, &row.branch);
                let spec = Spec {
                    role,
                    model,
                    system: &system,
                    tools: &defs,
                };
                if last {
                    (transcript::last_turn_request(&spec, &messages), "turn")
                } else {
                    (transcript::request(&spec, &messages), "turn")
                }
            }
        };
        if !last
            && let Err(over) = budget::check(
                budget,
                spent,
                row.started_at,
                now(),
                Next::Turn {
                    estimate_micros: estimate,
                },
            )
        {
            return self.end(row, State::OverBudget, &over.why(budget)).await;
        }
        let job = dispatch(
            &self.env,
            &caller,
            "models_submit",
            &json!({
                "request": request,
                "lane": "ai-agents",
                "timeout_min": 14,
                "task": crate::task(&row.id, role, spent.turns + 1),
            }),
        )
        .await;
        match job {
            Ok(j) => {
                self.set(
                    "job_id = ?, job_kind = ?, job_state = ?, job_at = ?, job_error = NULL",
                    vec![
                        opt(j["id"].as_str()),
                        kind.into(),
                        opt(j["state"].as_str()),
                        now().into(),
                    ],
                )?;
                self.tell(
                    row,
                    if kind == "summary" {
                        "summarising its earlier work"
                    } else {
                        "thinking"
                    },
                    Value::Null,
                )
                .await?;
                self.state.storage().set_alarm(1_000).await
            }
            Err(e) => {
                // The gateway refused the job itself (full, or unreachable): try again later.
                worker::console_error!("agents: run {} could not submit a turn: {e}", row.id);
                self.set(
                    "failures = failures + 1, job_error = ?",
                    vec![e.to_string().into()],
                )?;
                // Shown in ai_agents_status, so a refused turn does not look like thinking.
                let why: String = e.to_string().chars().take(300).collect();
                self.tell(
                    row,
                    &format!("could not submit its turn; retrying in 30 s: {why}"),
                    Value::Null,
                )
                .await?;
                self.state.storage().set_alarm(30_000).await
            }
        }
    }

    async fn poll(&self, row: &RunRow) -> Result<()> {
        let Some(job) = &row.job_id else {
            return Ok(());
        };
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        let status = dispatch(&self.env, &caller, "models_status", &json!({ "id": job })).await?;
        self.set(
            "job_state = ?, job_error = ?",
            vec![opt(status["state"].as_str()), opt(status["error"].as_str())],
        )
    }

    /// Reads the finished turn into the messages and its tool calls into `calls`.
    async fn read(&self, row: &RunRow, spent: &Spent) -> Result<()> {
        let Some(job) = &row.job_id else {
            return Ok(());
        };
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        let result = dispatch(&self.env, &caller, "models_result", &json!({ "id": job })).await?;
        let model = result["model"].as_str().unwrap_or("claude-fable-5-2");
        let usage = result["usage"].clone();
        let mut spent = *spent;
        spent.turns += 1;
        spent.cost_micros = spent
            .cost_micros
            .saturating_add(budget::cost_micros(model, &usage));
        let content = match &result["content"] {
            Value::Array(blocks) if !blocks.is_empty() => Value::Array(blocks.clone()),
            _ => json!([{ "type": "text", "text": result["text"].as_str().unwrap_or("") }]),
        };
        if row.job_kind.as_deref() == Some("summary") {
            let summary = result["text"].as_str().unwrap_or_default();
            let messages = self.messages()?;
            if let Some(at) = transcript::cut_point(&messages, 20) {
                let kept = transcript::compacted(summary, &messages[at..]);
                self.db.exec("DELETE FROM messages", vec![])?;
                for m in &kept {
                    self.push_message(m)?;
                }
            }
            // The next turn's context is the summary and what was kept.
            self.set(
                "job_id = NULL, job_kind = NULL, job_state = NULL, last_usage = NULL, spent = ?",
                vec![serde_json::to_string(&spent)?.into()],
            )?;
        } else {
            self.push_message(&json!({ "role": "assistant", "content": content }))?;
            for (n, (id, name, input)) in transcript::tool_uses(&content).into_iter().enumerate() {
                self.db.exec(
                    "INSERT INTO calls (n, id, name, input) VALUES (?, ?, ?, ?)",
                    vec![
                        (n as i64).into(),
                        id.into(),
                        name.into(),
                        input.to_string().into(),
                    ],
                )?;
            }
            let fallback_left = (row.fallback_left - 1).max(0);
            // Back to the first choice once the fallback has served its turns.
            let model_index = if row.model_index > 0 && fallback_left == 0 {
                0
            } else {
                row.model_index
            };
            self.set(
                "job_id = NULL, job_kind = NULL, job_state = NULL, failures = 0, last_usage = ?, stop_reason = ?, spent = ?, fallback_left = ?, model_index = ?",
                vec![
                    usage.to_string().into(),
                    opt(result["stop_reason"].as_str()),
                    serde_json::to_string(&spent)?.into(),
                    fallback_left.into(),
                    model_index.into(),
                ],
            )?;
        }
        let mut r = row.clone();
        r.spent = serde_json::to_string(&spent)?;
        self.tell(&r, "acting", Value::Null).await?;
        self.state.storage().set_alarm(0).await
    }

    /// The turn's job ended without an answer.
    async fn retry(&self, row: &RunRow, role: Role) -> Result<()> {
        let failures = (row.failures + 1) as u32;
        let recovery = run::on_failure(
            row.job_state.as_deref().unwrap_or("failed"),
            row.job_error.as_deref().unwrap_or(""),
            failures,
            row.model_index.max(0) as usize,
            role.models(),
        );
        let clear = "job_id = NULL, job_kind = NULL, job_state = NULL";
        let storage = self.state.storage();
        match recovery {
            Recovery::Again => {
                self.set(
                    &format!("{clear}, failures = ?"),
                    vec![i64::from(failures).into()],
                )?;
                storage.set_alarm(2_000).await
            }
            Recovery::Fallback { index } => {
                self.set(
                    &format!("{clear}, failures = 0, model_index = ?, fallback_left = ?"),
                    vec![(index as i64).into(), i64::from(run::FALLBACK_TURNS).into()],
                )?;
                storage.set_alarm(0).await
            }
            Recovery::WaitProvider => {
                let wait = Wait::Provider { since: now() };
                self.set(
                    &format!(
                        "{clear}, failures = 0, model_index = 0, state = 'waiting', waiting = ?"
                    ),
                    vec![serde_json::to_string(&wait)?.into()],
                )?;
                let mut r = row.clone();
                r.state = "waiting".into();
                self.tell(
                    &r,
                    "the model provider keeps failing; trying again every 5 minutes (ai_agents_handoff moves it to the Claude chat)",
                    json!({ "waiting": wait }),
                )
                .await?;
                storage.set_alarm(run::PROVIDER_EVERY_MS).await
            }
            Recovery::StripThinking => {
                self.strip_stored_thinking()?;
                self.set(
                    &format!("{clear}, failures = ?"),
                    vec![i64::from(failures).into()],
                )?;
                storage.set_alarm(0).await
            }
            Recovery::Fail => {
                let why = format!(
                    "its model turn failed: {}",
                    row.job_error.as_deref().unwrap_or("no reason given")
                );
                self.end(row, State::Failed, &why).await
            }
        }
    }

    fn finish_call(&self, n: i64, result: &str, is_error: bool) -> Result<()> {
        self.db.exec(
            "UPDATE calls SET done = 1, result = ?, is_error = ? WHERE n = ?",
            vec![
                transcript::cut(result, None).into(),
                i64::from(is_error).into(),
                n.into(),
            ],
        )
    }

    /// Runs one tool call of the last turn.
    async fn call_tool(
        &self,
        row: &RunRow,
        role: Role,
        c: &CallRow,
        budget: &Budget,
        spent: &Spent,
    ) -> Result<()> {
        let storage = self.state.storage();
        let input: Value = serde_json::from_str(&c.input).unwrap_or(Value::Null);
        if let Err(why) = tools::check(role, &c.name, &input) {
            self.finish_call(c.n, &why, true)?;
            return storage.set_alarm(0).await;
        }
        self.db
            .exec("UPDATE calls SET started = 1 WHERE n = ?", vec![c.n.into()])?;
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        self.tell(row, &prompts::step_line(&c.name, &input), Value::Null)
            .await?;
        let outcome: std::result::Result<String, String> = match tools::kind(role, &c.name) {
            Some(Kind::Sandbox) => {
                let timeout_ms = input["timeout_s"].as_i64().unwrap_or(600) * 1_000;
                if c.name == "run"
                    && let Err(over) = budget::check(
                        budget,
                        spent,
                        row.started_at,
                        now(),
                        Next::Sandbox { timeout_ms },
                    )
                {
                    Err(format!(
                        "refused: {}; finish with what you have and report",
                        over.why(budget)
                    ))
                } else {
                    let t0 = now();
                    let answered =
                        sandbox::tool(&self.env, &row.id, &row.branch, &c.name, &input).await;
                    if c.name == "run" {
                        let mut s = *spent;
                        s.sandbox_ms = s.sandbox_ms.saturating_add(now() - t0);
                        self.set("spent = ?", vec![serde_json::to_string(&s)?.into()])?;
                    }
                    answered.map_err(|e| e.to_string())
                }
            }
            Some(Kind::Pigeon) => {
                match dispatch(&self.env, &caller, &c.name, &input).await {
                    Ok(v) => Ok(v.to_string()),
                    Err(e) => {
                        let why = e.to_string();
                        if let Some(id) = approval_id(&why) {
                            // The person's passkey is needed on this exact call: wait for it.
                            let wait = Wait::Approval {
                                id: id.clone(),
                                call: c.id.clone(),
                            };
                            self.set(
                                "state = 'waiting', waiting = ?",
                                vec![serde_json::to_string(&wait)?.into()],
                            )?;
                            let mut r = row.clone();
                            r.state = "waiting".into();
                            self.tell(
                                &r,
                                &format!("waiting for the person's passkey on `{}`", c.name),
                                json!({ "waiting": wait }),
                            )
                            .await?;
                            return storage.set_alarm(run::CHECK_EVERY_MS).await;
                        }
                        Err(why)
                    }
                }
            }
            Some(Kind::Agent) if c.name == "ask_person" => {
                // The run waits for the person's answer, which comes back through `answer`.
                let options: Vec<String> = input["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|o| o.as_str().map(String::from))
                    .collect();
                let wait = Wait::Answer {
                    call: c.id.clone(),
                    question: input["question"].as_str().unwrap_or_default().to_string(),
                    options,
                    since: now(),
                };
                self.set(
                    "state = 'waiting', waiting = ?",
                    vec![serde_json::to_string(&wait)?.into()],
                )?;
                let mut r = row.clone();
                r.state = "waiting".into();
                self.tell(
                    &r,
                    "waiting for the person's answer",
                    json!({ "waiting": wait }),
                )
                .await?;
                return storage.set_alarm(run::CHECK_EVERY_MS).await;
            }
            Some(Kind::Agent) if c.name == "wait_executors" => {
                let children =
                    call(&self.env, INDEX, "children", &json!({ "run": row.id })).await?;
                let live = live_children(&children);
                if live == 0 {
                    Ok(children_summary(&children, 0))
                } else {
                    let wait = Wait::Executors { since: now() };
                    self.set(
                        "state = 'waiting', waiting = ?",
                        vec![serde_json::to_string(&wait)?.into()],
                    )?;
                    let mut r = row.clone();
                    r.state = "waiting".into();
                    self.tell(
                        &r,
                        &format!("waiting for {live} executor(s)"),
                        json!({ "waiting": wait }),
                    )
                    .await?;
                    return storage.set_alarm(run::EXECUTORS_EVERY_MS).await;
                }
            }
            Some(Kind::Agent) if c.name != "report" => {
                self.planner_tool(row, &c.name, &input, budget, spent).await
            }
            Some(Kind::Agent) => self.agent_tool(row, &c.name, &input),
            None => Err(format!("`{}` is not one of this run's tools", c.name)),
        };
        match outcome {
            Ok(text) => self.finish_call(c.n, &text, false)?,
            Err(why) => self.finish_call(c.n, &why, true)?,
        }
        storage.set_alarm(0).await
    }

    /// A planner's tools over its executors.
    async fn planner_tool(
        &self,
        row: &RunRow,
        name: &str,
        input: &Value,
        budget: &Budget,
        spent: &Spent,
    ) -> std::result::Result<String, String> {
        let e = |e: Error| e.to_string();
        let child_id = input["run"].as_str().unwrap_or_default();
        let mine = async |id: &str| -> std::result::Result<Value, String> {
            let child = call(&self.env, INDEX, "get_any", &json!({ "run": id }))
                .await
                .map_err(e)?;
            if child["parent_run"].as_str() == Some(row.id.as_str()) {
                Ok(child)
            } else {
                Err(format!("{id} is not one of your executors"))
            }
        };
        match name {
            "spawn_executor" => {
                let role = if input["kind"] == "mechanical" {
                    Role::Mechanical
                } else {
                    Role::Executor
                };
                let child_budget = Budget::asked(role, &input["budget"])?;
                let left = Spent::left(budget, spent, row.started_at, now());
                child_budget.within(&left)?;
                budget::check(
                    budget,
                    spent,
                    row.started_at,
                    now(),
                    Next::Spawn {
                        cost_micros: child_budget.cost_micros,
                    },
                )
                .map_err(|over| over.why(budget))?;
                let id = crate::service::run_id().map_err(e)?;
                let mut planner_caller: Value =
                    serde_json::from_str(&row.caller).unwrap_or(Value::Null);
                if let Some(o) = planner_caller.as_object_mut() {
                    o.remove("run");
                }
                let caller = run_caller(&self.env, &planner_caller, role, &id).await;
                let branch = crate::service::branch(&id);
                let base = input["base"].as_str().unwrap_or("main");
                call(
                    &self.env,
                    INDEX,
                    "create",
                    &json!({
                        "run": id,
                        "role": role.name(),
                        "runtime": "gateway",
                        "parent_run": row.id,
                        "caller": caller,
                        "brief": input["brief"],
                        "base": base,
                        "branch": branch,
                        "budget": child_budget,
                    }),
                )
                .await
                .map_err(e)?;
                // The child's budget is set aside from the planner's at once.
                let mut s = *spent;
                s.executors += 1;
                s.cost_micros = s.cost_micros.saturating_add(child_budget.cost_micros);
                self.set(
                    "spent = ?",
                    vec![serde_json::to_string(&s).map_err(|x| x.to_string())?.into()],
                )
                .map_err(e)?;
                Ok(format!(
                    "started executor {id} on branch `{branch}` from `{base}`, with a budget of \
                     {} and {} turns; wait_executors waits for it",
                    budget::money(child_budget.cost_micros),
                    child_budget.turns
                ))
            }
            "executor_status" => Ok(mine(child_id).await?.to_string()),
            "executor_result" => {
                let child = mine(child_id).await?;
                if child["ended_at"].is_null() {
                    return Err(format!(
                        "{child_id} is {} and has not ended; wait_executors waits for it",
                        child["state"].as_str().unwrap_or("live")
                    ));
                }
                Ok(json!({
                    "run": child_id,
                    "state": child["state"],
                    "branch": child["branch"],
                    "report": child["report"],
                    "spent": child["spent"],
                })
                .to_string())
            }
            "executor_stop" => {
                mine(child_id).await?;
                call(
                    &self.env,
                    INDEX,
                    "stop_child",
                    &json!({ "run": child_id, "parent": row.id }),
                )
                .await
                .map(|v| v.to_string())
                .map_err(e)
            }
            other => Err(format!("`{other}` is not a planner tool")),
        }
    }

    /// The tools the run handles itself.
    fn agent_tool(
        &self,
        _row: &RunRow,
        name: &str,
        input: &Value,
    ) -> std::result::Result<String, String> {
        match name {
            "report" => {
                let state = if input["state"] == "done" {
                    "done"
                } else {
                    "failed"
                };
                let summary = input["summary"].as_str().unwrap_or_default();
                self.set(
                    "reported = ?, report = ?",
                    vec![state.into(), summary.into()],
                )
                .map_err(|e| e.to_string())?;
                Ok("reported; the run ends".into())
            }
            other => Err(format!("`{other}` is not available to runs yet")),
        }
    }

    /// Answers the `ask_person` call `call_id` with `text` and goes back to work.
    async fn answered(&self, row: &RunRow, call_id: &str, text: &str) -> Result<()> {
        if let Some(c) = self.calls()?.iter().find(|c| c.id == call_id) {
            self.finish_call(c.n, text, false)?;
        }
        self.set("state = 'running', waiting = NULL", vec![])?;
        let mut r = row.clone();
        r.state = "running".into();
        self.tell(&r, "answered; acting", json!({ "waiting": null }))
            .await?;
        self.state.storage().set_alarm(0).await
    }

    /// Looks at the approval a call waits on; once it is decided, the call runs or fails.
    /// Answers whether it was decided.
    async fn check_approval(&self, row: &RunRow, id: &str, call_id: &str) -> Result<bool> {
        let caller: Value = serde_json::from_str(&row.caller).unwrap_or(Value::Null);
        let status = dispatch(&self.env, &caller, "approvals_status", &json!({ "id": id })).await?;
        let state = status["state"].as_str().unwrap_or("pending");
        if state == "pending" {
            return Ok(false);
        }
        let calls = self.calls()?;
        let Some(c) = calls.iter().find(|c| c.id == call_id) else {
            self.set("state = 'running', waiting = NULL", vec![])?;
            self.state.storage().set_alarm(0).await?;
            return Ok(true);
        };
        let outcome = if state == "approved" {
            let mut input: Value = serde_json::from_str(&c.input).unwrap_or(json!({}));
            input["approval"] = json!(id);
            match dispatch(&self.env, &caller, &c.name, &input).await {
                Ok(v) => (v.to_string(), false),
                Err(e) => (e.to_string(), true),
            }
        } else {
            (
                format!("the person's approval {id} was {state}; the call did not run"),
                true,
            )
        };
        self.finish_call(c.n, &outcome.0, outcome.1)?;
        self.set("state = 'running', waiting = NULL", vec![])?;
        let mut r = row.clone();
        r.state = "running".into();
        self.tell(&r, "approval decided; acting", json!({ "waiting": null }))
            .await?;
        self.state.storage().set_alarm(0).await?;
        Ok(true)
    }
}

/// The approval id in a refusal that waits for the person's passkey, as `dispatch` words it:
/// "needs the person's passkey: approval `{id}` …".
pub fn approval_id(why: &str) -> Option<String> {
    let rest = why.split("approval `").nth(1)?;
    let id = rest.split('`').next()?;
    (!id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| id.to_string())
}

/// The role named `s`; an executor for anything else.
pub fn role_named(s: &str) -> Role {
    match s {
        "planner" => Role::Planner,
        "mechanical" => Role::Mechanical,
        _ => Role::Executor,
    }
}

/// The run's git grant for its sandbox: fetch, and push to its own branches only, until its
/// deadline (`docs/plans/ai-agents.md`, decision 3). Asked of core's `credential` op, kind
/// `run_grant`; until core mints them this is an error, and the sandbox has no git credential.
pub async fn grant(env: &Env, caller: &Value, start: &Value) -> Result<String> {
    let key = caller_key(env).await?;
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service(PLATFORM)?;
    let asked = json!({
        "kind": "run_grant",
        "instance": start["run"],
        "scopes": [],
    });
    match pigeon_contract::ask(
        &platform,
        "https://core",
        "credential",
        &asked,
        Some((key.as_bytes(), caller)),
        &nonce,
    )
    .await?
    {
        Answer::Ok(v) => v["token"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| bad("core answered no grant")),
        Answer::Error(f) => Err(bad(f.message)),
    }
}

/// How many of `children` (the index's rows) are still live.
pub fn live_children(children: &Value) -> usize {
    children
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["ended_at"].is_null())
        .count()
}

/// Whether any of `children` ended after `since` (ms).
pub fn ended_since(children: &Value, since: i64) -> bool {
    children
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["ended_at"].as_i64().is_some_and(|t| t > since))
}

/// What a planner reads when its wait ends: each executor that ended since `since`, with its
/// state and the start of its report, then how many are still live.
pub fn children_summary(children: &Value, since: i64) -> String {
    let mut lines = Vec::new();
    for c in children.as_array().into_iter().flatten() {
        if c["ended_at"].as_i64().is_some_and(|t| t > since) {
            let report: String = c["report"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(600)
                .collect();
            lines.push(format!(
                "- {} {} on `{}`: {report}",
                c["run"].as_str().unwrap_or("?"),
                c["state"].as_str().unwrap_or("?"),
                c["branch"].as_str().unwrap_or("?"),
            ));
        }
    }
    let live = live_children(children);
    if lines.is_empty() {
        lines.push("no executor has ended since you last waited".into());
    }
    lines.push(format!(
        "{live} executor(s) still running; executor_result has each report"
    ));
    lines.join("\n")
}

/// What a Claude Code session needs to take over a run: its brief, where its work is, what it
/// had done and spent, and how to register itself. Pure.
pub fn handoff_packet(public: &Value, brief: &str, base: Option<&str>) -> Value {
    let branch = public["branch"].as_str().unwrap_or_default();
    let run = public["run"].as_str().unwrap_or_default();
    let prompt = format!(
        "You take over pigeon AI agent run {run}, handed to the Claude chat by the person.\n\n\
         Brief:\n\n{brief}\n\n\
         Its work so far is on branch `{branch}` (started from `{}`); fetch it from Artifacts \
         and go on from its tip. Its last step: {}. Its report so far: {}.\n\n\
         Call ai_agents_report with run {run} as you go and at your end.",
        base.unwrap_or("main"),
        public["step"].as_str().unwrap_or("none"),
        public["report"].as_str().unwrap_or("none"),
    );
    json!({
        "run": run,
        "role": public["role"],
        "brief": brief,
        "branch": branch,
        "base": base.unwrap_or("main"),
        "spent": public["spent"],
        "session_prompt": prompt,
        "next": "start a Claude Code session with session_prompt on this repository, then \
                 ai_agents_register it with parent_run set to this run",
    })
}

/// The service user key of a run of `role`: its delegate, linked to the person.
pub fn service_key(role: Role) -> &'static str {
    match role {
        Role::Planner => "ai-agent:planner",
        Role::Executor | Role::Mechanical => "ai-agent:executor",
    }
}

/// Asks core for service user `key` (made on first ask; with a link request for the person
/// `caller` acts for while it is not linked).
pub async fn service_user(env: &Env, caller: &Value, key: &str) -> Result<Value> {
    let k = caller_key(env).await?;
    let nonce = pigeon_core::webauthn::random()?;
    let platform = env.service(PLATFORM)?;
    match pigeon_contract::ask(
        &platform,
        "https://core",
        "service_user",
        &json!({ "key": key }),
        Some((k.as_bytes(), caller)),
        &nonce,
    )
    .await?
    {
        Answer::Ok(v) => Ok(v),
        Answer::Error(f) => Err(bad(f.message)),
    }
}

/// The person `caller` (as core serializes it) stands for: the one it acts for, or itself.
pub fn person_of(caller: &Value) -> Option<&str> {
    caller["acting_for"]["id"]
        .as_str()
        .or_else(|| caller["user"]["id"].as_str())
}

/// Who run `run` acts as: its role's service user when that user is linked to the same person
/// as `starter`; otherwise `starter` itself. Marked with the run's id either way. Pure.
pub fn chosen_caller(starter: &Value, service: Option<&Value>, run: &str) -> Value {
    let mut caller = match service {
        Some(s)
            if s["acting_for"]["id"].as_str().is_some()
                && s["acting_for"]["id"].as_str() == person_of(starter) =>
        {
            json!({ "user": s["user"], "acting_for": s["acting_for"] })
        }
        _ => starter.clone(),
    };
    caller["run"] = json!(run);
    caller
}

/// [`chosen_caller`], asking core for the role's service user; the starter when core cannot
/// answer.
pub async fn run_caller(env: &Env, starter: &Value, role: Role, run: &str) -> Value {
    let service = service_user(env, starter, service_key(role)).await.ok();
    let svc = service.as_ref().map(|s| &s["caller"]);
    chosen_caller(starter, svc, run)
}

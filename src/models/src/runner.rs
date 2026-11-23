//! Async model jobs, the objects that run them: one Durable Object class (`ModelJobs`, with
//! SQLite) in two roles. Object `index` keeps every job's row: it queues jobs, starts them while
//! their lane has room ([`jobs::LANES`]), marks a silent one lost, sends notifies and deletes
//! rows once kept long enough. Each job streams in its own object, `job:{id}`, from that
//! object's alarm: the request goes through the AI Gateway with the caller's tags and no payload
//! kept, its answer is read as it arrives, progress goes to `index` every 3 seconds and the
//! result once at the end. The prompt is kept only until the job starts (in `index`) and while it
//! runs (in its own object); the answer only in `index`, until it expires. Requests and answers
//! that may be large (an AI agent's turn, its content blocks) are kept in `parts`, cut into pieces
//! well under a row's limit.

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use pigeon_core::ai_gateway::{self, Streamed};
use pigeon_core::durable::{self, Db, opt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{
    Env, Error, Request, Response, Result, SqlStorageValue, State as ObjectState, console_error,
    js_sys,
};

use super::Dialect;
use super::cache;
use super::jobs::{self, State};
use super::stream::Reading;

/// The class's binding.
pub const BINDING: &str = "MODEL_JOBS";
/// The one index object.
pub const INDEX: &str = "index";

/// Each object's schema, one statement per migration, applied in order and never edited. An
/// index object uses `jobs`; a job object uses `run`.
const MIGRATIONS: [&str; 9] = [
    "CREATE TABLE jobs (id TEXT PRIMARY KEY, caller TEXT, caller_name TEXT, for_id TEXT, task TEXT, provider TEXT NOT NULL, model TEXT NOT NULL, state TEXT NOT NULL, request TEXT, timeout_ms INTEGER NOT NULL, created_at INTEGER NOT NULL, started_at INTEGER, deadline_at INTEGER, finished_at INTEGER, progress_at INTEGER, progress_chars INTEGER, tokens_in INTEGER, tokens_out INTEGER, stop_reason TEXT, reply_model TEXT, text TEXT, error TEXT, notify_url TEXT, notify_key TEXT, notify_tries INTEGER NOT NULL DEFAULT 0, notify_at INTEGER, read_at INTEGER, expires_at INTEGER NOT NULL)",
    "CREATE TABLE run (id TEXT PRIMARY KEY, request TEXT NOT NULL, started INTEGER NOT NULL DEFAULT 0)",
    "ALTER TABLE jobs ADD COLUMN lane TEXT NOT NULL DEFAULT 'default'",
    "ALTER TABLE jobs ADD COLUMN usage TEXT",
    "ALTER TABLE run ADD COLUMN cancelled INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE parts (owner TEXT NOT NULL, n INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY (owner, n))",
    "CREATE INDEX jobs_lane_state ON jobs (lane, state)",
    "CREATE TABLE cache_usage (finished_at INTEGER NOT NULL, provider TEXT NOT NULL, model TEXT NOT NULL, caller TEXT, caller_name TEXT, for_id TEXT, for_name TEXT, task TEXT, cache_read INTEGER NOT NULL, cache_write_5m INTEGER NOT NULL, cache_write_1h INTEGER NOT NULL, cost_micros INTEGER NOT NULL)",
    "ALTER TABLE jobs ADD COLUMN log_id TEXT",
];

/// The longest piece of a large value kept in one row of `parts`, in bytes.
const PART_BYTES: usize = 512 * 1024;

/// What the class needs from the hosting Worker.
pub struct Settings {
    /// The AI gateway's ID.
    pub gateway: String,
    /// Where a job's Cloudflare token comes from.
    pub token: Token,
}

/// Where the objects get the token a job's model call acts with.
pub enum Token {
    /// From the parent token, by its Secrets Store binding: core (pigeon-platform), which holds
    /// it.
    Parent(String),
    /// A short-lived `ai_gateway` token core mints for each job (`credential`, kind `scoped`),
    /// asked over the `PLATFORM` binding as this caller: a Worker that holds no parent token
    /// (pigeon-model-gateway), signing as itself.
    Scoped(Value),
}

/// A request as a job carries it, from submission until it has run.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Ask {
    /// How the provider's requests and answers are shaped.
    pub dialect: Dialect,
    /// The gateway's name for the provider.
    pub provider: String,
    /// The provider's path, under the gateway's.
    pub path: String,
    /// Headers the request needs beside the gateway's.
    pub headers: Vec<(String, String)>,
    /// The tags the request carries in the gateway's logs.
    pub tags: Vec<(String, String)>,
    /// The provider's request body, asking for a stream.
    pub body: Value,
    /// How long it may run, in milliseconds.
    pub timeout_ms: i64,
}

/// A job as submitted.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct New {
    /// The job's id.
    pub id: String,
    /// Who sent it.
    pub caller: Option<String>,
    /// The sender's name.
    pub caller_name: Option<String>,
    /// The person the sender acts for, when any.
    pub for_id: Option<String>,
    /// Its task label.
    pub task: Option<String>,
    /// The gateway's name for its provider.
    pub provider: String,
    /// The model asked.
    pub model: String,
    /// Where to POST when it ends.
    pub notify_url: Option<String>,
    /// The key that notify is signed with.
    pub notify_key: Option<String>,
    /// The lane it runs in.
    #[serde(default = "default_lane")]
    pub lane: String,
    /// The request itself.
    pub ask: Ask,
}

fn default_lane() -> String {
    "default".into()
}

/// A job's row.
#[derive(Deserialize, Debug)]
struct Row {
    id: String,
    caller: Option<String>,
    caller_name: Option<String>,
    for_id: Option<String>,
    task: Option<String>,
    provider: String,
    model: String,
    state: String,
    request: Option<String>,
    timeout_ms: i64,
    created_at: i64,
    started_at: Option<i64>,
    deadline_at: Option<i64>,
    finished_at: Option<i64>,
    progress_at: Option<i64>,
    progress_chars: Option<i64>,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    stop_reason: Option<String>,
    reply_model: Option<String>,
    text: Option<String>,
    error: Option<String>,
    notify_url: Option<String>,
    notify_key: Option<String>,
    notify_tries: i64,
    read_at: Option<i64>,
    expires_at: i64,
    #[serde(default = "default_lane")]
    lane: String,
    usage: Option<String>,
    /// The AI Gateway log of the job's request, when the stream opened.
    log_id: Option<String>,
}

impl Row {
    fn state(&self) -> State {
        State::named(&self.state).unwrap_or(State::Lost)
    }

    /// What `models_status` shows: never the prompt, the answer or the notify key.
    fn public(&self) -> Value {
        json!({
            "id": self.id,
            "state": self.state,
            "provider": self.provider,
            "model": self.model,
            "task": self.task,
            "lane": self.lane,
            "caller_name": self.caller_name,
            "created_at": self.created_at,
            "started_at": self.started_at,
            "deadline_at": self.deadline_at,
            "finished_at": self.finished_at,
            "progress_at": self.progress_at,
            "chars_so_far": self.progress_chars,
            "tokens_in": self.tokens_in,
            "tokens_out": self.tokens_out,
            "stop_reason": self.stop_reason,
            "log_id": self.log_id,
            "error": self.error,
            "timeout_ms": self.timeout_ms,
            "notify": self.notify_url.is_some(),
            "read_at": self.read_at,
            "expires_at": self.expires_at,
        })
    }
}

fn now() -> i64 {
    js_sys::Date::now() as i64
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn opt_i(v: Option<i64>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, Into::into)
}

/// Calls `op` on object `name`.
pub async fn call(env: &Env, name: &str, op: &str, args: &Value) -> Result<Value> {
    durable::call(env, BINDING, name, op, args).await
}

/// The objects' logic; the hosting Worker's Durable Object class wraps it.
pub struct ModelJobs {
    db: Db,
    state: ObjectState,
    env: Env,
    settings: Settings,
}

impl ModelJobs {
    /// The objects' logic, its migrations applied.
    pub fn new(state: ObjectState, env: Env, settings: Settings) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self {
            db,
            state,
            env,
            settings,
        })
    }

    /// Answers one operation from another object or the capability.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    /// The alarm: a job object runs its job; the index keeps the queue moving.
    pub async fn alarm(&self) -> Result<Response> {
        if self.run_row()?.is_some() {
            self.run().await?;
        } else {
            self.sweep().await?;
        }
        Response::ok("ok")
    }

    /// Keeps `value` as `owner`'s pieces in `parts`, replacing any it had.
    fn put_parts(&self, owner: &str, value: &str) -> Result<()> {
        self.drop_parts(owner)?;
        let mut rest = value;
        let mut n = 0i64;
        while !rest.is_empty() {
            let mut cut = rest.len().min(PART_BYTES);
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            let (piece, tail) = rest.split_at(cut);
            self.db.exec(
                "INSERT INTO parts (owner, n, data) VALUES (?, ?, ?)",
                vec![owner.into(), n.into(), piece.into()],
            )?;
            rest = tail;
            n += 1;
        }
        Ok(())
    }

    /// `owner`'s value from `parts`, joined; `None` when it has none.
    fn get_parts(&self, owner: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct Part {
            data: String,
        }
        let parts: Vec<Part> = self.db.rows(
            "SELECT data FROM parts WHERE owner = ? ORDER BY n",
            vec![owner.into()],
        )?;
        if parts.is_empty() {
            return Ok(None);
        }
        Ok(Some(parts.into_iter().map(|p| p.data).collect()))
    }

    fn drop_parts(&self, owner: &str) -> Result<()> {
        self.db
            .exec("DELETE FROM parts WHERE owner = ?", vec![owner.into()])
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        match op {
            // ---- the index ----
            "submit" => {
                let new: New = serde_json::from_value(a.clone())?;
                self.submit(new).await
            }
            "status" => {
                let row = self.readable(s("id"), s("reader"))?;
                Ok(row.public())
            }
            "result" => self.result(s("id"), s("reader")).await,
            "cancel" => self.cancel(s("id"), s("reader")).await,
            "progress" => {
                self.db.exec(
                    "UPDATE jobs SET progress_at = ?, progress_chars = ? WHERE id = ? AND state = 'running'",
                    vec![now().into(), opt_i(a["chars"].as_i64()), s("id").into()],
                )?;
                Ok(Value::Null)
            }
            "finish" => self.finish(a).await,
            "cache_usage" => self.cache_rows(a["since"].as_i64().unwrap_or(0)),
            // ---- a job object ----
            "start" => {
                if self.run_row()?.is_none() {
                    self.put_parts("run", &a["ask"].to_string())?;
                    self.db.exec(
                        "INSERT INTO run (id, request) VALUES (?, '')",
                        vec![s("id").into()],
                    )?;
                    self.state.storage().set_alarm(0).await?;
                }
                Ok(Value::Null)
            }
            "stop" => {
                self.db.exec("UPDATE run SET cancelled = 1", vec![])?;
                Ok(Value::Null)
            }
            other => Err(bad(&format!("no operation `{other}`"))),
        }
    }

    fn row(&self, id: &str) -> Result<Option<Row>> {
        self.db
            .row("SELECT * FROM jobs WHERE id = ?", vec![id.into()])
    }

    /// Job `id`, when `reader` may read it; the same answer as for no job otherwise, so a job's
    /// existence says nothing to anyone else.
    fn readable(&self, id: &str, reader: &str) -> Result<Row> {
        self.row(id)?
            .filter(|r| jobs::may_read(r.caller.as_deref(), r.for_id.as_deref(), reader))
            .ok_or_else(|| bad("no such job, or not yours"))
    }

    fn count(&self, states: &str) -> Result<u32> {
        self.count_in(states, None)
    }

    /// How many jobs are in `states`, in `lane` when one is named.
    fn count_in(&self, states: &str, lane: Option<&str>) -> Result<u32> {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let n: Option<N> = match lane {
            Some(l) => self.db.row(
                &format!("SELECT COUNT(*) AS n FROM jobs WHERE state IN {states} AND lane = ?"),
                vec![l.into()],
            )?,
            None => self.db.row(
                &format!("SELECT COUNT(*) AS n FROM jobs WHERE state IN {states}"),
                vec![],
            )?,
        };
        Ok(n.map_or(0, |n| n.n.max(0) as u32))
    }

    async fn submit(&self, new: New) -> Result<Value> {
        jobs::may_submit(self.count("('queued', 'running')")?).map_err(|e| bad(&e))?;
        let t = now();
        // The request waits in `parts`, which holds a turn of any size.
        self.put_parts(
            &format!("req:{}", new.id),
            &serde_json::to_string(&new.ask)?,
        )?;
        self.db.exec(
            "INSERT INTO jobs (id, caller, caller_name, for_id, task, provider, model, state, request, timeout_ms, created_at, notify_url, notify_key, expires_at, lane) VALUES (?, ?, ?, ?, ?, ?, ?, 'queued', NULL, ?, ?, ?, ?, ?, ?)",
            vec![
                new.id.as_str().into(),
                opt(new.caller.as_deref()),
                opt(new.caller_name.as_deref()),
                opt(new.for_id.as_deref()),
                opt(new.task.as_deref()),
                new.provider.as_str().into(),
                new.model.as_str().into(),
                new.ask.timeout_ms.into(),
                t.into(),
                opt(new.notify_url.as_deref()),
                opt(new.notify_key.as_deref()),
                jobs::expires_at(t, None).into(),
                new.lane.as_str().into(),
            ],
        )?;
        self.pump().await?;
        self.reschedule().await?;
        let row = self.row(&new.id)?.ok_or_else(|| bad("the job vanished"))?;
        Ok(row.public())
    }

    /// Starts queued jobs, oldest first in each lane, while the lane has room.
    async fn pump(&self) -> Result<()> {
        for (lane, _) in jobs::LANES {
            self.pump_lane(lane).await?;
        }
        Ok(())
    }

    async fn pump_lane(&self, lane: &str) -> Result<()> {
        let room = jobs::to_start(
            lane,
            self.count_in("('running')", Some(lane))?,
            self.count_in("('queued')", Some(lane))?,
        );
        if room == 0 {
            return Ok(());
        }
        let queued: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE state = 'queued' AND lane = ? ORDER BY created_at, id LIMIT ?",
            vec![lane.into(), i64::from(room).into()],
        )?;
        for row in queued {
            let t = now();
            let kept = match &row.request {
                Some(r) => Some(r.clone()),
                None => self.get_parts(&format!("req:{}", row.id))?,
            };
            let ask: Option<Value> = kept.as_deref().and_then(|r| serde_json::from_str(r).ok());
            let started = match ask {
                Some(ask) => {
                    call(
                        &self.env,
                        &format!("job:{}", row.id),
                        "start",
                        &json!({ "id": row.id, "ask": ask }),
                    )
                    .await
                }
                None => Err(bad("its request is gone")),
            };
            match started {
                // The request now lives in the job's object only.
                Ok(_) => {
                    self.db.exec(
                        "UPDATE jobs SET state = 'running', request = NULL, started_at = ?, deadline_at = ? WHERE id = ? AND state = 'queued'",
                        vec![t.into(), (t + row.timeout_ms).into(), row.id.as_str().into()],
                    )?;
                    self.drop_parts(&format!("req:{}", row.id))?;
                }
                Err(e) => {
                    console_error!("models: starting job {}: {e}", row.id);
                    self.end(
                        &row,
                        State::Failed,
                        None,
                        Some(&format!("could not start: {e}")),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Moves `row` to final state `to` with what came of it, if the move is allowed; queues its
    /// notify. Answers whether it moved.
    fn end(
        &self,
        row: &Row,
        to: State,
        reply: Option<&Value>,
        error: Option<&str>,
    ) -> Result<bool> {
        if !jobs::allowed(row.state(), to) {
            return Ok(false);
        }
        let r = reply.cloned().unwrap_or(Value::Null);
        let t = now();
        let tags = self
            .get_parts(&format!("req:{}", row.id))?
            .and_then(|p| serde_json::from_str::<Ask>(&p).ok())
            .map(|a| a.tags)
            .unwrap_or_default();
        self.drop_parts(&format!("req:{}", row.id))?;
        if let Some(blocks) = r["content"].as_array().filter(|b| !b.is_empty()) {
            self.put_parts(
                &format!("content:{}", row.id),
                &Value::Array(blocks.clone()).to_string(),
            )?;
        }
        let usage = r
            .get("usage")
            .filter(|u| u.is_object())
            .map(Value::to_string);
        self.db.exec(
            "UPDATE jobs SET state = ?, request = NULL, finished_at = ?, text = ?, reply_model = ?, stop_reason = ?, tokens_in = ?, tokens_out = ?, error = ?, notify_at = ?, usage = ?, log_id = ? WHERE id = ? AND state = ?",
            vec![
                to.name().into(),
                t.into(),
                opt(r["text"].as_str()),
                opt(r["model"].as_str()),
                opt(r["stop_reason"].as_str()),
                opt_i(r["tokens_in"].as_i64()),
                opt_i(r["tokens_out"].as_i64()),
                opt(error),
                opt_i(row.notify_url.as_ref().map(|_| t)),
                opt(usage.as_deref()),
                opt(r["log_id"].as_str()),
                row.id.as_str().into(),
                row.state.as_str().into(),
            ],
        )?;
        if let Some(c) = cache::Row::new(t, &row.provider, &row.model, &tags, &r["usage"]) {
            self.keep_cache(&c)?;
        }
        Ok(true)
    }

    /// Keeps `c` in `cache_usage` and drops the rows past [`cache::KEEP_HOURS`].
    fn keep_cache(&self, c: &cache::Row) -> Result<()> {
        self.db.exec(
            "DELETE FROM cache_usage WHERE finished_at < ?",
            vec![(c.finished_at - cache::KEEP_HOURS * 3_600_000).into()],
        )?;
        self.db.exec(
            "INSERT INTO cache_usage (finished_at, provider, model, caller, caller_name, for_id, for_name, task, cache_read, cache_write_5m, cache_write_1h, cost_micros) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                c.finished_at.into(),
                c.provider.as_str().into(),
                c.model.as_str().into(),
                opt(c.caller.as_deref()),
                opt(c.caller_name.as_deref()),
                opt(c.for_id.as_deref()),
                opt(c.for_name.as_deref()),
                opt(c.task.as_deref()),
                c.cache_read.into(),
                c.cache_write_5m.into(),
                c.cache_write_1h.into(),
                c.cost_micros.into(),
            ],
        )
    }

    /// The cache rows finished since `since` (milliseconds), for `models_usage`. Summed in SQL
    /// per tag set: one row per provider, model and tag set, with the latest finish time.
    fn cache_rows(&self, since: i64) -> Result<Value> {
        let rows: Vec<cache::Row> = self.db.rows(
            "SELECT MAX(finished_at) AS finished_at, provider, model, caller, caller_name, \
             for_id, for_name, task, SUM(cache_read) AS cache_read, \
             SUM(cache_write_5m) AS cache_write_5m, SUM(cache_write_1h) AS cache_write_1h, \
             SUM(cost_micros) AS cost_micros FROM cache_usage WHERE finished_at >= ? \
             GROUP BY provider, model, caller, caller_name, for_id, for_name, task",
            vec![since.into()],
        )?;
        Ok(serde_json::to_value(rows)?)
    }

    async fn finish(&self, a: &Value) -> Result<Value> {
        let id = a["id"].as_str().unwrap_or_default();
        let to = a["state"]
            .as_str()
            .and_then(State::named)
            .filter(|s| s.terminal())
            .ok_or_else(|| bad("a final state is required"))?;
        if let Some(row) = self.row(id)? {
            self.end(&row, to, a.get("reply"), a["error"].as_str())?;
        }
        self.pump().await?;
        self.reschedule().await?;
        Ok(Value::Null)
    }

    async fn result(&self, id: &str, reader: &str) -> Result<Value> {
        let row = self.readable(id, reader)?;
        if !row.state().terminal() {
            return Err(bad(&format!(
                "job {id} is {}; ask models_status, or wait for its notify",
                row.state
            )));
        }
        let read_at = row.read_at.unwrap_or_else(now);
        self.db.exec(
            "UPDATE jobs SET read_at = ?, expires_at = ? WHERE id = ?",
            vec![
                read_at.into(),
                jobs::expires_at(row.created_at, Some(read_at)).into(),
                id.into(),
            ],
        )?;
        self.reschedule().await?;
        let parsed = |v: Option<String>| -> Value {
            v.and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null)
        };
        Ok(json!({
            "id": row.id,
            "state": row.state,
            "provider": row.provider,
            "model": row.reply_model.as_deref().unwrap_or(&row.model),
            "text": row.text,
            "content": parsed(self.get_parts(&format!("content:{}", row.id))?),
            "stop_reason": row.stop_reason,
            "tokens_in": row.tokens_in,
            "tokens_out": row.tokens_out,
            "usage": parsed(row.usage.clone()),
            "log_id": row.log_id,
            "error": row.error,
            "kept_until": jobs::expires_at(row.created_at, Some(read_at)),
        }))
    }

    /// Cancels job `id` for `reader`: a queued one never starts; a running one is told to stop
    /// and frees its slot at once.
    async fn cancel(&self, id: &str, reader: &str) -> Result<Value> {
        let row = self.readable(id, reader)?;
        jobs::may_cancel(row.state()).map_err(|e| bad(&e))?;
        let was_running = row.state() == State::Running;
        self.end(
            &row,
            State::Cancelled,
            None,
            Some("cancelled by its sender"),
        )?;
        if was_running {
            // The job's object stops reading at its next chunk; its report is then ignored.
            if let Err(e) = call(&self.env, &format!("job:{id}"), "stop", &json!({})).await {
                console_error!("models: stopping job {id}: {e}");
            }
        }
        self.pump().await?;
        self.reschedule().await?;
        let row = self.row(id)?.ok_or_else(|| bad("the job vanished"))?;
        Ok(row.public())
    }

    /// The index's alarm: lost jobs, the queue, notifies, expiry; then the next alarm.
    async fn sweep(&self) -> Result<()> {
        let t = now();
        let running: Vec<Row> = self
            .db
            .rows("SELECT * FROM jobs WHERE state = 'running'", vec![])?;
        for row in running {
            if jobs::lost(row.state(), row.deadline_at, t) {
                self.end(
                    &row,
                    State::Lost,
                    None,
                    Some("its runner stopped without a result"),
                )?;
            }
        }
        if let Err(e) = self.pump().await {
            console_error!("models: starting jobs: {e}");
        }
        let due: Vec<Row> = self.db.rows(
            "SELECT * FROM jobs WHERE notify_at IS NOT NULL AND notify_at <= ?",
            vec![t.into()],
        )?;
        for row in due {
            self.notify(&row).await?;
        }
        self.db
            .exec("DELETE FROM jobs WHERE expires_at <= ?", vec![t.into()])?;
        // Pieces whose job is gone: `req:{id}` and `content:{id}`.
        self.db.exec(
            "DELETE FROM parts WHERE substr(owner, instr(owner, ':') + 1) NOT IN (SELECT id FROM jobs)",
            vec![],
        )?;
        self.reschedule().await
    }

    /// Sends `row`'s notify once; a failure is tried again later, a few times.
    async fn notify(&self, row: &Row) -> Result<()> {
        let (Some(url), Some(key)) = (&row.notify_url, &row.notify_key) else {
            return self.db.exec(
                "UPDATE jobs SET notify_at = NULL WHERE id = ?",
                vec![row.id.as_str().into()],
            );
        };
        let body = jobs::notify_body(
            &row.id,
            row.state(),
            &row.provider,
            row.reply_model.as_deref().unwrap_or(&row.model),
            row.task.as_deref(),
            row.tokens_in,
            row.tokens_out,
            row.finished_at.unwrap_or_else(now),
        )
        .to_string();
        let status = pigeon_core::notify::post(url, key, body.as_bytes()).await;
        let tries = row.notify_tries as u32 + 1;
        let next = match &status {
            Ok(s) if (200..300).contains(s) => None,
            other => {
                // The job and the status only: never the URL's query, the key or the body.
                let why = match other {
                    Ok(s) => s.to_string(),
                    Err(e) => e.to_string(),
                };
                console_error!(
                    "models: notify for job {} failed (try {tries}): {why}",
                    row.id
                );
                jobs::notify_retry(tries, now())
            }
        };
        self.db.exec(
            "UPDATE jobs SET notify_tries = ?, notify_at = ? WHERE id = ?",
            vec![i64::from(tries).into(), opt_i(next), row.id.as_str().into()],
        )
    }

    async fn reschedule(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct At {
            at: Option<i64>,
        }
        let one = |q: &str| -> Result<Option<i64>> {
            Ok(self.db.row::<At>(q, vec![])?.and_then(|a| a.at))
        };
        let candidates = [
            one("SELECT MIN(deadline_at) AS at FROM jobs WHERE state = 'running'")?
                .map(|d| d + jobs::LOST_AFTER_MS + 1),
            // A queued job is started by a finish; this is the backstop.
            one("SELECT MIN(created_at) AS at FROM jobs WHERE state = 'queued'")?
                .map(|_| now() + 60_000),
            one("SELECT MIN(notify_at) AS at FROM jobs")?,
            one("SELECT MIN(expires_at) AS at FROM jobs")?,
        ];
        let storage = self.state.storage();
        match candidates.into_iter().flatten().min() {
            Some(at) => {
                let current = storage.get_alarm().await?;
                if current.is_none_or(|c| c > at) {
                    storage.set_alarm((at - now()).max(0)).await?;
                }
            }
            None => storage.delete_alarm().await?,
        }
        Ok(())
    }

    // ---- a job object ----

    fn run_row(&self) -> Result<Option<RunRow>> {
        self.db.row("SELECT * FROM run LIMIT 1", vec![])
    }

    /// Runs this object's job once. An alarm that finds it already started was restarted
    /// mid-stream: the job is lost rather than sent again, so it is never paid for twice.
    async fn run(&self) -> Result<()> {
        let Some(row) = self.run_row()? else {
            return Ok(());
        };
        let report = if row.started != 0 {
            json!({ "id": row.id, "state": "lost", "error": "its runner restarted mid-stream" })
        } else {
            self.db.exec(
                "UPDATE run SET started = 1 WHERE id = ?",
                vec![row.id.as_str().into()],
            )?;
            let request = if row.request.is_empty() {
                self.get_parts("run")?.unwrap_or_default()
            } else {
                row.request.clone()
            };
            match serde_json::from_str::<Ask>(&request) {
                Ok(ask) => self.stream(&row.id, &ask).await,
                Err(e) => {
                    json!({ "id": row.id, "state": "failed", "error": format!("its request is unreadable: {e}") })
                }
            }
        };
        // The request goes before the result is reported; the index keeps only the answer.
        self.db.exec("DELETE FROM run", vec![])?;
        self.db.exec("DELETE FROM parts", vec![])?;
        for wait in [0u64, 1_000, 5_000] {
            worker::Delay::from(std::time::Duration::from_millis(wait)).await;
            match call(&self.env, INDEX, "finish", &report).await {
                Ok(_) => return Ok(()),
                Err(e) => console_error!("models: reporting job {}: {e}", row.id),
            }
        }
        // The index marks it lost past its deadline.
        Ok(())
    }

    /// Streams `ask` until it ends, fails or runs out of time; the `finish` report.
    async fn stream(&self, id: &str, ask: &Ask) -> Value {
        let deadline = now() + ask.timeout_ms;
        let account = match &self.settings.token {
            Token::Parent(binding) => pigeon_core::account_from_env(&self.env, binding).await,
            Token::Scoped(caller) => super::credential_as(&self.env, caller, &["ai_gateway"]).await,
        };
        let account = match account {
            Ok(a) => a,
            Err(e) => return json!({ "id": id, "state": "failed", "error": e.to_string() }),
        };
        let headers: Vec<(&str, &str)> = ask
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let tags: Vec<(&str, &str)> = ask
            .tags
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let mut reading = Reading::default();
        let mut timed_out = false;
        let mut cancelled = false;
        let mut tries = 0u32;
        // The gateway's log of the request; it bills a failed or timed-out stream too.
        let mut log_id: Option<String> = None;
        let streamed = loop {
            let env = &self.env;
            let db = &self.db;
            let read = async |mut body: worker::ByteStream, opened: Option<String>| -> Result<()> {
                log_id = opened;
                let mut reported = now();
                loop {
                    let left = deadline - now();
                    if left <= 0 {
                        timed_out = true;
                        return Ok(());
                    }
                    let next = body.next();
                    let wait = worker::Delay::from(std::time::Duration::from_millis(left as u64));
                    futures_util::pin_mut!(next);
                    futures_util::pin_mut!(wait);
                    match select(next, wait).await {
                        Either::Left((Some(chunk), _)) => reading.feed(ask.dialect, &chunk?),
                        Either::Left((None, _)) => {
                            reading.end(ask.dialect);
                            return Ok(());
                        }
                        Either::Right(_) => {
                            timed_out = true;
                            return Ok(());
                        }
                    }
                    // A cancel reaches this object as `stop` while it awaits the network.
                    if stopped(db) {
                        cancelled = true;
                        return Ok(());
                    }
                    if now() - reported >= jobs::PROGRESS_EVERY_MS {
                        reported = now();
                        let chars = reading.text.chars().count() as i64;
                        if let Err(e) =
                            call(env, INDEX, "progress", &json!({ "id": id, "chars": chars })).await
                        {
                            console_error!("models: progress of job {id}: {e}");
                        }
                    }
                }
            };
            let streamed = ai_gateway::stream(
                &account,
                &self.settings.gateway,
                &ask.provider,
                &ask.path,
                &headers,
                &tags,
                &ask.body,
                read,
            )
            .await;
            // A refusal before any of the answer arrived is sent again, a few times, while
            // there is time: nothing was streamed, so nothing is paid for twice.
            if let Ok(Streamed::Refused(ran, opened)) = &streamed {
                log_id = opened.clone();
                tries += 1;
                if jobs::retryable(ran.status)
                    && !stopped(&self.db)
                    && let Some(wait) = jobs::retry_wait_ms(tries)
                    && now() + (wait as i64) < deadline
                {
                    console_error!(
                        "models: job {id} refused with {}; trying again in {wait} ms",
                        ran.status
                    );
                    worker::Delay::from(std::time::Duration::from_millis(wait)).await;
                    continue;
                }
            }
            break streamed;
        };
        let mut reply = serde_json::to_value(reading.reply()).unwrap_or(Value::Null);
        reply["content"] = reading.content();
        reply["usage"] = reading.usage();
        reply["log_id"] = json!(log_id);
        let (state, error) = match streamed {
            _ if cancelled => (State::Cancelled, Some("cancelled by its sender".into())),
            Err(e) => (State::Failed, Some(e.to_string())),
            Ok(Streamed::Refused(ran, _)) => {
                let shown: String = ran.body.to_string().chars().take(500).collect();
                (
                    State::Failed,
                    Some(format!("{} answered {}: {shown}", ask.provider, ran.status)),
                )
            }
            Ok(Streamed::Read((), _)) if timed_out => (
                State::TimedOut,
                Some(format!(
                    "stopped after its timeout of {} min; the text so far is kept",
                    ask.timeout_ms / 60_000
                )),
            ),
            Ok(Streamed::Read((), _)) => match (&reading.error, reading.done) {
                (Some(e), _) => (State::Failed, Some(e.clone())),
                (None, false) => (
                    State::Failed,
                    Some("the stream ended before the answer was complete".into()),
                ),
                (None, true) => (State::Succeeded, None),
            },
        };
        json!({ "id": id, "state": state.name(), "reply": reply, "error": error })
    }
}

/// Whether this job object was told to stop.
fn stopped(db: &Db) -> bool {
    #[derive(Deserialize)]
    struct C {
        cancelled: i64,
    }
    db.row::<C>("SELECT cancelled FROM run LIMIT 1", vec![])
        .ok()
        .flatten()
        .is_some_and(|c| c.cancelled != 0)
}

/// A job object's one row.
#[derive(Deserialize)]
struct RunRow {
    id: String,
    request: String,
    started: i64,
}

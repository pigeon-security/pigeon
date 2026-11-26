//! pigeon-dotest-a: a throwaway Worker for the Durable Object transfer rehearsal
//! (docs/plans/do-rehearsal.md; docs/plans/workers-split.md, T17). It hosts a SQLite class,
//! [`Probe`], that holds three rows and a pending alarm, and answers on its workers.dev URL what
//! the object holds, so the same answer can be compared before and after the class is
//! transferred to pigeon-dotest-b. It holds no secrets and only dummy data; every route asks
//! for the header `X-Rehearsal: dotest`, a speed bump, not security. Built for
//! `wasm32-unknown-unknown` only.
//!
//! Rebuilt for the second rehearsal (docs/plans/do-rehearsal.md, "Second rehearsal"): a cron
//! trigger asks the object for its state every five minutes and logs the answer as one
//! `console.error` line, `dotest-state {...}`, so `observability_errors` for this Worker shows
//! what `GET /state` would answer without anyone reaching workers.dev. While the object still
//! runs this Worker's code, the first such tick also seeds it, as `POST /seed` does.

#[cfg(not(target_arch = "wasm32"))]
compile_error!(
    "pigeon-dotest-a builds for wasm32 only: cargo build --target wasm32-unknown-unknown"
);

use serde_json::{Value, json};
use worker::{Context, Date, Env, Method, Request, Response, Result, SqlStorage, State, event};
// The `durable_object` macro's expansion names `wasm_bindgen` at the crate root.
use worker::wasm_bindgen;

/// This Worker's name, as deployed.
const WORKER: &str = "pigeon-dotest-a";
/// The Durable Object binding.
const BINDING: &str = "PROBE";
/// The one object the rehearsal uses.
const OBJECT: &str = "probe";
/// The header every route asks for, and its value.
const HEADER: (&str, &str) = ("X-Rehearsal", "dotest");
/// How far ahead the seeded alarm is: 24 hours, in milliseconds.
const ALARM_AHEAD_MS: i64 = 24 * 60 * 60 * 1000;

/// The rehearsal's object: three rows, a counter of fired alarms, and one pending alarm.
#[worker::durable_object]
pub struct Probe {
    state: State,
}

impl Probe {
    fn sql(&self) -> Result<SqlStorage> {
        let sql = self.state.storage().sql();
        sql.exec(
            "CREATE TABLE IF NOT EXISTS rows (n INTEGER PRIMARY KEY, label TEXT NOT NULL, at INTEGER NOT NULL)",
            None,
        )?;
        sql.exec(
            "CREATE TABLE IF NOT EXISTS counters (name TEXT PRIMARY KEY, n INTEGER NOT NULL)",
            None,
        )?;
        Ok(sql)
    }

    /// Writes the three rows (once: seeding again keeps the first ones) and sets the alarm 24 h
    /// ahead when none is set.
    async fn seed(&self) -> Result<Value> {
        let sql = self.sql()?;
        let at = Date::now().as_millis() as i64;
        for n in 1..=3i64 {
            sql.exec(
                "INSERT OR IGNORE INTO rows (n, label, at) VALUES (?, ?, ?)",
                vec![n.into(), format!("row-{n}").into(), at.into()],
            )?;
        }
        let storage = self.state.storage();
        if storage.get_alarm().await?.is_none() {
            storage.set_alarm(ALARM_AHEAD_MS).await?;
        }
        self.read().await
    }

    /// What the object holds: its id, the Worker whose code runs it, its rows, its alarm and how
    /// many alarms have fired.
    async fn read(&self) -> Result<Value> {
        let sql = self.sql()?;
        let rows: Vec<Value> = sql
            .exec("SELECT n, label, at FROM rows ORDER BY n", None)?
            .to_array()?;
        let fired: Vec<Value> = sql
            .exec("SELECT n FROM counters WHERE name = 'alarm_fired'", None)?
            .to_array()?;
        let fired = fired.first().and_then(|r| r["n"].as_i64()).unwrap_or(0);
        let alarm = self.state.storage().get_alarm().await?;
        Ok(json!({
            "id": self.state.id().to_string(),
            "host": WORKER,
            "rows": rows,
            "alarm": alarm,
            "alarm_fired": fired,
        }))
    }

    /// The cron's question: seeds (once, as `POST /seed`; this is the source's copy, which runs
    /// the object only before the transfer), then answers as `GET /state`.
    async fn tick(&self) -> Result<Value> {
        self.seed().await
    }
}

impl worker::DurableObject for Probe {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match (req.method(), req.path().as_str()) {
            (Method::Post, "/seed") => Response::from_json(&self.seed().await?), // route: open
            (Method::Get, "/state") => Response::from_json(&self.read().await?), // route: open
            (Method::Get, "/tick") => Response::from_json(&self.tick().await?),  // route: open
            _ => Response::error("not found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        self.sql()?.exec(
            "INSERT INTO counters (name, n) VALUES ('alarm_fired', 1) ON CONFLICT (name) DO UPDATE SET n = n + 1",
            None,
        )?;
        Response::ok("fired")
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if req.headers().get(HEADER.0)?.as_deref() != Some(HEADER.1) {
        return Response::error("not found", 404);
    }
    match (req.method(), req.path().as_str()) {
        (Method::Post, "/seed") => {} // route: open
        (Method::Get, "/state") => {} // route: open
        _ => return Response::error("not found", 404),
    }
    ask(&env, req).await
}

/// Every five minutes (`[triggers]`): asks the object `/tick` through this Worker's binding and
/// logs the answer, which is what `GET /state` answers, as one error line, so
/// `observability_errors` lists it (the same state each time folds into one line with a count).
#[event(scheduled)]
async fn scheduled(_event: worker::ScheduledEvent, env: Env, _ctx: worker::ScheduleContext) {
    let answer = async {
        let req = Request::new("https://probe/tick", Method::Get)?;
        let mut answer = ask(&env, req).await?;
        let status = answer.status_code();
        let text = answer.text().await?;
        Ok::<_, worker::Error>(if status == 200 {
            text
        } else {
            json!({ "worker": WORKER, "status": status, "body": text }).to_string()
        })
    }
    .await;
    match answer {
        Ok(state) => worker::console_error!("dotest-state {state}"),
        Err(e) => {
            let e = json!({ "worker": WORKER, "error": e.to_string() });
            worker::console_error!("dotest-state {e}")
        }
    }
}

/// Sends `req` to the object named [`OBJECT`] through [`BINDING`], and adds to its answer which
/// Worker asked and the id its binding gives that name.
async fn ask(env: &Env, req: Request) -> Result<Response> {
    let namespace = env.durable_object(BINDING)?;
    let id = namespace.id_from_name(OBJECT)?;
    let named = id.to_string();
    let mut answer = id.get_stub()?.fetch_with_request(req).await?;
    if answer.status_code() != 200 {
        return Ok(answer);
    }
    let mut object: Value = answer.json().await?;
    object["worker"] = json!(WORKER);
    object["id_from_name"] = json!(named);
    Response::from_json(&object)
}

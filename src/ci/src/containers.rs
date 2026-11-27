//! The Cloudflare Containers connector: one container per job attempt, each held by its own
//! `CiRunner` Durable Object named by the attempt. The container starts from the base image
//! with the job, its token and the platform's URLs in its environment; its runner fetches the
//! commit through the git proxy and reports on `/ci/runner/{job}`. The container stopping is the
//! backstop: the runner object's alarm watches it, and an exit without a result is reported to
//! the scheduler as a lost runner, which the state machine ignores once the job has its result.
//!
//! Cloudflare stops a container shortly after its object goes idle unless the object sets an
//! inactivity timeout, which a restarted object no longer has. So the runner sets one covering
//! the job's timeout and a margin ([`keep_ms`]) after the start, on every watch, and when a
//! restarted object finds its container still running.
//!
//! Instances are `standard-3` (2 vCPU, 8 GiB, 16 GB) for `small` and `standard-4` (4 vCPU,
//! 12 GiB, 20 GB) for a reduced `medium`, the largest Cloudflare offers. Under the
//! `durable_object` scheduling policy Cloudflare caps nothing, so the gateway keeps its own
//! limit on concurrent jobs. No docker, and no `network = "none"`: the runner must reach the
//! proxies.

use pigeon_core::containers::{self, Exit};
use pigeon_core::durable::{self, Db};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result, State, console_error, js_sys};

use super::class::{Class, Offered, Size};
use super::config::{Arch, Os};
use super::route::Availability;
use super::state::Report;
use super::{Connector, Cx, Dispatched, Hook, Pending, Run, Seen};

/// The connector's name.
pub const NAME: &str = "containers";
/// The runner objects' binding.
pub const BINDING: &str = "CI_RUNNER";
/// How often a runner object looks at its container.
const WATCH_MS: i64 = 60_000;
/// How soon after its container ends a runner object looks at it.
const ENDED_WATCH_MS: i64 = 1_000;
/// How long past the job's own timeout its container is kept, for the fetch, the report and
/// the deadline's allowance.
const KEEP_MARGIN_MIN: i64 = 15;

/// How long a job's container is kept running while its object is idle: its timeout and a
/// margin, at most Cloudflare's six hours.
pub fn keep_ms(timeout_min: u32) -> i64 {
    ((i64::from(timeout_min) + KEEP_MARGIN_MIN) * 60_000).min(containers::MAX_KEEP_MS)
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

fn now() -> i64 {
    js_sys::Date::now() as i64
}

/// The containers connector's settings.
pub struct Containers {
    /// The base image's key under `[containers.images]` in wrangler.toml; `None` until the base
    /// image is set, which leaves the connector unconfigured.
    pub image: Option<String>,
    /// The most jobs it runs at once.
    pub limit: u32,
    /// The runner's entrypoint in the base image.
    pub entrypoint: Vec<String>,
}

/// The instance a class runs on.
fn instance(class: &Class) -> Option<&'static str> {
    match class.size {
        Size::Small => Some("standard-3"),
        Size::Medium => Some("standard-4"),
        _ => None,
    }
}

/// The sizes Containers offers: (size, vCPU, memory GB, disk GB). The one source of truth.
const SIZES: [(Size, u32, u32, u32); 2] = [(Size::Small, 2, 8, 16), (Size::Medium, 4, 12, 20)];

/// The (vCPU, memory GB, disk GB) of an offered class name such as `linux-amd64-small`, or
/// None when Containers does not offer that class.
pub fn class_resources(class_name: &str) -> Option<(u32, u32, u32)> {
    SIZES.iter().find_map(|&(size, cpu, mem, disk)| {
        (offered(size, cpu, mem, disk).class.name() == class_name).then_some((cpu, mem, disk))
    })
}

fn offered(size: Size, cpu: u32, memory_gb: u32, disk_gb: u32) -> Offered {
    Offered {
        class: Class {
            os: Os::Linux,
            arch: Arch::Amd64,
            size,
            docker: false,
            gpu: false,
        },
        cpu,
        memory_gb,
        disk_gb,
        max_timeout_min: 360,
        network_none: false,
    }
}

/// The environment the runner gets.
fn runner_env(run: &Run, token: &str) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("PIGEON_JOB", run.job.clone()),
        ("PIGEON_COMMIT", run.commit.clone()),
        ("PIGEON_SCRIPT", run.script.clone()),
        ("PIGEON_TIMEOUT_MIN", run.timeout_min.to_string()),
        ("PIGEON_GIT_URL", run.git_url.clone()),
        ("PIGEON_RUNNER_URL", run.runner_url.clone()),
        ("PIGEON_JOB_TOKEN", token.to_string()),
    ];
    if run.pass_token {
        env.push(("PIGEON_PASS_TOKEN", "1".to_string()));
    }
    env
}

/// The runner object an attempt runs in, which is also its handle.
pub fn handle(run: &Run) -> String {
    format!("{}-{}", run.job, run.attempt)
}

async fn runner(env: &Env, handle: &str, op: &str, args: Value) -> Result<Value> {
    durable::call(env, BINDING, handle, op, &args).await
}

impl Connector for Containers {
    fn name(&self) -> &'static str {
        NAME
    }

    fn externals(&self) -> Vec<Value> {
        vec![json!({
            "name": "Cloudflare Containers",
            "kind": "CI connector",
            "detail": "runners in core's CiRunner containers",
            "place": "container",
        })]
    }

    fn offers(&self) -> Vec<Offered> {
        SIZES
            .iter()
            .map(|&(size, cpu, mem, disk)| offered(size, cpu, mem, disk))
            .collect()
    }

    fn takes_token(&self) -> bool {
        true
    }

    fn limit(&self) -> Option<u32> {
        Some(self.limit)
    }

    fn available<'a>(&'a self, cx: &'a Cx<'a>) -> Pending<'a, Availability> {
        Box::pin(async move {
            let configured = self.image.is_some() && cx.env.durable_object(BINDING).is_ok();
            Ok(Availability {
                configured,
                healthy: configured,
                free: None,
                queued: 0,
            })
        })
    }

    fn dispatch<'a>(&'a self, cx: &'a Cx<'a>, run: &'a Run) -> Pending<'a, Dispatched> {
        Box::pin(async move {
            let image = self
                .image
                .as_deref()
                .ok_or_else(|| bad("no base image is set"))?;
            let instance = instance(&run.class)
                .ok_or_else(|| bad(&format!("no instance runs {}", run.class)))?;
            let token = run
                .token
                .as_deref()
                .ok_or_else(|| bad("a container needs the job token"))?;
            let handle = handle(run);
            let env: serde_json::Map<String, Value> = runner_env(run, token)
                .into_iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect();
            runner(
                cx.env,
                &handle,
                "start",
                json!({
                    "job": run.job, "image": image, "instance": instance,
                    "entrypoint": self.entrypoint, "env": env,
                    "keep_ms": keep_ms(run.timeout_min),
                }),
            )
            .await?;
            Ok(Dispatched { handle, url: None })
        })
    }

    fn status<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, Option<Report>> {
        Box::pin(async move {
            let v = runner(cx.env, handle, "status", json!({})).await?;
            Ok(lost(&v))
        })
    }

    fn cancel<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, ()> {
        Box::pin(async move {
            runner(cx.env, handle, "stop", json!({})).await?;
            Ok(())
        })
    }

    fn webhook<'a>(&'a self, _cx: &'a Cx<'a>, _hook: &'a Hook) -> Pending<'a, Option<Seen>> {
        Box::pin(async { Err(bad("containers report on /ci/runner, not by webhook")) })
    }
}

/// What a runner object's status says: a container that has stopped is a lost runner, since a
/// runner that reported its result made the job final first.
fn lost(status: &Value) -> Option<Report> {
    if status["running"].as_bool() == Some(false) && status["started"].as_bool() == Some(true) {
        let why = lost_why(status["exit"].as_i64(), status["why"].as_str());
        Some(Report::Infra { why })
    } else {
        None
    }
}

/// Why a container that stopped before its runner reported was lost: its exit code, with what
/// the runner (ci/runner/entrypoint.sh) means by it, or Cloudflare's reason when it ended
/// without one (it never started, or it was stopped). Neither is known when the object asked
/// too late: Cloudflare refuses `monitor()` once a container is no longer running.
pub fn lost_why(exit: Option<i64>, why: Option<&str>) -> String {
    match (exit, why.map(str::trim).filter(|w| !w.is_empty())) {
        (Some(code), _) => {
            let meant = match code {
                2 => " (the runner's environment was incomplete)",
                3 => " (the runner could not fetch the commit through the git proxy)",
                _ => "",
            };
            format!("the container exited with code {code} before reporting{meant}")
        }
        (None, Some(why)) => {
            let why: String = why.chars().take(WHY_MAX).collect();
            format!("the container stopped before reporting: {why}")
        }
        (None, None) => "the container stopped before reporting".to_string(),
    }
}

/// The most of Cloudflare's reason a lost runner's detail carries, in characters.
const WHY_MAX: usize = 200;

const MIGRATIONS: [&str; 3] = [
    "CREATE TABLE run (id INTEGER PRIMARY KEY CHECK (id = 1), job TEXT NOT NULL, started_at INTEGER NOT NULL, exit INTEGER, ended INTEGER NOT NULL)",
    "ALTER TABLE run ADD COLUMN keep_ms INTEGER",
    "ALTER TABLE run ADD COLUMN why TEXT",
];

/// One job attempt's container; the hosting Worker's `CiRunner` class wraps it. It keeps the
/// job's id and how its container ended, never the job token.
pub struct Runner {
    db: Db,
    state: State,
    env: Env,
}

#[derive(Deserialize)]
struct Kept {
    job: String,
    exit: Option<i64>,
    /// How the container ended when it had no exit code, as Cloudflare said.
    why: Option<String>,
    ended: i64,
    /// Rows from before it was kept read as the ceiling.
    keep_ms: Option<i64>,
}

impl Kept {
    fn keep(&self) -> i64 {
        self.keep_ms.unwrap_or(containers::MAX_KEEP_MS)
    }
}

impl Runner {
    /// The runner object, its migrations applied.
    pub fn new(state: State, env: Env) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        let runner = Self { db, state, env };
        // A restarted object has no inactivity timeout: its running container would be stopped
        // before the next watch, so it is set again at once.
        if containers::running(&runner.state)
            && let Ok(Some(kept)) = runner.kept()
            && kept.ended == 0
        {
            match containers::ask_keep(&runner.state, kept.keep()) {
                Ok(Some(answer)) => worker::wasm_bindgen_futures::spawn_local(async move {
                    if let Err(e) = answer.await {
                        console_error!("ci: runner for {}: keeping its container: {e}", kept.job);
                    }
                }),
                Ok(None) => {}
                Err(e) => console_error!("ci: runner for {}: keeping its container: {e}", kept.job),
            }
        }
        Ok(runner)
    }

    fn kept(&self) -> Result<Option<Kept>> {
        self.db
            .row("SELECT job, exit, why, ended, keep_ms FROM run", vec![])
    }

    /// Answers the connector's operations.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, a| self.handle(op, &a).await).await
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        match op {
            "start" => {
                if self.kept()?.is_some() {
                    return Err(bad("this attempt has already started"));
                }
                let s = |k: &str| a[k].as_str().unwrap_or_default();
                let env: Vec<(String, String)> = a["env"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect();
                let env: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let entrypoint: Vec<&str> = a["entrypoint"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                containers::start(
                    &self.state,
                    &containers::Start {
                        image: s("image"),
                        instance: s("instance"),
                        env: &env,
                        entrypoint: &entrypoint,
                        internet: true,
                    },
                )?;
                let keep = a["keep_ms"].as_i64().unwrap_or(containers::MAX_KEEP_MS);
                self.db.exec(
                    "INSERT INTO run (id, job, started_at, ended, keep_ms) VALUES (1, ?, ?, 0, ?)",
                    vec![s("job").into(), now().into(), keep.into()],
                )?;
                self.watch(s("job"));
                containers::keep_for(&self.state, keep).await?;
                self.state.storage().set_alarm(WATCH_MS).await?;
                Ok(json!({ "started": true }))
            }
            "status" => {
                let kept = self.kept()?;
                Ok(json!({
                    "started": kept.is_some(),
                    "running": containers::running(&self.state),
                    "exit": kept.as_ref().and_then(|k| k.exit),
                    "why": kept.and_then(|k| k.why),
                }))
            }
            "stop" => {
                containers::destroy(&self.state, "cancelled").await?;
                self.db.exec("UPDATE run SET ended = 1", vec![])?;
                Ok(Value::Null)
            }
            other => Err(bad(&format!("no operation `{other}`"))),
        }
    }

    /// Asks Cloudflare, right after the start, how the container will end, and records it when
    /// it does: its exit code, or why it ended without one. Asked any later, once the container
    /// has stopped, `monitor()` is refused and how it ended is lost, which left every lost
    /// runner's detail as a bare "stopped before reporting" (docs/plans/ci-container-starts.md).
    fn watch(&self, job: &str) {
        let ended = match containers::watch(&self.state) {
            Ok(ended) => ended,
            Err(e) => {
                console_error!("ci: runner for {job}: watching its container: {e}");
                return;
            }
        };
        let storage = self.state.storage();
        let sql = storage.sql();
        let job = job.to_string();
        worker::wasm_bindgen_futures::spawn_local(async move {
            let (exit, why) = match ended.await {
                Exit::Code(c) => (Some(c), None),
                Exit::Failed(why) => (None, Some(why)),
            };
            if let Some(why) = &why {
                console_error!("ci: runner for {job}: its container ended: {why}");
            }
            let args: Vec<worker::SqlStorageValue> = vec![
                exit.map_or(worker::SqlStorageValue::Null, Into::into),
                durable::opt(why.as_deref()),
            ];
            if let Err(e) = sql.exec(
                "UPDATE run SET exit = ?, why = ? WHERE exit IS NULL AND why IS NULL",
                args,
            ) {
                console_error!("ci: runner for {job}: recording how its container ended: {e}");
            }
            // The watch tells the scheduler now rather than at its next round, so a runner lost
            // at the start is retried at once; one that reported is ignored there.
            if let Err(e) = storage.set_alarm(ENDED_WATCH_MS).await {
                console_error!("ci: runner for {job}: watching sooner: {e}");
            }
        });
    }

    /// Watches the container; once it has stopped, tells the scheduler, which ignores it when
    /// the job already has its result.
    pub async fn alarm(&self) -> Result<Response> {
        let Some(kept) = self.kept()? else {
            return Response::ok("idle");
        };
        if kept.ended == 1 {
            return Response::ok("ended");
        }
        if containers::running(&self.state) {
            if let Err(e) = containers::keep_for(&self.state, kept.keep()).await {
                console_error!("ci: runner for {}: keeping its container: {e}", kept.job);
            }
            self.state.storage().set_alarm(WATCH_MS).await?;
            return Response::ok("running");
        }
        // How it ended, as recorded when it did (Runner::watch); else asked now, which after a
        // restart, or once the container has stopped, answers nothing.
        let (exit, why) = if kept.exit.is_some() || kept.why.is_some() {
            (kept.exit, kept.why.clone())
        } else {
            match containers::monitor(&self.state).await {
                Ok(Exit::Code(c)) => (Some(c), None),
                Ok(Exit::Failed(why)) => (None, Some(why)),
                Err(_) => (None, None),
            }
        };
        self.db.exec(
            "UPDATE run SET ended = 1, exit = ?, why = ?",
            vec![
                exit.map_or(worker::SqlStorageValue::Null, Into::into),
                durable::opt(why.as_deref()),
            ],
        )?;
        let handle = self
            .state
            .id()
            .name()
            .unwrap_or_else(|| format!("{}-?", kept.job));
        let report = lost(&json!({ "started": true, "running": false, "exit": exit, "why": why }));
        if let Some(report) = report {
            let seen = Seen {
                handle,
                report,
                url: None,
            };
            if let Err(e) = durable::call(
                &self.env,
                super::BINDING,
                super::OBJECT,
                "seen",
                &json!({ "connector": NAME, "seen": seen }),
            )
            .await
            {
                console_error!("ci: runner for {}: telling the scheduler: {e}", kept.job);
            }
        }
        Response::ok("ended")
    }
}

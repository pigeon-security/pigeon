//! CI: a gateway that routes CI runs to runners. A caller names a job in the repository's
//! `.pigeon/ci.toml` and a commit; the gateway reads the job's needs at that commit, picks a
//! connector (a CI system: Cloudflare Containers, Buildkite, GitHub Actions) that can run its
//! runner class, dispatches it, and follows it to a result. It routes runs; it does not decide
//! which checks exist, and it never reads the job's script.
//!
//! Jobs live in the `CiScheduler`, a Durable Object with SQLite storage whose alarm dispatches
//! queued jobs, polls overdue ones and enforces deadlines. Each state change is written once and
//! fans out: to `ci_status` and `ci_jobs`, the `pigeon-ci-events` queue, a signed POST to the
//! job's `notify` URL, and, for a final result, a git note on the commit under `refs/notes/ci`.
//!
//! A runner fetches the commit through the platform's git proxy with a job token: read-only,
//! for one repository and commit, living until the job's deadline. Webhooks land on
//! `/ci/hooks/{connector}` and are verified by their connector before anything in them is
//! read; runners report on `/ci/runner/{job}` with their job token. Every job carries its
//! caller and an optional `task` label, as model requests do.

pub mod buildkite;
pub mod class;
pub mod config;
pub mod containers;
pub mod notes;
pub mod notify;
pub mod output;
pub mod route;
pub mod scheduler;
pub mod state;
pub mod token;

use std::future::Future;
use std::pin::Pin;

use pigeon_core::mcp::Tool;
use pigeon_core::webauthn::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Method, Request, Response, Result};

use crate::console::sections::{Section, Tab};
use crate::users::{Caller, User};
use crate::{Call, Capability, Context, Footprint, Serve, Slot, arg, unknown};
use class::{Class, Offered, class_for};
use config::{Job, Needs};
use route::Availability;
use state::Report;

/// A connector's or reader's work in flight.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

/// The scheduler's binding, and the one object of it.
pub const BINDING: &str = "CI_SCHEDULER";
pub const OBJECT: &str = "scheduler";
/// The queue binding state changes are sent to.
pub const EVENTS: &str = "CI_EVENTS";
/// The queue's name.
pub const EVENTS_QUEUE: &str = "pigeon-ci-events";

/// The longest task label a job carries; a longer one is cut.
const MAX_TASK_CHARS: usize = 100;
/// Why nothing can run while no connector is set up.
const NO_CONNECTORS: &str = "no CI connector is set up yet";
/// The largest webhook body read.
const MAX_HOOK_BYTES: usize = 1 << 20;

/// What a connector acts with.
pub struct Cx<'a> {
    pub env: &'a Env,
    /// The platform's hostname, such as `platform.pigeon.dev`.
    pub host: &'a str,
}

/// One attempt of a job, as a connector is asked to run it.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub job: String,
    pub attempt: u32,
    /// `namespace/name`.
    pub repo: String,
    pub commit: String,
    /// The job's name in `ci.toml`.
    pub name: String,
    /// The script to run, from the repository's root.
    pub script: String,
    pub class: Class,
    pub timeout_min: u32,
    /// The job token, for connectors whose runner takes it in its environment.
    #[serde(skip)]
    pub token: Option<String>,
    /// Where the runner reports: `https://{host}/ci/runner/{job}`.
    pub runner_url: String,
    /// Where the runner fetches: `https://{proxy_host}/git/{repo}.git`.
    pub git_url: String,
}

/// A run a connector has accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatched {
    /// The connector's own id for it, such as a build number.
    pub handle: String,
    /// Where a person can watch it.
    pub url: Option<String>,
}

/// A webhook delivery, as received.
pub struct Hook {
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Hook {
    /// The value of header `name` (lowercase), if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What a verified webhook says of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub handle: String,
    pub report: Report,
    pub url: Option<String>,
}

/// A CI system the gateway can send jobs to.
pub trait Connector {
    /// Its name, such as `containers`: the `connector` callers pin and `/ci/hooks/{name}`.
    fn name(&self) -> &'static str;

    /// The classes it can run, with their real limits.
    fn offers(&self) -> Vec<Offered>;

    /// The secrets it needs set, declared on the secrets page.
    fn secrets(&self) -> Vec<Slot> {
        Vec::new()
    }

    /// Whether its runner takes the job token in its environment; otherwise the runner
    /// exchanges its own OIDC token for one at `/ci/token`.
    fn takes_token(&self) -> bool {
        false
    }

    /// The most of its jobs that may be live at once, which the scheduler enforces; `None` for
    /// no limit of the gateway's own.
    fn limit(&self) -> Option<u32> {
        None
    }

    /// Whether it is configured (its secrets set) and healthy, and how much room it has.
    fn available<'a>(&'a self, cx: &'a Cx<'a>) -> Pending<'a, Availability>;

    /// Starts `run`. An error is an infrastructure failure: the job may go elsewhere.
    fn dispatch<'a>(&'a self, cx: &'a Cx<'a>, run: &'a Run) -> Pending<'a, Dispatched>;

    /// What it knows of run `handle`, if anything has happened.
    fn status<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, Option<Report>>;

    /// Stops run `handle`.
    fn cancel<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, ()>;

    /// Verifies an OIDC token a runner presents at `/ci/token` and answers the handle of the run
    /// it vouches for; `None` when this connector's runners do not exchange tokens.
    fn exchange<'a>(&'a self, _cx: &'a Cx<'a>, _jwt: &'a str) -> Pending<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }

    /// Verifies `hook` before anything in it is read, then answers what it says of a run;
    /// `None` when it is verified but not about a run. An error means unverified.
    fn webhook<'a>(&'a self, cx: &'a Cx<'a>, hook: &'a Hook) -> Pending<'a, Option<Seen>>;
}

/// Reads a file of a repository at a commit.
pub trait Files {
    /// The text of `path` at `commit` of `repo`, or `None` when it is not there.
    fn read<'a>(
        &'a self,
        cx: &'a Cx<'a>,
        repo: &'a str,
        commit: &'a str,
        path: &'a str,
    ) -> Pending<'a, Option<String>>;
}

/// Files read through the git proxy's reader: the one commit, one deep, with a git token
/// minted for the read and revoked after.
pub struct ProxyFiles {
    /// The Secrets Store binding of the parent token the git token is minted from.
    pub parent: String,
}

/// The longest file read, such as `ci.toml`.
const MAX_FILE_BYTES: usize = 64 * 1024;

impl Files for ProxyFiles {
    fn read<'a>(
        &'a self,
        cx: &'a Cx<'a>,
        repo: &'a str,
        commit: &'a str,
        path: &'a str,
    ) -> Pending<'a, Option<String>> {
        Box::pin(async move {
            let (namespace, name) = repo
                .split_once('/')
                .ok_or_else(|| bad(&format!("not a repository: {repo}")))?;
            let account = scheduler::account(cx.env, &self.parent).await?;
            let Some(bytes) =
                crate::git_proxy::read_file(&account, namespace, name, commit, path).await?
            else {
                return Ok(None);
            };
            if bytes.len() > MAX_FILE_BYTES {
                return Err(bad(&format!(
                    "{path} is larger than {} KiB",
                    MAX_FILE_BYTES / 1024
                )));
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| bad(&format!("{path} is not UTF-8")))
        })
    }
}

/// The gateway's settings: the connectors in the operator's order, and the rest.
pub struct Settings {
    /// Best first.
    pub connectors: Vec<Box<dyn Connector>>,
    pub files: Box<dyn Files>,
    /// The repositories CI may run, as `namespace/name`.
    pub repos: Vec<String>,
    /// The platform's hostname: runners report here.
    pub host: String,
    /// The proxies' hostname: runners fetch through its git proxy.
    pub proxy_host: String,
    /// The Secrets Store binding of the parent token, for the notes push.
    pub parent: String,
    /// How long a queued job waits for its first-ranked connector before any other will do.
    pub wait_limit_ms: i64,
}

impl Settings {
    fn connector(&self, name: &str) -> Option<&dyn Connector> {
        self.connectors
            .iter()
            .find(|c| c.name() == name)
            .map(|c| c.as_ref())
    }
}

/// The CI capability.
pub struct Ci {
    pub settings: Settings,
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// Whether `s` is a full commit id.
fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The job record a submit creates, before the scheduler has it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewJob {
    pub id: String,
    pub repo: String,
    pub commit: String,
    pub name: String,
    pub job: Job,
    pub class: String,
    pub pin: Option<String>,
    pub task: Option<String>,
    pub caller: Option<String>,
    pub caller_name: Option<String>,
    pub notify_url: Option<String>,
    pub notify_key: Option<String>,
}

/// The caller's tags: who, the person they act for when linked, and the task, cut short.
fn tags(
    caller: Option<&Caller>,
    task: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let (id, name) = match caller {
        Some(c) => {
            let name = match &c.acting_for {
                Some(f) => format!("{} for {}", c.user.name, f.name),
                None => c.user.name.clone(),
            };
            (Some(c.user.id.clone()), Some(name))
        }
        None => (None, None),
    };
    let task = task
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(MAX_TASK_CHARS).collect());
    (id, name, task)
}

/// Calls the scheduler.
async fn scheduler(env: &Env, op: &str, args: Value) -> Result<Value> {
    pigeon_core::durable::call(env, BINDING, OBJECT, op, &args).await
}

/// The grant of job token `presented`, if it is a live job token: the interface the git proxy
/// calls to accept a runner's fetch. The caller checks the route and the commit with
/// [`token::reaches`] and [`token::allows_fetch`].
pub async fn job_grant(env: &Env, presented: &str) -> Result<Option<token::Grant>> {
    let Some(hash) = token::lookup_hash(presented) else {
        return Ok(None);
    };
    let found = scheduler(env, "grant", json!({ "hash": hash })).await?;
    Ok(serde_json::from_value(found).unwrap_or(None))
}

impl Ci {
    fn cx<'a>(&'a self, env: &'a Env) -> Cx<'a> {
        Cx {
            env,
            host: &self.settings.host,
        }
    }

    fn repo<'a>(&self, args: &'a Value) -> Result<&'a str> {
        let repo = arg(args, "repo")?;
        if !self.settings.repos.iter().any(|r| r == repo) {
            return Err(bad(&format!(
                "CI runs only {}; not `{repo}`",
                self.settings.repos.join(", ")
            )));
        }
        Ok(repo)
    }

    fn commit(args: &Value) -> Result<&str> {
        let commit = arg(args, "commit")?;
        if !is_commit(commit) {
            return Err(bad("`commit` must be a full 40-character commit id"));
        }
        Ok(commit)
    }

    /// Job `name` of `repo`'s `ci.toml` at `commit`.
    async fn job(&self, env: &Env, repo: &str, commit: &str, name: &str) -> Result<Job> {
        config::job_name(name).map_err(|e| bad(&e))?;
        let cx = self.cx(env);
        let text = self
            .settings
            .files
            .read(&cx, repo, commit, config::PATH)
            .await?
            .ok_or_else(|| bad(&format!("{repo} has no {} at {commit}", config::PATH)))?;
        let mut cfg = config::parse(&text).map_err(|e| bad(&e))?;
        cfg.jobs.remove(name).ok_or_else(|| {
            let names: Vec<&String> = cfg.jobs.keys().collect();
            bad(&format!(
                "{} at {commit} has no job `{name}`; it has {names:?}",
                config::PATH
            ))
        })
    }

    /// Each connector's offers and availability, as the scheduler last saw them.
    async fn pool(&self, env: &Env) -> Result<Vec<(String, Vec<Offered>, Availability)>> {
        let seen = scheduler(env, "pool", json!({})).await?;
        Ok(self
            .settings
            .connectors
            .iter()
            .map(|c| {
                let a = seen[c.name()]
                    .as_object()
                    .and_then(|_| serde_json::from_value(seen[c.name()].clone()).ok())
                    .unwrap_or_default();
                (c.name().to_string(), c.offers(), a)
            })
            .collect())
    }

    /// Routes a job, dispatching nothing.
    async fn route(
        &self,
        env: &Env,
        needs: &Needs,
        timeout_min: u32,
        pin: Option<&str>,
    ) -> Result<std::result::Result<route::Routed, Vec<(String, String)>>> {
        let pool = self.pool(env).await?;
        let candidates: Vec<route::Candidate> = pool
            .iter()
            .enumerate()
            .map(|(i, (name, offers, a))| route::Candidate {
                name,
                rank: i as u32,
                offers,
                availability: *a,
            })
            .collect();
        let ask = route::Ask {
            needs,
            timeout_min,
            pin,
            exclude: &[],
        };
        Ok(route::route(&ask, &candidates))
    }

    async fn ci_route(&self, env: &Env, args: &Value) -> Result<Value> {
        let (needs, timeout_min, job) = if args.get("needs").is_some_and(Value::is_object) {
            let mut needs = serde_json::to_value(Needs::default())?;
            for (k, v) in args["needs"].as_object().into_iter().flatten() {
                if needs.get(k).is_none() {
                    return Err(bad(&format!("`needs` has no `{k}`")));
                }
                needs[k] = v.clone();
            }
            let needs: Needs =
                serde_json::from_value(needs).map_err(|e| bad(&format!("`needs`: {e}")))?;
            config::check_needs(&needs).map_err(|e| bad(&e))?;
            let timeout = args["timeout_min"]
                .as_u64()
                .unwrap_or(u64::from(config::DEFAULT_TIMEOUT_MIN));
            if !(1..=u64::from(config::MAX_TIMEOUT_MIN)).contains(&timeout) {
                return Err(bad("`timeout_min` must be from 1 to 360"));
            }
            (needs, timeout as u32, None)
        } else {
            let repo = self.repo(args)?;
            let commit = Self::commit(args)?;
            let job = self.job(env, repo, commit, arg(args, "job")?).await?;
            (job.needs.clone(), job.timeout_min, Some(job))
        };
        let class = class_for(&needs).map_err(|e| bad(&e))?;
        let routed = self
            .route(env, &needs, timeout_min, args["connector"].as_str())
            .await?;
        Ok(match routed {
            Ok(r) => json!({
                "class": class.name(), "needs": needs, "timeout_min": timeout_min, "job": job,
                "would_go_to": route::choose(&r, false).map(|p| &p.connector),
                "ranked": r.ranked, "ruled_out": reasons(&r.ruled_out),
            }),
            Err(why) => json!({
                "class": class.name(), "needs": needs, "timeout_min": timeout_min, "job": job,
                "refused": true, "ruled_out": reasons(&why),
                "note": why.is_empty().then_some(NO_CONNECTORS),
            }),
        })
    }

    async fn ci_submit(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let repo = self.repo(args)?;
        let commit = Self::commit(args)?.to_ascii_lowercase();
        let name = arg(args, "job")?;
        let pin = args["connector"].as_str();
        if let Some(p) = pin
            && self.settings.connector(p).is_none()
        {
            return Err(bad(&format!("no connector `{p}`")));
        }
        let notify_url = match args["notify"].as_str() {
            Some(u) => Some(notify::check_url(u, &self.settings.host).map_err(|e| bad(&e))?),
            None => None,
        };
        let job = self.job(cx.env, repo, &commit, name).await?;
        let class = class_for(&job.needs).map_err(|e| bad(&e))?;
        // A job nothing could run now is refused at once.
        if let Err(why) = self.route(cx.env, &job.needs, job.timeout_min, pin).await? {
            if why.is_empty() {
                return Err(bad(NO_CONNECTORS));
            }
            return Err(bad(&format!(
                "no connector can run `{name}` ({class}): {}",
                why.iter()
                    .map(|(c, r)| format!("{c}: {r}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
        let notify_key = match notify_url {
            Some(_) => Some(random()?),
            None => None,
        };
        let (caller, caller_name, task) = tags(cx.caller, args["task"].as_str());
        let new = NewJob {
            id: random()?[..22].to_string(),
            repo: repo.into(),
            commit,
            name: name.into(),
            job,
            class: class.name(),
            pin: pin.map(String::from),
            task,
            caller,
            caller_name,
            notify_url,
            notify_key: notify_key.clone(),
        };
        let mut out = scheduler(cx.env, "submit", serde_json::to_value(&new)?).await?;
        if let Some(k) = notify_key {
            out["notify_key"] = json!(k);
            out["notify_key_note"] = json!(
                "Shown once. Each POST to `notify` carries `Pigeon-Signature: t=<unix seconds>,\
                 v1=<hex HMAC-SHA256(key, \"<t>.<body>\")>`, the key being these characters as \
                 they are."
            );
        }
        Ok(out)
    }

    /// `/ci/hooks/{connector}`: verified by the connector before anything in it is read.
    async fn hook(&self, env: &Env, mut req: Request, name: &str) -> Result<Response> {
        let Some(c) = self.settings.connector(name) else {
            return Response::error("not found", 404);
        };
        if req.method() != Method::Post {
            return Response::error("POST only", 405);
        }
        let body = req.bytes().await?;
        if body.len() > MAX_HOOK_BYTES {
            return Response::error("too large", 413);
        }
        let headers = req
            .headers()
            .entries()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        let hook = Hook { headers, body };
        let cx = self.cx(env);
        let seen = match c.webhook(&cx, &hook).await {
            Ok(seen) => seen,
            Err(e) => {
                // The reason, never a header or the body.
                worker::console_error!("ci hook {name}: refused: {e}");
                return Response::error("unverified", 401);
            }
        };
        if let Some(seen) = seen {
            scheduler(env, "seen", json!({ "connector": name, "seen": seen })).await?;
        }
        Response::ok("ok")
    }

    /// `/ci/runner/{job}`: a runner's report, with its job token.
    async fn runner(&self, env: &Env, mut req: Request, job: &str) -> Result<Response> {
        if req.method() != Method::Post {
            return Response::error("POST only", 405);
        }
        let presented = req
            .headers()
            .get("Authorization")
            .ok()
            .flatten()
            .and_then(|h| {
                let (scheme, rest) = h.trim().split_once(' ')?;
                scheme
                    .eq_ignore_ascii_case("bearer")
                    .then(|| rest.trim().to_string())
            });
        let Some(hash) = presented.as_deref().and_then(token::lookup_hash) else {
            return Response::error("a job token is required", 401);
        };
        let refused =
            || Response::error("the job token is unknown, expired or not this job's", 401);
        // The token is checked before the body is read, so a caller without one learns
        // nothing from what the body gets wrong.
        if !token::reaches(&req.path()) {
            return refused();
        }
        let grant: Option<token::Grant> =
            serde_json::from_value(scheduler(env, "grant", json!({ "hash": hash })).await?)
                .unwrap_or(None);
        let now = worker::js_sys::Date::now() as i64;
        if !grant.is_some_and(|g| token::allows_job(&g, now, job)) {
            return refused();
        }
        let body: Value = req.json().await.unwrap_or(Value::Null);
        let report = match runner_report(&body) {
            Ok(r) => r,
            Err(why) => return Response::error(why, 400),
        };
        // What the script exited with and printed, with a final report only.
        let (exit_code, output) = match report {
            Report::Finished { .. } => (
                body["exit_code"].as_i64().filter(|c| (0..=255).contains(c)),
                output::keep(body["output_b64"].as_str()),
            ),
            _ => (None, None),
        };
        let detail: Option<String> = body["detail"]
            .as_str()
            .map(|d| d.chars().take(500).collect());
        let answer = scheduler(
            env,
            "runner",
            json!({
                "job": job, "hash": hash, "report": report, "detail": detail,
                "exit_code": exit_code, "output": output,
            }),
        )
        .await?;
        if answer.is_null() {
            return refused();
        }
        Response::from_json(&answer)
    }

    /// `/ci/token`: a runner's OIDC token, verified by the connector that issued its run,
    /// exchanged for a job token for that run's job only. The job token is answered once, as
    /// plain text, and kept only as its hash.
    async fn token(&self, env: &Env, mut req: Request) -> Result<Response> {
        let refused = || Response::error("the token is refused", 401);
        let Some(jwt) = bearer(&req) else {
            return refused();
        };
        let body: Value = req.json().await.unwrap_or(Value::Null);
        let Some(job) = body["job"]
            .as_str()
            .filter(|j| token::reaches(&format!("/ci/runner/{j}")))
        else {
            return Response::error("`job` is required", 400);
        };
        let cx = self.cx(env);
        for c in &self.settings.connectors {
            let handle = match c.exchange(&cx, &jwt).await {
                Ok(Some(h)) => h,
                Ok(None) => continue,
                Err(e) => {
                    // The reason, never the token.
                    worker::console_error!("ci token exchange ({}): {e}", c.name());
                    return refused();
                }
            };
            let minted = scheduler(
                env,
                "exchange",
                json!({ "connector": c.name(), "handle": handle, "job": job }),
            )
            .await?;
            let Some(token) = minted.as_str() else {
                worker::console_error!(
                    "ci token exchange ({}): no live job {job} on {handle}",
                    c.name()
                );
                return refused();
            };
            let mut res = Response::ok(token)?;
            res.headers_mut().set("Cache-Control", "no-store")?;
            return Ok(res);
        }
        refused()
    }

    async fn serve(&self, cx: &Context<'_>, req: Request) -> Result<Response> {
        let path = req.path();
        if let Some(name) = path.strip_prefix("/ci/hooks/") {
            let name = name.to_string();
            return self.hook(cx.env, req, &name).await;
        }
        if let Some(job) = path.strip_prefix("/ci/runner/") {
            let job = job.to_string();
            return self.runner(cx.env, req, &job).await;
        }
        match (req.method(), path.as_str()) {
            (Method::Post, "/ci/token") => self.token(cx.env, req).await,
            (Method::Get, "/ci/buildkite.sh") => script(BUILDKITE_BOOTSTRAP),
            (Method::Get, "/ci/runner.sh") => script(RUNNER),
            _ => Response::error("not found", 404),
        }
    }
}

/// The runner, served to Buildkite's bootstrap.
const RUNNER: &str = include_str!("../../../ci/runner/entrypoint.sh");
/// The bootstrap a Buildkite step runs: exchange its OIDC token, then run the runner.
const BUILDKITE_BOOTSTRAP: &str = include_str!("../../../ci/runner/buildkite.sh");

/// A shell script, served as text that is never cached.
fn script(text: &str) -> Result<Response> {
    let mut res = Response::ok(text)?;
    let h = res.headers_mut();
    h.set("Content-Type", "text/x-shellscript; charset=utf-8")?;
    h.set("Cache-Control", "no-store")?;
    h.set("X-Content-Type-Options", "nosniff")?;
    Ok(res)
}

/// The bearer token `req` carries.
fn bearer(req: &Request) -> Option<String> {
    let h = req.headers().get("Authorization").ok().flatten()?;
    let (scheme, rest) = h.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| rest.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// What a runner's report body says, once its token has been checked.
fn runner_report(body: &Value) -> std::result::Result<Report, &'static str> {
    match body["state"].as_str() {
        Some("running") => Ok(Report::Started),
        Some("finished") => match body["passed"].as_bool() {
            Some(passed) => Ok(Report::Finished { passed }),
            None => Err("`passed` must be true or false"),
        },
        _ => Err("`state` must be running or finished"),
    }
}

/// `(connector, reason)` pairs as JSON objects.
fn reasons(list: &[(String, String)]) -> Vec<Value> {
    list.iter()
        .map(|(c, r)| json!({ "connector": c, "reason": r }))
        .collect()
}

impl Capability for Ci {
    fn name(&self) -> &'static str {
        "ci"
    }

    /// CI's place in the console. It reports no cost: no connector says what a run cost.
    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Gateways,
            id: "ci",
            title: "CI",
            description: "Runs routed to CI connectors.",
            order: 2,
            cost: None,
        }]
    }

    fn section_data<'a>(
        &'a self,
        _cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async {
            Ok(
                json!({ "html": "<p class=\"muted\">The CI gateway records which connector ran each job and for how long, but not what it cost, so CI is not in the total yet.</p>" }),
            )
        })
    }

    fn tools(&self) -> Vec<Tool> {
        let names: Vec<&str> = self.settings.connectors.iter().map(|c| c.name()).collect();
        let job = json!({
            "repo": { "type": "string", "description": "`namespace/name`, such as `pigeon-security/pigeon`." },
            "commit": { "type": "string", "description": "A full 40-character commit id." },
            "job": { "type": "string", "description": "A job named in `.pigeon/ci.toml` at that commit." },
            "connector": { "type": "string", "enum": names, "description": "Pin the job to this connector." },
        });
        vec![
            Tool::bare(
                "ci_pool",
                "The CI connectors in the operator's order: the runner classes each offers, \
                 whether it is configured and healthy, its free slots and its queue.",
            ),
            Tool {
                name: "ci_route",
                description: "Where a job would run, dispatching nothing: its runner class, the \
                              connectors that could run it best first, and why each other one \
                              cannot. Give `repo`, `commit` and `job`, or `needs` (and \
                              `timeout_min`) directly.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "repo": job["repo"], "commit": job["commit"], "job": job["job"],
                        "connector": job["connector"],
                        "needs": { "type": "object", "description": "os, arch, cpu, memory_gb, disk_gb, docker, gpu, network (egress or none); small Linux defaults." },
                        "timeout_min": { "type": "integer" }
                    }
                }),
            },
            Tool {
                name: "ci_submit",
                description: "Run a job from `.pigeon/ci.toml` at a commit. Answers the job's id \
                              and state; with `notify`, also the key its POSTs are signed with, \
                              shown once. A job nothing could run is refused at once, with each \
                              connector's reason.",
                input_schema: json!({
                    "type": "object",
                    "required": ["repo", "commit", "job"],
                    "properties": {
                        "repo": job["repo"], "commit": job["commit"], "job": job["job"],
                        "connector": job["connector"],
                        "task": { "type": "string", "description": "A short label for what the run is for; 100 characters at most." },
                        "notify": { "type": "string", "description": "An https URL to POST each state change to, signed." }
                    }
                }),
            },
            Tool {
                name: "ci_status",
                description: "One job: its state, connector, attempts, link and every change.",
                input_schema: json!({
                    "type": "object", "required": ["id"],
                    "properties": { "id": { "type": "string" } }
                }),
            },
            Tool {
                name: "ci_jobs",
                description: "Recent jobs, newest first, filtered by any of `repo`, `commit`, \
                              `state`, `caller` (id or name) and `task`.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "repo": { "type": "string" }, "commit": { "type": "string" },
                        "state": { "type": "string", "enum": ["queued", "dispatched", "running", "succeeded", "failed", "cancelled", "timed_out", "error"] },
                        "caller": { "type": "string" }, "task": { "type": "string" },
                        "limit": { "type": "integer", "description": "50 by default, 200 at most." }
                    }
                }),
            },
            Tool {
                name: "ci_cancel",
                description: "Cancel a job that has not finished, at its connector too.",
                input_schema: json!({
                    "type": "object", "required": ["id"],
                    "properties": { "id": { "type": "string" } }
                }),
            },
            Tool {
                name: "ci_commit",
                description: "The CI results recorded for a commit: each job's final state, as \
                              the git note under `refs/notes/ci` says, and whether the note has \
                              been pushed.",
                input_schema: json!({
                    "type": "object", "required": ["repo", "commit"],
                    "properties": { "repo": job["repo"], "commit": job["commit"] }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "ci_pool" => {
                    let pool = self.pool(cx.env).await?;
                    json!({ "connectors": pool.iter().map(|(name, offers, a)| json!({
                        "connector": name,
                        "classes": offers.iter().map(|o| json!({
                            "class": o.class.name(), "cpu": o.cpu, "memory_gb": o.memory_gb,
                            "disk_gb": o.disk_gb, "max_timeout_min": o.max_timeout_min,
                            "network_none": o.network_none,
                        })).collect::<Vec<_>>(),
                        "availability": a,
                    })).collect::<Vec<_>>() })
                }
                "ci_route" => self.ci_route(cx.env, args).await?,
                "ci_submit" => self.ci_submit(cx, args).await?,
                "ci_status" => {
                    scheduler(cx.env, "status", json!({ "id": arg(args, "id")? })).await?
                }
                "ci_jobs" => scheduler(cx.env, "jobs", args.clone()).await?,
                "ci_cancel" => {
                    scheduler(cx.env, "cancel", json!({ "id": arg(args, "id")? })).await?
                }
                "ci_commit" => {
                    let repo = self.repo(args)?;
                    let commit = Self::commit(args)?.to_ascii_lowercase();
                    scheduler(cx.env, "commit", json!({ "repo": repo, "commit": commit })).await?
                }
                other => return Err(unknown(other)),
            })
        })
    }

    fn routes(&self) -> Vec<&'static str> {
        vec!["/ci/"]
    }

    fn fetch<'a>(&'a self, cx: &'a Context<'a>, req: Request) -> Serve<'a> {
        Box::pin(self.serve(cx, req))
    }

    fn secrets(&self) -> Vec<Slot> {
        self.settings
            .connectors
            .iter()
            .flat_map(|c| c.secrets())
            .collect()
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            queues: vec![EVENTS_QUEUE.into()],
            objects: vec![BINDING.into()],
            ..Footprint::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::User;

    fn user(id: &str, name: &str) -> User {
        User {
            id: id.into(),
            kind: "standard".into(),
            name: name.into(),
            created_at: 0,
        }
    }

    #[test]
    fn a_job_is_tagged_with_its_caller_whom_they_act_for_and_its_task() {
        let c = Caller {
            user: user("s1", "bot"),
            acting_for: Some(user("u1", "ada")),
        };
        let (id, name, task) = tags(Some(&c), Some(" triage "));
        assert_eq!(id.as_deref(), Some("s1"));
        assert_eq!(name.as_deref(), Some("bot for ada"));
        assert_eq!(task.as_deref(), Some("triage"));
        assert_eq!(tags(None, Some("  ")), (None, None, None));
        let long = "x".repeat(500);
        assert_eq!(tags(None, Some(&long)).2.unwrap().len(), MAX_TASK_CHARS);
    }

    #[test]
    fn a_runner_reports_running_or_a_result() {
        assert_eq!(
            runner_report(&json!({ "state": "running" })),
            Ok(Report::Started)
        );
        assert_eq!(
            runner_report(&json!({ "state": "finished", "passed": false })),
            Ok(Report::Finished { passed: false })
        );
        assert!(runner_report(&json!({ "state": "finished" })).is_err());
        assert!(runner_report(&Value::Null).is_err());
    }

    #[test]
    fn only_full_commit_ids_are_taken() {
        assert!(is_commit(&"a".repeat(40)));
        assert!(!is_commit("main"));
        assert!(!is_commit(&"g".repeat(40)));
    }

    #[test]
    fn hook_headers_are_found_by_lowercase_name() {
        let h = Hook {
            headers: vec![("x-buildkite-token".into(), "t".into())],
            body: vec![],
        };
        assert_eq!(h.header("x-buildkite-token"), Some("t"));
        assert_eq!(h.header("x-other"), None);
    }
}

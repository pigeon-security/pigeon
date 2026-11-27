//! Landing: a session's branch to `main` and to the deployed platform, with no person in between
//! unless one is asked for. `land {branch}` answers at once, and the Lander's alarm checks the
//! branch's tip is a fast-forward of `main` under `refs/heads/claude/` or `refs/heads/codex/`,
//! the agents' namespaces ([`state::PREFIXES`]; a failed check ends it
//! `refused`), then runs CI job `test` at it, waits for a person's passkey approval
//! when the setting asks for one or the commit changes what the gate itself runs, moves `main`
//! to it through the platform's own git path, and deploys each Worker it affects in that
//! Worker's lane ([`lanes`]): the Worker's deploy job uploads it through the Cloudflare API
//! proxy with its own job token. Every step is reported by `land_status` and the result is
//! noted on the commit under `refs/notes/land`.
//!
//! Landings live in the `Lander`, a Durable Object with SQLite storage whose alarm runs while
//! a landing or a deploy is live; many landings are live at once. A deploy job's token reaches
//! `/cf/` only while its deploy is running, only for its commit and its Worker's script
//! ([`deploy_caller`]). Who may land: a person, or an agent linked to one.

pub mod digest;
pub mod lanes;
pub mod merge;
pub mod state;
pub mod watch;

use pigeon_core::artifacts::Access;
use pigeon_core::attribution;
use pigeon_core::durable::{self, Db, opt};
use pigeon_core::git::{self, Update, ZERO};
use pigeon_core::gitobj::{self, Object};
use pigeon_core::lanes::After;
use pigeon_core::mcp::Tool;
use pigeon_core::policy::{Decision, Kind};
use pigeon_core::webauthn::random;
use pigeon_proxies::git_proxy;
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{
    Env, Error, Request, Response, Result, SqlStorageValue, State as ObjectState, console_error,
    js_sys,
};

use crate::proxy::ProxyCaller;
use crate::users::Caller;
use crate::{Call, Capability, Context, External, Footprint, Hosted, arg, unknown};
use state::{Seen, State};

/// The Lander's binding, and the one object of it.
pub const BINDING: &str = "LANDER";
pub const OBJECT: &str = "lander";
/// The ref landing notes live under.
pub const NOTES_REF: &str = "refs/notes/land";
/// How many times a step that failed on the gateway's side is tried before the landing is an
/// error.
const STEP_TRIES: i64 = 5;
/// How many times a note push is tried.
const NOTE_TRIES: i64 = 10;
/// How many test refs one alarm deletes at most.
const TEST_REF_BATCH: usize = 5;
/// How many refused deletes of one test ref before it is left alone.
const TEST_REF_TRIES: i64 = 3;
/// The Worker the map requires, deployed before any other a landing affects.
const CORE_WORKER: &str = "core";

fn now() -> i64 {
    js_sys::Date::now() as i64
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

/// How long one source of `work_status` may take, in milliseconds.
const SOURCE_LIMIT_MS: u64 = 20_000;

/// `work`, or an error once [`SOURCE_LIMIT_MS`] has passed.
async fn within<T>(work: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    let limit = worker::Delay::from(std::time::Duration::from_millis(SOURCE_LIMIT_MS));
    futures_util::pin_mut!(work);
    futures_util::pin_mut!(limit);
    match futures_util::future::select(work, limit).await {
        futures_util::future::Either::Left((v, _)) => v,
        futures_util::future::Either::Right(_) => Err(bad(&format!(
            "no answer within {} s",
            SOURCE_LIMIT_MS / 1000
        ))),
    }
}

fn opt_i(v: Option<i64>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, Into::into)
}

/// Where landing happens: the repository, its protected branch and the CI jobs that gate it.
#[derive(Clone)]
pub struct Settings {
    pub namespace: String,
    pub repo: String,
    /// The branch landings move, such as `refs/heads/main`.
    pub main: String,
    /// The Secrets Store binding of the parent token.
    pub parent: String,
    /// The CI jobs, in `.pigeon/ci.toml`, that test and deploy a commit.
    pub test_job: String,
    pub deploy_job: String,
    /// The CI connector the deploy job is pinned to: one whose runner can hand the job its
    /// token, such as `containers`.
    pub deploy_connector: Option<String>,
    /// The MCP server portal servers (their server IDs, such as `pigeon`) to sync once a
    /// landing has deployed, so the portal lists the deployed tools at once.
    pub portal_servers: Vec<String>,
    /// The portal's hostname, such as `mcp.pigeon.dev`, for the architecture page.
    pub portal_host: String,
    /// Whether the CI connectors' setup (Buildkite's: docs/plans/buildkite-mcp.md) runs once a
    /// landing has deployed, so the provider matches the deployed settings.
    pub setup_connectors: bool,
    /// The platform Workers and the paths each owns (ci/workers.toml, as this Worker was built
    /// with): which Workers a landing deploys, in which order, with which job.
    pub map: pigeon_core::lanes::Map,
}

impl Settings {
    fn repo_name(&self) -> String {
        format!("{}/{}", self.namespace, self.repo)
    }
}

/// The `land` capability: its tools and its console section.
pub struct Land {
    pub settings: Settings,
}

/// Calls the Lander.
async fn lander(env: &Env, op: &str, args: Value) -> Result<Value> {
    durable::call(env, BINDING, OBJECT, op, &args).await
}

/// The deploys that finished at or after `since_ms`, newest first, at most 50: read-only.
pub async fn deploys_since(env: &Env, since_ms: i64) -> Result<Vec<Value>> {
    let v = lander(env, "deploys_since", json!({ "since_ms": since_ms })).await?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

/// The landings waiting for a person's passkey, newest first. One is live at a time, so the
/// latest few hold every one.
pub async fn awaiting(env: &Env) -> Result<Vec<Value>> {
    let recent = lander(env, "list", json!({ "limit": 20 })).await?;
    Ok(recent
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|l| l["state"] == "awaiting_approval")
                .cloned()
                .collect()
        })
        .unwrap_or_default())
}

/// Approves landing `id` for `person`, a signed-in person who passed a passkey bound to it: the
/// console's write.
pub async fn approve(env: &Env, id: &str, person: &str) -> Result<Value> {
    lander(env, "approve", json!({ "id": id, "person": person })).await
}

/// Sets whether every landing waits for a person's approval, for `person`: the console's write.
pub async fn set_approval(env: &Env, on: bool, person: &str) -> Result<Value> {
    lander(env, "approval_set", json!({ "on": on, "person": person })).await
}

/// Whether `caller` may land: a person, or an agent acting for one.
pub fn may_land(caller: &Caller) -> bool {
    caller.user.kind == "standard" || caller.acting_for.is_some()
}

/// The commit a landing expects at its branch's tip, from the `commit` argument: none, or a
/// full commit id, lowercased.
pub fn expected_commit(given: Option<&str>) -> std::result::Result<Option<String>, String> {
    match given {
        None | Some("") => Ok(None),
        Some(c) => {
            let lower = c.to_ascii_lowercase();
            if git::is_id(&lower) {
                Ok(Some(lower))
            } else {
                Err(format!(
                    "`commit` is a full 40-character commit id, not `{c}`"
                ))
            }
        }
    }
}

/// Why a landing of `branch` is refused given its `tip` and the commit `expected` there, if it
/// is.
pub fn tip_refusal(branch: &str, tip: Option<&str>, expected: Option<&str>) -> Option<String> {
    let Some(tip) = tip else {
        return Some(format!("{branch} does not exist; push it first"));
    };
    match expected {
        Some(e) if !e.eq_ignore_ascii_case(tip) => Some(format!(
            "{branch} is at {tip}, not {e}: push the commit to land, or name the tip"
        )),
        _ => None,
    }
}

/// The proxy caller a CI job token stands for at `/cf/`, when it is the job of a running deploy
/// in a Worker's lane, for the commit its grant names, scoped to that Worker's script; `None`
/// otherwise. The interface the Cloudflare API proxy calls.
pub async fn deploy_caller(env: &Env, presented: &str, repo: &str) -> Result<Option<ProxyCaller>> {
    let Some(grant) = crate::ci::job_grant(env, presented).await? else {
        return Ok(None);
    };
    if grant.repo != repo || worker::js_sys::Date::now() as i64 >= grant.expires_at {
        return Ok(None);
    }
    let found = lander(
        env,
        "deploy_grant",
        json!({ "job": grant.job, "commit": grant.commit }),
    )
    .await?;
    let Some(id) = found["id"].as_str() else {
        return Ok(None);
    };
    Ok(Some(ProxyCaller {
        branch_prefix: None,
        id: crate::ci::token::lookup_hash(presented)
            .map(|h| h[..crate::proxy::ID_LEN].to_string())
            .unwrap_or_default(),
        label: format!("land {id} deploy"),
        person: found["by"].as_str().unwrap_or("land").to_string(),
        script: found["script"].as_str().map(str::to_string),
    }))
}

impl Land {
    async fn land(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("landing needs a caller with a person behind it"))?;
        if !may_land(caller) {
            return Err(bad(
                "only a person, or an agent linked to one, can land: link this agent to its \
                 person first (users_link)",
            ));
        }
        let branch = state::branch_ref(arg(args, "branch")?).map_err(|e| bad(&e))?;
        let commit = expected_commit(args["commit"].as_str()).map_err(|e| bad(&e))?;
        let id = random()?[..16].to_string();
        let by = match &caller.acting_for {
            Some(f) => format!("{} for {}", caller.user.name, f.name),
            None => caller.user.name.clone(),
        };
        lander(
            cx.env,
            "create",
            json!({
                "id": id, "branch": branch, "commit": commit,
                "caller": caller, "by": by, "run": caller.run,
            }),
        )
        .await?;
        lander(cx.env, "status", json!({ "id": id })).await
    }

    /// `land_watch`: tells the calling run about a landing's next changes.
    async fn watch(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx.caller.ok_or_else(|| bad("no caller"))?;
        let Some(run) = caller.run.as_deref() else {
            return Err(bad(
                "land_watch is for AI agent runs: it tells the calling run about the landing",
            ));
        };
        let id = arg(args, "id")?;
        lander(
            cx.env,
            "watch",
            json!({ "id": id, "run": run, "caller": caller }),
        )
        .await
    }

    /// `work_status`: the compact digest, or one run or landing in full.
    async fn work_status(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        if let Some(detail) = args["detail"].as_str().filter(|d| !d.is_empty()) {
            return match digest::parse_detail(detail).map_err(|e| bad(&e))? {
                digest::Detail::Run(run) => {
                    crate::dispatch(cx, "ai_agents_status", &json!({ "run": run })).await
                }
                digest::Detail::Landing(id) => lander(cx.env, "status", json!({ "id": id })).await,
            };
        }
        let since = args["since"].as_i64();
        // Trimmed at the source: the digest shows a title line and never a report.
        let (runs_args, inbox_args) = (
            json!({ "limit": 100, "brief": "line", "report": false }),
            json!({ "since": 0, "brief": false, "report": false }),
        );
        let none = json!({});
        let landings_work = async {
            let live = lander(
                cx.env,
                "list",
                json!({ "limit": 50, "state": "live", "compact": true }),
            );
            let recent = lander(
                cx.env,
                "list",
                json!({ "limit": if since.is_some() { 50 } else { 8 }, "compact": true }),
            );
            let (live, recent) = futures_util::future::join(live, recent).await;
            Ok((live?, recent?))
        };
        let ((runs, landings, approvals, inbox, release), tokens) = futures_util::future::join(
            futures_util::future::join5(
                within(crate::dispatch(cx, "ai_agents_status", &runs_args)),
                within(landings_work),
                within(crate::dispatch(cx, "approvals_list", &none)),
                within(crate::dispatch(cx, "ai_agents_inbox", &inbox_args)),
                within(crate::dispatch(cx, "ios_releases", &none)),
            ),
            within(crate::ledger::gauge(cx.env)),
        )
        .await;
        let why = |r: Result<Value>| r.map_err(|e| e.to_string());
        let sources = digest::Sources {
            runs: why(runs),
            landings: landings.map_err(|e| e.to_string()),
            approvals: why(approvals),
            inbox: why(inbox),
            release: why(release),
            tokens: Some(
                tokens
                    .map(|g| crate::observability::gauge_view(&g))
                    .map_err(|e| e.to_string()),
            ),
        };
        Ok(digest::build(&sources, since))
    }

    async fn cancel(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx.caller.ok_or_else(|| bad("no caller"))?;
        if !may_land(caller) {
            return Err(bad(
                "only a person, or an agent linked to one, can cancel a landing",
            ));
        }
        let id = arg(args, "id")?;
        let cancelled = lander(
            cx.env,
            "cancel",
            json!({ "id": id, "by": caller.user.name }),
        )
        .await?;
        if let Some(job) = cancelled["test_job"].as_str() {
            // The landing is cancelled whatever CI says; its job is stopped too where it can be.
            if let Err(e) = crate::dispatch(cx, "ci_cancel", &json!({ "id": job })).await {
                console_error!("land: cancelling test job {job}: {e}");
            }
        }
        lander(cx.env, "status", json!({ "id": id })).await
    }
}

impl Capability for Land {
    fn name(&self) -> &'static str {
        "land"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "land",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "land",
                description: "Land a session branch under claude/ or codex/ (push it first \
                              with branch_push, or git through the proxy). Answers at once with the \
                              landing in `checking`, and many landings run at once: each is \
                              checked (its base on main, what it changed, whether a person \
                              must approve) and tested on its own, outside any lock; a failed \
                              check ends it `refused` with the reason. Once tested (and \
                              approved where needed) it is `ready` for the merge lock: main \
                              moves to it by compare-and-swap, and when main moved meanwhile \
                              the Lander rebases the branch itself (tested again only when \
                              main moved in what its tests cover; `rebase_refused` when both \
                              changed the same file in ways that do not merge). Then it is \
                              `merged`, and each Worker it affects deploys in its own lane \
                              (one deploy per Worker at a time, Workers in parallel, core \
                              first when it is affected), the newest merged commit for that \
                              Worker; it is `landed` once each has deployed it. \
                              A docs-only landing needs no tests and no deploy. Never \
                              force-push to make a landing fit. Follow it with land_status, \
                              which shows the queue.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "branch": { "type": "string", "description": "Such as `claude/my-change` or `codex/my-change`." },
                        "commit": { "type": "string", "description": "The commit expected at the branch's tip; refused if the tip differs." }
                    },
                    "required": ["branch"]
                }),
            },
            Tool {
                name: "land_status",
                description: "A landing (the latest when no id is given): its state (checking, \
                              testing, awaiting_approval, merging, deploying, landed, or how \
                              it stopped: refused, test_failed, …), each step with its time \
                              and detail, its CI jobs, its approval, and whether its note is \
                              on the commit. With `compact: true` a few KB only: the landing \
                              row, its last step, its incidents and the queue.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "compact": { "type": "boolean", "description": "A short summary instead of the full answer. Default false." }
                    }
                }),
            },
            Tool {
                name: "land_list",
                description: "Recent landings, newest first, as compact rows (id, branch, \
                              commit, state, workers, incident, detail, updated_at). Call \
                              land_status with an id for more.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Default 10." },
                        "state": { "type": "string", "description": "A state name, `live` or `ended`." }
                    }
                }),
            },
            Tool {
                name: "land_watch",
                description: "For AI agent runs: tell the calling run, as a note, when this \
                              landing awaits approval, merges, lands or ends, or an incident \
                              includes it. Answers the compact landing row. `land` already \
                              does this for the landing it starts.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool {
                name: "work_status",
                description: "One compact digest (under about 8 KB) of the work in flight: \
                              live runs with cost and what they wait for, the landing queue \
                              and recent landings, what waits for the person, and the latest \
                              iOS release. A source that fails is listed under `unavailable`. \
                              `since` (ms) also shows runs ended and landings updated after \
                              it. `detail` is `run:<id>` or `landing:<id>` for that one item \
                              in full.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "since": { "type": "integer", "description": "Milliseconds since the epoch." },
                        "detail": { "type": "string", "description": "`run:<id>` or `landing:<id>`." }
                    }
                }),
            },
            Tool {
                name: "land_cancel",
                description: "Cancel a landing that has not touched main yet (checking, \
                              testing or awaiting approval), and its test job.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool::bare(
                "portal_sync",
                "Sync the MCP server portal now, as a landing does once it has deployed: each \
                 server's state afterwards (`ready`, `waiting`, `stale` or `error`), or why it \
                 could not be synced.",
            ),
            Tool {
                name: "portal_oauth_redirects",
                description: "List, add or remove ONE exact OAuth redirect URI on the MCP \
                              Server Portal's own Access application. One exact `https://` \
                              URI, never a wildcard. `list` is read-only. `add` and `remove` \
                              plan the change and create an approval; nothing changes until \
                              the person approves it, then portal_oauth_redirects_apply runs \
                              it. The Access policy, dynamic client registration, PKCE and \
                              grants are not changed, and no other application is touched.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["list", "add", "remove"] },
                        "uri": { "type": "string", "description": "One exact https:// redirect URI, no wildcard." }
                    },
                    "required": ["action"]
                }),
            },
            Tool {
                name: "portal_oauth_redirects_apply",
                description: "Apply an approved portal_oauth_redirects plan: re-reads the \
                              portal's Access application, changes only its one exact \
                              redirect URI, and answers the list and any other field that \
                              differs afterwards. The Access policy, DCR, PKCE and grants are \
                              not changed; only the portal's own application is touched.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "plan": { "type": "string", "description": "The approval id." } },
                    "required": ["plan"]
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "land" => self.land(cx, args).await,
                "land_status" => {
                    lander(
                        cx.env,
                        "status",
                        json!({ "id": args["id"].as_str(), "compact": args["compact"] == true }),
                    )
                    .await
                }
                "land_list" => {
                    digest::state_filter(args["state"].as_str()).map_err(|e| bad(&e))?;
                    lander(
                        cx.env,
                        "list",
                        json!({
                            "limit": args["limit"].as_i64().unwrap_or(10).clamp(1, 50),
                            "state": args["state"].as_str(), "compact": true,
                        }),
                    )
                    .await
                }
                "land_watch" => self.watch(cx, args).await,
                "work_status" => self.work_status(cx, args).await,
                "land_cancel" => self.cancel(cx, args).await,
                "portal_sync" => {
                    let mut out = serde_json::Map::new();
                    for server in &self.settings.portal_servers {
                        let v = match pigeon_core::mcp_portal::sync(cx.account, server).await {
                            Ok(v) => v,
                            Err(e) => json!({ "status": "not synced", "why": e.to_string() }),
                        };
                        out.insert(server.clone(), v);
                    }
                    Ok(Value::Object(out))
                }
                "portal_oauth_redirects" => {
                    crate::portal_oauth::redirects(cx, &self.settings.portal_host, args).await
                }
                "portal_oauth_redirects_apply" => {
                    crate::portal_oauth::apply(cx, &self.settings.portal_host, args).await
                }
                other => Err(unknown(other)),
            }
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            repos: vec![self.settings.repo_name()],
            externals: if self.settings.portal_servers.is_empty() {
                Vec::new()
            } else {
                vec![
                    External::new(
                        "MCP Server Portal",
                        "portal",
                        format!(
                            "{}, servers {}",
                            self.settings.portal_host,
                            self.settings.portal_servers.join(", ")
                        ),
                    )
                    .place("account")
                    .flows(
                        "server sync after each deploy",
                        "MCP clients at the portal host",
                    ),
                ]
            },
            ..Footprint::default()
        }
    }
}

// ---- the Lander ----

/// The Lander's schema, one statement per migration, applied in order and never edited.
const MIGRATIONS: [&str; 25] = [
    "CREATE TABLE landings (id TEXT PRIMARY KEY, branch TEXT NOT NULL, commit_sha TEXT NOT NULL, main_at TEXT NOT NULL, caller TEXT NOT NULL, by_name TEXT NOT NULL, state TEXT NOT NULL, approval_reason TEXT, approved_by TEXT, awaiting_since INTEGER, test_job TEXT, deploy_job TEXT, tries INTEGER NOT NULL DEFAULT 0, detail TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER, note_pushed INTEGER NOT NULL DEFAULT 0, note_tries INTEGER NOT NULL DEFAULT 0)",
    "CREATE TABLE events (landing TEXT NOT NULL, at INTEGER NOT NULL, state TEXT NOT NULL, detail TEXT)",
    "CREATE TABLE notes (commit_sha TEXT PRIMARY KEY, text TEXT NOT NULL)",
    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, by_name TEXT, at INTEGER NOT NULL)",
    // Landing in lanes (docs/plans/workers-split.md): the branch's base on main, the commit its
    // tests last passed at, whether it changes docs alone, and when it reached main.
    "ALTER TABLE landings ADD COLUMN base TEXT",
    "ALTER TABLE landings ADD COLUMN tested TEXT",
    "ALTER TABLE landings ADD COLUMN docs_only INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE landings ADD COLUMN merged_at INTEGER",
    // Deploys per Worker: the Workers a landing affects (a JSON list of names), those its
    // deploy has still to deploy, in order, and the one deploying now.
    "ALTER TABLE landings ADD COLUMN workers TEXT",
    "ALTER TABLE landings ADD COLUMN deploys TEXT",
    "ALTER TABLE landings ADD COLUMN deploy_worker TEXT",
    // Deploy lanes, one per Worker (phase 5): each deploy of one Worker at one commit. `covers`
    // is the `merged_at` of the landing whose commit it deploys (`landing`): it carries every
    // landing for that Worker merged at or before, listed in `carries` (JSON). `state` is
    // running, succeeded, failed or error. Landings no longer use `deploys` and
    // `deploy_worker`, except one a Lander before lanes left deploying (`adopt_legacy`).
    "CREATE TABLE deploys (id INTEGER PRIMARY KEY AUTOINCREMENT, worker TEXT NOT NULL, landing TEXT NOT NULL, commit_sha TEXT NOT NULL, covers INTEGER NOT NULL, carries TEXT NOT NULL, job TEXT, state TEXT NOT NULL, tries INTEGER NOT NULL DEFAULT 0, detail TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER)",
    // Attribution: the landings a deploy included with their commits and merge times (JSON
    // `[{id, commit, merged_at}]`), why it ran (lane, rollback, bisect or restore) and the
    // incident it belongs to.
    "ALTER TABLE deploys ADD COLUMN included TEXT",
    "ALTER TABLE deploys ADD COLUMN purpose TEXT",
    "ALTER TABLE deploys ADD COLUMN incident INTEGER",
    // A lane's deploy that failed, being attributed (`lanes::bisect`): the deploy that failed,
    // the Worker's last good build before it (JSON `{commit, covers}`), the landings it
    // included, and how it ended: open, isolated or unisolated, the guilty landing, a summary.
    "CREATE TABLE incidents (id INTEGER PRIMARY KEY AUTOINCREMENT, worker TEXT NOT NULL, failed_deploy INTEGER NOT NULL, landing TEXT NOT NULL, last_good TEXT, included TEXT NOT NULL, state TEXT NOT NULL, guilty TEXT, summary TEXT, redeploys INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER)",
    // Whether the other lanes wait for core to deploy a landing first (the map's
    // `core_first`); landings from before default to waiting, as they all did.
    "ALTER TABLE landings ADD COLUMN ordered INTEGER NOT NULL DEFAULT 1",
    // The Worker's script a deploy resolved from the map at its commit; the deploy grant answers it.
    "ALTER TABLE deploys ADD COLUMN script TEXT",
    // AI agent runs that watch a landing, and the events still to tell them (`watch`).
    "CREATE TABLE watchers (landing TEXT, run TEXT, caller TEXT, PRIMARY KEY (landing, run))",
    "CREATE TABLE run_events (id INTEGER PRIMARY KEY AUTOINCREMENT, run TEXT, caller TEXT, text TEXT, tries INTEGER NOT NULL DEFAULT 0, at INTEGER)",
    // A deploy Cloudflare rate limited: how many times it was deployed again for that, and the
    // time before which its next job is not submitted (docs/plans/lander-fixes.md, fix 1).
    "ALTER TABLE deploys ADD COLUMN rate_retries INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE deploys ADD COLUMN not_before INTEGER",
    // The digest of a landing's change to the guarded paths, and the one its approval was given
    // for: an approval holds across the Lander's rebase only while they are equal (fix 2).
    "ALTER TABLE landings ADD COLUMN gate_digest TEXT",
    "ALTER TABLE landings ADD COLUMN approved_digest TEXT",
    // How many times its `test` job was submitted again after its runner was lost (fix 2).
    "ALTER TABLE landings ADD COLUMN test_retries INTEGER NOT NULL DEFAULT 0",
];

/// What the Lander needs from the hosting Worker, beside the settings: the capabilities it
/// runs CI's tools through, as `/mcp` would, and the sources they were built from.
pub struct Hosting {
    pub worker: &'static str,
    pub capabilities: fn() -> Vec<Box<dyn Capability>>,
    pub sources: &'static [(&'static str, &'static str)],
    /// The capability Workers bound to the hosting Worker, whose tools the Lander runs too
    /// (such as CI's, once CI is its own Worker).
    pub bindings: &'static [crate::remote::Binding],
}

#[derive(Deserialize, Debug, Clone)]
struct Row {
    id: String,
    branch: String,
    commit_sha: String,
    main_at: String,
    caller: String,
    by_name: String,
    state: String,
    approval_reason: Option<String>,
    approved_by: Option<String>,
    awaiting_since: Option<i64>,
    test_job: Option<String>,
    deploy_job: Option<String>,
    tries: i64,
    detail: Option<String>,
    created_at: i64,
    updated_at: i64,
    finished_at: Option<i64>,
    note_pushed: i64,
    note_tries: i64,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    tested: Option<String>,
    #[serde(default)]
    docs_only: i64,
    #[serde(default)]
    merged_at: Option<i64>,
    #[serde(default)]
    workers: Option<String>,
    #[serde(default)]
    deploys: Option<String>,
    #[serde(default)]
    deploy_worker: Option<String>,
    #[serde(default = "waits_for_core")]
    ordered: i64,
    #[serde(default)]
    gate_digest: Option<String>,
    #[serde(default)]
    approved_digest: Option<String>,
    #[serde(default)]
    test_retries: i64,
}

/// A landing row read without the `ordered` column waits for core, as every landing once did.
fn waits_for_core() -> i64 {
    1
}

impl Row {
    fn state(&self) -> State {
        State::named(&self.state).unwrap_or(State::Error)
    }

    fn public(&self) -> Value {
        json!({
            "id": self.id, "branch": self.branch,
            "commit": (!self.commit_sha.is_empty()).then_some(&self.commit_sha),
            "main_was": (!self.main_at.is_empty()).then_some(&self.main_at),
            "by": self.by_name, "state": self.state, "awaiting_since": self.awaiting_since,
            "needs_approval": self.approval_reason, "approved_by": self.approved_by,
            "test_job": self.test_job, "deploy_job": self.deploy_job, "detail": self.detail,
            "created_at": self.created_at, "updated_at": self.updated_at,
            "finished_at": self.finished_at, "note_pushed": self.note_pushed != 0,
            "note_tries": self.note_tries, "base": self.base, "tested_at": self.tested,
            "docs_only": self.docs_only != 0, "merged_at": self.merged_at,
            "workers": self.workers.as_deref().and_then(|w| serde_json::from_str::<Value>(w).ok()),
            "deploys_left": self.deploys.as_deref().and_then(|w| serde_json::from_str::<Value>(w).ok()),
            "deploying": self.deploy_worker,
            "gate_digest": self.gate_digest, "approved_digest": self.approved_digest,
            "test_retries": self.test_retries,
        })
    }

    /// Whether it still waits for a person's approval: one is needed and none was given.
    /// The Workers it affects, by name; `core` for a landing checked before there were lanes.
    fn workers(&self) -> Vec<String> {
        self.workers
            .as_deref()
            .and_then(|w| serde_json::from_str(w).ok())
            .unwrap_or_else(|| vec!["core".to_string()])
    }

    /// The Workers its deploy has still to deploy, in order.
    fn deploys(&self) -> Vec<String> {
        self.deploys
            .as_deref()
            .and_then(|w| serde_json::from_str(w).ok())
            .unwrap_or_else(|| vec!["core".to_string()])
    }

    fn needs_approval(&self) -> bool {
        self.approval_reason.is_some() && self.approved_by.is_none()
    }
}

struct SubmittedJob {
    id: String,
    reused: bool,
}

/// A CI deploy job that has ended, as the Lander keeps it.
struct JobEnd {
    /// Whether it succeeded.
    passed: bool,
    /// One line saying how it ended.
    detail: String,
    /// Its exit code, kept only for state `failed`.
    exit_code: Option<i64>,
    /// The job's state: `succeeded`, `failed`, `cancelled`, `timed_out` or `error`.
    state: String,
    /// The last 2000 characters of its output.
    tail: String,
}

/// One deploy in a Worker's lane.
#[derive(Deserialize, Debug, Clone)]
struct DeployRow {
    id: i64,
    worker: String,
    landing: String,
    commit_sha: String,
    covers: i64,
    carries: String,
    job: Option<String>,
    state: String,
    tries: i64,
    detail: Option<String>,
    created_at: i64,
    updated_at: i64,
    finished_at: Option<i64>,
    #[serde(default)]
    included: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    incident: Option<i64>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    rate_retries: i64,
    #[serde(default)]
    not_before: Option<i64>,
}

impl DeployRow {
    fn pure(&self) -> lanes::Deploy {
        lanes::Deploy {
            worker: self.worker.clone(),
            covers: self.covers,
            // A state this build does not know is taken as an error: it never covers a landing.
            state: lanes::DeployState::named(&self.state).unwrap_or(lanes::DeployState::Error),
            incident: self.incident.is_some(),
        }
    }

    fn carries(&self) -> Vec<String> {
        serde_json::from_str(&self.carries).unwrap_or_default()
    }

    /// The landings it included with their commits; `None` for a deploy made before they were
    /// recorded.
    fn included(&self) -> Option<Vec<lanes::Included>> {
        self.included
            .as_deref()
            .and_then(|i| serde_json::from_str(i).ok())
    }

    fn purpose(&self) -> lanes::Purpose {
        self.purpose
            .as_deref()
            .and_then(lanes::Purpose::named)
            .unwrap_or(lanes::Purpose::Lane)
    }

    fn public(&self) -> Value {
        json!({
            "id": self.id, "worker": self.worker, "landing": self.landing,
            "commit": self.commit_sha, "purpose": self.purpose().name(),
            "incident": self.incident, "carries": self.carries(),
            "included": self.included.as_deref().and_then(|i| serde_json::from_str::<Value>(i).ok()),
            "job": self.job, "state": self.state,
            "tries": self.tries, "detail": self.detail, "created_at": self.created_at,
            "updated_at": self.updated_at, "finished_at": self.finished_at,
        })
    }
}

/// A lane's failed deploy being attributed.
#[derive(Deserialize, Debug, Clone)]
struct IncidentRow {
    id: i64,
    worker: String,
    failed_deploy: i64,
    landing: String,
    last_good: Option<String>,
    included: String,
    state: String,
    guilty: Option<String>,
    summary: Option<String>,
    redeploys: i64,
    created_at: i64,
    updated_at: i64,
    finished_at: Option<i64>,
}

impl IncidentRow {
    fn last_good(&self) -> Option<lanes::Good> {
        self.last_good
            .as_deref()
            .and_then(|g| serde_json::from_str(g).ok())
    }

    fn included(&self) -> Vec<lanes::Included> {
        serde_json::from_str(&self.included).unwrap_or_default()
    }

    fn public(&self) -> Value {
        json!({
            "id": self.id, "worker": self.worker, "failed_deploy": self.failed_deploy,
            "landing": self.landing,
            "last_good": self.last_good.as_deref().and_then(|g| serde_json::from_str::<Value>(g).ok()),
            "included": serde_json::from_str::<Value>(&self.included).ok(),
            "state": self.state, "guilty": self.guilty, "summary": self.summary,
            "redeploys": self.redeploys, "created_at": self.created_at,
            "updated_at": self.updated_at, "finished_at": self.finished_at,
        })
    }
}

/// The Lander's logic; the hosting Worker's Durable Object class wraps it.
pub struct Lander {
    db: Db,
    state: ObjectState,
    env: Env,
    settings: Settings,
    hosting: Hosting,
}

/// The live states, as SQL.
const LIVE: &str =
    "('checking', 'testing', 'awaiting_approval', 'ready', 'merging', 'merged', 'deploying')";

impl Lander {
    pub fn new(state: ObjectState, env: Env, settings: Settings, hosting: Hosting) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self {
            db,
            state,
            env,
            settings,
            hosting,
        })
    }

    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    /// The alarm: each live landing's next step, oldest first; then the merge lock goes to
    /// whoever waits for it; then the deploy lanes, one per Worker; then notes still to push,
    /// then a few test refs to delete, and the next alarm.
    pub async fn alarm(&self) -> Result<Response> {
        for row in self.all_live()? {
            self.step_logged(row).await?;
        }
        if let Some(row) = self.take_merge_lock()? {
            self.step_logged(row).await?;
        }
        self.lanes().await?;
        if let Err(e) = self.push_notes().await {
            console_error!("land: pushing notes: {e}");
        }
        if let Err(e) = self.deliver_events().await {
            console_error!("land: telling runs: {e}");
        }
        if let Err(e) = self.delete_test_refs().await {
            console_error!("land: deleting test refs: {e}");
        }
        self.reschedule().await?;
        Response::ok("ok")
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        let s = |k: &str| a[k].as_str().unwrap_or_default();
        match op {
            "create" => {
                let same: Option<Row> = self.db.row(
                    &format!("SELECT * FROM landings WHERE state IN {LIVE} AND branch = ? LIMIT 1"),
                    vec![s("branch").into()],
                )?;
                if let Some(live) = same {
                    return Err(bad(&format!(
                        "landing {} of {} is {}; cancel it first (land_cancel) to land the \
                         branch again",
                        live.id, live.branch, live.state
                    )));
                }
                let t = now();
                self.db.exec(
                    "INSERT INTO landings (id, branch, commit_sha, main_at, caller, by_name, state, approval_reason, created_at, updated_at) VALUES (?, ?, ?, '', ?, ?, 'checking', NULL, ?, ?)",
                    vec![
                        s("id").into(),
                        s("branch").into(),
                        s("commit").into(),
                        a["caller"].to_string().into(),
                        s("by").into(),
                        t.into(),
                        t.into(),
                    ],
                )?;
                self.event(s("id"), State::Checking, Some("checking the branch"))?;
                if let Some(run) = a["run"].as_str() {
                    self.watch(s("id"), run, &a["caller"].to_string())?;
                }
                // The checks start at once, in the alarm.
                self.state.storage().set_alarm(0).await?;
                Ok(Value::Null)
            }
            "job" => {
                let column = match s("kind") {
                    "test" => "test_job",
                    "deploy" => "deploy_job",
                    _ => return Err(bad("`kind` is test or deploy")),
                };
                self.db.exec(
                    &format!("UPDATE landings SET {column} = ?, updated_at = ? WHERE id = ?"),
                    vec![s("job").into(), now().into(), s("id").into()],
                )?;
                self.reschedule().await?;
                Ok(Value::Null)
            }
            "fail" => {
                if let Some(row) = self.row(s("id"))? {
                    self.apply(&row, &Seen::Failed, Some(s("why")))?;
                }
                self.reschedule().await?;
                Ok(Value::Null)
            }
            "deploys_since" => {
                let since = a["since_ms"].as_i64().unwrap_or(0);
                let rows: Vec<DeployRow> = self.db.rows(
                    "SELECT * FROM deploys WHERE finished_at >= ? ORDER BY id DESC LIMIT 50",
                    vec![since.into()],
                )?;
                Ok(json!(
                    rows.iter().map(DeployRow::public).collect::<Vec<_>>()
                ))
            }
            "watch" => {
                let row = self.row(s("id"))?.ok_or_else(|| bad("no such landing"))?;
                self.watch(&row.id, s("run"), &a["caller"].to_string())?;
                Ok(digest::compact(&row.public(), self.has_incident(&row.id)?))
            }
            "status" => {
                let row = match a["id"].as_str() {
                    Some(id) => self.row(id)?,
                    None => self.db.row(
                        "SELECT * FROM landings ORDER BY created_at DESC LIMIT 1",
                        vec![],
                    )?,
                };
                let Some(row) = row else {
                    return Err(bad("no such landing"));
                };
                let events: Vec<Value> = self.db.rows(
                    "SELECT at, state, detail FROM events WHERE landing = ? ORDER BY at, rowid",
                    vec![row.id.as_str().into()],
                )?;
                let mut out = row.public();
                out["steps"] = json!(events);
                // Each deploy that carried it, with the landings and commits it included, and
                // each incident that judged it (with its rollback, bisect and restore deploys).
                let like = format!("%\"{}\"%", row.id);
                let deploys: Vec<DeployRow> = self.db.rows(
                    "SELECT * FROM deploys WHERE carries LIKE ? OR incident IN (SELECT id FROM incidents WHERE included LIKE ?) ORDER BY id",
                    vec![like.as_str().into(), like.as_str().into()],
                )?;
                out["deploys"] = json!(deploys.iter().map(DeployRow::public).collect::<Vec<_>>());
                let incidents: Vec<IncidentRow> = self.db.rows(
                    "SELECT * FROM incidents WHERE included LIKE ? ORDER BY id",
                    vec![like.as_str().into()],
                )?;
                out["incidents"] = json!(
                    incidents
                        .iter()
                        .map(IncidentRow::public)
                        .collect::<Vec<_>>()
                );
                // The latest incidents still open, that could not isolate their cause, or that
                // may have left a Worker on a failing build: for the person.
                let open: Vec<IncidentRow> = self.db.rows(
                    "SELECT * FROM incidents WHERE state IN ('open', 'unisolated', 'stuck', 'environment', 'rate_limited') ORDER BY id DESC LIMIT 10",
                    vec![],
                )?;
                // An incident of a Worker the map no longer has needs no action.
                out["incidents_to_see"] = json!(
                    open.iter()
                        .filter(|i| self.settings.map.worker(&i.worker).is_some())
                        .map(IncidentRow::public)
                        .collect::<Vec<_>>()
                );
                out["queue"] = json!(
                    self.all_live()?
                        .iter()
                        .map(|r| json!({ "id": r.id, "branch": r.branch, "state": r.state }))
                        .collect::<Vec<_>>()
                );
                if a["compact"] == true {
                    return Ok(digest::compact_status(&out));
                }
                Ok(out)
            }
            "list" => {
                let limit = a["limit"].as_i64().unwrap_or(10).clamp(1, 50);
                let states = digest::state_filter(a["state"].as_str()).map_err(|e| bad(&e))?;
                let rows: Vec<Row> = match states {
                    // The state names are fixed words, so they are written into the query.
                    Some(states) => {
                        let names: Vec<String> =
                            states.iter().map(|s| format!("'{}'", s.name())).collect();
                        self.db.rows(
                            &format!(
                                "SELECT * FROM landings WHERE state IN ({}) ORDER BY created_at DESC LIMIT ?",
                                names.join(", ")
                            ),
                            vec![limit.into()],
                        )?
                    }
                    None => self.db.rows(
                        "SELECT * FROM landings ORDER BY created_at DESC LIMIT ?",
                        vec![limit.into()],
                    )?,
                };
                if a["compact"] == true {
                    let mut out = Vec::new();
                    for r in &rows {
                        out.push(digest::compact(&r.public(), self.has_incident(&r.id)?));
                    }
                    return Ok(json!(out));
                }
                Ok(json!(rows.iter().map(Row::public).collect::<Vec<_>>()))
            }
            "cancel" => {
                let row = self.row(s("id"))?.ok_or_else(|| bad("no such landing"))?;
                let why = format!("cancelled by {}", s("by"));
                if !self.apply(&row, &Seen::Cancel, Some(&why))? {
                    return Err(bad(&format!(
                        "landing {} is {}; only one checking, testing, awaiting approval or ready can be cancelled",
                        row.id, row.state
                    )));
                }
                self.reschedule().await?;
                Ok(json!({ "test_job": row.test_job }))
            }
            "approve" => {
                let row = self.row(s("id"))?.ok_or_else(|| bad("no such landing"))?;
                if row.state() != State::AwaitingApproval {
                    return Err(bad(&format!(
                        "landing {} is {}, not awaiting approval",
                        row.id, row.state
                    )));
                }
                // The approval is for the change to the guarded paths as checked: its digest, in
                // the same write. Only the Lander's rebase of that same change keeps it.
                self.db.exec(
                    "UPDATE landings SET approved_by = ?, approved_digest = gate_digest WHERE id = ?",
                    vec![s("person").into(), row.id.as_str().into()],
                )?;
                let why = format!("approved by {}", s("person"));
                self.apply(&row, &Seen::Approved, Some(&why))?;
                self.state.storage().set_alarm(0).await?;
                Ok(json!({ "message": format!(
                    "Approved: {} lands at {} once the merge lock is free.",
                    row.branch.trim_start_matches("refs/heads/"),
                    &row.commit_sha[..12.min(row.commit_sha.len())]
                ) }))
            }
            "approval_get" => {
                #[derive(Deserialize)]
                struct Setting {
                    value: String,
                    by_name: Option<String>,
                    at: i64,
                }
                let found: Option<Setting> = self.db.row(
                    "SELECT value, by_name, at FROM settings WHERE key = 'approval'",
                    vec![],
                )?;
                Ok(match found {
                    Some(f) => json!({ "on": f.value == "on", "by": f.by_name, "at": f.at }),
                    None => json!({ "on": false, "by": null, "at": null }),
                })
            }
            "approval_set" => {
                let on = a["on"]
                    .as_bool()
                    .ok_or_else(|| bad("`on` is true or false"))?;
                self.db.exec(
                    "INSERT INTO settings (key, value, by_name, at) VALUES ('approval', ?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, by_name = excluded.by_name, at = excluded.at",
                    vec![
                        (if on { "on" } else { "off" }).into(),
                        s("person").into(),
                        now().into(),
                    ],
                )?;
                Ok(json!({ "message": if on {
                    "Every landing now waits for a person's approval after its tests."
                } else {
                    "Landings no longer wait for approval, except those that change what the gate runs."
                } }))
            }
            "deploy_grant" => {
                if s("job").is_empty() {
                    return Ok(Value::Null);
                }
                // A running deploy in a Worker's lane, for that Worker's script alone.
                let lane: Option<DeployRow> = self.db.row(
                    "SELECT * FROM deploys WHERE state = 'running' AND job = ? AND commit_sha = ? LIMIT 1",
                    vec![s("job").into(), s("commit").into()],
                )?;
                if let Some(d) = lane {
                    let compiled = self
                        .settings
                        .map
                        .worker(&d.worker)
                        .map(|w| w.script.as_str());
                    let Some(script) = lanes::grant_script(d.script.as_deref(), compiled) else {
                        return Ok(Value::Null);
                    };
                    let by = self
                        .row(&d.landing)?
                        .map_or_else(|| "land".to_string(), |r| r.by_name);
                    return Ok(json!({ "id": d.landing, "by": by, "script": script }));
                }
                // A deploy a Lander before lanes started, until it is adopted.
                let row: Option<Row> = self.db.row(
                    "SELECT * FROM landings WHERE state = 'deploying' AND deploy_job = ? AND commit_sha = ?",
                    vec![s("job").into(), s("commit").into()],
                )?;
                Ok(match row {
                    Some(r) if !s("job").is_empty() => {
                        let name = r.deploy_worker.as_deref().unwrap_or("core");
                        let script = self.settings.map.worker(name).map(|w| w.script.clone());
                        json!({ "id": r.id, "by": r.by_name, "script": script })
                    }
                    _ => Value::Null,
                })
            }
            other => Err(bad(&format!("no operation `{other}`"))),
        }
    }

    /// Whether the setting asks a person to approve every landing.
    fn approval_on(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct V {
            value: String,
        }
        let v: Option<V> = self
            .db
            .row("SELECT value FROM settings WHERE key = 'approval'", vec![])?;
        Ok(v.is_some_and(|v| v.value == "on"))
    }

    /// The `merged_at` covered by the first core deploy this Lander saw pass: a core build at or
    /// after it holds this Lander, so core may be rolled back to it. `None` until one passed.
    fn core_lander_since(&self) -> Result<Option<i64>> {
        #[derive(Deserialize)]
        struct V {
            value: String,
        }
        let v: Option<V> = self.db.row(
            "SELECT value FROM settings WHERE key = 'core_lander_since'",
            vec![],
        )?;
        Ok(v.and_then(|v| serde_json::from_str::<Value>(&v.value).ok())
            .and_then(|v| v["covers"].as_i64()))
    }

    fn row(&self, id: &str) -> Result<Option<Row>> {
        self.db
            .row("SELECT * FROM landings WHERE id = ?", vec![id.into()])
    }

    fn live(&self) -> Result<Option<Row>> {
        self.db.row(
            &format!("SELECT * FROM landings WHERE state IN {LIVE} ORDER BY created_at LIMIT 1"),
            vec![],
        )
    }

    /// Every live landing, oldest first.
    fn all_live(&self) -> Result<Vec<Row>> {
        self.db.rows(
            &format!("SELECT * FROM landings WHERE state IN {LIVE} ORDER BY created_at"),
            vec![],
        )
    }

    /// One landing's step; a failure on the gateway's side is tried again, then an error.
    async fn step_logged(&self, row: Row) -> Result<()> {
        let id = row.id.clone();
        if let Err(e) = self.step(row).await {
            console_error!("land: landing {id}: {e}");
            self.retry_or_fail(&id, &e.to_string())?;
        }
        Ok(())
    }

    /// The merge lock for the oldest `ready` landing, when no landing holds it: that landing,
    /// now `merging`.
    fn take_merge_lock(&self) -> Result<Option<Row>> {
        let held: Option<Row> = self.db.row(
            "SELECT * FROM landings WHERE state = 'merging' LIMIT 1",
            vec![],
        )?;
        if held.is_some() {
            return Ok(None);
        }
        let next: Option<Row> = self.db.row(
            "SELECT * FROM landings WHERE state = 'ready' ORDER BY created_at LIMIT 1",
            vec![],
        )?;
        let Some(row) = next else {
            return Ok(None);
        };
        self.apply(&row, &Seen::Start, Some("took the merge lock"))?;
        self.row(&row.id)
    }

    /// The deploy lanes, one per Worker: adopts a deploy a Lander before lanes left running,
    /// steps each running deploy, settles the merged landings whose deploys have ended, and
    /// starts what each idle lane deploys next.
    async fn lanes(&self) -> Result<()> {
        self.adopt_legacy()?;
        let running: Vec<DeployRow> = self.db.rows(
            "SELECT * FROM deploys WHERE state = 'running' ORDER BY id",
            vec![],
        )?;
        for d in running {
            self.deploy_logged(d).await?;
        }
        for d in self.drive_incidents()? {
            self.deploy_logged(d).await?;
        }
        self.settle()?;
        for d in self.start_lanes()? {
            self.deploy_logged(d).await?;
        }
        Ok(())
    }

    /// Inserts a running deploy, its job still to submit, and answers it.
    #[allow(clippy::too_many_arguments)]
    fn insert_deploy(
        &self,
        worker: &str,
        landing: &str,
        commit: &str,
        covers: i64,
        carries: &[String],
        included: &[lanes::Included],
        purpose: lanes::Purpose,
        incident: Option<i64>,
    ) -> Result<DeployRow> {
        let t = now();
        self.db.exec(
            "INSERT INTO deploys (worker, landing, commit_sha, covers, carries, included, purpose, incident, state, detail, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'running', ?, ?, ?)",
            vec![
                worker.into(),
                landing.into(),
                commit.into(),
                covers.into(),
                serde_json::to_string(carries)?.into(),
                serde_json::to_string(included)?.into(),
                purpose.name().into(),
                opt_i(incident),
                format!("{worker} to deploy at {commit} ({})", purpose.name()).into(),
                t.into(),
                t.into(),
            ],
        )?;
        // Nothing awaits between the insert and this read.
        self.db
            .row("SELECT * FROM deploys ORDER BY id DESC LIMIT 1", vec![])?
            .ok_or_else(|| bad("the deploy just started is not there"))
    }

    /// The landings a deploy included, with their commits: as recorded, or, for a deploy made
    /// before they were, its carried landings as they stand.
    fn included_of(&self, d: &DeployRow) -> Result<Vec<lanes::Included>> {
        if let Some(i) = d.included() {
            return Ok(i);
        }
        let mut out = Vec::new();
        for id in d.carries() {
            if let Some(r) = self.row(&id)? {
                out.push(lanes::Included {
                    merged_at: r.merged_at.unwrap_or(r.updated_at),
                    commit: r.commit_sha,
                    id,
                });
            }
        }
        out.sort_by_key(|l| l.merged_at);
        Ok(out)
    }

    /// Whether the rollback of the incident of failed deploy `failed` is skipped: nothing was
    /// uploaded ([`lanes::never_uploaded`], from the exit code in the deploy's detail) and
    /// the Worker's newest earlier deploy is the succeeded one `last_good` names.
    fn rollback_skipped(&self, failed: &DeployRow) -> Result<bool> {
        let code = failed.detail.as_deref().and_then(lanes::exit_code_in);
        if !lanes::never_uploaded(&failed.state, code) {
            return Ok(false);
        }
        #[derive(Deserialize)]
        struct I {
            id: i64,
        }
        let newest: Option<I> = self.db.row(
            "SELECT id FROM deploys WHERE worker = ? AND id < ? ORDER BY id DESC LIMIT 1",
            vec![failed.worker.as_str().into(), failed.id.into()],
        )?;
        let good: Option<I> = self.db.row(
            "SELECT id FROM deploys WHERE worker = ? AND state = 'succeeded' AND id < ? ORDER BY id DESC LIMIT 1",
            vec![failed.worker.as_str().into(), failed.id.into()],
        )?;
        Ok(lanes::skip_rollback(
            true,
            newest.map(|n| n.id),
            good.map(|g| g.id),
        ))
    }

    /// Opens an incident for lane deploy `d`, which just failed: its Worker's last good build
    /// before it, and the landings it included, are what [`lanes::bisect`] works from.
    fn open_incident(&self, d: &DeployRow) -> Result<()> {
        let included = self.included_of(d)?;
        let good: Option<DeployRow> = self.db.row(
            "SELECT * FROM deploys WHERE worker = ? AND state = 'succeeded' AND id < ? ORDER BY id DESC LIMIT 1",
            vec![d.worker.as_str().into(), d.id.into()],
        )?;
        let skipped = match (&good, self.deploy(d.id)?) {
            (Some(_), Some(f)) => self.rollback_skipped(&f)?,
            _ => false,
        };
        let last_good = good.map(|g| lanes::Good {
            commit: g.commit_sha,
            covers: g.covers,
        });
        let t = now();
        let summary = if skipped && d.worker == CORE_WORKER {
            format!(
                "the core deploy of {} failed; nothing was uploaded, so core is still on its good build",
                d.commit_sha
            )
        } else if skipped {
            format!(
                "{}'s deploy of {} failed; nothing was uploaded, so no rollback, then deploying the {} included commits one at a time, oldest first",
                d.worker,
                d.commit_sha,
                included.len()
            )
        } else if d.worker == CORE_WORKER {
            format!(
                "the core deploy of {} failed; rolling core back to its last good build that holds this Lander, if there is one; not bisected (core runs the Lander)",
                d.commit_sha
            )
        } else {
            format!(
                "{}'s deploy of {} failed; {}, then deploying the {} included commits one at a time, oldest first",
                d.worker,
                d.commit_sha,
                match &last_good {
                    Some(g) => format!("rolling back to {}", g.commit),
                    None => lanes::first_deploy_text(&d.worker),
                },
                included.len()
            )
        };
        self.db.exec(
            "INSERT INTO incidents (worker, failed_deploy, landing, last_good, included, state, summary, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'open', ?, ?, ?)",
            vec![
                d.worker.as_str().into(),
                d.id.into(),
                d.landing.as_str().into(),
                opt(last_good
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .as_deref()),
                serde_json::to_string(&included)?.into(),
                summary.as_str().into(),
                t.into(),
                t.into(),
            ],
        )?;
        #[derive(Deserialize)]
        struct I {
            id: i64,
        }
        let inc: I = self
            .db
            .row("SELECT id FROM incidents ORDER BY id DESC LIMIT 1", vec![])?
            .ok_or_else(|| bad("the incident just opened is not there"))?;
        self.db.exec(
            "UPDATE deploys SET incident = ? WHERE id = ?",
            vec![inc.id.into(), d.id.into()],
        )?;
        for l in &included {
            self.notify(&l.id, &watch::incident_text(&l.id, &d.worker, &summary))?;
            if let Some(r) = self.row(&l.id)?
                && !r.state().terminal()
            {
                self.note_detail(&r, &format!("incident {}: {summary}", inc.id))?;
            }
        }
        Ok(())
    }

    /// Closes each `unisolated` or `stuck` incident that a later succeeded lane deploy of its
    /// Worker covers ([`lanes::incident_covered`]); the state becomes [`lanes::COVERED`].
    fn close_covered(&self) -> Result<()> {
        let open: Vec<IncidentRow> = self.db.rows(
            "SELECT * FROM incidents WHERE state IN ('unisolated', 'stuck', 'environment', 'rate_limited') ORDER BY id",
            vec![],
        )?;
        for inc in open {
            let Some(failed) = self.deploy(inc.failed_deploy)? else {
                continue;
            };
            let green: Option<DeployRow> = self.db.row(
                "SELECT * FROM deploys WHERE worker = ? AND state = 'succeeded' AND (purpose = 'lane' OR purpose IS NULL) ORDER BY id DESC LIMIT 1",
                vec![inc.worker.as_str().into()],
            )?;
            let Some(green) = green else {
                continue;
            };
            if !lanes::incident_covered(
                &inc.state,
                failed.id,
                failed.covers,
                green.id,
                green.covers,
            ) {
                continue;
            }
            let t = now();
            let summary = format!(
                "{}; covered by deploy {} at {}, which passed",
                inc.summary.as_deref().unwrap_or(""),
                green.id,
                green.commit_sha
            );
            self.db.exec(
                "UPDATE incidents SET state = ?, summary = ?, updated_at = ?, finished_at = ? WHERE id = ? AND state = ?",
                vec![
                    lanes::COVERED.into(),
                    summary.as_str().into(),
                    t.into(),
                    t.into(),
                    inc.id.into(),
                    inc.state.as_str().into(),
                ],
            )?;
        }
        Ok(())
    }

    /// Each open incident with no deploy running: its next redeploy, started (answered, its
    /// job still to submit), or its verdict, applied to the landings it included.
    fn drive_incidents(&self) -> Result<Vec<DeployRow>> {
        let open: Vec<IncidentRow> = self.db.rows(
            "SELECT * FROM incidents WHERE state = 'open' ORDER BY id",
            vec![],
        )?;
        let mut out = Vec::new();
        for inc in open {
            let mine: Vec<DeployRow> = self.db.rows(
                "SELECT * FROM deploys WHERE incident = ? AND id != ? ORDER BY id",
                vec![inc.id.into(), inc.failed_deploy.into()],
            )?;
            if mine.iter().any(|d| d.state == "running") {
                continue;
            }
            // A redeploy that failed for the environment ends the incident: no bisect, and no
            // landing is to blame.
            if let Some(env) = mine.iter().find(|d| {
                d.state == "error"
                    && d.detail
                        .as_deref()
                        .is_some_and(lanes::is_environment_detail)
            }) {
                let why = env.detail.clone().unwrap_or_default();
                let t = now();
                // A rate limit that outlasted its redeploys is recorded as such.
                let (to, what) = if lanes::is_rate_limit_detail(&why) {
                    (lanes::RATE_LIMITED, "stayed rate limited by Cloudflare")
                } else {
                    (lanes::ENVIRONMENT, "then failed for the environment")
                };
                self.db.exec(
                    "UPDATE incidents SET state = ?, summary = ?, redeploys = ?, updated_at = ?, finished_at = ? WHERE id = ? AND state = 'open'",
                    vec![
                        to.into(),
                        format!(
                            "{}'s deploy failed; a redeploy {what}, so no bisect; the Worker may still be on a failing build: {why}",
                            inc.worker
                        )
                        .into(),
                        (mine.len() as i64).into(),
                        t.into(),
                        t.into(),
                        inc.id.into(),
                    ],
                )?;
                for l in inc.included() {
                    if let Some(r) = self.row(&l.id)? {
                        self.apply(&r, &Seen::Failed, Some(&why))?;
                    }
                }
                continue;
            }
            let tried: Vec<lanes::Tried> = mine
                .iter()
                .map(|d| match d.state.as_str() {
                    "succeeded" => lanes::Tried::Passed,
                    "failed" => lanes::Tried::Failed,
                    _ => lanes::Tried::Error,
                })
                .collect();
            let included = inc.included();
            let last_good = inc.last_good();
            let t = now();
            // Core runs the Lander: rolled back only to a build holding this Lander, never
            // bisected (`lanes::core_incident`).
            let skipped = match self.deploy(inc.failed_deploy)? {
                Some(f) => self.rollback_skipped(&f)?,
                None => false,
            };
            let next = if inc.worker == CORE_WORKER {
                let failed: Option<DeployRow> = self.deploy(inc.failed_deploy)?;
                let failed_commit = failed.map(|d| d.commit_sha).unwrap_or_default();
                lanes::core_incident_from(
                    last_good.as_ref(),
                    self.core_lander_since()?,
                    &failed_commit,
                    &included,
                    &tried,
                    skipped,
                )
            } else {
                lanes::bisect_from(
                    &inc.worker,
                    last_good.as_ref(),
                    &included,
                    &tried,
                    lanes::REDEPLOYS,
                    skipped,
                )
            };
            match next {
                lanes::Next::Deploy {
                    purpose,
                    commit,
                    covers,
                    carries,
                } => {
                    let with: Vec<lanes::Included> = included
                        .iter()
                        .filter(|l| carries.contains(&l.id))
                        .cloned()
                        .collect();
                    let d = self.insert_deploy(
                        &inc.worker,
                        &inc.landing,
                        &commit,
                        covers,
                        &carries,
                        &with,
                        purpose,
                        Some(inc.id),
                    )?;
                    self.db.exec(
                        "UPDATE incidents SET redeploys = ?, summary = ?, updated_at = ? WHERE id = ?",
                        vec![
                            (tried.len() as i64 + 1).into(),
                            format!(
                                "{}'s deploy failed{}; redeploy {} of at most {} ({}): {commit}",
                                inc.worker,
                                if skipped { " (nothing was uploaded)" } else { "" },
                                tried.len() + 1,
                                lanes::REDEPLOYS,
                                purpose.name()
                            )
                            .into(),
                            t.into(),
                            inc.id.into(),
                        ],
                    )?;
                    out.push(d);
                }
                lanes::Next::Done(v) => {
                    let state = match (&v.guilty, &v.on) {
                        (_, None) => "stuck",
                        (Some(_), Some(_)) => "isolated",
                        (None, Some(_)) => "unisolated",
                    };
                    self.db.exec(
                        "UPDATE incidents SET state = ?, guilty = ?, summary = ?, redeploys = ?, updated_at = ?, finished_at = ? WHERE id = ? AND state = 'open'",
                        vec![
                            state.into(),
                            opt(v.guilty.as_deref()),
                            if skipped && inc.worker != CORE_WORKER {
                                format!("{}; nothing was uploaded, so no rollback", v.summary)
                            } else {
                                v.summary.clone()
                            }
                            .into(),
                            (v.redeploys as i64).into(),
                            t.into(),
                            t.into(),
                            inc.id.into(),
                        ],
                    )?;
                    for (id, why) in &v.failed {
                        if let Some(r) = self.row(id)? {
                            let why = format!("incident {}: {why}", inc.id);
                            self.apply(&r, &Seen::JobEnded { passed: false }, Some(&why))?;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The live landings in main, waiting for their deploys, as the lanes see them.
    fn merged(&self) -> Result<Vec<(Row, lanes::Merged)>> {
        let rows: Vec<Row> = self.db.rows(
            "SELECT * FROM landings WHERE state IN ('merged', 'deploying') ORDER BY merged_at, created_at",
            vec![],
        )?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let m = lanes::Merged {
                    id: r.id.clone(),
                    merged_at: r.merged_at.unwrap_or(r.updated_at),
                    commit: r.commit_sha.clone(),
                    workers: r.workers(),
                    ordered: r.ordered & lanes::ORDER_CORE_FIRST != 0,
                    proxy_first: r.ordered & lanes::ORDER_PROXY_FIRST != 0,
                };
                (r, m)
            })
            .collect())
    }

    /// The deploys that can still matter to `merged` (not empty): the running ones, and those
    /// that carried a commit at or after the oldest one's merge.
    fn deploys_for(&self, merged: &[(Row, lanes::Merged)]) -> Result<Vec<lanes::Deploy>> {
        let oldest = merged.iter().map(|(_, m)| m.merged_at).min().unwrap_or(0);
        let rows: Vec<DeployRow> = self.db.rows(
            "SELECT * FROM deploys WHERE state = 'running' OR covers >= ?",
            vec![oldest.into()],
        )?;
        Ok(rows.iter().map(DeployRow::pure).collect())
    }

    /// Moves each live merged landing whose deploys have come to an end: `landed` once every
    /// Worker it affects deployed a commit at or after its merge, `deploy_failed` (or `error`)
    /// once a deploy that carried it did not pass (or could not run).
    fn settle(&self) -> Result<()> {
        let merged = self.merged()?;
        if merged.is_empty() {
            return Ok(());
        }
        let deploys = self.deploys_for(&merged)?;
        for (row, m) in &merged {
            let Some(seen) = lanes::outcome(m, &deploys) else {
                continue;
            };
            let (seen, detail) = match seen {
                Seen::JobEnded { passed: true } => (
                    // Carried by deploys that started before it reached `deploying`.
                    if row.state() == State::Merged {
                        Seen::Superseded
                    } else {
                        seen
                    },
                    self.deployed_text(m)?,
                ),
                Seen::JobEnded { passed: false } => (
                    seen,
                    "a deploy that carried it failed (see its deploys)".to_string(),
                ),
                _ => {
                    // An environment failure that ended a deploy says so for the landing.
                    let mut why =
                        "a deploy that carried it could not be run (see its deploys)".to_string();
                    for w in &m.workers {
                        let found: Vec<DeployRow> = self.db.rows(
                            "SELECT * FROM deploys WHERE worker = ? AND state = 'error' AND incident IS NULL AND covers >= ? ORDER BY id DESC",
                            vec![w.as_str().into(), m.merged_at.into()],
                        )?;
                        if let Some(d) = found.iter().find(|d| {
                            d.detail
                                .as_deref()
                                .is_some_and(lanes::is_environment_detail)
                        }) {
                            why = d.detail.clone().unwrap_or(why);
                            break;
                        }
                    }
                    (seen, why)
                }
            };
            self.apply(row, &seen, Some(&detail))?;
        }
        Ok(())
    }

    /// `deployed: <Workers>` for a landing every Worker of which deployed it, naming those whose
    /// deploy was skipped because the Worker was removed at its commit.
    fn deployed_text(&self, m: &lanes::Merged) -> Result<String> {
        let mut deployed = Vec::new();
        let mut skipped = Vec::new();
        for w in &m.workers {
            let found: Vec<DeployRow> = self.db.rows(
                "SELECT * FROM deploys WHERE worker = ? AND state = 'succeeded' AND covers >= ? ORDER BY id",
                vec![w.as_str().into(), m.merged_at.into()],
            )?;
            let skip = found.iter().find_map(|d| {
                d.detail
                    .as_deref()
                    .filter(|x| lanes::is_skipped_detail(x))
                    .map(str::to_string)
            });
            match skip {
                Some(why) if found.len() == 1 => skipped.push(format!("{w} ({why})")),
                _ => deployed.push(w.as_str()),
            }
        }
        let mut text = format!("deployed: {}", deployed.join(", "));
        if !skipped.is_empty() {
            text.push_str(&format!("; {}", skipped.join("; ")));
        }
        Ok(text)
    }

    /// Starts what each idle lane deploys next ([`lanes::starts`]): a running deploy whose job
    /// is still to submit, with the merged landings it carries now `deploying`.
    fn start_lanes(&self) -> Result<Vec<DeployRow>> {
        let merged = self.merged()?;
        if merged.is_empty() {
            return Ok(Vec::new());
        }
        let deploys = self.deploys_for(&merged)?;
        let plain: Vec<lanes::Merged> = merged.iter().map(|(_, m)| m.clone()).collect();
        // A lane with an open incident is the incident's until it ends.
        #[derive(Deserialize)]
        struct W {
            worker: String,
        }
        let held: Vec<W> = self.db.rows(
            "SELECT DISTINCT worker FROM incidents WHERE state = 'open'",
            vec![],
        )?;
        let held: Vec<&str> = held.iter().map(|w| w.worker.as_str()).collect();
        let mut out = Vec::new();
        for st in lanes::starts(CORE_WORKER, &plain, &deploys, &held) {
            let included: Vec<lanes::Included> = st
                .carries
                .iter()
                .filter_map(|id| plain.iter().find(|m| &m.id == id))
                .map(|m| lanes::Included {
                    id: m.id.clone(),
                    commit: m.commit.clone(),
                    merged_at: m.merged_at,
                })
                .collect();
            let d = self.insert_deploy(
                &st.worker,
                &st.landing,
                &st.commit,
                st.covers,
                &st.carries,
                &included,
                lanes::Purpose::Lane,
                None,
            )?;
            let why = format!(
                "{}'s lane deploys {} (landing {}), carrying {}",
                st.worker,
                st.commit,
                st.landing,
                st.carries.join(", ")
            );
            for id in &st.carries {
                if let Some((row, _)) = merged.iter().find(|(r, _)| &r.id == id) {
                    self.apply(row, &Seen::Start, Some(&why))?;
                }
            }
            out.push(d);
        }
        Ok(out)
    }

    /// A landing a Lander before lanes left `deploying` (its `deploys` list set) hands its
    /// deploy to the lanes: the Workers it already deployed count as deployed at its commit, its
    /// running job as a running deploy, and the lanes deploy the rest.
    fn adopt_legacy(&self) -> Result<()> {
        let legacy: Vec<Row> = self.db.rows(
            "SELECT * FROM landings WHERE state = 'deploying' AND deploys IS NOT NULL",
            vec![],
        )?;
        for row in legacy {
            let covers = row.merged_at.unwrap_or(row.updated_at);
            // What the old deploy lane carried: every merged landing up to this one.
            let carried: Vec<Row> = self.db.rows(
                "SELECT * FROM landings WHERE state IN ('merged', 'deploying') AND merged_at <= ?",
                vec![covers.into()],
            )?;
            let mut ids: Vec<String> = carried.iter().map(|r| r.id.clone()).collect();
            if !ids.contains(&row.id) {
                ids.push(row.id.clone());
            }
            let carries = serde_json::to_string(&ids)?;
            let union: std::collections::BTreeSet<String> = carried
                .iter()
                .chain(std::iter::once(&row))
                .flat_map(Row::workers)
                .collect();
            let affected = pigeon_core::lanes::Affected {
                docs_only: false,
                shared: false,
                smoke: false,
                workers: union,
                ordered: false,
            };
            let order: Vec<String> = self
                .settings
                .map
                .deploy_order(&affected)
                .iter()
                .map(|w| w.name.clone())
                .collect();
            let left = row.deploys();
            let t = now();
            let insert = |worker: &str, state: &str, job: Option<&str>, detail: &str| {
                self.db.exec(
                    "INSERT INTO deploys (worker, landing, commit_sha, covers, carries, job, state, detail, created_at, updated_at, finished_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    vec![
                        worker.into(),
                        row.id.as_str().into(),
                        row.commit_sha.as_str().into(),
                        covers.into(),
                        carries.as_str().into(),
                        opt(job),
                        state.into(),
                        detail.into(),
                        t.into(),
                        t.into(),
                        opt_i((state != "running").then_some(t)),
                    ],
                )
            };
            for w in order.iter().filter(|w| !left.contains(w)) {
                insert(w, "succeeded", None, "deployed before deploy lanes")?;
            }
            if let Some(job) = row.deploy_job.as_deref().filter(|j| !j.is_empty()) {
                let w = row
                    .deploy_worker
                    .clone()
                    .or_else(|| left.first().cloned())
                    .unwrap_or_else(|| CORE_WORKER.to_string());
                insert(&w, "running", Some(job), "started before deploy lanes")?;
            }
            self.db.exec(
                "UPDATE landings SET deploys = NULL WHERE id = ?",
                vec![row.id.as_str().into()],
            )?;
        }
        Ok(())
    }

    fn deploy(&self, id: i64) -> Result<Option<DeployRow>> {
        self.db
            .row("SELECT * FROM deploys WHERE id = ?", vec![id.into()])
    }

    /// One deploy's step; a failure on the gateway's side is tried again, then an error.
    async fn deploy_logged(&self, d: DeployRow) -> Result<()> {
        let id = d.id;
        if let Err(e) = self.deploy_step(d).await {
            console_error!("land: deploy {id}: {e}");
            let Some(d) = self.deploy(id)? else {
                return Ok(());
            };
            if d.tries + 1 >= STEP_TRIES {
                self.end_deploy(&d, lanes::DeployState::Error, &e.to_string())?;
            } else {
                self.db.exec(
                    "UPDATE deploys SET tries = tries + 1, detail = ?, updated_at = ? WHERE id = ? AND state = 'running'",
                    vec![format!("retrying: {e}").into(), now().into(), id.into()],
                )?;
            }
        }
        Ok(())
    }

    /// Ends a running deploy.
    fn end_deploy(&self, d: &DeployRow, to: lanes::DeployState, detail: &str) -> Result<()> {
        let t = now();
        self.db.exec(
            "UPDATE deploys SET state = ?, detail = ?, updated_at = ?, finished_at = ? WHERE id = ? AND state = 'running'",
            vec![
                to.name().into(),
                detail.into(),
                t.into(),
                t.into(),
                d.id.into(),
            ],
        )
    }

    /// Whether `dir` is in `commit`'s tree. A read that fails counts as present, so the Worker is
    /// deployed as before.
    async fn dir_present_at(&self, commit: &str, dir: &str) -> bool {
        let s = &self.settings;
        let read = async {
            let account = pigeon_core::account_from_env(&self.env, &s.parent).await?;
            let session =
                merge::Session::open(&account, &s.namespace, &s.repo, Access::Read).await?;
            let ids = git::path_ids(&session.remote, commit, &[dir]).await;
            session.close().await;
            ids
        };
        match read.await {
            Ok(ids) => ids.first().is_some_and(Option::is_some),
            Err(e) => {
                console_error!("land: reading {dir} at {commit}: {e}");
                true
            }
        }
    }

    /// Records lane deploy `d`, which stayed rate limited after every redeploy and ended in error
    /// with `detail`, as an incident in state [`lanes::RATE_LIMITED`]: infrastructure, so no
    /// rollback, no bisect and nobody guilty. It holds no lane and fails no landing by itself (the
    /// deploy is not marked as the incident's); the Worker's next green lane deploy covers it.
    fn record_rate_limited(&self, d: &DeployRow, detail: &str) -> Result<()> {
        let included = self.included_of(d)?;
        let good: Option<DeployRow> = self.db.row(
            "SELECT * FROM deploys WHERE worker = ? AND state = 'succeeded' AND id < ? ORDER BY id DESC LIMIT 1",
            vec![d.worker.as_str().into(), d.id.into()],
        )?;
        let last_good = good.map(|g| lanes::Good {
            commit: g.commit_sha,
            covers: g.covers,
        });
        let summary = format!(
            "{}'s deploy of {} stayed rate limited by Cloudflare after {} redeploys: infrastructure, not the change; no rollback, no bisect, no landing to blame; land again, or the Worker's next deploy covers it ({detail})",
            d.worker,
            d.commit_sha,
            lanes::RATE_LIMIT_REDEPLOYS
        );
        let t = now();
        self.db.exec(
            "INSERT INTO incidents (worker, failed_deploy, landing, last_good, included, state, summary, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                d.worker.as_str().into(),
                d.id.into(),
                d.landing.as_str().into(),
                opt(last_good
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
                    .as_deref()),
                serde_json::to_string(&included)?.into(),
                lanes::RATE_LIMITED.into(),
                summary.as_str().into(),
                t.into(),
                t.into(),
            ],
        )?;
        for l in &included {
            self.notify(&l.id, &watch::incident_text(&l.id, &d.worker, &summary))?;
        }
        Ok(())
    }

    /// The text of `ci/workers.toml` at `commit` through git: its text, no file, or why it could
    /// not be read.
    async fn map_text_at(&self, commit: &str) -> std::result::Result<Option<String>, String> {
        let s = &self.settings;
        let account = pigeon_core::account_from_env(&self.env, &s.parent)
            .await
            .map_err(|e| e.to_string())?;
        let session = merge::Session::open(&account, &s.namespace, &s.repo, Access::Read)
            .await
            .map_err(|e| e.to_string())?;
        let read = git::read_file(&session.remote, commit, merge::MAP_PATH).await;
        session.close().await;
        match read {
            Ok(Some(bytes)) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| e.to_string()),
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A running deploy's next step: its job submitted, or, once the job ended, its outcome
    /// and, when it passed, its Worker's `after` steps.
    async fn deploy_step(&self, d: DeployRow) -> Result<()> {
        let s = &self.settings;
        let Some(job) = &d.job else {
            // A rate-limited deploy waits out its backoff before its next job.
            if d.not_before.is_some_and(|t| now() < t) {
                return Ok(());
            }
            let landing = self
                .row(&d.landing)?
                .ok_or_else(|| bad(&format!("no landing {} to deploy", d.landing)))?;
            // The entry comes from the map at the deploy's commit; the compiled map is the
            // fallback, and the detail says which one answered.
            let text = self.map_text_at(&d.commit_sha).await;
            // A lane's Worker removed at its commit, entry and source, is skipped: nothing to
            // deploy, and nothing for its landings to wait for (docs/plans/lander-fixes.md, fix 3).
            if d.purpose() == lanes::Purpose::Lane
                && let Ok(Some(t)) = &text
                && let Ok(at_commit) = pigeon_core::lanes::parse(t)
                && at_commit.worker(&d.worker).is_none()
            {
                let dir = s.map.worker(&d.worker).map(|w| w.dir.clone());
                let present = match &dir {
                    Some(dir) => Some(self.dir_present_at(&d.commit_sha, dir).await),
                    None => None,
                };
                if let Some(why) =
                    lanes::removed_at(Some(&at_commit), &d.worker, dir.as_deref(), present)
                {
                    self.end_deploy(
                        &d,
                        lanes::DeployState::Succeeded,
                        &lanes::skipped_detail(&why),
                    )?;
                    return Ok(());
                }
            }
            let (worker, _, which) = lanes::resolve_entry(text, &d.worker, &s.map)
                .ok_or_else(|| bad(&format!("the map has no Worker `{}` to deploy", d.worker)))?;
            let mut submit = json!({
                "repo": s.repo_name(), "commit": d.commit_sha,
                "job": worker.deploy_job,
                "task": match d.purpose() {
                    lanes::Purpose::Lane => format!("land {} deploy {}", d.landing, worker.name),
                    p => format!(
                        "land {} deploy {} ({} for incident {})",
                        d.landing,
                        worker.name,
                        p.name(),
                        d.incident.unwrap_or_default()
                    ),
                },
            });
            if let Some(c) = &s.deploy_connector {
                submit["connector"] = json!(c);
            }
            let submitted = self.submit_ci(&landing, &submit).await?;
            let job = submitted.id.as_str();
            self.db.exec(
                "UPDATE deploys SET job = ?, script = ?, tries = 0, detail = ?, updated_at = ? WHERE id = ? AND state = 'running' AND job IS NULL",
                vec![
                    job.into(),
                    worker.script.as_str().into(),
                    format!(
                        "deploying {} at {}: job {job} submitted, {which}{}",
                        worker.script,
                        d.commit_sha,
                        if d.detail.as_deref().is_some_and(|x| x.contains(lanes::ENV_RETRY)) {
                            format!(" {}", lanes::ENV_RETRY)
                        } else {
                            String::new()
                        }
                    )
                    .into(),
                    now().into(),
                    d.id.into(),
                ],
            )?;
            // Each landing it carries (for an incident's redeploy, each landing the incident
            // judges) names it among its deploys, with the landings and commits it included:
            // its note's `deploy=` and `land_status`.
            let included = self.included_of(&d)?;
            let named =
                lanes::deploy_note(&worker.name, d.purpose(), job, &d.commit_sha, &included);
            let targets = match d.incident {
                Some(inc) => {
                    let found: Option<IncidentRow> = self
                        .db
                        .row("SELECT * FROM incidents WHERE id = ?", vec![inc.into()])?;
                    found.map(|i| i.included().into_iter().map(|l| l.id).collect())
                }
                None => None,
            }
            .unwrap_or_else(|| d.carries());
            for id in targets {
                self.db.exec(
                    "UPDATE landings SET deploy_job = CASE WHEN deploy_job IS NULL OR deploy_job = '' THEN ? ELSE deploy_job || ',' || ? END, updated_at = ? WHERE id = ?",
                    vec![
                        named.as_str().into(),
                        named.as_str().into(),
                        now().into(),
                        id.as_str().into(),
                    ],
                )?;
            }
            return Ok(());
        };
        let Some(end) = self.job_ended_code(job).await? else {
            return Ok(());
        };
        let (passed, exit_code) = (end.passed, end.exit_code);
        let compiled = s.map.worker(&d.worker);
        let script = d
            .script
            .clone()
            .or_else(|| compiled.map(|w| w.script.clone()))
            .ok_or_else(|| bad(&format!("the map has no Worker `{}` to deploy", d.worker)))?;
        let after = compiled.map(|w| w.after.clone()).unwrap_or_default();
        // A failure of the environment blames no landing: a lane deploy is deployed once more,
        // anything else ends in error.
        if !passed && let Some(why) = lanes::environment_fault(&end.state, end.exit_code, &end.tail)
        {
            let retried = d
                .detail
                .as_deref()
                .is_some_and(|x| x.contains(lanes::ENV_RETRY));
            let in_incident = d.incident.is_some() || d.purpose() != lanes::Purpose::Lane;
            // A rate limit is deployed again after a backoff, a few times, lane or incident
            // alike (docs/plans/lander-fixes.md, fix 1).
            let limited = end.state == "failed" && lanes::rate_limited(&end.tail).is_some();
            let step = if limited {
                lanes::rate_limit_step(d.rate_retries, &why)
            } else {
                lanes::environment_step(retried, in_incident, &why)
            };
            match step {
                lanes::EnvStep::Redeploy => self.db.exec(
                    "UPDATE deploys SET job = NULL, tries = 0, detail = ?, updated_at = ? WHERE id = ? AND state = 'running'",
                    vec![
                        format!(
                            "{}: environment fault ({why}); deploying once more at {} {}",
                            script,
                            d.commit_sha,
                            lanes::ENV_RETRY
                        )
                        .into(),
                        now().into(),
                        d.id.into(),
                    ],
                )?,
                lanes::EnvStep::RedeployAfter(wait) => self.db.exec(
                    "UPDATE deploys SET job = NULL, tries = 0, rate_retries = rate_retries + 1, not_before = ?, detail = ?, updated_at = ? WHERE id = ? AND state = 'running'",
                    vec![
                        (now() + wait).into(),
                        format!(
                            "{}: {why}; not this landing's fault; deploying again at {} in {} minutes (rate limit redeploy {} of {})",
                            script,
                            d.commit_sha,
                            wait / 60_000,
                            d.rate_retries + 1,
                            lanes::RATE_LIMIT_REDEPLOYS
                        )
                        .into(),
                        now().into(),
                        d.id.into(),
                    ],
                )?,
                lanes::EnvStep::Error(detail) => {
                    self.end_deploy(&d, lanes::DeployState::Error, &detail)?;
                    if limited && !in_incident {
                        self.record_rate_limited(&d, &detail)?;
                    }
                }
            }
            return Ok(());
        }
        let mut detail = format!("{script}: {}", end.detail);
        if let Some(c) = exit_code {
            detail.push_str(&format!(" (exit code {c})"));
        }
        let to = if passed {
            lanes::DeployState::Succeeded
        } else {
            lanes::DeployState::Failed
        };
        self.end_deploy(&d, to, &detail)?;
        // A lane's own deploy that failed is attributed: the Worker goes back to its last good
        // build and the landings it included are deployed one at a time (`drive_incidents`).
        // Nothing awaits between the two writes. The deploy job's own live check (core's
        // architecture page) fails the job, so it counts here; the `after` steps below are
        // best-effort and never do.
        if !passed && d.incident.is_none() && d.purpose() == lanes::Purpose::Lane {
            self.open_incident(&d)?;
        }
        // The first core deploy this Lander sees pass holds this Lander (main moves forward, and
        // the build running it is at or before that commit): core's rollbacks go no further back.
        if passed && d.worker == CORE_WORKER && d.purpose() == lanes::Purpose::Lane {
            let at = now();
            self.db.exec(
                "INSERT INTO settings (key, value, by_name, at) VALUES ('core_lander_since', ?, 'lander', ?) ON CONFLICT(key) DO NOTHING",
                vec![
                    json!({ "commit": d.commit_sha, "covers": d.covers })
                        .to_string()
                        .into(),
                    at.into(),
                ],
            )?;
        }
        // A green lane deploy closes the incidents it covers; best-effort.
        if passed
            && d.purpose() == lanes::Purpose::Lane
            && let Err(e) = self.close_covered()
        {
            console_error!("close_covered failed: {e}");
        }
        if passed {
            for step in &after {
                match step {
                    After::PortalSync => self.sync_portal(d.id, &detail).await,
                    After::ConnectorsSetup => self.setup_connectors(d.id, &d.commit_sha).await,
                    // The deploy job itself checks the live architecture page.
                    After::Architecture => {}
                }
            }
        }
        Ok(())
    }

    fn event(&self, id: &str, to: State, detail: Option<&str>) -> Result<()> {
        self.db.exec(
            "INSERT INTO events (landing, at, state, detail) VALUES (?, ?, ?, ?)",
            vec![id.into(), now().into(), to.name().into(), opt(detail)],
        )
    }

    /// Moves `row` on `seen`, if that moves it; writes the step and, at the end, the note.
    /// Answers whether it moved.
    fn apply(&self, row: &Row, seen: &Seen, detail: Option<&str>) -> Result<bool> {
        // Read afresh: the row may be stale after an await (a cancel in between). Nothing awaits
        // from here to the update, so the object takes no other request meanwhile.
        let Some(current) = self.row(&row.id)? else {
            return Ok(false);
        };
        if current.state != row.state {
            return Ok(false);
        }
        let row = &current;
        let Some(to) = state::next(row.state(), row.needs_approval(), seen) else {
            return Ok(false);
        };
        let t = now();
        // The lanes order merges by `merged_at`: strictly increasing, in main's order (merges
        // take one lock, one at a time).
        let merged_at = if to == State::Merged {
            #[derive(Deserialize)]
            struct M {
                m: Option<i64>,
            }
            let last = self
                .db
                .row::<M>("SELECT MAX(merged_at) AS m FROM landings", vec![])?
                .and_then(|m| m.m);
            Some(last.map_or(t, |l| t.max(l + 1)))
        } else {
            row.merged_at
        };
        self.db.exec(
            "UPDATE landings SET state = ?, detail = ?, tries = 0, updated_at = ?, finished_at = ?, awaiting_since = ?, merged_at = ? WHERE id = ? AND state = ?",
            vec![
                to.name().into(),
                opt(detail),
                t.into(),
                opt_i(to.terminal().then_some(t)),
                opt_i((to == State::AwaitingApproval).then_some(t).or(row.awaiting_since)),
                opt_i(merged_at),
                row.id.as_str().into(),
                row.state.as_str().into(),
            ],
        )?;
        self.event(&row.id, to, detail)?;
        if watch::notable(to) {
            let commit: String = row.commit_sha.chars().take(12).collect();
            let text = watch::event_text(
                &row.id,
                &row.branch,
                &commit,
                to.name(),
                detail.unwrap_or_default(),
            );
            self.notify(&row.id, &text)?;
        }
        if to.terminal() && row.main_at.is_empty() {
            // It never passed its checks: nothing to note, its commit may be no commit at all.
            self.db.exec(
                "UPDATE landings SET note_pushed = 1 WHERE id = ?",
                vec![row.id.as_str().into()],
            )?;
        } else if to.terminal() {
            let at = String::from(js_sys::Date::new(&(t as f64).into()).to_iso_string());
            let text = state::note(
                &row.id,
                to,
                &row.by_name,
                row.test_job.as_deref(),
                row.deploy_job.as_deref(),
                row.approved_by.as_deref(),
                &at,
            );
            // A commit landed more than once keeps the latest note.
            self.db.exec(
                "INSERT INTO notes (commit_sha, text) VALUES (?, ?) ON CONFLICT(commit_sha) DO UPDATE SET text = excluded.text",
                vec![row.commit_sha.as_str().into(), text.into()],
            )?;
            self.db.exec(
                "UPDATE landings SET note_pushed = 0, note_tries = 0 WHERE id = ?",
                vec![row.id.as_str().into()],
            )?;
        }
        Ok(true)
    }

    /// Runs the CI connectors' setup after deploy `id` (in a lane) passed, and adds what it did
    /// to the deploy's detail: `buildkite: created queue pigeon-linux-small, …`, `ok`, or why
    /// not. A setup that fails leaves the deploy passed; the connector then reads as not set up.
    async fn setup_connectors(&self, id: i64, deployed: &str) {
        if !self.settings.setup_connectors {
            return;
        }
        // Right after a deploy, the CI scheduler may still be the old build for a moment, and
        // so may this Lander: the expected build is the commit just deployed, not this Lander's
        // own. A scheduler of another build refuses, one older than the check answers without
        // its build, an older one still has no `setup`: wait for the deployed build's
        // scheduler, up to four minutes. A scheduler that checks never applies stale settings;
        // one older than the check may run its own, and is then asked again.
        let build = deployed;
        let mut answer = crate::ci::setup(&self.env, true, build).await;
        for _ in 0..12 {
            let stale = match &answer {
                Ok(v) => !crate::ci::same_build(v, build),
                Err(e) => {
                    let e = e.to_string();
                    e.contains("no operation `setup`") || e.contains("the CI scheduler is build")
                }
            };
            if !stale {
                break;
            }
            worker::Delay::from(std::time::Duration::from_secs(20)).await;
            answer = crate::ci::setup(&self.env, true, build).await;
        }
        let said = match answer {
            Ok(v) if crate::ci::same_build(&v, build) => v["reports"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, r)| format!("{name}: {}", crate::ci::setup_summary(r)))
                .collect::<Vec<_>>(),
            Ok(_) => vec!["CI setup: not run (the CI scheduler stayed on the old build; the next landing runs it)".into()],
            Err(e) => vec![format!("CI setup: not run ({e})")],
        };
        if said.is_empty() {
            return;
        }
        #[derive(serde::Deserialize)]
        struct D {
            detail: Option<String>,
        }
        let detail = match self
            .db
            .row::<D>("SELECT detail FROM deploys WHERE id = ?", vec![id.into()])
        {
            Ok(Some(d)) => d.detail.unwrap_or_default(),
            _ => String::new(),
        };
        let text = format!("{detail}; {}", said.join("; "));
        if let Err(e) = self.db.exec(
            "UPDATE deploys SET detail = ? WHERE id = ?",
            vec![text.into(), id.into()],
        ) {
            console_error!("CI setup after {id}: {e}");
        }
    }

    /// Syncs the portal's servers after deploy `id` (in a lane) passed, and adds how each went
    /// to its detail. A sync that fails leaves the deploy passed: the portal syncs on its own
    /// within hours, and the detail says so.
    async fn sync_portal(&self, id: i64, detail: &str) {
        let s = &self.settings;
        if s.portal_servers.is_empty() {
            return;
        }
        let account = pigeon_core::account_from_env(&self.env, &s.parent).await;
        let mut said = Vec::new();
        for server in &s.portal_servers {
            let synced = match &account {
                Ok(a) => pigeon_core::mcp_portal::sync(a, server).await,
                Err(e) => Err(Error::RustError(e.to_string())),
            };
            let outcome = match synced {
                Ok(v) => v["status"].as_str().unwrap_or("synced").to_string(),
                Err(e) => {
                    console_error!("portal sync of `{server}` after {id}: {e}");
                    format!("not synced ({e})")
                }
            };
            said.push(format!("portal server {server}: {outcome}"));
        }
        let text = format!("{detail}; {}", said.join("; "));
        if let Err(e) = self.db.exec(
            "UPDATE deploys SET detail = ? WHERE id = ?",
            vec![text.into(), id.into()],
        ) {
            console_error!("portal sync after {id}: {e}");
        }
    }

    /// A step that failed on the gateway's side: tried again, up to [`STEP_TRIES`], then an error.
    fn retry_or_fail(&self, id: &str, why: &str) -> Result<()> {
        let Some(row) = self.row(id)? else {
            return Ok(());
        };
        if row.tries + 1 >= STEP_TRIES {
            self.apply(&row, &Seen::Failed, Some(why))?;
        } else {
            self.db.exec(
                "UPDATE landings SET tries = tries + 1, detail = ? WHERE id = ?",
                vec![format!("retrying: {why}").into(), id.into()],
            )?;
        }
        Ok(())
    }

    /// A CI job's state, as its scheduler holds it, once it has ended: whether it passed, how,
    /// its state and output tail, and its exit code (kept in memory, then written into a
    /// deploy's detail as `(exit code N)`). The exit code is returned only when the job's state
    /// is exactly `failed`; for `timed_out`, `cancelled` and `error` it is `None`.
    async fn job_ended_code(&self, job: &str) -> Result<Option<JobEnd>> {
        let status = crate::ci::job_status(&self.env, job).await?;
        let state = status["state"].as_str().unwrap_or_default().to_string();
        let ended = matches!(
            state.as_str(),
            "succeeded" | "failed" | "cancelled" | "timed_out" | "error"
        );
        if !ended {
            return Ok(None);
        }
        let detail = match status["detail"].as_str() {
            Some(d) => format!("CI job {job}: {state}: {d}"),
            None => format!("CI job {job}: {state}"),
        };
        let exit_code = if state == "failed" {
            status["exit_code"].as_i64()
        } else {
            None
        };
        let tail: String = {
            let out = status["output"].as_str().unwrap_or_default();
            let skip = out.chars().count().saturating_sub(2000);
            out.chars().skip(skip).collect()
        };
        Ok(Some(JobEnd {
            passed: state == "succeeded",
            detail,
            exit_code,
            state,
            tail,
        }))
    }

    /// Submit through the existing CI capability and approval gate as the landing's caller.
    async fn submit_ci(&self, row: &Row, args: &Value) -> Result<SubmittedJob> {
        let answer = self.dispatch_as(&row.caller, "ci_submit", args).await?;
        let id = answer["id"]
            .as_str()
            .ok_or_else(|| bad("CI answered no job id"))?;
        Ok(SubmittedJob {
            id: id.to_string(),
            reused: answer["reused"].as_bool() == Some(true),
        })
    }

    /// Runs `tool` as the caller kept as JSON in `caller_json`, through the same dispatch as `/mcp`.
    async fn dispatch_as(&self, caller_json: &str, tool: &str, args: &Value) -> Result<Value> {
        let caller: Caller = serde_json::from_str(caller_json)?;
        let account = pigeon_core::account_from_env(&self.env, &self.settings.parent).await?;
        let capabilities = crate::split::all(
            &self.env,
            (self.hosting.capabilities)(),
            self.hosting.bindings,
        )
        .await;
        let hosted = Hosted {
            worker: self.hosting.worker,
            capabilities: &capabilities,
            sources: self.hosting.sources,
        };
        let cx = Context {
            account: &account,
            env: &self.env,
            hosted: &hosted,
            caller: Some(&caller),
            caller_note: None,
            instance: None,
            worker: None,
        };
        crate::dispatch(&cx, tool, args).await
    }

    /// The checks `land` leaves to the alarm: the branch's tip, that it is a fast-forward of
    /// main, and whether a person must approve it. A failed check ends the landing `refused`.
    async fn check(&self, row: Row) -> Result<()> {
        let s = &self.settings;
        // Landing itself is the policy's to allow (`land-and-deploy`).
        let branch = row
            .branch
            .strip_prefix("refs/heads/")
            .unwrap_or(&row.branch);
        let req = crate::policy::request(&row.by_name, Kind::Land, branch);
        let answer = crate::policy::decide(&git_proxy::vars(&s.main, state::PREFIX), &req);
        crate::policy::record(&self.env, "lander", &req, &answer, None).await?;
        if answer.decision != Decision::Allow {
            let why = format!(
                "the policy does not allow landing {branch}: {} ({})",
                answer.reason,
                answer.rule.as_deref().unwrap_or("no rule")
            );
            self.apply(&row, &Seen::CheckRefused, Some(&why))?;
            return Ok(());
        }
        let account = pigeon_core::account_from_env(&self.env, &s.parent).await?;
        let tip = git_proxy::tip(&account, &s.namespace, &s.repo, &row.branch).await?;
        let expected = (!row.commit_sha.is_empty()).then_some(row.commit_sha.as_str());
        if let Some(why) = tip_refusal(&row.branch, tip.as_deref(), expected) {
            self.apply(&row, &Seen::CheckRefused, Some(&why))?;
            return Ok(());
        }
        let Some(tip) = tip else {
            return Ok(());
        };
        let session = merge::Session::open(&account, &s.namespace, &s.repo, Access::Read).await?;
        let seen = merge::see(&session, &s.main, &tip).await;
        session.close().await;
        let seen = match seen? {
            Ok(seen) => seen,
            Err(why) => {
                self.apply(&row, &Seen::CheckRefused, Some(&why))?;
                return Ok(());
            }
        };
        // What the branch changed since its base on main decides what it needs: approvals for
        // the guarded paths among them, and no tests or deploy when they are docs alone.
        let changed = seen.changed_paths();
        let guarded = state::guarded_in(&changed);
        let vars = git_proxy::vars(&s.main, state::PREFIX);
        let mut decided = Vec::new();
        let approvals = state::approvals(&guarded, |req| {
            let a = crate::policy::decide(&vars, req);
            decided.push((req.clone(), a.clone()));
            a
        });
        for (req, a) in &decided {
            crate::policy::record(&self.env, "lander", req, a, None).await?;
        }
        let reason = state::approval_reason(self.approval_on()?, &approvals.passkey);
        let delegated = (!approvals.delegate.is_empty()).then(|| {
            format!(
                "; the person's delegate approves {}",
                approvals.delegate.join(", ")
            )
        });
        // A lockfile change in the workspace's own entries alone follows the crates' own
        // manifests, which pick their lanes.
        let for_lanes: Vec<&str> = changed
            .iter()
            .copied()
            .filter(|p| !(seen.lock_members_only && *p == pigeon_core::lanes::LOCK))
            .collect();
        let mut affected = seen.map.classify(&for_lanes);
        // A Worker the commit removes, entry and source, is not deployed.
        for (name, _) in &seen.removed_workers {
            affected.workers.remove(name);
        }
        let skipped = if seen.removed_workers.is_empty() {
            String::new()
        } else {
            format!(
                "; skipped {}: removed at this commit",
                seen.removed_workers
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let docs_only = affected.docs_only;
        let workers = serde_json::to_string(&affected.workers)?;
        let order_bits = (i64::from(affected.ordered) * lanes::ORDER_CORE_FIRST)
            | (i64::from(lanes::changes_proxy_first(&changed) || !seen.added_workers.is_empty())
                * lanes::ORDER_PROXY_FIRST);
        let on = if seen.forward() {
            format!("a fast-forward of main at {}", seen.main)
        } else {
            format!(
                "based on {}, main is at {} (the Lander rebases it when it merges)",
                seen.base, seen.main
            )
        };
        let what = if docs_only {
            "; docs only: no tests, no deploy".to_string()
        } else if affected.workers.is_empty() {
            format!("; {} paths changed: tests, no deploy", changed.len())
        } else {
            format!(
                "; {} paths changed, for {}{}",
                changed.len(),
                affected
                    .workers
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if affected.ordered && affected.workers.len() > 1 {
                    " (core first)"
                } else {
                    ""
                }
            )
        };
        // From here to the move nothing awaits; `apply` re-reads the row, so a cancel during the
        // awaits above wins.
        self.db.exec(
            "UPDATE landings SET commit_sha = ?, main_at = ?, base = ?, docs_only = ?, workers = ?, ordered = ?, approval_reason = ?, gate_digest = ?, updated_at = ? WHERE id = ? AND state = 'checking'",
            vec![
                tip.as_str().into(),
                seen.main.as_str().into(),
                seen.base.as_str().into(),
                i64::from(docs_only).into(),
                workers.into(),
                order_bits.into(),
                opt(reason.as_deref()),
                opt(seen.gate_digest.as_deref()),
                now().into(),
                row.id.as_str().into(),
            ],
        )?;
        self.apply(
            &row,
            &Seen::Checked,
            Some(&format!(
                "checked: {tip} is {on}{what}{skipped}{}",
                delegated.unwrap_or_default()
            )),
        )?;
        if let Some(r) = self.row(&row.id)?
            && r.state() == State::Testing
        {
            Box::pin(self.step(r)).await?;
        }
        Ok(())
    }

    /// Moves main to the landing's commit, holding the merge lock: by compare-and-swap when it
    /// is a fast-forward of main; otherwise rebased onto main by the Lander, and tested again
    /// first when main moved in what its tests cover.
    async fn merge(&self, session: &merge::Session<'_>, row: &Row) -> Result<()> {
        let s = &self.settings;
        let seen = match merge::see(session, &s.main, &row.commit_sha).await? {
            Ok(seen) => seen,
            Err(why) => {
                self.apply(row, &Seen::MergeRefused, Some(&why))?;
                return Ok(());
            }
        };
        let reached = |detail: &str| -> Result<bool> {
            let seen = if row.docs_only != 0 {
                Seen::DocsMerged
            } else {
                Seen::Merged
            };
            self.apply(row, &seen, Some(detail))
        };
        // Main already holds it: a retry after a lost answer, or a branch already in main.
        if seen.main == row.commit_sha || seen.base == row.commit_sha {
            reached("main already contains the commit")?;
            return Ok(());
        }
        if seen.forward() {
            if merge::push(session, &s.main, Some(&seen.main), &row.commit_sha, &[]).await? {
                reached(&format!(
                    "main moved from {} to {}",
                    seen.main, row.commit_sha
                ))?;
            } else {
                self.note_detail(row, "main moved while merging; trying again")?;
            }
            return Ok(());
        }
        let rebased = merge::rebase_onto_main(
            session,
            &seen,
            &row.commit_sha,
            attribution::LANDER,
            now() / 1000,
        )
        .await?;
        let r = match rebased {
            Ok(r) => r,
            Err(why) => {
                self.apply(row, &Seen::RebaseRefused, Some(&why))?;
                return Ok(());
            }
        };
        if r.already {
            self.db.exec(
                "UPDATE landings SET commit_sha = ? WHERE id = ? AND state = 'merging'",
                vec![r.commit.as_str().into(), row.id.as_str().into()],
            )?;
            reached(&format!("main at {} already holds its changes", r.commit))?;
            return Ok(());
        }
        let merged = if r.merged.is_empty() {
            String::new()
        } else {
            format!(", merging {} line by line", r.merged.join(", "))
        };
        // An approval holds across this rebase only for the same change to the guarded paths
        // (docs/plans/lander-fixes.md, fix 2); otherwise it is void, and the rebased commit is
        // tested again and waits for the person.
        let approved = row.approved_by.is_some();
        let holds = state::approval_holds(
            approved,
            row.approved_digest.as_deref(),
            r.gate_digest.as_deref(),
        );
        let void = approved && !holds;
        let merged = if holds {
            format!("{merged}; the approval holds: its gated changes are unchanged")
        } else if void {
            format!(
                "{merged}; the approval is void: its gated changes differ after the rebase, so it waits for the person again"
            )
        } else {
            merged
        };
        if r.retest || void {
            let name = merge::test_ref(&row.id);
            let old = merge::tip(session, &name).await?;
            if !merge::push(session, &name, old.as_deref(), &r.commit, &r.objects).await? {
                return Err(bad(&format!(
                    "the rebased commit {} could not be pushed to {name} for its tests",
                    r.commit
                )));
            }
            self.db.exec(
                "UPDATE landings SET commit_sha = ?, base = ?, gate_digest = ?, test_job = NULL, test_retries = 0 WHERE id = ? AND state = 'merging'",
                vec![
                    r.commit.as_str().into(),
                    seen.main.as_str().into(),
                    opt(r.gate_digest.as_deref()),
                    row.id.as_str().into(),
                ],
            )?;
            if void {
                self.db.exec(
                    "UPDATE landings SET approved_by = NULL, approved_digest = NULL WHERE id = ? AND state = 'merging'",
                    vec![row.id.as_str().into()],
                )?;
            }
            let why = if r.retest {
                "main moved in what its tests cover"
            } else {
                "its approval is void"
            };
            self.apply(
                row,
                &Seen::Retest,
                Some(&format!(
                    "{why}; rebased onto {} as {}{merged}, testing it again",
                    seen.main, r.commit
                )),
            )?;
            return Ok(());
        }
        if merge::push(session, &s.main, Some(&seen.main), &r.commit, &r.objects).await? {
            self.db.exec(
                "UPDATE landings SET commit_sha = ?, base = ?, gate_digest = ? WHERE id = ? AND state = 'merging'",
                vec![
                    r.commit.as_str().into(),
                    seen.main.as_str().into(),
                    opt(r.gate_digest.as_deref()),
                    row.id.as_str().into(),
                ],
            )?;
            reached(&format!(
                "rebased onto main at {} as {}{merged}; main moved in nothing its tests cover, so main moved to it",
                seen.main, r.commit
            ))?;
        } else {
            self.note_detail(row, "main moved while rebasing; trying again")?;
        }
        Ok(())
    }

    /// Sets a live landing's detail without moving it.
    fn note_detail(&self, row: &Row, detail: &str) -> Result<()> {
        self.db.exec(
            "UPDATE landings SET detail = ?, updated_at = ? WHERE id = ?",
            vec![detail.into(), now().into(), row.id.as_str().into()],
        )
    }

    /// The live landing's next step.
    async fn step(&self, row: Row) -> Result<()> {
        let s = &self.settings;
        match row.state() {
            State::Checking => self.check(row).await?,
            State::Testing if row.docs_only != 0 => {
                self.apply(
                    &row,
                    &Seen::JobEnded { passed: true },
                    Some("docs only: nothing to test"),
                )?;
            }
            State::Testing => {
                let Some(job) = &row.test_job else {
                    let submitted = self
                        .submit_ci(
                            &row,
                            &json!({
                                "repo": s.repo_name(), "commit": row.commit_sha,
                                "job": s.test_job, "task": format!("land {} test", row.id),
                                "reuse": true,
                            }),
                        )
                        .await?;
                    let job = submitted.id.as_str();
                    let detail = if submitted.reused {
                        format!("reused test job {job}")
                    } else {
                        format!("test job {job} submitted")
                    };
                    self.db.exec(
                        "UPDATE landings SET test_job = ?, detail = ?, updated_at = ? WHERE id = ? AND state = 'testing'",
                        vec![
                            job.into(),
                            detail.into(),
                            now().into(),
                            row.id.as_str().into(),
                        ],
                    )?;
                    return Ok(());
                };
                if let Some(end) = self.job_ended_code(job).await? {
                    let (passed, detail) = (end.passed, end.detail);
                    // Its runner was lost: the same commit is tested once more, in this same
                    // landing, so an approval it holds stays (docs/plans/lander-fixes.md, fix 2).
                    if !passed && state::test_rerun(&end.state, row.test_retries) {
                        self.db.exec(
                            "UPDATE landings SET test_job = NULL, test_retries = test_retries + 1, detail = ?, updated_at = ? WHERE id = ? AND state = 'testing'",
                            vec![
                                format!("{detail}; the runner was lost, so {} is tested once more", row.commit_sha).into(),
                                now().into(),
                                row.id.as_str().into(),
                            ],
                        )?;
                        return Ok(());
                    }
                    // The setting as it is now: turned on while the tests ran, it still holds
                    // this landing.
                    if passed && row.approval_reason.is_none() && self.approval_on()? {
                        self.db.exec(
                            "UPDATE landings SET approval_reason = ? WHERE id = ? AND approval_reason IS NULL",
                            vec![
                                state::approval_reason(true, &[]).unwrap_or_default().into(),
                                row.id.as_str().into(),
                            ],
                        )?;
                    }
                    if passed {
                        self.db.exec(
                            "UPDATE landings SET tested = commit_sha WHERE id = ? AND state = 'testing'",
                            vec![row.id.as_str().into()],
                        )?;
                    }
                    self.apply(&row, &Seen::JobEnded { passed }, Some(&detail))?;
                }
            }
            State::AwaitingApproval => {
                let since = row.awaiting_since.unwrap_or(row.updated_at);
                if now() - since > state::APPROVAL_MS {
                    self.apply(
                        &row,
                        &Seen::ApprovalLapsed,
                        Some("no approval within a day"),
                    )?;
                }
            }
            State::Merging => {
                let account = pigeon_core::account_from_env(&self.env, &s.parent).await?;
                let session =
                    merge::Session::open(&account, &s.namespace, &s.repo, Access::Write).await?;
                let merged = self.merge(&session, &row).await;
                session.close().await;
                merged?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Pushes every note kept as one notes commit over `refs/notes/land`'s tip, when one is
    /// waiting and has tries left.
    async fn push_notes(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let waiting: Option<N> = self.db.row(
            &format!(
                "SELECT COUNT(*) AS n FROM landings WHERE note_pushed = 0 AND note_tries < {NOTE_TRIES} AND finished_at IS NOT NULL"
            ),
            vec![],
        )?;
        if waiting.is_none_or(|w| w.n == 0) {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Note {
            commit_sha: String,
            text: String,
        }
        let notes: Vec<Note> = self.db.rows(
            "SELECT commit_sha, text FROM notes ORDER BY commit_sha",
            vec![],
        )?;
        let s = &self.settings;
        let account = pigeon_core::account_from_env(&self.env, &s.parent).await?;
        let session =
            merge::Session::open_for(&account, &s.namespace, &s.repo, Access::Write, 300).await?;
        let remote = &session.remote;
        let pairs: Vec<(String, String)> =
            notes.into_iter().map(|n| (n.commit_sha, n.text)).collect();
        let pushed = async {
            let refs = git::refs(remote, "git-receive-pack").await?;
            let tip = refs.refs.get(NOTES_REF).cloned();
            let (objects, id) =
                notes_commit(&pairs, tip.as_deref(), now() / 1000).map_err(|e| bad(&e))?;
            let update = Update {
                name: NOTES_REF.to_string(),
                old: tip.unwrap_or_else(|| ZERO.to_string()),
                new: id,
            };
            let status = git::push(remote, &refs.caps, &[update], &gitobj::pack(&objects)).await?;
            if status.iter().any(|x| x == &format!("ok {NOTES_REF}")) {
                Ok(())
            } else {
                Err(bad(&format!("push of {NOTES_REF}: {}", status.join("; "))))
            }
        }
        .await;
        session.close().await;
        let column = if pushed.is_ok() {
            "note_pushed = 1"
        } else {
            "note_tries = note_tries + 1"
        };
        self.db.exec(
            &format!(
                "UPDATE landings SET {column} WHERE note_pushed = 0 AND finished_at IS NOT NULL"
            ),
            vec![],
        )?;
        pushed
    }

    /// Whether test refs may still need deleting: the one-time sweep is not done, or a landing
    /// has ended since the last clean pass. The `settings` row `test_refs_swept` is the marker;
    /// its value is when the last clean pass began.
    fn test_refs_pending(&self) -> Result<bool> {
        #[derive(Deserialize)]
        struct V {
            value: String,
        }
        let v: Option<V> = self.db.row(
            "SELECT value FROM settings WHERE key = 'test_refs_swept'",
            vec![],
        )?;
        let Some(v) = v else {
            return Ok(true);
        };
        let since = v.value.parse::<i64>().unwrap_or(0);
        let ended: Option<Value> = self.db.row(
            "SELECT id FROM landings WHERE finished_at >= ? LIMIT 1",
            vec![since.into()],
        )?;
        Ok(ended.is_some())
    }

    /// Deletes the test refs of landings that are not live, at most `TEST_REF_BATCH` per call.
    /// Best-effort: a refused delete is counted in `settings` and given up on after
    /// `TEST_REF_TRIES`. Once nothing is left, records the sweep as done.
    async fn delete_test_refs(&self) -> Result<()> {
        if !self.test_refs_pending()? {
            return Ok(());
        }
        let started = now();
        #[derive(Deserialize)]
        struct Id {
            id: String,
        }
        let live: Vec<Id> = self
            .db
            .rows("SELECT id FROM landings WHERE finished_at IS NULL", vec![])?;
        let s = &self.settings;
        let account = pigeon_core::account_from_env(&self.env, &s.parent).await?;
        let session = merge::Session::open(&account, &s.namespace, &s.repo, Access::Write).await?;
        let found = async {
            let mut left = 0;
            let mut tried = 0;
            for (name, tip) in merge::test_refs(&session).await? {
                let id = merge::test_ref_id(&name).unwrap_or_default().to_string();
                if live.iter().any(|l| l.id == id) {
                    continue;
                }
                let key = format!("test_ref_fail:{id}");
                #[derive(Deserialize)]
                struct V {
                    value: String,
                }
                let fails: Option<V> = self
                    .db
                    .row("SELECT value FROM settings WHERE key = ?", vec![key.as_str().into()])?;
                let fails = fails.and_then(|v| v.value.parse::<i64>().ok()).unwrap_or(0);
                if fails >= TEST_REF_TRIES {
                    continue;
                }
                if tried >= TEST_REF_BATCH {
                    left += 1;
                    continue;
                }
                tried += 1;
                let deleted = merge::delete_test_ref(&session, &name, &tip).await;
                if !matches!(deleted, Ok(true)) {
                    match &deleted {
                        Ok(_) => console_error!("land: the remote kept {name}"),
                        Err(e) => console_error!("land: deleting {name}: {e}"),
                    }
                    self.db.exec(
                        "INSERT INTO settings (key, value, by_name, at) VALUES (?, '1', 'lander', ?) ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1, at = excluded.at",
                        vec![key.as_str().into(), now().into()],
                    )?;
                    if fails + 1 < TEST_REF_TRIES {
                        left += 1;
                    }
                }
            }
            Ok::<i64, worker::Error>(left)
        }
        .await;
        session.close().await;
        if found? == 0 {
            self.db.exec(
                "INSERT INTO settings (key, value, by_name, at) VALUES ('test_refs_swept', ?, 'lander', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, at = excluded.at",
                vec![started.to_string().into(), now().into()],
            )?;
        }
        Ok(())
    }

    /// Makes run `run` (with its `caller` JSON) a watcher of landing `landing`.
    fn watch(&self, landing: &str, run: &str, caller: &str) -> Result<()> {
        self.db.exec(
            "INSERT OR REPLACE INTO watchers (landing, run, caller) VALUES (?, ?, ?)",
            vec![landing.into(), run.into(), caller.into()],
        )
    }

    /// Whether an incident included landing `id`.
    fn has_incident(&self, id: &str) -> Result<bool> {
        let like = format!("%\"{id}\"%");
        let found: Option<Value> = self.db.row(
            "SELECT id FROM incidents WHERE included LIKE ? LIMIT 1",
            vec![like.as_str().into()],
        )?;
        Ok(found.is_some())
    }

    /// Queues `text` for each run that watches landing `landing`.
    fn notify(&self, landing: &str, text: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct W {
            run: String,
            caller: String,
        }
        let watchers: Vec<W> = self.db.rows(
            "SELECT run, caller FROM watchers WHERE landing = ?",
            vec![landing.into()],
        )?;
        for w in watchers {
            self.db.exec(
                "INSERT INTO run_events (run, caller, text, tries, at) VALUES (?, ?, ?, 0, ?)",
                vec![w.run.into(), w.caller.into(), text.into(), now().into()],
            )?;
        }
        Ok(())
    }

    /// Tells runs the events queued for them, oldest first, a few per alarm: a note to the run as
    /// the caller that watched. A run that is gone, or an event tried too often, is dropped.
    async fn deliver_events(&self) -> Result<()> {
        #[derive(Deserialize)]
        struct Ev {
            id: i64,
            run: String,
            caller: String,
            text: String,
            tries: i64,
        }
        let events: Vec<Ev> = self.db.rows(
            "SELECT id, run, caller, text, tries FROM run_events ORDER BY id LIMIT ?",
            vec![watch::EVENT_BATCH.into()],
        )?;
        for ev in events {
            let args = json!({ "run": ev.run, "text": ev.text, "event": true });
            match self.dispatch_as(&ev.caller, "ai_agents_note", &args).await {
                Ok(_) => self
                    .db
                    .exec("DELETE FROM run_events WHERE id = ?", vec![ev.id.into()])?,
                Err(e) => {
                    let why = e.to_string();
                    if watch::drop_event(&why, ev.tries) {
                        self.db
                            .exec("DELETE FROM run_events WHERE id = ?", vec![ev.id.into()])?;
                    } else {
                        self.db.exec(
                            "UPDATE run_events SET tries = tries + 1 WHERE id = ?",
                            vec![ev.id.into()],
                        )?;
                    }
                    console_error!("land: telling run {}: {why}", ev.run);
                }
            }
        }
        Ok(())
    }

    async fn reschedule(&self) -> Result<()> {
        // A deploy runs on after every landing it carried has ended (another lane failed).
        let running: Option<Value> = self.db.row(
            "SELECT id FROM deploys WHERE state = 'running' UNION ALL SELECT id FROM incidents WHERE state = 'open' LIMIT 1",
            vec![],
        )?;
        let live = self.live()?.is_some() || running.is_some();
        let notes: Option<Value> = self.db.row(
            &format!(
                "SELECT id FROM landings WHERE note_pushed = 0 AND note_tries < {NOTE_TRIES} AND finished_at IS NOT NULL LIMIT 1"
            ),
            vec![],
        )?;
        let events: Option<Value> = self.db.row("SELECT id FROM run_events LIMIT 1", vec![])?;
        let storage = self.state.storage();
        if live || notes.is_some() || events.is_some() || self.test_refs_pending().unwrap_or(false)
        {
            let at = now() + state::POLL_MS;
            let current = storage.get_alarm().await?;
            if current.is_none_or(|c| c > at) {
                storage.set_alarm(state::POLL_MS).await?;
            }
        } else {
            storage.delete_alarm().await?;
        }
        Ok(())
    }
}

/// The objects of a notes commit over `notes` (`(commit id, text)`) with `parent` the ref's tip:
/// the blobs, the flat tree named by commit id, then the commit; and the commit's id.
pub fn notes_commit(
    notes: &[(String, String)],
    parent: Option<&str>,
    unix_secs: i64,
) -> std::result::Result<(Vec<Object>, String), String> {
    let mut objects: Vec<Object> = Vec::new();
    let mut entries: Vec<(String, String)> = Vec::new();
    for (commit, text) in notes {
        if !git::is_id(commit) {
            return Err(format!("not a commit id: {commit:?}"));
        }
        let blob = Object::blob(text.as_bytes());
        let id = blob.id();
        if !objects.iter().any(|o| o.id() == id) {
            objects.push(blob);
        }
        entries.push((commit.to_ascii_lowercase(), id));
    }
    let tree = Object::tree(&entries)?;
    let tree_id = tree.id();
    objects.push(tree);
    let parents: Vec<String> = parent.iter().map(|p| p.to_string()).collect();
    let message = format!("land: {} notes", notes.len());
    let commit = Object::commit(
        &tree_id,
        &parents,
        &attribution::author(),
        attribution::LANDER,
        unix_secs,
        &message,
    )?;
    let id = commit.id();
    objects.push(commit);
    Ok((objects, id))
}

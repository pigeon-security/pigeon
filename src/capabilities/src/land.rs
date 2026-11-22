//! Landing: a session's branch to `main` and to the deployed platform, with no person in between
//! unless one is asked for. `land {branch}` answers at once, and the Lander's alarm checks the
//! branch's tip is a fast-forward of `main` under `refs/heads/claude/` (a failed check ends it
//! `refused`), then runs CI job `test` at it, waits for a person's passkey approval
//! when the setting asks for one or the commit changes what the gate itself runs, moves `main`
//! to it through the platform's own git path, and runs CI job `deploy` at it, which uploads the
//! platform through the Cloudflare API proxy with its own job token. Every step is reported by
//! `land_status` and the result is noted on the commit under `refs/notes/land`.
//!
//! Landings live in the `Lander`, a Durable Object with SQLite storage whose alarm follows the
//! live one; one landing is live at a time. The deploy job's token reaches `/cf/` only while
//! its landing is `deploying` and only for its commit ([`deploy_caller`]). Who may land: a
//! person, or an agent linked to one.

pub mod merge;
pub mod state;

use pigeon_core::artifacts::{self, Access};
use pigeon_core::durable::{self, Db, opt};
use pigeon_core::git::{self, Remote, Update, ZERO};
use pigeon_core::gitobj::{self, Object};
use pigeon_core::lanes::After;
use pigeon_core::mcp::Tool;
use pigeon_core::policy::{Decision, Kind};
use pigeon_core::webauthn::random;
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{
    Env, Error, Request, Response, Result, SqlStorageValue, State as ObjectState, console_error,
    js_sys,
};

use crate::console::sections::{Section, Tab};
use crate::git_proxy;
use crate::proxy::ProxyCaller;
use crate::users::{Caller, User};
use crate::{Call, Capability, Context, Footprint, Hosted, arg, unknown};
use state::{Seen, State};

/// The Lander's binding, and the one object of it.
pub const BINDING: &str = "LANDER";
pub const OBJECT: &str = "lander";
/// The ref landing notes live under.
pub const NOTES_REF: &str = "refs/notes/land";
/// The author and committer of notes commits.
const WHO: &str = "pigeon <land@pigeon.dev>";
/// How many times a step that failed on the gateway's side is tried before the landing is an
/// error.
const STEP_TRIES: i64 = 5;
/// How many times a note push is tried.
const NOTE_TRIES: i64 = 10;
/// The CI scheduler, whose job states the Lander follows.
const CI_BINDING: &str = "CI_SCHEDULER";
const CI_OBJECT: &str = "scheduler";

fn now() -> i64 {
    js_sys::Date::now() as i64
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
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

/// The proxy caller a CI job token stands for at `/cf/`, when it is the deploy job of the live
/// landing, in `deploying`, for the commit its grant names; `None` otherwise. The interface the
/// Cloudflare API proxy calls.
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
                "caller": caller, "by": by,
            }),
        )
        .await?;
        lander(cx.env, "status", json!({ "id": id })).await
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

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "land",
                description: "Land a session branch under claude/. Answers at once with the \
                              landing in `checking`, and many landings run at once: each is \
                              checked (its base on main, what it changed, whether a person \
                              must approve) and tested on its own, outside any lock; a failed \
                              check ends it `refused` with the reason. Once tested (and \
                              approved where needed) it is `ready` for the merge lock: main \
                              moves to it by compare-and-swap, and when main moved meanwhile \
                              the Lander rebases the branch itself (tested again only when \
                              main moved in what its tests cover; `rebase_refused` when both \
                              changed the same file in ways that do not merge). Then it is \
                              `merged`, and the deploy lane deploys the newest merged commit. \
                              A docs-only landing needs no tests and no deploy. Never \
                              force-push to make a landing fit. Follow it with land_status, \
                              which shows the queue.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "branch": { "type": "string", "description": "Such as `claude/my-change`." },
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
                              on the commit.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } }
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
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "land" => self.land(cx, args).await,
                "land_status" => {
                    lander(cx.env, "status", json!({ "id": args["id"].as_str() })).await
                }
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
                other => Err(unknown(other)),
            }
        })
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Approvals,
            id: "approvals",
            title: "Waiting for you",
            description: "Only what cannot go ahead without your passkey.",
            order: 1,
            cost: None,
        }]
    }

    /// The landings waiting for a passkey.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        _query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move { Ok(json!({ "landings": awaiting(cx.env).await? })) })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            repos: vec![self.settings.repo_name()],
            ..Footprint::default()
        }
    }
}

// ---- the Lander ----

/// The Lander's schema, one statement per migration, applied in order and never edited.
const MIGRATIONS: [&str; 11] = [
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
];

/// What the Lander needs from the hosting Worker, beside the settings: the capabilities it
/// runs CI's tools through, as `/mcp` would, and the sources they were built from.
pub struct Hosting {
    pub worker: &'static str,
    pub capabilities: fn() -> Vec<Box<dyn Capability>>,
    pub sources: &'static [(&'static str, &'static str)],
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

    /// The alarm: each live landing's next step, oldest first; then the merge lock and the
    /// deploy lane go to whoever waits for them; then notes still to push, and the next alarm.
    pub async fn alarm(&self) -> Result<Response> {
        for row in self.all_live()? {
            self.step_logged(row).await?;
        }
        if let Some(row) = self.take_merge_lock()? {
            self.step_logged(row).await?;
        }
        if let Some(row) = self.take_deploy_lane()? {
            self.step_logged(row).await?;
        }
        if let Err(e) = self.push_notes().await {
            console_error!("land: pushing notes: {e}");
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
                out["queue"] = json!(
                    self.all_live()?
                        .iter()
                        .map(|r| json!({ "id": r.id, "branch": r.branch, "state": r.state }))
                        .collect::<Vec<_>>()
                );
                Ok(out)
            }
            "list" => {
                let limit = a["limit"].as_i64().unwrap_or(10).clamp(1, 50);
                let rows: Vec<Row> = self.db.rows(
                    "SELECT * FROM landings ORDER BY created_at DESC LIMIT ?",
                    vec![limit.into()],
                )?;
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
                self.db.exec(
                    "UPDATE landings SET approved_by = ? WHERE id = ?",
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

    /// The deploy lane for the newest `merged` landing, when no deploy is running: that
    /// landing, now `deploying`. Main moves forward only, so its commit holds every older
    /// merged landing's too.
    fn take_deploy_lane(&self) -> Result<Option<Row>> {
        let busy: Option<Row> = self.db.row(
            "SELECT * FROM landings WHERE state = 'deploying' LIMIT 1",
            vec![],
        )?;
        if busy.is_some() {
            return Ok(None);
        }
        let next: Option<Row> = self.db.row(
            "SELECT * FROM landings WHERE state = 'merged' ORDER BY merged_at DESC, created_at DESC LIMIT 1",
            vec![],
        )?;
        let Some(row) = next else {
            return Ok(None);
        };
        // Main moves forward only, so the newest merged commit holds every older merged
        // landing's too: it deploys every Worker any of them affects, so none is lost.
        let carried: Vec<Row> = self.db.rows(
            "SELECT * FROM landings WHERE state = 'merged' AND merged_at <= ?",
            vec![row.merged_at.unwrap_or(i64::MAX).into()],
        )?;
        let union: std::collections::BTreeSet<String> =
            carried.iter().flat_map(Row::workers).collect();
        let affected = pigeon_core::lanes::Affected {
            docs_only: false,
            shared: false,
            smoke: false,
            workers: union,
        };
        let mut order: Vec<String> = self
            .settings
            .map
            .deploy_order(&affected)
            .iter()
            .map(|w| w.name.clone())
            .collect();
        if order.is_empty() {
            order.push("core".to_string());
        }
        self.db.exec(
            "UPDATE landings SET deploys = ?, deploy_worker = NULL, deploy_job = NULL WHERE id = ? AND state = 'merged'",
            vec![serde_json::to_string(&order)?.into(), row.id.as_str().into()],
        )?;
        let why = format!("took the deploy lane: {}", order.join(", "));
        self.apply(&row, &Seen::Start, Some(&why))?;
        self.row(&row.id)
    }

    /// The merged landings older than `row`, which its deploy carried: `seen` moves each.
    fn settle_older(&self, row: &Row, seen: &Seen, detail: &str) -> Result<()> {
        let older: Vec<Row> = self.db.rows(
            "SELECT * FROM landings WHERE state = 'merged' AND merged_at <= ? AND id != ?",
            vec![
                row.merged_at.unwrap_or(i64::MAX).into(),
                row.id.as_str().into(),
            ],
        )?;
        for o in older {
            self.apply(&o, seen, Some(detail))?;
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
        self.db.exec(
            "UPDATE landings SET state = ?, detail = ?, tries = 0, updated_at = ?, finished_at = ?, awaiting_since = ?, merged_at = ? WHERE id = ? AND state = ?",
            vec![
                to.name().into(),
                opt(detail),
                t.into(),
                opt_i(to.terminal().then_some(t)),
                opt_i((to == State::AwaitingApproval).then_some(t).or(row.awaiting_since)),
                opt_i((to == State::Merged).then_some(t).or(row.merged_at)),
                row.id.as_str().into(),
                row.state.as_str().into(),
            ],
        )?;
        self.event(&row.id, to, detail)?;
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

    /// Runs the CI connectors' setup after landing `id` deployed, and adds what it did to the
    /// detail: `buildkite: created queue pigeon-linux-small, …`, `ok`, or why not. A setup that
    /// fails leaves the landing landed; the connector then reads as not set up.
    async fn setup_connectors(&self, id: &str, deployed: &str) {
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
            .row::<D>("SELECT detail FROM landings WHERE id = ?", vec![id.into()])
        {
            Ok(Some(d)) => d.detail.unwrap_or_default(),
            _ => String::new(),
        };
        let text = format!("{detail}; {}", said.join("; "));
        if let Err(e) = self.db.exec(
            "UPDATE landings SET detail = ? WHERE id = ?",
            vec![text.into(), id.into()],
        ) {
            console_error!("CI setup after {id}: {e}");
        }
    }

    /// Syncs the portal's servers after landing `id` deployed, and adds how each went to its
    /// detail. A sync that fails leaves the landing landed: the portal syncs on its own within
    /// hours, and the detail says so.
    async fn sync_portal(&self, id: &str, detail: &str) {
        let s = &self.settings;
        if s.portal_servers.is_empty() {
            return;
        }
        let account = crate::ci::scheduler::account(&self.env, &s.parent).await;
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
            "UPDATE landings SET detail = ? WHERE id = ?",
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

    /// A CI job's state, as its scheduler holds it: `Some(passed)` once it has ended.
    async fn job_ended(&self, job: &str) -> Result<Option<(bool, String)>> {
        let status = durable::call(
            &self.env,
            CI_BINDING,
            CI_OBJECT,
            "status",
            &json!({ "id": job }),
        )
        .await?;
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
        Ok(Some((state == "succeeded", detail)))
    }

    /// Runs `tool` as the landing's caller, through the same dispatch as `/mcp`.
    async fn dispatch(&self, row: &Row, tool: &str, args: &Value) -> Result<Value> {
        let caller: Caller = serde_json::from_str(&row.caller)?;
        let account = crate::ci::scheduler::account(&self.env, &self.settings.parent).await?;
        let capabilities = (self.hosting.capabilities)();
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
        crate::policy::record(&self.env, "lander", &req, &answer).await?;
        if answer.decision != Decision::Allow {
            let why = format!(
                "the policy does not allow landing {branch}: {} ({})",
                answer.reason,
                answer.rule.as_deref().unwrap_or("no rule")
            );
            self.apply(&row, &Seen::CheckRefused, Some(&why))?;
            return Ok(());
        }
        let account = crate::ci::scheduler::account(&self.env, &s.parent).await?;
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
            crate::policy::record(&self.env, "lander", req, a).await?;
        }
        let reason = state::approval_reason(self.approval_on()?, &approvals.passkey);
        let delegated = (!approvals.delegate.is_empty()).then(|| {
            format!(
                "; the person's delegate approves {}",
                approvals.delegate.join(", ")
            )
        });
        let affected = seen.map.classify(&changed);
        let docs_only = affected.docs_only;
        let workers = serde_json::to_string(&affected.workers)?;
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
        } else {
            format!(
                "; {} paths changed, for {}",
                changed.len(),
                affected
                    .workers
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        // From here to the move nothing awaits; `apply` re-reads the row, so a cancel during the
        // awaits above wins.
        self.db.exec(
            "UPDATE landings SET commit_sha = ?, main_at = ?, base = ?, docs_only = ?, workers = ?, approval_reason = ?, updated_at = ? WHERE id = ? AND state = 'checking'",
            vec![
                tip.as_str().into(),
                seen.main.as_str().into(),
                seen.base.as_str().into(),
                i64::from(docs_only).into(),
                workers.into(),
                opt(reason.as_deref()),
                now().into(),
                row.id.as_str().into(),
            ],
        )?;
        self.apply(
            &row,
            &Seen::Checked,
            Some(&format!(
                "checked: {tip} is {on}{what}{}",
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
        let rebased =
            merge::rebase_onto_main(session, &seen, &row.commit_sha, WHO, now() / 1000).await?;
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
        if r.retest {
            let name = merge::test_ref(&row.id);
            let old = merge::tip(session, &name).await?;
            if !merge::push(session, &name, old.as_deref(), &r.commit, &r.objects).await? {
                return Err(bad(&format!(
                    "the rebased commit {} could not be pushed to {name} for its tests",
                    r.commit
                )));
            }
            self.db.exec(
                "UPDATE landings SET commit_sha = ?, base = ?, test_job = NULL WHERE id = ? AND state = 'merging'",
                vec![
                    r.commit.as_str().into(),
                    seen.main.as_str().into(),
                    row.id.as_str().into(),
                ],
            )?;
            self.apply(
                row,
                &Seen::Retest,
                Some(&format!(
                    "main moved in what its tests cover; rebased onto {} as {}{merged}, testing it again",
                    seen.main, r.commit
                )),
            )?;
            return Ok(());
        }
        if merge::push(session, &s.main, Some(&seen.main), &r.commit, &r.objects).await? {
            self.db.exec(
                "UPDATE landings SET commit_sha = ?, base = ? WHERE id = ? AND state = 'merging'",
                vec![
                    r.commit.as_str().into(),
                    seen.main.as_str().into(),
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
                    let job = self
                        .dispatch(
                            &row,
                            "ci_submit",
                            &json!({
                                "repo": s.repo_name(), "commit": row.commit_sha,
                                "job": s.test_job, "task": format!("land {} test", row.id),
                            }),
                        )
                        .await?;
                    let job = job["id"]
                        .as_str()
                        .ok_or_else(|| bad("CI answered no job id"))?;
                    self.db.exec(
                        "UPDATE landings SET test_job = ?, detail = ?, updated_at = ? WHERE id = ? AND state = 'testing'",
                        vec![
                            job.into(),
                            format!("test job {job} submitted").into(),
                            now().into(),
                            row.id.as_str().into(),
                        ],
                    )?;
                    return Ok(());
                };
                if let Some((passed, detail)) = self.job_ended(job).await? {
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
                let account = crate::ci::scheduler::account(&self.env, &s.parent).await?;
                let session =
                    merge::Session::open(&account, &s.namespace, &s.repo, Access::Write).await?;
                let merged = self.merge(&session, &row).await;
                session.close().await;
                merged?;
            }
            State::Deploying => {
                let queue = row.deploys();
                let name = queue.first().cloned().unwrap_or_else(|| "core".to_string());
                let worker = s
                    .map
                    .worker(&name)
                    .ok_or_else(|| bad(&format!("the map has no Worker `{name}` to deploy")))?
                    .clone();
                match &row.deploy_job {
                    None => {
                        let mut submit = json!({
                            "repo": s.repo_name(), "commit": row.commit_sha,
                            "job": worker.deploy_job,
                            "task": format!("land {} deploy {}", row.id, worker.name),
                        });
                        if let Some(c) = &s.deploy_connector {
                            submit["connector"] = json!(c);
                        }
                        let job = self.dispatch(&row, "ci_submit", &submit).await?;
                        let job = job["id"]
                            .as_str()
                            .ok_or_else(|| bad("CI answered no job id"))?;
                        self.db.exec(
                            "UPDATE landings SET deploy_job = ?, deploy_worker = ?, detail = ?, updated_at = ? WHERE id = ?",
                            vec![
                                job.into(),
                                worker.name.as_str().into(),
                                format!("deploying {}: job {job} submitted", worker.script).into(),
                                now().into(),
                                row.id.as_str().into(),
                            ],
                        )?;
                    }
                    Some(job) => {
                        let Some((passed, detail)) = self.job_ended(job).await? else {
                            return Ok(());
                        };
                        let detail = format!("{}: {detail}", worker.script);
                        if !passed {
                            self.apply(&row, &Seen::JobEnded { passed }, Some(&detail))?;
                            self.settle_older(
                                &row,
                                &Seen::JobEnded { passed: false },
                                &format!(
                                    "the deploy of landing {}, which carried it, failed",
                                    row.id
                                ),
                            )?;
                            return Ok(());
                        }
                        let rest: Vec<String> = queue.iter().skip(1).cloned().collect();
                        if rest.is_empty() {
                            self.apply(&row, &Seen::JobEnded { passed }, Some(&detail))?;
                            self.settle_older(
                                &row,
                                &Seen::Superseded,
                                &format!("deployed with landing {}", row.id),
                            )?;
                        } else {
                            self.db.exec(
                                "UPDATE landings SET deploys = ?, deploy_job = NULL, deploy_worker = NULL, detail = ?, updated_at = ? WHERE id = ? AND state = 'deploying'",
                                vec![
                                    serde_json::to_string(&rest)?.into(),
                                    format!("{detail}; next {}", rest.join(", ")).into(),
                                    now().into(),
                                    row.id.as_str().into(),
                                ],
                            )?;
                        }
                        for step in &worker.after {
                            match step {
                                After::PortalSync => self.sync_portal(&row.id, &detail).await,
                                After::ConnectorsSetup => {
                                    self.setup_connectors(&row.id, &row.commit_sha).await;
                                }
                                // The deploy job itself checks the live architecture page.
                                After::Architecture => {}
                            }
                        }
                    }
                }
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
        let account = crate::ci::scheduler::account(&self.env, &s.parent).await?;
        let found = artifacts::repo(&account, &s.namespace, &s.repo)
            .await?
            .ok_or_else(|| bad("no repository to note"))?;
        let minted = artifacts::mint(&account, &s.namespace, &s.repo, Access::Write, 300).await?;
        let remote = Remote {
            url: found.remote.trim_end_matches('/').to_string(),
            authorization: format!("Bearer {}", minted.value),
        };
        let pairs: Vec<(String, String)> =
            notes.into_iter().map(|n| (n.commit_sha, n.text)).collect();
        let pushed = async {
            let refs = git::refs(&remote, "git-receive-pack").await?;
            let tip = refs.refs.get(NOTES_REF).cloned();
            let (objects, id) =
                notes_commit(&pairs, tip.as_deref(), now() / 1000).map_err(|e| bad(&e))?;
            let update = Update {
                name: NOTES_REF.to_string(),
                old: tip.unwrap_or_else(|| ZERO.to_string()),
                new: id,
            };
            let status = git::push(&remote, &refs.caps, &[update], &gitobj::pack(&objects)).await?;
            if status.iter().any(|x| x == &format!("ok {NOTES_REF}")) {
                Ok(())
            } else {
                Err(bad(&format!("push of {NOTES_REF}: {}", status.join("; "))))
            }
        }
        .await;
        if let Some(id) = &minted.id
            && let Err(e) = artifacts::revoke(&account, &s.namespace, &s.repo, id).await
        {
            console_error!("land: revoking the notes push token: {e}");
        }
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

    async fn reschedule(&self) -> Result<()> {
        let live = self.live()?.is_some();
        let notes: Option<Value> = self.db.row(
            &format!(
                "SELECT id FROM landings WHERE note_pushed = 0 AND note_tries < {NOTE_TRIES} AND finished_at IS NOT NULL LIMIT 1"
            ),
            vec![],
        )?;
        let storage = self.state.storage();
        if live || notes.is_some() {
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
    let commit = Object::commit(&tree_id, &parents, WHO, unix_secs, &message)?;
    let id = commit.id();
    objects.push(commit);
    Ok((objects, id))
}

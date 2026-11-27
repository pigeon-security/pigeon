//! The Approver: runs what an approval stands for once the person approves it, as the caller
//! that asked, with no agent round-trip (docs/plans/approvals-auto-run.md). The console's
//! approve write only hands the approval's id here ([`enqueue`]); this object keeps it and runs
//! it from its alarm, so the run outlives the console's request. Each run is claimed in the
//! Directory first (once, ever), made through the same `dispatch` as `/mcp` with the caller,
//! its role and the person's policy checked again now, and its outcome recorded on the
//! approval: done with its answer and any next approval, or failed with why.

use pigeon_core::approval::{self, Record};
use pigeon_core::durable::{self, Db};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Env, Request, Response, Result, State as ObjectState, console_error, console_log};

use crate::approvals::{self, FollowUp};
use crate::land::Hosting;
use crate::users::{Caller, directory};
use crate::{Context, Hosted};

/// The Durable Object binding, in the hosting Worker's wrangler.toml.
pub const BINDING: &str = "APPROVER";
/// The one object.
const OBJECT: &str = "approver";

const MIGRATIONS: [&str; 1] =
    ["CREATE TABLE queue (id TEXT PRIMARY KEY, state TEXT NOT NULL, queued_ms INTEGER NOT NULL)"];

/// Hands approval `id`, just approved, to the Approver, which runs it from its alarm.
pub async fn enqueue(env: &Env, id: &str) -> Result<()> {
    durable::call(env, BINDING, OBJECT, "enqueue", &json!({ "id": id })).await?;
    Ok(())
}

fn now() -> i64 {
    worker::js_sys::Date::now() as i64
}

#[derive(Deserialize)]
struct Row {
    id: String,
    state: String,
}

/// The Approver's logic; the hosting Worker's Durable Object class wraps it.
pub struct Approver {
    db: Db,
    state: ObjectState,
    env: Env,
    hosting: Hosting,
    /// The Secrets Store binding of the parent token.
    parent: String,
}

/// Why a run did not happen, recorded as its failure.
fn refused(why: impl Into<String>) -> String {
    why.into()
}

impl Approver {
    pub fn new(state: ObjectState, env: Env, hosting: Hosting, parent: &str) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self {
            db,
            state,
            env,
            hosting,
            parent: parent.to_string(),
        })
    }

    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        match op {
            "enqueue" => {
                let id = a["id"].as_str().unwrap_or_default();
                if id.is_empty() || id.len() > 64 {
                    return Err(worker::Error::RustError("no approval id".into()));
                }
                self.db.exec(
                    "INSERT OR IGNORE INTO queue (id, state, queued_ms) VALUES (?, 'queued', ?)",
                    vec![id.into(), now().into()],
                )?;
                self.state.storage().set_alarm(10).await?;
                Ok(json!({ "queued": id }))
            }
            other => Err(worker::Error::RustError(format!(
                "the Approver has no operation `{other}`"
            ))),
        }
    }

    /// The alarm: a run still marked running died with the last alarm (alarms never overlap),
    /// so it is recorded as interrupted and never run again; then each queued approval runs,
    /// oldest first, including any queued meanwhile.
    pub async fn alarm(&self) -> Result<Response> {
        let died: Vec<Row> = self.db.rows(
            "SELECT id, state FROM queue WHERE state = 'running'",
            vec![],
        )?;
        for row in died {
            let ended = directory(
                &self.env,
                "approval.outcome",
                json!({ "id": row.id, "error": "interrupted while running: whether it changed \
                        anything is unknown; read the state before asking again" }),
            )
            .await;
            if let Err(e) = ended {
                console_error!("approver: recording {} as interrupted: {e}", row.id);
            }
            self.forget(&row.id)?;
        }
        loop {
            let next: Option<Row> = self.db.row(
                "SELECT id, state FROM queue WHERE state = 'queued' ORDER BY queued_ms LIMIT 1",
                vec![],
            )?;
            let Some(row) = next else { break };
            debug_assert_eq!(row.state, "queued");
            self.db.exec(
                "UPDATE queue SET state = 'running' WHERE id = ?",
                vec![row.id.as_str().into()],
            )?;
            if let Err(e) = self.run(&row.id).await {
                console_error!("approver: approval {}: {e}", row.id);
            }
            self.forget(&row.id)?;
        }
        Response::ok("ok")
    }

    fn forget(&self, id: &str) -> Result<()> {
        self.db
            .exec("DELETE FROM queue WHERE id = ?", vec![id.into()])
    }

    /// Runs approval `id`, once: claims it, runs it as its caller, records the outcome.
    async fn run(&self, id: &str) -> Result<()> {
        let env = &self.env;
        let got = directory(env, "approval.get", json!({ "id": id })).await?;
        let Some(r) = serde_json::from_value::<Option<Record>>(got)? else {
            return Ok(());
        };
        let planned = approvals::follow_up(&r);
        let tool = match &planned {
            Ok(Some(f)) => f.tool.clone(),
            Ok(None) => return Ok(()),
            Err(_) => r.subject.clone(),
        };
        let claimed = directory(env, "approval.claim", json!({ "id": id, "tool": tool })).await?;
        if let Some(why) = claimed["refused"].as_str() {
            console_log!("approver: approval {id} not run: {why}");
            return Ok(());
        }
        let ran = match planned {
            Ok(Some(f)) => self.execute(&r, &f).await,
            Ok(None) => Err(refused("nothing runs for this approval")),
            Err(why) => Err(why),
        };
        let outcome = match ran {
            Ok(answer) => json!({
                "id": id,
                "answer": answer,
                "summary": approvals::summarize(&tool, &answer),
                "next": approvals::next_of(&answer),
            }),
            // The narrow race where the caller's own call took the approval first: it ran
            // once, for the caller, who has its answer.
            Err(why) if why.contains(&approval::Refusal::Used(id.into()).to_string()) => json!({
                "id": id,
                "answer": Value::Null,
                "summary": "The caller's own call ran it first; its answer went to the caller.",
            }),
            Err(why) => json!({ "id": id, "error": why }),
        };
        directory(env, "approval.outcome", outcome).await?;
        Ok(())
    }

    /// Makes the call `f` that approval `r` stands for, as `r`'s caller, checked again now: the
    /// caller must still exist and act for the same person; `dispatch` asks their role and the
    /// person's policy, and the tool spends the approval for exactly what was approved.
    async fn execute(&self, r: &Record, f: &FollowUp) -> std::result::Result<Value, String> {
        let env = &self.env;
        let found = directory(env, "caller.of", json!({ "user_id": r.caller_id }))
            .await
            .map_err(|e| e.to_string())?;
        let mut caller: Caller = serde_json::from_value::<Option<Caller>>(found)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| refused("the caller that asked no longer exists"))?;
        if let Some(why) = acting_refusal(r, &caller) {
            return Err(why);
        }
        caller.run = r.summary["run"].as_str().map(str::to_string);
        approvals::recheck(&caller.user.name, &f.tool)?;
        let account = pigeon_core::account_from_env(env, &self.parent)
            .await
            .map_err(|e| e.to_string())?;
        let capabilities =
            crate::split::all(env, (self.hosting.capabilities)(), self.hosting.bindings).await;
        let hosted = Hosted {
            worker: self.hosting.worker,
            capabilities: &capabilities,
            sources: self.hosting.sources,
        };
        let scope = match &f.scope {
            Some(id) => {
                let record = crate::instances::one(env, id)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| refused(format!("instance `{id}` no longer exists")))?;
                Some(
                    crate::instances::tools::Scope::new(&record)
                        .map_err(|e| format!("instance `{id}`: {e}"))?,
                )
            }
            None => None,
        };
        let cx = Context {
            account: &account,
            env,
            hosted: &hosted,
            caller: Some(&caller),
            caller_note: None,
            instance: scope.as_ref(),
            worker: None,
        };
        crate::dispatch_claimed(&cx, &f.tool, &f.args)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Why `caller`, as the Directory knows it now, may not run approval `r`: it must act for the
/// person the approval was for (or for no one, when it was for no one). Pure.
pub fn acting_refusal(r: &Record, caller: &Caller) -> Option<String> {
    let acting = caller.acting_for.as_ref().map(|u| u.id.as_str());
    let ok = match r.approver_id.as_deref() {
        None => acting.is_none(),
        Some(p) => acting == Some(p) || caller.user.id == p,
    };
    (!ok).then(|| {
        "the caller that asked no longer acts for the person who approved; nothing ran".into()
    })
}

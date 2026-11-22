//! A run's sandbox: a Durable Object class, `Sandbox`, one object per run (`sb:{run}`), each
//! holding one container built from the CI base image with `pigeon-sandboxd` in it
//! (`ci/sandbox/`). The run object asks it to prepare the checkout and to run each sandbox tool
//! call; it starts the container when there is none (after a deploy, or an idle stop) and
//! checks the run's branch out again, so the branch on Artifacts is the run's checkpoint.
//!
//! The container gets a key of its own, made here and never shown to the model, so only this
//! object reaches its server. Git reaches the Artifacts proxy with the run's grant
//! (`docs/plans/ai-agents.md`, decision 3); until core mints run grants the sandbox has no git
//! credential, and a checkout of a private repository fails, saying so.

use pigeon_core::containers::{self, Start};
use pigeon_core::durable::{self, Db, opt};
use serde::Deserialize;
use serde_json::{Value, json};
use worker::{Env, Error, Request, Response, Result, State as ObjectState};

/// The class's binding.
pub const BINDING: &str = "SANDBOXES";
/// The port `pigeon-sandboxd` listens on.
const PORT: u16 = 8080;
/// The image key in the Worker's `[[containers]]`.
const IMAGE: &str = "sandbox";
/// The repository a run checks out, through the proxy.
pub const REPO_URL: &str = "https://proxy.pigeon.dev/git/pigeon-security/pigeon.git";
/// How long the container outlives the object's last request: a run's tool calls are seconds
/// to minutes apart, and a planner waits on executors for longer.
const KEEP_MS: i64 = 45 * 60_000;

const MIGRATIONS: [&str; 1] = [
    "CREATE TABLE sb (run TEXT PRIMARY KEY, key TEXT NOT NULL, role TEXT NOT NULL, base TEXT NOT NULL, branch TEXT NOT NULL, git_auth TEXT, prepared INTEGER NOT NULL DEFAULT 0)",
];

fn bad(why: impl Into<String>) -> Error {
    Error::RustError(why.into())
}

/// Calls `op` on run `run`'s sandbox.
pub async fn call(env: &Env, run: &str, op: &str, args: &Value) -> Result<Value> {
    durable::call(env, BINDING, &format!("sb:{run}"), op, args).await
}

/// Runs sandbox tool `name` for run `run`, whose branch is `branch`; answers the text the model
/// reads, or the refusal.
pub async fn tool(env: &Env, run: &str, branch: &str, name: &str, input: &Value) -> Result<String> {
    let answer = call(
        env,
        run,
        "tool",
        &json!({ "name": name, "input": input, "branch": branch }),
    )
    .await?;
    Ok(answer
        .as_str()
        .map_or_else(|| answer.to_string(), String::from))
}

/// The server's op for sandbox tool `name`, and the body it takes.
pub fn op_for(name: &str, input: &Value, branch: &str) -> Option<(&'static str, Value)> {
    Some(match name {
        "read_file" => (
            "read",
            json!({ "path": input["path"], "from_line": input["from_line"], "to_line": input["to_line"] }),
        ),
        "write_file" => (
            "write",
            json!({ "path": input["path"], "content": input["content"] }),
        ),
        "edit_file" => (
            "edit",
            json!({ "path": input["path"], "old": input["old"], "new": input["new"] }),
        ),
        "list" => (
            "list",
            json!({ "path": input["path"], "depth": input["depth"] }),
        ),
        "grep" => (
            "grep",
            json!({ "pattern": input["pattern"], "path": input["path"], "glob": input["glob"] }),
        ),
        "run" => (
            "exec",
            json!({ "cmd": input["cmd"], "timeout_s": input["timeout_s"] }),
        ),
        "commit_push" => (
            "commit_push",
            json!({ "message": input["message"], "branch": branch }),
        ),
        _ => return None,
    })
}

/// What the model reads of the server's answer to `op`.
pub fn shown(op: &str, answer: &Value) -> String {
    match op {
        "read" => format!(
            "{}\n[lines {}–{} of {}]",
            answer["text"].as_str().unwrap_or_default(),
            answer["from"],
            answer["to"],
            answer["lines"]
        ),
        "exec" => {
            let head = if answer["timed_out"] == true {
                format!("timed out after {} ms", answer["ms"])
            } else {
                format!("exit {} after {} ms", answer["exit"], answer["ms"])
            };
            let saved = answer["saved_at"].as_str().unwrap_or_default();
            format!(
                "{head}; {} bytes of output, the whole of it in `{saved}`\n{}",
                answer["bytes"],
                answer["output"].as_str().unwrap_or_default()
            )
        }
        "grep" => {
            let more = if answer["truncated"] == true {
                " (more not shown)"
            } else {
                ""
            };
            format!(
                "{} matches{more}\n{}",
                answer["count"],
                answer["matches"].as_str().unwrap_or_default()
            )
        }
        "list" => {
            let entries: Vec<&str> = answer["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let more = if answer["truncated"] == true {
                "\n(more not shown)"
            } else {
                ""
            };
            format!("{}{more}", entries.join("\n"))
        }
        "commit_push" if answer["nothing"] == true => {
            format!("nothing to commit; the branch is at {}", answer["commit"])
        }
        "commit_push" => format!("pushed {}", answer["commit"].as_str().unwrap_or_default()),
        _ => answer.to_string(),
    }
}

/// The instance type a run of `role` gets: builds need the larger one.
pub fn instance(role: &str) -> &'static str {
    if role == "planner" {
        "standard-1"
    } else {
        "standard-4"
    }
}

#[derive(Deserialize, Clone)]
struct Sb {
    run: String,
    key: String,
    role: String,
    base: String,
    branch: String,
    git_auth: Option<String>,
    prepared: i64,
}

/// The objects' logic; the hosting Worker's Durable Object class wraps it.
pub struct Sandbox {
    db: Db,
    state: ObjectState,
    env: Env,
}

impl Sandbox {
    /// The object's logic, its migrations applied.
    pub fn new(state: ObjectState, env: Env) -> Result<Self> {
        let db = Db::open(&state, &MIGRATIONS)?;
        Ok(Self { db, state, env })
    }

    /// Answers one operation from the run's object.
    pub async fn fetch(&self, req: Request) -> Result<Response> {
        durable::serve(req, async |op, args| self.handle(op, &args).await).await
    }

    fn sb(&self) -> Result<Option<Sb>> {
        self.db.row("SELECT * FROM sb LIMIT 1", vec![])
    }

    async fn handle(&self, op: &str, a: &Value) -> Result<Value> {
        match op {
            "prepare" => {
                if self.sb()?.is_none() {
                    let key = format!(
                        "{}{}",
                        pigeon_core::webauthn::random()?,
                        pigeon_core::webauthn::random()?
                    );
                    let caller = &a["caller"];
                    let git_auth = crate::object::grant(&self.env, caller, a).await.ok();
                    self.db.exec(
                        "INSERT INTO sb (run, key, role, base, branch, git_auth) VALUES (?, ?, ?, ?, ?, ?)",
                        vec![
                            a["run"].as_str().unwrap_or_default().into(),
                            key.into(),
                            a["role"].as_str().unwrap_or("executor").into(),
                            a["base"].as_str().unwrap_or("main").into(),
                            a["branch"].as_str().unwrap_or_default().into(),
                            opt(git_auth.as_deref()),
                        ],
                    )?;
                }
                let sb = self.sb()?.ok_or_else(|| bad("the sandbox vanished"))?;
                self.ready(&sb).await
            }
            "tool" => {
                let sb = self
                    .sb()?
                    .ok_or_else(|| bad("the sandbox was never prepared"))?;
                self.ready(&sb).await?;
                let name = a["name"].as_str().unwrap_or_default();
                let (op, body) = op_for(name, &a["input"], &sb.branch)
                    .ok_or_else(|| bad(format!("`{name}` is not a sandbox tool")))?;
                let answer = self.post(&sb, op, &body).await?;
                Ok(Value::String(shown(op, &answer)))
            }
            "destroy" => {
                containers::destroy(&self.state, "the run ended").await?;
                self.db.exec("DELETE FROM sb", vec![])?;
                Ok(Value::Null)
            }
            other => Err(bad(format!("no operation `{other}`"))),
        }
    }

    /// Makes sure the container runs and the checkout is ready: starts it when needed and
    /// checks the run's branch out (again).
    async fn ready(&self, sb: &Sb) -> Result<Value> {
        if !containers::running(&self.state) {
            let mut env: Vec<(&str, &str)> = vec![("PIGEON_SANDBOX_KEY", sb.key.as_str())];
            if let Some(auth) = &sb.git_auth {
                env.push(("PIGEON_GIT_AUTH", auth.as_str()));
            }
            containers::start(
                &self.state,
                &Start {
                    image: IMAGE,
                    instance: instance(&sb.role),
                    env: &env,
                    entrypoint: &["/usr/local/bin/pigeon-sandboxd"],
                    internet: true,
                },
            )?;
            self.db.exec("UPDATE sb SET prepared = 0", vec![])?;
        }
        containers::keep_for(&self.state, KEEP_MS).await?;
        if sb.prepared != 0 && containers::running(&self.state) {
            return Ok(json!({ "ready": true }));
        }
        // The server takes a moment to listen after the container starts.
        let mut last = String::new();
        for wait in [0u64, 500, 1_000, 2_000, 4_000, 8_000, 15_000] {
            worker::Delay::from(std::time::Duration::from_millis(wait)).await;
            match self
                .post(
                    sb,
                    "prepare",
                    &json!({
                        "repo_url": REPO_URL,
                        "base": sb.base,
                        "branch": sb.branch,
                        "name": format!("pigeon AI agent run {}", sb.run),
                        "email": "ai-agents@pigeon.dev",
                    }),
                )
                .await
            {
                Ok(v) => {
                    self.db.exec("UPDATE sb SET prepared = 1", vec![])?;
                    return Ok(v);
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(bad(format!("the sandbox did not get ready: {last}")))
    }

    /// Posts op `op` to the container's server.
    async fn post(&self, sb: &Sb, op: &str, body: &Value) -> Result<Value> {
        let auth = format!("Bearer {}", sb.key);
        let (status, text) = containers::post(
            &self.state,
            PORT,
            &format!("/op/{op}"),
            &[("Authorization", auth.as_str())],
            &body.to_string(),
        )
        .await?;
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }));
        if status == 200 {
            Ok(v)
        } else {
            Err(bad(v["error"]
                .as_str()
                .unwrap_or("the sandbox failed")
                .to_string()))
        }
    }
}

//! Models: requests to model providers through one Cloudflare AI Gateway, and what they used.
//! Each provider is declared once, with its key's alias and the dialect it speaks; its key is a
//! secret set on the secrets page, which the gateway reads from the Secrets Store, so no key
//! ever reaches a caller or a chat. A request takes one common shape whatever the provider, and
//! the gateway's logs answer how many requests, tokens and how much cost each model has used:
//! the measure a router between providers, or between here and a provider, will decide by.
//!
//! Each request is tagged with who sent it (the caller, and the person they act for when
//! linked) and an optional task label, so usage can be grouped and filtered by those as well as
//! by model. The gateway logs keep those tags and the counts, never a prompt or an answer.
//!
//! `models_run` is one request, answered while the caller waits, about a minute at most. A
//! longer one is a job: `models_submit` queues it, it streams through the same gateway with the
//! same tags in a Durable Object of its own ([`runner`]), and `models_status` and
//! `models_result` follow and fetch it, with an optional signed notify when it ends.
//!
//! The capability is its own Worker, `pigeon-model-gateway` ([`ModelGateway`], behind the
//! contract in `pigeon-contract`), and core still hosts the job objects' class and, until its
//! binding lands, the same tools through [`Models`] directly. Either way every function here
//! takes the account to act on and the caller as core serializes it, as JSON
//! (`{user: {id, name}, acting_for: {id, name}?}`), so the logic is one.

use pigeon_contract::{
    Answer, CONTRACT, CallRequest, CapabilityManifest, Credential, CredentialRequest, FailureKind,
    Manifest, SecretOp, SecretRequest, SectionRequest, SectionSpec, Service, Signed, SlotSpec,
    ToolSpec,
};
use pigeon_core::Account;
use pigeon_core::ai_gateway::{self, Dimension, Filter, SECRET_SCOPE, tag};
use pigeon_core::expiry::Expiry;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::webauthn::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Env, Error, Fetch, Headers, Method, Request, RequestInit, Result};

pub mod daily;
pub mod jobs;
pub mod runner;
pub mod stream;

/// The capability's name.
pub const NAME: &str = "models";

/// The model gateway Worker's script name.
pub const WORKER: &str = "pigeon-model-gateway";

/// Every tool the capability offers.
pub const TOOLS: [&str; 8] = [
    "models_setup",
    "models_providers",
    "models_run",
    "models_submit",
    "models_status",
    "models_result",
    "models_logs",
    "models_usage",
];

/// The Secrets Store binding of the key core signs callers with.
const CALLER_KEY: &str = "CALLER_KEY";

/// The Service Binding to core.
const PLATFORM: &str = "PLATFORM";

/// The console section the capability fills.
const SECTION: &str = "cost";

/// The error for a tool the capability does not have.
pub fn unknown(tool: &str) -> Error {
    Error::RustError(format!("no tool `{tool}`"))
}

/// The credential purposes (`credential` kind `scoped`) a tool's Cloudflare calls need, or
/// `None` for a tool that makes none (a job's tools, which reach the job objects only).
pub fn purposes(tool: &str) -> Option<&'static [&'static str]> {
    match tool {
        "models_setup" => Some(&["ai_gateway", "secrets_store"]),
        "models_submit" | "models_status" | "models_result" => None,
        _ => Some(&["ai_gateway"]),
    }
}

/// Who is calling, read from the caller's JSON: their id and name, and the person they act
/// for, when any.
struct Who {
    id: String,
    name: String,
    acting_for: Option<(String, String)>,
}

/// The caller in `caller` (`{user: {id, name}, acting_for}`), when it names a user.
fn who(caller: &Value) -> Option<Who> {
    let id = caller["user"]["id"].as_str().filter(|i| !i.is_empty())?;
    let name = caller["user"]["name"].as_str().unwrap_or_default();
    let acting_for = caller["acting_for"]["id"].as_str().map(|f| {
        (
            f.to_string(),
            caller["acting_for"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    });
    Some(Who {
        id: id.to_string(),
        name: name.to_string(),
        acting_for,
    })
}

/// `v` as JSON; null if it cannot be.
fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The most logs the console's chart reads for one window, so a busy gateway cannot run it long.
const CHART_LOGS: usize = 5_000;

/// Model providers behind one gateway.
pub struct Models {
    /// The gateway's ID, such as `pigeon`.
    pub gateway: String,
    /// The providers, the first the default.
    pub providers: Vec<Provider>,
    /// Where a person sets a key, such as `https://platform.pigeon.dev/secrets`.
    pub secrets_page: String,
    /// The platform's hostname, which a job's notify may not point at.
    pub host: String,
    /// The proxies' hostname, which a job's notify may not point at either.
    pub proxy_host: String,
}

/// A provider as the gateway knows it.
pub struct Provider {
    /// The gateway's name for it, such as `anthropic`.
    pub slug: &'static str,
    /// Which of its keys, such as `default`.
    pub alias: &'static str,
    /// How its requests and answers are shaped.
    pub dialect: Dialect,
    /// The model a request names when it names none.
    pub default_model: &'static str,
    /// Where to get a key, for the secrets page.
    pub key_help: &'static str,
}

/// How a provider's requests and answers are shaped.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// The Claude Platform's Messages API.
    Anthropic,
}

/// A request in the common shape.
#[derive(Debug, PartialEq)]
pub struct Ask {
    /// The model asked.
    pub model: String,
    /// The system prompt, when any.
    pub system: Option<String>,
    /// `[{role, content}]`, as the providers take them.
    pub messages: Value,
    /// The most tokens the answer may take.
    pub max_tokens: u64,
}

/// An answer in the common shape.
#[derive(Serialize, Debug, PartialEq)]
pub struct Reply {
    /// The model that answered.
    pub model: String,
    /// The answer's text.
    pub text: String,
    /// Why the answer stopped, as the provider says.
    pub stop_reason: Option<String>,
    /// The tokens read.
    pub tokens_in: Option<u64>,
    /// The tokens written.
    pub tokens_out: Option<u64>,
}

fn bad(why: &str) -> Error {
    Error::RustError(why.to_string())
}

impl Dialect {
    /// The provider's path for a request, under the gateway's.
    pub fn path(self) -> &'static str {
        match self {
            Dialect::Anthropic => "v1/messages",
        }
    }

    /// Headers a request needs beside the gateway's.
    pub fn headers(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Dialect::Anthropic => &[("anthropic-version", "2023-06-01")],
        }
    }

    /// `ask` as the provider's request body.
    pub fn body(self, ask: &Ask) -> Value {
        match self {
            Dialect::Anthropic => {
                let mut b = json!({
                    "model": ask.model,
                    "max_tokens": ask.max_tokens,
                    "messages": ask.messages,
                });
                if let Some(s) = &ask.system {
                    b["system"] = json!(s);
                }
                b
            }
        }
    }

    /// The provider's answer in the common shape.
    pub fn reply(self, v: &Value) -> Result<Reply> {
        match self {
            Dialect::Anthropic => {
                let blocks = v["content"]
                    .as_array()
                    .ok_or_else(|| bad(&format!("not a Messages answer: {v}")))?;
                let text = blocks
                    .iter()
                    .filter(|b| b["type"] == "text")
                    .filter_map(|b| b["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                Ok(Reply {
                    model: v["model"].as_str().unwrap_or_default().into(),
                    text,
                    stop_reason: v["stop_reason"].as_str().map(String::from),
                    tokens_in: v["usage"]["input_tokens"].as_u64(),
                    tokens_out: v["usage"]["output_tokens"].as_u64(),
                })
            }
        }
    }

    /// Checks `key` with the provider itself, before the gateway is given it: a cheap call that
    /// needs a working key and spends nothing.
    async fn check(self, key: &str) -> Result<String> {
        match self {
            Dialect::Anthropic => {
                let headers = Headers::new();
                headers.set("x-api-key", key)?;
                headers.set("anthropic-version", "2023-06-01")?;
                let mut init = RequestInit::new();
                init.with_method(Method::Get).with_headers(headers);
                let url = "https://api.anthropic.com/v1/models?limit=1";
                let mut r = Fetch::Request(Request::new_with_init(url, &init)?)
                    .send()
                    .await?;
                let status = r.status_code();
                let text = r.text().await?;
                if (200..300).contains(&status) {
                    Ok("the Claude Platform accepts it".into())
                } else {
                    let shown: String = text.chars().take(200).collect();
                    Err(bad(&format!(
                        "the Claude Platform refused it: {status}: {shown}"
                    )))
                }
            }
        }
    }
}

/// `args` in the common shape, for `provider`.
pub fn ask(args: &Value, provider: &Provider) -> Result<Ask> {
    let messages = match (&args["messages"], args["prompt"].as_str()) {
        (Value::Array(m), _) if !m.is_empty() => Value::Array(m.clone()),
        (_, Some(p)) => json!([{ "role": "user", "content": p }]),
        _ => return Err(bad("give `prompt`, or `messages` as [{role, content}]")),
    };
    Ok(Ask {
        model: args["model"]
            .as_str()
            .unwrap_or(provider.default_model)
            .into(),
        system: args["system"].as_str().map(String::from),
        messages,
        max_tokens: args["max_tokens"].as_u64().unwrap_or(1024).clamp(1, 64_000),
    })
}

/// The longest task label a request carries; a longer one is cut.
const MAX_TASK_CHARS: usize = 100;

/// The tags a request from `caller` (its JSON; null for none) carries, `task` among them when
/// given.
pub fn tags(caller: &Value, task: Option<&str>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(c) = who(caller) {
        out.push((tag::CALLER, c.id));
        out.push((tag::CALLER_NAME, c.name));
        if let Some((id, name)) = c.acting_for {
            out.push((tag::FOR, id));
            out.push((tag::FOR_NAME, name));
        }
    }
    if let Some(t) = task.map(str::trim).filter(|t| !t.is_empty()) {
        out.push((tag::TASK, t.chars().take(MAX_TASK_CHARS).collect()));
    }
    out
}

/// `args`' filter for `models_usage`.
fn filter(args: &Value) -> Filter {
    let s = |k: &str| args[k].as_str().map(String::from);
    Filter {
        provider: s("provider"),
        model: s("model"),
        caller: s("caller"),
        task: s("task"),
        failed: args["failed"].as_bool().unwrap_or(false),
    }
}

/// The most log entries `models_logs` answers.
const MAX_LOG_ENTRIES: usize = 200;

/// How many entries `models_logs` answers: 20 unless asked, 1 to [`MAX_LOG_ENTRIES`].
fn log_limit(args: &Value) -> usize {
    args["limit"]
        .as_u64()
        .map_or(20, |n| n.clamp(1, MAX_LOG_ENTRIES as u64) as usize)
}

/// `args`' grouping for `models_usage`: by model when none is named.
fn dimensions(args: &Value) -> Result<Vec<Dimension>> {
    let Some(names) = args["by"].as_array() else {
        return Ok(vec![Dimension::Model]);
    };
    let mut out = Vec::new();
    for n in names {
        let n = n.as_str().unwrap_or_default();
        let d = Dimension::named(n).ok_or_else(|| bad(&format!("cannot group by `{n}`")))?;
        if !out.contains(&d) {
            out.push(d);
        }
    }
    Ok(out)
}

impl Models {
    fn secret(&self, p: &Provider) -> String {
        ai_gateway::secret_name(&self.gateway, p.slug, p.alias)
    }

    fn provider(&self, slug: Option<&str>) -> Result<&Provider> {
        match slug {
            None => self
                .providers
                .first()
                .ok_or_else(|| bad("no provider is declared")),
            Some(s) => self
                .providers
                .iter()
                .find(|p| p.slug == s)
                .ok_or_else(|| bad(&format!("no provider `{s}`"))),
        }
    }

    /// The gateway, and every provider's key handed to it where the key is set.
    async fn setup(&self, account: &Account) -> Result<Value> {
        let gateway = ai_gateway::ensure(account, &self.gateway).await?;
        let writer = Writer::open(account).await?;
        let stored = writer.list().await;
        let stored = writer.finish(stored).await?;
        let mut providers = Vec::new();
        for p in &self.providers {
            let name = self.secret(p);
            let state = match stored.iter().find(|s| s.name == name) {
                Some(s) => {
                    let added =
                        ai_gateway::ensure_provider(account, &self.gateway, p.slug, p.alias, &s.id)
                            .await?;
                    json!({ "provider": p.slug, "alias": p.alias, "key": "set", "added_to_gateway": added })
                }
                None => json!({
                    "provider": p.slug, "alias": p.alias, "key": "missing",
                    "set_it_at": format!("{}#{name}", self.secrets_page),
                }),
            };
            providers.push(state);
        }
        Ok(json!({ "gateway": gateway, "providers": providers }))
    }

    async fn run(&self, account: &Account, caller: &Value, args: &Value) -> Result<Value> {
        let p = self.provider(args["provider"].as_str())?;
        let raw = args.get("raw").filter(|r| r.is_object());
        let body = match raw {
            Some(r) => r.clone(),
            None => p.dialect.body(&ask(args, p)?),
        };
        let tags = tags(caller, args["task"].as_str());
        let tags: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let ran = ai_gateway::run(
            account,
            &self.gateway,
            p.slug,
            p.dialect.path(),
            p.dialect.headers(),
            &tags,
            &body,
        )
        .await?;
        if !(200..300).contains(&ran.status) {
            return Err(bad(&format!(
                "{} answered {}: {}",
                p.slug, ran.status, ran.body
            )));
        }
        if raw.is_some() {
            return Ok(ran.body);
        }
        let mut reply = to_value(p.dialect.reply(&ran.body)?);
        reply["provider"] = json!(p.slug);
        Ok(reply)
    }

    /// The gateway's latest log entries: what each request was, never what it said.
    async fn recent(&self, account: &Account, args: &Value) -> Result<Value> {
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let limit = log_limit(args);
        let since = Date::now() - hours * 3_600_000.0;
        let logs = ai_gateway::logs(account, &self.gateway, since, &filter(args), limit).await?;
        Ok(json!({
            "since": String::from(Date::new(&since.into()).to_iso_string()),
            "entries": logs.len(),
            "more": logs.len() >= limit,
            "logs": logs,
        }))
    }

    /// Queues a job: the request in the common shape, streamed, with the caller's tags.
    async fn submit(&self, env: &Env, caller_json: &Value, args: &Value) -> Result<Value> {
        let caller = who(caller_json)
            .ok_or_else(|| bad("a job needs a caller, who alone can read its result"))?;
        let p = self.provider(args["provider"].as_str())?;
        let asked = ask(args, p)?;
        let mut body = p.dialect.body(&asked);
        body["stream"] = json!(true);
        let timeout_ms = jobs::timeout_ms(&args["timeout_min"]).map_err(|e| bad(&e))?;
        let notify_url = match args["notify"].as_str() {
            Some(u) => Some(
                pigeon_core::notify::check_url(u, &[&self.host, &self.proxy_host])
                    .map_err(|e| bad(&e))?,
            ),
            None => None,
        };
        let notify_key = match notify_url {
            Some(_) => Some(random()?),
            None => None,
        };
        let task = args["task"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| t.chars().take(MAX_TASK_CHARS).collect::<String>());
        let own = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let tags: Vec<(String, String)> = tags(caller_json, task.as_deref())
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let id: String = random()?.chars().take(22).collect();
        let new = runner::New {
            id,
            caller: Some(caller.id.clone()),
            caller_name: Some(caller.name.clone()),
            for_id: caller.acting_for.as_ref().map(|(id, _)| id.clone()),
            task,
            provider: p.slug.into(),
            model: asked.model.clone(),
            notify_url,
            notify_key: notify_key.clone(),
            ask: runner::Ask {
                dialect: p.dialect,
                provider: p.slug.into(),
                path: p.dialect.path().into(),
                headers: own(p.dialect.headers()),
                tags,
                body,
                timeout_ms,
            },
        };
        let mut out =
            runner::call(env, runner::INDEX, "submit", &serde_json::to_value(&new)?).await?;
        if let Some(k) = notify_key {
            out["notify_key"] = json!(k);
            out["notify_key_note"] = json!(pigeon_core::notify::KEY_NOTE);
        }
        Ok(out)
    }

    /// `op` (`status` or `result`) of job `id`, for the caller.
    async fn job(&self, env: &Env, caller: &Value, op: &str, args: &Value) -> Result<Value> {
        let reader = who(caller).map(|c| c.id).unwrap_or_default();
        let id = args["id"].as_str().ok_or_else(|| bad("`id` is required"))?;
        runner::call(
            env,
            runner::INDEX,
            op,
            &json!({ "id": id, "reader": reader }),
        )
        .await
    }

    async fn usage(&self, account: &Account, args: &Value) -> Result<Value> {
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let by = dimensions(args)?;
        let filter = filter(args);
        let since = Date::now() - hours * 3_600_000.0;
        let logs = ai_gateway::logs(account, &self.gateway, since, &filter, 2_000).await?;
        let groups = ai_gateway::summarize(&logs, &by);
        let cost: f64 = groups.iter().filter_map(|u| u.cost).sum();
        let by: Vec<&str> = by
            .iter()
            .filter_map(|d| Dimension::ALL.iter().find(|(_, x)| x == d).map(|(n, _)| *n))
            .collect();
        Ok(json!({
            "since": String::from(Date::new(&since.into()).to_iso_string()),
            "hours": hours,
            "requests": logs.len(),
            "truncated": logs.len() >= 2_000,
            "tokens_in": groups.iter().map(|u| u.tokens_in).sum::<u64>(),
            "tokens_out": groups.iter().map(|u| u.tokens_out).sum::<u64>(),
            "cost": cost,
            "payloads_kept": groups.iter().map(|u| u.payloads_kept).sum::<u64>(),
            "by": by,
            "groups": groups,
        }))
    }
}

/// The tab the cost section sits on, as its path names it.
pub const SECTION_TAB: &str = "cost";
/// The cost section's title.
pub const SECTION_TITLE: &str = "Cost";
/// The cost section's line under the title.
pub const SECTION_DESCRIPTION: &str = "Model cost through the AI Gateway, per UTC day. No prompts or answers are kept; CI reports no cost yet.";
/// The cost section's place within its tab.
pub const SECTION_ORDER: u16 = 1;
/// The name the cost section adds to the overview's total under.
pub const SECTION_COST: &str = "Model cost";

impl Models {
    /// The capability's anchor for its one console section.
    pub fn section_id(&self) -> &'static str {
        SECTION
    }

    /// The MCP tools the capability offers.
    pub fn tools(&self) -> Vec<Tool> {
        let slugs: Vec<&str> = self.providers.iter().map(|p| p.slug).collect();
        vec![
            Tool::bare(
                "models_setup",
                "Have the AI gateway exist and hand it every provider key that is set. A \
                 missing key comes back with a link to the secrets page; it runs again by \
                 itself when a key is set there.",
            ),
            Tool::bare(
                "models_providers",
                "The provider keys the AI gateway holds: provider, alias, a preview of the key \
                 and when it was last used. Never the key.",
            ),
            Tool {
                name: "models_run",
                description: "Send one request to a model through the AI gateway and get its \
                              answer: text, stop reason and tokens. Give `prompt`, or \
                              `messages` as [{role, content}]; `raw` sends a body in the \
                              provider's own shape instead and answers its own. The request is \
                              tagged with its caller and `task`, for `models_usage`; its log \
                              keeps no prompt or answer.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": slugs, "description": "The first declared when omitted." },
                        "model": { "type": "string", "description": "The provider's default when omitted." },
                        "system": { "type": "string" },
                        "prompt": { "type": "string" },
                        "messages": { "type": "array", "items": { "type": "object" } },
                        "max_tokens": { "type": "integer", "description": "1024 by default." },
                        "raw": { "type": "object" },
                        "task": { "type": "string", "description": "A short label for what the request is for, such as `triage`; 100 characters at most." }
                    }
                }),
            },
            Tool {
                name: "models_submit",
                description: "Queue a request to a model as a job, for answers that take longer \
                              than models_run's minute: it streams through the AI gateway with \
                              the same tags and no prompt or answer in the log, for up to \
                              `timeout_min` (10 by default, 14 at most). Answers the job's id at \
                              once; follow it with models_status and fetch it with \
                              models_result. With `notify`, an https URL gets a signed POST when \
                              it ends (the key is shown once). Four run at once; more wait.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": slugs, "description": "The first declared when omitted." },
                        "model": { "type": "string", "description": "The provider's default when omitted." },
                        "system": { "type": "string" },
                        "prompt": { "type": "string" },
                        "messages": { "type": "array", "items": { "type": "object" } },
                        "max_tokens": { "type": "integer", "description": "1024 by default, 64000 at most." },
                        "task": { "type": "string", "description": "A short label, as for models_run." },
                        "timeout_min": { "type": "integer", "description": "1 to 14; 10 by default." },
                        "notify": { "type": "string", "description": "An https URL to POST to when the job ends." }
                    }
                }),
            },
            Tool {
                name: "models_status",
                description: "A model job's state (queued, running, succeeded, failed, \
                              timed_out, lost), its times, the characters streamed so far and, \
                              once it ends, its tokens and any error. Only its sender, or the \
                              person they act for, can see it.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool {
                name: "models_result",
                description: "A finished model job's answer: text, stop reason, tokens and any \
                              error (a timed-out job keeps its text so far). Kept for 10 \
                              minutes once read, and 24 hours from submission at most.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool {
                name: "models_logs",
                description: "The AI gateway's latest log entries, newest first: id, time, \
                              provider, model, status, success, cached, tokens in and out, \
                              cost, duration, the request's tags (caller, task) and whether \
                              its bodies were kept (`collect_log_payload`). Never a prompt or \
                              an answer. `limit` 20 by default, 200 at most.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "hours": { "type": "number", "description": "How far back; 24 by default, 744 at most." },
                        "limit": { "type": "integer", "description": "20 by default, 200 at most." },
                        "provider": { "type": "string", "enum": slugs },
                        "model": { "type": "string" },
                        "caller": { "type": "string", "description": "A user's ID or name, who sent the request or was acted for." },
                        "task": { "type": "string" },
                        "failed": { "type": "boolean", "description": "Only requests that failed." }
                    }
                }),
            },
            Tool {
                name: "models_usage",
                description: "What the AI gateway's requests used over a window, from its \
                              logs: requests, failures, tokens in and out, and cost, in all and \
                              in groups: by provider and model unless `by` names others. \
                              `model`, `caller` and `task` keep only the requests that match; \
                              a model in either form (`claude-haiku-5-5`, \
                              `anthropic/claude-haiku-5.5`).",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "hours": { "type": "number", "description": "24 by default, 744 at most." },
                        "provider": { "type": "string", "enum": slugs },
                        "model": { "type": "string" },
                        "caller": { "type": "string", "description": "A user's ID or name, who sent the request or was acted for." },
                        "task": { "type": "string" },
                        "by": {
                            "type": "array",
                            "items": { "type": "string", "enum": ["model", "caller", "task"] },
                            "description": "What to group by; [\"model\"] by default."
                        }
                    }
                }),
            },
        ]
    }

    /// The secrets the capability needs set: each provider's key, readable by the AI gateway.
    pub fn secrets(&self) -> Vec<SlotSpec> {
        self.providers
            .iter()
            .map(|p| SlotSpec {
                name: self.secret(p),
                purpose: format!(
                    "The {} key the AI gateway `{}` uses for alias `{}`. {}",
                    p.slug, self.gateway, p.alias, p.key_help
                ),
                scopes: vec![SECRET_SCOPE.to_string()],
            })
            .collect()
    }

    /// The capability as a manifest entry: its tools, its cost section, its secrets and its
    /// footprint (the gateway).
    pub fn capability(&self) -> CapabilityManifest {
        CapabilityManifest {
            name: NAME.into(),
            tools: self
                .tools()
                .into_iter()
                .map(|t| ToolSpec {
                    name: t.name.into(),
                    description: t.description.into(),
                    input_schema: t.input_schema,
                })
                .collect(),
            sections: vec![SectionSpec {
                tab: SECTION_TAB.into(),
                id: SECTION.into(),
                title: SECTION_TITLE.into(),
                description: SECTION_DESCRIPTION.into(),
                order: SECTION_ORDER,
                cost: Some(SECTION_COST.into()),
            }],
            routes: Vec::new(),
            footprint: json!({ "gateways": [self.gateway] }),
            secrets: self.secrets(),
            wants: Vec::new(),
        }
    }

    /// Runs `tool` with `args` for `caller` (its JSON; null for none), acting on `account`,
    /// which every tool but a job's needs ([`purposes`]).
    pub async fn call(
        &self,
        env: &Env,
        account: Option<&Account>,
        caller: &Value,
        tool: &str,
        args: &Value,
    ) -> Result<Value> {
        let account = || account.ok_or_else(|| bad(&format!("{tool} needs an account")));
        Ok(match tool {
            "models_setup" => self.setup(account()?).await?,
            "models_providers" => {
                to_value(ai_gateway::provider_configs(account()?, &self.gateway).await?)
            }
            "models_run" => self.run(account()?, caller, args).await?,
            "models_usage" => self.usage(account()?, args).await?,
            "models_logs" => self.recent(account()?, args).await?,
            "models_submit" => self.submit(env, caller, args).await?,
            "models_status" => self.job(env, caller, "status", args).await?,
            "models_result" => self.job(env, caller, "result", args).await?,
            other => return Err(unknown(other)),
        })
    }

    /// The cost section's data: cost per day per model family over the last `days` days, from
    /// the gateway's logs.
    pub async fn section(&self, account: &Account, query: &Value) -> Result<Value> {
        let days = query["days"].as_u64().unwrap_or(7).clamp(1, 30) as usize;
        let now = Date::new_0();
        let today = String::from(now.to_iso_string());
        // Reaching back past the start of the window's first UTC day; daily() trims.
        let since = now.get_time() - (days as f64) * 86_400_000.0;
        let logs = ai_gateway::logs(
            account,
            &self.gateway,
            since,
            &Filter::default(),
            CHART_LOGS,
        )
        .await?;
        let mut answer = to_value(daily::daily(&logs, &today, days));
        answer["truncated"] = json!(logs.len() >= CHART_LOGS);
        Ok(answer)
    }

    /// The provider whose key is secret `name`.
    fn keyed(&self, name: &str) -> Result<&Provider> {
        self.providers
            .iter()
            .find(|p| self.secret(p) == name)
            .ok_or_else(|| bad("no provider uses this secret"))
    }

    /// Checks `value` as secret `name` with its provider, before it is written.
    pub async fn check_secret(&self, name: &str, value: &str) -> Result<String> {
        self.keyed(name)?.dialect.check(value).await
    }

    /// A provider's API key: Anthropic's API, like the others', answers no expiry, and the
    /// key is readable only by the AI gateway, so there is nothing to ask.
    pub fn secret_expiry(&self) -> Expiry {
        Expiry::Unknown {
            why: "the provider's API reports no key expiry".into(),
        }
    }

    /// A key just set as secret `name` (Secrets Store id `id`): the gateway is set up and given
    /// it.
    pub async fn secret_set(&self, account: &Account, name: &str, id: &str) -> Result<String> {
        let p = self.keyed(name)?;
        ai_gateway::ensure(account, &self.gateway).await?;
        let added =
            ai_gateway::ensure_provider(account, &self.gateway, p.slug, p.alias, id).await?;
        Ok(if added {
            format!("the gateway `{}` now uses it for {}", self.gateway, p.slug)
        } else {
            format!(
                "the gateway `{}` already used it for {}",
                self.gateway, p.slug
            )
        })
    }
}

/// The model gateway Worker's side of the contract: [`Models`] served to core. Every
/// Cloudflare call acts on a short-lived token core mints for it (`credential`, kind
/// `scoped`), asked over the `PLATFORM` binding as the caller core signed; the gateway holds no
/// parent token. The job objects are reached over the `MODEL_JOBS` binding.
pub struct ModelGateway {
    /// The capability it serves.
    pub models: Models,
}

/// The key core signs callers with, from the Secrets Store binding [`CALLER_KEY`].
async fn caller_key(env: &Env) -> Result<String> {
    let missing = || bad("the caller key (CALLER_KEY) is not set in the Secrets Store");
    let store = env.secret_store(CALLER_KEY).map_err(|_| missing())?;
    secrets_store::read(&store).await?.ok_or_else(missing)
}

/// An account acting with a token core minted for `purposes`, asked as `caller`.
pub async fn credential(env: &Env, caller: &Signed, purposes: &[&str]) -> Result<Account> {
    let platform = env.service(PLATFORM)?;
    let key = caller_key(env).await?;
    let nonce = random()?;
    let request = CredentialRequest {
        kind: "scoped".into(),
        instance: None,
        scopes: purposes.iter().map(|p| p.to_string()).collect(),
    };
    let answer = pigeon_contract::ask(
        &platform,
        "https://core",
        "credential",
        &serde_json::to_value(&request)?,
        Some((key.as_bytes(), &caller.caller)),
        &nonce,
    )
    .await?;
    match answer {
        Answer::Ok(value) => {
            let given: Credential = serde_json::from_value(value)
                .map_err(|e| bad(&format!("core's credential does not parse: {e}")))?;
            Ok(Account {
                id: given.account_id,
                parent: String::new(),
                secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
                bearer: Some(given.token),
            })
        }
        Answer::Error(f) => Err(bad(&format!("core gave no credential: {}", f.message))),
    }
}

/// A result as an answer: its value, or a `failed` error with its message.
fn answer(result: Result<Value>) -> Answer {
    match result {
        Ok(v) => Answer::Ok(v),
        Err(e) => Answer::error(FailureKind::Failed, e.to_string()),
    }
}

impl ModelGateway {
    /// Runs one tool for `caller`, with a credential when the tool needs one.
    async fn run_tool(&self, env: &Env, caller: &Signed, req: &CallRequest) -> Answer {
        if !TOOLS.contains(&req.tool.as_str()) {
            return Answer::error(FailureKind::UnknownTool, format!("no tool `{}`", req.tool));
        }
        let account = match purposes(&req.tool) {
            Some(p) => match credential(env, caller, p).await {
                Ok(a) => Some(a),
                Err(e) => return answer(Err(e)),
            },
            None => None,
        };
        answer(
            self.models
                .call(env, account.as_ref(), &caller.caller, &req.tool, &req.args)
                .await,
        )
    }

    /// The cost section's data for `caller`.
    async fn cost(&self, env: &Env, caller: &Signed, req: &SectionRequest) -> Answer {
        if req.id != SECTION {
            return Answer::error(FailureKind::BadRequest, format!("no section `{}`", req.id));
        }
        match credential(env, caller, &["ai_gateway"]).await {
            Ok(account) => answer(self.models.section(&account, &req.query).await),
            Err(e) => answer(Err(e)),
        }
    }

    /// Checks, acts on or dates one of the providers' keys.
    async fn key(&self, env: &Env, caller: &Signed, req: &SecretRequest) -> Answer {
        match req.op {
            SecretOp::Check => {
                let value = req.value.as_deref().unwrap_or_default();
                answer(
                    self.models
                        .check_secret(&req.name, value)
                        .await
                        .map(Value::String),
                )
            }
            SecretOp::Expiry => Answer::Ok(to_value(self.models.secret_expiry())),
            SecretOp::Set => {
                let id = req.id.as_deref().unwrap_or_default();
                match credential(env, caller, &["ai_gateway"]).await {
                    Ok(account) => answer(
                        self.models
                            .secret_set(&account, &req.name, id)
                            .await
                            .map(Value::String),
                    ),
                    Err(e) => answer(Err(e)),
                }
            }
        }
    }
}

impl Service for ModelGateway {
    fn manifest(&self) -> Manifest {
        Manifest {
            contract: CONTRACT,
            worker: WORKER.into(),
            commit: option_env!("PIGEON_COMMIT").unwrap_or_default().into(),
            capabilities: vec![self.models.capability()],
        }
    }

    fn call<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: CallRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move { self.run_tool(env, caller, &req).await })
    }

    fn section<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: SectionRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move { self.cost(env, caller, &req).await })
    }

    fn secret<'a>(
        &'a self,
        env: &'a Env,
        caller: &'a Signed,
        req: SecretRequest,
    ) -> pigeon_contract::serve::Answering<'a> {
        Box::pin(async move { self.key(env, caller, &req).await })
    }
}

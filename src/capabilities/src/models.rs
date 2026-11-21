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
//! same tags in a Durable Object of its own (`models/runner.rs`), and `models_status` and
//! `models_result` follow and fetch it, with an optional signed notify when it ends.

use pigeon_core::Account;
use pigeon_core::ai_gateway::{self, Dimension, Filter, SECRET_SCOPE, tag};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::Writer;
use pigeon_core::webauthn::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::console::sections::{Section, Tab};
use crate::users::{Caller, User};
use crate::{Call, Capability, Context, Footprint, Slot, to_value, unknown};

pub mod daily;
pub mod jobs;
pub mod runner;
pub mod stream;

/// The most logs the console's chart reads for one window, so a busy gateway cannot run it long.
const CHART_LOGS: usize = 5_000;

/// Model providers behind one gateway.
pub struct Models {
    /// The gateway's ID, such as `pigeon`.
    pub gateway: String,
    pub providers: Vec<Provider>,
    /// Where a person sets a key, such as `https://platform.pigeon.dev/secrets`.
    pub secrets_page: String,
    /// The platform's hostname and its proxies', which a job's notify may not point at.
    pub host: String,
    pub proxy_host: String,
}

/// A provider as the gateway knows it.
pub struct Provider {
    /// The gateway's name for it, such as `anthropic`.
    pub slug: &'static str,
    /// Which of its keys, such as `default`.
    pub alias: &'static str,
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
    pub model: String,
    pub system: Option<String>,
    /// `[{role, content}]`, as the providers take them.
    pub messages: Value,
    pub max_tokens: u64,
}

/// An answer in the common shape.
#[derive(Serialize, Debug, PartialEq)]
pub struct Reply {
    pub model: String,
    pub text: String,
    pub stop_reason: Option<String>,
    pub tokens_in: Option<u64>,
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

/// The tags a request from `caller` carries, `task` among them when given.
pub fn tags(caller: Option<&Caller>, task: Option<&str>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(c) = caller {
        out.push((tag::CALLER, c.user.id.clone()));
        out.push((tag::CALLER_NAME, c.user.name.clone()));
        if let Some(f) = &c.acting_for {
            out.push((tag::FOR, f.id.clone()));
            out.push((tag::FOR_NAME, f.name.clone()));
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
    async fn setup(&self, cx: &Context<'_>) -> Result<Value> {
        let gateway = ai_gateway::ensure(cx.account, &self.gateway).await?;
        let writer = Writer::open(cx.account).await?;
        let stored = writer.list().await;
        let stored = writer.finish(stored).await?;
        let mut providers = Vec::new();
        for p in &self.providers {
            let name = self.secret(p);
            let state = match stored.iter().find(|s| s.name == name) {
                Some(s) => {
                    let added = ai_gateway::ensure_provider(
                        cx.account,
                        &self.gateway,
                        p.slug,
                        p.alias,
                        &s.id,
                    )
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

    async fn run(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let p = self.provider(args["provider"].as_str())?;
        let raw = args.get("raw").filter(|r| r.is_object());
        let body = match raw {
            Some(r) => r.clone(),
            None => p.dialect.body(&ask(args, p)?),
        };
        let tags = tags(cx.caller, args["task"].as_str());
        let tags: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let ran = ai_gateway::run(
            cx.account,
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
    async fn submit(&self, cx: &Context<'_>, args: &Value) -> Result<Value> {
        let caller = cx
            .caller
            .ok_or_else(|| bad("a job needs a caller, who alone can read its result"))?;
        let p = self.provider(args["provider"].as_str())?;
        let asked = ask(args, p)?;
        let mut body = p.dialect.body(&asked);
        body["stream"] = json!(true);
        let timeout_ms = jobs::timeout_ms(&args["timeout_min"]).map_err(|e| bad(&e))?;
        let notify_url = match args["notify"].as_str() {
            Some(u) => Some(
                crate::notify::check_url(u, &[&self.host, &self.proxy_host])
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
        let tags: Vec<(String, String)> = tags(Some(caller), task.as_deref())
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let new = runner::New {
            id: random()?[..22].to_string(),
            caller: Some(caller.user.id.clone()),
            caller_name: Some(caller.user.name.clone()),
            for_id: caller.acting_for.as_ref().map(|f| f.id.clone()),
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
        let mut out = runner::call(
            cx.env,
            runner::INDEX,
            "submit",
            &serde_json::to_value(&new)?,
        )
        .await?;
        if let Some(k) = notify_key {
            out["notify_key"] = json!(k);
            out["notify_key_note"] = json!(crate::notify::KEY_NOTE);
        }
        Ok(out)
    }

    /// `op` (`status` or `result`) of job `id`, for the caller.
    async fn job(&self, cx: &Context<'_>, op: &str, args: &Value) -> Result<Value> {
        let reader = cx.caller.map(|c| c.user.id.as_str()).unwrap_or_default();
        let id = crate::arg(args, "id")?;
        runner::call(
            cx.env,
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

impl Capability for Models {
    fn name(&self) -> &'static str {
        "models"
    }

    fn sections(&self) -> Vec<Section> {
        vec![Section {
            tab: Tab::Gateways,
            id: "models",
            title: "Models",
            description: "Cost through the AI Gateway. No prompts or answers are kept.",
            order: 1,
            cost: Some("Model cost"),
        }]
    }

    /// Cost per day per model family over the last `days` days, from the gateway's logs.
    fn section_data<'a>(
        &'a self,
        cx: &'a Context<'a>,
        _id: &'a str,
        _person: &'a User,
        query: &'a Value,
    ) -> Call<'a> {
        Box::pin(async move {
            let days = query["days"].as_u64().unwrap_or(7).clamp(1, 30) as usize;
            let now = Date::new_0();
            let today = String::from(now.to_iso_string());
            // Reaching back past the start of the window's first UTC day; daily() trims.
            let since = now.get_time() - (days as f64) * 86_400_000.0;
            let logs = ai_gateway::logs(
                cx.account,
                &self.gateway,
                since,
                &Filter::default(),
                CHART_LOGS,
            )
            .await?;
            let mut answer = to_value(daily::daily(&logs, &today, days));
            answer["truncated"] = json!(logs.len() >= CHART_LOGS);
            Ok(answer)
        })
    }

    fn tools(&self) -> Vec<Tool> {
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

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            Ok(match tool {
                "models_setup" => self.setup(cx).await?,
                "models_providers" => {
                    to_value(ai_gateway::provider_configs(cx.account, &self.gateway).await?)
                }
                "models_run" => self.run(cx, args).await?,
                "models_usage" => self.usage(cx.account, args).await?,
                "models_logs" => self.recent(cx.account, args).await?,
                "models_submit" => self.submit(cx, args).await?,
                "models_status" => self.job(cx, "status", args).await?,
                "models_result" => self.job(cx, "result", args).await?,
                other => return Err(unknown(other)),
            })
        })
    }

    fn secrets(&self) -> Vec<Slot> {
        self.providers
            .iter()
            .map(|p| Slot {
                name: self.secret(p),
                purpose: format!(
                    "The {} key the AI gateway `{}` uses for alias `{}`. {}",
                    p.slug, self.gateway, p.alias, p.key_help
                ),
                scopes: &[SECRET_SCOPE],
            })
            .collect()
    }

    fn check_secret<'a>(&'a self, _cx: &'a Context<'a>, name: &'a str, value: &'a str) -> Call<'a> {
        Box::pin(async move {
            let p = self
                .providers
                .iter()
                .find(|p| self.secret(p) == name)
                .ok_or_else(|| bad("no provider uses this secret"))?;
            Ok(Value::String(p.dialect.check(value).await?))
        })
    }

    /// A provider's API key: Anthropic's API, like the others', answers no expiry, and the
    /// key is readable only by the AI gateway, so there is nothing to ask.
    fn secret_expiry<'a>(&'a self, _cx: &'a Context<'a>, _name: &'a str) -> crate::Expiring<'a> {
        Box::pin(async {
            Ok(crate::expiry::Expiry::Unknown {
                why: "the provider's API reports no key expiry".into(),
            })
        })
    }

    /// A key just set: the gateway is set up and given it.
    fn secret_set<'a>(&'a self, cx: &'a Context<'a>, name: &'a str, id: &'a str) -> Call<'a> {
        Box::pin(async move {
            let p = self
                .providers
                .iter()
                .find(|p| self.secret(p) == name)
                .ok_or_else(|| bad("no provider uses this secret"))?;
            ai_gateway::ensure(cx.account, &self.gateway).await?;
            let added =
                ai_gateway::ensure_provider(cx.account, &self.gateway, p.slug, p.alias, id).await?;
            Ok(Value::String(if added {
                format!("the gateway `{}` now uses it for {}", self.gateway, p.slug)
            } else {
                format!(
                    "the gateway `{}` already used it for {}",
                    self.gateway, p.slug
                )
            }))
        })
    }

    fn footprint(&self) -> Footprint {
        Footprint {
            gateways: vec![self.gateway.clone()],
            objects: vec![runner::BINDING.into()],
            ..Footprint::default()
        }
    }
}

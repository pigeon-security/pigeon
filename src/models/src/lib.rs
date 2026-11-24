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
use pigeon_core::ai_gateway::{self, Dimension, Filter, GatewayKind, SECRET_SCOPE, tag};
use pigeon_core::expiry::Expiry;
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::{self, Writer};
use pigeon_core::webauthn::random;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Env, Error, Fetch, Headers, Method, Request, RequestInit, Result};

pub mod anthropic;
pub mod billing;
pub mod cache;
pub mod capabilities;
pub mod daily;
pub mod jobs;
pub mod openai;
pub mod runner;
pub mod stream;
pub mod turn;

/// The capability's name.
pub const NAME: &str = "models";

/// The versions the model gateway declares: the `models` capability.
pub fn versions() -> Vec<pigeon_core::versions::Component> {
    vec![pigeon_core::versions::Component::began(
        NAME,
        pigeon_core::versions::Kind::Capability,
    )]
}

/// The model gateway Worker's script name.
pub const WORKER: &str = "pigeon-model-gateway";

/// Every tool the capability offers.
pub const TOOLS: [&str; 12] = [
    "models_setup",
    "models_providers",
    "models_run",
    "models_submit",
    "models_status",
    "models_result",
    "models_cancel",
    "models_logs",
    "models_usage",
    "models_cost",
    "models_daily",
    "models_billing",
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
        "models_submit" | "models_status" | "models_result" | "models_cancel"
        | "models_billing" => None,
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

/// Model providers behind one or more gateways.
pub struct Models {
    /// The gateways, each with its kind. The first is the default for a job stored without one.
    pub gateways: Vec<Gateway>,
    /// The providers, the first the default.
    pub providers: Vec<Provider>,
    /// Extra key slots, as (gateway, slug, alias): declared and handed to their gateway by setup
    /// and when set, though no provider routes there yet.
    pub staged: Vec<(&'static str, &'static str, &'static str)>,
    /// Where a person sets a key, such as `https://platform.pigeon.dev/secrets`.
    pub secrets_page: String,
    /// The platform's hostname, which a job's notify may not point at.
    pub host: String,
    /// The proxies' hostname, which a job's notify may not point at either.
    pub proxy_host: String,
}

/// An AI gateway pigeon uses, and who pays for what goes through it.
#[derive(Clone, Debug)]
pub struct Gateway {
    /// The gateway's ID, such as `pigeon`.
    pub id: String,
    /// Whether it bills stored keys or Cloudflare credits.
    pub kind: GatewayKind,
}

/// Who pays for a provider's requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Route {
    /// A key stored on the gateway, the person's own.
    Byok,
    /// Cloudflare credits (Unified Billing).
    Credits,
}

impl Route {
    /// The route's name in answers: `byok` or `credits`.
    pub fn name(self) -> &'static str {
        match self {
            Route::Byok => "byok",
            Route::Credits => "credits",
        }
    }
}

/// How a log is labelled: the provider name and route it counts under. A log of a credits gateway
/// is `cf-{provider}` by credits. A log of a BYOK or retired gateway is the log's own provider,
/// by key. Workers AI is the exception: its log is named `workers-ai`, by credits. Pure, and not
/// dependent on time.
pub fn route_of(kind: GatewayKind, log_provider: &str) -> (String, Route) {
    match kind {
        GatewayKind::Credits if log_provider == ai_gateway::WORKERS_AI => {
            (log_provider.to_string(), Route::Credits)
        }
        GatewayKind::Credits => (format!("cf-{log_provider}"), Route::Credits),
        GatewayKind::Byok | GatewayKind::Retired => (log_provider.to_string(), Route::Byok),
    }
}

/// A provider as the gateway knows it.
pub struct Provider {
    /// What callers select it by and choices list, such as `cf-anthropic`.
    pub name: &'static str,
    /// The gateway it goes through, by ID.
    pub gateway: &'static str,
    /// Who pays: a stored key, or credits.
    pub route: Route,
    /// The gateway's name for it (its path), such as `anthropic`.
    pub slug: &'static str,
    /// Which of its keys, such as `default`.
    pub alias: &'static str,
    /// How its requests and answers are shaped.
    pub dialect: Dialect,
    /// What a model id looks like for it.
    pub ids: ModelIds,
    /// The model a request names when it names none.
    pub default_model: &'static str,
    /// Where to get a key, for the secrets page.
    pub key_help: &'static str,
    /// Where the provider's own billed usage is read, when it can be.
    pub billing: Option<Billing>,
}

/// Where a provider's own billed usage is read. OpenAI's usage API could be a later variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Billing {
    /// Anthropic's Admin API usage and cost reports, read with an admin key.
    AnthropicAdmin,
}

/// How a provider's requests and answers are shaped.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// The Claude Platform's Messages API.
    Anthropic,
    /// OpenAI's Chat Completions API.
    #[serde(rename = "openai")]
    OpenAi,
}

/// What a model id looks like for one provider. A refusal names the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelIds {
    /// A Claude id: `claude-` and a name.
    Claude,
    /// A plain id: letters, digits, `.`, `-`, `_` and `:`, 100 characters at most.
    Plain,
    /// A Workers AI id: exactly `@cf/<org>/<model>`, each part of letters, digits, `.`, `-`, `_`.
    WorkersAi,
}

impl ModelIds {
    /// Whether `name` is an id this rule takes. The error names the rule.
    pub fn check(self, name: &str) -> std::result::Result<(), String> {
        let part = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        };
        let ok = match self {
            ModelIds::Claude => name.starts_with("claude-"),
            ModelIds::Plain => {
                !name.is_empty()
                    && name.len() <= 100
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
            }
            ModelIds::WorkersAi => {
                let mut parts = name.strip_prefix("@cf/").unwrap_or("").split('/');
                name.len() <= 100
                    && matches!(
                        (parts.next(), parts.next(), parts.next()),
                        (Some(org), Some(model), None) if part(org) && part(model)
                    )
            }
        };
        if ok {
            Ok(())
        } else {
            Err(self.rule().to_string())
        }
    }

    /// The rule in words, as a refusal says it.
    pub fn rule(self) -> &'static str {
        match self {
            ModelIds::Claude => "`model` must name a Claude model, such as claude-sonnet-5-5",
            ModelIds::Plain => {
                "`model` must be a plain model id: letters, digits, `.`, `-`, `_` and `:`, \
                 100 characters at most"
            }
            ModelIds::WorkersAi => {
                "`model` must be a Workers AI model id, exactly `@cf/<org>/<model>`, each part \
                 of letters, digits, `.`, `-` and `_`, 100 characters at most"
            }
        }
    }
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
            Dialect::OpenAi => "chat/completions",
        }
    }

    /// The provider's path for a request with this `body`: OpenAI's `responses` for a body in
    /// the Responses shape (`input`, no `messages`), else [`Dialect::path`].
    pub fn path_for(self, body: &Value) -> &'static str {
        if self == Dialect::OpenAi && body.get("input").is_some() && body.get("messages").is_none()
        {
            "responses"
        } else {
            self.path()
        }
    }

    /// Headers a request needs beside the gateway's.
    pub fn headers(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Dialect::Anthropic => &[("anthropic-version", "2023-06-01")],
            Dialect::OpenAi => &[],
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
            Dialect::OpenAi => {
                let mut messages = Vec::new();
                if let Some(s) = &ask.system {
                    messages.push(json!({ "role": "system", "content": s }));
                }
                if let Some(list) = ask.messages.as_array() {
                    messages.extend(list.iter().cloned());
                }
                json!({
                    "model": ask.model,
                    "max_completion_tokens": ask.max_tokens,
                    "messages": messages,
                })
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
            Dialect::OpenAi => {
                if v["output"].is_array() {
                    return Ok(responses_reply(v));
                }
                let choice = v["choices"]
                    .get(0)
                    .ok_or_else(|| bad(&format!("not a Chat Completions answer: {v}")))?;
                let finish = choice["finish_reason"].as_str().map(|r| {
                    match r {
                        "stop" => "end_turn",
                        "tool_calls" => "tool_use",
                        "length" => "max_tokens",
                        "content_filter" => "refusal",
                        other => other,
                    }
                    .to_string()
                });
                Ok(Reply {
                    model: v["model"].as_str().unwrap_or_default().into(),
                    text: choice["message"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    stop_reason: finish,
                    tokens_in: v["usage"]["prompt_tokens"].as_u64(),
                    tokens_out: v["usage"]["completion_tokens"].as_u64(),
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
            Dialect::OpenAi => {
                let headers = Headers::new();
                headers.set("Authorization", &format!("Bearer {key}"))?;
                let mut init = RequestInit::new();
                init.with_method(Method::Get).with_headers(headers);
                let url = "https://api.openai.com/v1/models";
                let mut r = Fetch::Request(Request::new_with_init(url, &init)?)
                    .send()
                    .await?;
                let status = r.status_code();
                let text = r.text().await?;
                if (200..300).contains(&status) {
                    Ok("OpenAI accepts it".into())
                } else {
                    let text = if key.is_empty() {
                        text
                    } else {
                        text.replace(key, "[key]")
                    };
                    let shown: String = text.chars().take(200).collect();
                    Err(bad(&format!("OpenAI refused it: {status}: {shown}")))
                }
            }
        }
    }
}

/// A checked Messages `body` in the shape `dialect` takes: shaped for the model by the capability
/// table for Anthropic, translated for OpenAI. A body the model cannot take is refused.
fn translated(dialect: Dialect, body: Value) -> Result<Value> {
    match dialect {
        Dialect::Anthropic => Ok(anthropic::request(&body)),
        Dialect::OpenAi => turn::refuse(openai::request(&body)).map_err(|e| bad(&e)),
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

/// `pairs` as owned strings, for a job to carry.
fn own(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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

/// The log ids in `args` for `models_cost`: 1 to [`ai_gateway::MAX_LOG_IDS`], each checked by
/// [`ai_gateway::check_log_id`], so no unchecked id reaches a URL.
pub fn log_ids(args: &Value) -> Result<Vec<String>> {
    let list = args["log_ids"]
        .as_array()
        .ok_or_else(|| bad("`log_ids` is required: a list of log ids"))?;
    if list.is_empty() || list.len() > ai_gateway::MAX_LOG_IDS {
        return Err(bad(&format!(
            "`log_ids` takes 1 to {} ids, not {}",
            ai_gateway::MAX_LOG_IDS,
            list.len()
        )));
    }
    list.iter()
        .enumerate()
        .map(|(n, v)| {
            let id = v
                .as_str()
                .ok_or_else(|| bad(&format!("`log_ids`[{n}] is not a string")))?;
            ai_gateway::check_log_id(id).map_err(|e| bad(&format!("`log_ids`[{n}]: {e}")))?;
            Ok(id.to_string())
        })
        .collect()
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

/// A key slot: a stored key a gateway uses for a provider, under an alias.
struct Slot {
    gateway: &'static str,
    slug: &'static str,
    alias: &'static str,
    /// Where to get the key.
    help: &'static str,
    /// How the provider is spoken to, for checking a key.
    dialect: Dialect,
}

/// How long a credits gateway's reading is kept in one isolate.
const GUARD_TTL_MS: f64 = 300_000.0;

type Guarded = (f64, Value, Vec<ai_gateway::ProviderConfig>);

thread_local! {
    static GUARDED: std::cell::RefCell<std::collections::HashMap<String, Guarded>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The order gateways are read in: Byok, then Retired, then Credits.
fn kind_rank(kind: GatewayKind) -> u8 {
    match kind {
        GatewayKind::Byok => 0,
        GatewayKind::Retired => 1,
        GatewayKind::Credits => 2,
    }
}

/// Whether a Cloudflare API error text says the gateway does not exist: a 404, or a body with
/// Cloudflare's "not found" message (the API's exact answer for a missing gateway is not verified).
pub fn is_missing(error: &str) -> bool {
    error.contains(": 404:") || error.to_lowercase().contains("not found")
}

/// Reads gateway `gateway`'s settings and keys; `None` when it does not exist (the API's 404).
async fn read_gateway(
    account: &Account,
    gateway: &str,
) -> Result<Option<(Value, Vec<ai_gateway::ProviderConfig>)>> {
    let not_found = |e: &worker::Error| is_missing(&e.to_string());
    let settings = match ai_gateway::gateway_settings(account, gateway).await {
        Ok(s) => s,
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    let configs = match ai_gateway::provider_configs(account, gateway).await {
        Ok(c) => c,
        Err(e) if not_found(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some((settings, configs)))
}

/// Refuses a credits request on `gateway` unless the gateway would really bill credits: see
/// [`ai_gateway::credits_guard`]. A gateway that does not exist is refused. The gateway and its
/// keys are read at most every 5 minutes per isolate.
pub async fn guard_credits(account: &Account, gateway: &str) -> Result<()> {
    let fresh = GUARDED.with(|g| {
        g.borrow()
            .get(gateway)
            .filter(|(at, ..)| Date::now() - at < GUARD_TTL_MS)
            .cloned()
    });
    let (_, settings, configs) = match fresh {
        Some(g) => g,
        None => {
            let Some((settings, configs)) = read_gateway(account, gateway).await? else {
                return Err(bad(&format!(
                    "the credits gateway `{gateway}` does not exist: run models_setup"
                )));
            };
            let read = (Date::now(), settings, configs);
            GUARDED.with(|g| g.borrow_mut().insert(gateway.to_string(), read.clone()));
            read
        }
    };
    ai_gateway::credits_guard(gateway, &settings, &configs).map_err(|e| bad(&e))
}

/// [`guard_credits`] for a request to `provider`: for Workers AI it also checks the gateway bills
/// Workers AI as Unified Billing ([`ai_gateway::workers_ai_guard`]).
pub async fn guard_credits_for(account: &Account, gateway: &str, provider: &str) -> Result<()> {
    guard_credits(account, gateway).await?;
    if provider != ai_gateway::WORKERS_AI {
        return Ok(());
    }
    let settings = GUARDED.with(|g| g.borrow().get(gateway).map(|(_, s, _)| s.clone()));
    match settings {
        Some(s) => ai_gateway::workers_ai_guard(gateway, &s).map_err(|e| bad(&e)),
        None => Err(bad(&format!(
            "the credits gateway `{gateway}` was not read: try again"
        ))),
    }
}

thread_local! {
    /// When each BYOK (gateway, slug) last passed [`guard_byok`]. Only successes are kept.
    static BYOK_READY: std::cell::RefCell<std::collections::HashMap<String, f64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Refuses a BYOK request for `slug` on `gateway` unless it is ready: see
/// [`ai_gateway::byok_ready`]. Only a success is kept, for 5 minutes per isolate. When the
/// gateway cannot be read for a reason other than not existing, it logs a warning and lets the
/// request go ahead, so a Cloudflare API error does not stop every turn.
pub async fn guard_byok(account: &Account, gateway: &str, slug: &str) -> Result<()> {
    let key = format!("{gateway}/{slug}");
    let fresh = BYOK_READY.with(|g| {
        g.borrow()
            .get(&key)
            .is_some_and(|at| Date::now() - at < GUARD_TTL_MS)
    });
    if fresh {
        return Ok(());
    }
    let read = match read_gateway(account, gateway).await {
        Ok(r) => r,
        Err(e) => {
            worker::console_warn!("BYOK readiness of `{gateway}` could not be read: {e}");
            return Ok(());
        }
    };
    let (settings, configs) = match &read {
        Some((s, c)) => (Some(s), c.as_slice()),
        None => (None, &[][..]),
    };
    ai_gateway::byok_ready(settings, configs, gateway, slug).map_err(|e| bad(&e))?;
    BYOK_READY.with(|g| g.borrow_mut().insert(key, Date::now()));
    Ok(())
}

/// Hands a key to `gateway` as `provider`'s, only when the gateway is declared Byok. Never to a
/// credits or retired gateway, whatever is declared or staged.
async fn hand_key(
    kinds: &[Gateway],
    account: &Account,
    gateway: &str,
    provider: &str,
    alias: &str,
    secret_id: &str,
) -> Result<bool> {
    let byok = kinds
        .iter()
        .any(|g| g.id == gateway && g.kind == GatewayKind::Byok);
    if !byok {
        return Err(bad(&format!(
            "gateway `{gateway}` is not a BYOK gateway: keys are never handed to it"
        )));
    }
    ai_gateway::ensure_provider(account, gateway, provider, alias, secret_id).await
}

impl Models {
    /// The key slots: every BYOK provider's, then the staged ones.
    fn slots(&self) -> Vec<Slot> {
        let mut out: Vec<Slot> = self
            .providers
            .iter()
            .filter(|p| p.route == Route::Byok)
            .map(|p| Slot {
                gateway: p.gateway,
                slug: p.slug,
                alias: p.alias,
                help: p.key_help,
                dialect: p.dialect,
            })
            .collect();
        for (gateway, slug, alias) in &self.staged {
            if let Some(p) = self.providers.iter().find(|p| p.slug == *slug) {
                out.push(Slot {
                    gateway,
                    slug,
                    alias,
                    help: p.key_help,
                    dialect: p.dialect,
                });
            }
        }
        out
    }

    fn secret(&self, s: &Slot) -> String {
        ai_gateway::secret_name(s.gateway, s.slug, s.alias)
    }

    /// The gateway `id`'s kind; Byok when it is not declared.
    fn kind_of(&self, id: &str) -> GatewayKind {
        self.gateways
            .iter()
            .find(|g| g.id == id)
            .map_or(GatewayKind::Byok, |g| g.kind)
    }

    /// The provider called `name`; the first when none is named.
    fn provider(&self, name: Option<&str>) -> Result<&Provider> {
        match name {
            None => self
                .providers
                .first()
                .ok_or_else(|| bad("no provider is declared")),
            Some(s) => self
                .providers
                .iter()
                .find(|p| p.name == s)
                .ok_or_else(|| bad(&format!("no provider `{s}`"))),
        }
    }

    /// `models_providers`: each gateway with its kind, whether it exists and the keys it holds,
    /// and each provider with its gateway, route and whether it is ready to use, and why not.
    async fn providers_view(&self, account: &Account) -> Result<Value> {
        let mut read = Vec::new();
        for g in &self.gateways {
            read.push((g, read_gateway(account, &g.id).await?));
        }
        let gateways: Vec<Value> = read
            .iter()
            .map(|(g, r)| {
                json!({
                    "id": g.id,
                    "kind": g.kind,
                    "exists": r.is_some(),
                    "keys": r.as_ref().map(|(_, c)| to_value(c)).unwrap_or_else(|| json!([])),
                })
            })
            .collect();
        let providers: Vec<Value> = self
            .providers
            .iter()
            .map(|p| {
                let found = read
                    .iter()
                    .find(|(g, _)| g.id == p.gateway)
                    .and_then(|(_, r)| r.as_ref());
                let (settings, configs) = match found {
                    Some((s, c)) => (Some(s), c.as_slice()),
                    None => (None, &[][..]),
                };
                let verdict = match p.route {
                    Route::Byok => ai_gateway::byok_ready(settings, configs, p.gateway, p.slug),
                    Route::Credits => match settings {
                        Some(s) => {
                            ai_gateway::credits_guard(p.gateway, s, configs).and_then(|()| {
                                if p.slug == ai_gateway::WORKERS_AI {
                                    ai_gateway::workers_ai_guard(p.gateway, s)
                                } else {
                                    Ok(())
                                }
                            })
                        }
                        None => Err(format!(
                            "the credits gateway `{}` does not exist: run models_setup",
                            p.gateway
                        )),
                    },
                };
                json!({
                    "name": p.name,
                    "gateway": p.gateway,
                    "route": p.route.name(),
                    "ready": verdict.is_ok(),
                    "why": verdict.err(),
                })
            })
            .collect();
        Ok(json!({ "gateways": gateways, "providers": providers }))
    }

    /// Every gateway, and every key slot's key handed to its gateway where the key is set.
    async fn setup(&self, account: &Account) -> Result<Value> {
        let mut ensured = Vec::new();
        for g in &self.gateways {
            ensured.push(ai_gateway::ensure(account, &g.id, g.kind).await?);
        }
        let writer = Writer::open(account).await?;
        let stored = writer.list().await;
        let stored = writer.finish(stored).await?;
        let mut providers = Vec::new();
        for p in self.slots() {
            let name = self.secret(&p);
            let state = match stored.iter().find(|s| s.name == name) {
                Some(s) => {
                    let added =
                        hand_key(&self.gateways, account, p.gateway, p.slug, p.alias, &s.id)
                            .await?;
                    json!({ "gateway": p.gateway, "provider": p.slug, "alias": p.alias, "key": "set", "added_to_gateway": added })
                }
                None => json!({
                    "gateway": p.gateway, "provider": p.slug, "alias": p.alias, "key": "missing",
                    "set_it_at": format!("{}#{name}", self.secrets_page),
                }),
            };
            providers.push(state);
        }
        // The keys each gateway holds are read after the new ones were handed over.
        let mut gateways = Vec::new();
        for (g, mut e) in self.gateways.iter().zip(ensured) {
            let configs = ai_gateway::provider_configs(account, &g.id).await?;
            let declared: Vec<&str> = self
                .slots()
                .iter()
                .filter(|s| s.gateway == g.id)
                .map(|s| s.slug)
                .collect();
            e.drift
                .extend(ai_gateway::stored_key_drift(g.kind, &configs, &declared));
            gateways.push(json!({ "kind": g.kind, "gateway": e }));
        }
        Ok(json!({ "gateways": gateways, "providers": providers }))
    }

    async fn run(
        &self,
        env: &Env,
        account: &Account,
        caller: &Value,
        args: &Value,
    ) -> Result<Value> {
        let p = self.provider(args["provider"].as_str())?;
        match p.route {
            Route::Credits => guard_credits_for(account, p.gateway, p.slug).await?,
            Route::Byok => guard_byok(account, p.gateway, p.slug).await?,
        }
        let raw = args.get("raw").filter(|r| r.is_object());
        let body = match raw {
            Some(r) => r.clone(),
            None => p.dialect.body(&ask(args, p)?),
        };
        let tags = tags(caller, args["task"].as_str());
        let tags: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (*k, v.as_str())).collect();
        // Workers AI: a token for this call only, revoked whether the call worked or not.
        let call_token = open_call_token(env, account, caller, p.slug).await?;
        let mut headers = p.dialect.headers().to_vec();
        let extra = call_token
            .as_ref()
            .map(|t| t.headers(p.gateway))
            .unwrap_or_default();
        headers.extend(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let ran = ai_gateway::run(
            account,
            p.gateway,
            p.slug,
            p.dialect.path_for(&body),
            &headers,
            &tags,
            &body,
        )
        .await;
        if let Some(t) = call_token {
            close_call_token(env, account, caller, t).await;
        }
        let ran = ran?;
        if !(200..300).contains(&ran.status) {
            if p.route == Route::Credits
                && let Some(plain) = ai_gateway::error_code(&ran.body)
                    .and_then(|c| ai_gateway::credits_error(ran.status, c))
            {
                return Err(bad(&plain));
            }
            return Err(bad(&format!(
                "{} answered {}: {}",
                p.name, ran.status, ran.body
            )));
        }
        if raw.is_some() {
            return Ok(ran.body);
        }
        let mut reply = to_value(p.dialect.reply(&ran.body)?);
        reply["provider"] = json!(p.name);
        Ok(reply)
    }

    /// Every declared gateway's logs since `since_ms`, each with its kind and whether the read
    /// stopped at `max`; Byok gateways first.
    async fn gateway_logs(
        &self,
        account: &Account,
        since_ms: f64,
        filter: &Filter,
        max: usize,
    ) -> Result<Vec<daily::GatewayLogs>> {
        let mut order: Vec<&Gateway> = self.gateways.iter().collect();
        order.sort_by_key(|g| kind_rank(g.kind));
        let mut out = Vec::new();
        for g in order {
            let logs = ai_gateway::logs(account, &g.id, since_ms, filter, max).await?;
            out.push(daily::GatewayLogs {
                id: g.id.clone(),
                kind: g.kind,
                truncated: logs.len() >= max,
                logs,
            });
        }
        Ok(out)
    }

    /// Every gateway's logs in one list, each log's provider replaced by its [`route_of`] name,
    /// newest first.
    async fn labelled_logs(
        &self,
        account: &Account,
        since_ms: f64,
        filter: &Filter,
        max: usize,
    ) -> Result<Vec<ai_gateway::Log>> {
        let mut all = Vec::new();
        for g in self.gateway_logs(account, since_ms, filter, max).await? {
            for mut log in g.logs {
                log.provider = route_of(g.kind, &log.provider).0;
                all.push(log);
            }
        }
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(max);
        Ok(all)
    }

    /// The gateway's latest log entries: what each request was, never what it said.
    async fn recent(&self, account: &Account, args: &Value) -> Result<Value> {
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let limit = log_limit(args);
        let since = Date::now() - hours * 3_600_000.0;
        let logs = self
            .labelled_logs(account, since, &filter(args), limit)
            .await?;
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
        // The job's request is checked here, so a job is not queued only to fail.
        let mut headers = own(p.dialect.headers());
        let (model, body) = match args.get("request").filter(|r| !r.is_null()) {
            // An AI agent's turn: a whole Messages body, checked.
            Some(request) => {
                if !args["prompt"].is_null() || !args["messages"].is_null() {
                    return Err(bad("give `request`, or `prompt`/`messages`, not both"));
                }
                let t = turn::turn(request, &args["betas"], p.default_model, p.dialect, p.ids)
                    .map_err(|e| bad(&e))?;
                if let Some(b) = t.beta {
                    headers.push(("anthropic-beta".into(), b));
                }
                (t.model, translated(p.dialect, t.body)?)
            }
            None => {
                if !args["betas"].is_null() {
                    return Err(bad("`betas` goes with `request` only"));
                }
                let asked = ask(args, p)?;
                let mut body = p.dialect.body(&asked);
                body["stream"] = json!(true);
                if p.dialect == Dialect::OpenAi {
                    body["stream_options"] = json!({ "include_usage": true });
                }
                (asked.model, body)
            }
        };
        let lane = jobs::lane(&args["lane"]).map_err(|e| bad(&e))?;
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
            model,
            notify_url,
            notify_key: notify_key.clone(),
            lane: lane.into(),
            ask: runner::Ask {
                dialect: p.dialect,
                provider: p.slug.into(),
                path: p.dialect.path_for(&body).into(),
                headers,
                tags,
                body,
                timeout_ms,
                gateway: Some(p.gateway.to_string()),
                credits: p.route == Route::Credits,
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

    /// `models_cost`: the gateway's own cost of each request, by its log id, looked up on every
    /// declared gateway (Byok first); the first that has a log answers for it.
    async fn cost(&self, account: &Account, args: &Value) -> Result<Value> {
        let ids = log_ids(args)?;
        let mut order: Vec<&Gateway> = self.gateways.iter().collect();
        order.sort_by_key(|g| kind_rank(g.kind));
        let mut merged: Vec<ai_gateway::LogLookup> =
            vec![ai_gateway::LogLookup::NotFound; ids.len()];
        for g in order {
            let missing: Vec<usize> = (0..ids.len())
                .filter(|i| !matches!(merged[*i], ai_gateway::LogLookup::Found(_)))
                .collect();
            if missing.is_empty() {
                break;
            }
            let wanted: Vec<String> = missing.iter().map(|i| ids[*i].clone()).collect();
            let looked = ai_gateway::log_costs(account, &g.id, &wanted).await?;
            for (i, l) in missing.into_iter().zip(looked) {
                // A failure is kept only where no gateway has found the log.
                if matches!(l, ai_gateway::LogLookup::Found(_))
                    || !matches!(l, ai_gateway::LogLookup::NotFound)
                {
                    merged[i] = l;
                }
            }
        }
        Ok(ai_gateway::cost_answer(&ids, &merged))
    }

    /// `models_usage`: the gateway's logs and the model gateway's cache tokens, added up.
    async fn usage(&self, env: &Env, account: &Account, args: &Value) -> Result<Value> {
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let by = dimensions(args)?;
        let filter = filter(args);
        let since = Date::now() - hours * 3_600_000.0;
        let prefix = args["task_prefix"].as_str().filter(|p| !p.is_empty());
        let mut logs = self.labelled_logs(account, since, &filter, 2_000).await?;
        if let Some(prefix) = prefix {
            logs.retain(|l| {
                l.metadata
                    .get("task")
                    .is_some_and(|t| t.starts_with(prefix))
            });
        }
        let groups = ai_gateway::summarize(&logs, &by);
        let cost: f64 = groups.iter().filter_map(|u| u.cost).sum();
        // The cache token counts are not in the gateway's logs: the model gateway's jobs kept
        // them. The gateway's cost already includes the cache, so none is added to it.
        let rows: Vec<cache::Row> = serde_json::from_value(
            runner::call(
                env,
                runner::INDEX,
                "cache_usage",
                &json!({ "since": since as i64 }),
            )
            .await?,
        )?;
        let rows: Vec<cache::Row> = rows
            .into_iter()
            .filter(|r| cache::keeps(r, &filter, prefix))
            .collect();
        let tokens_in: u64 = groups.iter().map(|u| u.tokens_in).sum();
        let tokens_out: u64 = groups.iter().map(|u| u.tokens_out).sum();
        let payloads_kept: u64 = groups.iter().map(|u| u.payloads_kept).sum();
        let (groups, cached) = cache::merge(groups, &rows, &by);
        let by: Vec<&str> = by
            .iter()
            .filter_map(|d| Dimension::ALL.iter().find(|(_, x)| x == d).map(|(n, _)| *n))
            .collect();
        Ok(json!({
            "since": String::from(Date::new(&since.into()).to_iso_string()),
            "hours": hours,
            "requests": logs.len(),
            "truncated": logs.len() >= 2_000,
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "cost": cost,
            "cache_read_tokens": cached.read,
            "cache_write_tokens": cached.write,
            "cost_with_cache": cost,
            "payloads_kept": payloads_kept,
            "by": by,
            "groups": groups,
        }))
    }
}

impl Models {
    /// `models_daily`: every gateway's cost per UTC day over the last `days` days (1 to 31), by
    /// model family, provider name, route and AI agent run.
    async fn daily_by_run(&self, account: &Account, args: &Value) -> Result<Value> {
        let days = args["days"].as_u64().unwrap_or(7).clamp(1, 31) as usize;
        let now = Date::new_0();
        let today = String::from(now.to_iso_string());
        let since = now.get_time() - (days as f64) * 86_400_000.0;
        let gateways = self
            .gateway_logs(account, since, &Filter::default(), CHART_LOGS)
            .await?;
        let truncated = gateways.iter().any(|g| g.truncated);
        let mut answer = to_value(daily::daily_by_gateways(&gateways, &today, days));
        answer["truncated"] = json!(truncated);
        Ok(answer)
    }
}

/// The credential purpose of a Workers AI call: core mints a token with Workers AI Read.
const WORKERS_AI_PURPOSE: &str = "workers_ai";

/// A token minted for one Workers AI call. It is never stored, logged or returned: use it for
/// the call, then hand it to [`close_call_token`].
pub struct CallToken {
    /// The bearer value.
    value: String,
    /// The id Cloudflare knows the token by, to revoke it.
    id: String,
}

impl CallToken {
    /// The headers of the call: its gateway named and this token
    /// ([`ai_gateway::workers_ai_headers`]).
    pub fn headers(&self, gateway: &str) -> Vec<(String, String)> {
        ai_gateway::workers_ai_headers(gateway, &self.value)
    }
}

/// Asks for a token for one call to `provider`: `None` unless it is Workers AI, which needs a
/// token per call and holds none. With a parent token in `account` the token is minted here;
/// otherwise core mints it for `caller`.
pub async fn open_call_token(
    env: &Env,
    account: &Account,
    caller: &Value,
    provider: &str,
) -> Result<Option<CallToken>> {
    if provider != ai_gateway::WORKERS_AI {
        return Ok(None);
    }
    if !account.parent.is_empty() {
        let request = pigeon_core::tokens::MintRequest {
            purpose: format!("worker-{WORKERS_AI_PURPOSE}"),
            policies: vec![pigeon_core::tokens::Policy::new(
                pigeon_core::tokens::Scope::Account,
                &["Workers AI Read"],
            )],
            ttl_secs: 900,
        };
        let m = pigeon_core::tokens::mint(&account.parent, &account.id, &request).await?;
        return Ok(Some(CallToken {
            value: m.value,
            id: m.id,
        }));
    }
    let given = credential_given(env, caller, &[WORKERS_AI_PURPOSE]).await?;
    let id = given
        .token_id
        .filter(|i| !i.is_empty())
        .ok_or_else(|| bad("core gave no Workers AI credential: its answer had no token id"))?;
    Ok(Some(CallToken {
        value: given.token,
        id,
    }))
}

/// Ends a call's token. With a parent token in `account` the call minted its own token and
/// revokes it. A token from core's `credential` is shared and core owns its life, so nothing is
/// revoked. A failure is logged as a warning and never fails the turn: the token expires on its
/// own in minutes.
pub async fn close_call_token(_env: &Env, account: &Account, _caller: &Value, token: CallToken) {
    if account.parent.is_empty() {
        return;
    }
    if let Err(e) = pigeon_core::tokens::revoke(&account.parent, &account.id, &token.id).await {
        worker::console_warn!("a Workers AI call token could not be revoked: {e}");
    }
}

/// What the admin key's `Set` answers: nothing is handed to the AI gateway.
const ADMIN_SET_NOTE: &str = "the model gateway reads the key through its binding";

/// The admin key, from its Secrets Store binding. The error names the Access tab and never a
/// value.
async fn admin_key(env: &Env) -> Result<String> {
    let missing = || {
        bad(
            "ANTHROPIC_ADMIN_KEY is not set: set it on the console's Access tab (Secrets and credentials)",
        )
    };
    let store = env
        .secret_store(billing::ADMIN_KEY)
        .map_err(|_| missing())?;
    secrets_store::read(&store)
        .await
        .ok()
        .flatten()
        .filter(|k| !k.is_empty())
        .ok_or_else(missing)
}

/// One GET of the Admin API with `key`: the status and the body (null when it is not JSON).
async fn admin_get(url: &str, key: &str) -> Result<(u16, Value)> {
    let headers = Headers::new();
    headers.set("x-api-key", key)?;
    headers.set("anthropic-version", "2023-06-01")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let mut r = Fetch::Request(Request::new_with_init(url, &init)?)
        .send()
        .await?;
    let status = r.status_code();
    let text = r.text().await?;
    Ok((status, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// A short reason from a failed report's answer; never the key.
fn why_failed(status: u16, body: &Value) -> String {
    let msg = body["error"]["message"].as_str().unwrap_or_default();
    let shown: String = msg.chars().take(200).collect();
    format!("Anthropic answered {status}: {shown}")
}

/// The status of a one-row usage report read, from yesterday to now.
async fn admin_probe(key: &str) -> Result<u16> {
    let now = String::from(Date::new_0().to_iso_string());
    let today = now.get(..10).unwrap_or_default();
    let from = daily::days_ending(today, 2)
        .into_iter()
        .next()
        .unwrap_or_default();
    let url = billing::usage_url(&format!("{from}T00:00:00Z"), &now, 1, None);
    Ok(admin_get(&url, key).await?.0)
}

impl Models {
    /// `models_billing`: a provider's own billed usage per UTC day. A report that fails goes to
    /// `unavailable` and the other still answers.
    async fn billing(&self, env: &Env, args: &Value) -> Result<Value> {
        let p = match args["provider"].as_str() {
            Some(s) => self.provider(Some(s))?,
            None => self
                .providers
                .iter()
                .find(|p| p.billing.is_some())
                .ok_or_else(|| bad("no provider has a billing source"))?,
        };
        if p.billing != Some(Billing::AnthropicAdmin) {
            return Err(bad(&format!("`{}` has no billing source", p.name)));
        }
        let n = args["days"].as_u64().unwrap_or(7).clamp(1, 31) as usize;
        let key = admin_key(env).await?;
        let now = String::from(Date::new_0().to_iso_string());
        let today = now.get(..10).unwrap_or_default();
        let days = daily::days_ending(today, n);
        let start = format!("{}T00:00:00Z", days.first().map_or(today, String::as_str));
        let mut by_day = std::collections::BTreeMap::new();
        let mut unavailable = Vec::new();
        for report in ["usage_report", "cost_report"] {
            let mut page: Option<String> = None;
            let mut seen = std::collections::BTreeMap::new();
            let failed = 'pages: {
                for _ in 0..billing::MAX_PAGES {
                    let url = if report == "usage_report" {
                        billing::usage_url(&start, &now, billing::USAGE_LIMIT, page.as_deref())
                    } else {
                        billing::cost_url(&start, &now, page.as_deref())
                    };
                    let (status, body) = match admin_get(&url, &key).await {
                        Ok(r) => r,
                        Err(e) => break 'pages Some(e.to_string().replace(&key, "…")),
                    };
                    if status != 200 {
                        break 'pages Some(why_failed(status, &body));
                    }
                    let more = if report == "usage_report" {
                        billing::add_usage(&mut seen, &body)
                    } else {
                        billing::add_cost(&mut seen, &body)
                    };
                    match (more.has_more, more.next_page) {
                        (true, Some(next)) => page = Some(next),
                        _ => break 'pages None,
                    }
                }
                None
            };
            match failed {
                // A failed report leaves out what it read: half a report is wrong.
                Some(why) => unavailable.push(json!({ "source": report, "why": why })),
                None => merge_report(&mut by_day, seen, report),
            }
        }
        Ok(billing::answer(&days, &by_day, &unavailable))
    }
}

/// Folds one report's days into `all`: usage gives tokens, cost gives dollars.
fn merge_report(
    all: &mut std::collections::BTreeMap<String, billing::Day>,
    seen: std::collections::BTreeMap<String, billing::Day>,
    report: &str,
) {
    for (day, d) in seen {
        let into = all.entry(day).or_default();
        if report == "usage_report" {
            into.tokens = d.tokens;
        } else {
            into.cost_usd = d.cost_usd;
        }
        for (m, e) in d.by_model {
            let slot = into.by_model.entry(m).or_default();
            if report == "usage_report" {
                slot.tokens = e.tokens;
            } else {
                slot.cost_usd = e.cost_usd;
            }
        }
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
        let slugs: Vec<&str> = self.providers.iter().map(|p| p.name).collect();
        vec![
            Tool::bare(
                "models_setup",
                "Have the AI gateway exist and hand it every provider key that is set. A \
                 missing key comes back with a link to the secrets page; it runs again by \
                 itself when a key is set there.",
            ),
            Tool::bare(
                "models_providers",
                "The AI gateways (kind, whether they exist, the provider keys they hold: provider, alias, a preview of the key) and each provider with its gateway, route and whether it is ready, and why not. \
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
                        "messages": { "type": "array", "items": { "type": "object", "additionalProperties": true } },
                        "max_tokens": { "type": "integer", "description": "1024 by default." },
                        "raw": { "type": "object", "additionalProperties": true },
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
                              it ends (the key is shown once). Four run at once in the default \
                              lane and sixteen in the ai-agents lane; more wait. A refusal before any \
                              answer arrives (overloaded, rate-limited) is tried again, up to 3 \
                              times.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": slugs, "description": "The first declared when omitted." },
                        "model": { "type": "string", "description": "The provider's default when omitted." },
                        "system": { "type": "string" },
                        "prompt": { "type": "string" },
                        "messages": { "type": "array", "items": { "type": "object", "additionalProperties": true } },
                        "max_tokens": { "type": "integer", "description": "1024 by default, 64000 at most." },
                        "task": { "type": "string", "description": "A short label, as for models_run." },
                        "timeout_min": { "type": "integer", "description": "1 to 14; 10 by default." },
                        "notify": { "type": "string", "description": "An https URL to POST to when the job ends." },
                        "request": { "type": "object", "additionalProperties": true, "description": "Instead of prompt or messages: a whole Messages request (system blocks, tools, tool_choice, thinking, cache_control and the like), as an AI agent's turn sends it, for any provider: an Anthropic Messages body, translated for OpenAI. The job streams it; models_result answers its content blocks and usage." },
                        "betas": { "type": "array", "items": { "type": "string", "enum": turn::BETAS }, "description": "With `request`: the provider betas it uses." },
                        "lane": { "type": "string", "enum": ["default", "ai-agents"], "description": "`ai-agents` for an AI agent's turn, which runs beside other jobs with its own cap; `default` otherwise." }
                    }
                }),
            },
            Tool {
                name: "models_status",
                description: "A model job's state (queued, running, succeeded, failed, \
                              timed_out, lost, cancelled), its times, the characters streamed so far and, \
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
                description: "A finished model job's answer: text, content blocks (tool calls \
                              and thinking included), stop reason, tokens, usage with cache \
                              reads and writes, and any error (a timed-out job keeps its text so \
                              far). Kept for 10 minutes once read, and 24 hours from submission \
                              at most.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                }),
            },
            Tool {
                name: "models_cancel",
                description: "Cancel a model job that has not ended: a queued one never starts, \
                              a running one stops at its next chunk. Only its sender, or the \
                              person they act for, can cancel it.",
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
                name: "models_cost",
                description: "The AI gateway's exact cost of single requests, by log id: the \
                              `log_id` that `models_result` and `models_status` answer for a \
                              job. Each answer has `found`, `cost_usd` (the gateway's, which \
                              includes prompt-cache reads and writes), `model`, `tokens_in` and \
                              `tokens_out`. A log not written yet is `found: false`; ask again \
                              in a few seconds.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "log_ids": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": ai_gateway::MAX_LOG_IDS,
                            "items": { "type": "string", "minLength": 1, "maxLength": 100, "pattern": "^[A-Za-z0-9_-]+$" },
                            "description": "1 to 50 log ids."
                        }
                    },
                    "required": ["log_ids"]
                }),
            },
            Tool {
                name: "models_daily",
                description: "Read-only. The AI gateway's cost per UTC day over the last `days` \
                              days, by model family and by AI agent run, from its logs.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "days": { "type": "integer", "description": "1 to 31; 7 by default." }
                    }
                }),
            },
            Tool {
                name: "models_billing",
                description: "Read-only. A provider's own billed usage per UTC day over the last \
                              `days` days, from its billing API (Anthropic's Admin API usage and \
                              cost reports): cost and tokens per day and per model. Days with no \
                              data are left out. Needs the ANTHROPIC_ADMIN_KEY secret.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": slugs, "description": "The first provider with billing when omitted." },
                        "days": { "type": "integer", "description": "1 to 31; 7 by default." }
                    }
                }),
            },
            Tool {
                name: "models_usage",
                description: "What the AI gateway's requests used over a window, from its \
                              logs: requests, failures, tokens in and out, and cost, in all and \
                              in groups: by provider and model unless `by` names others. The \
                              logs lack prompt-cache token counts, so those come from the model \
                              gateway's own jobs (not `models_run`): `cache_read_tokens` and \
                              `cache_write_tokens`, for information. `cost` is the gateway's \
                              and already includes prompt-cache reads and writes; \
                              `cost_with_cache` is a deprecated alias of `cost`, kept one \
                              release. \
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
                        "task_prefix": { "type": "string", "description": "Only requests whose task starts with this, such as `ai-agent/` for every AI agent run's turns." },
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
        let mut out: Vec<SlotSpec> = self
            .slots()
            .iter()
            .map(|p| SlotSpec {
                name: self.secret(p),
                purpose: format!(
                    "The {} key the AI gateway `{}` uses for alias `{}`. {}",
                    p.slug, p.gateway, p.alias, p.help
                ),
                scopes: vec![SECRET_SCOPE.to_string()],
            })
            .collect();
        if self
            .providers
            .iter()
            .any(|p| p.billing == Some(Billing::AnthropicAdmin))
        {
            out.push(SlotSpec {
                name: billing::ADMIN_KEY.into(),
                purpose: "An Anthropic Admin API key (sk-ant-admin…, made by an organization \
                          admin in the Claude Console), used to read Anthropic's usage and cost \
                          reports."
                    .into(),
                scopes: vec![secrets_store::WORKERS.to_string()],
            });
        }
        out
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
            footprint: json!({
                "gateways": self.gateways.iter().map(|g| g.id.clone()).collect::<Vec<_>>(),
                "externals": self.gateways.iter().map(|g| json!({ "name": "AI Gateway", "kind": "model gateway", "detail": format!("gateway {}", g.id) })).collect::<Vec<_>>(),
            }),
            secrets: self.secrets(),
            wants: Vec::new(),
            versions: versions(),
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
            "models_providers" => self.providers_view(account()?).await?,
            "models_run" => self.run(env, account()?, caller, args).await?,
            "models_usage" => self.usage(env, account()?, args).await?,
            "models_cost" => self.cost(account()?, args).await?,
            "models_daily" => self.daily_by_run(account()?, args).await?,
            "models_billing" => self.billing(env, args).await?,
            "models_logs" => self.recent(account()?, args).await?,
            "models_submit" => self.submit(env, caller, args).await?,
            "models_status" => self.job(env, caller, "status", args).await?,
            "models_result" => self.job(env, caller, "result", args).await?,
            "models_cancel" => self.job(env, caller, "cancel", args).await?,
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
        let gateways = self
            .gateway_logs(account, since, &Filter::default(), CHART_LOGS)
            .await?;
        let truncated = gateways.iter().any(|g| g.truncated);
        let logs: Vec<ai_gateway::Log> = gateways.into_iter().flat_map(|g| g.logs).collect();
        let mut answer = to_value(daily::daily(&logs, &today, days));
        answer["truncated"] = json!(truncated);
        Ok(answer)
    }

    /// The key slot for secret `name`.
    fn keyed(&self, name: &str) -> Result<Slot> {
        self.slots()
            .into_iter()
            .find(|s| self.secret(s) == name)
            .ok_or_else(|| bad("no provider uses this secret"))
    }

    /// Checks `value` as secret `name` with its provider, before it is written.
    pub async fn check_secret(&self, name: &str, value: &str) -> Result<String> {
        if name == billing::ADMIN_KEY {
            if !value.starts_with("sk-ant-admin") {
                return Err(bad("an Anthropic Admin API key starts with `sk-ant-admin`"));
            }
            return match admin_probe(value).await? {
                200 => Ok("Anthropic accepts the admin key".into()),
                s => Err(bad(&format!("Anthropic refused the admin key: {s}"))),
            };
        }
        self.keyed(name)?.dialect.check(value).await
    }

    /// A key's expiry. A provider's API key: Anthropic's API, like the others', answers no
    /// expiry, and the key is readable only by the AI gateway, so there is nothing to ask. The
    /// admin key is read through the binding and tried on a one-row report.
    pub async fn secret_expiry(&self, env: &Env, name: &str) -> Expiry {
        if name != billing::ADMIN_KEY {
            return Expiry::Unknown {
                why: "the provider's API reports no key expiry".into(),
            };
        }
        let key = match admin_key(env).await {
            Ok(k) => k,
            Err(e) => {
                return Expiry::Unknown {
                    why: format!("not bound yet: {e}"),
                };
            }
        };
        match admin_probe(&key).await {
            Ok(401 | 403) => Expiry::Refused {
                why: "Anthropic refuses the admin key (401/403): it has expired or been revoked"
                    .into(),
            },
            Ok(200) => Expiry::Unknown {
                why: "Anthropic reports no expiry for admin keys; the key works".into(),
            },
            Ok(s) => Expiry::Unknown {
                why: format!("the check failed: Anthropic answered {s}"),
            },
            Err(e) => Expiry::Unknown {
                why: format!("the check failed: {e}"),
            },
        }
    }

    /// A key just set as secret `name` (Secrets Store id `id`): the gateway is set up and given
    /// it.
    pub async fn secret_set(&self, account: &Account, name: &str, id: &str) -> Result<String> {
        if name == billing::ADMIN_KEY {
            return Ok(ADMIN_SET_NOTE.into());
        }
        let p = self.keyed(name)?;
        let kind = self.kind_of(p.gateway);
        if kind != GatewayKind::Byok {
            return Err(bad(&format!(
                "gateway `{}` is not a BYOK gateway: keys are never handed to it",
                p.gateway
            )));
        }
        ai_gateway::ensure(account, p.gateway, kind).await?;
        let added = hand_key(&self.gateways, account, p.gateway, p.slug, p.alias, id).await?;
        Ok(if added {
            format!("the gateway `{}` now uses it for {}", p.gateway, p.slug)
        } else {
            format!("the gateway `{}` already used it for {}", p.gateway, p.slug)
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
    credential_as(env, &caller.caller, purposes).await
}

/// An account acting with a token core minted for `purposes`, asked as `caller` (a caller as
/// core serializes it: a person, or this Worker as a service when no one is calling, such as a
/// job object's alarm).
pub async fn credential_as(env: &Env, caller: &Value, purposes: &[&str]) -> Result<Account> {
    let given = credential_given(env, caller, purposes).await?;
    Ok(Account {
        id: given.account_id,
        parent: String::new(),
        secrets_store: env.var("CF_SECRETS_STORE_ID")?.to_string(),
        bearer: Some(given.token),
    })
}

/// Asks core for a `scoped` credential for `purposes`, as `caller`: the token, its account and
/// its id.
async fn credential_given(env: &Env, caller: &Value, purposes: &[&str]) -> Result<Credential> {
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
        Some((key.as_bytes(), caller)),
        &nonce,
    )
    .await?;
    match answer {
        Answer::Ok(value) => serde_json::from_value(value)
            .map_err(|e| bad(&format!("core's credential does not parse: {e}"))),
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
            SecretOp::Expiry => {
                Answer::Ok(to_value(self.models.secret_expiry(env, &req.name).await))
            }
            SecretOp::Set if req.name == billing::ADMIN_KEY => {
                Answer::Ok(Value::String(ADMIN_SET_NOTE.into()))
            }
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

/// A whole (not streamed) Responses API answer in the common shape. Its `output_tokens` already
/// include the reasoning tokens.
fn responses_reply(v: &Value) -> Reply {
    let items = v["output"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = items
        .iter()
        .filter(|i| i["type"] == "message")
        .flat_map(|i| i["content"].as_array().into_iter().flatten())
        .filter(|c| c["type"] == "output_text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let calls = items.iter().any(|i| i["type"] == "function_call");
    Reply {
        model: v["model"].as_str().unwrap_or_default().into(),
        text,
        stop_reason: stream::responses_stop(v, calls),
        tokens_in: v["usage"]["input_tokens"].as_u64(),
        tokens_out: v["usage"]["output_tokens"].as_u64(),
    }
}

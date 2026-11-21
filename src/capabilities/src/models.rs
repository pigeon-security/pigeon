//! Models: requests to model providers through one Cloudflare AI Gateway, and what they used.
//! Each provider is declared once, with its key's alias and the dialect it speaks; its key is a
//! secret set on the secrets page, which the gateway reads from the Secrets Store, so no key
//! ever reaches a caller or a chat. A request takes one common shape whatever the provider, and
//! the gateway's logs answer how many requests, tokens and how much cost each model has used:
//! the measure a router between providers, or between here and a provider, will decide by.

use pigeon_core::Account;
use pigeon_core::ai_gateway::{self, SECRET_SCOPE};
use pigeon_core::mcp::Tool;
use pigeon_core::secrets_store::Writer;
use serde::Serialize;
use serde_json::{Value, json};
use worker::js_sys::Date;
use worker::{Error, Fetch, Headers, Method, Request, RequestInit, Result};

use crate::{Call, Capability, Context, Footprint, Slot, to_value, unknown};

/// Model providers behind one gateway.
pub struct Models {
    /// The gateway's ID, such as `pigeon`.
    pub gateway: String,
    pub providers: Vec<Provider>,
    /// Where a person sets a key, such as `https://platform.pigeon.dev/secrets`.
    pub secrets_page: String,
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
#[derive(Clone, Copy, Debug, PartialEq)]
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
        let ran = ai_gateway::run(
            cx.account,
            &self.gateway,
            p.slug,
            p.dialect.path(),
            p.dialect.headers(),
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

    async fn usage(&self, account: &Account, args: &Value) -> Result<Value> {
        let hours = args["hours"]
            .as_f64()
            .unwrap_or(24.0)
            .clamp(0.1, 24.0 * 31.0);
        let since = Date::now() - hours * 3_600_000.0;
        let logs = ai_gateway::logs(
            account,
            &self.gateway,
            since,
            args["provider"].as_str(),
            2_000,
        )
        .await?;
        let by = ai_gateway::summarize(&logs);
        let cost: f64 = by.iter().filter_map(|u| u.cost).sum();
        Ok(json!({
            "since": String::from(Date::new(&since.into()).to_iso_string()),
            "hours": hours,
            "requests": logs.len(),
            "truncated": logs.len() >= 2_000,
            "tokens_in": by.iter().map(|u| u.tokens_in).sum::<u64>(),
            "tokens_out": by.iter().map(|u| u.tokens_out).sum::<u64>(),
            "cost": cost,
            "by_model": by,
        }))
    }
}

impl Capability for Models {
    fn name(&self) -> &'static str {
        "models"
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
                              provider's own shape instead and answers its own.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string", "enum": slugs, "description": "The first declared when omitted." },
                        "model": { "type": "string", "description": "The provider's default when omitted." },
                        "system": { "type": "string" },
                        "prompt": { "type": "string" },
                        "messages": { "type": "array", "items": { "type": "object" } },
                        "max_tokens": { "type": "integer", "description": "1024 by default." },
                        "raw": { "type": "object" }
                    }
                }),
            },
            Tool {
                name: "models_usage",
                description: "What the AI gateway's requests used over a window, from its \
                              logs: requests, failures, tokens in and out, and cost, in all and \
                              by provider and model.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "hours": { "type": "number", "description": "24 by default, 744 at most." },
                        "provider": { "type": "string", "enum": slugs }
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
            ..Footprint::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Provider {
        Provider {
            slug: "anthropic",
            alias: "default",
            dialect: Dialect::Anthropic,
            default_model: "claude-haiku-5-5",
            key_help: "",
        }
    }

    #[test]
    fn a_prompt_becomes_one_user_message_on_the_default_model() {
        let a = ask(&json!({ "prompt": "hi" }), &claude()).unwrap();
        assert_eq!(a.model, "claude-haiku-5-5");
        assert_eq!(a.messages, json!([{ "role": "user", "content": "hi" }]));
        assert_eq!(a.max_tokens, 1024);
        assert!(ask(&json!({}), &claude()).is_err());
    }

    #[test]
    fn the_anthropic_body_carries_system_only_when_given() {
        let mut a = ask(&json!({ "prompt": "hi", "max_tokens": 5 }), &claude()).unwrap();
        let b = Dialect::Anthropic.body(&a);
        assert_eq!(b["max_tokens"], 5);
        assert!(b.get("system").is_none());
        a.system = Some("be brief".into());
        assert_eq!(Dialect::Anthropic.body(&a)["system"], "be brief");
    }

    #[test]
    fn an_anthropic_answer_reads_as_text_and_tokens() {
        let v = json!({
            "model": "claude-haiku-5-5",
            "content": [{ "type": "text", "text": "Hel" }, { "type": "text", "text": "lo" }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 8, "output_tokens": 2 }
        });
        let r = Dialect::Anthropic.reply(&v).unwrap();
        assert_eq!(r.text, "Hello");
        assert_eq!((r.tokens_in, r.tokens_out), (Some(8), Some(2)));
        assert!(Dialect::Anthropic.reply(&json!({ "error": {} })).is_err());
    }
}

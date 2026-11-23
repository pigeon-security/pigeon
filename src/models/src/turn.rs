//! An AI agent's turn: a whole Messages request sent as a job, checked here before it is queued.
//!
//! Nothing in this file does I/O. It only reads JSON and says yes or no.

use serde_json::{Value, json};

use super::Dialect;

/// The betas a turn may ask for, sent as `anthropic-beta`.
pub const BETAS: [&str; 3] = [
    "compact-2026-01-12",
    "context-management-2025-06-27",
    "task-budgets-2026-03-13",
];

/// The top-level keys a turn's request may carry.
pub const KEYS: [&str; 15] = [
    "model",
    "max_tokens",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
    "context_management",
    "metadata",
    "temperature",
    "top_p",
    "top_k",
    "stop_sequences",
    "cache_control",
];

/// A turn's most output tokens.
pub const MAX_TOKENS: u64 = 128_000;
/// A turn's output tokens unless asked.
pub const DEFAULT_MAX_TOKENS: u64 = 8_192;
/// The largest request a turn may send, serialized.
pub const MAX_BYTES: usize = 4 << 20;

/// The most tools a turn may carry.
const MAX_TOOLS: usize = 128;

/// A checked turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// The body to send, with `stream: true`.
    pub body: Value,
    /// The model it names.
    pub model: String,
    /// The `anthropic-beta` header's value, when betas were asked.
    pub beta: Option<String>,
}

/// Checks `request` (a Messages body) and `betas` (null, or an array of names), naming
/// `default_model` when the request names none. The model is checked for the provider's
/// `dialect`. The body stays a Messages body: translating it for OpenAI is the caller's step.
pub fn turn(
    request: &Value,
    betas: &Value,
    default_model: &str,
    dialect: Dialect,
) -> Result<Turn, String> {
    let map = request
        .as_object()
        .ok_or_else(|| "`request` must be a Messages request: an object".to_string())?;

    check_keys(map.contains_key("stream"), map.keys().map(String::as_str))?;
    check_messages(map.get("messages"))?;
    let model = resolve_model(map.get("model"), default_model, dialect)?;
    let max_tokens = resolve_max_tokens(map.get("max_tokens"))?;
    check_system(map.get("system"))?;
    check_tools(map.get("tools"))?;
    let beta = resolve_betas(betas, dialect)?;

    let mut body = map.clone();
    body.insert("model".to_string(), json!(model));
    body.insert("max_tokens".to_string(), json!(max_tokens));
    body.insert("stream".to_string(), json!(true));
    let body = Value::Object(body);

    let text = serde_json::to_string(&body)
        .map_err(|e| format!("the request could not be serialized, so it was refused: {e}"))?;
    if text.len() > MAX_BYTES {
        return Err(format!(
            "the request is {} bytes; a turn sends at most 4 MiB",
            text.len()
        ));
    }

    Ok(Turn { body, model, beta })
}

/// Refuses a `stream` key, then refuses any key a turn may not carry.
fn check_keys<'a>(has_stream: bool, keys: impl Iterator<Item = &'a str>) -> Result<(), String> {
    if has_stream {
        return Err("`request` may not set `stream`: a job always streams".to_string());
    }
    for key in keys {
        if !KEYS.contains(&key) {
            return Err(format!(
                "`request` may not carry `{}`; it may carry: {}",
                key,
                KEYS.join(", ")
            ));
        }
    }
    Ok(())
}

/// Checks that `messages` is a non-empty array of well-formed messages.
fn check_messages(messages: Option<&Value>) -> Result<(), String> {
    let list = match messages {
        Some(Value::Array(list)) => list,
        _ => return Err("`messages` must be a non-empty array of messages".to_string()),
    };
    if list.is_empty() {
        return Err("`messages` must not be empty: a turn needs at least one message".to_string());
    }
    for (i, item) in list.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("`messages[{i}]` must be an object"))?;
        match obj.get("role").and_then(Value::as_str) {
            Some("user") | Some("assistant") => {}
            _ => {
                return Err(format!(
                    "`messages[{i}].role` must be \"user\" or \"assistant\""
                ));
            }
        }
        match obj.get("content") {
            Some(Value::String(_)) | Some(Value::Array(_)) => {}
            _ => {
                return Err(format!(
                    "`messages[{i}].content` must be a string or an array"
                ));
            }
        }
    }
    Ok(())
}

/// Picks the model: the one asked for, or the default. It must suit the provider.
fn resolve_model(
    model: Option<&Value>,
    default_model: &str,
    dialect: Dialect,
) -> Result<String, String> {
    let name = match model {
        None => default_model,
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        Some(_) => return Err("`model` must be a non-empty string".to_string()),
    };
    match dialect {
        Dialect::Anthropic => {
            if !name.starts_with("claude-") {
                return Err(
                    "`model` must name a Claude model, such as claude-sonnet-5-5".to_string(),
                );
            }
        }
        Dialect::OpenAi => {
            let plain = !name.is_empty()
                && name.len() <= 100
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
            if !plain {
                return Err(
                    "`model` must be a plain model id: letters, digits, `.`, `-`, `_` \
                            and `:`, 100 characters at most"
                        .to_string(),
                );
            }
        }
    }
    Ok(name.to_string())
}

/// Picks the output token limit: the one asked for, or the default.
fn resolve_max_tokens(value: Option<&Value>) -> Result<u64, String> {
    match value {
        None => Ok(DEFAULT_MAX_TOKENS),
        Some(v) => match v.as_u64() {
            Some(n) if (1..=MAX_TOKENS).contains(&n) => Ok(n),
            _ => Err(format!(
                "`max_tokens` must be an integer from 1 to {MAX_TOKENS}"
            )),
        },
    }
}

/// Checks `system`: a string, or an array of text blocks.
fn check_system(system: Option<&Value>) -> Result<(), String> {
    match system {
        None | Some(Value::String(_)) => Ok(()),
        Some(Value::Array(blocks)) => {
            for (i, block) in blocks.iter().enumerate() {
                let is_text = block
                    .as_object()
                    .and_then(|o| o.get("type"))
                    .and_then(Value::as_str)
                    == Some("text");
                if !is_text {
                    return Err(format!(
                        "`system[{i}]` must be an object with `type` \"text\""
                    ));
                }
            }
            Ok(())
        }
        Some(_) => Err("`system` must be a string or an array of text blocks".to_string()),
    }
}

/// Checks `tools`: a short array of objects that each have a string `name` or `type`.
fn check_tools(tools: Option<&Value>) -> Result<(), String> {
    let list = match tools {
        None => return Ok(()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("`tools` must be an array of tools".to_string()),
    };
    if list.len() > MAX_TOOLS {
        return Err(format!(
            "`tools` has {} tools; a turn carries at most {MAX_TOOLS}",
            list.len()
        ));
    }
    for (i, tool) in list.iter().enumerate() {
        let obj = tool
            .as_object()
            .ok_or_else(|| format!("`tools[{i}]` must be an object"))?;
        let named = obj.get("name").is_some_and(Value::is_string);
        let typed = obj.get("type").is_some_and(Value::is_string);
        if !named && !typed {
            return Err(format!(
                "`tools[{i}]` must have a string `name`, or a string `type` for a server tool"
            ));
        }
    }
    Ok(())
}

/// Turns the asked-for betas into a header value, or `None` when there are none.
fn resolve_betas(betas: &Value, dialect: Dialect) -> Result<Option<String>, String> {
    if dialect == Dialect::OpenAi
        && betas.as_array().is_none_or(|l| !l.is_empty())
        && !betas.is_null()
    {
        return Err("betas are Anthropic's: OpenAI takes none, so `betas` must be omitted".into());
    }
    let list = match betas {
        Value::Null => return Ok(None),
        Value::Array(list) => list,
        _ => return Err("`betas` must be null or an array of beta names".to_string()),
    };
    let mut kept: Vec<&str> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let name = item
            .as_str()
            .ok_or_else(|| format!("`betas[{i}]` must be a string"))?;
        if !BETAS.contains(&name) {
            return Err(format!(
                "beta `{}` is not one a turn may ask for; these are: {}",
                name,
                BETAS.join(", ")
            ));
        }
        if !kept.contains(&name) {
            kept.push(name);
        }
    }
    if kept.is_empty() {
        Ok(None)
    } else {
        Ok(Some(kept.join(",")))
    }
}

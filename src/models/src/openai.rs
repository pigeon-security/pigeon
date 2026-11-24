//! An Anthropic Messages request, as an AI agent's turn sends it, translated to a streamed
//! OpenAI Chat Completions request. Agent transcripts stay in the Messages shape; this is the
//! edge where they meet OpenAI. Pure: JSON in, JSON out.

use serde_json::{Map, Value, json};

/// The note that stands where an image was dropped.
const IMAGE_NOTE: &str = "[image omitted]";

/// The most stop sequences Chat Completions takes.
const MAX_STOP: usize = 4;

/// Translates `body`, a checked Messages request, to a streamed Chat Completions request.
pub fn request(body: &Value) -> Result<Value, String> {
    let map = body
        .as_object()
        .ok_or_else(|| "the request must be an object".to_string())?;
    let mut out = Map::new();
    if let Some(model) = map.get("model") {
        out.insert("model".into(), model.clone());
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = system_text(map.get("system")) {
        messages.push(json!({ "role": "system", "content": system }));
    }
    let list = map
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "`messages` must be an array".to_string())?;
    for message in list {
        match message["role"].as_str() {
            Some("assistant") => messages.push(assistant(&message["content"])),
            Some("user") => user(&message["content"], &mut messages),
            _ => return Err("a message's role must be \"user\" or \"assistant\"".to_string()),
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    let tools = tools(map.get("tools"));
    let has_tools = !tools.is_empty();
    if has_tools {
        out.insert("tools".into(), Value::Array(tools));
        if let Some((choice, parallel)) = tool_choice(map.get("tool_choice")) {
            out.insert("tool_choice".into(), choice);
            if let Some(p) = parallel {
                out.insert("parallel_tool_calls".into(), json!(p));
            }
        }
    }

    if let Some(n) = map.get("max_tokens").filter(|v| v.is_u64()) {
        out.insert("max_completion_tokens".into(), n.clone());
    }
    let model = map.get("model").and_then(Value::as_str).unwrap_or("");
    if has_tools && !effort_with_tools(model) {
        // Chat Completions refuses function tools with a reasoning effort on these models,
        // and asks for `none` instead.
        out.insert("reasoning_effort".into(), json!("none"));
    } else if let Some(effort) = body["output_config"]["effort"].as_str()
        && matches!(effort, "low" | "medium" | "high")
    {
        out.insert("reasoning_effort".into(), json!(effort));
    }
    if let Some(stops) = map.get("stop_sequences").and_then(Value::as_array) {
        let stops: Vec<&str> = stops
            .iter()
            .filter_map(Value::as_str)
            .take(MAX_STOP)
            .collect();
        if !stops.is_empty() {
            out.insert("stop".into(), json!(stops));
        }
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = map.get(key).filter(|v| v.is_number()) {
            out.insert(key.into(), v.clone());
        }
    }
    // Dropped: cache_control, thinking, context_management, metadata and top_k.
    out.insert("stream".into(), json!(true));
    out.insert("stream_options".into(), json!({ "include_usage": true }));
    Ok(Value::Object(out))
}

/// The models Chat Completions refuses function tools with a reasoning effort for (OpenAI's
/// 400: "Function tools with reasoning_effort are not supported for gpt-6-luna").
const NO_EFFORT_WITH_TOOLS: [&str; 1] = ["gpt-6-luna"];

/// Whether `model` takes a reasoning effort beside function tools in Chat Completions. A
/// `provider/` prefix and a dated suffix (`gpt-6-luna-2026-…`) are read as the model. Pure.
pub fn effort_with_tools(model: &str) -> bool {
    let model = model.rsplit('/').next().unwrap_or(model);
    !NO_EFFORT_WITH_TOOLS
        .iter()
        .any(|m| model == *m || model.strip_prefix(m).is_some_and(|r| r.starts_with('-')))
}

/// The system prompt as one string: the string itself, or its text blocks joined.
fn system_text(system: Option<&Value>) -> Option<String> {
    let text = match system? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => texts(blocks),
        _ => return None,
    };
    if text.is_empty() { None } else { Some(text) }
}

/// The text of the `text` blocks in `blocks`, joined with a newline.
fn texts(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tools as functions. A server tool (no `input_schema`) is dropped.
fn tools(tools: Option<&Value>) -> Vec<Value> {
    let Some(list) = tools.and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let schema = t.get("input_schema").filter(|s| s.is_object())?;
            let mut function = json!({ "name": name, "parameters": schema });
            if let Some(d) = t["description"].as_str() {
                function["description"] = json!(d);
            }
            Some(json!({ "type": "function", "function": function }))
        })
        .collect()
}

/// `tool_choice` as Chat Completions takes it, with `parallel_tool_calls` when the request
/// turned parallel calls off.
fn tool_choice(choice: Option<&Value>) -> Option<(Value, Option<bool>)> {
    let choice = choice?;
    let parallel = if choice["disable_parallel_tool_use"].as_bool() == Some(true) {
        Some(false)
    } else {
        None
    };
    let value = match choice["type"].as_str()? {
        "auto" => json!("auto"),
        "any" => json!("required"),
        "none" => return Some((json!("none"), None)),
        "tool" => json!({ "type": "function", "function": { "name": choice["name"].as_str()? } }),
        _ => return None,
    };
    Some((value, parallel))
}

/// An assistant message: its text, and its tool uses as `tool_calls`. Thinking is dropped.
fn assistant(content: &Value) -> Value {
    let blocks = match content {
        Value::String(s) => return json!({ "role": "assistant", "content": s }),
        Value::Array(blocks) => blocks,
        _ => return json!({ "role": "assistant", "content": "" }),
    };
    let text = texts(blocks);
    let calls: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            let input = b.get("input").filter(|i| !i.is_null());
            json!({
                "id": b["id"],
                "type": "function",
                "function": {
                    "name": b["name"],
                    "arguments": input.map_or_else(|| "{}".to_string(), Value::to_string),
                },
            })
        })
        .collect();
    let mut m = json!({ "role": "assistant" });
    m["content"] = if text.is_empty() && !calls.is_empty() {
        Value::Null
    } else {
        json!(text)
    };
    if !calls.is_empty() {
        m["tool_calls"] = Value::Array(calls);
    }
    m
}

/// A user message: a `tool` message for each tool result, in order, then one `user` message
/// with the text that is left. Images are dropped and noted.
fn user(content: &Value, out: &mut Vec<Value>) {
    let blocks = match content {
        Value::String(s) => {
            out.push(json!({ "role": "user", "content": s }));
            return;
        }
        Value::Array(blocks) => blocks,
        _ => return,
    };
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        match b["type"].as_str() {
            Some("tool_result") => {
                let mut text = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(inner) => inner
                        .iter()
                        .filter_map(|i| match i["type"].as_str() {
                            Some("text") => i["text"].as_str().map(String::from),
                            Some("image") => Some(IMAGE_NOTE.to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                if b["is_error"].as_bool() == Some(true) {
                    text = format!("Error: {text}");
                }
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": b["tool_use_id"],
                    "content": text,
                }));
            }
            Some("text") => {
                if let Some(t) = b["text"].as_str() {
                    parts.push(t.to_string());
                }
            }
            Some("image") => parts.push(IMAGE_NOTE.to_string()),
            _ => {}
        }
    }
    if !parts.is_empty() {
        out.push(json!({ "role": "user", "content": parts.join("\n") }));
    }
}

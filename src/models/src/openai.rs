//! An Anthropic Messages request, as an AI agent's turn sends it, translated to a streamed
//! OpenAI Chat Completions request, or to a Responses API request when the model's row says so
//! and the turn has tools and asks for reasoning. Agent transcripts stay in the Messages shape;
//! this is the edge where they meet OpenAI. Pure: JSON in, JSON out.

use serde_json::{Map, Value, json};

use crate::capabilities::{self, Capabilities, Choice};

/// The note that stands where an image was dropped.
const IMAGE_NOTE: &str = "[image omitted]";

/// The most stop sequences Chat Completions takes.
const MAX_STOP: usize = 4;

/// The reasoning effort a Messages request asks for, when it is one OpenAI takes.
fn effort_asked(body: &Value) -> Option<&str> {
    body["output_config"]["effort"]
        .as_str()
        .filter(|e| matches!(*e, "low" | "medium" | "high"))
}

/// Whether `body`, a checked Messages request, goes to the Responses API: its model's row says
/// so, it has tools and it asks for an effort. Pure.
pub fn wants_responses(body: &Value) -> bool {
    let caps = capabilities::of(body["model"].as_str().unwrap_or(""));
    caps.uses_responses(
        caps.tools && !tools(body.get("tools")).is_empty(),
        effort_asked(body).is_some(),
    )
}

/// Translates `body`, a checked Messages request, to a streamed request: Responses API when
/// [`wants_responses`], else Chat Completions.
pub fn request(body: &Value) -> Result<Value, String> {
    let map = body
        .as_object()
        .ok_or_else(|| "the request must be an object".to_string())?;
    let mut out = Map::new();
    if let Some(model) = map.get("model") {
        out.insert("model".into(), model.clone());
    }

    if wants_responses(body) {
        return responses(map);
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

    let model = map.get("model").and_then(Value::as_str).unwrap_or("");
    let caps = capabilities::of(model);
    let tools = if caps.tools {
        tools(map.get("tools"))
    } else {
        Vec::new()
    };
    let has_tools = !tools.is_empty();
    if has_tools {
        out.insert("tools".into(), Value::Array(tools));
        if let Some((choice, parallel)) = tool_choice(map.get("tool_choice"), &caps) {
            out.insert("tool_choice".into(), choice);
            if let Some(p) = parallel {
                out.insert("parallel_tool_calls".into(), json!(p));
            }
        }
    }

    if let Some(n) = map.get("max_tokens").and_then(Value::as_u64) {
        out.insert(
            "max_completion_tokens".into(),
            json!(n.min(caps.max_output)),
        );
    }
    if !caps.effort {
        // The model takes no reasoning effort: none is sent.
    } else if has_tools && !caps.effort_with_tools {
        // Chat Completions refuses function tools with a reasoning effort on this model, and
        // asks for `none` instead.
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
/// turned parallel calls off. A kind the model does not take (`caps`) becomes `auto`, and an
/// unsupported `none` is left out.
fn tool_choice(choice: Option<&Value>, caps: &Capabilities) -> Option<(Value, Option<bool>)> {
    let choice = choice?;
    let parallel = if choice["disable_parallel_tool_use"].as_bool() == Some(true) {
        Some(false)
    } else {
        None
    };
    let kind = Choice::named(choice["type"].as_str()?)?;
    let kind = match kind {
        Choice::None if !caps.takes(Choice::None) => return None,
        k if caps.takes(k) => k,
        _ => Choice::Auto,
    };
    let value = match kind {
        Choice::Auto => json!("auto"),
        Choice::Any => json!("required"),
        Choice::None => return Some((json!("none"), None)),
        Choice::Tool => {
            json!({ "type": "function", "function": { "name": choice["name"].as_str()? } })
        }
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
                let text = result_text(b);
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

/// The text of a `tool_result` block: its content as text, with `Error: ` before it when the
/// result is an error. Images are noted.
fn result_text(b: &Value) -> String {
    let text = match &b["content"] {
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
        format!("Error: {text}")
    } else {
        text
    }
}

/// Translates a checked Messages request to a streamed Responses API request (`/v1/responses`).
/// The system prompt is `instructions`; messages become `input` items; a tool use is a
/// `function_call` item and its result a `function_call_output` item, joined by `call_id`.
/// Thinking is dropped (OpenAI's reasoning items cannot be replayed without storing them).
/// `stop_sequences` are dropped: the Responses API has no `stop`.
fn responses(map: &Map<String, Value>) -> Result<Value, String> {
    let model = map.get("model").and_then(Value::as_str).unwrap_or("");
    let caps = capabilities::of(model);
    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    if let Some(system) = system_text(map.get("system")) {
        out.insert("instructions".into(), json!(system));
    }
    let list = map
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "`messages` must be an array".to_string())?;
    let mut input: Vec<Value> = Vec::new();
    for message in list {
        match message["role"].as_str() {
            Some("assistant") => assistant_items(&message["content"], &mut input),
            Some("user") => user_items(&message["content"], &mut input),
            _ => return Err("a message's role must be \"user\" or \"assistant\"".to_string()),
        }
    }
    out.insert("input".into(), Value::Array(input));

    let flat: Vec<Value> = tools(map.get("tools"))
        .into_iter()
        .map(|t| {
            let f = &t["function"];
            let mut tool = json!({
                "type": "function",
                "name": f["name"],
                "parameters": f["parameters"],
                // The Responses API makes functions strict unless told not to, and strict needs
                // closed schemas that an agent's tools do not promise.
                "strict": false,
            });
            if let Some(d) = f.get("description") {
                tool["description"] = d.clone();
            }
            tool
        })
        .collect();
    out.insert("tools".into(), Value::Array(flat));
    if let Some((choice, parallel)) = tool_choice(map.get("tool_choice"), &caps) {
        // Responses names a forced function without the `function` wrapper.
        let choice = match choice["function"]["name"].as_str() {
            Some(name) => json!({ "type": "function", "name": name }),
            None => choice,
        };
        out.insert("tool_choice".into(), choice);
        if let Some(p) = parallel {
            out.insert("parallel_tool_calls".into(), json!(p));
        }
    }
    if let Some(effort) = effort_asked(&Value::Object(map.clone())) {
        out.insert("reasoning".into(), json!({ "effort": effort }));
    }
    if let Some(n) = map.get("max_tokens").and_then(Value::as_u64) {
        out.insert("max_output_tokens".into(), json!(n.min(caps.max_output)));
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = map.get(key).filter(|v| v.is_number()) {
            out.insert(key.into(), v.clone());
        }
    }
    // Dropped: cache_control, thinking, context_management, metadata, top_k, stop_sequences.
    out.insert("store".into(), json!(false));
    out.insert("stream".into(), json!(true));
    Ok(Value::Object(out))
}

/// An assistant message as Responses input items: its text, then a `function_call` per tool use.
fn assistant_items(content: &Value, out: &mut Vec<Value>) {
    let blocks = match content {
        Value::String(s) => {
            out.push(json!({ "role": "assistant", "content": s }));
            return;
        }
        Value::Array(blocks) => blocks,
        _ => return,
    };
    let text = texts(blocks);
    if !text.is_empty() {
        out.push(json!({ "role": "assistant", "content": text }));
    }
    for b in blocks.iter().filter(|b| b["type"] == "tool_use") {
        let input = b.get("input").filter(|i| !i.is_null());
        out.push(json!({
            "type": "function_call",
            "call_id": b["id"],
            "name": b["name"],
            "arguments": input.map_or_else(|| "{}".to_string(), Value::to_string),
        }));
    }
}

/// A user message as Responses input items: a `function_call_output` per tool result, in order,
/// then one user message with the text that is left. Images are dropped and noted.
fn user_items(content: &Value, out: &mut Vec<Value>) {
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
            Some("tool_result") => out.push(json!({
                "type": "function_call_output",
                "call_id": b["tool_use_id"],
                "output": result_text(b),
            })),
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

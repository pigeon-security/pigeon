//! An Anthropic Messages request, shaped for the model it names by the capability table. Agent
//! transcripts are already Messages; this only removes what the model would answer with a 400.
//! Pure: JSON in, JSON out.

use serde_json::{Value, json};

use crate::capabilities::{self, Choice};

/// Whether `block` is a thinking block, redacted or not.
fn is_thinking(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking") | Some("redacted_thinking")
    )
}

/// Shapes `body`, a checked Messages request, for its model:
///
/// - `tool_choice` of a kind the model does not take: `tool` and `any` become `auto` (with
///   `disable_parallel_tool_use` kept, which `auto` takes), and `none` is left out.
/// - `max_tokens` is cut to the model's largest output.
/// - An assistant message never ends in a thinking block (the provider refuses it): trailing
///   ones are dropped, and a message left empty is dropped with them.
pub fn request(body: &Value) -> Value {
    let mut out = body.clone();
    let Some(map) = out.as_object_mut() else {
        return out;
    };
    let caps = capabilities::of(body["model"].as_str().unwrap_or(""));

    if let Some(kind) = body["tool_choice"]["type"].as_str().and_then(Choice::named)
        && !caps.takes(kind)
    {
        if kind == Choice::None {
            map.remove("tool_choice");
        } else {
            let mut choice = json!({ "type": "auto" });
            if let Some(p) = body["tool_choice"].get("disable_parallel_tool_use") {
                choice["disable_parallel_tool_use"] = p.clone();
            }
            map.insert("tool_choice".into(), choice);
        }
    }

    if let Some(n) = body["max_tokens"].as_u64() {
        map.insert("max_tokens".into(), json!(n.min(caps.max_output)));
    }

    if let Some(messages) = body["messages"].as_array() {
        let kept: Vec<Value> = messages
            .iter()
            .filter_map(without_trailing_thinking)
            .collect();
        map.insert("messages".into(), Value::Array(kept));
    }
    out
}

/// `message` without the thinking blocks that end an assistant message, or `None` when nothing
/// is left of it.
fn without_trailing_thinking(message: &Value) -> Option<Value> {
    let blocks = match (message["role"].as_str(), message["content"].as_array()) {
        (Some("assistant"), Some(blocks)) => blocks,
        _ => return Some(message.clone()),
    };
    let end = blocks
        .iter()
        .rposition(|b| !is_thinking(b))
        .map_or(0, |i| i + 1);
    if end == blocks.len() {
        return Some(message.clone());
    }
    if end == 0 {
        return None;
    }
    let mut copy = message.clone();
    copy["content"] = Value::Array(blocks[..end].to_vec());
    Some(copy)
}

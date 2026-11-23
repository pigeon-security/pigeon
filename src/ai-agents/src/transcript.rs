//! Builds the next model turn for an AI agent run and handles tool results and compaction.
//!
//! Everything here is pure. It only turns stored messages into request bodies and back. It
//! never panics and never touches the network.

use crate::Role;
use serde_json::{Map, Value, json};

/// What stays fixed for a whole run: the role, model, system prompt and tools.
pub struct Spec<'a> {
    /// The role this run plays.
    pub role: Role,
    /// The model name to ask.
    pub model: &'a str,
    /// The system prompt text.
    pub system: &'a str,
    /// The tool definitions offered to the model.
    pub tools: &'a [Value],
}

/// The most output tokens a turn may use for this role.
pub fn max_tokens(role: Role) -> u64 {
    match role {
        Role::Planner => 32_000,
        Role::Executor => 16_000,
        Role::Mechanical => 8_192,
    }
}

/// The thinking effort for this role, if it has one.
pub fn effort(role: Role) -> Option<&'static str> {
    match role {
        Role::Planner => Some("high"),
        Role::Executor => Some("medium"),
        Role::Mechanical => None,
    }
}

/// Tells whether the model supports adaptive thinking.
pub fn thinks(model: &str) -> bool {
    model.starts_with("claude-opus-")
        || model.starts_with("claude-sonnet-")
        || model.starts_with("claude-fable-")
}

/// Builds the Messages API request body for the next turn.
///
/// The system prompt and the last tool carry a cache marker. Together they form one cached
/// prefix. A top-level marker caches the growing conversation.
pub fn request(spec: &Spec, messages: &[Value]) -> Value {
    let cc = match spec.role {
        Role::Planner => json!({"type": "ephemeral", "ttl": "1h"}),
        _ => json!({"type": "ephemeral"}),
    };

    let mut body = Map::new();
    body.insert("model".to_string(), json!(spec.model));
    body.insert("max_tokens".to_string(), json!(max_tokens(spec.role)));
    body.insert("messages".to_string(), Value::Array(messages.to_vec()));
    body.insert(
        "system".to_string(),
        json!([{"type": "text", "text": spec.system, "cache_control": cc.clone()}]),
    );

    if !spec.tools.is_empty() {
        let mut tools: Vec<Value> = spec.tools.to_vec();
        if let Some(obj) = tools.last_mut().and_then(Value::as_object_mut) {
            obj.insert("cache_control".to_string(), cc);
        }
        body.insert("tools".to_string(), Value::Array(tools));
    }

    body.insert("cache_control".to_string(), json!({"type": "ephemeral"}));

    if thinks(spec.model) {
        body.insert("thinking".to_string(), json!({"type": "adaptive"}));
        if let Some(e) = effort(spec.role) {
            body.insert("output_config".to_string(), json!({"effort": e}));
        }
    }

    Value::Object(body)
}

/// The most output tokens the last turn may use.
pub const LAST_TURN_MAX_TOKENS: u64 = 4096;

/// Builds the request for a run's last turn: only the `report` tool, with `tool_choice` auto
/// (some models refuse a forced choice; the prompt tells the model to report), no thinking and
/// the messages' thinking blocks stripped (their signatures are bound to the full tool list).
/// A reply without a report ends the run over budget. The tool carries no cache marker: tools
/// come before the system prompt, and a 5-minute marker before a planner's 1-hour one is refused.
pub fn last_turn_request(spec: &Spec, messages: &[Value]) -> Value {
    let mut body = request(spec, &strip_thinking(messages));
    if let Some(obj) = body.as_object_mut() {
        let tools: Vec<Value> = spec
            .tools
            .iter()
            .filter(|t| t.get("name").and_then(Value::as_str) == Some("report"))
            .cloned()
            .collect();
        obj.insert("tools".to_string(), Value::Array(tools));
        obj.insert("tool_choice".to_string(), json!({"type": "auto"}));
        obj.insert("max_tokens".to_string(), json!(LAST_TURN_MAX_TOKENS));
        obj.remove("thinking");
        obj.remove("output_config");
    }
    body
}

/// The longest tool result, in bytes, that is passed on without cutting.
pub const MAX_RESULT_BYTES: usize = 16 * 1024;

/// How many bytes of the start of a long result are kept.
const HEAD_BYTES: usize = 6 * 1024;

/// How many bytes of the end of a long result are kept.
const TAIL_BYTES: usize = 8 * 1024;

/// Finds the nearest char boundary at or before `i`.
fn floor_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Finds the nearest char boundary at or after `i`.
fn ceil_boundary(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Cuts text to at most `max` bytes at a char boundary, adding an ellipsis if cut.
fn cut_to(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let end = floor_boundary(text, max);
    let head = text.get(..end).unwrap_or("");
    format!("{head}…")
}

/// Shortens a long tool result by dropping its middle.
///
/// Short text is returned as is. Long text keeps its first 6 KiB and last 8 KiB, with a line in
/// between that says how many bytes were cut. If `saved_at` is given, that line also names the
/// file with the full output.
pub fn cut(text: &str, saved_at: Option<&str>) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text.to_string();
    }
    let head_end = floor_boundary(text, HEAD_BYTES);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(TAIL_BYTES));
    let head = text.get(..head_end).unwrap_or("");
    let tail = text.get(tail_start..).unwrap_or("");
    let removed = tail_start.saturating_sub(head_end);
    let saved = match saved_at {
        Some(path) => format!("; the whole output is in `{path}`"),
        None => String::new(),
    };
    format!("{head}\n[… {removed} bytes cut{saved} …]\n{tail}")
}

/// Lists every `tool_use` block of an assistant message as (id, name, input).
///
/// Blocks without an id or a name are skipped. A missing input becomes `{}`.
pub fn tool_uses(content: &Value) -> Vec<(String, String, Value)> {
    let mut out = Vec::new();
    let Some(blocks) = content.as_array() else {
        return out;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            continue;
        };
        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
        out.push((id.to_string(), name.to_string(), input));
    }
    out
}

/// Builds one user message holding a `tool_result` block per result.
///
/// Each result is (tool_use_id, text, is_error). The `is_error` flag is only written when it is
/// true.
pub fn results_message(results: &[(String, String, bool)]) -> Value {
    let blocks: Vec<Value> = results
        .iter()
        .map(|(id, text, is_error)| {
            let mut block = json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": text,
            });
            if *is_error && let Some(obj) = block.as_object_mut() {
                obj.insert("is_error".to_string(), Value::Bool(true));
            }
            block
        })
        .collect();
    json!({"role": "user", "content": blocks})
}

/// Reads one token counter from a usage object. A missing one counts as zero.
fn counter(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// The context size of the last turn: fresh, cache-read and cache-written input tokens.
pub fn input_tokens(usage: &Value) -> u64 {
    counter(usage, "input_tokens")
        .saturating_add(counter(usage, "cache_read_input_tokens"))
        .saturating_add(counter(usage, "cache_creation_input_tokens"))
}

/// The context size at which a run should be compacted.
pub const COMPACT_AT: u64 = 150_000;

/// Tells whether the last turn's context is big enough to compact.
pub fn needs_compaction(usage: &Value) -> bool {
    input_tokens(usage) >= COMPACT_AT
}

/// Tells whether a message is a user message that starts a clean cut.
///
/// Its content must be a string, or an array with no `tool_result` block.
fn starts_cleanly(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    match message.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(blocks)) => !blocks
            .iter()
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

/// Finds where to cut the history so that `messages[cut..]` is the part to keep.
///
/// It aims to keep about the last `keep_last` messages. It moves forward to the first clean user
/// message, so no tool result loses its tool use. Returns None if there is no good cut or fewer
/// than 2 messages would stay.
pub fn cut_point(messages: &[Value], keep_last: usize) -> Option<usize> {
    let len = messages.len();
    if len <= keep_last.saturating_add(1) {
        return None;
    }
    let start = len.saturating_sub(keep_last);
    let (index, _) = messages
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, m)| starts_cleanly(m))?;
    if len.saturating_sub(index) < 2 {
        return None;
    }
    Some(index)
}

/// The system prompt used when summarising older messages.
const SUMMARY_SYSTEM: &str = "You summarise an AI agent's earlier work for the AI agent itself. Keep every fact it needs to go on: the task, decisions made and why, files read and changed, commands run and their results, errors met, what remains. Drop pleasantries and repeated output. Write plain prose under 1500 words.";

/// The most bytes of one tool input or result shown in a summary request.
const RENDER_LIMIT: usize = 2000;

/// Turns the content of a tool result into plain text.
///
/// It accepts a string or an array of text blocks.
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let parts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            parts.join("\n")
        }
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Renders one content block as a line of text, or None if it is skipped.
fn render_block(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        Some("tool_use") => {
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
            let input = cut_to(&input.to_string(), RENDER_LIMIT);
            Some(format!("[tool {name}: {input}]"))
        }
        Some("tool_result") => {
            let text = result_text(block.get("content"));
            Some(format!("[result: {}]", cut_to(&text, RENDER_LIMIT)))
        }
        _ => None,
    }
}

/// Renders older messages as plain text for the summariser.
fn render_messages(older: &[Value]) -> String {
    let mut out = String::new();
    for message in older {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        out.push_str(&format!("## {role}\n"));
        match message.get("content") {
            Some(Value::String(s)) => {
                out.push_str(s);
                out.push('\n');
            }
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if let Some(line) = render_block(block) {
                        out.push_str(&line);
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
        out.push('\n');
    }
    out
}

/// Builds a request that asks `model` to summarise the `older` messages.
///
/// The request has no thinking and no tools, and allows 4096 output tokens.
pub fn summary_request(model: &str, older: &[Value]) -> Value {
    json!({
        "model": model,
        "max_tokens": 4096,
        "system": SUMMARY_SYSTEM,
        "messages": [
            {"role": "user", "content": render_messages(older)}
        ],
    })
}

/// Builds the new message list after compaction: the summary, then `kept`.
///
/// If `kept` starts with a user message, the summary is merged into it so that roles still
/// alternate.
pub fn compacted(summary: &str, kept: &[Value]) -> Vec<Value> {
    let text = format!("Summary of your earlier work in this run:\n\n{summary}");

    if let Some(first) = kept.first()
        && first.get("role").and_then(Value::as_str) == Some("user")
    {
        let mut blocks = vec![json!({"type": "text", "text": text})];
        match first.get("content") {
            Some(Value::String(s)) => blocks.push(json!({"type": "text", "text": s})),
            Some(Value::Array(items)) => blocks.extend(items.iter().cloned()),
            _ => {}
        }
        let mut out = vec![json!({"role": "user", "content": blocks})];
        out.extend(kept.iter().skip(1).cloned());
        return out;
    }

    let mut out = vec![json!({"role": "user", "content": text})];
    out.extend(kept.iter().cloned());
    out
}

/// Copies the messages with thinking blocks removed from assistant messages.
///
/// This is needed when a turn falls back to another model. An assistant message left empty
/// gets a short placeholder text block.
pub fn strip_thinking(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut copy = message.clone();
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                return copy;
            }
            let Some(blocks) = message.get("content").and_then(Value::as_array) else {
                return copy;
            };
            let mut kept: Vec<Value> = blocks
                .iter()
                .filter(|b| {
                    !matches!(
                        b.get("type").and_then(Value::as_str),
                        Some("thinking") | Some("redacted_thinking")
                    )
                })
                .cloned()
                .collect();
            if kept.is_empty() {
                kept.push(json!({"type": "text", "text": "(thinking removed)"}));
            }
            if let Some(obj) = copy.as_object_mut() {
                obj.insert("content".to_string(), Value::Array(kept));
            }
            copy
        })
        .collect()
}

/// Removes the thinking blocks that end an assistant message's content.
///
/// The provider refuses a message whose final block is thinking (a turn cut off by `max_tokens`
/// during thinking leaves one). Returns `None` when nothing is left, so the caller drops the
/// message. Other messages come back unchanged.
pub fn repair_message(message: &Value) -> Option<Value> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Some(message.clone());
    }
    let Some(blocks) = message.get("content").and_then(Value::as_array) else {
        return Some(message.clone());
    };
    let is_thinking = |b: &Value| {
        matches!(
            b.get("type").and_then(Value::as_str),
            Some("thinking") | Some("redacted_thinking")
        )
    };
    let mut end = blocks.len();
    while end > 0 && is_thinking(&blocks[end - 1]) {
        end -= 1;
    }
    if end == blocks.len() {
        return Some(message.clone());
    }
    if end == 0 {
        return None;
    }
    let mut copy = message.clone();
    if let Some(obj) = copy.as_object_mut() {
        obj.insert("content".to_string(), Value::Array(blocks[..end].to_vec()));
    }
    Some(copy)
}

/// Copies the messages with `repair_message` applied, dropping the ones left empty.
pub fn repair_transcript(messages: &[Value]) -> Vec<Value> {
    messages.iter().filter_map(repair_message).collect()
}

/// The longest note `ai_agents_note` takes, in bytes.
pub const MAX_NOTE_BYTES: usize = 4000;

/// The marker that starts a stored note which is an event from pigeon, not the person's note.
pub const EVENT_MARKER: &str = "\u{1}event\u{1}";

/// How an event from pigeon reads in the run's transcript.
pub fn event_text(text: &str) -> String {
    format!("An event from pigeon (not the person), delivered while you work:\n\n{text}")
}

/// How a stored note reads in the transcript: as an event when it has the marker, else as a
/// note from the person.
pub fn note_text(text: &str) -> String {
    match text.strip_prefix(EVENT_MARKER) {
        Some(event) => event_text(event),
        None => format!(
            "A note from the person, sent while you work (ai_agents_note). Take it into account \
             from now on; it does not answer a pending question:\n\n{text}"
        ),
    }
}

/// Where queued notes go before a turn, given the transcript's last message: into it when it
/// is the user's (results, a nudge, the brief), which keeps the roles alternating, else as a
/// new user message. Answers the message to store, and whether it replaces the last one.
pub fn with_notes(last: Option<&Value>, notes: &[String]) -> (Value, bool) {
    let blocks: Vec<Value> = notes
        .iter()
        .map(|n| json!({"type": "text", "text": note_text(n)}))
        .collect();
    match last {
        Some(m) if m["role"] == "user" => {
            let mut content: Vec<Value> = match &m["content"] {
                Value::String(s) => vec![json!({"type": "text", "text": s})],
                Value::Array(a) => a.clone(),
                _ => Vec::new(),
            };
            content.extend(blocks);
            (json!({"role": "user", "content": content}), true)
        }
        _ => (json!({"role": "user", "content": blocks}), false),
    }
}

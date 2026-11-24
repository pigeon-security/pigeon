//! A provider's streamed answer, read as it arrives: server-sent events, split anywhere across
//! the chunks the network hands over, folded into the common reply shape. The content blocks
//! (text, thinking, tool calls) and the cache usage are kept too, not only the text. Pure:
//! bytes in, the reply so far out.

use serde_json::{Map, Value, json};

use super::{Dialect, Reply};

/// A streamed answer being read.
#[derive(Debug, Default)]
pub struct Reading {
    /// What has arrived and is not yet a whole line.
    pending: Vec<u8>,
    /// The current event's `data:` lines.
    data: Vec<String>,
    /// The model answering, as it says.
    pub model: String,
    /// The text so far, from every text block.
    pub text: String,
    /// The content blocks so far, by their index. A block not yet started is `Null`.
    pub blocks: Vec<Value>,
    /// Each block's tool input, as JSON text, until the block stops.
    partial: Vec<String>,
    /// Why the answer stopped.
    pub stop_reason: Option<String>,
    /// The tokens read.
    pub tokens_in: Option<u64>,
    /// The tokens written so far.
    pub tokens_out: Option<u64>,
    /// The tokens read from the provider's cache.
    pub cache_read: Option<u64>,
    /// The tokens written to the provider's cache.
    pub cache_write: Option<u64>,
    /// The part of the cache writes made to the 5-minute cache, when the provider splits them.
    pub cache_write_5m: Option<u64>,
    /// The part of the cache writes made to the 1-hour cache, when the provider splits them.
    pub cache_write_1h: Option<u64>,
    /// The provider's error, when it sent one in the stream.
    pub error: Option<String>,
    /// Whether the provider said the answer is complete.
    pub done: bool,
    /// Chat Completions: the block holding the text, once there is text.
    text_at: Option<usize>,
    /// Chat Completions: each tool call's own index, with the block it builds.
    calls: Vec<(u64, usize)>,
}

/// The longest line kept while waiting for its end: an event larger than this is an error.
const MAX_LINE: usize = 1 << 20;

/// The highest content block index accepted.
const MAX_BLOCK: u64 = 1024;

/// Appends `s` to the string field `key` of `map`, making the field when it is missing.
fn append(map: &mut Map<String, Value>, key: &str, s: &str) {
    if let Some(Value::String(cur)) = map.get_mut(key) {
        cur.push_str(s);
    } else {
        map.insert(key.into(), Value::String(s.into()));
    }
}

impl Reading {
    /// Reads `chunk`, the next bytes of the stream, as `dialect` streams.
    pub fn feed(&mut self, dialect: Dialect, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                let data = std::mem::take(&mut self.data).join("\n");
                if !data.is_empty() {
                    self.event(dialect, &data);
                }
            } else if let Some(d) = line.strip_prefix("data:") {
                self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            }
            // `event:`, `id:`, `retry:` and comments: the data says its own type.
        }
        if self.pending.len() > MAX_LINE {
            self.pending.clear();
            self.error
                .get_or_insert_with(|| "the stream sent a line longer than 1 MiB".into());
        }
    }

    /// Ends the stream: an event left without its blank line is read.
    pub fn end(&mut self, dialect: Dialect) {
        if !self.pending.is_empty() {
            self.feed(dialect, b"\n");
        }
        self.feed(dialect, b"\n");
    }

    /// Reads one whole event's data.
    fn event(&mut self, dialect: Dialect, data: &str) {
        if dialect == Dialect::OpenAi && data.trim() == "[DONE]" {
            self.finish_calls();
            self.done = true;
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        match dialect {
            Dialect::OpenAi if v["type"].is_string() => self.response_event(&v),
            Dialect::OpenAi => self.chunk(&v),
            Dialect::Anthropic => match v["type"].as_str() {
                Some("message_start") => {
                    let m = &v["message"];
                    if let Some(model) = m["model"].as_str() {
                        self.model = model.into();
                    }
                    self.read_usage(&m["usage"]);
                }
                Some("content_block_start") => self.block_start(&v),
                Some("content_block_delta") => self.block_delta(&v),
                Some("content_block_stop") => self.block_stop(&v),
                Some("message_delta") => {
                    if let Some(r) = v["delta"]["stop_reason"].as_str() {
                        self.stop_reason = Some(r.into());
                    }
                    // Cumulative, as the provider counts them.
                    self.read_usage(&v["usage"]);
                }
                Some("message_stop") => self.done = true,
                Some("error") => {
                    let e = &v["error"];
                    self.error = Some(format!(
                        "{}: {}",
                        e["type"].as_str().unwrap_or("error"),
                        e["message"]
                            .as_str()
                            .unwrap_or("the provider failed mid-stream")
                    ));
                }
                _ => {}
            },
        }
    }

    /// Reads one Chat Completions chunk.
    fn chunk(&mut self, v: &Value) {
        if v["error"].is_object() {
            let e = &v["error"];
            let kind = e["type"].as_str().or(e["code"].as_str()).unwrap_or("error");
            self.error = Some(format!(
                "{kind}: {}",
                e["message"]
                    .as_str()
                    .unwrap_or("the provider failed mid-stream")
            ));
            return;
        }
        if let Some(model) = v["model"].as_str() {
            self.model = model.into();
        }
        let choice = &v["choices"][0];
        if let Some(t) = choice["delta"]["content"].as_str()
            && !t.is_empty()
        {
            self.push_text(t);
        }
        if let Some(calls) = choice["delta"]["tool_calls"].as_array() {
            for (n, call) in calls.iter().enumerate() {
                self.tool_call(n as u64, call);
            }
        }
        if let Some(r) = choice["finish_reason"].as_str() {
            self.stop_reason = Some(
                match r {
                    "stop" => "end_turn",
                    "tool_calls" => "tool_use",
                    "length" => "max_tokens",
                    "content_filter" => "refusal",
                    other => other,
                }
                .into(),
            );
            self.finish_calls();
        }
        let u = &v["usage"];
        if u.is_object() {
            let prompt = u["prompt_tokens"].as_u64();
            let cached = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0);
            self.tokens_in = prompt.map(|p| p.saturating_sub(cached)).or(self.tokens_in);
            self.cache_read = prompt.map(|_| cached).or(self.cache_read);
            self.tokens_out = u["completion_tokens"].as_u64().or(self.tokens_out);
            self.cache_write = Some(0);
        }
    }

    /// Adds `t` to the text and to the one text block, which it makes when there is none yet.
    fn push_text(&mut self, t: &str) {
        self.text.push_str(t);
        let at = match self.text_at {
            Some(at) => at,
            None => {
                self.blocks.push(json!({"type": "text", "text": ""}));
                self.partial.push(String::new());
                let at = self.blocks.len() - 1;
                self.text_at = Some(at);
                at
            }
        };
        if let Some(m) = self.blocks.get_mut(at).and_then(Value::as_object_mut) {
            append(m, "text", t);
        }
    }

    /// Reads one Responses API event, told by its `type`. Tool calls are keyed by `output_index`.
    fn response_event(&mut self, v: &Value) {
        let kind = v["type"].as_str().unwrap_or("");
        match kind {
            "response.created" | "response.in_progress" => {
                if let Some(m) = v["response"]["model"].as_str() {
                    self.model = m.into();
                }
            }
            "response.output_text.delta" => {
                if let Some(t) = v["delta"].as_str().filter(|t| !t.is_empty()) {
                    self.push_text(t);
                }
            }
            "response.output_item.added" if v["item"]["type"] == "function_call" => {
                let index = v["output_index"].as_u64().unwrap_or(0);
                let item = &v["item"];
                let call = json!({
                    "id": item["call_id"],
                    "function": { "name": item["name"], "arguments": item["arguments"] },
                });
                self.tool_call(index, &call);
            }
            "response.function_call_arguments.delta" => {
                let index = v["output_index"].as_u64().unwrap_or(0);
                let call = json!({ "index": index, "function": { "arguments": v["delta"] } });
                self.tool_call(index, &call);
            }
            "response.output_item.done" if v["item"]["type"] == "function_call" => {
                // The whole arguments, when no delta came.
                let index = v["output_index"].as_u64().unwrap_or(0);
                let at = self
                    .calls
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map(|(_, at)| *at);
                let empty = at
                    .and_then(|at| self.partial.get(at))
                    .is_none_or(String::is_empty);
                if empty {
                    let item = &v["item"];
                    let call = json!({
                        "index": index,
                        "id": item["call_id"],
                        "function": { "name": "", "arguments": item["arguments"] },
                    });
                    self.tool_call(index, &call);
                }
            }
            "response.completed" | "response.incomplete" => {
                let r = &v["response"];
                if let Some(m) = r["model"].as_str() {
                    self.model = m.into();
                }
                self.finish_calls();
                let calls = !self.calls.is_empty();
                self.stop_reason = responses_stop(r, calls);
                let u = &r["usage"];
                if u.is_object() {
                    let input = u["input_tokens"].as_u64();
                    let cached = u["input_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    self.tokens_in = input.map(|p| p.saturating_sub(cached)).or(self.tokens_in);
                    self.cache_read = input.map(|_| cached).or(self.cache_read);
                    // Reasoning tokens are billed as output and are already in this count.
                    self.tokens_out = u["output_tokens"].as_u64().or(self.tokens_out);
                    self.cache_write = Some(0);
                }
                self.done = true;
            }
            "response.failed" => {
                let e = &v["response"]["error"];
                self.error = Some(format!(
                    "{}: {}",
                    e["code"].as_str().unwrap_or("error"),
                    e["message"]
                        .as_str()
                        .unwrap_or("the provider failed mid-stream")
                ));
            }
            "error" => {
                self.error = Some(format!(
                    "{}: {}",
                    v["code"].as_str().unwrap_or("error"),
                    v["message"]
                        .as_str()
                        .unwrap_or("the provider failed mid-stream")
                ));
            }
            _ => {}
        }
    }

    /// Reads one entry of a chunk's `tool_calls`; `n` is its place when it names no index.
    fn tool_call(&mut self, n: u64, call: &Value) {
        let index = call["index"].as_u64().unwrap_or(n);
        let at = match self.calls.iter().find(|(i, _)| *i == index) {
            Some((_, at)) => *at,
            None => {
                if self.calls.len() as u64 > MAX_BLOCK {
                    self.error = Some(format!("the stream named more than {MAX_BLOCK} tool calls"));
                    return;
                }
                self.blocks
                    .push(json!({"type": "tool_use", "id": "", "name": "", "input": {}}));
                self.partial.push(String::new());
                let at = self.blocks.len() - 1;
                self.calls.push((index, at));
                at
            }
        };
        let f = &call["function"];
        if let Some(m) = self.blocks.get_mut(at).and_then(Value::as_object_mut) {
            if let Some(id) = call["id"].as_str() {
                m.insert("id".into(), json!(id));
            }
            if let Some(name) = f["name"].as_str() {
                append(m, "name", name);
            }
        }
        if let (Some(a), Some(buf)) = (f["arguments"].as_str(), self.partial.get_mut(at)) {
            buf.push_str(a);
        }
    }

    /// Parses each tool call's joined arguments. Invalid JSON becomes `{}` and an error note.
    fn finish_calls(&mut self) {
        for (_, at) in self.calls.clone() {
            let Some(raw) = self.partial.get_mut(at).map(std::mem::take) else {
                continue;
            };
            if raw.trim().is_empty() {
                continue;
            }
            let input = match serde_json::from_str::<Value>(&raw) {
                Ok(v) if v.is_object() => v,
                _ => {
                    self.error
                        .get_or_insert_with(|| format!("tool input for call {at} was not JSON"));
                    json!({})
                }
            };
            if let Some(m) = self.blocks.get_mut(at).and_then(Value::as_object_mut) {
                m.insert("input".into(), input);
            }
        }
    }

    /// Reads a usage object, keeping each old value when the new one is absent.
    fn read_usage(&mut self, u: &Value) {
        self.tokens_in = u["input_tokens"].as_u64().or(self.tokens_in);
        self.tokens_out = u["output_tokens"].as_u64().or(self.tokens_out);
        self.cache_read = u["cache_read_input_tokens"].as_u64().or(self.cache_read);
        self.cache_write = u["cache_creation_input_tokens"]
            .as_u64()
            .or(self.cache_write);
        let split = &u["cache_creation"];
        self.cache_write_5m = split["ephemeral_5m_input_tokens"]
            .as_u64()
            .or(self.cache_write_5m);
        self.cache_write_1h = split["ephemeral_1h_input_tokens"]
            .as_u64()
            .or(self.cache_write_1h);
    }

    /// Makes room for block `i` and answers its place, or sets an error when `i` is too high.
    fn room(&mut self, i: u64) -> Option<usize> {
        if i > MAX_BLOCK {
            self.error = Some(format!(
                "the stream named content block {i}, more than {MAX_BLOCK}"
            ));
            return None;
        }
        let at = usize::try_from(i).ok()?;
        if self.blocks.len() <= at {
            self.blocks.resize(at + 1, Value::Null);
        }
        if self.partial.len() <= at {
            self.partial.resize(at + 1, String::new());
        }
        Some(at)
    }

    /// Reads a `content_block_start` event.
    fn block_start(&mut self, v: &Value) {
        let Some(i) = v["index"].as_u64().and_then(|i| self.room(i)) else {
            return;
        };
        if let Some(slot) = self.blocks.get_mut(i) {
            *slot = v["content_block"].clone();
        }
        if let Some(buf) = self.partial.get_mut(i) {
            buf.clear();
        }
    }

    /// Reads a `content_block_delta` event.
    fn block_delta(&mut self, v: &Value) {
        let delta = &v["delta"];
        let kind = delta["type"].as_str().unwrap_or("");
        if kind == "text_delta"
            && let Some(t) = delta["text"].as_str()
        {
            self.text.push_str(t);
        }
        let Some(i) = v["index"].as_u64().and_then(|i| self.room(i)) else {
            return;
        };
        let Some(slot) = self.blocks.get_mut(i) else {
            return;
        };
        if slot.is_null() {
            *slot = json!({"type": "unknown"});
        }
        match kind {
            "text_delta" => {
                if let (Some(t), Some(m)) = (delta["text"].as_str(), slot.as_object_mut()) {
                    append(m, "text", t);
                }
            }
            "input_json_delta" => {
                if let (Some(p), Some(buf)) =
                    (delta["partial_json"].as_str(), self.partial.get_mut(i))
                {
                    buf.push_str(p);
                }
            }
            "thinking_delta" => {
                if let (Some(t), Some(m)) = (delta["thinking"].as_str(), slot.as_object_mut()) {
                    append(m, "thinking", t);
                }
            }
            "signature_delta" => {
                if let (Some(s), Some(m)) = (delta["signature"].as_str(), slot.as_object_mut()) {
                    m.insert("signature".into(), Value::String(s.into()));
                }
            }
            _ => {
                // A kind not known yet: keep it only when it is plain text fields.
                if let (Some(fields), Some(m)) = (delta.as_object(), slot.as_object_mut()) {
                    let plain = fields.iter().all(|(k, val)| k == "type" || val.is_string());
                    if plain {
                        for (k, val) in fields {
                            if k == "type" {
                                continue;
                            }
                            if let Some(s) = val.as_str() {
                                append(m, k, s);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Reads a `content_block_stop` event.
    fn block_stop(&mut self, v: &Value) {
        let Some(i) = v["index"].as_u64() else {
            return;
        };
        if i > MAX_BLOCK {
            self.error = Some(format!(
                "the stream named content block {i}, more than {MAX_BLOCK}"
            ));
            return;
        }
        let Some(i) = usize::try_from(i).ok() else {
            return;
        };
        let Some(buf) = self.partial.get_mut(i) else {
            return;
        };
        let raw = std::mem::take(buf);
        if !raw.is_empty() {
            match serde_json::from_str::<Value>(&raw) {
                Ok(input) => {
                    if let Some(m) = self.blocks.get_mut(i).and_then(Value::as_object_mut) {
                        m.insert("input".into(), input);
                    }
                }
                Err(_) => {
                    self.error = Some(format!("tool input for block {i} was not JSON"));
                }
            }
        } else if let Some(m) = self.blocks.get_mut(i).and_then(Value::as_object_mut) {
            let tool = matches!(
                m.get("type").and_then(Value::as_str),
                Some("tool_use" | "server_tool_use")
            );
            if tool && !m.get("input").is_some_and(Value::is_object) {
                m.insert("input".into(), json!({}));
            }
        }
    }

    /// The content blocks so far, as a JSON array. Blocks not yet started are left out.
    pub fn content(&self) -> Value {
        Value::Array(
            self.blocks
                .iter()
                .filter(|b| !b.is_null())
                .cloned()
                .collect(),
        )
    }

    /// The usage so far, as a JSON object. What is not known is null.
    pub fn usage(&self) -> Value {
        let mut usage = json!({
            "input_tokens": self.tokens_in,
            "output_tokens": self.tokens_out,
            "cache_read_input_tokens": self.cache_read,
            "cache_creation_input_tokens": self.cache_write,
        });
        if self.cache_write_5m.is_some() || self.cache_write_1h.is_some() {
            usage["cache_creation"] = json!({
                "ephemeral_5m_input_tokens": self.cache_write_5m.unwrap_or(0),
                "ephemeral_1h_input_tokens": self.cache_write_1h.unwrap_or(0),
            });
        }
        usage
    }

    /// The reply so far.
    pub fn reply(&self) -> Reply {
        Reply {
            model: self.model.clone(),
            text: self.text.clone(),
            stop_reason: self.stop_reason.clone(),
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
        }
    }
}

/// Why a Responses API answer `r` stopped, in Anthropic's words: `tool_use` when it called a
/// function, `max_tokens` or `refusal` when it is incomplete for that reason, else `end_turn`.
/// `None` when the answer says no status.
pub fn responses_stop(r: &Value, calls: bool) -> Option<String> {
    let reason = match r["status"].as_str()? {
        "incomplete" => match r["incomplete_details"]["reason"].as_str() {
            Some("max_output_tokens") => "max_tokens",
            Some("content_filter") => "refusal",
            _ => "end_turn",
        },
        "completed" if calls => "tool_use",
        "completed" => "end_turn",
        other => other,
    };
    Some(reason.into())
}

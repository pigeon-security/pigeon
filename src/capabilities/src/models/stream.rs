//! A provider's streamed answer, read as it arrives: server-sent events, split anywhere across
//! the chunks the network hands over, folded into the common reply shape. Pure: bytes in, the
//! reply so far out.

use serde_json::Value;

use super::{Dialect, Reply};

/// A streamed answer being read.
#[derive(Debug, Default)]
pub struct Reading {
    /// What has arrived and is not yet a whole line.
    pending: Vec<u8>,
    /// The current event's `data:` lines.
    data: Vec<String>,
    pub model: String,
    pub text: String,
    pub stop_reason: Option<String>,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// The provider's error, when it sent one in the stream.
    pub error: Option<String>,
    /// Whether the provider said the answer is complete.
    pub done: bool,
}

/// The longest line kept while waiting for its end: an event larger than this is an error.
const MAX_LINE: usize = 1 << 20;

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

    fn event(&mut self, dialect: Dialect, data: &str) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        match dialect {
            Dialect::Anthropic => match v["type"].as_str() {
                Some("message_start") => {
                    let m = &v["message"];
                    if let Some(model) = m["model"].as_str() {
                        self.model = model.into();
                    }
                    self.tokens_in = m["usage"]["input_tokens"].as_u64().or(self.tokens_in);
                    self.tokens_out = m["usage"]["output_tokens"].as_u64().or(self.tokens_out);
                }
                Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
                    if let Some(t) = v["delta"]["text"].as_str() {
                        self.text.push_str(t);
                    }
                }
                Some("message_delta") => {
                    if let Some(r) = v["delta"]["stop_reason"].as_str() {
                        self.stop_reason = Some(r.into());
                    }
                    // Cumulative, as the provider counts them.
                    self.tokens_in = v["usage"]["input_tokens"].as_u64().or(self.tokens_in);
                    self.tokens_out = v["usage"]["output_tokens"].as_u64().or(self.tokens_out);
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

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = "event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\
\n\
event: ping\n\
data: {\"type\": \"ping\"}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo — ok\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hidden\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

    fn read_in(chunk: usize) -> Reading {
        let mut r = Reading::default();
        for piece in STREAM.as_bytes().chunks(chunk) {
            r.feed(Dialect::Anthropic, piece);
        }
        r.end(Dialect::Anthropic);
        r
    }

    #[test]
    fn a_stream_reads_the_same_however_it_is_split() {
        for chunk in [1, 2, 3, 7, 64, 4096] {
            let r = read_in(chunk);
            assert_eq!(r.text, "Hello — ok", "chunks of {chunk}");
            assert_eq!(r.model, "claude-opus-5-5");
            assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
            assert_eq!((r.tokens_in, r.tokens_out), (Some(12), Some(7)));
            assert!(r.done && r.error.is_none());
        }
    }

    #[test]
    fn an_error_event_is_kept_and_crlf_lines_read() {
        let mut r = Reading::default();
        r.feed(
            Dialect::Anthropic,
            b"event: error\r\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\r\n\r\n",
        );
        assert_eq!(r.error.as_deref(), Some("overloaded_error: Overloaded"));
        assert!(!r.done);
    }

    #[test]
    fn a_last_event_without_its_blank_line_is_read_at_the_end() {
        let mut r = Reading::default();
        r.feed(
            Dialect::Anthropic,
            b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}",
        );
        assert_eq!(r.text, "");
        r.end(Dialect::Anthropic);
        assert_eq!(r.text, "x");
    }

    #[test]
    fn junk_and_an_endless_line_do_not_break_it() {
        let mut r = Reading::default();
        r.feed(Dialect::Anthropic, b": comment\ndata: not json\n\n");
        assert!(r.error.is_none() && r.text.is_empty());
        r.feed(Dialect::Anthropic, &vec![b'x'; MAX_LINE + 1]);
        assert!(r.error.is_some());
    }
}

//! A Model Context Protocol server over the streamable HTTP transport, stateless: each POST
//! carries one JSON-RPC message and is answered with plain JSON. It offers tools only; the
//! caller supplies them and how to run one.

use serde::Serialize;
use serde_json::{Value, json};
use worker::Result;

/// The protocol version answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The server's name and version, as `initialize` answers them.
pub struct Server<'a> {
    pub name: &'a str,
    pub version: &'a str,
    /// Shown to the client after it connects.
    pub instructions: &'a str,
}

/// A tool a client may call.
#[derive(Serialize)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// The JSON Schema of its arguments.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl Tool {
    /// A tool taking no arguments.
    pub fn bare(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }
}

/// The answer to one JSON-RPC `message`, or `None` for a notification, which gets none.
/// `call` runs a tool by name with its arguments; its error becomes a tool error the model can
/// read, not a protocol error.
pub async fn answer(
    server: &Server<'_>,
    tools: &[Tool],
    message: &Value,
    call: impl AsyncFn(&str, &Value) -> Result<Value>,
) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    let params = &message["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": server.name, "version": server.version },
            "instructions": server.instructions,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            if !tools.iter().any(|t| t.name == name) {
                return Some(error(id, -32602, &format!("unknown tool `{name}`")));
            }
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args).await {
                Ok(v) => json!({
                    "content": [{ "type": "text", "text": text(&v) }],
                    "structuredContent": v,
                    "isError": false,
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true,
                }),
            }
        }
        _ => return Some(error(id, -32601, &format!("unknown method `{method}`"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

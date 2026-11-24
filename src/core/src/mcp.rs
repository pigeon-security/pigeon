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

/// What a call of a tool that is not offered is told, wherever it is called from.
pub fn unknown_tool(name: &str) -> String {
    format!("unknown tool `{name}`")
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
                return Some(error(id, -32602, &unknown_tool(name)));
            }
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args).await {
                Ok(v) => call_result(v),
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

/// A tool's successful result. MCP requires `structuredContent` to be an object, so a list or
/// scalar goes in the text only; otherwise clients reject the whole result.
fn call_result(v: Value) -> Value {
    let mut r = json!({
        "content": [{ "type": "text", "text": text(&v) }],
        "isError": false,
    });
    if v.is_object() {
        r["structuredContent"] = v;
    }
    r
}

fn text(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// One way a tool's declaration breaks the rules some MCP clients (ChatGPT) enforce.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The tool's name.
    pub tool: String,
    /// Where in the `inputSchema`, such as `/properties/brief`; empty for the tool's name.
    pub path: String,
    /// The rule broken, as a short code: `name`, `top-type`, `type`, `items`, `shape` or `ref`.
    pub rule: &'static str,
}

/// Whether `name` matches `^[a-zA-Z0-9_-]{1,64}$`.
fn name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Every rule that tool `name`, with `input_schema`, breaks. Pure: it reads nothing else and
/// changes nothing. An empty list means the tool conforms.
pub fn lint_tool(name: &str, input_schema: &Value) -> Vec<Violation> {
    let mut found = Vec::new();
    if !name_ok(name) {
        found.push(Violation {
            tool: name.into(),
            path: String::new(),
            rule: "name",
        });
    }
    lint_schema(name, input_schema, "", true, &mut found);
    found
}

/// Checks `schema` at `path`, then its properties, items and `anyOf`/`oneOf` branches.
/// `top` is true for the tool's whole schema, which must be an `object` with no `anyOf`.
fn lint_schema(tool: &str, schema: &Value, path: &str, top: bool, out: &mut Vec<Violation>) {
    let mut bad = |path: &str, rule: &'static str| {
        out.push(Violation {
            tool: tool.into(),
            path: path.into(),
            rule,
        })
    };
    let Some(obj) = schema.as_object() else {
        bad(path, if top { "top-type" } else { "type" });
        return;
    };
    if obj.contains_key("$ref") {
        bad(path, "ref");
        return;
    }
    // The declared types: one name, or a list of names.
    let kinds: Vec<&str> = match obj.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let branches: Vec<(&str, &Vec<Value>)> = ["anyOf", "oneOf"]
        .into_iter()
        .filter_map(|k| obj.get(k).and_then(Value::as_array).map(|a| (k, a)))
        .collect();
    if top {
        if kinds != ["object"] {
            bad(path, "top-type");
        }
    } else if kinds.is_empty() && branches.is_empty() {
        bad(path, "type");
    }
    if kinds.contains(&"array") {
        if obj.contains_key("items") {
            let items = format!("{path}/items");
            let mut inner = Vec::new();
            lint_schema(tool, &obj["items"], &items, false, &mut inner);
            out.extend(inner);
        } else {
            out.push(Violation {
                tool: tool.into(),
                path: path.into(),
                rule: "items",
            });
        }
    }
    if kinds.contains(&"object")
        && !obj.contains_key("properties")
        && !obj.contains_key("additionalProperties")
    {
        out.push(Violation {
            tool: tool.into(),
            path: path.into(),
            rule: "shape",
        });
    }
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        for (key, sub) in props {
            lint_schema(tool, sub, &format!("{path}/properties/{key}"), false, out);
        }
    }
    if let Some(extra) = obj.get("additionalProperties").filter(|v| v.is_object()) {
        lint_schema(
            tool,
            extra,
            &format!("{path}/additionalProperties"),
            false,
            out,
        );
    }
    if !top {
        for (key, list) in branches {
            for (i, sub) in list.iter().enumerate() {
                lint_schema(tool, sub, &format!("{path}/{key}/{i}"), false, out);
            }
        }
    }
}

/// The conformance report of `tools`: every violation, the counts, and the size of the
/// `tools/list` answer's `tools` as JSON, in bytes. Read-only.
pub fn conformance(tools: &[Tool]) -> Value {
    let violations: Vec<Violation> = tools
        .iter()
        .flat_map(|t| lint_tool(t.name, &t.input_schema))
        .collect();
    let bytes = serde_json::to_string(&json!({ "tools": tools })).map_or(0, |s| s.len());
    json!({
        "tools_checked": tools.len(),
        "violation_count": violations.len(),
        "violations": violations,
        "tools_list_bytes": bytes,
        "tool_count": tools.len(),
    })
}

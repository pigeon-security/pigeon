//! `call {tool, args}`: runs any hosted capability's tool by name. A session's MCP tool list is
//! fixed when the session starts, so a tool deployed after it is unreachable by name; through
//! `call` it is not. The tool runs as a direct call would ([`crate::dispatch`]): the same
//! caller, the same checks, the same errors. `call` never calls itself, and gives its caller
//! nothing it did not have: every tool it reaches is on the same endpoint, for the same caller.

use pigeon_core::mcp::Tool;
use serde_json::{Value, json};
use worker::Error;

use crate::{Call, Capability, Context, unknown};

/// This tool's name.
pub const CALL: &str = "call";

/// The tool `call` is asked for, and its arguments, when it may run them: any tool but `call`
/// itself, with arguments that are an object (`{}` when none are given), as `/mcp` takes them.
pub fn requested(args: &Value) -> std::result::Result<(&str, Value), String> {
    let tool = args["tool"]
        .as_str()
        .ok_or_else(|| "`tool` is required".to_string())?;
    if tool == CALL {
        return Err("`call` does not call itself".into());
    }
    let inner = match args.get("args") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err("`args` must be an object".into()),
    };
    Ok((tool, inner))
}

/// The `call` tool.
pub struct Dispatch;

impl Capability for Dispatch {
    fn name(&self) -> &'static str {
        "dispatch"
    }

    fn versions(&self) -> Vec<pigeon_core::versions::Component> {
        vec![pigeon_core::versions::Component::began(
            "dispatch",
            pigeon_core::versions::Kind::Capability,
        )]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![Tool {
            name: CALL,
            description: "Run any of this platform's tools by name, with its arguments: for a \
                          tool deployed after this session started, which its tool list does \
                          not show yet. Answers and fails exactly as calling the tool directly \
                          would. `architecture_describe` lists every tool.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tool": { "type": "string", "description": "The tool's name, such as `land_status`." },
                    "args": { "type": "object", "description": "Its arguments; none when omitted." }
                },
                "required": ["tool"]
            }),
        }]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            if tool != CALL {
                return Err(unknown(tool));
            }
            let (name, inner) = requested(args).map_err(Error::RustError)?;
            crate::dispatch(cx, name, &inner).await
        })
    }
}

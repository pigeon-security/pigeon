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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner;

    struct Fake(&'static str, &'static [&'static str]);

    impl Capability for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        fn tools(&self) -> Vec<Tool> {
            self.1.iter().map(|n| Tool::bare(n, "")).collect()
        }
        fn call<'a>(&'a self, _cx: &'a Context<'a>, tool: &'a str, _a: &'a Value) -> Call<'a> {
            Box::pin(async move { Err(unknown(tool)) })
        }
    }

    fn caps() -> Vec<Box<dyn Capability>> {
        vec![
            Box::new(Fake("models", &["models_run", "models_usage"])),
            Box::new(Fake("land", &["land", "land_status"])),
            Box::new(Dispatch),
        ]
    }

    #[test]
    fn a_tool_is_found_on_the_capability_that_offers_it() {
        let caps = caps();
        assert_eq!(owner(&caps, "land_status").unwrap().name(), "land");
        assert_eq!(owner(&caps, "models_run").unwrap().name(), "models");
        assert_eq!(owner(&caps, "call").unwrap().name(), "dispatch");
        assert!(owner(&caps, "land_").is_none());
        assert!(owner(&caps, "nothing").is_none());
    }

    #[test]
    fn call_takes_a_name_and_object_arguments() {
        let asked = json!({ "tool": "land", "args": { "branch": "claude/x" } });
        let (t, a) = requested(&asked).unwrap();
        assert_eq!((t, a), ("land", json!({ "branch": "claude/x" })));
        assert_eq!(
            requested(&json!({ "tool": "land_status" })).unwrap().1,
            json!({})
        );
        assert_eq!(
            requested(&json!({ "tool": "land_status", "args": null }))
                .unwrap()
                .1,
            json!({})
        );
        for bad in [json!({ "args": {} }), json!({ "tool": 3 })] {
            assert_eq!(requested(&bad).unwrap_err(), "`tool` is required");
        }
        for bad in [json!([1]), json!("x"), json!(1)] {
            assert_eq!(
                requested(&json!({ "tool": "land", "args": bad })).unwrap_err(),
                "`args` must be an object"
            );
        }
    }

    #[test]
    fn call_never_calls_itself() {
        assert!(requested(&json!({ "tool": "call", "args": { "tool": "call" } })).is_err());
        assert!(requested(&json!({ "tool": "call" })).is_err());
    }

    #[test]
    fn an_unknown_tool_reads_as_a_direct_call_reads() {
        assert_eq!(
            pigeon_core::mcp::unknown_tool("nothing"),
            "unknown tool `nothing`"
        );
    }
}

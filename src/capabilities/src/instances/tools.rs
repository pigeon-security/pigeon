//! Tool registration and target binding for the instance MCP endpoint.

use pigeon_core::mcp::Tool;
use serde_json::{Value, json};

use super::record::Instance;

#[derive(Clone, Copy)]
enum Capability {
    Publishing,
    Storage,
    Source,
}

const TOOLS: &[(&str, Capability)] = &[
    ("site_deploy", Capability::Publishing),
    ("site_rollback", Capability::Publishing),
    ("storage_list_objects", Capability::Storage),
    ("storage_get_object", Capability::Storage),
    (crate::source::read::RESOLVE, Capability::Source),
    (crate::source::read::FILE, Capability::Source),
];

const STORAGE_BINDINGS: &[&str] = &["account", "bucket", "presign", "expires_s"];

/// An approved instance's enabled capabilities. The request cannot supply a replacement scope.
pub struct Scope {
    instance: String,
    publishing: bool,
    storage: bool,
    source: bool,
}

impl Scope {
    pub fn new(instance: &Instance) -> serde_json::Result<Self> {
        Ok(Self {
            instance: instance.id.clone(),
            publishing: pigeon_core::site_publish::site(&serde_json::to_value(instance)?).is_ok(),
            storage: instance
                .storage
                .as_ref()
                .is_some_and(|profile| profile.enabled()),
            source: crate::source::read::approved_repo(instance).is_ok(),
        })
    }

    fn capability(&self, tool: &str) -> Option<Capability> {
        TOOLS.iter().find_map(|(name, capability)| {
            let enabled = match capability {
                Capability::Publishing => self.publishing,
                Capability::Storage => self.storage,
                Capability::Source => self.source,
            };
            (*name == tool && enabled).then_some(*capability)
        })
    }

    /// Keep the tools available to this instance and hide arguments supplied by the profile.
    pub fn restrict_tools(&self, tools: &mut Vec<Tool>) {
        tools.retain_mut(|tool| {
            if tool.name == crate::dispatch::CALL {
                tool.description = "Run an enabled tool of this instance by name.";
                return self.publishing || self.storage || self.source;
            }
            let Some(capability) = self.capability(tool.name) else {
                return false;
            };
            let hidden = |name: &str| {
                name == "instance"
                    || (matches!(capability, Capability::Storage) && STORAGE_BINDINGS.contains(&name))
            };
            if let Some(properties) = tool.input_schema["properties"].as_object_mut() {
                properties.retain(|name, _| !hidden(name));
            }
            if let Some(required) = tool.input_schema["required"].as_array_mut() {
                required.retain(|name| name.as_str().is_none_or(|name| !hidden(name)));
            }
            match tool.name {
                "storage_list_objects" => tool.description = "List objects in the instance’s approved storage location. Supply a relative prefix or the continuation token from the preceding page.",
                "storage_get_object" => tool.description = "Read a small object from the instance’s approved storage location. Supply a relative key.",
                _ => {}
            }
            true
        });
    }

    /// Bind direct and generic calls before either reaches the platform's usual approval gate.
    pub fn bind(&self, tool: &str, args: &Value) -> Result<Value, String> {
        if tool == crate::dispatch::CALL {
            let (name, inner) = crate::dispatch::requested(args)?;
            let mut bound = args.clone();
            bound["args"] = self.bind_tool(name, &inner)?;
            return Ok(bound);
        }
        self.bind_tool(tool, args)
    }

    fn bind_tool(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let capability = self
            .capability(tool)
            .ok_or("tool is unavailable on this instance")?;
        let mut bound = args.clone();
        let parameters = bound.as_object_mut().ok_or("expected tool arguments")?;
        if parameters
            .get("instance")
            .is_some_and(|target| target != &self.instance)
        {
            return Err("tool arguments cannot change the target instance".into());
        }
        if matches!(capability, Capability::Source)
            && parameters.keys().any(|name| {
                !matches!(name.as_str(), "instance" | "revision")
                    && !(tool == crate::source::read::FILE && name == "path")
            })
        {
            return Err("source repository and provider are fixed by the instance".into());
        }
        if matches!(capability, Capability::Storage)
            && STORAGE_BINDINGS
                .iter()
                .any(|name| parameters.contains_key(*name))
        {
            return Err("storage scope and download settings are fixed by the instance".into());
        }
        parameters.insert("instance".into(), json!(&self.instance));
        Ok(bound)
    }
}

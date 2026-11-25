//! Tool and console-view registration, and target binding, for an approved instance. One
//! [`Scope`] built from the record drives both the instance MCP endpoint's tools and the
//! console's instance areas (`console_manifest`), so neither decides on its own.

use pigeon_core::mcp::Tool;
use serde_json::{Value, json};

use super::record::Instance;

/// One tool's instance-specific contract, declared by the owning capability.
#[derive(Clone, Copy)]
pub struct Registration {
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub arguments: &'static [&'static str],
}

/// One console view a capability adds to an instance while it is enabled: a page in one of the
/// console's areas (`console::sections::AREAS`). Its area and id form the console's URL, so a
/// shipped id never changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    /// The area it sits in, such as `site`.
    pub area: &'static str,
    /// Its id within the area, such as `deployments`.
    pub id: &'static str,
    pub title: &'static str,
    /// Its place within the area, smallest first.
    pub order: u16,
    /// The capability version that first serves it. An instance whose pin selects an older
    /// version still lists it, flagged for an upgrade.
    pub since: Option<&'static str>,
}

/// What a capability adds to an instance: nothing while its profile is unset.
#[derive(Clone, Copy)]
pub struct Enabled {
    pub tools: &'static [Registration],
    pub views: &'static [View],
}

impl Enabled {
    pub const NONE: Enabled = Enabled {
        tools: &[],
        views: &[],
    };

    /// `tools` and `views` when `on`, else nothing.
    pub fn when(on: bool, tools: &'static [Registration], views: &'static [View]) -> Self {
        if on {
            Self { tools, views }
        } else {
            Self::NONE
        }
    }
}

type Register = fn(&Instance) -> serde_json::Result<Enabled>;

/// A capability that an instance enables by a field on its approved record.
pub struct Contributor {
    /// Its name in the console manifest, such as `publishing`.
    pub name: &'static str,
    /// The versioned component its pin names, such as `sites`.
    pub component: &'static str,
    pub register: Register,
}

/// Every instance capability. The console lists an area's views by their order, not this one.
pub const CAPABILITIES: &[Contributor] = &[
    Contributor {
        name: "publishing",
        component: "sites",
        register: super::publishing::instance_tools,
    },
    Contributor {
        name: "site_passkeys",
        component: "site_passkeys",
        register: crate::site_passkeys::instance_views,
    },
    Contributor {
        name: "storage",
        component: "storage",
        register: crate::storage::instance_tools,
    },
    Contributor {
        name: "source",
        component: "source",
        register: crate::source::read::instance_tools,
    },
    Contributor {
        name: "ci",
        component: "ci",
        register: crate::ci::instance_tools,
    },
    Contributor {
        name: "observability",
        component: "observability",
        register: crate::observability::instance::instance_tools,
    },
    Contributor {
        name: "ingest",
        component: "ingest",
        register: crate::ingest::instance_tools,
    },
];

/// What core does with an instance-targeted call from the global connector when the receiving
/// Worker's manifest does not advertise signed instance context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Keep the legacy envelope (CI reads, whose gateway may predate context).
    Legacy,
    /// Refuse before sending: the receiver never gets an unscoped copy of the call.
    Refuse,
}

/// A capability Worker's tools that act on one approved instance. When the global connector
/// (`/mcp` with no `instance`) names an instance in such a tool's arguments, core reads the
/// approved record, binds the call as the instance endpoint would ([`Scope::bind`]), and signs it
/// with that instance's context. The tools are the registrations the instance endpoint offers,
/// so both routes share one declaration.
pub struct Targeted {
    /// The capability's name in its Worker's manifest.
    pub capability: &'static str,
    /// Its instance-targeted tools.
    pub tools: &'static [Registration],
    pub unsupported: Unsupported,
}

/// Every remote capability with instance-targeted tools.
pub const TARGETED: &[Targeted] = &[
    Targeted {
        capability: super::publishing::CAPABILITY,
        tools: super::publishing::TOOLS,
        unsupported: Unsupported::Refuse,
    },
    Targeted {
        capability: crate::ci::Ci::NAME,
        tools: crate::ci::SCOPED_TOOLS,
        unsupported: Unsupported::Legacy,
    },
];

/// The declaration and instance a global-connector call targets: `None` for a tool that is not
/// instance-targeted, or a call that names no instance, which stay on the platform path. Pure.
pub fn global_target<'a>(
    capability: &str,
    tool: &str,
    args: &'a Value,
) -> Result<Option<(&'static Targeted, &'a str)>, String> {
    let Some(target) = TARGETED.iter().find(|target| {
        target.capability == capability && target.tools.iter().any(|t| t.name == tool)
    }) else {
        return Ok(None);
    };
    match args.get("instance") {
        None => Ok(None),
        Some(Value::String(id)) => Ok(Some((target, id.as_str()))),
        Some(_) => Err("`instance` must be an instance id".into()),
    }
}

/// Binds a global-connector call to the approved record `instance` names, with the instance
/// endpoint's checks: the record exists and enables `tool`, and the arguments stay inside its
/// registration. Answers the arguments to send. Pure.
pub fn bind_global(
    instance: &str,
    record: Option<&Instance>,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    let unknown = || format!("there is no approved instance `{instance}`");
    let record = record.filter(|r| r.id == instance).ok_or_else(unknown)?;
    let scope = Scope::new(record).map_err(|e| format!("instance `{instance}`: {e}"))?;
    scope
        .bind(tool, args)
        .map_err(|why| format!("`{tool}` on instance `{instance}`: {why}"))
}

/// A view an instance's enabled capability adds, with that capability.
#[derive(Clone, Copy, Debug)]
pub struct ScopedView {
    pub capability: &'static str,
    pub component: &'static str,
    pub view: &'static View,
}

/// An approved instance's enabled capabilities. The request cannot supply a replacement scope.
pub struct Scope {
    instance: String,
    registrations: Vec<&'static Registration>,
    views: Vec<ScopedView>,
}

impl Scope {
    pub fn new(instance: &Instance) -> serde_json::Result<Self> {
        let mut registrations: Vec<&'static Registration> = Vec::new();
        let mut views: Vec<ScopedView> = Vec::new();
        for contributor in CAPABILITIES {
            let enabled = (contributor.register)(instance)?;
            for view in enabled.views {
                if views
                    .iter()
                    .any(|known| known.view.area == view.area && known.view.id == view.id)
                {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate instance view registration: {}/{}",
                        view.area, view.id,
                    )));
                }
                views.push(ScopedView {
                    capability: contributor.name,
                    component: contributor.component,
                    view,
                });
            }
            for registration in enabled.tools {
                if registrations
                    .iter()
                    .any(|known| known.name == registration.name)
                {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate instance tool registration: {}",
                        registration.name,
                    )));
                }
                registrations.push(registration);
            }
        }
        Ok(Self {
            instance: instance.id.clone(),
            registrations,
            views,
        })
    }

    /// The console views the instance's enabled capabilities add.
    pub fn views(&self) -> &[ScopedView] {
        &self.views
    }

    /// The approved target identifier, used when core signs a call to another Worker.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    fn registration(&self, tool: &str) -> Option<&Registration> {
        self.registrations
            .iter()
            .copied()
            .find(|registration| registration.name == tool)
    }

    /// Keep the tools available to this instance and show only the arguments each registration
    /// accepts; the profile supplies the rest.
    pub fn restrict_tools(&self, tools: &mut Vec<Tool>) {
        tools.retain_mut(|tool| {
            if tool.name == crate::dispatch::CALL {
                tool.description = "Run an enabled tool of this instance by name.";
                return !self.registrations.is_empty();
            }
            let Some(registration) = self.registration(tool.name) else {
                return false;
            };
            let visible = |name: &str| name != "instance" && registration.arguments.contains(&name);
            if let Some(properties) = tool.input_schema["properties"].as_object_mut() {
                properties.retain(|name, _| visible(name));
            }
            if let Some(required) = tool.input_schema["required"].as_array_mut() {
                required.retain(|name| name.as_str().is_some_and(visible));
            }
            if let Some(description) = registration.description {
                tool.description = description;
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
        let registration = self
            .registration(tool)
            .ok_or("tool is unavailable on this instance")?;
        let mut bound = args.clone();
        let parameters = bound.as_object_mut().ok_or("expected tool arguments")?;
        if parameters
            .get("instance")
            .is_some_and(|target| target != &self.instance)
        {
            return Err("tool arguments cannot change the target instance".into());
        }
        if let Some(argument) = parameters.keys().find(|name| {
            name.as_str() != "instance"
                && !registration.arguments.contains(&name.as_str())
                && !(name.as_str() == crate::approvals::ARG && crate::approvals::gated(tool))
        }) {
            return Err(format!(
                "argument `{argument}` is unavailable for this instance tool"
            ));
        }
        parameters.insert("instance".into(), json!(&self.instance));
        Ok(bound)
    }
}

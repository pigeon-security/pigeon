//! Versions, read-only: what every hosted capability declares (`capabilities_versions`), and what
//! moving each instance's pins up to the newest served versions would take
//! (`capabilities_upgrade_plan`). Neither changes anything; a pin moves only through
//! `instance_propose` and the person's passkey. The design is in docs/plans/versioning.md.

use pigeon_core::mcp::Tool;
use pigeon_core::versions::{Component, Kind, pin_status, upgrade};
use serde_json::{Value, json};
use worker::{Error, Result};

use crate::instances::record::Instance;
use crate::{Call, Capability, Context, to_value, unknown};

/// The versioning tools, as a capability.
pub struct Versioning;

/// A declared component with the Worker that serves it.
pub type Carried = (Component, String);

/// Every component the capabilities declare, with the Worker serving it, plus the names of the
/// capabilities that declare none.
pub fn gather(cx: &Context<'_>) -> (Vec<Carried>, Vec<String>) {
    let mut carried = Vec::new();
    let mut undeclared = Vec::new();
    for c in cx.hosted.capabilities {
        let versions = c.versions();
        if versions.is_empty() {
            undeclared.push(c.name().to_string());
        }
        let worker = c.worker().unwrap_or(cx.hosted.worker).to_string();
        carried.extend(versions.into_iter().map(|v| (v, worker.clone())));
    }
    (carried, undeclared)
}

/// The components alone, in the order they were declared.
fn plain(components: &[Carried]) -> Vec<Component> {
    components.iter().map(|(c, _)| c.clone()).collect()
}

/// The answer of `capabilities_versions`: each component once (the first declaration wins), the
/// undeclared capabilities, each of `instances` with its pins checked, and the capability
/// catalogue (placement, scope and availability; docs/plans/capability-scoping.md).
pub fn versions_view(
    components: &[Carried],
    undeclared: &[String],
    instances: &[Instance],
    changelog: bool,
) -> Value {
    let mut seen: Vec<&str> = Vec::new();
    let mut rows = Vec::new();
    for (c, worker) in components {
        if seen.contains(&c.name.as_str()) {
            continue;
        }
        seen.push(&c.name);
        let mut row = json!({
            "name": c.name,
            "kind": to_value(c.kind),
            "version": c.version,
            "serves": c.serves,
            "worker": worker,
            "problems": c.problems(),
        });
        if changelog {
            row["changelog"] = to_value(&c.changelog);
        }
        rows.push(row);
    }
    let carried = plain(components);
    let instances: Vec<Value> = instances
        .iter()
        .map(|i| json!({ "id": i.id, "pins": to_value(pin_status(&i.pins, &carried)) }))
        .collect();
    json!({
        "components": rows,
        "undeclared": undeclared,
        "instances": instances,
        "catalogue": to_value(crate::catalogue::CATALOGUE),
    })
}

/// The answer of `capabilities_upgrade_plan`: for each of `instances`, the steps that move its
/// pins to the newest served versions, and the pins that hold it.
pub fn upgrade_view(components: &[Carried], instances: &[Instance]) -> Value {
    let carried = plain(components);
    let rows: Vec<Value> = instances
        .iter()
        .map(|i| {
            let held: Vec<String> = pin_status(&i.pins, &carried)
                .into_iter()
                .filter(|s| s.state == pigeon_core::versions::PinState::Held)
                .map(|s| s.component)
                .collect();
            json!({
                "id": i.id,
                "steps": to_value(upgrade(&i.pins, &carried)),
                "held": held,
            })
        })
        .collect();
    json!({
        "instances": rows,
        "note": "This changes nothing. To move a pin, propose the instance record with the new \
                 pin through `instance_propose`; it waits for the person's passkey.",
    })
}

/// The instances the call asks about: the named one, or all.
async fn chosen(cx: &Context<'_>, args: &Value) -> Result<Vec<Instance>> {
    match args["instance"].as_str() {
        Some(id) => match crate::instances::one(cx.env, id).await? {
            Some(i) => Ok(vec![i]),
            None => Err(Error::RustError(format!("no instance `{id}`"))),
        },
        None => crate::instances::all(cx.env).await,
    }
}

impl Capability for Versioning {
    fn name(&self) -> &'static str {
        "versioning"
    }

    fn versions(&self) -> Vec<Component> {
        vec![Component::began("versioning", Kind::Capability)]
    }

    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "capabilities_versions",
                description: "Read-only. The versions pigeon declares: each component's name, \
                              kind, newest version, versions served, Worker and problems, the \
                              capabilities that declare none, and each instance's pins checked \
                              against them, and the capability catalogue: each capability's \
                              placement, scope and availability. `changelog` adds each \
                              component's changelog; `instance` limits the pins to one instance.",
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "changelog": { "type": "boolean" },
                        "instance": { "type": "string" }
                    }
                }),
            },
            Tool {
                name: "capabilities_upgrade_plan",
                description: "Read-only. For each instance (or the one named), what moving its \
                              pins to the newest served versions takes: the steps with their \
                              breaking changes and migrations, and the pins that hold it. \
                              Changes nothing.",
                input_schema: json!({
                    "type": "object",
                    "properties": { "instance": { "type": "string" } }
                }),
            },
        ]
    }

    fn call<'a>(&'a self, cx: &'a Context<'a>, tool: &'a str, args: &'a Value) -> Call<'a> {
        Box::pin(async move {
            match tool {
                "capabilities_versions" => {
                    let (components, undeclared) = gather(cx);
                    let instances = chosen(cx, args).await?;
                    let changelog = args["changelog"].as_bool().unwrap_or(false);
                    Ok(versions_view(
                        &components,
                        &undeclared,
                        &instances,
                        changelog,
                    ))
                }
                "capabilities_upgrade_plan" => {
                    let (components, _) = gather(cx);
                    let instances = chosen(cx, args).await?;
                    Ok(upgrade_view(&components, &instances))
                }
                other => Err(unknown(other)),
            }
        })
    }
}

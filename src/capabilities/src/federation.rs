//! Explicit, bilateral MCP read access between instances served by this core.

use pigeon_core::mcp::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use worker::{Env, Error, Result};

use crate::digitalocean::health;
use crate::instances::{self, record::Instance, tools::Scope};
use crate::membership::{self, Access, Role};
use crate::users::{Caller, User, directory};
use crate::{Context, Hosted, catalogue};

pub const NAME: &str = "mcp-federation";
pub const DISCOVER: &str = "federated_instances";
pub const READS: &[&str] = &[health::STATUS, health::JOBS, health::JOB];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub peer: String,
    pub person: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_apps: bool,
    pub tools: Vec<String>,
}

fn opaque(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn valid(record: &Instance, grant: &Grant) -> bool {
    !grant.peer.eq_ignore_ascii_case(&record.id)
        && (1..=64).contains(&grant.peer.len())
        && grant.peer.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'/' | b'.')
        })
        && opaque(&grant.person)
        && (if grant.all_apps {
            grant.app.is_empty()
        } else {
            opaque(&grant.app) && grant.person != grant.app
        })
        && !grant.tools.is_empty()
        && grant.tools.len() <= READS.len()
        && grant
            .tools
            .iter()
            .enumerate()
            .all(|(i, tool)| READS.contains(&tool.as_str()) && !grant.tools[..i].contains(tool))
}

pub(crate) fn valid_grants(record: &Instance, capability: &str, grants: &[Grant]) -> bool {
    grants.is_empty()
        || (capability == NAME
            && grants.len() <= 16
            && grants.iter().enumerate().all(|(i, grant)| {
                valid(record, grant)
                    && !grants[..i].iter().any(|prior| {
                        prior.peer == grant.peer
                            && prior.person == grant.person
                            && prior.app == grant.app
                    })
            }))
}

pub fn changed(current: Option<&Instance>, next: &Instance) -> bool {
    current.and_then(|record| record.capabilities.get(NAME)) != next.capabilities.get(NAME)
}

pub(crate) fn grants(record: &Instance) -> &[Grant] {
    record
        .capabilities
        .get(NAME)
        .map_or(&[], |settings| &settings.federation)
}

impl Grant {
    fn covers_app(&self, app: &str, registered: bool) -> bool {
        if self.all_apps {
            registered
        } else {
            self.app == app
        }
    }
}

fn exact_member(access: &Access, id: &str) -> bool {
    let id = membership::key(id);
    access.instances.iter().any(|i| i == id)
        && access
            .roles
            .get(id)
            .is_some_and(|role| *role >= Role::Viewer)
}

/// Both records, both membership roles and the explicit person/app grant must agree.
pub fn permitted(
    source: &Instance,
    destination: &Instance,
    access: &Access,
    person: &str,
    app: &str,
    registered: bool,
    tool: &str,
) -> bool {
    let allows = |record: &Instance, peer: &str| {
        grants(record).iter().any(|grant| {
            grant.peer == peer
                && grant.person == person
                && grant.covers_app(app, registered)
                && grant.tools.iter().any(|name| name == tool)
        })
    };
    access.person == person
        && READS.contains(&tool)
        && catalogue::is_home(source)
        && !catalogue::is_home(destination)
        && source.id != destination.id
        && exact_member(access, &source.id)
        && exact_member(access, &destination.id)
        && allows(source, &destination.id)
        && allows(destination, &source.id)
}

fn auth(caller: &Caller, oauth: bool) -> Option<&User> {
    if !oauth || caller.run.is_some() || caller.user.kind != "service" {
        return None;
    }
    caller.acting_for.as_ref().filter(|u| u.kind == "standard")
}

/// Build each request's authority from Directory state. Existing OAuth sessions gain and lose
/// access with the current approvals, with no cross-Worker credential or exported assertion.
pub struct Available {
    source: Option<String>,
    destinations: Vec<(Scope, Vec<&'static str>)>,
}

impl Available {
    pub async fn load(env: &Env, caller: &Caller, oauth: bool) -> Result<Self> {
        let empty = Self {
            source: None,
            destinations: Vec::new(),
        };
        let mut destinations = Vec::new();
        let Some(person) = auth(caller, oauth) else {
            return Ok(empty);
        };
        let access = membership::access(env, &person.id).await?;
        let Some(home) = access.home.as_deref() else {
            return Ok(empty);
        };
        let source = match instances::one(env, home).await? {
            Some(record) => Some(record),
            None => match instances::rename::renamed_from(home) {
                Some(old) => instances::one(env, old).await?,
                None => None,
            },
        };
        let Some(source) = source else {
            return Ok(empty);
        };
        if !grants(&source).iter().any(|g| g.person == person.id) {
            return Ok(empty);
        }
        let registered = directory(
            env,
            "oauth.service_registered",
            json!({"service_user_id": caller.user.id}),
        )
        .await?
            == true;
        for peer in grants(&source)
            .iter()
            .filter(|g| g.person == person.id && g.covers_app(&caller.user.id, registered))
            .map(|g| &g.peer)
            .filter(|peer| exact_member(&access, peer))
            .collect::<std::collections::BTreeSet<_>>()
        {
            let Some(destination) = instances::one(env, peer).await? else {
                continue;
            };
            if destination.id != *peer {
                continue;
            }
            let scope = Scope::new(&destination)?;
            let tools: Vec<_> = READS
                .iter()
                .copied()
                .filter(|tool| {
                    permitted(
                        &source,
                        &destination,
                        &access,
                        &person.id,
                        &caller.user.id,
                        registered,
                        tool,
                    ) && scope.bind(tool, &json!({})).is_ok()
                })
                .collect();
            if !tools.is_empty() {
                destinations.push((scope, tools));
            }
        }
        Ok(Self {
            source: Some(source.id),
            destinations,
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        READS.iter().copied().filter(|name| {
            self.destinations
                .iter()
                .any(|(_, tools)| tools.contains(name))
        })
    }

    pub fn tools(&self, hosted: &Hosted<'_>) -> Vec<Tool> {
        let mut tools: Vec<_> = self
            .names()
            .filter_map(|name| {
                crate::owner(hosted.capabilities, name)?
                    .tools()
                    .into_iter()
                    .find(|tool| tool.name == name)
            })
            .collect();
        if !tools.is_empty() {
            for tool in &mut tools {
                tool.input_schema["properties"]["instance"] = json!({
                    "type": "string",
                    "description": "Explicit destination from federated_instances."
                });
                if let Some(required) = tool.input_schema["required"].as_array_mut() {
                    if !required.iter().any(|name| name == "instance") {
                        required.push(json!("instance"));
                    }
                } else {
                    tool.input_schema["required"] = json!(["instance"]);
                }
            }
            tools.push(Tool::bare(
                DISCOVER,
                "List the instances and read tools approved for this MCP connection.",
            ));
        }
        tools
    }

    pub async fn call(&self, cx: &Context<'_>, name: &str, args: &Value) -> Result<Value> {
        if name == DISCOVER {
            if !args.as_object().is_some_and(|fields| fields.is_empty()) {
                return Err(Error::RustError(
                    "federated_instances takes no arguments".into(),
                ));
            }
            return Ok(
                json!({"instances": self.destinations.iter().map(|(scope, tools)| {
                json!({"instance": scope.instance(), "tools": tools})
            }).collect::<Vec<_>>() }),
            );
        }
        let instance = crate::arg(args, "instance")?;
        let Some((scope, _)) = self
            .destinations
            .iter()
            .find(|(scope, tools)| scope.instance() == instance && tools.contains(&name))
        else {
            return Err(Error::RustError(
                "this federated tool is not approved for that instance".into(),
            ));
        };
        let scoped = Context {
            account: cx.account,
            env: cx.env,
            hosted: cx.hosted,
            caller: cx.caller,
            caller_note: cx.caller_note,
            instance: Some(scope),
            worker: None,
        };
        let result = crate::dispatch_checked(&scoped, name, args).await;
        if let Some(caller) = cx.caller
            && let Some(person) = caller.acting_for.as_ref()
        {
            worker::console_log!(
                "{}",
                json!({
                    "event": "mcp_federation", "instance": scope.instance(),
                    "source": self.source, "source_worker": cx.hosted.worker, "person": person.id,
                    "app": caller.user.id, "tool": name, "succeeded": result.is_ok()
                })
            );
        }
        result
    }
}

//! Module inventory from the same source and hosted manifests as the architecture diagram.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::{Entry, Part, Source, built_from, code, refs};
use crate::Hosted;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    InstanceFeature,
    MainPlatformFeature,
    SharedFoundation,
    PlatformAdministration,
    Routing,
    Unclassified,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::InstanceFeature => "Instance feature",
            Self::MainPlatformFeature => "Main-platform feature",
            Self::SharedFoundation => "Shared foundation",
            Self::PlatformAdministration => "Platform administration",
            Self::Routing => "Routing and host setup",
            Self::Unclassified => "Unclassified",
        }
    }
}

/// A named source module or a tool capability registered from another Worker.
#[derive(Clone, Serialize)]
pub struct Module {
    pub name: String,
    pub source: String,
    /// The rollout classification. It does not enforce access.
    pub scope: Scope,
    /// Workers that register this capability. Shared code can have no registered owner.
    pub hosted_by: Vec<String>,
    /// Workers whose deployment inputs include its source code.
    pub built_into: Vec<String>,
    /// Tool and route names reported by the current hosted capability.
    pub tools: Vec<String>,
    pub routes: Vec<String>,
    /// Static Rust module references, qualified by crate.
    pub dependencies: Vec<String>,
    pub versions: Vec<String>,
}

fn scope(name: &str, foundation: bool) -> Scope {
    use Scope::*;
    if foundation {
        return SharedFoundation;
    }
    match name {
        "ci" | "data-ingest" | "ingest" | "observability" | "site_passkeys" | "sites"
        | "source" | "storage" => InstanceFeature,
        "ai-agents" | "apple" | "changelog" | "cloud" | "cost" | "github" | "land" | "mirror"
        | "models" => MainPlatformFeature,
        "approvals" | "dispatch" | "expiry" | "instances" | "ledger" | "linked" | "oauth"
        | "passkeys" | "policy" | "proxy" | "remote" | "secrets" | "split" | "users" => {
            SharedFoundation
        }
        "architecture" | "console" | "portal_oauth" | "versioning" => PlatformAdministration,
        "public_host" | "site" => Routing,
        _ => Unclassified,
    }
}

pub(super) fn collect(
    sources: &[&Source<'_>],
    caps: &[Part],
    blocks: &[Part],
    entries: &[Entry],
    hosted: &Hosted<'_>,
) -> Vec<Module> {
    let cap_dir = "src/capabilities/src";
    let core_dir = "src/core/src";
    let mut modules: BTreeMap<(String, bool), String> = caps
        .iter()
        .map(|p| ((p.name.clone(), true), format!("{cap_dir}/{}.rs", p.name)))
        .chain(
            blocks
                .iter()
                .map(|p| ((p.name.clone(), false), format!("{core_dir}/{}.rs", p.name))),
        )
        .collect();
    for capability in hosted.capabilities {
        let name = capability.name();
        let crate_lib = format!("src/{name}/src/lib.rs");
        if sources.iter().any(|s| s.path == crate_lib) {
            // A facade can share the name of a dedicated implementation crate.
            modules.entry((name.to_string(), true)).or_insert(crate_lib);
        } else {
            modules
                .entry((name.to_string(), true))
                .or_insert_with(|| format!("{cap_dir}/{name}.rs"));
        }
    }
    let cap_names = caps.iter().map(|part| part.name.clone()).collect();
    let core_names = blocks.iter().map(|part| part.name.clone()).collect();
    modules
        .into_iter()
        .map(|((name, capability), source)| {
            let module_dir = source.trim_end_matches(".rs");
            let crate_dir = format!("src/{name}/src/");
            let related: Vec<_> = sources
                .iter()
                .filter(|s| {
                    s.path == source
                        || s.path
                            .strip_prefix(module_dir)
                            .is_some_and(|p| p.starts_with('/'))
                        || (capability && s.path.starts_with(&crate_dir))
                })
                .collect();
            let mut dependencies = BTreeSet::new();
            for related in &related {
                let rust = code(related.text);
                let (names, prefix) = if related.path.starts_with(core_dir) {
                    (&core_names, "pigeon-core")
                } else {
                    (&cap_names, "pigeon-capabilities")
                };
                // A dedicated capability's local crate modules are implementation details.
                if related.path.starts_with(core_dir) || related.path.starts_with(cap_dir) {
                    dependencies.extend(
                        refs(&rust, "crate::", names)
                            .into_iter()
                            .filter(|called| called != &name)
                            .map(|called| format!("{prefix}::{called}")),
                    );
                }
                if capability {
                    dependencies.extend(
                        refs(&rust, "pigeon_core::", &core_names)
                            .into_iter()
                            .map(|called| format!("pigeon-core::{called}")),
                    );
                }
            }
            let mut owners = BTreeSet::new();
            let mut tools = BTreeSet::new();
            let mut routes = BTreeSet::new();
            let mut versions = BTreeSet::new();
            if capability {
                for c in hosted.capabilities.iter().filter(|c| c.name() == name) {
                    owners.insert(c.worker().unwrap_or(hosted.worker).to_string());
                    tools.extend(c.tools().into_iter().map(|tool| tool.name.to_string()));
                    routes.extend(c.routes().into_iter().map(str::to_string));
                    versions.extend(
                        c.versions()
                            .into_iter()
                            .map(|v| format!("{} {}", v.name, v.version)),
                    );
                }
            }
            let built_into: BTreeSet<_> = entries
                .iter()
                .filter(|entry| related.iter().any(|s| built_from(entry, s.path)))
                .map(|entry| entry.script.clone())
                .collect();
            Module {
                scope: scope(&name, !capability),
                name,
                source,
                hosted_by: owners.into_iter().collect(),
                built_into: built_into.into_iter().collect(),
                tools: tools.into_iter().collect(),
                routes: routes.into_iter().collect(),
                dependencies: dependencies.into_iter().collect(),
                versions: versions.into_iter().collect(),
            }
        })
        .collect()
}

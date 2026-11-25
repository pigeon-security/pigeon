//! Registration for the publishing tools served by pigeon-sites, and the Site views they add.
use super::{
    record::Instance,
    tools::{Enabled, Registration, View},
};

/// The capability pigeon-sites serves these tools under.
pub const CAPABILITY: &str = "sites";

pub const TOOLS: &[Registration] = &[
    Registration {
        name: "site_deploy",
        description: None,
        arguments: &["rev"],
    },
    Registration {
        name: "site_releases",
        description: None,
        arguments: &["limit"],
    },
    Registration {
        name: "site_rollback",
        description: None,
        arguments: &["release"],
    },
];

const VIEWS: &[View] = &[
    View {
        area: "site",
        id: "sites",
        title: "Sites",
        order: 10,
        since: None,
    },
    View {
        area: "site",
        id: "deployments",
        title: "Deployments",
        order: 20,
        since: None,
    },
];

/// Whether the record names a publishable site.
pub fn enabled(instance: &Instance) -> serde_json::Result<bool> {
    Ok(pigeon_core::site_publish::site(&serde_json::to_value(instance)?).is_ok())
}

pub fn instance_tools(instance: &Instance) -> serde_json::Result<Enabled> {
    Ok(Enabled::when(enabled(instance)?, TOOLS, VIEWS))
}

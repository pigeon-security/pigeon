//! Registration for the publishing tools served by pigeon-sites.
use super::{record::Instance, tools::Registration};

const TOOLS: &[Registration] = &[
    Registration {
        name: "site_deploy",
        description: None,
        arguments: &["rev"],
    },
    Registration {
        name: "site_rollback",
        description: None,
        arguments: &["release"],
    },
];

pub fn instance_tools(instance: &Instance) -> serde_json::Result<&'static [Registration]> {
    Ok(
        if pigeon_core::site_publish::site(&serde_json::to_value(instance)?).is_ok() {
            TOOLS
        } else {
            &[]
        },
    )
}

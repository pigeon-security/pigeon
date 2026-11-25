//! The console's areas and the sections platform capabilities put in them. An area is one item
//! of the console's navigation, with a stable id ([`AREAS`]); `console_manifest` lists the
//! areas an instance shows (docs/plans/console-redesign.md). A section is something a platform
//! capability declares in an area: a view of it (`view`), or, until the legacy panels go, only
//! a data anchor that `/console/api/section/{id}` answers. Several capabilities may fill the
//! same anchor, as `secrets` and `proxy` both fill `credentials`, and the console answers it
//! once with every owner. Pure: no I/O and no `worker` types.

use serde::Serialize;

/// Which part of the navigation an area sits in.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Above every instance: Inbox.
    Top,
    /// The selected instance's own areas.
    Instance,
    /// Platform-wide areas, shown on the home instance only.
    Platform,
}

/// One item of the console's navigation.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    /// Its stable id, which the console's URL carries: a shipped id never changes.
    pub id: &'static str,
    pub title: &'static str,
    pub group: Group,
    /// Its keyboard shortcut after `g`.
    pub key: &'static str,
}

const fn area(id: &'static str, title: &'static str, group: Group, key: &'static str) -> Area {
    Area {
        id,
        title,
        group,
        key,
    }
}

/// Every area, in the navigation's order.
pub const AREAS: [Area; 14] = [
    area("inbox", "Inbox", Group::Top, "i"),
    area("overview", "Overview", Group::Instance, "v"),
    area("site", "Site", Group::Instance, "w"),
    area("storage", "Storage", Group::Instance, "b"),
    area("source", "Source", Group::Instance, "r"),
    area("ci", "CI", Group::Instance, "k"),
    area("metrics", "Metrics", Group::Instance, "m"),
    area("ingest", "Ingest", Group::Instance, "e"),
    area("capabilities", "Capabilities", Group::Instance, "x"),
    area("sessions", "Sessions", Group::Platform, "s"),
    area("cost", "Cost", Group::Platform, "c"),
    area("access", "Access", Group::Platform, "a"),
    area("instances", "Instances", Group::Platform, "n"),
    area("platform", "Platform", Group::Platform, "p"),
];

/// The area with id `id`, if any.
pub fn area_named(id: &str) -> Option<&'static Area> {
    AREAS.iter().find(|a| a.id == id)
}

/// The area a capability Worker's `SectionSpec.tab` names: an area id, or one of the old tab
/// names a Worker built before areas sends (`approvals`, `architecture`).
pub fn area_of_tab(tab: &str) -> Option<&'static Area> {
    let id = match tab {
        "approvals" => "inbox",
        "architecture" => "platform",
        other => other,
    };
    area_named(id)
}

/// A part of an area that a platform capability fills.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Section {
    /// The id of the area it sits in, one of [`AREAS`].
    pub area: &'static str,
    /// Its id: the view's id within the area when `view` is set, and the anchor
    /// `/console/api/section/{id}` answers. Lowercase ASCII letters, digits and hyphens.
    pub id: &'static str,
    pub title: &'static str,
    /// One line under the title.
    pub description: &'static str,
    /// Its place within the area, smallest first.
    pub order: u16,
    /// The name it adds to the overview's total cost under, when it reports a cost.
    pub cost: Option<&'static str>,
    /// Whether `console_manifest` lists it as a view. A section that is not a view is a legacy
    /// panel's data anchor only, deleted with the legacy panels.
    pub view: bool,
}

/// Whether `id` is a valid anchor: 1 to 32 characters of a-z, 0-9 and `-`.
pub fn anchor(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

//! The console's areas and the views hosted capabilities put in them. An area is one item of
//! the console's navigation, with a stable id ([`AREAS`]); `console_manifest` lists the areas an
//! instance shows: those an enabled capability fills (docs/plans/console-redesign.md,
//! docs/plans/capability-scoping.md). A section is a view a hosted capability declares in one
//! of the areas; its data comes from the tools the view reads through `/app/op`. Pure: no I/O
//! and no `worker` types.

use serde::Serialize;

/// The part of the navigation an area sat in before capability scoping. Only a label now: the
/// manifest still sends it for the clients that decode it (the iOS app), and nothing decides
/// by it. An area shows on an instance while an enabled capability fills it.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    /// Above every instance: Inbox.
    Top,
    /// The selected instance's own areas.
    Instance,
    /// Areas the home instance's platform capabilities fill today.
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
pub const AREAS: [Area; 16] = [
    area("inbox", "Inbox", Group::Top, "i"),
    area("overview", "Overview", Group::Instance, "v"),
    area("site", "Site", Group::Instance, "w"),
    area("storage", "Storage", Group::Instance, "b"),
    area("source", "Source", Group::Instance, "r"),
    area("ci", "CI", Group::Instance, "k"),
    area("metrics", "Metrics", Group::Instance, "m"),
    area("jobs", "Jobs", Group::Instance, "j"),
    area("ingest", "Ingest", Group::Instance, "e"),
    area("email", "Email", Group::Instance, "l"),
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

/// A view a hosted capability adds to one of the areas.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Section {
    /// The id of the area it sits in, one of [`AREAS`].
    pub area: &'static str,
    /// The view's stable id within the area: lowercase ASCII letters, digits and hyphens.
    pub id: &'static str,
    pub title: &'static str,
    /// One line under the title.
    pub description: &'static str,
    /// Its place within the area, smallest first.
    pub order: u16,
}

/// Whether `id` is a valid view id: 1 to 32 characters of a-z, 0-9 and `-`.
pub fn view_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

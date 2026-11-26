//! The capability catalogue (docs/plans/capability-scoping.md): for each capability a hosting
//! Worker runs for an instance, where it may run (its [`Placement`]), what its data is about
//! (its [`Scope`]) and which instances may enable it (its [`Availability`]). Every console view
//! comes from a capability enabled on the selected instance: [`hosted_enabled`] says which of
//! the hosted capabilities are, and `console_manifest` shows only their views. The capabilities
//! an instance record enables by its own profile fields (publishing, storage, source, CI,
//! Metrics, ingest) are `instances::tools::CAPABILITIES`. Pure: no I/O.

use serde::Serialize;

use crate::instances::record::{AccountRef, Instance};

/// Where a capability runs for an instance.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Inside the Worker that already serves the instance: `pigeon-platform` on home.
    Inline,
    /// In a Worker of its own, behind a Service Binding.
    OwnWorker,
}

/// What a capability's data is about.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One instance: its secrets, its spend, its runs.
    Instance,
    /// The platform as a whole: every instance, every person.
    Platform,
}

/// Which instances may enable a capability.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Any instance.
    Any,
    /// Only the home instance, the one the platform runs in.
    HomeOnly,
}

/// One capability of the catalogue.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The capability's name, as its hosted `Capability::name` and the console manifest give it.
    pub name: &'static str,
    /// The placements it offers. More than one means an instance may choose.
    pub placements: &'static [Placement],
    /// Where it runs on the home instance today.
    pub home: Placement,
    /// The Worker it runs in on home when that is a Worker of its own.
    pub home_worker: Option<&'static str>,
    pub scope: Scope,
    pub availability: Availability,
    /// The capabilities it needs enabled beside it.
    pub requires: &'static [&'static str],
}

impl Entry {
    /// Whether an instance may choose where it runs.
    pub fn offers_choice(&self) -> bool {
        self.placements.len() > 1
    }
}

const INLINE: &[Placement] = &[Placement::Inline];
const EITHER: &[Placement] = &[Placement::Inline, Placement::OwnWorker];

const fn entry(
    name: &'static str,
    placements: &'static [Placement],
    home: Placement,
    home_worker: Option<&'static str>,
    scope: Scope,
    availability: Availability,
    requires: &'static [&'static str],
) -> Entry {
    Entry {
        name,
        placements,
        home,
        home_worker,
        scope,
        availability,
        requires,
    }
}

/// Every catalogued capability. A hosted capability missing here is treated as home-only.
pub const CATALOGUE: &[Entry] = &[
    entry(
        "ai-agents",
        EITHER,
        Placement::OwnWorker,
        Some("pigeon-ai-agents"),
        Scope::Instance,
        Availability::Any,
        &[],
    ),
    entry(
        "observability",
        INLINE,
        Placement::Inline,
        None,
        Scope::Instance,
        Availability::Any,
        &[],
    ),
    entry(
        "secrets",
        INLINE,
        Placement::Inline,
        None,
        Scope::Instance,
        Availability::Any,
        &[],
    ),
    entry(
        "proxy",
        EITHER,
        Placement::OwnWorker,
        Some("pigeon-proxy"),
        Scope::Instance,
        Availability::Any,
        &["secrets"],
    ),
    entry(
        "models",
        EITHER,
        Placement::OwnWorker,
        Some("pigeon-model-gateway"),
        Scope::Instance,
        Availability::Any,
        &[],
    ),
    // Instances › All instances and Platform › Endpoints, and creating and linking instances.
    entry(
        "instances",
        INLINE,
        Placement::Inline,
        None,
        Scope::Platform,
        Availability::HomeOnly,
        &[],
    ),
    // Identity stays platform-level; per-instance membership is a later phase.
    entry(
        "users",
        INLINE,
        Placement::Inline,
        None,
        Scope::Platform,
        Availability::HomeOnly,
        &[],
    ),
    // The selected instance's own setup: on home the platform's, as before.
    entry(
        "architecture",
        INLINE,
        Placement::Inline,
        None,
        Scope::Instance,
        Availability::Any,
        &[],
    ),
];

/// The catalogue's entry for `name`, if it has one.
pub fn find(name: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.name == name)
}

/// Whether `record` is the home account's instance: the one the platform runs in.
pub fn is_home(record: &Instance) -> bool {
    record.account == AccountRef::Home
}

/// Whether hosted capability `name` is enabled on `record`, so its views show in that
/// instance's console. The home instance runs the platform, so every hosted capability is
/// enabled there. Another instance enables none yet: per-capability enablement on linked
/// instances is M4.
pub fn hosted_enabled(record: &Instance, _name: &str) -> bool {
    is_home(record)
}

/// The capabilities enabled on `record`, by name: those its profile fields enable (`scope`),
/// then the hosted ones [`hosted_enabled`] says are, in the order `hosted` lists them.
pub fn enabled(
    record: &Instance,
    scope: &crate::instances::tools::Scope,
    hosted: &crate::Hosted,
) -> Vec<&'static str> {
    let mut out = scope.capabilities().to_vec();
    for c in hosted.capabilities {
        let name = c.name();
        if hosted_enabled(record, name) && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

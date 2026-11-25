//! The console's tabs and the sections capabilities put in them. A tab is one of five fixed
//! pages, listed in the header's order (Approvals only while something waits). A section is one
//! part of a tab; several capabilities may fill the same one, as `secrets` and `proxy` both fill
//! Credentials, and the console lists it once with every owner. Pure: no I/O and no `worker` types.

use serde::Serialize;

/// A console tab.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Tab {
    /// What waits for the person's passkey; listed only while something does.
    Approvals,
    /// What pigeon runs, here and in linked accounts.
    Instances,
    Architecture,
    /// Secrets and credentials, people, and delegation.
    Access,
    Cost,
}

impl Tab {
    /// Every tab, in the header's order.
    pub const ALL: [Tab; 5] = [
        Tab::Approvals,
        Tab::Instances,
        Tab::Architecture,
        Tab::Access,
        Tab::Cost,
    ];

    /// Its path: `/approvals`, `/instances`, `/architecture`, `/access`, `/cost`.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Approvals => "/approvals",
            Tab::Instances => "/instances",
            Tab::Architecture => "/architecture",
            Tab::Access => "/access",
            Tab::Cost => "/cost",
        }
    }

    /// Its title in the header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Approvals => "Approvals",
            Tab::Instances => "Instances",
            Tab::Architecture => "Architecture",
            Tab::Access => "Access",
            Tab::Cost => "Cost",
        }
    }

    /// The tab at `path`, if any.
    pub fn from_path(path: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|tab| tab.path() == path)
    }
}

/// Where the console's old tabs went: `/gateways` to Cost and `/settings` to Access. A browser
/// keeps the fragment, so `/settings#NAME` still lands on that secret.
pub fn moved(path: &str) -> Option<&'static str> {
    match path {
        "/gateways" => Some(Tab::Cost.path()),
        "/settings" => Some(Tab::Access.path()),
        _ => None,
    }
}

/// A part of a tab that a capability fills.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Section {
    pub tab: Tab,
    /// Its anchor, such as `models`: lowercase ASCII letters, digits and hyphens.
    pub id: &'static str,
    pub title: &'static str,
    /// One line under the title.
    pub description: &'static str,
    /// Its place within the tab, smallest first.
    pub order: u16,
    /// The name it adds to the overview's total cost under, when it reports a cost.
    pub cost: Option<&'static str>,
}

/// Whether `id` is a valid anchor: 1 to 32 characters of a-z, 0-9 and `-`.
pub fn anchor(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

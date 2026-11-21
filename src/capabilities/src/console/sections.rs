//! The console's tabs and the sections capabilities put in them. A tab is one of four fixed
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
    Architecture,
    /// Secrets and credentials, people, and delegation.
    Access,
    Cost,
}

impl Tab {
    /// Every tab, in the header's order.
    pub const ALL: [Tab; 4] = [Tab::Approvals, Tab::Architecture, Tab::Access, Tab::Cost];

    /// Its path: `/approvals`, `/architecture`, `/access`, `/cost`.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Approvals => "/approvals",
            Tab::Architecture => "/architecture",
            Tab::Access => "/access",
            Tab::Cost => "/cost",
        }
    }

    /// Its title in the header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Approvals => "Approvals",
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

/// A section as the console lists it: the section and the capabilities that fill it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Listed {
    #[serde(flatten)]
    pub section: Section,
    /// Every capability that declares it, by name.
    pub owners: Vec<String>,
}

/// A tab as the console lists it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TabListing {
    pub tab: Tab,
    pub path: &'static str,
    pub title: &'static str,
    pub sections: Vec<Listed>,
    /// How many things wait for the person, on the Approvals tab; the page hides the tab at 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<usize>,
}

/// Every tab with its sections, from `(capability name, section)` pairs: tabs in the header's
/// order, all four even when empty; sections by `order` then `id`, one per tab and id with
/// every owner, its title and order from the first owner by name. A section whose id is not a
/// valid anchor is left out.
pub fn tabs(declared: &[(String, Section)]) -> Vec<TabListing> {
    let mut valid: Vec<&(String, Section)> =
        declared.iter().filter(|(_, s)| anchor(s.id)).collect();
    valid.sort_by(|a, b| a.0.cmp(&b.0));
    Tab::ALL
        .into_iter()
        .map(|tab| TabListing {
            tab,
            path: tab.path(),
            title: tab.title(),
            waiting: None,
            sections: listed_in(tab, &valid),
        })
        .collect()
}

/// One tab's sections, merged by id, from declarations sorted by capability name.
fn listed_in(tab: Tab, sorted: &[&(String, Section)]) -> Vec<Listed> {
    let mut listed: Vec<Listed> = Vec::new();
    for (name, section) in sorted.iter().map(|pair| (&pair.0, &pair.1)) {
        if section.tab != tab {
            continue;
        }
        match listed.iter_mut().find(|l| l.section.id == section.id) {
            Some(existing) => {
                if !existing.owners.contains(name) {
                    existing.owners.push(name.clone());
                }
            }
            None => listed.push(Listed {
                section: section.clone(),
                owners: vec![name.clone()],
            }),
        }
    }
    for l in &mut listed {
        l.owners.sort();
    }
    listed.sort_by(|a, b| (a.section.order, a.section.id).cmp(&(b.section.order, b.section.id)));
    listed
}

/// The overview's costs: `(name, cost)` for each section that reports one, in section order,
/// from `(section id, cost)` answers, and their total. A section that declares a cost but has
/// no answer counts as 0.
pub fn costs(listing: &[TabListing], answers: &[(String, f64)]) -> (Vec<(&'static str, f64)>, f64) {
    let items: Vec<(&'static str, f64)> = listing
        .iter()
        .flat_map(|tab| tab.sections.iter())
        .filter_map(|l| Some((l.section.cost?, answer_for(answers, l.section.id))))
        .collect();
    let total = items.iter().map(|(_, cost)| cost).sum();
    (items, total)
}

/// The first answer for a section id, or 0 when there is none.
fn answer_for(answers: &[(String, f64)], id: &str) -> f64 {
    answers
        .iter()
        .find(|(answer_id, _)| answer_id == id)
        .map_or(0.0, |(_, cost)| *cost)
}

/// Whether `id` is a valid anchor: 1 to 32 characters of a-z, 0-9 and `-`.
pub fn anchor(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

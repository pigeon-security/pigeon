//! The console's tabs and the sections capabilities put in them. A tab is one of four fixed
//! pages, always listed in the header's order. A section is one part of a tab; several
//! capabilities may fill the same one, as `secrets` and `proxy` both fill Keys, and the console
//! lists it once with every owner. Pure: no I/O and no `worker` types.

use serde::Serialize;

/// A console tab.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Tab {
    Overview,
    Architecture,
    Gateways,
    Settings,
}

impl Tab {
    /// Every tab, in the header's order.
    pub const ALL: [Tab; 4] = [
        Tab::Overview,
        Tab::Architecture,
        Tab::Gateways,
        Tab::Settings,
    ];

    /// Its path: `/`, `/architecture`, `/gateways`, `/settings`.
    pub fn path(self) -> &'static str {
        match self {
            Tab::Overview => "/",
            Tab::Architecture => "/architecture",
            Tab::Gateways => "/gateways",
            Tab::Settings => "/settings",
        }
    }

    /// Its title, as the header shows it.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Architecture => "Architecture",
            Tab::Gateways => "Gateways",
            Tab::Settings => "Settings",
        }
    }

    /// The tab a request path opens: exact paths only, so `/settings/x` and `/Settings` are none.
    pub fn from_path(path: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|tab| tab.path() == path)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn section(tab: Tab, id: &'static str, order: u16) -> Section {
        Section {
            tab,
            id,
            title: "T",
            description: "D",
            order,
            cost: None,
        }
    }

    fn decl(name: &str, s: Section) -> (String, Section) {
        (name.to_string(), s)
    }

    fn ids(listing: &TabListing) -> Vec<&'static str> {
        listing.sections.iter().map(|l| l.section.id).collect()
    }

    #[test]
    fn tabs_come_in_header_order_even_with_none_declared() {
        let listing = tabs(&[]);
        let order: Vec<Tab> = listing.iter().map(|t| t.tab).collect();
        assert_eq!(order, Tab::ALL.to_vec());
        assert!(listing.iter().all(|t| t.sections.is_empty()));
    }

    #[test]
    fn two_owners_of_one_id_make_one_section_listing_both() {
        let declared = vec![
            decl("secrets", section(Tab::Settings, "keys", 1)),
            decl("proxy", section(Tab::Settings, "keys", 5)),
        ];
        let settings = &tabs(&declared)[3];
        assert_eq!(settings.sections.len(), 1);
        assert_eq!(settings.sections[0].owners, vec!["proxy", "secrets"]);
        assert_eq!(settings.sections[0].section.order, 5);
    }

    #[test]
    fn sections_sort_by_order_then_id_and_bad_anchors_are_left_out() {
        let declared = vec![
            decl("a", section(Tab::Gateways, "zeta", 1)),
            decl("a", section(Tab::Gateways, "beta", 2)),
            decl("a", section(Tab::Gateways, "alpha", 2)),
            decl("a", section(Tab::Gateways, "Not An Anchor", 0)),
        ];
        assert_eq!(ids(&tabs(&declared)[2]), vec!["zeta", "alpha", "beta"]);
    }

    #[test]
    fn a_path_opens_a_tab_only_when_exact() {
        assert_eq!(Tab::from_path("/"), Some(Tab::Overview));
        assert_eq!(Tab::from_path("/settings"), Some(Tab::Settings));
        assert_eq!(Tab::from_path("/settings/x"), None);
        assert_eq!(Tab::from_path("/Settings"), None);
    }

    #[test]
    fn costs_sum_the_declared_ones_and_a_missing_answer_is_zero() {
        let mut models = section(Tab::Gateways, "models", 1);
        models.cost = Some("Model cost");
        let mut ci = section(Tab::Gateways, "ci", 2);
        ci.cost = Some("CI cost");
        let declared = vec![
            decl("models", models),
            decl("ci", ci),
            decl("secrets", section(Tab::Settings, "keys", 1)),
        ];
        let (items, total) = costs(&tabs(&declared), &[("models".into(), 44.3)]);
        assert_eq!(items, vec![("Model cost", 44.3), ("CI cost", 0.0)]);
        assert!((total - 44.3).abs() < 1e-9);
    }
}

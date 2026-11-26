//! The versions `pigeon-sites` declares: its own, and each template's.

use pigeon_core::versions::{Component, Entry, Kind};

/// A template as a component: `name`, serving `serves` (newest first) at the newest, with its
/// changelog (newest first) as `(version, breaking, summary, migration)`.
fn template_component(
    name: &str,
    serves: &[&str],
    changelog: impl Iterator<Item = (&'static str, bool, &'static str, Option<&'static str>)>,
) -> Component {
    let mut c = Component::new(
        name,
        Kind::Template,
        serves.first().copied().unwrap_or("1.0.0"),
    )
    .serving(serves);
    c.changelog = changelog
        .map(|(version, breaking, summary, migration)| Entry {
            version: version.into(),
            breaking,
            summary: summary.into(),
            migration: migration.map(str::to_string),
        })
        .collect();
    c
}

/// The `blog` template as a component: its changelog from [`pigeon_blog::CHANGELOG`] (newest
/// first), serving every version in [`pigeon_blog::TEMPLATES`], at the newest.
pub fn blog_component() -> Component {
    let serves: Vec<&str> = pigeon_blog::TEMPLATES.iter().map(|t| t.version).collect();
    template_component(
        "blog",
        &serves,
        pigeon_blog::CHANGELOG
            .iter()
            .map(|e| (e.version, e.breaking, e.summary, e.migration)),
    )
}

/// The `landing` template as a component: its changelog from [`pigeon_landing::CHANGELOG`]
/// (newest first), serving every version in [`pigeon_landing::TEMPLATES`], at the newest.
pub fn landing_component() -> Component {
    let serves: Vec<&str> = pigeon_landing::TEMPLATES
        .iter()
        .map(|t| t.version)
        .collect();
    template_component(
        "landing",
        &serves,
        pigeon_landing::CHANGELOG
            .iter()
            .map(|e| (e.version, e.breaking, e.summary, e.migration)),
    )
}

/// The `sites` capability as a component.
pub fn sites_component() -> Component {
    Component::began("sites", Kind::Capability)
}

/// Everything this Worker declares: `sites`, then the `blog` and `landing` templates.
pub fn declared() -> Vec<Component> {
    vec![sites_component(), blog_component(), landing_component()]
}

//! The versions `pigeon-sites` declares: its own, and the blog template's.

use pigeon_core::versions::{Component, Entry, Kind};

/// The `blog` template as a component: its changelog from [`pigeon_blog::CHANGELOG`] (newest
/// first), serving every version in [`pigeon_blog::TEMPLATES`], at the newest.
pub fn blog_component() -> Component {
    let serves: Vec<&str> = pigeon_blog::TEMPLATES.iter().map(|t| t.version).collect();
    let mut c = Component::new(
        "blog",
        Kind::Template,
        pigeon_blog::TEMPLATES
            .first()
            .map_or("1.0.0", |t| t.version),
    )
    .serving(&serves);
    c.changelog = pigeon_blog::CHANGELOG
        .iter()
        .map(|e| Entry {
            version: e.version.into(),
            breaking: e.breaking,
            summary: e.summary.into(),
            migration: e.migration.map(str::to_string),
        })
        .collect();
    c
}

/// The `sites` capability as a component.
pub fn sites_component() -> Component {
    Component::began("sites", Kind::Capability)
}

/// Everything this Worker declares: `sites`, then the `blog` template.
pub fn declared() -> Vec<Component> {
    vec![sites_component(), blog_component()]
}

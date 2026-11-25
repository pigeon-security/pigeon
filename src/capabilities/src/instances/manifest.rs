//! `console_manifest`: the console's navigation for one instance, as data (docs/plans/
//! console-redesign.md, M3, M4, M6, P12, P16). The approved record is the only input that
//! decides what an instance shows: its [`Scope`] gives the instance areas' views, the same
//! Scope that limits the instance's MCP endpoint. Platform capabilities' view sections fill the
//! platform areas, on the home instance only. It answers ids, titles, orders and counts, never
//! HTML, so the console and the iOS app build the same navigation from it. Pure but for
//! [`console_manifest`], which reads the record, versions and inbox.

use pigeon_core::approval::{Kind as ApprovalKind, Record, State};
use pigeon_core::versions::{Component, Requirement, Version, find};
use serde_json::{Value, json};
use worker::{Error, Result};

use super::record::{AccountRef, Instance};
use super::tools::Scope;
use crate::Context;
use crate::console::sections::{AREAS, Group, Section, area_named};

/// The tool's name.
pub const TOOL: &str = "console_manifest";

/// A platform capability's view section, with the capability that declares it.
pub type PlatformView = (&'static str, Section);

/// Whether `record` is the home account's instance: the one the platform areas show on.
pub fn is_home(record: &Instance) -> bool {
    record.account == AccountRef::Home
}

/// The version of `component` that `record` runs: the newest served one its pin allows, or the
/// newest served one when it has no pin. None when pigeon declares no such component, or the
/// pin allows nothing served.
pub fn selected(record: &Instance, component: &str, carried: &[Component]) -> Option<Version> {
    let c = find(carried, component)?;
    match record.pins.get(component) {
        Some(pin) => c.select(&Requirement::parse(pin).ok()?),
        None => c.current(),
    }
}

/// The upgrade a view needs on `record`: set when the view was first served by a newer version
/// than the one the record's pin selects.
pub fn upgrade(since: Option<&str>, selected: Option<Version>) -> Value {
    match (since.and_then(|s| Version::parse(s).ok()), selected) {
        (Some(since), Some(selected)) if since > selected => {
            json!({ "selected": selected.to_string(), "since": since.to_string() })
        }
        _ => Value::Null,
    }
}

/// The instance an approval is about, when it names one: an instance draft's or linked plan's
/// subject, or a call's `instance` argument. One that names none is the home instance's.
pub fn approval_instance(r: &Record) -> Option<&str> {
    match r.kind {
        ApprovalKind::InstanceDraft | ApprovalKind::LinkedPlan => Some(r.subject.as_str()),
        _ => r.summary["args"]["instance"]
            .as_str()
            .or_else(|| r.summary["instance"].as_str()),
    }
}

/// How many requests wait in `record`'s Inbox: on the home instance every pending approval and
/// every landing awaiting approval; on any other, only the approvals about it (M6).
pub fn inbox_count(record: &Instance, open: &[Record], landings: usize) -> usize {
    let pending = open.iter().filter(|r| r.state == State::Pending);
    if is_home(record) {
        pending.count() + landings
    } else {
        pending
            .filter(|r| approval_instance(r) == Some(record.id.as_str()))
            .count()
    }
}

/// The first hostname the instance serves, else its first zone.
fn host(record: &Instance) -> Option<&str> {
    record
        .sites
        .iter()
        .flat_map(|s| s.targets.iter())
        .find_map(|t| match t {
            super::record::Target::Zone { hostname, .. }
            | super::record::Target::Domain { hostname } => Some(hostname.as_str()),
            _ => None,
        })
        .or_else(|| record.zones.first().map(String::as_str))
}

/// The manifest: `record`'s areas in [`AREAS`] order, each with its views sorted by order.
/// Inbox, Overview and Capabilities are always there; an instance area only while a capability
/// the record enables adds a view to it; a platform area only on the home instance and while a
/// platform capability declares a view in it. `inbox` is None when it could not be read.
pub fn manifest(
    record: &Instance,
    scope: &Scope,
    platform: &[PlatformView],
    carried: &[Component],
    inbox: Option<usize>,
) -> Value {
    let home = is_home(record);
    let mut views: Vec<(&'static str, u16, Value)> = Vec::new();
    for v in scope.views() {
        views.push((
            v.view.area,
            v.view.order,
            json!({
                "id": v.view.id, "title": v.view.title, "order": v.view.order,
                "capability": v.capability,
                "upgrade": upgrade(v.view.since, selected(record, v.component, carried)),
            }),
        ));
    }
    if home {
        for (capability, s) in platform {
            let platform_area = area_named(s.area).is_some_and(|a| a.group == Group::Platform);
            let known = views
                .iter()
                .any(|(a, _, v)| *a == s.area && v["id"] == s.id);
            if !s.view || !platform_area || known {
                continue;
            }
            views.push((
                s.area,
                s.order,
                json!({
                    "id": s.id, "title": s.title, "order": s.order,
                    "capability": capability, "upgrade": null,
                }),
            ));
        }
    }
    let always = ["inbox", "overview", "capabilities"];
    let areas: Vec<Value> = AREAS
        .iter()
        .filter_map(|area| {
            let mut mine: Vec<&(&str, u16, Value)> =
                views.iter().filter(|(a, _, _)| *a == area.id).collect();
            if mine.is_empty() && !always.contains(&area.id) {
                return None;
            }
            mine.sort_by_key(|(_, order, view)| (*order, view["id"].as_str().map(String::from)));
            Some(json!({
                "id": area.id, "title": area.title, "group": area.group, "key": area.key,
                "views": mine.into_iter().map(|(_, _, v)| v.clone()).collect::<Vec<_>>(),
            }))
        })
        .collect();
    json!({
        "instance": {
            "id": record.id, "title": record.title, "home": home, "host": host(record),
        },
        "areas": areas,
        "inbox": { "count": inbox },
    })
}

/// `console_manifest`: the manifest of the instance `args.instance` names, or of the home
/// instance. Its Inbox counts what waits for the person the caller is or acts for.
pub async fn console_manifest(cx: &Context<'_>, args: &Value) -> Result<Value> {
    let record = match args["instance"].as_str().filter(|id| !id.is_empty()) {
        Some(id) => super::one(cx.env, id)
            .await?
            .ok_or_else(|| Error::RustError(format!("no instance `{id}`")))?,
        None => super::all(cx.env)
            .await?
            .into_iter()
            .find(is_home)
            .ok_or_else(|| Error::RustError("no home instance".into()))?,
    };
    let scope = Scope::new(&record)?;
    let platform: Vec<PlatformView> = cx
        .hosted
        .capabilities
        .iter()
        .flat_map(|c| {
            let name = c.name();
            c.sections().into_iter().map(move |s| (name, s))
        })
        .collect();
    let carried: Vec<Component> = crate::versioning::gather(cx)
        .0
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    let person = cx.caller.and_then(|c| {
        Some(&c.user)
            .filter(|u| u.kind == "standard")
            .or(c.acting_for.as_ref().filter(|u| u.kind == "standard"))
    });
    // A source that cannot be read leaves the count unknown, not the navigation missing.
    let inbox = match person {
        Some(person) => match crate::approvals::open_for(cx.env, person).await {
            Ok(open) => {
                let landings = if is_home(&record) {
                    crate::land::awaiting(cx.env).await.ok().map(|l| l.len())
                } else {
                    Some(0)
                };
                landings.map(|l| inbox_count(&record, &open, l))
            }
            Err(_) => None,
        },
        None => None,
    };
    Ok(manifest(&record, &scope, &platform, &carried, inbox))
}

//! The capability catalogue (docs/plans/capability-scoping.md): for each capability a hosting
//! Worker runs for an instance, where it may run (its [`Placement`]), what its data is about
//! (its [`Scope`]) and which instances may enable it (its [`Availability`]). Every console view
//! comes from a capability enabled on the selected instance: [`hosted_enabled`] says which of
//! the hosted capabilities are, and `console_manifest` shows only their views. An instance
//! record lists the catalogue capabilities it enables in `capabilities`, checked by
//! [`problems`]. The capabilities a record enables by its own profile fields (publishing,
//! storage, source, CI, Metrics, ingest) are `instances::tools::CAPABILITIES`. Pure: no I/O.

use serde::{Deserialize, Serialize};

use crate::instances::record::{AccountRef, Instance};

/// Where a capability runs for an instance.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Inside the Worker that already serves the instance: `pigeon-platform` on home.
    Inline,
    /// In a Worker of its own, behind a Service Binding.
    OwnWorker,
}

impl Placement {
    /// As a record and the console write it: `inline` or `own_worker`.
    pub fn as_str(self) -> &'static str {
        match self {
            Placement::Inline => "inline",
            Placement::OwnWorker => "own_worker",
        }
    }
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
    /// The capability's name, as its hosted `Capability::name`, the console manifest and an
    /// instance record's `capabilities` give it.
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
    /// Whether a linked instance's record may enable it yet. M4 turns it on one capability at a
    /// time, each with what the instance's Worker needs to serve it and the scopes it needs.
    pub linked: bool,
    /// Whether every instance has it, so a record never lists it: `instances`, whose record,
    /// plan and apply every instance needs.
    pub always: bool,
    /// Whether even home must list this capability in its approved record.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub opt_in: bool,
}

impl Entry {
    /// Whether an instance may choose where it runs.
    pub fn offers_choice(&self) -> bool {
        self.placements.len() > 1
    }
}

const INLINE: &[Placement] = &[Placement::Inline];
const EITHER: &[Placement] = &[Placement::Inline, Placement::OwnWorker];
/// Only in a Worker of its own: the instance's data Worker.
const OWN: &[Placement] = &[Placement::OwnWorker];

/// An instance-scoped capability any instance may have, run inline, needing nothing beside it,
/// not yet on linked instances: what each entry differs from.
const BASE: Entry = Entry {
    name: "",
    placements: INLINE,
    home: Placement::Inline,
    home_worker: None,
    scope: Scope::Instance,
    availability: Availability::Any,
    requires: &[],
    linked: false,
    always: false,
    opt_in: false,
};

/// One that runs inline or in its own Worker, and on home runs in `worker`.
const fn own(name: &'static str, worker: &'static str) -> Entry {
    Entry {
        name,
        placements: EITHER,
        home: Placement::OwnWorker,
        home_worker: Some(worker),
        ..BASE
    }
}

/// One about the platform as a whole, which only home may have.
const fn platform(name: &'static str) -> Entry {
    Entry {
        name,
        scope: Scope::Platform,
        availability: Availability::HomeOnly,
        ..BASE
    }
}

/// Every catalogued capability. A hosted capability missing here is treated as home-only.
pub const CATALOGUE: &[Entry] = &[
    // Sessions › Live and Compare.
    own("ai-agents", "pigeon-ai-agents"),
    // Metrics › Workers, and on home Cost › 7 days, Today and 30 days: both shown only when the
    // record's own `observability` is enabled. Not `linked`: a linked instance's Cost would be its
    // own account's spend, and Cloudflare offers no Billing OAuth scope to read it (M4 step 3,
    // deferred; docs/plans/capability-scoping.md says what would build it).
    Entry {
        name: "observability",
        ..BASE
    },
    // Access › Secrets. On a linked instance (M4, step 2) its own account's Secrets Store,
    // which needs the link's `secrets-store.write` (`linked::CAPABILITY_SCOPES`).
    Entry {
        name: "secrets",
        linked: true,
        ..BASE
    },
    // Access › Tokens: issuing, listing and revoking proxy credentials, which core serves. The
    // proxies those credentials reach, `/git/` and `/cf/` on the proxy host, are served by the
    // proxy Worker, `pigeon-proxy`, and are not this capability.
    Entry {
        name: "proxy",
        requires: &["secrets"],
        ..BASE
    },
    // Cloud Gateway setup status runs inline on core.
    Entry {
        linked: true,
        opt_in: true,
        name: crate::cloud_scope::NAME,
        placements: &[Placement::Inline, Placement::OwnWorker],
        ..BASE
    },
    // DigitalOcean connection management is served inline on core.
    Entry {
        name: crate::digitalocean::NAME,
        linked: true,
        opt_in: true,
        requires: &[crate::cloud_scope::NAME],
        ..BASE
    },
    Entry {
        name: crate::federation::NAME,
        linked: true,
        opt_in: true,
        ..BASE
    },
    // Jobs › Runs reads instance-owned job history in core.
    Entry {
        name: JOBS,
        linked: true,
        opt_in: true,
        ..BASE
    },
    // Platform › Models.
    own("models", "pigeon-model-gateway"),
    // Instances › All instances, Platform › Endpoints, and creating and linking instances.
    platform(MULTI_INSTANCE),
    // Access › People and Apps. Identity stays platform-level; per-instance membership is a
    // later phase.
    platform("users"),
    // Platform › Architecture: the selected instance's own setup; on home the platform's. On a
    // linked instance (M4, step 1) it reads only the record and what the platform serves for
    // it, so it needs no linked-account scope.
    Entry {
        name: "architecture",
        linked: true,
        ..BASE
    },
    // No views of its own: every instance's record, `console_manifest`, plan, apply and retire.
    Entry {
        name: "instances",
        linked: true,
        always: true,
        ..BASE
    },
    // The instance's unified data store, lite tier (docs/plans/email.md): bucket, D1 and data
    // Worker `{instance}-data`, `pigeon-data` on home, made by an instance plan, never by a
    // landing. Opt-in even on home; not on linked instances yet.
    Entry {
        name: crate::data_store::NAME,
        placements: OWN,
        home: Placement::OwnWorker,
        home_worker: Some("pigeon-data"),
        opt_in: true,
        ..BASE
    },
    // Email › Mailboxes (docs/plans/email.md): mail received through the zone's catch-all into
    // the data store, so it needs `data` and runs in the data Worker. Opt-in even on home.
    Entry {
        name: crate::email::NAME,
        placements: OWN,
        home: Placement::OwnWorker,
        home_worker: Some("pigeon-data"),
        requires: &[crate::data_store::NAME],
        opt_in: true,
        ..BASE
    },
    // No views of its own: the record's `redirects`, answered by the instance's own site
    // Worker (docs/plans/redirects.md). Set by that field, never listed in `capabilities`.
    Entry {
        name: "redirects",
        linked: true,
        ..BASE
    },
];

/// The hosted capabilities that are internal plumbing rather than something a person uses:
/// request dispatch, the Worker split, versioning, the console itself, passkeys, OAuth, the
/// public host. Each is still a declared component in `capabilities_versions`,
/// marked `plumbing`, so the console can tuck it away instead of listing it beside the
/// capabilities a person enables or works with.
pub const PLUMBING: &[&str] = &[
    "dispatch",
    "split",
    "versioning",
    "console",
    "passkeys",
    "oauth",
    "public_host",
];

/// Whether the hosted capability or component `name` is internal plumbing ([`PLUMBING`]).
pub fn is_plumbing(name: &str) -> bool {
    PLUMBING.contains(&name)
}

/// The capability that works across instances: split out of `instances` (M2).
pub const MULTI_INSTANCE: &str = "multi-instance";

/// Read-only history of jobs that ran for an instance.
pub const JOBS: &str = "jobs";

/// The catalogue's entry for `name`, if it has one.
pub fn find(name: &str) -> Option<&'static Entry> {
    CATALOGUE.iter().find(|e| e.name == name)
}

/// Whether `record` is the home account's instance: the one the platform runs in.
pub fn is_home(record: &Instance) -> bool {
    record.account == AccountRef::Home
}

/// Whether hosted capability `name` is enabled on `record`, so its views show in that
/// instance's console. The home instance runs the platform, so every hosted capability
/// is enabled there except catalogued opt-in capabilities. Linked instances have the capabilities
/// every instance has and the linked capabilities their records list.
pub fn hosted_enabled(record: &Instance, name: &str) -> bool {
    let home = is_home(record);
    match find(name) {
        None => home,
        Some(e) if e.availability == Availability::HomeOnly => home,
        Some(e) if home => !e.opt_in || record.capabilities.contains_key(name),
        Some(e) => e.always || (e.linked && record.capabilities.contains_key(name)),
    }
}

/// The placements a linked instance's record may give a capability: `own_worker` beside
/// `inline` only where the entry offers it and there is a build of its Worker for a linked
/// account (`instances::plan::LINKED_WORKERS`, M5). There is none yet, so `inline` alone for
/// every entry (docs/plans/capability-scoping.md, M5).
pub fn linked_placements(entry: &Entry) -> &'static [Placement] {
    let built = crate::instances::plan::LINKED_WORKERS
        .iter()
        .any(|w| w.capability == entry.name);
    if built && entry.placements.contains(&Placement::OwnWorker) {
        EITHER
    } else {
        INLINE
    }
}

/// Where capability `name` runs for `record`, and in which Worker when that is one of its own:
/// on home what is deployed (the catalogue's `home` and `home_worker`; the record cannot move
/// it); on a linked instance what its record says, `inline` unless set, inside the instance's
/// own Worker. Cloud Gateway's linked setup status is served in core. `None` for a name
/// the catalogue does not list.
pub fn placement(record: &Instance, name: &str) -> Option<(Placement, Option<String>)> {
    let e = find(name)?;
    if is_home(record) {
        return Some((e.home, e.home_worker.map(str::to_string)));
    }
    let p = record
        .capabilities
        .get(name)
        .and_then(|s| s.placement)
        .unwrap_or(Placement::Inline);
    Some(match p {
        Placement::Inline
            if matches!(
                name,
                crate::cloud_scope::NAME
                    | crate::digitalocean::NAME
                    | crate::federation::NAME
                    | JOBS
            ) =>
        {
            (p, Some("pigeon-platform".into()))
        }
        Placement::Inline => (p, record.worker.clone()),
        Placement::OwnWorker => (p, Some(own_worker_name(&record.id, name))),
    })
}

/// Where each catalogue capability enabled on `record` runs, for the console's Capabilities
/// view: `{name: {placement, worker, read_only, choices}}`. On home every entry, read-only
/// (what is deployed); on a linked instance the ones every instance has and those its record
/// lists, with the placements its record may give each (`inline` alone today).
pub fn placements(record: &Instance) -> serde_json::Value {
    let home = is_home(record);
    let mut out = serde_json::Map::new();
    for e in CATALOGUE {
        if !hosted_enabled(record, e.name) {
            continue;
        }
        if let Some((p, worker)) = placement(record, e.name) {
            out.insert(
                e.name.to_string(),
                serde_json::json!({
                    "placement": p,
                    "worker": worker,
                    "read_only": home,
                    "choices": if home { &[][..] } else { linked_placements(e) },
                }),
            );
        }
    }
    serde_json::Value::Object(out)
}

/// The name of capability `name`'s own Worker in instance `id`'s linked account:
/// `<instance>-<capability>`, with any `/` of the id as `-` (M5).
pub fn own_worker_name(id: &str, name: &str) -> String {
    format!("{}-{name}", id.replace('/', "-"))
}

/// What is wrong with `name`'s `placement` on `record`: one the catalogue does not offer; on
/// home, any but what is deployed (read-only); on a linked instance, any it may not take yet.
fn placement_problem(record: &Instance, e: &Entry, placement: Placement) -> Option<String> {
    let name = e.name;
    let p = placement.as_str();
    if !e.placements.contains(&placement) {
        let offered: Vec<&str> = e.placements.iter().map(|p| p.as_str()).collect();
        return Some(format!(
            "The capability \"{name}\" cannot run {p}: it offers {}.",
            offered.join(" or ")
        ));
    }
    if is_home(record) {
        return (placement != e.home).then(|| {
            let runs = match e.home_worker {
                Some(w) => format!("in its own Worker, {w}"),
                None => "inline".to_string(),
            };
            format!(
                "The capability \"{name}\" runs {runs} on the home instance; its placement there \
                 is read-only, and moving it is a code landing, not a record change."
            )
        });
    }
    (!linked_placements(e).contains(&placement)).then(|| {
        format!(
            "The capability \"{name}\" cannot run {p} on a linked instance yet: only inline is \
             accepted."
        )
    })
}

/// What is wrong with the catalogue capabilities `record` lists, for record validation: a name
/// the catalogue does not know, one every instance has, a home-only one on a record that is not
/// home, one a linked instance may not enable yet, one without the capabilities it requires, or
/// a placement it may not have ([`placement_problem`]).
pub fn problems(record: &Instance) -> Vec<String> {
    let home = is_home(record);
    let mut out = Vec::new();
    for name in record.capabilities.keys() {
        let Some(e) = find(name) else {
            out.push(format!(
                "The capability \"{name}\" is not one pigeon knows."
            ));
            continue;
        };
        if e.always {
            out.push(format!(
                "The capability \"{name}\" is on every instance; leave it out of capabilities."
            ));
            continue;
        }
        if e.availability == Availability::HomeOnly && !home {
            out.push(format!(
                "The capability \"{name}\" is home-only: only the home instance may enable it."
            ));
            continue;
        }
        if !home && !e.linked {
            out.push(format!(
                "The capability \"{name}\" cannot be enabled on a linked instance yet."
            ));
        }
        if let Some(p) = record.capabilities[name].placement
            && let Some(why) = placement_problem(record, e, p)
        {
            out.push(why);
        }
        match (name.as_str(), record.capabilities[name].team_id.as_deref()) {
            (crate::digitalocean::NAME, Some(id))
                if pigeon_cloud::compute::is_digitalocean_team_id(id) => {}
            (crate::digitalocean::NAME, _) => {
                out.push("DigitalOcean requires a valid team_id.".into());
            }
            (_, Some(_)) => {
                out.push(format!(
                    "The capability \"{name}\" does not accept a team_id."
                ));
            }
            _ => {}
        }
        if let Some(spaces) = &record.capabilities[name].spaces {
            if name != crate::digitalocean::NAME {
                out.push(format!(
                    "The capability \"{name}\" does not accept a Spaces binding."
                ));
            } else if let Err(why) = spaces.validate() {
                out.push(why.into());
            }
        }
        if name != crate::digitalocean::NAME && record.capabilities[name].droplet_health {
            out.push(format!(
                "The capability \"{name}\" does not accept Droplet health checks."
            ));
        }
        let grants = &record.capabilities[name].federation;
        if !crate::federation::valid_grants(record, name, grants) {
            out.push(
                "MCP Federation requires distinct peers, people, apps and approved read tools."
                    .into(),
            );
        }
        out.extend(crate::data_store::field_problems(
            name,
            &record.capabilities[name],
        ));
        out.extend(crate::email::field_problems(
            record,
            name,
            &record.capabilities[name],
        ));
        for r in e.requires {
            if !record.capabilities.contains_key(*r) {
                out.push(format!(
                    "The capability \"{name}\" needs \"{r}\" enabled beside it."
                ));
            }
        }
    }
    out
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

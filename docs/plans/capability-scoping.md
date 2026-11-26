# Capability scoping: every console view comes from a capability on the instance

Status: **decided** (2026-11-26); M1 built. Code references are to main at the time of writing.

## Goal

Every console view comes from a capability enabled on the selected instance. The hard-coded
"Platform-wide" group (`console::sections::Group::Platform`) goes away as a rule: the home
instance is not special in the navigation, it simply has more capabilities enabled, including
ones only it may use. This also fixes a confusing sidebar where the home instance's title
("pigeon platform") sat above a "Platform-wide" heading.

The navigation becomes: Inbox, then the selected instance's areas in the fixed `AREAS` order.
An area shows only while an enabled capability fills it (Overview and Capabilities always
show), and there is no group heading. Area ids, keys and URLs stay as they are.

## The model

Each capability in the catalogue (`src/capabilities/src/catalogue.rs`) declares:

- **placement**: where it runs for an instance. `inline` (inside the Worker that already serves
  the instance: `pigeon-platform` on home, the instance's own Worker on a linked account) or
  `own_worker` (a Worker of its own), plus whether the capability offers that choice at all.
- **scope**: `instance` (its data belongs to one instance: Secrets, Cost, Sessions) or
  `platform` (it is about the platform as a whole: every instance, every person).
- **availability**: `any` (any instance may enable it) or `home_only` (only the home instance).

| Capability | Views | Placement options (home today) | Scope | Availability |
|---|---|---|---|---|
| `ai-agents` | Sessions › Live, Compare | inline or own (home: own, `pigeon-ai-agents`) | instance | any |
| `observability` | Cost › 7 days, Today, 30 days (and Metrics › Workers) | inline | instance | any |
| `secrets` | Access › Secrets | inline | instance | any |
| `proxy` (requires `secrets`) | Access › Tokens | inline or own (home: own, `pigeon-proxy`) | instance | any |
| `models` | Platform › Models | inline or own (home: own, `pigeon-model-gateway`) | instance | any |
| `multi-instance` (new, split out of `instances`) | Instances › All instances, Platform › Endpoints, creating and linking instances | inline | platform | **home_only** |
| `users` | Access › People, Apps | inline | platform | **home_only** |
| `architecture` | Platform › Architecture | inline | instance | any |
| `instances` | none of its own: every instance's record, plan and apply | inline | instance | any |

The instance capabilities the record enables by its own fields (`publishing`, `site_passkeys`,
`storage`, `source`, `ci`, `observability`'s Metrics, `ingest`) keep working as they do: their
enablement is the record field, their views come from `instances::tools::CAPABILITIES`.

### Decisions

1. **`users` stays home-only for now.** Identity is platform-level: one directory of people and
   passkeys. Per-instance membership is a later phase (below), not a per-instance `users`.
2. **Platform › Architecture shows the selected instance's own setup.** On home it looks as
   today: the platform's Workers, Durable Objects, containers, routes and tools, and the
   drawing. On a linked instance it shows that instance's Worker (or site), bucket, hostnames,
   enabled capabilities and the tools its scoped MCP endpoint offers. `/architecture.json` on
   pigeon.dev is the public model and is untouched (deploys check its `commit`). The diagram's
   look is being redesigned separately; this changes only which data is shown.

## Today (before M1)

- `console::sections::AREAS` gives each area a `Group`: `top` (Inbox), `instance` (Overview to
  Capabilities) or `platform` (Sessions, Cost, Access, Instances, Platform).
- `instances::manifest::manifest` adds hosted capabilities' sections only on the home instance
  and only in `Group::Platform` areas. A linked instance never sees them.
- `shell.js` puts a heading with the instance's title over the instance group and a
  "Platform-wide" heading over the platform group; the page head says "Platform-wide" too.
- iOS (`ios/Pigeon/Navigation/ManifestStore.swift`) decodes each area's `group`; only
  `OverviewScreen` reads it (to list the instance's own areas).

## Phases

### M1: catalogue fields, and the manifest by capability (no visible change but the headings)

- `catalogue.rs`: `Placement`, `Scope`, `Availability` and one `Entry` per row above, with the
  placement options, today's placement on home and its Worker, `requires`. Inert: nothing
  enforces them yet. `capabilities_versions` answers the catalogue beside the components.
- `console_manifest` derives the areas from the capabilities enabled on the instance: the
  record's own capabilities (its `Scope`), plus the hosted capabilities the catalogue says are
  enabled there. On home that is every hosted capability (home runs the platform); on a linked
  instance none yet. A hosted capability the catalogue does not list is treated as home-only.
  `Group` no longer decides anything; the manifest still sends each area's `group` so the iOS
  app and older consoles keep decoding it.
- `shell.js`: no group headings in the sidebar; the page head shows the instance's title as the
  crumb on every area and no "Platform-wide" label.
- Platform › Architecture is instance-scoped: `architecture_describe {instance}` on a linked
  instance answers its own architecture (Worker, bucket, hostnames, enabled capabilities,
  scoped tools), and the view passes the selected instance. Home is unchanged.
- Tests pin the area and view lists per instance kind (home, linked publishing, bare).

Because Platform › Architecture is not yet enabled on a linked instance, the scoped view is
reachable there only once architecture is enabled on it (see M4). The op answers it already.

### M2: `multi-instance` split out of `instances`; `home_only` enforced

- `multi-instance` is its own capability: Instances › All instances, Platform › Endpoints, and
  creating instances (`instance_propose` of a record with a new id) and linking them
  (`/linked/begin`). `instances` keeps what every instance needs: its record, `instances_list`,
  `console_manifest`, `instance_plan` and `instance_apply`, retire.
- The record gains `capabilities`: the catalogue capabilities it enables beyond its profile
  fields, by name, each with its settings (none yet; `placement` in M3). Absent when empty, so
  no stored record's digest changes.
- Record validation refuses an unknown name, a `home_only` capability on a record that is not
  home (`multi-instance`, `users`), and, until M4 turns each on, any catalogue capability on a
  linked record. `requires` is checked (`proxy` needs `secrets`).
- The manifest never shows a `home_only` capability's views on another instance, whatever the
  record says.

### M3: placement in the record

- `capabilities.<name>.placement` (`inline` or `own_worker`, and the Worker for `own_worker`).
- On home it is read-only and must match the catalogue's home placement (today's deployment:
  `pigeon-ai-agents`, `pigeon-proxy`, `pigeon-model-gateway`); the console shows it, the
  record cannot move it (moving a home capability is a code landing, not a record change).
- On a linked instance only `inline` is accepted.
- The Capabilities view lists the catalogue's capabilities with their placement, scope and
  availability, and Architecture draws the placement.

### M4: per-capability enablement on linked instances, one at a time

Each step turns the catalogue's `linked` flag on for one capability, adds what the instance's
Worker needs to serve it inline, and adds the linked-account OAuth scopes it needs to
`linked::PERMISSIONS`. **The person approves each scope change** (a new consent on every linked
account), so each step is its own landing with its own decision:

1. `architecture` (needs no scope: it reads only the record) — can ride along with step 1.
2. `secrets`: the instance's own Secrets Store (Secrets Store Write on the linked account).
3. `observability` Cost: the instance account's usage (Account Analytics Read, Billing Read).
4. `proxy` and Access › Tokens: proxy credentials scoped to the instance (requires `secrets`).
5. `models`: an AI Gateway in the instance's account (AI Gateway Write), its own provider keys
   in its Secrets Store.
6. `ai-agents`: runs scoped to the instance, its model turns through step 5, its sandbox
   (Containers, Durable Objects: Workers Scripts Write already held).

Each step's tests: the record enables it on a linked instance, the manifest shows its views
there with the instance's data only, and the ops refuse another instance's data.

### M5: `own_worker` placement on linked instances

`instance_plan`/`instance_apply` plan and upload a capability's own Worker in the linked
account (`<instance>-<capability>`), bind it to the instance Worker, and record it in the
ledger as pigeon's, so retire removes it. Only capabilities whose catalogue entry offers the
choice (`ai-agents`, `proxy`, `models`).

### Later: membership

Today any signed-in person can approve any change on any instance: identity is platform-level
and there is no notion of who belongs to which instance. Membership adds per-instance roles —
`owner` (approves record changes, links, retires), `operator` (approves deploys and runs),
`viewer` (reads) — kept on the platform with identity staying platform-level (the `users`
directory and passkeys stay home-only). The approval paths (`console::write`, `approvals`)
check the approver's role on the instance the change is about; the manifest hides an instance
from a person with no role on it. This closes the gap that any signed-in person can approve a
change on any instance.

## Log

- 2026-11-26: planned.
- 2026-11-26: M1 built. Until M2 splits it, the catalogue lists `instances` (which still
  declares All instances and Endpoints) as platform-scoped and home-only. A hosted capability's
  view may now sit in any area but Inbox, not only the formerly platform-wide ones; no hosted
  capability declares one elsewhere today, so no list changed. Architecture on a linked
  instance answers `instance_model`: its Worker, hostnames, bucket (`resources.buckets`, absent
  from the platform's model), zone, repository, enabled capabilities and scoped tools, with the
  platform's commit and digest, drawn by the same `diagram`; `instance_tools` is kept.
  `capabilities_versions` answers `catalogue`. The recorded manifests now name each view's
  real capability (`ai-agents`, `observability`), which the shell contract checks.

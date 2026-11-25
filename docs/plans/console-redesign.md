# Console redesign

Status: planned 2026-11-25. Phase 1 (the manifest contract) is on branch
`claude/console-manifest`; phases 2 and 3 are being built in parallel on their own branches;
phases 4 and 5 not started. Nothing is deployed.

Design: the approved prototype and spec, revision r5 (`console-redesign-r5.html`, a static page
made in a session; its "Spec & decisions" section is the source of the decisions below). Once
this plan exists it wins where the two differ ([README](README.md)).

This plan replaces [console.md](console.md) and [console-hardening.md](console-hardening.md),
which stay as the record of what they built. When it is done, no code from the old console
remains in the repository or in any deployed Worker: the [decommission checklist](#decommission-checklist)
names every piece and what replaces it.

## Why

The console at `platform.pigeon.dev/app` grew in three layers: the first console's panels
(`console/{approvals,instances,keys,users}.js` with `console.css`, mounted through
`management.js` and the `window.pigeonConsole` shim), the `/app` shell with one script and
stylesheet per view, and a read-only Settings view that repeats what the panels show. Tabs are
hard-coded twice (the `Tab` enum in `console/sections.rs`, and `coreTabs`, `linkedTabs` and
`settingsTabs` in `shell.js`), a linked instance gets a placeholder Home, and each write has its
own route and client code. The instance MCP endpoint already knows, from the approved record,
which capabilities an instance has; the console guesses separately.

The redesign makes the approved instance record the one input (M1), computes the navigation
from it (D1, M3), gives every write one path and every read one path (P10, P11), and deletes
everything else (P17).

## Decisions

Accepted (r5, with the person's decisions of 2026-11-25):

| Id | Decision |
| --- | --- |
| D1 | The navigation is computed per selected instance. Inbox first; then the instance group: Overview, one area per enabled capability (Site, Storage, Source, CI, Metrics, Ingest), Capabilities; then, on the home instance only, the platform group: Sessions, Cost, Access, Instances, Platform. |
| D2 | One pink accent (`#E07FA8` dark, `#B03A6A` light); status colours are separate. |
| M1 | The approved instance record is the single source of truth for which capabilities are enabled (their profile fields) and at what version (`pins`). The console only reads it, through the same `Scope` that drives the instance MCP endpoint. |
| M2 | One way to change an instance: Instance › Capabilities offers Enable, Configure, Upgrade and Disable, each one record diff applied with a passkey (today `/instance/put`); agents propose the same diff with `instance_propose`, it waits in Inbox, and `instance_apply` applies it. Overview edits only non-capability fields. |
| M3 | Each instance capability's registration returns console views next to its tool `Registration`s, collected by `Scope::new`. Platform capabilities declare views through `CapabilityManifest.sections`, whose `tab` field is an area id. One read-only `console_manifest(instance)` replaces `instances_tabs` and the shell's tab lists. |
| M4 | A view newer than the instance's selected capability version stays in the navigation and opens on an upgrade notice whose button is the Capabilities Upgrade action. |
| M5 | Switching to an instance that lacks the current area opens its Overview and says why in a toast. |
| M6 | Inbox is on every instance, filtered to that instance's requests; on the home instance it shows every request, with an Instance column. |
| C1–C7 | Cuts from r1: no read-only duplicates; form plus diff record editing; Compare without model defaults; no disabled placeholders; Sessions keeps only what waits on the person; Approvals as one table; one-hop redirects. |
| P1 | The instance switcher at the top of the sidebar is the only way to pick an instance (`o` opens it). All instances is a platform area on the home instance. |
| P2 | Every write uses one confirm dialog that names what changes, then asks for the passkey in place. |
| P3 | One reveal-once component for proxy tokens, person invites and visitor invites. |
| P4 | Destructive actions confirm in that dialog with a danger button; click-twice goes. |
| P5 | Approvals is renamed Inbox. Landings get Approve only (no deny route exists). |
| P6 | Compare's charts fold into its table as inline bars. |
| P7 | Site Access becomes Site › Visitors, added by `site_passkeys` (Q4). Visitors load with the view. |
| P8 | Apple signing actions stay in Access › Secrets, shown only when `APPLE_ASC_KEY` is set. |
| P9 | The linked-account Home placeholder goes; a linked instance opens on Overview. |
| P10 | One write path: every passkey write is `POST /console/write/<action>/{begin,finish}` with `{target}`, called by one client function behind the confirm dialog. |
| P11 | One read path: views read only through `POST /app/op` and its allowlist (`console/web/ops.rs`). |
| P12 | Stable ids: area and view ids from the manifest form the URL (`/app?instance=&area=&view=`). A shipped id never changes; a rename ships with a redirect. |
| P13 | One stylesheet and one component set (DataTable, StatusPill, Dialog, RevealOnce, Facts) in `shell.css` and `shell.js`. **Deferred:** the CI lint that rejects inline styles and literal colours in views. |
| P14 | Fixture mode: every view renders from recorded fixtures (loaded, empty, error) without sign-in. **Deferred:** the Playwright light and dark screenshots in CI. |
| P15 | A contract test: every manifest view has a registered renderer, every op it reads is on the allowlist, every write action has a route. |
| P16 | `console_manifest` answers data only, never HTML, so the iOS app can build the same navigation. |
| P17 | Delete what the manifest replaces (see the checklist). |
| Q1 | Site gets a Deployments view, added by publishing, with Rollback. |
| Q2 | Pin upgrades use the record write; they need no new route. |
| Q3 | Platform areas are reachable only from the home instance, for now. |
| Q4 | `site_passkeys` gets its own profile field on the record; existing `sites[].auth = "passkey"` values migrate into it, and publishing stays its prerequisite. |
| N1 | To build: a console write for model defaults (`ai_agents_config`: defaults by role, planner by kind), passkey-approved; Platform › Models shows Edit against it. |

## The manifest (phase 1)

### Areas

Fixed in `console::sections::AREAS`, in navigation order. Ids and keys never change (P12).

| Id | Title | Group | Key | Present when |
| --- | --- | --- | --- | --- |
| `inbox` | Inbox | top | `i` | always |
| `overview` | Overview | instance | `v` | always |
| `site` | Site | instance | `w` | publishing or site passkeys enabled |
| `storage` | Storage | instance | `b` | storage enabled |
| `source` | Source | instance | `r` | source enabled with an approved repository |
| `ci` | CI | instance | `k` | CI enabled |
| `metrics` | Metrics | instance | `m` | observability enabled with an approved Worker |
| `ingest` | Ingest | instance | `e` | ingest enabled |
| `capabilities` | Capabilities | instance | `x` | always |
| `sessions` | Sessions | platform | `s` | home instance |
| `cost` | Cost | platform | `c` | home instance |
| `access` | Access | platform | `a` | home instance |
| `instances` | Instances | platform | `n` | home instance |
| `platform` | Platform | platform | `p` | home instance |

An area other than the three fixed ones appears only while at least one view is declared in it.

### How views are declared

**Instance capabilities** declare views beside their tool registrations
(`src/capabilities/src/instances/tools.rs`). Each capability's `instance_tools(&Instance)`
returns `Enabled { tools, views }`, or `Enabled::NONE` while its profile is unset;
`CAPABILITIES` lists each `Contributor` with its manifest name and the versioned component its
pin names; `Scope::new` collects tools and views together and refuses a duplicate of either. A
`View` is `{ area, id, title, order, since }`.

| Capability | Component | Enabled by | Views |
| --- | --- | --- | --- |
| `publishing` | `sites` | `publishing` names a site (`site_publish::site`) | `site:sites` (10), `site:deployments` (20) |
| `site_passkeys` | `site_passkeys` | publishing enabled and a site with `auth = "passkey"` (Q4 moves this to its own field) | `site:visitors` (30) |
| `storage` | `storage` | `storage` enabled | `storage:buckets` |
| `source` | `source` | `source` enabled with an approved repository | `source:repository` |
| `ci` | `ci` | `ci` profile in scope | `ci:jobs` |
| `observability` | `observability` | `observability` enabled with an approved Worker | `metrics:workers` |
| `ingest` | `ingest` | `ingest` enabled | `ingest:runs` |

`site_passkeys` adds no instance tools (`site_passkey` is for pigeon-sites alone). Metrics ›
Errors (in the spec) waits for an instance-scoped errors read, so it is not declared: a view
with nothing to read would be a disabled placeholder (C4).

**Platform capabilities** declare views as console sections: `Section` locally and
`SectionSpec` on the wire (`src/contract/src/wire.rs`). `tab` is now an area id (core still maps
the old names `approvals` and `architecture` from a Worker built before areas), and the new
`view` flag says whether `console_manifest` lists it. A section with `view: false` is a legacy
panel's data anchor for `/console/api/section/{id}` only; those go in phase 5.

| Area | Views (order) | Declared by |
| --- | --- | --- |
| `sessions` | `live` (10), `compare` (20) | ai-agents (`pigeon-ai-agents`) |
| `cost` | `7d` (10), `today` (20), `all` (30) | observability (`cost_daily`) |
| `access` | `secrets` (10), `tokens` (20), `people` (30), `apps` (40) | secrets, proxy, users |
| `instances` | `list` (10) | instances |
| `platform` | `architecture` (10), `models` (20), `endpoints` (30) | architecture, models, instances |

### `console_manifest`

A read-only tool of the `instances` capability, callable over MCP and as an `/app` op
(`{ "name": "console_manifest", "args": { "instance": "…" } }`). Without `instance` it answers
the home instance.

```json
{
  "instance": { "id": "sample", "title": "Sample", "home": false, "host": "blog.example.com" },
  "areas": [
    { "id": "inbox", "title": "Inbox", "group": "top", "key": "i", "views": [] },
    { "id": "overview", "title": "Overview", "group": "instance", "key": "v", "views": [] },
    { "id": "site", "title": "Site", "group": "instance", "key": "w", "views": [
      { "id": "sites", "title": "Sites", "order": 10, "capability": "publishing", "upgrade": null },
      { "id": "deployments", "title": "Deployments", "order": 20, "capability": "publishing", "upgrade": null },
      { "id": "visitors", "title": "Visitors", "order": 30, "capability": "site_passkeys",
        "upgrade": { "selected": "1.3.2", "since": "1.4.0" } } ] },
    { "id": "capabilities", "title": "Capabilities", "group": "instance", "key": "x", "views": [] }
  ],
  "inbox": { "count": 3 }
}
```

- `host`: the first zone or domain hostname a site is published at, else the first zone; null
  when neither.
- `upgrade` (M4): set when the view's `since` is newer than the version the instance's pin
  selects for the capability's component (no pin: the newest served version, so never).
- `inbox.count` (M6): pending approvals the person may decide, plus landings awaiting approval,
  on the home instance; elsewhere only approvals about that instance (an instance draft's or
  linked plan's subject, or a call's `instance` argument). Null when its sources cannot be read,
  so the navigation never fails for a count.
- Data only: ids, titles, numbers, booleans. No HTML (P16).

Code: `src/capabilities/src/instances/manifest.rs` (pure `manifest`, `inbox_count`, `selected`,
`upgrade`; the tool in `console_manifest`); smoke tests `console_manifest_*` in
`tests/smoke/instances.rs`.

## Writes and reads today

### Write actions

| Write | Route today | Client today | Target (phase 2) |
| --- | --- | --- | --- |
| Approve a tool call, instance draft, linked plan or other plan | `/console/write/approval-approve/*` | `approvals.js` (4 renderers) | unchanged |
| Deny a request | `/console/write/approval-deny/*` | `approvals.js` | unchanged |
| Approve a landing | `/console/write/land-approve/*` | `approvals.js`, Sessions banner | unchanged |
| Landings need approval on/off | `/console/write/land-approval/*` | `approvals.js` | unchanged |
| Invite a person | `/console/write/invite/*` | `users.js` | unchanged |
| Remove a passkey | `/console/write/passkey-remove/*` | `users.js` | unchanged |
| Disconnect an app | `/console/write/app-disconnect/*` | `users.js` | unchanged |
| Set or replace a secret | `/secrets/put/{begin,finish}` (`secrets.rs`) | `keys.js` | `/console/write/secret-put/*` |
| Issue a proxy token | `/tokens/issue/{begin,finish}` (`proxy.rs`) | `keys.js` | `/console/write/token-issue/*` |
| Revoke a proxy token | `/tokens/revoke/{begin,finish}` (`proxy.rs`) | `keys.js` | `/console/write/token-revoke/*` |
| Unlink Cloudflare | `/linked/unlink/{begin,finish}` (`linked.rs`) | `instances.js` | `/console/write/linked-unlink/*` |
| Put an instance record (enable, configure, upgrade, disable, edit) | `/instance/put/{begin,finish}` (`instances.rs`) | `instances.js` raw JSON editor | `/console/write/instance-put/*` |
| Invite a site visitor | `/console/site-access/invite/{begin,finish}` | `site_passkeys/visitor.js` | `/console/write/site-invite/*` |
| Model defaults (N1) | none | none | `/console/write/model-defaults/*` (new) |
| Stop, note or extend a run | `/app/op` (`ai_agents_stop`, `_note`, `_extend`, gated by tool approval) | `sessions.js` | unchanged (op, not a passkey write) |
| Link or relink Cloudflare | `/linked/begin` (an OAuth redirect, no passkey) | `instances.js`, `connections.js` | stays its own route: it starts OAuth, it changes nothing |

Sign-in, sign-out (`/console/sign-in/*`, `/console/sign-out`) and the enrolment pages
(`/auth/enrol`, `/auth/link`, `/auth/passkey.js`) are not console writes and stay.

### Read ops

Through `/app/op` today (`console/web/ops.rs`): `ai_agents_status`, `land_list`, `cost_daily`,
`capabilities_versions`, `capabilities_upgrade_plan`, `app_linked_details`,
`architecture_describe`, `ai_agents_compare`, `tokens_status`, `oauth_clients`, `users_people`,
`secrets_list`, `instances_list`, `ai_agents_models`, `users_whoami`, and now
`console_manifest`.

Outside it today, to move onto it in phase 2: `/console/api/section/{approvals, people,
delegation, credentials, instances, cost, platform, ai-agents}` (legacy panels and
`management.js`), `/console/site-access/{hosts,list}` (visitor.js), and the instance area reads
phase 4 needs: `approvals_list` (Inbox), `storage_list_buckets`/`_objects`/`_get_object`,
`source_resolve_revision`/`_read_file`, `ci_jobs`/`ci_status`, `observability_worker_metrics`,
`ingest_status`/`_report`, and the Sites deployment history for Deployments. Each instance op
binds to the selected instance through the instance's `Scope` (`bind`), as on the instance MCP
endpoint.

## Phases

1. **Manifest** (`claude/console-manifest`). Views beside tool registrations; areas replace the
   `Tab` enum; `SectionSpec.tab` is an area id with `view`; `console_manifest` as tool and op;
   smoke tests. `instances_tabs` and `/console/api/section` keep working.
2. **One write path and one read path** (parallel branch). `console/write.rs` holds every
   passkey write (the table above), each a `{begin, finish}` pair bound by purpose to its
   target, and `console.rs` routes `/console/write/<action>/*` only. `/app/op` is the only read
   path; its allowlist covers every op a view reads. The old write routes stay as thin aliases
   until phase 5.
3. **Shell, components, fixtures** (parallel branch). New `shell.js`/`shell.css` building the
   navigation from `console_manifest`: instance switcher (P1), area/view routing on
   `/app?instance=&area=&view=`, M5 toast, upgrade notice (M4); the component set (P13); one
   confirm dialog (P2, P4) and RevealOnce (P3); fixture mode (P14, without CI screenshots); the
   P15 contract test.
4. **Port the views**, area by area, each from fixtures first: Inbox (one table, landings
   Approve only, Instance column on home); Overview (facts, link/unlink, form-plus-diff editor
   for non-capability fields); Capabilities (Enable, Configure, Upgrade, Disable as record
   diffs; agents via `instance_propose`); Site › Sites, **Site › Deployments with Rollback**
   (`site_rollback` behind the confirm dialog) and **Site › Visitors** (list and invite, with
   RevealOnce); Storage › Buckets; Source › Repository; CI › Jobs; Metrics › Workers; Ingest ›
   Runs; Sessions › Live and Compare; Cost ranges; Access › Secrets (Apple actions, P8), Tokens,
   People, Apps; Instances › All instances; Platform › Architecture, Models (with N1's Edit),
   Endpoints. The iOS app moves from `instances_tabs` to `console_manifest`. Q4's profile field
   and its migration land here, and `site_passkeys::instance_views` reads it.
5. **Delete the legacy console**: the checklist below, in one landing per row group, each with
   its replacement already live.

## Decommission checklist

Done means each row is gone from the repository and from every deployed Worker, and a search for
its name finds nothing but this plan's record.

### Files

| Delete | Replaced by |
| --- | --- |
| `src/capabilities/src/console/approvals.js` | Inbox view |
| `src/capabilities/src/console/instances.js` | Overview and Capabilities views, Instances › All instances |
| `src/capabilities/src/console/keys.js` | Access › Secrets and Tokens views |
| `src/capabilities/src/console/users.js` | Access › People and Apps views |
| `src/capabilities/src/console/console.css` | `shell.css` tokens and components |
| `src/capabilities/src/console/web/management.js` and `management.css` (with the `window.pigeonConsole` shim it defines) | the views above; `window.pigeonApp` components |
| `src/capabilities/src/console/web/settings.js`, `settings.css` (read-only Settings: Access, Models, People panels) | Access views, Platform › Models |
| `src/capabilities/src/console/web/connections.js`, `connections.css` (Settings › Connections and Capabilities, `linked-home`, `linked-settings`) | Overview, Capabilities, Platform › Endpoints |
| `src/capabilities/src/console/web/{sessions,cost,architecture}.{js,css}` in their current form | the phase 4 views on the new components (no per-view stylesheets, P13) |
| `src/capabilities/src/site_passkeys/visitor.js` and its `ASSET` entry | Site › Visitors |
| `src/capabilities/src/console/snapshot.rs` (snapshots keyed by legacy section ids), unless a phase 4 view keeps snapshots per op | op-level caching where a view needs it |

### Rust

| Delete | Replaced by |
| --- | --- |
| `GET /console/api/section/{id}` (`Get::Section`, `Console::section`, `Console::snapshot`, `stamped`, `SECTION_MS` users outside ops) | `/app/op` reads |
| `Capability::section_data` and every implementation (approvals, land, users, instances, secrets, proxy, models, observability, remote) | the tools the views call through `/app/op` |
| Sections with `view: false` (`approvals`, `delegation`, `credentials`, `instances`, `cost`, `platform`, `ai-agents`) | the view sections |
| The `section` contract op (`OPS`), `SectionRequest`, and the ai-agents and models `section` handlers (the ai-agents one answers HTML, `page::section`) | ops; data only (P16) |
| `SectionSpec.tab` read as an old tab name (`sections::area_of_tab`'s mapping); then rename the field to `area` in a contract version | area ids only |
| `console/sections.rs` `anchor` once sections are only views (keep the id check in the P15 test) | `AREAS` and view ids |
| `instances_tabs` tool and `instances::tabs` (after the iOS app moves) | `console_manifest` |
| Old write routes: `/secrets/put/*`, `/tokens/issue/*`, `/tokens/revoke/*`, `/linked/unlink/*`, `/instance/put/*`, `/console/site-access/{invite/*,hosts,list}` and `site_passkeys::console::Action` | `/console/write/<action>/*`, `/app/op` |
| `Get::Asset` entries for `/console/{keys,users,approvals,instances}.js`, `/console/console.css`; `app_shell`'s legacy `<link>`/`<script>` tags and the `app_assets!` entries of deleted views | the new shell's asset list |
| `page::CREDENTIALS` and `GET /secrets`, `GET /tokens` redirects to it | redirects below |
| `instances_section`'s legacy anchor and `keys_section` | view sections |

The `Tab` enum and `sections::moved` were removed in phase 1.

### Shell

| Delete | Replaced by |
| --- | --- |
| `coreTabs`, `linkedTabs`, `settingsTabs`, `workspaceTabs` and `register`/`registerSettingsPanel` keyed by tab in `shell.js` | navigation from `console_manifest`; renderers keyed by `area:view` |
| `tab`/`subtab` URL parameters and the `#hash` → subtab mapping | `area`/`view` (P12), with a one-time mapping of old `/app?tab=…` links |
| The `pigeon.cost.period` local key tied to `subtab` | the Cost view's own state |

### Redirects

Keep, retargeted to one hop (C7): `/` → `/app`; `/approvals` → `/app?area=inbox`;
`/instances` → `/app?area=instances`; `/architecture` → `/app?area=platform&view=architecture`;
`/access`, `/settings` → `/app?area=access`; `/secrets[#NAME]` →
`/app?area=access&view=secrets[#NAME]`; `/tokens` → `/app?area=access&view=tokens`; `/cost`,
`/gateways` → `/app?area=cost`; old `/app?tab=…&subtab=…` links → their area and view.
Remove: `/app?tab=management&subtab=credentials` as a target, and the `/instances?i=` query
carry-over once `/app?instance=` is the only form. Keep the console's routes list
(`Console::routes`) to exactly the paths above.

### Tools, tests and docs

| Item | Action |
| --- | --- |
| `instances_tabs` | delete with the tool (above); remove from `docs/plans/mcp-tool-audit.md` and `ios-app-next.md`'s current-state text |
| iOS `ios/Pigeon/Views/TabBar.swift` (`instances_tabs`) | call `console_manifest` |
| `tests/smoke/instances.rs` `instances_tabs_for_the_app`; `tests/smoke/core.rs` `core_console_platform_section` assertions on legacy anchors and `?tab=` redirects | replaced by the manifest, contract (P15) and redirect tests |
| [console.md](console.md), [console-hardening.md](console-hardening.md) | marked superseded by this plan (done in phase 1); kept as the record |
| [README.md](README.md) | this plan's row updated as phases land |

## Log

- 2026-11-25: plan written. Phase 1 built on `claude/console-manifest`: `Enabled`/`View` in
  `instances/tools.rs`, `AREAS` in `console/sections.rs` (the `Tab` enum and `moved` removed),
  `SectionSpec.view`, `console_manifest`, six smoke tests. Departures from the spec: Metrics ›
  Errors is not declared (no instance-scoped read yet); Site lists Sites before Deployments and
  Visitors; Access › Apps is a view over the users section's data, which the legacy Delegation
  panel also reads; no view declares `since` yet, as every component is at 1.0.0.

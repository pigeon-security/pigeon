# Console redesign

Status: phases 1 to 5 built (2026-11-25). Phases 1 to 4 landed on main; phase 5, the
decommission, is on `claude/console-decommission`, done pending deploy verification (the live
checks are in the Log). The console is the shell at `/app/<instance>/<area>/<view>`.

Design: the approved prototype and spec, revision r5 (`console-redesign-r5.html`, a static page
made in a session; its "Spec & decisions" section is the source of the decisions below). Once
this plan exists it wins where the two differ ([README](README.md)).

This plan replaces the first console's two plans, `console.md` and `console-hardening.md`,
deleted with its code ([Archive](#archive) keeps their record). When it is done, no code from
the old console remains in the repository or in any deployed Worker: the
[decommission checklist](#decommission-checklist) names every piece and what replaces it.

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

### Bearer parity (the iOS app)

The person decided the rebuilt iOS app (the approved iPhone mock-up, its Spec's "Ops each
screen uses") uses the same reads, the same write path and the same passkey binding as the
web. The app holds an OAuth access token acting for a person, not a session cookie.

- **Reads:** `POST /app/op` with `Authorization: Bearer <token>`,
  `Content-Type: application/json`, body `{ "name", "args" }`. The person is the one the
  token acts for (`oauth::caller` → `console::app_person`: `acting_for`, a standard user only;
  an app acting for no one gets 403, an unknown or expired token 401). The op then runs exactly
  as that person's session would: the one `READS` allowlist with its exact arguments, the
  `INSTANCE_OPS` binding to the instance's approved record (`bind_global`), `dispatch` as the
  person, the 10-second timeout. Answers `200 {data}`, `403` for an op or argument not
  allowed, `400 {error}`, `504 {error, retry: true}`. No op or argument was added; the app's
  `ios_releases` is not a console read and stays on MCP. `instances_list` and
  `console_manifest` (`{instance}`) are reachable this way.
- **Writes:** `POST /console/write/<action>/{begin,finish}` with the same bearer and body
  `{target, change}` (finish adds the passkey's answer, as from the web). Allowed by bearer
  (`write::app`): `secret-set`, `token-issue`, `token-revoke`, `linked-unlink`,
  `instance-put`, `visitor-invite`, `visitor-revoke`, `passkey-remove`, `app-disconnect`,
  `invite`, `land-approve`, `approval-approve`, `approval-deny`, `model-defaults`,
  `agents-config`, `site-rollback`, `site-deploy`, `run-stop`, `run-note`, `run-extend`. The
  purpose is the session path's: the person's id, the action, the target and the change's
  digest (and the approval's own purpose for `approval-approve`), answered only by that
  person's passkey, once, within the challenge's lifetime; tool writes meet the person's
  `call_tool` policy first. Not by bearer: `land-approval`, `apple-adopt`, `apple-identity`
  (the app does not make them); they answer 403 to a bearer.
- **CSRF:** a bearer counts only on `/app/op` and the write routes (`Post::takes_bearer`);
  there the cookie is not read at all, so a bad bearer is refused and never falls back to the
  session. Only then is the Origin check skipped (`session::admitted`): a browser never sends
  an `Authorization` header by itself and the console answers no CORS preflight. A request
  without a bearer still needs `Origin: https://<host>`, and every POST still needs JSON.
  Signing in and out never take a bearer.

Smoke: `console_app_bearer_parity`, `console_write_actions_each_have_a_handler_and_policy`,
`platform_approvals_list_and_bearer_writes`.

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
5. **Delete the legacy console**: the checklist below, each row with its replacement already
   live. Done on `claude/console-decommission`, pending deploy verification.

## Decommission checklist

Done means each row is gone from the repository and from every deployed Worker, and a search for
its name finds nothing but this plan's record. Every row is done on `claude/console-decommission`
(repository side); `console_decommissioned` in `tests/smoke/console.rs` fails if a deleted file
comes back, if a source under `src/` or `ios/` names a removed route, panel or tool, or if an old
bookmark is anything but one redirect to a console page or a 404. The deployed side is checked
after deploy (Log).

### Files

| Delete | Replaced by | Status |
| --- | --- | --- |
| `src/capabilities/src/console/approvals.js` | Inbox view | Done |
| `src/capabilities/src/console/instances.js` | Overview and Capabilities views, Instances › All instances | Done |
| `src/capabilities/src/console/keys.js` | Access › Secrets and Tokens views | Done |
| `src/capabilities/src/console/users.js` | Access › People and Apps views | Done |
| `src/capabilities/src/console/console.css` | `shell.css` tokens and components | Done |
| `src/capabilities/src/console/web/management.js` and `management.css` (with the `window.pigeonConsole` shim it defines) | the views above; `window.pigeonApp` components | Done |
| `src/capabilities/src/console/web/settings.js`, `settings.css` (read-only Settings: Access, Models, People panels) | Access views, Platform › Models | Done |
| `src/capabilities/src/console/web/connections.js`, `connections.css` (Settings › Connections and Capabilities, `linked-home`, `linked-settings`) | Overview, Capabilities, Platform › Endpoints | Done |
| `src/capabilities/src/console/web/{sessions,cost,architecture}.{js,css}` in their current form | the phase 4 views on the new components (no per-view stylesheets, P13) | Done |
| `src/capabilities/src/site_passkeys/visitor.js` and its `ASSET` entry | Site › Visitors | Done |
| `src/capabilities/src/console/web/legacy-shell.{js,css}` (the old `shell.js`, served at `/app/legacy-shell.*` while the new shell is at `/app/next`) | `shell.{js,css}` and `views.js` served at `/app` | Done |
| `todo(...)` placeholders in `web/views.js`, `legacyLink` and the TODO-port renderer in `shell.js`, and `CONSOLE_TODO_PORT` in `tests/smoke/console.rs` | ported renderers; the list is empty | Done |
| `src/capabilities/src/console/snapshot.rs` (snapshots keyed by legacy section ids), unless a phase 4 view keeps snapshots per op | op-level caching where a view needs it | Done |

### Rust

| Delete | Replaced by | Status |
| --- | --- | --- |
| `GET /console/api/section/{id}` (`Get::Section`, `Console::section`, `Console::snapshot`, `stamped`, `SECTION_MS` users outside ops) | `/app/op` reads | Done |
| `Capability::section_data` and every implementation (approvals, land, users, instances, secrets, proxy, models, observability, remote) | the tools the views call through `/app/op` | Done |
| Sections with `view: false` (`approvals`, `delegation`, `credentials`, `instances`, `cost`, `platform`, `ai-agents`) | the view sections | Done |
| The `section` contract op (`OPS`), `SectionRequest`, and the ai-agents and models `section` handlers (the ai-agents one answers HTML, `page::section`) | ops; data only (P16) | Done |
| `SectionSpec.tab` read as an old tab name (`sections::area_of_tab`'s mapping); then rename the field to `area` in a contract version | area ids only | Done in core: only area ids are read, and only sections with `view` set. The wire fields `tab`, `cost` and `view` stay until a contract version renames or drops them |
| `console/sections.rs` `anchor` once sections are only views (keep the id check in the P15 test) | `AREAS` and view ids | Done |
| `instances_tabs` tool and `instances::tabs` (after the iOS app moves) | `console_manifest` | Done |
| Old write routes: `/secrets/put/*`, `/tokens/issue/*`, `/tokens/revoke/*`, `/linked/unlink/*`, `/instance/put/*`, `/console/site-access/{invite/*,hosts,list}` and `site_passkeys::console::Action` | `/console/write/<action>/*`, `/app/op` | Done |
| `Get::Asset` entries for `/console/{keys,users,approvals,instances}.js`, `/console/console.css`; `app_shell`'s legacy `<link>`/`<script>` tags and the `app_assets!` entries of deleted views | the new shell's asset list | Done |
| `page::CREDENTIALS` and `GET /secrets`, `GET /tokens` redirects to it | redirects below | Done |
| `page::app_shell`, `APP_ASSETS` and `Get::App`'s legacy page; then `page::NEXT_BASE`, with `/app/next/*` redirected to `/app/*` in one hop, `next_shell` served at `/app` with `data-base="/app"`, its assets at `/app/{shell.css,shell.js,views.js}` | the new shell at `/app` | Done; `/app/next` was never deployed, so it is not redirected |
| `instances_section`'s legacy anchor and `keys_section` | view sections | Done |

The `Tab` enum and `sections::moved` were removed in phase 1.

### Shell

| Delete | Replaced by | Status |
| --- | --- | --- |
| `coreTabs`, `linkedTabs`, `settingsTabs`, `workspaceTabs` and `register`/`registerSettingsPanel` keyed by tab in `shell.js` | navigation from `console_manifest`; renderers keyed by `area:view` | Done |
| `tab`/`subtab` URL parameters and the `#hash` → subtab mapping | `area`/`view` (P12), with a one-time mapping of old `/app?tab=…` links | Done |
| The `pigeon.cost.period` local key tied to `subtab` | the Cost view's own state | Done |

### Redirects

Kept, each one 302 to `/app/<home>/<area>[/<view>]`, the home instance read from the instance
records (`page::BOOKMARKS`, `page::OLD_TABS`, `Console::routes`; the console's root `/app` when
no home instance can be read). A browser keeps the fragment, so `/secrets#NAME` lands on
Access › Secrets with `#NAME`.

| Old URL | Goes to |
| --- | --- |
| `/` | `/app` (the shell opens the home instance's Inbox) |
| `/approvals` | `inbox` |
| `/instances` | `instances/list` |
| `/architecture` | `platform/architecture` |
| `/access`, `/settings` | `access` |
| `/secrets[#NAME]` | `access/secrets[#NAME]` |
| `/tokens` | `access/tokens` |
| `/cost`, `/gateways` | `cost` |
| `/app?tab=…&subtab=…[&instance=…]` | its area and view (`OLD_TABS`), on that instance when named |
| `/app?instance=…` | that instance's `overview` |

Removed: `/app?tab=management&subtab=credentials` as a target (`page::CREDENTIALS`), the
`/instances?i=` carry-over, the shell's client-side `?tab=`/`#hash` rewriting (`/app#people`
now opens the console's default page), and `/app/next/*` (never deployed). Everything else of
the first console answers 404.

### Tools, tests and docs

| Item | Action | Status |
| --- | --- | --- |
| `instances_tabs` | delete with the tool (above); remove from `docs/plans/mcp-tool-audit.md` and `ios-app-next.md`'s current-state text | Done |
| iOS `ios/Pigeon/Views/TabBar.swift` (`instances_tabs`) | call `console_manifest` | Done |
| `tests/smoke/instances.rs` `instances_tabs_for_the_app`; `tests/smoke/core.rs` `core_console_platform_section` assertions on legacy anchors and `?tab=` redirects | replaced by the manifest, contract (P15) and redirect tests | Done |
| `console.md`, `console-hardening.md` | marked superseded by this plan (done in phase 1); kept as the record | Deleted; their record is under [Archive](#archive) |
| [README.md](README.md) | this plan's row updated as phases land | Done |

## Log

- 2026-11-25: plan written. Phase 1 built on `claude/console-manifest`: `Enabled`/`View` in
  `instances/tools.rs`, `AREAS` in `console/sections.rs` (the `Tab` enum and `moved` removed),
  `SectionSpec.view`, `console_manifest`, six smoke tests. Departures from the spec: Metrics ›
  Errors is not declared (no instance-scoped read yet); Site lists Sites before Deployments and
  Visitors; Access › Apps is a view over the users section's data, which the legacy Delegation
  panel also reads; no view declares `since` yet, as every component is at 1.0.0.
- 2026-11-25: phase 3 built on `claude/console-shell`: the new shell is served at `/app/next`
  beside the old one (moved to `web/legacy-shell.{js,css}`), so the current views keep working
  untouched until they are ported and no adapter between two component sets is written and
  thrown away. Departure from the spec: URLs are paths, `/app/next/<instance>/<area>/<view>`
  (`/app/<instance>/<area>/<view>` after cutover), with old `?tab=&subtab=` and `#hash` links
  rewritten in one hop by the shell's `LEGACY` table. Inbox (on `app_inbox`), Overview and
  Instances › All instances are ported as proofs of the shell; every other view registers an
  explicit TODO-port placeholder linking to the same page at `/app`. Writes go only through
  `/console/write/<action>/{begin,finish}` with `{target, change}`, from the confirm dialog.
  Fixture mode runs from `web/fixtures.html?fixtures=1` on a local static server; the Worker
  never serves it or `web/fixtures/`. `console_shell_contract` (P15) derives every view from
  `console_manifest` for a fully enabled home instance and checks renderers, `ops::READS`,
  `write::ACTIONS` and the stylesheet's tokens.
- 2026-11-25: phase 4, Site, Overview and Capabilities ported on `claude/console-port-site`
  (`web/views/site.js`, `web/views/capabilities.js`). Site › Sites reads the record; Deployments
  lists `site_releases` live first, with Publish (`site-deploy`) and Rollback (`site-rollback`,
  destructive); Visitors loads `app_site_hosts` and `app_site_visitors` with the view, invites
  through `visitor-invite` (its answer now carries `reveal`, shown once) and removes through
  `visitor-revoke`. Capabilities makes Enable, Configure, Upgrade (the pin
  `capabilities_upgrade_plan` suggests) and Disable each one `instance-put` diff; Overview edits
  the other fields as a form or JSON with a live diff, and links or unlinks (`linked-unlink`).
  Until Q4, site_passkeys is shown as `sites[].auth`. No new ops were needed.
- 2026-11-25: phase 4, Access and Platform ported on `claude/console-port-access-platform`
  (`web/views/access.js`, `web/views/platform.js`). New reads: `credentials_expiry`, and the
  console's own `app_proxy_tokens` (live and recently expired proxy credentials) and
  `app_people` (people with passkeys, apps with their person, kind and token count). New
  writes: `apple-adopt` and `apple-identity` (P8) on the one write path. `token-issue` and
  `invite` answer `reveal` for ShowOnce. ConfirmDialog takes `form` and `prepare`, so a secret's
  value is typed in the dialog and only its digest reaches `begin`. Endpoints stays read-only
  (no write exists); its portal URL is `mcp.` plus the platform host's parent domain.
- 2026-11-25: phase 5, the decommission, on `claude/console-decommission`. The shell is served at
  `/app/<instance>/<area>/<view>` (`page::shell`, assets at `/app/{shell.css,shell.js,views.js,
  views/*.js}`); the first console's panels, shell, per-view scripts and styles, `visitor.js`,
  `/console/api/section` with section snapshots, `section_data`, the sections that were only
  data anchors, the `section` contract op with the models and ai-agents handlers (and
  ai-agents' HTML `page.rs`), the old write routes and their adapters (`write::LEGACY`,
  `adapt`, `Who::AnyPasskey`: every write is now bound to one person), `/apple/{adopt,identity}`,
  `/console/site-access/*`, `instances_tabs`, and `/app/op`'s run writes are deleted. Old
  bookmarks redirect in one hop on the server (above). The iOS app moved: `instances_tabs` to
  `instances_list` plus `console_manifest` per instance (`TabBar.swift`), and `/secrets/put` to
  `/console/write/secret-set/{begin,finish}` by bearer (`Approver.swift`); `secret-set` is the
  one action added to the bearer allowance, bound to the person the app acts for like the
  others. Departure: released app builds older than this change cannot set secrets (their
  route is gone) and fall back from `instances_tabs` to `instances_list`. Guarded by
  `console_decommissioned`. Live checks after deploy: `/app` serves the shell and every view
  opens; `/`, `/approvals`, `/instances`, `/architecture`, `/access`, `/settings`, `/secrets`,
  `/tokens`, `/cost`, `/gateways` and `/app?tab=…` answer one 302 to `/app/<home>/…`; the old
  assets (`/console/keys.js`, `/console/console.css`, `/app/legacy-shell.js`,
  `/app/visitor.js`), `GET /console/api/section/credentials` and `POST /secrets/put/begin`,
  `/tokens/issue/begin`, `/console/site-access/list` answer 404; `instances_tabs` is not in the
  MCP tool list.
- 2026-11-26: bearer parity for the iOS rebuild, on `claude/ios-bearer-parity` (see "Bearer
  parity" above). `/app/op` takes the app's bearer as well as the session; every write the
  mock-up names is allowed by bearer with the session path's binding. No op, argument or
  policy rule changed.

## Archive

`console.md` (the first console: tabs, sections, passkey sign-in and passkey-bound writes,
first at the platform's root, from `53a0068` at `/app`) and `console-hardening.md` (sections
loading on their own with snapshots, timeouts and retry; the tabs narrowed to Approvals,
Architecture, Access and Cost; landing `slWijyeD5dnMaimB`) were deleted with the code they
describe. Both are in git history before `claude/console-decommission`. What they established and
this console keeps: the passkey session cookie (`__Host-pigeon_session`, SameSite Strict, 30
minutes idle, 8 hours at most), same-origin JSON POSTs only, writes bound to the exact change by
a passkey of their own, and a read that times out at 10 seconds with a retry.

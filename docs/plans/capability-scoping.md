# Capability scoping: every console view comes from a capability on the instance

Status: **decided** (2026-11-26); M1, M2 and M3 built; M4 built for `architecture` and `secrets`, the rest deferred with plans (log); M5's mechanism built, with no capability placed in its own Worker on a linked instance yet. Code references are to main at the time of writing.

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
| `observability` | Cost › 7 days, Today, 30 days (home only, for now) and Metrics › Workers, both while the record enables `observability` | inline | instance | any |
| `secrets` | Access › Secrets | inline | instance | any |
| `proxy` (requires `secrets`) | Access › Tokens (issuing and revoking proxy credentials; the proxy host itself is served by `pigeon-proxy`) | inline | instance | any |
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

Built: `CapabilitySettings.placement` (absent unless set, so `{}` and every stored record keep
their digest); `catalogue::problems` refuses a placement the entry does not offer, any but the
deployed one on home, and any but `linked_placements` (`inline`) off home; `catalogue::placement`
and `placements` say where each runs (home: the catalogue's `home`/`home_worker`; a linked
instance: inline in its own Worker); `capabilities_versions` answers them on each instance row,
and the Capabilities view shows a Runs column (Inline / Own Worker, the Worker, "read-only" on
home). Architecture does not draw the placement yet: the diagram is being redesigned separately.

### M4: per-capability enablement on linked instances, one at a time

Each step turns the catalogue's `linked` flag on for one capability, adds what the instance's
Worker needs to serve it inline, and adds the linked-account OAuth scopes it needs to
`linked::PERMISSIONS`. **The person approves each scope change** (a new consent on every linked
account), so each step is its own landing with its own decision:

1. `architecture` (needs no scope: it reads only the record) — can ride along with step 1.
2. `secrets`: the instance's own Secrets Store (Secrets Store Write on the linked account).
3. `observability` Cost: the instance account's usage (Account Analytics Read, Billing Read).
   **Deferred** (log, 2026-11-26): Cloudflare's OAuth scopes include no Billing Read.
   Today Cost follows the record's own `observability` toggle, as Metrics does
   (`observability::instance::instance_tools`): its views show only on an instance whose record
   enables `observability`, and only on home, because `cost_daily` reads the platform's spend as
   a whole. A linked instance with `observability` gets Metrics and no Cost until this step adds
   per-linked-account cost.
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

Built (mechanism only):

- `plan::LINKED_WORKERS` lists each capability's Worker build for a linked account: its module
  and the Service Binding name. It is empty.
- `catalogue::linked_placements` offers `own_worker` only where the entry offers it and a build
  is listed, so every entry is `inline` on a linked instance.
- Plan. For a record placing a capability `own_worker`, `capability_workers` plans the
  `<instance>-<capability>` Worker (`catalogue::own_worker_name`) before the instance Worker:
  `upload_capability_worker` when missing or changed, `ok` when pigeon's last upload matches,
  a conflict when the name is not pigeon's, and blocked with no build.
- Apply uploads it and records it in the ledger's `workers`, so retire deletes it as it does
  the instance Worker. The instance Worker's upload binds it (`plan::services`,
  `workers::Binding::Service`, new in core). Its digest changes only when it has one to bind
  (`with_services`), so every current plan is as before.
- Nothing is planned for home: its Workers are the platform's.

Why no capability uses it yet: `models` and `ai-agents` need builds made for a linked account
(M4 steps 5 and 6, in the log). `proxy` offers only `inline` in the catalogue: core serves
Access › Tokens, and `pigeon-proxy` is not this capability.

### Platform admin (built 2026-11-26)

A person is the platform's **admin** or a **member**. The role is platform-level, like identity,
so it lives in `users` (home-only, `scope: platform`): the Directory's `users.role` column,
`admin` or null. It is not a catalogue capability and not a record setting.

- **First admin.** The migration that adds the column makes the oldest standard user (Willow)
  the first admin; on a new platform `users_bootstrap` makes the first person one. Everyone
  else is a member until an admin makes them an admin.
- **Admin only:** inviting a person (`users_invite`, console `invite`), removing a user
  (`users_remove`), disconnecting an app (`oauth_revoke`, console `app-disconnect`), adding a
  passkey link for someone else (`users_enrol` of another person), removing another person's
  passkey (`passkeys_remove`, console `passkey-remove`; one's own needs no role, a last one is
  never removed) and changing a role (`users_admin`, console `person-admin`). The last admin
  cannot be removed or made a member.
- **One rule, every path.** The rules are pure functions in `users` (`passkey_removal`,
  `admin_only`, `role_change`); the MCP tools and the console's writes ask them, the writes at
  `begin` (before the passkey) and again at `finish`. `passkey-remove`, `app-disconnect`,
  `invite` and the new `person-admin` are the `users` capability's writes, no longer the
  `console` plumbing's. The iOS app makes the first three by bearer as its person, under the same
  rules; `person-admin` is the console's only.
- **The role is read now.** Each check reads the Directory, never a role carried on a caller, so
  a change of role holds at once, also for a run already going. A service user has the role of
  the person it acts for; one acting for no one is no person and no admin.
- **Policy is separate.** `.pigeon/policy.toml` says what agents may do without asking; the role
  says who may do it at all. `users_admin` is in `risky-tools-need-passkey`, so an agent acting
  for an admin still needs the admin's passkey for each call.
- **Not gated yet:** `users_rename` and `users_unlink` of someone else.
- **Membership:** a platform admin is `owner` on every instance ([membership.md](membership.md)).

### Later: membership

Built 2026-11-26, as planned here with one change (`operator` also approves record changes,
links and retirements; only member management is the owner's): see
[membership.md](membership.md).

Before it, any signed-in person could approve any change on any instance: identity is platform-level
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
- 2026-11-26: M2 built. `multi-instance` is its own hosted capability (`instances::MultiInstance`,
  on the platform Worker), with All instances and Endpoints; its tools (`instance_propose` of a
  new id, `/linked/begin`) stay served by `instances` and `linked`, which only home's console
  and the platform MCP endpoint reach, so no runtime gate was added beside record validation
  and the manifest. The catalogue's entries carry `linked` (false for all but `instances` until
  M4) and `always` (`instances`: every instance has it, so a record never lists it, and
  validation refuses it there). The record's `capabilities` maps a name to settings that hold
  nothing yet; `catalogue::problems` is part of `record::validate`. `requires` is checked on
  every record, home included.
- 2026-11-26: plumbing flag. `catalogue::PLUMBING` names the hosted capabilities that are
  internal plumbing, not something a person uses: `dispatch`, `split`, `versioning`, `console`,
  `passkeys`, `oauth`, `public_host` and `images`. `capabilities_versions` marks each component
  row `plumbing` (additive; no public view carries it), and the console's Capabilities view on
  home lists them under a folded "Internal" heading (Show / Hide) below "Platform capabilities",
  as does the iOS app. The `proxy` entry now says what is true: core serves Access › Tokens
  inline; `pigeon-proxy` serves the proxy host's `/git/` and `/cf/`, which are not this
  capability.
- 2026-11-26: `images` folded into `observability`. `containers_images` is now one of
  `observability`'s tools (`observability/images.rs`; same name, schema and answer), and the
  `images` capability, its versions component, its `PLUMBING` entry and its inventory class are
  gone. `observability` is 1.1.0.
- 2026-11-26: platform admin built ("Platform admin", above): the Directory's `users.role`,
  Willow the first admin by migration, admin-only invites, removals, app disconnects, others'
  passkey links and passkeys, and role changes, at the MCP tools and the console's writes alike.
- 2026-11-26: M3 built (above). The record's one setting is `placement`; there is no Worker
  name in it: an `own_worker` placement on a linked instance would be `<instance>-<capability>`
  (`catalogue::own_worker_name`, M5), and home's Workers are the catalogue's.
- 2026-11-26: M4 step 1 built: `architecture` is `linked`. It needs no linked-account scope.
  A linked record that lists `capabilities.architecture` shows Platform › Architecture (its own
  `instance_model`). Without it, a linked record's areas are unchanged. The Capabilities view
  on a linked instance lists the catalogue capabilities it may enable, with Enable (which
  writes `capabilities.<name>: {}`) and Disable (which also removes what requires it). The
  Overview editor no longer edits `capabilities`.
- 2026-11-26: M4 step 2 built: `secrets` is `linked`.
  - **Scope.** It needs the link's `secrets-store.write` ("Secrets Store Write"; Cloudflare's
    OAuth list has it). `linked::CAPABILITY_SCOPES` names each capability's extra scope;
    `requested_permissions` is `PERMISSIONS` plus those. The client asks for them as
    `optional_scopes`, so a person may decline one at consent.
  - **Rolling it out.** Run `linked_setup` once more. Seeing a scope the registered client
    lacks (`scopes_to_add`), it PATCHes the client's `scopes` and `optional_scopes`, keeps the
    new list for the consent URL, and says which instances to re-link.
  - **Existing links.** They keep what they were granted. A link whose granted `scope` lacks
    the scope is refused before any token is opened (`capability_refusal`, "re-link docs ...
    and allow it, to enable secrets"). A Cloudflare 403 on a link that did not report its
    scopes reads the same way (`explain`). `linked_status` lists `relink_for`. Today only
    noisypigeon is linked, and it must be re-linked only if its record enables `secrets`.
  - **Plan and apply.** `instance_plan` of a linked record enabling secrets reads the
    account's Secrets Store (`plan::SeenCapabilities`, `observe_capabilities`). It adds
    `create_secrets_store` (`pigeon`) when there is none, recorded in the ledger's
    `secrets_stores`; retire leaves the store, since it holds the person's secrets. A link
    without the scope adds a `keep` step with the refusal, which does not stop the site's
    steps. A record without `secrets` plans and digests exactly as before.
  - **Console.** Access › Secrets on a linked instance calls `secrets_list {instance}`, which
    lists the names and change dates in its own store, never values and never the platform's.
    It refuses a record that does not enable secrets. Nothing on a linked instance declares a
    secret yet, so the view has no Set: setting one there waits for the first capability that
    reads one (proxy or models).
- 2026-11-26: M4 step 3, `observability` Cost on a linked instance: **deferred**.
  - **What it would mean.** A linked instance's Cost would be its own account's Cloudflare
    spend, not the platform's.
  - **Why deferred.** Cloudflare's OAuth scope list (408 scopes, read with `linked_scopes`
    today) has no Billing scope. It has `account-analytics.read` (usage), but usage alone is
    not spend: the prices would be pigeon's guess.
  - **Today.** A linked instance with `observability` keeps Metrics and shows no Cost, pinned
    by `console_manifest_by_capability_per_instance_kind`.
  - **Plan.** Add `account-analytics.read` as `observability`'s capability scope. Add
    `cost_daily {instance}`, reading that account's Workers, R2 and DO usage by GraphQL and
    pricing it with the table `cost.rs` uses for the platform. Label it an estimate. Show
    Cost › 7 days/Today/30 days on linked records with `observability`. Revisit if Cloudflare
    adds a Billing Read OAuth scope.
- 2026-11-26: M4 step 4, `proxy` and Access › Tokens on a linked instance: **not yet**.
  - **What a proxy credential is today.** It reaches the platform's own proxies: `/git/` (the
    platform's repositories) and `/cf/` (the Cloudflare API, with the platform's minted
    tokens), served by `pigeon-proxy`.
  - **What it would be on a linked instance.** The same credential, bound to one instance:
    `/cf/` calls only that instance's account and zones, with its link's token, and `/git/`
    only its record's repository.
  - **Why not yet.** That is a new cross-account credential path. It is not a view to turn
    on: the credential rows and checks (`pigeon_proxies::proxy`, shared with `pigeon-proxy`)
    and `pigeon-proxy`'s `credential` request to core both change. The caller-keys work is
    changing that signing path now. Turning the catalogue flag on alone would show the
    platform's tokens on a linked instance, so `proxy` stays not `linked`.
  - **Plan.**
    1. Credentials carry an optional `instance`. `token-issue` from a linked instance's
       Access › Tokens sets it, and `app_proxy_tokens {instance}` lists only that
       instance's.
    2. The proxy's `credential` request names the instance. Core answers with
       `linked::account_for_capability(.., "proxy")`, never the parent token.
    3. `cf_proxy::ALLOWED` is checked against `/accounts/{linked id}/` and the record's
       zones. `/git/` takes only `record.repo`.
    4. No new OAuth scope: `/cf/` reaches only what the link already grants. `proxy` keeps
       requiring `secrets`.
    5. Tests: a credential for instance A is refused on B's account and repository, and on
       the platform's.
- 2026-11-26: M4 step 5, `models` on a linked instance: **not yet**.
  - **What it would be.** An AI Gateway in the instance's own account, with its provider keys
    in that account's Secrets Store (step 2). Cloudflare's OAuth list has the scopes:
    `aig.write` and `aig.run`, and `aig.read` for the logs `cost_daily` reads.
  - **Why not inline.** Inline would be the instance's own Worker, the static site Worker
    (`site_worker.js`). It holds no model code, and putting the gateway client in it would put
    provider calls in a Worker that serves visitors.
  - **Why not yet.** `models` therefore needs `own_worker` on a linked account (M5 below)
    with a model-gateway build fit to upload there, its keys declared as linked secrets, and
    Platform › Models reading per instance. None of these exists yet, and a half-built gateway
    would hold provider keys with no one using it. So `models` stays not `linked`.
  - **Plan.**
    1. Add `aig.write`, `aig.run` and `aig.read` as `models`' `CAPABILITY_SCOPES`.
    2. Let `linked_placements(models)` offer `own_worker`.
    3. Add a `LINKED_WORKERS` module for `models` (M5): a gateway Worker reading its keys
       from the linked store.
    4. Plan: a `CreateGateway` step and the Worker's upload and binding.
    5. Add `ai_agents_models`/`models_*` `{instance}` reads.
    6. Tests: the Worker binds only its own store and gateway, and another instance's
       reads are refused.
- 2026-11-26: M4 step 6, `ai-agents` on a linked instance: **not yet**.
  - **What it needs.** Runs need the sandbox: containers (`containers.write`), Durable
    Objects and Workflows in the instance's account. Model turns go through step 5's gateway.
  - **Why not yet.** That is a second `pigeon-ai-agents`, with its container image built and
    pushed into a linked account. It is large new infrastructure, and it depends on step 5.
    `ai-agents` stays not `linked`.
  - **Plan.** After step 5, a slim run Worker per linked account (M5's own Worker). Home's
    `ai-agents` keeps the inbox, approvals and sessions list, and dispatches a run to the
    instance's Worker over its workers.dev address with a signed caller context. The
    container image comes from the platform's registry. Scopes: `containers.write`,
    `workers-scripts.write` (held). Sessions › Live filters by instance.
- 2026-11-26: M5 mechanism built (M5, above), with every catalogue entry still inline on linked
  instances. The test drives it with a stand-in build. The first real use is `models` (M4
  step 5): list its build in `LINKED_WORKERS`, and validation, plan, apply and retire follow.

# Versions of every capability, connector and template, and pins per instance

Status: **designed; phase 1 under way** (2026-11-23, overnight). Branch
`claude/ai-agent-Xtnf1hGsI6gk`. Nothing here changes a live instance: pins are set only by the
person approving an instance record, as every record change is today.

## Goal

The person's words: "noisypigeon uses pigeon-sites 1.2.3 and blog template 2.0.1 while
willowgraysen uses pigeon-sites 2.0 and blog 3. Upgrades, and breaking changes that require
upgrades, can then be planned and worked."

So:
- every part of pigeon an instance depends on has a version, declared in code beside it;
- every breaking change is marked, with the migration it needs;
- each instance can pin what it depends on;
- pigeon says, per instance, what an upgrade would take, as a list of work items.

It covers all of pigeon: the core capabilities in `src/capabilities`, the capability Workers
(`pigeon-sites`, `pigeon-model-gateway`, `pigeon-ci-gateway`, `pigeon-ai-agents`), connectors
(CI connectors, the linked-account provider, storage providers), site templates (`blog`), and
code pigeon installs into an instance's own account (the site Worker).

Hard lines:
- No new policy rules. No change to `ci/workers.toml`, `.pigeon/ci.toml` or the policy.
- Tests only in `tests/smoke.rs`.
- No pin or upgrade is applied to a live instance by this work. A pin is a field of the instance
  record, so it changes only through `instance_propose` (or the console's Edit form) and the
  person's passkey.

## What is true today (read 2026-11-23)

- **Capabilities** answer `name`, `tools`, `footprint` and so on (`pigeon_capabilities::Capability`).
  None has a version. A capability Worker's manifest carries the commit it was built from, and
  no version.
- **Templates.** `pigeon_blog` carries one template, `blog@1`, at exact version `1.0.0`
  (`TEMPLATE`, `VERSION`). `.pigeon/site.toml` names `template = "blog@1"`. Each site deploy
  answers `template`, `template_version` and `template_digest`. There is no second version to
  choose.
- **Instances.** `Instance` (`instances/record.rs`) has `id`, `title`, `account`, `zones`, `repo`,
  `sites`, `worker`, `worker_override` and `version` (the record's own edit counter). It refuses
  unknown fields. `instance_propose` validates a whole record and waits for the person's
  passkey; `instance_plan` / `instance_apply` plan and apply resources in a linked account.
- **Installed code.** `instance_apply` uploads one site Worker module (`plan::SITE_WORKER`) into
  the instance's account. The ledger keeps the digest of what it last uploaded per Worker, not a
  version.
- **Deploys.** The Lander deploys each platform Worker in its lane. There is one live version of
  each platform Worker at a time, shared by every instance.

## Decisions

### 1. What a version is

- A **component** is anything with a version: a capability, a connector, a template, or code
  pigeon installs into an instance's account. Its **name** is unique across pigeon:
  - a capability is named as the capability (`instances`, `sites`, `models`, `ci`, `ai-agents`);
  - a part it carries is `owner.part`: `ci.buildkite`, `ci.containers`, `storage.scaleway`,
    `storage.digitalocean`, `storage.backblaze`, `linked.cloudflare`, `instances.site_worker`;
  - a template is named as the template: `blog`.
- A **version** is semver `MAJOR.MINOR.PATCH`, no pre-release or build parts (yet).
  - MAJOR moves on a breaking change: one that needs an instance, a consumer repository or a
    caller to change something.
  - MINOR moves on an addition that keeps everything working.
  - PATCH moves on a fix.
  - Below 1.0.0, MINOR is the breaking position, as in Cargo.
- Every component that exists today starts at **1.0.0**: it is live and in use. Its changelog's
  first entry is "1.0.0: the state when versions began".
- **Declared in code beside the component.** The `Capability` trait gains
  `fn versions(&self) -> Vec<versions::Component>`: its own component first, then the parts it
  carries. A capability Worker sends the same list in its manifest (`CapabilityManifest.versions`,
  defaulting to empty, so the contract stays at version 1 and old peers still understand each
  other). `pigeon_blog` declares its template versions itself; `pigeon-sites` reports them.
- **The types** live in `pigeon_core::versions`, pure and tested in the smoke suite:
  `Version`, `Requirement`, `Kind` (`capability`, `connector`, `template`, `installed`),
  `Entry` and `Component`.

```rust
pub struct Component {
    pub name: String,          // "blog", "ci.buildkite"
    pub kind: Kind,
    pub version: String,       // the newest version this build serves
    pub serves: Vec<String>,   // every version it can serve at once; empty means only `version`
    pub changelog: Vec<Entry>, // newest first
}
pub struct Entry {
    pub version: String,
    pub breaking: bool,
    pub summary: String,
    pub migration: Option<String>, // required when breaking: the work an instance must do
}
```

- **Rules a declaration keeps**, checked by the smoke suite for everything it can build natively
  and by `capabilities_versions` at run time (as problems it reports):
  - `version` and every changelog version parse;
  - the changelog is newest first, strictly decreasing, and its first entry is `version`;
  - a `breaking` entry has a `migration`, and moves MAJOR (MINOR below 1.0.0) from the entry
    before it;
  - a non-breaking entry does not move MAJOR;
  - every `serves` version parses, and `version` is among them.

### 2. How breaking changes are marked

- In the component's changelog, as an entry with `breaking: true` and the `migration` it needs,
  written for the person or agent doing the work, such as "edit `.pigeon/site.toml`: `template =
  "blog@2"`, then rename `[site.look]` to `[site.theme]`".
- The changelog lives in the code beside the component, so a change and its entry land
  together and the reviewer sees both in one diff.
- The plan for each breaking change says how pigeon keeps the old version working for pinned
  instances (decision 4), or that it can't and why.

### 3. How an instance pins

- `Instance` gains `pins: {component: requirement}`, such as
  `{"sites": "^1.2", "blog": "=1.0.0"}`. Absent or empty means "follow what pigeon runs", which is
  today's behaviour.
- **Requirements** use Cargo's grammar: `=1.2.3` (exactly), `^1.2` (at least 1.2.0, below 2.0.0),
  `~1.2` (at least 1.2.0, below 1.3.0), `*`, and a bare `1.2.3`, `2.0` or `3`, which mean the
  same as with `^`. So the person's "blog 3" is `"3"` or `"^3"`, and "exactly 1.2.3" is `"=1.2.3"`.
- **Set as every record change is set.** `instance_propose` (or the console's Edit form) with the
  new `pins`; the diff the person approves shows `pins.blog: null → "^1"`. Nothing applies until
  their passkey. A pin is not applied to a live instance by any agent.
- **Checked when proposed.** Each pin must name a component pigeon declares and parse as a
  requirement; otherwise the record is refused with the problem. A pin that no version pigeon
  serves satisfies is allowed: it is how an instance is held back before a breaking change lands
  (decision 4), and it is reported as **held**.
- **Checked when used.** Every operation that acts for an instance asks for its pin first:
  - `site_deploy` and `site_render` select the template version (decision 4);
  - `site_deploy` refuses when the instance pins `sites` to a range the running `pigeon-sites`
    is not in;
  - `instance_plan` holds the site Worker's upload when the instance pins
    `instances.site_worker` below what pigeon would upload (phase 2).

### 4. Serving more than one version at once

Three cases, from cheap to expensive:

1. **Templates and other pure code: carried side by side.** `pigeon_blog` carries a list of
   template versions. The renderer for an instance is the newest version the instance's pin
   allows within the major its `.pigeon/site.toml` names (`blog@1` names major 1). If the pin and
   the file disagree on the major, the deploy refuses and says which to change: the pin is the
   person's, the file is the content's, and a major move is a migration of the content. Each
   deploy records the exact version it used (it does today). A new major (`blog@2`) is embedded
   beside the old one; an old one is dropped only when no instance pins it, which
   `capabilities_upgrade_plan` shows.
2. **Installed code: a recorded version per instance.** The site Worker module pigeon uploads
   into an instance's account carries a version (`instances.site_worker`). The ledger records the
   version uploaded per Worker. An instance pinned below the module pigeon carries is not
   uploaded to: `instance_plan` shows a **held** step instead, and applies nothing for it (phase
   2). An instance's own Worker keeps running whatever was last uploaded, so holding is safe.
3. **Platform Worker code: one live version, refusal and plan-only below a breaking change.**
   `pigeon-sites` and the other capability Workers run one version for everyone. So:
   - an operation for an instance whose pin the running version does not satisfy is refused with
     the pin, the running version and the breaking entries between them, and points at
     `capabilities_upgrade_plan`;
   - (phase 3) the Lander refuses to land a breaking entry for a component while an instance
     pins below it, unless the same landing keeps serving the old major (case 1) or the person
     moves the pin first;
   - (phase 4, only when a real case needs it) an old major of a capability Worker kept running
     as its own script (`pigeon-sites-v1`, bound as `SITES_V1`), with core routing an instance's
     calls by its pin. This is a new Worker per held major, so it is a gate change and a
     `ci/workers.toml` entry; it is designed when first needed, not now.

### 5. Seeing versions: `capabilities_versions` and `architecture_describe`

- `capabilities_versions {changelog?: bool, instance?: string}` (read-only): every component
  pigeon declares, with its kind, version, the versions it serves, the Worker that runs it, any
  declaration problems, and (with `changelog`) its changelog. Then each instance's pins with a
  status per pin: `ok` (the version it selects), `held` (no served version satisfies it; the
  breaking entries in the way), or `unknown` (names nothing pigeon declares). Capabilities that
  declare nothing are listed as `undeclared`.
- `architecture_describe` gains `versions`: every component's name, kind and version (no
  changelogs), from the same declarations. The model is drawn from code, so no instance's pins
  are on the public architecture page.

### 6. Planning upgrades: `capabilities_upgrade_plan`

- `capabilities_upgrade_plan {instance?: string}` (read-only): for each instance (or one), for
  each pinned component, what moving the pin up to the newest version would take:
  - `from`: the pin and the newest served version it allows (or none, when held);
  - `to`: the newest version pigeon serves;
  - `pin`: the suggested new requirement (`^MAJOR.MINOR.PATCH` of `to`);
  - `work`: every breaking entry above the pin's reach up to `to`, oldest first, each with its
    migration; empty means the move is safe.
- It changes nothing and creates nothing. To move, the person (or an agent, for their approval)
  proposes the record with the new pin through `instance_propose`. Phase 2 adds
  `capabilities_upgrade_propose {instance, component}`, which does exactly that in one call: the
  record with that one pin moved, waiting on the Approvals tab.

## Phases

Each phase is one landing (or a few, one concern each), with its smoke scenarios.

| Phase | What | Live change |
| --- | --- | --- |
| 0 | This plan (docs only) | none |
| 1a | Foundation: `pigeon_core::versions` (versions, requirements, components, declaration rules, pin status, upgrade steps); `Capability::versions`; `CapabilityManifest.versions`; remote capabilities report theirs. Smoke: `core_versions_semver_pins_and_upgrades` | none (nothing declares yet) |
| 1b | Declarations: every core capability, the capability Workers' manifests, the connectors (CI, storage, linked) at 1.0.0. Smoke: every capability in `src/capabilities` declares a version that keeps the rules | versions reported |
| 1c | Instances and tools: `Instance.pins`, pin checks in `instance_propose` and the console write, `capabilities_versions`, `capabilities_upgrade_plan`, `architecture_describe`'s `versions`. Smoke: `core_instances_pins_checked_and_planned` | two read-only tools; records accept `pins` |
| 1d | Templates selectable: `pigeon_blog` carries a list of template versions and its changelog; `site_render {pin}` and `site_deploy` select by the instance's `blog` pin within `site.toml`'s major; `site_deploy` refuses when the instance pins `sites` outside the running version. Smoke: `blog_template_versions_selected_by_pin` | deploys honour pins (none is set) |
| 2 | Installed code and proposals: the ledger records `instances.site_worker`'s version per Worker; `instance_plan` holds uploads for pinned instances; `capabilities_upgrade_propose`; the console's Instance tab shows pins, versions and the upgrade plan | none until the person sets a pin |
| 3 | The Lander refuses a breaking entry that leaves a pinned instance with no served version (a gate change, its own landing, called out) | landing refusals |
| 4 | Only when needed: an old major of a capability Worker kept running beside the new one, routed by pin | a new Worker per held major |

## What the person does

Nothing is needed for phase 1 to work: no instance has pins, so everything behaves as today.
To pin an instance, edit its record on the console (or approve an agent's `instance_propose`)
with `pins`, such as `{"blog": "^1", "sites": "^1"}`.

## Log

- 2026-11-23: design. Read: the instances capability (record, plan, propose), `pigeon_blog`,
  `pigeon-sites`' deploy, the contract's manifest, the architecture model and `ci/workers.toml`.

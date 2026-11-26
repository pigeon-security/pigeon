# Per-instance membership

Status: **built** (2026-11-26) on `claude/instance-membership`; not landed. The person's policy
change for `instance_member_set` waits for their passkey (see "Policy", below). Planned in
[capability-scoping.md](capability-scoping.md) ("Later: membership").

## Why

Identity is platform-level: one sign-in and one set of passkeys per person, in the Directory.
Until now, any signed-in person could read, write and approve changes on every instance. An
approval binds a person, an action, a target and a digest, but nothing said whether that person
belonged to the instance the change was about.

## Model

- **Roles per instance:** (instance, person) → `owner`, `operator` or `viewer`.
  - `viewer` reads the instance.
  - `operator` also writes to it and approves or denies changes to it: record changes, links,
    retirements, deploys, rollbacks, visitors, runs.
  - `owner` also changes who belongs to it.
- **Where:** the Directory's `members` table (`instance`, `person_id`, `role`), not the
  instance record. A record's digest and version never change with who belongs to it, and
  identity and roles stay together, home-only, with `users`.
  - Keyed by the instance's id. The home instance's old id (`pigeon-security/platform`, R1) is
    kept under its new one, `pigeon.dev`, before or after the rename has run.
- **Platform admins** (`users.role = 'admin'`) are `owner` everywhere, with no rows.
- **Platform-wide requests:** a request that names no instance (Access, Platform, Sessions,
  Landings, CI, cost) is about the home instance. So it needs the same roles there:
  - viewer on home to read;
  - operator on home to write.
  This keeps today's rule that platform-wide areas are for home's people.
  - A request naming an instance that does not exist yet (a new record) is also about home.
- **New instances:** the person who makes one is its first owner. That is whoever saves a new
  record with `instance-put`, or approves a new instance's draft.
- **New people:** an invited person (`users_invite`, console `invite`) starts as `viewer` on
  the home instance, so the console opens for them. An owner or admin gives them more. The
  platform's first person (`users_bootstrap`) is its admin.
- **Removing:** removing a user removes their memberships. Retiring an instance removes its
  members.

## Migration

The `members` table's migration (`membership::MEMBERS_TABLE`, then `MEMBERS_SEED`) runs once,
in the Directory, on the first request after the deploy. It makes every person who exists then
(each standard user, not service users or site visitors) `owner` of every instance in
`instances:index`. On deploy, then:

- Willow, the admin, is owner everywhere by role and also has rows.
- Every other person is owner of `pigeon.dev`, `noisypigeon`, `sandbox` and `willowgraysen`.

Nobody loses access. The admin narrows it afterwards, from Access › People or with
`instance_member_set`.

## Enforcement

The rules are pure functions in `membership` (`Access::check`, `member_change`, `write_need`,
`tool_need`). Every path asks them with the person's `Access`, which it reads from the Directory
at that moment (`members.access`) and never takes from a caller.

| Path | Needs | Where |
| --- | --- | --- |
| Approving or denying an approval (`approval-approve`, `approval-deny`) | operator on the approval's instance (`manifest::approval_instance`; home when none) | `console::write::begin` and `finish` |
| Console writes on an instance (`instance-put`, `site-deploy`, `site-rollback`, `visitor-invite`, `visitor-revoke`, `linked-unlink`) | operator on the target instance (a site's instance) | `console::write::begin` and `finish` |
| Linking a Cloudflare account to an instance (`/linked/begin`) | operator on the instance | `linked::begin` |
| Other console writes (`secret-set`, tokens, `land-approve`, `land-approval`, models, limits, runs, Apple) | operator on home | same |
| `users` writes (`passkey-remove`, `invite`, `app-disconnect`, `person-admin`, `member-set`) | their own rules (admin, or one's own; owner for `member-set`) | `users::write_rule` |
| Tools from outside (MCP, `call`, `/app/op` tool reads, a capability Worker's dispatch for a run) | viewer for a console read called with only its read arguments (`console::ops::allowed`); operator for anything else; on the instance endpoint's instance, else the `instance` argument's, else home | `dispatch_checked` |
| The console's own reads (`app_*`) | viewer on `instance`, else home; the whole inbox is filtered instead | `console::ops::serve` |
| `approvals_list`, the Inbox count | lists only approvals the person may decide: theirs, on an instance they operate | `approvals::open_for` |
| The switcher (`instances_list`, console and iOS) | lists only the person's instances; an admin sees all | `instances::listed` |
| `console_manifest` | viewer on the instance. With none named, home if the person may see it, else their first instance. Answers `instance.role` | `manifest::console_manifest` |

- **Calls inside a tool** (`cost_daily` gathering `models_daily`, `land_status` gathering
  `ai_agents_status`) go straight to `dispatch`. So a read needs only its own role.
- **Acting for a person:** a service user or OAuth app has the roles of the person it acts for
  (`users::person_of`). This holds by MCP and, for the iOS app, by bearer alike.
- **Acting for no one:** a service user acting for no one is not a person. Membership does not
  ask about it: it is the platform's own automation (the Lander's continuation, CI), as before.
- **People tools:** `users`, `passkeys`, `oauth`, `approvals` and `call` keep their own rules.
- **Apple push:** `apns_register` and `apns_unregister` are about the person's own device.

## Management

- `instance_members {instance}`: the members and their roles, with the platform's admins
  listed apart. For anyone with a role there.
- `instance_member_set {instance, person, role}`: `owner`, `operator`, `viewer` or `none`.
  Only an owner of the instance or a platform admin may call it.
- Console: Access › People shows each person's roles on the instances the viewer may see.
  - A person's expanded row lists their roles, with Change for an owner or an admin.
  - Change makes the console-only write `member-set`: target the instance, change
    `{person, role}`, bound to the passkey like every write.
- **Last owner:** the last owner of an instance cannot be removed or demoted unless the
  platform has an admin (`membership::LAST_OWNER`). The Directory checks this in the same call
  that writes, so two changes cannot both pass. While the platform has an admin (the last
  admin always stays one), an owner can be removed, and the admins remain owners.

## Policy

`instance_member_set` changes who may approve on an instance, so it should need the person's
passkey for each call, as `users_admin` does. The person adds it to
`risky-tools-need-passkey` in `.pigeon/policy.toml`, and the smoke suite's `GATED` list follows.
Until then, an agent acting for an owner or admin can call it without a passkey. Membership is
still checked.

## Open

- A role per area within an instance, such as a viewer who may deploy, is not modelled.
- Service users acting for no one are not limited per instance.
- The iOS app does not yet hide writes by `instance.role`. The server refuses them.

## Log

- 2026-11-26: built: the `members` table and its migration; `membership`'s rules at the write
  path, `dispatch_checked`, the console's reads, the approvals list and the switcher;
  `instance_members` and `instance_member_set`; `member-set` and the roles in Access › People.

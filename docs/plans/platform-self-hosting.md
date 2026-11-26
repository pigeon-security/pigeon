# pigeon.dev as an instance of pigeon

Status: **decided** (2026-11-26); H1 and L1 in progress, the `/platform` redirect built, the
rest not started. Code references are to main at the time of writing.

## Goal

**pigeon.dev is a self-contained instance of pigeon, like any other instance.** The home
instance's Worker *is* the platform: `pigeon-platform`, platform-owned. One Worker serves two
hostnames, by Host:

- `pigeon.dev` — the instance's published landing site: immutable releases from the
  instance's bucket, served the way `src/capabilities/src/instances/site_worker.js` serves
  other instances (`current` at :7, `resolve` at :27), plus the dynamic `/architecture`,
  `/architecture.json`, `/mark/*` and icons from `pigeon_core::home`.
- `platform.pigeon.dev` — the console, MCP, OAuth, `/auth`, the GitHub webhook, the linked
  callback, Apple/AASA and the iOS endpoints. **Unchanged.**

## Decisions

1. **Rename the home instance id `pigeon-security/platform` → `pigeon.dev`.** It matches the
   zone and removes the slash that broke console URLs (`console/page.rs:199-208` now
   percent-encodes it as a workaround). Its own migration phase, R1 below.
2. **Keep the console on `platform.pigeon.dev`.** The earlier option of serving it at
   `pigeon.dev/platform` (Related Origin Requests for passkeys, a second `__Host-` cookie, a
   strict CSP) is **dropped**: on one origin, any script ever served on `pigeon.dev` could call
   the console with the person's cookie, a service worker at `/` could intercept it, and the
   `__Host-` cookie cannot be path-scoped. The landing site would have to stay script-free
   forever for that to be safe. A separate origin costs nothing.
3. **`pigeon.dev/platform` redirects.** `/platform` and `/platform/<rest>` answer one 302 to
   `https://platform.pigeon.dev/app` and `…/app/<rest>`, query kept
   (`pigeon_core::home::console_redirect`, used by `src/sites/src/apex.rs` and
   `src/capabilities/src/site.rs`; smoke `sites_apex_pages`). **Built.**

## Today

- The apex is a Workers Custom Domain owned by `pigeon-platform` or `pigeon-sites`
  depending on `settings::site()` (`src/platform/src/settings.rs:126-133`). Core serves it in
  `src/capabilities/src/site.rs` (host check at `src/platform/src/lib.rs:186`); `pigeon-sites`
  in `src/sites/src/apex.rs`. Both render `src/core/src/home.rs`.
- `ci/deploy/deploy.sh:144-166` and the `site_publish` pre-flight (`site.rs`, `publish`) require
  `https://pigeon.dev/architecture.json` to answer with the deployed commit. **This contract
  stays.**
- The home record is `account: home`, no zones, no sites, no Worker, CI only.
  `instance_plan` refuses home (`instances.rs:454-458`); publishing requires a GitHub repo and
  a linked account (`record.rs:634-648`, `sites_scope.rs:19-29`, `deploy.rs:75-86`).
- The home instance is found by account kind, never by id (`console.rs:174-179`,
  `manifest.rs:26-27`, `ci.rs:33`, iOS `InstanceStore.platform`), which keeps R1 small.

## The home record

```
id:            pigeon.dev
account:       home                       (immutable)
zones:         ["pigeon.dev"]
worker:        pigeon-platform            (platform-owned: plan/apply never touch it)
platform:      { host: "platform.pigeon.dev" }   (new concept: where console/MCP/OAuth live)
sites:         [{ name: "landing", template: "landing@1", target: apex }]
publishing:    from Artifacts (the pigeon repo's `site/` dir), bucket in the home account
source, ci, observability
```

- **Platform-owned** resources (`pigeon-platform`, `pigeon-sites`, gateways, `proxy`,
  `ai-agents`, the apex/`platform.`/`proxy.` hosts, Secrets Store, D1/DO, queues) belong to
  the Lander and setup tools. Never in plan steps, never in the ledger; shown read-only.
- **Managed** resources (the landing bucket, its releases) are what `instance_apply` creates.

### Guard rails (unchanged from the earlier plan)

1. Reserved Worker names and footprint hosts are `Blocked` in `plan::plan` for home; the
   ledger-conflict rule is a second line.
2. Exactly one home record; the account kind can never change in a draft; home sites may not
   target reserved labels (the apex site is allowed only as the `landing` template).
3. Retire stays refused for home (`retire.rs:191`). Ingest and storage stay linked-only.

### Credentials

Home plan/apply and publishing mint a short-lived purpose-scoped token from the parent token
(new purpose `instance_home`: R2 write on the landing bucket, custom domains on the zone),
revoked after each apply. No Workers-scripts write: the Worker is platform-owned.

## Serving: core serves the apex

Port `site_worker.js`'s `current`/`resolve` into core (Rust, in `site.rs`) for the home
instance's landing site: read `current` from the home bucket, resolve the path in the release
manifest, stream the object. `/architecture`, `/architecture.json`, `/mark/*`, the icons and
`/platform*` stay dynamic and answered first. With no release, fall back to today's
`home_html`. Then `pigeon-sites` no longer owns the apex: `settings::site()` drops `also`, and
`apex.rs` is removed once nothing points at it. The earlier core→sites forwarding idea is
dropped.

## R1: instance id rename migration

Every place keyed by the id, from main:

| What | Where |
|---|---|
| Instances index and record key | `instances.rs:32` (`instances:index`), `:46-47` (`instance:{id}`) |
| Id validation: `.` is not allowed today | `instances/record.rs:200-206` (`label`), `:230-246` (`check_id`) |
| Ledger | `instances/plan.rs:51-52` (`ledger:{id}`) |
| GitHub deploy/push state | `github.rs:92-93`, `:312-313` (home has none; check) |
| Linked/ingest keys | `linked.rs:176`, `ingest.rs:516,612`, `retire.rs:163-164` (linked only; none for home) |
| CI scope and attribution | `ci.rs:28-70` (resolves by repo, returns `record.id`); `src/ci/src/scope.rs:80-95` (`for_read` matches the id); `jobs.instance` column, `src/ci/src/scheduler.rs:53,1378-1384,490-500`; `containers.rs:191,321`; `lib.rs:774,1141-1155` |
| Approvals and drafts | `subject` = id: `instances.rs:264,547,577`, `retiring.rs:232,270`, `manifest.rs:53-56`, `approvals.rs:616` |
| Console URLs and bookmarks | `console/page.rs:199-208`; `console.rs:174-185` (`home_url`) |
| `instances_list` consumers | iOS `ios/Pigeon/Views/TabBar.swift:68,82` (`selectedInstance` in UserDefaults; falls back to the platform) |
| Observability | `observability/instance.rs:71-173` (`instance` argument) |
| Tests | `tests/smoke.rs:3056-3080`, `tests/smoke/console.rs:1280-1286`, `tests/smoke/instances.rs:33`, `tests/smoke/sites.rs:505` |
| Docs | `instances/record.rs:18`, `console/page.rs:201`, `docs/plans/linked-accounts.md:12,475,511`, `ios-app-next.md:125-128`, `email.md:70` |

Steps:

1. **Inert code:** allow a dotted single-part id equal to a zone in `check_id` (home only);
   an `aliases` lookup `pigeon-security/platform` → `pigeon.dev` used by record reads,
   `for_read`, console URL parsing and observability args.
2. **Migrate (one tool, approved by passkey):** write `instance:pigeon.dev`, swap the index
   entry, copy `ledger:` (and `github:` keys if present), `UPDATE jobs SET instance = ?` in
   pigeon-ci, refuse (expire) pending approvals whose subject is the old id, and keep the old
   record as a tombstone pointing at the new one.
3. **Compat window (30 days):** old console URLs 301 to the new id; CI reads and MCP calls
   naming the old id are answered for the new one; iOS reselects the platform automatically.
4. **Verify:** `instances_list` shows one home record `pigeon.dev`; `ci_status`/`ci_jobs` for
   `pigeon.dev` include pre-rename jobs; `/app/pigeon.dev/inbox` loads; no Directory key still
   names the old id except the tombstone; a CI run after the rename is attributed to
   `pigeon.dev`.
5. **Remove** the alias and tombstone; update tests and docs.

## Phases

- **H1** guard rails and validation, no behaviour change. *In progress.*
- **L1** `landing@1` template crate (pure, peer of `pigeon-blog`; no scripts; tokens embedded,
  digest tied to version), template dispatch (`deploy.rs:425-437`), `versions.rs` component.
  *In progress.*
- **H2** home plan/apply with the minted `instance_home` token; the platform target in the
  record; platform-owned resources excluded from plans.
- **L2–L3** home publishing from Artifacts: `sites[].build` as root (`record.rs:134-136`),
  an Artifacts archive read (`artifacts.rs:566-591` reads single files only), the home bucket,
  shared release/rollback/list code.
- **S1** core serves the apex release (fallback `home_html`); `/architecture.json` unchanged;
  `pigeon-sites` leaves the apex.
- **R1** instance id rename, above.
- **H4** console shows the platform-owned inventory read-only on the home Overview.

## Steps only the person can take

- Approve the new home record and each home `instance_apply` (passkey).
- Add `instance_home` to `purpose_permissions` and confirm the parent token holds it.
- Run `site_publish` to move the apex onto `pigeon-platform` for good (S1).
- Approve the R1 migration.

## Open questions

1. Home publishing from Artifacts (no GitHub App) or GitHub?
2. Home metrics: the platform Workers too, or only managed resources?
3. Do other instances later get a `platform.<zone>` target of their own?

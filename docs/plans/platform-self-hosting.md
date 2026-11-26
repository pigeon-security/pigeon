# pigeon.dev as an instance of pigeon

Status: **decided** (2026-11-26); H1 and L1 done, the `/platform` redirect built, H2, L2–L3 and
S1 built, R1 steps 1–2 built (the migration not yet run), the rest not started. Code references
are to main at the time of writing.

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
- The home record is `account: home`, no zones, no sites, no Worker, CI only. Since H2,
  `instance_plan`/`instance_apply` accept home (with the `instance_home` token, below).
  Since L2–L3, home publishes from Artifacts (below); a linked instance still needs a GitHub
  repo and a linked account (`record.rs`, `sites_scope.rs`, `deploy.rs`).
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

As built (H2): `platform` is `PlatformRef { host }` on the record (home only; a platform-owned
host of the platform zone, never the apex). `template` is a site field (only `landing@1`). The
apex target is `{ "kind": "domain", "hostname": "pigeon.dev" }`, allowed for home only on a
`landing@1` site. Both new fields are omitted when unset, so existing record digests do not
move. The home plan is the bucket step plus `Keep` lines for the platform's Worker and the apex
(read-only, never a change, never in the ledger).

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

As built (H2, `instances/home.rs`, `split::purpose_permissions`): `instance_home` is
`Workers R2 Storage Write` on the account (creating a bucket is account-level; Cloudflare's
bucket-scoped groups cover objects only), and `Zone Read` on the platform zone alone (no DNS, certificate or route write until a
home step needs one: every home custom-domain step is platform-owned and blocked today). Core mints it
through the token ledger (`mint_for`, 600 s), hands it to the work as the account's bearer (so
no helper mints from the parent), and revokes it after each plan and each apply, on success and
failure. A capability Worker may not ask `credential` for it (`CORE_ONLY_PURPOSES`). If the
purpose is unknown or the parent cannot mint it, plan/apply refuse before any step. A home
Worker is never uploaded and workers.dev never turned on (`plan::NO_SCRIPTS`), and
`plan::home_refusal` re-checks every step before apply.

## As built (L2–L3): home publishing

- **Record.** `publishing: {state: enabled, site}` is valid for home when `repo.kind` is
  `artifacts` and the named site is `landing@1` (`record.rs`, `validate`); a linked record still
  needs GitHub and a linked account, with the same message. `sites[].build` is the content
  root for home (the repository root when unset); linked deploys ignore it, as before.
- **Who serves the tools.** `site_deploy`, `site_releases` and `site_rollback` for the home
  record are core's own (`instances/home_site.rs`, routed in `remote.rs` by
  `home_site::routed`), after the same policy gate and passkey as a linked call; the console's
  Site › Deployments and its `site-deploy`/`site-rollback` writes reach them unchanged.
  pigeon-sites never acts for home: `sites_scope` still refuses the record.
- **Source (L2).** `artifacts::read_tree` resolves the revision (`main` by default) and reads
  every regular file under the root of that one commit, over a read-only git token revoked
  after (`pack::read_tree`): at most 256 files, 8 MiB, 8 directories deep, 512-byte paths.
  A root that is not a relative path, `..`/`.`/empty names, backslashes or control characters,
  symbolic links and submodules are refused; nothing is written.
- **Render.** pigeon-sites renders, as it carries the templates and declares their versions:
  core sends the files to its private `site_build` (not in its manifest, signed with the
  instance's context, pure, no credential), which answers a `landing@1` release at the version
  the record's `landing` pin allows, or the content's problems. Core takes only `landing@1` and
  site paths back. This keeps `pigeon-landing` out of core.
- **Storage (L3).** The release is staged with `site_publish::stage` (blobs it lacks,
  `deploys/{release}.json`, `release-meta/{release}.json`), the record is checked unchanged,
  then `live` moves (`site_publish::activate`), all in `pigeon-platform-site` inside
  `home::with_token`: one `instance_home` token per call, revoked on success and failure.
  Rollback (`site_publish::staged`) and listing (`site_publish::releases`) are the same code
  pigeon-sites now uses for linked instances. No new permission: R2 write covers the reads.
- **Not touched.** No Worker upload, no domain attach; the apex was still served by
  `pigeon_core::home` until S1. The `sites` pin is not checked for home (core publishes it).

What the person does next:

1. Propose the record change (`instance_propose`), approve it with the passkey:
   `sites[0].build: "site"` and `publishing: {"state": "enabled", "site": "landing"}`.
2. Run `site_deploy {instance: "pigeon-security/platform"}` (or Site › Deployments › Deploy),
   approving with the passkey; `site_releases` lists it. pigeon.dev serves it once S1 has
   landed and the domain is on core.

## Serving: core serves the apex

Port `site_worker.js`'s `current`/`resolve` into core (Rust, in `site.rs`) for the home
instance's landing site: read `current` from the home bucket, resolve the path in the release
manifest, stream the object. `/architecture`, `/architecture.json`, `/mark/*`, the icons and
`/platform*` stay dynamic and answered first. With no release, fall back to today's
`home_html`. Then `pigeon-sites` no longer owns the apex: `settings::site()` drops `also`, and
`apex.rs` is removed once nothing points at it. The earlier core→sites forwarding idea is
dropped.

## As built (S1): core serves the apex

- **Order** (`site::apex`, `src/capabilities/src/site.rs`): `/platform*` answers the console
  302; `/architecture`, `/architecture.json`, `/mark/head.webp`, `/favicon-32.png` and
  `/apple-touch-icon.png` are built into core and answered next, whatever the release holds;
  every other `GET`/`HEAD` is the release; any other method is 404, as before.
- **Reading the release.** `site_worker.js`'s `current`/`resolve` are in Rust in
  `pigeon_core::site_publish` beside the publishing code (`live`, `resolve`, `pick`,
  `cache_control`, `etag`, `decode_path`), over a read-only `Read` trait every `Store` gets.
  Core reads `pigeon-platform-site` through a new **R2 binding, `HOME_SITE`**, in
  `src/platform/wrangler.toml` (not a `ci/`-gated path): no token is minted or held to serve,
  and the binding can be no wider than that one bucket. Writes still go only through the minted
  `instance_home` token (L3). Each isolate rereads `live` at most every 30 s, as the JS does,
  and keeps the last release it read if a read fails.
- **Serving.** The file the path resolves to (exact, then `…/index.html`; `..` never resolves),
  else the release's `/404.html` with 404, with the JS's ETag/304, `Cache-Control` (60 s for
  `.html`/`.xml`, an hour otherwise), `X-Pigeon-Release` and `X-Pigeon-Commit`. Images and
  fonts are streamed; text (pages, CSS, SVG, XML) is read whole and checked.
- **Fallback.** No binding, nothing live, `live` not naming a release, a missing manifest or
  blob, a read error, a bad escape, or no `/index.html`: today's answer, `home_html` for `/`
  (never a 5xx) and 404 for the rest.
- **No script.** Every answer carries `home::CSP` (no `script-src`, `default-src 'none'`). Its
  `style-src` now also allows `'self'`, because the release links `/assets/pigeon.css` rather
  than inlining it; still no script. At serve time a script type (`javascript`, `ecmascript`,
  `wasm`) is never answered, and text carrying `<script`, `javascript:` or an `on…=` attribute
  in a tag is refused (logged, built-in answer instead): defence in depth behind L1's check.
- **Ownership.** `settings::site()` has no `also` (and no service binding to move to):
  `site_publish` offers only `pigeon-platform`. Its pre-flight for core itself now checks core's
  own `/architecture.json` names a 40-character commit and answers which release `/` will serve
  (`release`, `null` for the built-in page). Later clean-up (`site` 1.0.1): `Site` lost `also`
  and `bindings`, `site_publish` lost its `worker` argument and the other-Worker page
  pre-flight, and it always attaches the apex to this Worker.
- **pigeon-sites keeps `apex.rs` for now.** The apex's custom domain is still attached to
  pigeon-sites (`x-served-by: pigeon-sites` on 2026-11-26). Removing it in this landing would
  break pigeon.dev, and deploy.sh's `/architecture.json` check, until the person moves the
  domain. Remove `src/sites/src/apex.rs` (and its `mod`/host branch in `src/sites/src/lib.rs`)
  in the landing after step 2 below.
- Smoke: `apex_serves_the_home_release`, `linked_site_worker_serving_unchanged`,
  `site_on_the_apex`. `site_worker.js` is unchanged.

What the person does after S1 lands and deploys:

1. Check the binding deployed: `site_releases {instance: "pigeon-security/platform"}` shows the
   live release.
2. Run `site_publish {}` (it serves from `pigeon-platform`). It checks core's own
   `/architecture.json` names a commit, answers the `release` `/` will serve, and attaches
   `pigeon.dev` to `pigeon-platform` as its Workers Custom Domain, taking it from pigeon-sites.
3. Verify: `curl -sI https://pigeon.dev/` shows `x-served-by: pigeon-platform`,
   `x-pigeon-release: release-…` and the CSP; `https://pigeon.dev/architecture.json` names
   main's commit; `https://pigeon.dev/nope` is the release's 404 page.
4. Done: `src/sites/src/apex.rs` is removed; pigeon-sites answers 404 on the apex and may no
   longer ask core for `architecture_describe`.

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

### As built (R1 steps 1–2)

Steps 1 and 2 are built; **the migration has not run.** Until it does, nothing reads
differently: every alias is keyed on the tombstone, which only the migration writes.

- **Ids.** `record.rs` `check_id`: an id with a dot and no slash is allowed for the home record
  only, and only when it is one of its `zones` (`pigeon.dev`); linked ids are unchanged. `label`
  is unchanged (it is the DNS-label check). The signed Worker context accepts a dotted id too
  (`pigeon_contract::caller::valid_instance`, a DNS name with no slash), so instance-scoped
  calls to capability Workers keep working after the rename.
- **The alias** (`instances/rename.rs`). `ALIASES = [("pigeon-security/platform",
  "pigeon.dev")]`; an alias is active only while `instance:pigeon-security/platform` holds a
  `Tombstone {renamed_to, renamed_from, renamed_ms, compat_until_ms, approval}`. Any id not in
  `ALIASES` is answered without a Directory read. Where it is used:
  - record reads: `instances::one` follows the tombstone, so the `?instance=` MCP endpoint, the
    signed-context scope (`split.rs`), observability, publishing and the manifest read the new
    record;
  - every tool call: `dispatch` rewrites an `instance` argument naming the old id
    (`instances::current_args`), and so do the console's reads (`console/web/ops.rs`, before
    its `app_*` reads); this covers MCP tools that take `instance`, observability's
    `instance` argument (whose answer now names `record.id`) and CI's scoped reads;
  - CI: core's `ci_scope` answers a read naming the old id for the new one with
    `renamed_from` (`ci::resolve_renamed`), and `pigeon_ci::scope::for_read` accepts it
    (`Scope::answers`). Attribution resolves by repository, so new jobs name `pigeon.dev`
    with no alias;
  - console URLs: `/app/pigeon-security%2Fplatform/<rest>?<query>` answers **301** to
    `/app/pigeon.dev/<rest>?<query>` (`console.rs`, `page::page_instance`, `page::moved`); old
    `/app?instance=…` bookmarks land on the new id in one hop; `page::is_page` allows a dot in
    the instance segment when an area follows it;
  - a draft or record naming the old id is refused (`instances.rs` `check`, and the Directory's
    `apply_draft` expires a draft whose key holds a tombstone);
  - home rollback (`home_site.rs`) accepts a release staged under the old id.
- **iOS** needs no change: `ManifestStore.selected` (`ios/Pigeon/Navigation/ManifestStore.swift`)
  falls back to the home instance when the stored `selectedInstance` is not listed.
- **The migration** is `instance_rename_home` (`instances/renaming.rs`), arguments
  `{dry_run?: bool, plan?: string}`:
  - without `plan`: reads the Directory and the CI scheduler and lists exactly what it would
    do. With `dry_run: true` that is all. Otherwise, when there is something to do and nothing
    in the way, it asks for the person's passkey: a `linked_plan` approval marked `rename`,
    subject the old id, its digest bound to the old id, the new id, the home record's digest
    and the steps (30 minutes to approve, as any plan).
  - with `plan: <approval>`: reads everything again, refuses if the record's digest or the
    plan changed (nothing is changed), takes the approval once, and runs the steps in order:
    write `instance:pigeon.dev` (the same record, id `pigeon.dev`, version + 1); move each
    per-instance key present (`ledger:`, `github:deploy:`, `github:push:`, `ingest:runs:`,
    `data_url:`, `data_key:`; today only the ledger); expire the open approvals whose subject is
    the old id (new Directory op `approval.expire`, with a note; the rename's own are left);
    swap the index entry; `UPDATE jobs SET instance = 'pigeon.dev' WHERE instance = …` in the
    CI scheduler (new scheduler op `instance_rename`, through `CI_SCHEDULER` as the Lander
    reads it: `pigeon_ci::rename_instance`); last, the tombstone with a 30-day
    `compat_until_ms`. Each step is safe to run again, and the steps are derived from what is
    stored, so a run that stopped part way plans only what remains, and a run after it is done
    answers "nothing to do".
  - It refuses when `pigeon.dev` already holds a different record, when a moved key already
    holds a different value under the new id, when the old id is not the home record, or when
    `pigeon.dev` is not one of its zones.
  - Smoke: `home_id_rename_r1` (with an in-memory Directory/CI double).
- **Deploy order.** The contract change (every capability Worker), the scheduler op
  (`pigeon-ci-gateway`) and core (`pigeon-platform`) must all be deployed before the migration
  runs: one landing does it.

What the person does (step 4 above is the verify):

1. **Dry run.** Ask the agent to call `instance_rename_home {"dry_run": true}`. For the home
   record today it should list: write `instance:pigeon.dev` (version 5), move
   `ledger:pigeon-security/platform` (if the H2 apply recorded one), expire the open approvals
   naming `pigeon-security/platform` (if any), swap the index entry, re-attribute the CI jobs
   (with their count), replace the old key with the tombstone. Anything marked `stop:` means
   nothing will run.
2. **Approve.** The agent calls `instance_rename_home {}`; the plan waits on the console's
   Approvals tab ("Rename the home instance pigeon-security/platform to pigeon.dev"). Read the
   steps and approve with your passkey within 30 minutes; the agent then calls
   `instance_rename_home {"plan": "<approval>"}`. If the record changed in between, nothing
   runs: plan again.
3. **Verify** (step 4): `instances_list` shows one home record, `pigeon.dev`; `ci_jobs
   {instance: "pigeon.dev"}` includes pre-rename jobs, and `ci_jobs {instance:
   "pigeon-security/platform"}` answers the same; `/app/pigeon.dev/inbox` loads and
   `/app/pigeon-security%2Fplatform/inbox` answers 301 to it; `instance_rename_home
   {"dry_run": true}` answers "already pigeon.dev; nothing to do"; the next CI run is
   attributed to `pigeon.dev`. The iOS app reselects the home instance by itself.

**Step 5, after `compat_until_ms` (30 days):** delete the `ALIASES` entry (which turns every
alias off), forget the key `instance:pigeon-security/platform`, drop `renamed_from` from the CI
scope and the old-id fallback in `home_site.rs` rollback, and update the tests and docs that
name the old id (`tests/smoke/instances.rs`, `tests/smoke/console.rs`, `tests/smoke.rs`,
`docs/plans/linked-accounts.md`, `ios-app-next.md`, `email.md`). `check_id`'s dotted-home rule
stays.

## Phases

- **H1** guard rails and validation, no behaviour change. *Done.*
- **L1** `landing@1` template crate (pure, peer of `pigeon-blog`; no scripts; tokens embedded,
  digest tied to version), template dispatch (`deploy.rs:425-437`), `versions.rs` component.
  *Done.*
- **H2** home plan/apply with the minted `instance_home` token; the platform target in the
  record; platform-owned resources excluded from plans. *Built* (smoke
  `home_instance_plans_and_applies_with_a_minted_token`).
- **L2–L3** home publishing from Artifacts: `sites[].build` as root (`record.rs:134-136`),
  an Artifacts archive read (`artifacts.rs:566-591` reads single files only), the home bucket,
  shared release/rollback/list code. *Built* (smoke
  `home_site_publishes_from_artifacts_with_a_minted_token`); see "As built (L2–L3)" below.
- **S1** core serves the apex release (fallback `home_html`); `/architecture.json` unchanged;
  `pigeon-sites` leaves the apex. *Built* (smoke `apex_serves_the_home_release`); see "As built
  (S1)" above. The domain moves when the person runs `site_publish`; `apex.rs` goes after.
- **R1** instance id rename, above. Steps 1–2 *built* (smoke `home_id_rename_r1`); the
  migration waits for the person's passkey; step 5 after 30 days.
- **H4** console shows the platform-owned inventory read-only on the home Overview.

## Steps only the person can take

- Approve the new home record and each home `instance_apply` (passkey).
- Nothing for the token: the parent mints `instance_home` with its Account API Tokens Write.
- Run `site_publish {}` to move the apex onto `pigeon-platform` for good (S1; steps in "As
  built (S1)").
- Approve the R1 migration (`instance_rename_home`; see "As built (R1 steps 1–2)").

## Open questions

1. ~~Home publishing from Artifacts (no GitHub App) or GitHub?~~ Artifacts (L2–L3).
2. ~~Home metrics: the platform Workers too, or only managed resources?~~ The platform Workers
   too. `observability_worker_metrics {instance: "pigeon.dev"}` reads the home record's
   `worker` and every Worker in the lane map (ci/workers.toml, the list `observability_summary`
   and the architecture model also read): `pigeon-platform`, `pigeon-sites`,
   `pigeon-model-gateway`, `pigeon-cloud-gateway`, `pigeon-ci-gateway`, `pigeon-proxy` and
   `pigeon-ai-agents`. One GraphQL query (`scriptName_in`) under one analytics token; the
   answer's `metrics` and `estimated_cost` are the totals, and `workers` adds one row per Worker
   (the console's Metrics › Workers shows them as a table). A linked instance still reads only
   its one approved Worker, with a one-row `workers`.
3. Do other instances later get a `platform.<zone>` target of their own?

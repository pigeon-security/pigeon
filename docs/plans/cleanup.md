# Clean-up: faster, safer iteration after the split

Status: **planned; items 1 and 2 landed; the rest planned per branch in "Second pass: the
branch plan", not built** (2026-11-22).
Nothing landed: the person's reviewer lands. Planner run `XE8RUxKJDU6J`, branch
`claude/ai-agent-XE8RUxKJDU6J`.

## Goal

The person's words: "find and do the clean-up work that makes iteration on pigeon faster, more
reliable and more resilient, before other work proceeds." Write a ranked list of clean-up tasks,
then do the top items that are safe tonight.

Hard lines for tonight (from the person, overnight):
- Build only code that does not touch `ci/`, `.pigeon/`, `.claude/settings.json` or the policy.
  Anything there is a follow-up for the person, written out exactly.
- No new Workers in `ci/workers.toml`. Nothing lands. Tests only in `tests/smoke.rs`.

## What is true today (read 2026-11-22, main at `5ced0e0`, then `e510c56`, docs only)

- Six Workers: core (`pigeon-platform`), sites, model-gateway, ci-gateway, proxy and ai-agents.
  The lane map is `ci/workers.toml`.
- The gate (`ci/runner/test.sh`): no stray tests, `cargo fmt`, `cargo clippy --workspace` for
  WASM, clippy on the smoke test natively, the smoke suite (60 s cap), the settings check, then
  `cargo check -p pigeon-platform`. The `test` job on `5ced0e0` took 101 s.
- Deploys on `5ced0e0`: core 184 s, each other Worker 106 to 111 s. Each deploy job builds its
  Worker from scratch and runs `wrangler deploy` against the stand-in twice before the real one
  (`ci/deploy/allowlist.sh`).
- `land_status` lists seven incidents in `incidents_to_see` on every call: core 1, 2, 3
  (`unisolated`), dotest-a 4, core 5, ai-agents 7 and 8 (`stuck`). Every Worker has deployed
  green since, and dotest-a is no longer in the map.
- This sandbox has one CPU, and cargo and node are installed but not on `PATH`
  (`/usr/local/cargo/bin`, `/usr/local/node/bin`).

## The ranked list

Ranked by what it saves or protects per unit of effort and risk. Effort: S (under an hour), M (a
day), L (more). "Tonight" means built on a branch tonight.

| # | Task | Effort | Risk | Who | State |
| --- | --- | --- | --- | --- | --- |
| 1 | Close incidents a later green deploy covers; hide unmapped Workers' incidents | S | low | code | landed (`3SKi_jbmv5_m6F2y`) |
| 2 | Smoke coverage for core's `dispatch` reaching bound Workers' tools | S | low | code | landed (with item 1) |
| 3 | Check every `wrangler.toml` in the gate, without building | S | low | person (ci/) | follow-up 1, script tested |
| 4 | A new Worker in one landing, not two | M | medium | code, later | procedure below; design below |
| 5 | Pinned CI job retries on its connector | none | none | none | already fixed on main |
| 6 | Core stops exporting `CiScheduler` and `ModelJobs` | S | medium | code, later | needs a rehearsal |
| 7 | Drop the gate's redundant `cargo check -p pigeon-platform` | XS | low | person (ci/) | follow-up 2 |
| 8 | Clearer incident text for a Worker's first deploy | S | low | code, later | proposed |
| 9 | Plan statuses out of date with the split | XS | none | docs | done on this branch |
| 10 | Delete `claude/land/<id>` test refs after a landing ends | S | low | code, later | proposed |
| 11 | Reuse the tested build at deploy | L | medium | code and ci/ | recorded, not designed |
| 12 | Revoke an AI agent run's git grant when the run ends | S | low | ai-agents planner | proposed |
| 13 | Move `CiRunner` to the CI gateway | L | high | code, later | open item of workers-split |

### 1. Close incidents a later green deploy covers

- **Problem.** An incident that ends `unisolated` or `stuck` stays in `land_status`'s
  `incidents_to_see` for ever. Nothing moves it on, even after its Worker deploys a newer build
  that passes. An incident for a Worker that left the map (dotest-a) can never be covered. Every
  reader of `land_status` (people, planners) has to know which ones are moot.
- **Evidence.** `src/capabilities/src/land.rs`, the `status` op: `incidents_to_see` is
  `state IN ('open', 'unisolated', 'stuck') ORDER BY id DESC LIMIT 10`. The only writes to
  `incidents.state` are `open_incident` and `drive_incidents` (to `isolated`, `unisolated` or
  `stuck`). Live: seven such incidents today, all covered or unmapped.
- **Fix (built, branch B2).**
  - A pure rule, `lanes::incident_covered`: an `unisolated` or `stuck` incident is covered by a
    later succeeded lane deploy of the same Worker that covers at least what the failed deploy
    covered.
  - The Lander's `close_covered` marks such incidents `covered`, adding which deploy covered
    them to the summary. It runs, best-effort, after any lane deploy passes, so the old ones
    close the first time any lane deploy passes after this ships.
  - `incidents_to_see` drops incidents whose Worker the map no longer has. The per-landing
    `incidents` list still shows every state.
  - `open` incidents, which hold a lane, are never touched.
- **Smoke.** `land_attributes_a_failed_deploy` gains the rule's cases.
- **Risk.** Low. It is in the Lander, which runs every landing, so the sweep never fails a deploy
  step or skips the `after` steps; an error is logged. No migration: state is free text.

### 2. Smoke coverage for core's `dispatch` of a remote tool

- **Problem.** A capability Worker's `dispatch` through core was served with core's own
  capabilities only, so a tool on another Worker (`models_submit`) was "unknown". The first live
  executor run could not take its first turn. Fixed today (`7b3f5aa`), but the smoke suite could
  not have caught it.
- **Evidence.** The choice lived in `src/platform/src/lib.rs` (the `split::CORE_HOST` branch),
  and `src/platform` builds for WASM only. The smoke suite runs natively on
  `pigeon-capabilities` and has no Workers runtime, so it stubs the bindings out entirely.
- **Is there a cheap way?** Yes: move the decision and the merge into pure functions in
  `pigeon-capabilities` and test those. A full core-host request needs `Env`, which the native
  suite does not have, and faking it would cost far more than it would catch.
- **Fix (built, branch B1).**
  - `split::merge(local, remote)`: the clash rule, pure; `split::all` uses it.
  - `split::op_needs_bound(op)`: only `dispatch` needs the bound Workers' tools.
  - `split::for_op(env, op, local, bindings)`, which core's fetch handler now calls.
  - `remote::Remote::new` is public, so the suite builds a remote capability from a recorded
    manifest.
  - Smoke: `core_dispatch_reaches_bound_workers_tools`. It checks that a merged list finds
    `models_submit` on the model gateway and core's own list does not (the bug as it was), that
    a clashing remote is left out, and that every other core op stays core-only.
- **Risk.** Low. Behaviour is unchanged; the code moves from the platform crate into the
  capabilities crate.

### 3. Check every `wrangler.toml` in the gate

- **Problem.** wrangler 4.166 refuses `max_instances` on a Durable Object-managed container. The
  gate never runs wrangler, so the error showed only at deploy: the first ai-agents deploy failed
  (landing `ot8xRV3UJFwmHSC5`, incident 7, fixed by `c94fca5`).
- **Evidence.** `ci/runner/test.sh` runs cargo only. `ci/deploy/allowlist.sh` runs a real
  `wrangler deploy` against the stand-in, but only inside the deploy job, after the merge.
- **Why not `wrangler deploy --dry-run` per Worker.** It runs each Worker's `[build]` command
  (`worker-build --release`), a full release build per Worker. Six Workers would add many minutes
  to every gate run.
- **Fix (tested tonight, not committed: it is a `ci/` change).** `wrangler types` reads and
  validates the whole config. With the `[build]` section stripped from a copy of the config, it
  does not build. One `npm ci` serves every Worker (all pin wrangler 4.166.0).
  - On main: every Worker's config reads clean, in 48 s in total on this one-CPU sandbox (14 s
    of it `npm ci`).
  - With `max_instances = 6` put back in ai-agents' container: it fails, naming the field, as
    the deploy did.
  - The exact change is follow-up 1.
- **Risk.** Low. It adds about 50 s to the gate (less with four CPUs and npm's cache) and needs
  the npm registry, which every deploy needs already.

### 4. A new Worker in one landing

- **Problem.** The landing that adds a Worker cannot deploy it.
- **Evidence.**
  - At checking, lanes come from main's map (`land/merge.rs`, `map_at(objects, main)`), which
    lacks the new entry. The new crate's paths match no Worker, so they count as shared.
  - At deploy, `deploy_step` and the `deploy_grant` op look the Worker up in the map compiled
    into the running core (`self.settings.map`, from `settings::map()`).
  - The deploy proxy in pigeon-proxy builds its scope from the map compiled into it.
  - So the Worker is known only once both core and the proxy have deployed a build that lists
    it. `src/ai-agents-worker/wrangler.toml` says so in a comment.
- **Procedure today (document; no change needed).**
  1. Landing A, a gate change (`ci/**`, delegate approval): the crate (workspace member,
     `wrangler.toml`, `package.json`), `ci/deploy/deploy-<name>.sh`, `[jobs.deploy-<name>]` in
     `.pigeon/ci.toml`, and the `[[worker]]` entry in `ci/workers.toml`. The map is
     `core_first` and belongs to core and the proxy, so core deploys first, then the proxy and
     the rest. The new Worker is not deployed.
  2. Wait until landing A is `landed` (core and proxy both deployed it).
  3. Landing B touches only the new crate, a one-line comment is enough. It is that Worker's
     lane alone, and it is the Worker's first deploy.
  4. Landing C binds it in core (`[[services]]` in `src/platform/wrangler.toml` and
     `settings::BINDINGS`): a service binding's target must exist before its caller deploys.
- **Design for one landing (later; not tonight).** It changes the Lander and the deploy scope,
  so it is not safe to build overnight.
  1. At checking, classify with main's map plus the `[[worker]]` entries the branch adds. Only
     new names count, never changed ones, so a branch still cannot move an existing path's
     lane. This is safe because the map is guarded (`ci/**`): adding an entry already needs
     the gate change approval.
  2. At deploy, resolve the Worker from main's map at the deploy's commit, falling back to the
     compiled one (already an open item in workers-split.md).
  3. Order the new Worker's first deploy after the proxy's deploy of the same landing, so the
     deploy proxy knows its script: a `proxy_first` beside `core_first`.
  - Effort M, risk medium: a bug here can refuse a deploy or widen the deploy scope. Smoke:
    extend `lanes_scope_paths_and_order_only_the_contract` and
    `land_deploys_in_lanes_per_worker`.

### 5. A pinned CI job could not retry after its container stopped

- **Checked: fixed on main.** `9312530` ("a job pinned to a connector retries on it after an
  infrastructure loss"), with the smoke scenario `ci_retries_a_pinned_job_on_its_connector`.
  Nothing to do.

### 6. Core still exports `CiScheduler` and `ModelJobs`

- **Problem.** Both classes moved (`transferred_classes`) to the gateways. Core's bindings name
  them with `script_name`, yet core still defines and exports both DO shims. It also builds them
  `settings::ci()` and `settings::model_jobs()`, which nothing else uses, and links their code.
  The doc on `CiScheduler` ("Core hosts it") is wrong.
- **Evidence.** `src/platform/src/lib.rs:27-48` and `79-99`; `src/platform/src/settings.rs:343`
  and `352`; `src/platform/wrangler.toml:111` and `113`. The workers-split Log's open item
  "Cleanup. Core stops exporting `ModelJobs` and `CiScheduler`".
- **Fix (later).** Remove the two shims and the two settings functions. Keep the v2 and v4
  migrations: they are history and must not be edited.
- **Risk: medium, so not tonight.** No one has checked whether Cloudflare accepts a script
  whose migrations once created a class it no longer exports, when that class was transferred
  away. Incident 5 shows that an upload can be refused over a moved class, and core runs the
  Lander. Rehearse first with a throwaway pair, as `do-rehearsal.md` did, and land on a quiet
  queue.

### 7. The gate checks the platform twice

- **Problem.** `cargo clippy --workspace -- -D warnings` already checks `pigeon-platform` for
  WASM (it is a workspace member and the default target is WASM). The last step,
  `cargo check -p pigeon-platform`, checks it again with rustc rather than clippy-driver, so
  the workspace crates are checked a second time.
- **Evidence.** `ci/runner/test.sh`, last step; `Cargo.toml` `members`; `.cargo/config.toml`.
- **Fix.** Follow-up 2. Measured tonight in the `test` jobs of B1 and B2: the step takes 7.6 to
  9.1 s of a 93 to 123 s job. Small, but it is free, and every gate run pays it.

### 8. Clearer incident text for a Worker's first deploy

- **Problem.** ai-agents incidents 7 and 8 say "may still be on a failing build: a person must
  look", but the Worker had never deployed: there was no build to be on, and nothing to roll
  back.
- **Evidence.** `land_status`: incidents 7 and 8 have `last_good: null`, `redeploys: 0`.
  workers-split.md's open items: "D cannot tell a first-deploy environment failure from a code
  fault" and "Skip the rollback when the failed job never uploaded".
- **Fix (later).** When `last_good` is `None` and the Worker has no earlier succeeded deploy,
  say "first deploy of <worker> failed; nothing was running and nothing is rolled back". Item 1
  closes such incidents once the Worker deploys.

### 9. Plan statuses out of date with the split

- **Problem.** The README's table, which agents read first, says workers-split is "phase 2
  under way" (the plan says complete). It says ai-agents is not deployed (pigeon-ai-agents is
  live and bound: `d30e183`, `06c9a61`). It says friction is "awaiting deploy" (its tools,
  `call` and the async model jobs, are live). `do-rehearsal.md` is not listed.
- **Fix (done on this branch).** The README rows for workers-split and friction, plus rows for
  do-rehearsal and this plan. The ai-agents row and the first lines of `friction.md` and
  `ai-agents.md` are left to their owners. The ai-agents planner landed its Log at `e510c56`
  during this run, and this run's grant cannot read main past `5ced0e0`, so editing its lines
  here could clash.

### 10. Test refs for rebased landings are never deleted

- **Problem.** A rebased commit under test is pushed to `refs/heads/claude/land/<id>`, and
  nothing deletes it. The refs pile up in Artifacts and in every `git ls-remote`.
- **Evidence.** `src/capabilities/src/land/merge.rs:378` (`test_ref`). No delete anywhere
  (grep). Recorded as a departure in workers-split.md, phase 1b.
- **Fix (later).** Delete the ref when a landing ends (landed, failed or cancelled),
  best-effort, through the same git push the Lander already makes.

### 11. Reuse the tested build at deploy

- **Problem.** Each deploy job builds its Worker again from scratch. That is most of the 106
  to 184 s per Worker, and a shared landing runs six of them on CI's containers.
- **Evidence.** The `ci_commit` timings for `5ced0e0`; `ci/deploy/deploy.sh` and
  `allowlist.sh`. Already recorded under "Later" in workers-split.md.
- **Fix.** Not designed. It needs a build cache or artifact store shared by jobs, which means a
  ci/ change and a decision about where builds are kept. The feasible first step (one build per
  deploy job) is B1 in "Second pass: the branch plan".

### 12. An AI agent run's git grant outlives the run

- **Problem.** A run grant lives 12 hours whatever the run does. ai-agents.md: "Revoking a run's
  grant when it ends is left for later."
- **Fix.** Revoke it when the run ends. That belongs to the ai-agents planner; recorded here so
  it is not lost.

### 13. `CiRunner` still core's

The workers-split plan's open item, with its handover steps. It is large and risky (a container
class cannot be transferred), and it is not clean-up. It is listed so the ranking is complete.

### Looked at, nothing to do

- The smoke suite: 30 to 31 scenarios in 6 to 19 s in tonight's CI runs, well under 60 s.
- The six `deploy-<worker>.sh` scripts are five-line wrappers over `deploy.sh`, not duplicates.
- `expect` in the DO constructors (`src/platform/src/lib.rs`): workers-rs's `DurableObject::new`
  cannot return an error, so a failed migration can only panic there. Leave as is.

## Decisions

- **Covered incidents close themselves; no close tool.** A manual `land_incident_close` would be
  a new tool, and the person's policy would have to decide who may call it. The automatic rule
  covers every case seen so far. Conventional choice, made without asking (the person is
  asleep).
- **"Covered" needs a later *lane* deploy that passed.** An incident's own rollback or restore
  deploys never close it, so a rollback that passed on an old build does not hide a failure on
  a newer one.
- **Unmapped Workers' incidents are hidden, not closed.** Rows are kept as history and still
  show on their landing.
- **The `dispatch` test tests the decision, not the request.** A native fake of `Env` is out of
  proportion; the decision and the merge are what broke.
- **The wrangler check uses `wrangler types`, not `--dry-run`.** It is the same validation
  without a release build per Worker.
- **No `ask_person`.** No question blocked the work.

## Branches

| Branch | What | Kind | Commit | CI `test` |
| --- | --- | --- | --- | --- |
| `claude/ai-agent-XE8RUxKJDU6J` | this plan, README statuses | docs only | the tip after `05081e9` | none needed (docs only; the Lander runs no job for docs) |
| B1 `claude/ai-agent-Fuzcl9MugNWk` | item 2: `split::merge`, `op_needs_bound`, `for_op`; smoke scenario | code (core) | `7607f74` | `c_mYd-9V5In7HKYLy1HLi0` succeeded (31 scenarios, 6 s) |
| B2 `claude/ai-agent-M933ZUIdJ1YQ` | item 1: covered incidents, unmapped ones hidden | code (core: the Lander) | `991e147` | `siR-QoAyITQcpitDKrRGdL` succeeded (30 scenarios, 19 s) |

B1 and B2 both touch `tests/smoke.rs` and core's lane. The Lander rebases only branches that
change different paths, so whichever lands second must first be rebased onto main by hand
(`git rebase main`; the two change different parts of the file). Each landing deploys core
alone (plus whatever the map adds for `src/capabilities`).

## Follow-ups for the person

1. **The wrangler config check (gate change, your approval).** Add `ci/runner/wrangler-config.sh`
   with this content:

   ```sh
   #!/bin/sh
   # Checks each platform Worker's wrangler.toml as the pinned wrangler reads it, without building:
   # a field wrangler refuses (such as `max_instances` on a Durable Object-managed container) fails
   # here rather than at deploy. Each config is copied without its [build] section and read by
   # `wrangler types`, which validates the whole config. The Workers come from ci/workers.toml.
   set -eu
   root=$(cd "$(dirname "$0")/../.." && pwd)
   . "$root/ci/deploy/node.sh"
   work=$(mktemp -d)
   trap 'rm -rf "$work"' EXIT
   # Every Worker pins the same wrangler; one install reads every config.
   tool=$root/src/platform
   (cd "$tool" && npm ci --no-audit --no-fund --loglevel=error)
   dirs=$(sed -n 's/^dir = "\(.*\)"$/\1/p' "$root/ci/workers.toml")
   failed=0
   for d in $dirs; do
       mkdir -p "$work/$d"
       awk '/^\[build\]/{skip=1;next} /^\[/{skip=0} !skip' "$root/$d/wrangler.toml" > "$work/$d/wrangler.toml"
       if (cd "$tool" && WRANGLER_SEND_METRICS=false CI=1 npx --no-install wrangler types \
           "$work/$d/types.d.ts" --config "$work/$d/wrangler.toml" >"$work/$d/log" 2>&1); then
           printf '%s/wrangler.toml: ok\n' "$d"
       else
           printf '%s/wrangler.toml: wrangler refuses it:\n' "$d" >&2
           grep -v '^$' "$work/$d/log" | sed 's/^/    /' >&2
           failed=1
       fi
   done
   exit "$failed"
   ```

   Then in `ci/runner/test.sh`, before `step passed`, add:

   ```sh
   step the wrangler.toml of every Worker, as the pinned wrangler reads it
   sh ci/runner/wrangler-config.sh
   ```

   Add
   `"ci/runner/wrangler-config.sh"` to `[checks] paths` in `ci/workers.toml`, so a change to it
   runs the tests and deploys nothing. This is a change to `ci/`, so it lands as a gate change.
2. **Drop the duplicate platform check (gate change).** In `ci/runner/test.sh`, remove the
   last two lines before `step passed`:
   `step cargo check -p pigeon-platform` and `cargo check -p pigeon-platform`. Update the
   header comment's "and the platform Worker, which builds for WASM only" to say clippy covers
   it. Compare the `test` job's duration before and after.
3. **Review and land** B1, B2 and this docs branch, in any order. Rebase the second of B1 and
   B2 onto main before landing it (see Branches). After B2's core deploy, `land_status` should
   show no `incidents_to_see` except dotest-a's, which is hidden, and the older ones read
   `covered` on their landings.
4. **Check the rehearsal Workers are gone.** `pigeon-dotest-a` and `pigeon-dotest-b` are to be
   deleted by hand in the dashboard, a first (`docs/plans/do-rehearsal.md`). If they still
   exist, delete them.
5. **Decide whether item 4 (a new Worker in one landing) is worth building** before the next
   Worker is added. Until then, the procedure in item 4 is the way.

## Second pass: the branch plan (2026-11-22)

Planner run `wy1pD62aNvwr`, from the person's brief: finish workers-split.md's open items 4
to 13 and this plan's items 4, 6, 8, 10, 11 and 13, safest first, one concern per branch,
nothing landed. Item 12 belongs to the AI agents planner. Items 3 and 7 are the person's gate
changes. The architecture diagram waits for the person. Read on main at `ca548c9`.

**Not built.** The run had 29 minutes of sandbox time. Each executor asks for an hour, so none
could start. The plan below is written so the next planner can hand each branch to an executor
as it stands.

### Facts the plan rests on

- The CI scheduler's `status` op answers the job's `exit_code` and the tail of its output
  (`src/ci/src/scheduler.rs`, `Row::public` and the `status` op). `Lander::job_ended` reads
  `status` but keeps only `state` and `detail` ("exit code N").
- `landings.ordered` is an INTEGER (migration 17, default 1). Bit flags keep old rows' meaning:
  1 is core first, 2 is proxy first.
- The deploy proxy's allowlist is `src/proxies/src/cf_proxy.rs`, in core's and the proxy's
  lanes. The proxy builds its scope (`CfProxy::platform`) from the map compiled into it.
- `Map::affected` (src/core) computes `ordered`. A src/core change deploys all six Workers. New
  Lander rules therefore go in src/capabilities, which is core's lane only.
- The Lander's line merge is strict. So each branch adds its smoke scenario after its own
  existing function (below), never at the end of the file, and no branch edits the plans' Logs
  (only the planner's docs branch does).

### Branches

The migration numbers and anchors below keep the branches from colliding. "After" is the
existing smoke function the new scenario goes directly after.

| # | Branch (concern) | Base | Paths | Lanes | Migration | Smoke scenario, after |
| --- | --- | --- | --- | --- | --- | --- |
| G1 | deploy.sh reports how far it got (gate change) | main | `ci/deploy/deploy.sh` (wrappers if needed) | all six (`shared`) | none | none (shell) |
| L1 | skip the rollback when nothing uploaded | main | land.rs, land/lanes.rs | core | none | `core_land_skips_the_rollback_when_nothing_uploaded`, after `land_attributes_a_failed_deploy` |
| L2 | first-deploy incident text (item 8) | main, parallel to L1 | land.rs `open_incident` text, lanes.rs `verdict` text | core | none | `core_land_first_deploy_incident_text`, after `land_from_check_to_landed` |
| L3 | environment failures blame no landing | L1 | land.rs, land/lanes.rs | core | none | `core_land_environment_failures_blame_no_landing`, after `land_commit_attribution` |
| L4 | proxy-first ordering for allowlist changes | main | land.rs (check), land/lanes.rs `starts` | core | none (bit 2 of `ordered`) | `core_land_deploys_the_proxy_first_for_allowlist_changes`, after `lanes_scope_paths_and_order_only_the_contract` |
| L5 | deploy map from main at deploy time | main | land.rs `deploy_step`, `deploy_grant`, merge.rs or lanes.rs | core | **18**: `ALTER TABLE deploys ADD COLUMN script TEXT` | `core_land_resolves_the_deploy_map_from_main`, after `land_deploys_in_lanes_per_worker` |
| L6 | a new Worker in one landing (item 4) | L4 and L5, rebased together | merge.rs `see`, land.rs check | core | none | `core_land_adds_a_worker_in_one_landing`, after `deploy_proxy_allowlist` |
| L7 | delete `claude/land/<id>` test refs (item 10) | main | land.rs, merge.rs | core | none (marker in `settings`) | `core_land_deletes_test_refs`, after `land_rebases_disjoint_branches_and_lanes` |
| K1 | per-Worker caller keys: design, then step 1 if safe alone | main | new `docs/plans/caller-keys.md`; remote.rs, split.rs | core (step 1) | none | `core_contract_per_worker_caller_keys`, after `contract_signed_callers_and_skew` |
| R1 | core stops exporting `CiScheduler`/`ModelJobs` (item 6): rehearsal plan, then code; **do not land** | main | new section in `do-rehearsal.md`; src/platform | core | none | none |
| R2 | `CiRunner` to the CI gateway (item 13): plan with the handover, then code; **do not land** | main | new `docs/plans/ci-runner-move.md`; src/ci, src/ci-gateway | core, ci-gateway | none | after `ci_route_and_run_on_a_fake_connector` |
| B1 | reuse the build within the deploy job (item 11, first step; gate change) | G1 | `ci/deploy/allowlist.sh`, `deploy.sh` | all six | none | none |

Waves: G1, L1, L2, L4, L5, L7, K1, R1, R2 together. Then L3 on L1, L6 on L4 + L5, B1 on G1.

### Each branch's contract

- **G1.** `deploy.sh` exits 3 if and only if it stopped before its first real `wrangler deploy`
  through the proxy began: the token and URL checks, the grant check, `allowlist.sh` (its build
  and the stand-in runs), `node.sh`, reading `wrangler.toml`. Nothing on Cloudflare changed.
  Once the real deploy has started it never exits 3: a 3 from wrangler there is mapped to 1. A
  refusal before the upload counts as started, to stay conservative. The header comment lists
  the codes. No new API call, so no `standin.mjs` change. Checked by hand: no token gives 3; an
  unreachable proxy gives 3; a stub `npx` that prints `Uploaded` and exits 3 gives 1.
- **L1.** A pure `lanes` function: never uploaded = job state `failed` and `exit_code` 3.
  Anything else may have uploaded, including every job before G1 lands. When nothing
  uploaded *and* the Worker's newest earlier deploy (any purpose) is the succeeded one
  `last_good` names, the incident starts as if the rollback passed. `bisect` goes straight to
  bisecting: with one included landing, that landing is the cause and nothing is redeployed.
  `core_incident` ends at once, core on its good build. Otherwise, as today. The summaries say
  nothing was uploaded. It must not change the two first-deploy strings L2 owns.
- **L2.** When the Worker has no earlier succeeded deploy: "first deploy of <worker> failed;
  nothing was running and nothing is rolled back" at open and in `verdict`. "May still be on a
  failing build: a person must look" stays for a Worker that had a good build but could not be
  returned to it.
- **L3.** A pure classifier over the job's state, exit code and output tail. Environment: job
  state `error` (runner lost, no infrastructure), `cancelled`, or the tail naming a known
  platform fault (Cloudflare `code: 10007`, a Cloudflare 5xx, "the proxy could not be
  reached"). Match conservatively: anything unknown is a code fault, as today; `timed_out`
  stays a code fault. A lane deploy that failed for the environment is deployed once more at
  the same commit with the same landings. If that also fails for the environment, the landings
  end `error` ("environment: <why>; not this landing's fault; land again"), not
  `deploy_failed`. The incident ends in a new state `environment` with no bisect.
  `incident_covered` closes it like `unisolated`. An incident redeploy that fails for the
  environment counts as `Tried::Error` (no blame), not `Tried::Failed`.
- **L4.** A constant in the Lander: proxy-first paths = `["src/proxies/src/cf_proxy.rs"]`. Set
  at check time as bit 2 of `ordered`. `Merged.proxy_first`, and `starts` takes the proxy's name
  (`"proxy"`). While the proxy has not deployed a proxy-first landing, no other lane (core's
  included) deploys a commit at or after it. With both bits: core, then the proxy, then the
  rest. A failed proxy deploy releases the others as a failed core deploy does for core first.
- **L5.** When `deploy_step` submits a deploy, it reads `ci/workers.toml` at the deploy's commit
  through git and takes the Worker's entry. If that fails, it falls back to the compiled map
  and says which it used in the deploy's `detail`. It records the script on the deploy row
  (migration 18). `deploy_grant` answers that script, so the proxy reads no git per call. Rows
  without it fall back to the compiled map. The choice is a pure function that the smoke suite
  tests.
- **L6.** At checking, the map is main's map plus the `[[worker]]` entries the branch *adds*
  (new names only, never changed ones; the map is guarded, so adding one already needs the
  gate approval). A landing that adds a Worker is proxy-first (L4), so the order is core, then
  the proxy (which now knows the script), then the new Worker's first deploy, resolved by L5.
  Binding it in core stays a later landing: a service binding's target must exist first.
- **L7.** When a landing is terminal, its test ref is deleted, best-effort, through the
  Lander's git push (a ref update to the all-zero id). The work is done in the async loop
  beside `push_notes`, a few per alarm. A pure check refuses any ref but
  `refs/heads/claude/land/<id>`. Older refs are cleared the same way, once.
- **K1.** Today one `CALLER_KEY` (Secrets Store) signs every call between core and the five
  capability Workers, so any of them can sign as another. The design: one key per Worker,
  `CALLER_KEY_<WORKER>`, made by `workers_setup`. Core holds all of them; each Worker binds only
  its own. Rollout, each step safe alone:
  1. Core verifies with the Worker's key *or* the shared key and still signs with the shared
     one.
  2. Each Worker binds its own key and verifies either, still signing with the shared one.
  3. Each side signs with the per-Worker key.
  4. The shared key is dropped.

  Each wrangler binding change is checked with `allowlist.sh`. Build step 1 only, and only if
  it needs no person action before it is safe.
- **R1.** Plan first, as `do-rehearsal.md` did. Two throwaway scripts: A transfers a class to B,
  then A stops exporting it, keeping its migrations and its binding with `script_name`. The
  question is whether Cloudflare takes A's upload. Remember incident 5: after a transfer, the
  source's binding must name the new host (`script_name`), or the upload is refused (10061).
  Never add `deleted_classes` for a transferred class. Code: remove the two shims in
  `src/platform/src/lib.rs` and `settings::ci()` / `settings::model_jobs()`. Keep migrations v2
  and v4. Land only after the rehearsal passes, on a quiet queue.
- **R2.** A container class cannot be transferred, so this is a handover, as phase 4 did.
  1. The CI gateway gets a new container class and its own migration.
  2. The scheduler dispatches to a second runner binding while core's `CiRunner` drains.
  3. Core's binding and class are removed in a later landing.

  It needs `containers = [...]` for ci-gateway in `ci/workers.toml` (the proxy's deploy
  scope), which is the person's gate change, and a rehearsal of the container application's
  creation through the proxy (`allowlist.sh` against the stand-in, with any new route in
  `standin.mjs` in the same branch). Code behind the rehearsal; do not land.
- **B1 (item 11, feasibility).** Reusing the *test job's* build is not feasible now. The gate
  runs `cargo clippy` (no code generation) and no release build, so there is nothing to reuse.
  Adding a release build of six Workers to every gate run and an artifact store for jobs (an
  R2 bucket, a scoped token, the proxy's allowlist) is a design decision for the person. What
  is feasible: each deploy job runs the Worker's `[build]` three times today (twice through
  `allowlist.sh` against the stand-in, once for the real deploy). The later two are mostly
  `wasm-bindgen`/optimiser work over an unchanged cargo build. B1 measures them and, if they
  cost more than about 20 s per deploy, builds once and deploys the built output three times
  (a config copy without `[build]`, beside the original so relative paths hold). It also
  checks that what the stand-in saw is what deploys. It is a gate change and deploys all six
  Workers.

### Landing order (safest first)

1. L2, L7: text and best-effort clean-up, core only.
2. L1, then L3 (stacked).
3. G1: a gate change that deploys all six Workers. It is safe before or after L1, because L1
   reads exit 3 only as a hint.
4. L5, then L4, then L6 (L6 is stacked on both). Land them on a quiet queue: they change how
   deploys are ordered and what a deploy may deploy.
5. K1 step 1.
6. B1, after G1.
7. R1 and R2: not to land until their rehearsals pass.

## Log

- 2026-11-22: designed. Read the plans README, workers-split.md (its Log and open items),
  ai-agents.md, do-rehearsal.md, `src/capabilities/src/lib.rs`, `split.rs`, `remote.rs`, the
  Lander (`land.rs`, `land/lanes.rs`, `land/merge.rs`), `src/platform/src/lib.rs` and
  `wrangler.toml`, `ci/workers.toml`, `ci/runner/test.sh`, `ci/deploy/*.sh` and
  `.pigeon/ci.toml`. Read `land_status` and `ci_commit` for `5ced0e0`.
  - Item 3's check was tried here with the pinned wrangler: clean on main for all six Workers;
    it fails on `max_instances` as the deploy did. `wrangler types` on a config that still has
    `[build]` starts the release build, so the strip is needed.
  - Executors: B1 (`Fuzcl9MugNWk`) and B2 (`M933ZUIdJ1YQ`), one concern each.
- 2026-11-22: both built, gate green locally and in CI (see Branches).
  - B2 (`991e147`, $0.11): as designed. An incident whose old summary is null gets a summary
    that starts "; covered by …"; every incident gets a summary when it opens, so this should
    not happen.
  - B1 (`7607f74`, $0.17): as designed, with one departure. The clash line's `console_error!`
    in `split::merge` is compiled for wasm32 only: natively the macro aborts the process, so
    `merge` could not run in the smoke suite. On WASM nothing changes.
  - This run's git grant pushes its own branch but cannot fetch, so the executors' diffs were
    reviewed from their reports and CI, not line by line. The reviewer should read both diffs.
- 2026-11-22: handoff from the Workers-split planner. Items 1 and 2 landed together
  (`3SKi_jbmv5_m6F2y`: commits `f0668a9`, `0baf513`). Items 3 and 7 wait on the person (gate
  changes). Items 4, 6, 8, 10, 11, 12 and 13 are open, unstarted; their next steps are in
  workers-split.md, Open items (handoff).
- 2026-11-22 23:40 UTC: handoff. Items 1 and 2 landed (`f0668a9`, `0baf513`). Items 3 and 7
  (gate changes) wait on the person's approval; item 12 is open in `ai-agents.md`, Open items 2.
- 2026-11-22: second pass, planner run `wy1pD62aNvwr`. Wrote "Second pass: the branch plan"
  for items 4, 6, 8, 10, 11 and 13, and workers-split.md's open items 8 to 12 (rollback skip,
  environment attribution, per-Worker caller keys, map at deploy time, proxy first). Item 11
  was checked: reusing the test job's build is not feasible without an artifact store, and
  B1 is the step that is. Nothing was built: the run had 29 minutes of sandbox time and the
  executors were refused for want of it. Next: a planner with an hour or more per executor
  spawns wave 1 as written.
- 2026-11-23: third pass, planner run `X0NALpyzcsSl`: the branch plan built, nothing landed.
  Each branch passed `sh ci/runner/test.sh` and CI `test`. All of them on current main
  together (L1 and L5 need a two-line merge in `deploy_step`) pass clippy and 53 smoke
  scenarios in 15 s.
  - G1 `claude/ai-agent-wZFCuBPHdEGP` (`d43fe72`), gate change: deploy.sh exits 3 only before
    the real deploy starts (an EXIT trap). The token and URL checks used to exit 2.
  - L1 `claude/ai-agent-qNtIHeBJWuPk` (`ffe0ff7`, on `283c4a1`): rollback skipped when the
    job failed with exit 3. The exit code is kept as `(exit code N)` in the deploy's detail.
    Review found a dropped SQL value in `drive_incidents` and an exit code read for every job
    state; both fixed in `ffe0ff7`.
  - L2 `claude/ai-agent-VgB5OyZrasn4` (`4a6d996`): first-deploy incident text.
  - L4 `claude/ai-agent-cSLCd3tMzvMY` (`9ca1e3c`): proxy first, bit 2 of `ordered`.
  - L5 `claude/ai-agent-5f4y0sNmaJGk` (`604c5cb`): the deploy entry from the map at the
    deploy's commit, migration 18 `deploys.script`.
  - L6 `claude/ai-agent-0X04t8tn5PQ5` (`1a7ca40`): L5 plus a merge of L4, then main's map
    plus the Workers a branch adds. It also edits a comment in
    `src/ai-agents-worker/wrangler.toml`, so it deploys ai-agents too.
  - L7 `claude/ai-agent-2246DknBO8Em` (`3d3f6ba`): test refs deleted, five per alarm.
  - K1 `claude/ai-agent-BL4VOu0OuzAO` (`9fee187`): `caller-keys.md` and step 1, inert until
    core binds a per-Worker key.
  - R1 and R2: rehearsal plans only (do-rehearsal.md, second rehearsal; ci-runner-move.md).
  - Not started: L3 (on L1) and B1 (on G1). The run reached its limit of 12 executors. Two
    were lost to Cloudflare API 500s when core minted their credentials, one went to L1's fix,
    and one rebased the second-pass plan but could not push it (`commit_push` pushes only
    uncommitted changes). This branch carries that rebase instead.

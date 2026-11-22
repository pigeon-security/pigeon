# Clean-up: faster, safer iteration after the split

Status: **planned; items 1 and 2 built on branches, item 9 (docs) on this branch** (2026-11-22).
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
| 1 | Close incidents a later green deploy covers; hide unmapped Workers' incidents | S | low | code | built (branch B2) |
| 2 | Smoke coverage for core's `dispatch` reaching bound Workers' tools | S | low | code | built (branch B1) |
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
- **Fix.** Follow-up 2. It saves the time of a second check of the workspace crates, probably
  20 to 40 s of the 101 s job. Measure it before and after.

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
  ci/ change and a decision about where builds are kept.

### 12. An AI agent run's git grant outlives the run

- **Problem.** A run grant lives 12 hours whatever the run does. ai-agents.md: "Revoking a run's
  grant when it ends is left for later."
- **Fix.** Revoke it when the run ends. That belongs to the ai-agents planner; recorded here so
  it is not lost.

### 13. `CiRunner` still core's

The workers-split plan's open item, with its handover steps. It is large and risky (a container
class cannot be transferred), and it is not clean-up. It is listed so the ranking is complete.

### Looked at, nothing to do

- The smoke suite: 26-odd scenarios in 11 to 24 s, well under 60 s.
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
| `claude/ai-agent-XE8RUxKJDU6J` | this plan, README statuses | docs only | see Log | see Log |
| B1 `claude/ai-agent-Fuzcl9MugNWk` | item 2: `split::merge`, `op_needs_bound`, `for_op`; smoke scenario | code (core) | see Log | see Log |
| B2 `claude/ai-agent-M933ZUIdJ1YQ` | item 1: covered incidents, unmapped ones hidden | code (core: the Lander) | see Log | see Log |

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

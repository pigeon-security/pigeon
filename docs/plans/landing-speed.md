# Landing speed: from green branch to deployed, and CI job overhead

Status: **planned (2026-11-24)**; phases below are landed one by one and each records its
before and after numbers in the Log. Planner run `OSRnBP7PiYg0`.

## Goal

Every later agent run gets cheaper and faster when the time between "the branch is green" and
"landed and deployed" falls (about 10 minutes per landing today), and when a CI job stops
spending most of its time on starting up. Correctness comes before speed: the Lander never
lands something that was not tested for its exact tree.

## What was measured (`ci_jobs` and `ci_commit`, 2026-11-23 and 24)

1. **A `test` job takes 140 to 500 s, about 170 s typically, and the suite itself takes about
   17 s.** What is left is the container start, the checkout and `cargo` building every
   dependency from scratch (clippy for WASM, clippy and the tests natively, the settings
   example, `cargo check -p pigeon-platform`). Example: commit `3ba923257a9d`, executor job
   `-x17msxu9Pjfbt6ktuiLu2` 190 s, Lander job `H5kKoxV76EXxK3x2bcVkaX` 180 s.
2. **The same commit's `test` job runs more than once.** `3ba923257a9d` was tested by its
   executor and again by the Lander. `6a28b96b7b52` ran twice in the Lander
   (`kG9b8Opcl1RLS9g-eQ3JD7` and a retest). `32c2f2b7287e` ran twice.
3. **Deploy scope.** Landing `eXkOoM1zmRYWHXka` changed `ai-agents` and `model-gateway` code
   and deployed all six Workers (core, ai-agents, ci-gateway, model-gateway, proxy, sites), core
   first, about 2.5 minutes each. Its commit `ebfead9` touched `src/core` and `src/contract`,
   which every Worker links (`ci/workers.toml` `[shared]`), so by crate the six were right; but
   the map is written by hand, and a path it does not name (`src/ingest*`, `src/data-worker`)
   counts as shared and deploys everything. The map is not checked against Cargo.
4. **A deploy job takes 130 to 250 s.** On `3ba923257a9d`: core 217 s, ai-agents 140 s,
   ci-gateway 154 s, model-gateway 137 s, proxy 130 s, sites 155 s. Each job builds its Worker
   from scratch, then runs `wrangler deploy` against the stand-in and then for real. The lanes
   also start about 25 to 30 s apart, not together.
5. **Merge to landed.** For `3ba923257a9d` the test job ended at +0 s and the last deploy
   ended at +350 s; with queueing and the lane stagger a landing takes about 6 to 10 minutes.

Baseline to beat (from the above): median `test` job about 175 s; deploy jobs 130 to 217 s;
six deploys for a change to a shared crate, and for any path the map does not name.

## Phases

Each phase is its own landing. Where it changes `ci/` or `.pigeon/ci.toml` (rule
`gate-changes`) the landing waits for the person's delegate; the work ships latent where it
needs the image or a Worker's permissions to change.

### Phase 1: reuse a green test for the same commit (Lander)

When the Lander is about to submit the `test` job for a commit, it first asks CI for jobs of
that repo and commit. A job that is `succeeded`, is the same job name, and ran on the same
runner image is reused: the landing goes straight to its green state and its step text says
`reused test job <id>`. The commit sha is the tree and the `.pigeon/ci.toml` at it, so the job
definition is the same by construction. The runner image is recorded on the job row when the
job is dispatched; a row with no image recorded is never reused (fails closed). After a rebase
onto a moved main the commit differs, so it is tested as today. Nothing but a `succeeded`
`test` job is ever reused, and never a result for another commit.

### Phase 2: where a `test` job's time goes, and a cheaper start

- Timing lines per stage in the job output: the runner (fetch, checkout, script) and each step
  of `test.sh` and the deploy scripts print `[timing] <stage> <seconds>s`.
- Free wins that keep results identical: no incremental compilation, no debug info in the dev
  and test profiles (by environment variable in the scripts, not in `Cargo.toml`), the
  registry fetched once (`cargo fetch --locked`) before the timed steps.
- A warm image: `ci/image/Dockerfile` v4 builds the dependencies for the current `Cargo.lock`
  (WASM and native) into a target directory in the image. The scripts use it only when the
  lockfile's hash recorded in the image equals the checkout's, else they build as today.
  **Latent**: pushing the image and pinning its digest in `src/platform/wrangler.toml` is the
  person's step.
- A shared cache outside the image (sccache or a target tarball on R2, keyed by `Cargo.lock`
  and the toolchain) is designed in the Log, not built: it needs a credential for the job and
  a binding the person must approve.
- Test outcomes are never cached here.

### Phase 3: deploy scope from Cargo

The Lander runs in a Worker and cannot run `cargo`, so the map stays a file the Lander reads,
and the gate checks it against Cargo. A smoke scenario derives, for each Worker, the crates it
builds (its crate and the path dependencies of its normal and build dependencies, transitively,
read from each `Cargo.toml`), the files compiled in (`include_str!` and `include_bytes!` that
leave the crate) and its own config and deploy script, and compares them with
`ci/workers.toml`. A path the map does not name, or a crate a Worker links that its patterns
omit, fails the gate. The map is corrected to match (crates no Worker links get `[checks]` or
their own entry, so they stop counting as shared). Scenarios pin the mapping for representative
paths (`src/models/x.rs` deploys core and model-gateway only; `src/blog/x.rs` core and sites;
`src/proxies/x.rs` core and proxy; `src/ci-gateway/x.rs` ci-gateway only; docs deploy nothing).
Core first when affected is kept.

### Phase 4: deploy job overhead

- The deploy scripts print the same stage timings and use the warm image of phase 2.
- Each deploy job builds its Worker's WASM once (`allowlist.sh` builds and `deploy.sh` reuses
  it, as today); the build's hash is printed as `artifact sha256 <hex>`.
- Latent, off by default: the Lander keeps the last deployed artifact hash per Worker and a
  deploy whose artifact is byte-identical to it uploads nothing.
- The lanes of one landing start in the same Lander step instead of about 25 s apart, with
  core still first when the contract or the map changed.

## What stays as it is

- Every landing is tested for its exact tree. Docs-only landings still merge without tests.
- The Lander reads the map from main, never from the branch.
- No secret goes into any commit, and nothing here changes the policy or the Claude settings.

## Log

- 2026-11-24: plan written from the measurements above.
- 2026-11-24: timing lines added. Every job script prints `[timing] <stage> <seconds>s` to its
  output: `ci/runner/test.sh` (stages `guard`, `fetch`, `fmt`, `clippy`, `smoke-build`,
  `smoke-run`, `settings`, `check-platform`, then `total`), `ci/deploy/deploy.sh` (`grant`,
  `node`, `deploy` per attempt, `published`, `total`), `ci/deploy/allowlist.sh` (`node`, `build`,
  `allowlist`) and `ci/runner/entrypoint.sh` (`fetch`, `checkout`, `script`, `total`; these go to
  the container log, not to the job's own output). Free build wins, by environment variable in
  test.sh and allowlist.sh: `CARGO_INCREMENTAL=0`, `CARGO_PROFILE_DEV_DEBUG=0`, and one
  `cargo fetch --locked` before the timed builds. Same checks and flags otherwise.

- 2026-11-24: phase 3 built. The deploy scope is now checked against Cargo by two smoke
  scenarios in `tests/smoke/land.rs`.
  - `land_scope_map_matches_the_cargo_graph` derives, for each Worker in `ci/workers.toml`, the
    crates it builds (path dependencies of normal, build and target dependencies, transitively),
    the `include_str!` and `include_bytes!` files that leave a crate, its `wrangler.toml`, assets
    and deploy script, and fails, naming the Worker and the path, when the Worker's patterns (or
    `[shared]`) do not match one. It uses the Lander's own `matches`. It also fails for a crate
    directory under `src/` that no entry names. The Lander at run time is unchanged: a path
    nothing names is still shared.
  - `land_scope_pins_representative_paths` pins the scope of sample paths on `Map::classify`.
  - Map correction: `src/ingest/*` added to core (pigeon-capabilities links pigeon-ingest);
    `src/models/*` added to ai-agents (pigeon-ai-agents links pigeon-models), so a models change
    now deploys core, model-gateway and ai-agents; `src/data-worker/*` and `src/ingest-runner/*`
    added to `[checks]`, since no Worker links them. Not changed: core keeps `src/blog/*` and
    `src/ai-agents/*`, because its build script embeds every crate's sources for the architecture
    page, though Cargo shows them only as dev-dependencies of capabilities.

  - CI scheduler: migration 16 adds `jobs.image`. When the containers connector dispatches a
    job, the row records the base image string pinned in `src/platform/wrangler.toml` under
    `[containers.images.base]`, read at compile time by `reuse::runner_image`. It is recorded
    only when it has a full `@sha256:<64 hex>` digest. Other connectors record none, and a
    requeue clears it. `image` shows in `ci_status` and `ci_jobs`.
  - `ci_submit` takes an optional boolean `reuse`. When true, the job has no `token = true`, and
    the job is not pinned to another connector, the gateway asks the scheduler for the newest job
    with `state = succeeded`, the same repo, the same job name, the same full lowercase 40-char
    commit and an `image` equal to the one recorded now. If one exists, the answer is
    `{id, state: "succeeded", reused: true}` and no job is created. Otherwise it submits as before.
    Without `reuse` nothing changes.
  - "The same image" means the same string, digest included. A row with no image, a different
    image, or an image of an older base is never reused. Never reused either: a job in any state
    but `succeeded`, a `token = true` job, another commit (a rebase gives a new sha), another job
    name, another repo.
  - The Lander sends `reuse: true` on the `test` dispatch only, never for deploy jobs. On
    `reused: true` it writes `reused test job <id>` as the landing's detail and keeps the id in
    `test_job`; `job_ended` then reads the job's real state as before. Cancelling a landing calls
    `ci_cancel` on that id; a finished job answers "it had already finished" and is left alone,
    so a reused job is never changed by it.
  - `ci/workers.toml`: `src/platform/wrangler.toml` is added to the `ci-gateway` paths because it
    is now compiled into that Worker.

- 2026-11-24: warm build and artifact hash built. The warm image is **latent**.
  - `ci/image/Dockerfile` is now v4. After the v3 steps it copies only `Cargo.toml`,
    `Cargo.lock`, `rust-toolchain.toml` and each member's `Cargo.toml` (no repository source),
    writes an empty `lib.rs` and `main.rs` per member (and a `build.rs` where a member has build
    dependencies), and builds the dependencies into `/opt/pigeon-target` with the environment
    of `test.sh`: WASM clippy and check, the native clippy and smoke build, the `capabilities`
    examples, and a release build of each `cdylib` Worker. It then removes pigeon's own stub
    artifacts and writes `/opt/pigeon-target/WARM_KEY`. The image was not built in the sandbox
    (no Docker there), so the layer is untested.
  - `ci/runner/warm.sh` is sourced by `test.sh` and `allowlist.sh`. `warm_target` compares
    `WARM_KEY` with the key for the checkout. **Key rule:** the sha256 of `Cargo.lock`, a space,
    and the sha256 of `rustc -Vv`. If they are equal it copies the directory (`cp -a`) to a
    temporary directory, exports `CARGO_TARGET_DIR` there and prints
    `[warm] using the image's dependency cache`. Otherwise it prints `[warm] cold build:
    <reason>` and changes nothing; a failed copy falls back the same way. It never fails the
    script and prints `[timing] warm <seconds>s`. Without the image (local runs, today's image)
    the build is as before.
  - `allowlist.sh` prints `artifact sha256 <hex>`, the hash of `build/` it already recorded, once
    per deploy (`deploy.sh` runs it). Exit codes and the finish trap of `deploy.sh` are unchanged.
  - A smoke scenario pins the above and runs `warm.sh` with no image directory.
  - **The person's steps to turn it on:**
    1. From the repository root: `docker build --platform linux/amd64 -f ci/image/Dockerfile -t
       pigeon-ci-base:v4 .` and fix the layer if the build finds a problem.
    2. From `src/platform`, with the pinned wrangler: `npx wrangler containers push
       pigeon-ci-base:v4`.
    3. Put the pushed `@sha256:` digest in `src/platform/wrangler.toml` under
       `[containers.images.base]`, and land that change.
    4. Rebuild and re-pin the image whenever `Cargo.lock` or the Rust toolchain changes; until
       then jobs print `[warm] cold build` and build as before.
    5. Check one job prints `[warm] using the image's dependency cache`, and that `worker-build`
       finds its output when `CARGO_TARGET_DIR` is set.
  - Designed, not built: a shared cache outside the image (sccache or a target tarball on R2,
    keyed by `Cargo.lock` and the toolchain). It needs a credential for the job and a binding
    the person must approve. Also not built: skipping a deploy whose artifact is identical to
    the last one (the Lander would keep the hash per Worker); it stays latent and out of scope.
- 2026-11-24: **before and after, first measurement.** Source: `ci_commit` and `ci_status` for
  commit `a01c4f17f50c` (the timing change itself, landed by `LGKMSYlaLVakkJT-`). Phase 1
  (reuse) was not live for it, and the warm image is not built, so this shows only the timing
  lines and the free build wins (no incremental build, no debug info, one `cargo fetch`).

  | Measure | Before (plan baseline) | After (`a01c4f17f50c`) |
  | --- | --- | --- |
  | Lander `test` job | about 175 s median (140 to 500 s) | 147 s (`vKH-aG39dy8NmjrfMxp_uW`) |
  | Executor `test` job | 190 s (`3ba923257a9d`) | 163 s (`PFpt0VcXIRVOU87MzelSnR`) |
  | Deploy core | 217 s | 205 s |
  | Deploy ai-agents | 140 s | 133 s |
  | Deploy ci-gateway | 154 s | 307 s (one slow run) |
  | Deploy model-gateway / proxy / sites | 137 / 130 / 155 s | 134 / 126 / 135 s |
  | Deploy cloud-gateway | not measured | 125 s |

  Where the time goes, from the new lines. Last stages of the Lander `test` job: `smoke-run`
  12 s, `settings` 1 s, `check-platform` 13 s, `total` 145 s, so the dependency builds
  (clippy, `smoke-build`) are about 110 s of it. Deploy core: `deploy` 14 s, `published` 1 s,
  `total` 205 s. Deploy ai-agents: `allowlist` 52 s, `node` 0 s, `deploy` 10 s, `total` 132 s.
  So a Worker's own `wrangler deploy` is 10 to 14 s; the build and the stand-in check are the
  rest. That is the target of the warm image. The numbers are single runs, not medians; the
  tail of the output is all `ci_status` keeps, so earlier stages are not shown. The effect of
  reuse (phase 1) and of the narrower scope (phase 3) is not in this table: the next landings
  carry it. Landing `LYb8T6cNuoHdzlct` was refused on a docs conflict and landed instead as
  `ZCYqUmC-4Ua9qF4x`, which deployed four Workers (ai-agents, ci-gateway, core, proxy), not six.

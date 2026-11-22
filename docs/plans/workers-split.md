# Workers split: capabilities as their own Workers, landing in lanes

Status: **complete** (2026-11-22). All five Workers are live and bound, the proxy is cut over,
and deploy lanes run with attribution (phase 5). Both Durable Object transfers are done:
`ModelJobs` to `pigeon-model-gateway` and `CiScheduler` to `pigeon-ci-gateway`. Later work is
listed in the Log's open items: the CI runners' move, then cleanup.

## Goal

The person's words: make non-core capabilities independent of core, so each can be iterated,
deployed and invoked on its own, and remove the merge and landing bottleneck, without separate
repositories. "Seed a planner session for the split, parallelizing the work through executors so
it can land at the same time."

Hard lines:
- No new policy rules unless the person asks for them.
- Tests only in `tests/smoke.rs`. Scenarios may be added, and each Worker's share of the suite
  stays under 60 s.
- A landing that changes `.pigeon/policy.toml` or `.claude/settings.json` needs their passkey.
- One repository. Artifacts is the source; GitHub mirrors `main` only.

## What is true today (read 2026-11-22, main at `65fc925`)

- **One Worker.** `pigeon-platform` (`src/platform`) hosts every capability listed in
  `settings::capabilities()`. It has 5 Durable Object classes: `Directory`, `CiScheduler`,
  `CiRunner` (with a container), `ModelJobs` and `Lander`. These are created by legacy
  `[[migrations]]` v1 to v5, all `new_sqlite_classes`. It also has one queue producer
  (`CI_EVENTS`), one queue consumer (`pigeon-source-events`, set up by `setup_source`), and 5
  Secrets Store bindings.
- **Three crates.**
  - `pigeon-core` holds the building blocks.
  - `pigeon-capabilities` holds every capability plus the `Capability` trait and `dispatch`.
  - `pigeon-platform` is the Worker: `settings.rs` and the DO shims.

  `build.rs` embeds every `src/*/src/**.rs` and `src/*/wrangler.toml` for the architecture page.
- **Capabilities reach each other in-process.**
  - `land` calls CI's tools through `dispatch` and reads `CI_SCHEDULER` directly.
  - `ci` and `ci/containers` call `CI_SCHEDULER` and `CI_RUNNER`.
  - `models/runner` calls `MODEL_JOBS`.
  - `users` (Directory) is read by approvals, oauth, linked and the proxies.
  - The deploy proxy (`CfProxy`) allows exactly one script, `pigeon-platform`.
- **The Lander** holds one live landing:
  `checking → testing → [awaiting_approval] → merging → deploying → landed`.
  - A second `land` is refused while one is live.
  - `merging` needs the branch to be a fast-forward of `main`.
  - The lock spans test (about 2.5 min) and deploy (about 3.5 min).
  - After deploy it syncs the portal and runs the Buildkite setup.
- **Guarded paths** (`land::state::GUARDED`): `.pigeon/**`, `ci/**`, `.cargo`,
  `rust-toolchain.toml`, the npm files and `.claude/settings.json`. A change to one is asked of the
  policy as `change_policy` (passkey) or `change_gate` (delegate).
- **In flight elsewhere.** Neither branch is landed. Both add code that belongs to the sites
  Worker.
  - The blog planner (`session_015XFA1ojbVbWeRgkAiZZxLr`, branch `claude/blog-phase1b`) is adding
    `pigeon_core::blog`. It is a pure renderer, "uses no other module of pigeon-core, so it can
    move to the sites Worker as it is".
  - The linked-accounts planner (`session_015XFA1kti5zV4yt43rtNdxY`, branch
    `claude/instance-plan`) is adding `instances/plan.rs`, `instance_plan` and `instance_apply`,
    and `site_worker.js`.

### Cloudflare, checked 2026-11-22

From developers.cloudflare.com:
- durable-objects/reference/durable-object-class-migrations-legacy
- durable-objects/reference/durable-objects-migrations
- containers
- workers/runtime-apis/bindings/service-bindings

What they say:
- **Transferring a DO class.** pigeon uses legacy migrations, so it uses
  `[[migrations.transferred_classes]] from, from_script, to` in the destination's config. The
  destination must export the class and must not create it first. Stored data comes along. The
  source needs no delete migration.
- **Old bindings.** The source's old bindings "automatically forward to the updated destination
  class". A binding can also name the class elsewhere with `script_name`.
- **Atomic, no rollback.** Migrations are atomic and "cannot be gradually deployed". "Rollbacks
  cannot cross a lifecycle change". Transferring back is not documented.
- **Not documented.** Nothing covers object IDs and alarms surviving a transfer, in-flight
  requests during one, or transfer of a container-enabled class.
- **Service bindings.**
  - A Worker reached only through bindings needs no public URL.
  - The target deploys before the caller.
  - Each call counts toward the subrequest limit, with at most 32 Worker invocations per
    request.
  - The callee's work is cancelled with the caller's context unless it uses `waitUntil`.

So every DO move below is rehearsed on throwaway scripts first. A container class is recreated,
never transferred.

## End state

Five platform Workers in one repository, named by the person: `pigeon-platform` (core),
`pigeon-ci-gateway`, `pigeon-model-gateway`, `pigeon-sites` and `pigeon-proxy`.

| Worker | Script | Owns | Durable Objects | Secrets |
| --- | --- | --- | --- | --- |
| core | `pigeon-platform` | identity (users, passkeys, oauth), policy, approvals, dispatch, secrets, instances records, linked accounts, land, source and mirror, the console shell, architecture, the pigeon.dev page, the proxies (until phase 6) | `Directory`, `Lander` | `CF_PARENT_TOKEN`, `GITHUB_MIRROR_TOKEN`, `CF_OAUTH_CLIENT_SECRET`, `LINK_KEY`, `CALLER_KEY` |
| ci | `pigeon-ci-gateway` | gateway, scheduler, runner and container, Buildkite setup and webhooks, `ci_*` and `containers_images` | `CiScheduler`, `CiRunner` (+ container) | `BUILDKITE_API_TOKEN`, `CALLER_KEY` |
| models | `pigeon-model-gateway` | AI Gateway, `models_*`, model jobs | `ModelJobs` | `CALLER_KEY` |
| sites | `pigeon-sites` | `blog@1` renderer, instance site deploys (render, R2 upload, routes, subdomains), `instance_plan` / `instance_apply` | none | `CALLER_KEY` |
| proxy | `pigeon-proxy` | `/git/`, `/cf/` on proxy.pigeon.dev | none | `CALLER_KEY` (never the parent token: scoped credentials and checks from core) |

A capability Worker never holds the parent token or a linked account's OAuth tokens. It asks
core for a short-lived, scoped token through the contract's `credential` op (below). Core mints
the token and the caller gets it only for the call in hand. The exception to decide in phase 3:
the model job runner streams for minutes, and may hold a minted gateway token for the job's
lifetime.

## The contract

Every capability Worker answers one interface over a Service Binding. It is defined once in a
new small crate, `pigeon-contract` (`src/contract`), which both sides depend on. The crate holds
wire types and helpers only: no capability code and no Worker.

Requests are `POST https://capability/<op>` with JSON, through `env.service(BINDING).fetch`.
Fetch, not RPC: workers-rs supports it today, and the payloads are already JSON.

| Op | Request | Answer |
| --- | --- | --- |
| `manifest` | `{}` | `{contract: 1, worker, commit, capabilities: [{name, tools, sections, routes, footprint, secrets, wants}]}` |
| `call` | `{caller, capability?, tool, args}` | `{ok: value}` or `{error: {message, kind}}` |
| `section` | `{caller, capability, id, query}` | the section's data |
| `fetch` | the original request, forwarded under its route | the response, as is |
| `events` | `{messages: [...]}` | `{done: {capability: summary}}` |
| `secret` | `{caller, capability, op: check\|set\|expiry, name, value?/id?}` | as the trait's methods |

Back the other way, a capability Worker calls core through its `PLATFORM` binding:

| Op | Request | Answer |
| --- | --- | --- |
| `credential` | `{caller, purpose: {kind, instance?, scopes}}` | `{token, account_id, expires_at}`: a minted Cloudflare token (under 15 min), or a linked account's access token, already refreshed, with its own expiry (at most 1 h) |
| `dispatch` | `{caller, tool, args}` | a core tool's answer, through core's gate (for example ci asking `users_whoami`) |

Rules:
- **Core is the front door.** It authenticates (`/mcp` OAuth, Access, passkeys), then runs
  `approvals::gate` in `dispatch` before forwarding a `call`. A capability Worker never runs the
  gate itself and never sees a request core has not let through.
- **Signed caller context.** `caller` is
  `{user, acting_for, approval?, iat, exp(≤60 s), nonce}`, HMAC-SHA256 signed with `CALLER_KEY`.
  This is a Secrets Store secret bound to every platform Worker, made by core's setup like
  `LINK_KEY`. The receiver checks the signature and expiry and refuses otherwise.
  - Service bindings are already private. The signature is the second wall: a route added by
    mistake to a capability Worker still serves nobody.
  - Webhooks (Buildkite's) are forwarded with no caller. They check their own HMAC, as today.
- **No public surface on capability Workers.** Each sets `workers_dev = false`,
  `preview_urls = false` and has no routes or custom domains. Their paths
  (`/ci/hook/buildkite`, `/sites/…`) are routes core forwards from `manifest.routes`.
- **Discovery.**
  - Core reads each binding's `manifest`, caching it in the isolate's memory for 60 s.
  - `tools/list`, `owner()`, the console's sections, the secrets page, the architecture model
    and the queue fan-out all merge local capabilities with remote manifests.
  - A capability deploy therefore needs no core deploy. The portal sync runs after either.
  - A binding whose manifest fails is left out of `tools/list` and shown as `unreachable` on the
    architecture page. A call to one of its tools fails with that reason.
- **Version skew.** `contract` is an integer.
  - Core speaks contract `N` and accepts manifests of `N` and `N-1`.
  - A capability Worker answers ops it does not know with `{error: {kind: "unknown_op"}}`, and
    core treats that as "none".
  - Fields are only added. A removal needs a contract bump and one release in which both
    versions are spoken.
  - Smoke scenario `contract_skew`: core-side merge with an old manifest, an unknown op and a
    missing field.
- **Limits.** `/mcp → core → capability` is 2 invocations. `→ core credential` is 3. Nothing may
  chain capability to capability: a capability that needs another capability's tool asks core's
  `dispatch`.

`pigeon-capabilities` gains `remote::Remote`, a `Capability` built from a manifest entry and a
binding. With it, `dispatch`, `owner`, the console and the architecture stay one code path.
Capability Workers implement `pigeon_contract::Service` (manifest, call, section, fetch, events,
secret) with a `serve(req, env, service)` entry that does the routing and signature check.

## Repository layout and the path map

New crates, one per Worker. `src/capabilities` stays core's.

```
src/core/          pigeon-core          shared building blocks
src/contract/      pigeon-contract      the contract (new; shared)
src/capabilities/  pigeon-capabilities  core's capabilities, Remote (core)
src/platform/      pigeon-platform      core Worker
src/sites/         pigeon-sites         sites Worker: lib + wrangler.toml (phase 2)
src/proxy/         pigeon-proxy         proxy Worker (phase 6)
src/models/        pigeon-model-gateway        models Worker (phase 3)
src/ci/            pigeon-ci-gateway            ci Worker (phase 4)
```

A capability Worker's crate depends on `pigeon-core` and `pigeon-contract` only, never on
`pigeon-capabilities`. Otherwise a core change would silently change its binary.

`ci/workers.toml` is the path map:

```toml
# Which Worker each path belongs to. The first matching entry wins; a path no entry matches
# belongs to `shared`. Read from main by the Lander, never from the branch being landed.
version = 1

[docs]                       # merge only: no tests beyond the docs check, no deploy
paths = ["docs/*", "README.md"]

[shared]                     # every Worker's tests; deploys core first, then the rest
paths = ["src/core/*", "src/contract/*", "Cargo.toml", "Cargo.lock", ".cargo/*",
         "rust-toolchain.toml", ".pigeon/*", ".claude/*", "ci/*"]

[[worker]]
name = "core"
script = "pigeon-platform"
dir = "src/platform"
paths = ["src/platform/*", "src/capabilities/*"]
test_job = "test"            # today's job, unchanged
deploy_job = "deploy"
after = ["architecture", "portal_sync"]

[[worker]]
name = "sites"
script = "pigeon-sites"
dir = "src/sites"
paths = ["src/sites/*"]
test_job = "test-sites"
deploy_job = "deploy-sites"
after = ["portal_sync"]
```

`tests/smoke.rs` is not in any Worker's paths. Every change to it would otherwise be `shared`. It
is test-shared instead:
- Scenarios are named by Worker: `core_…`, `sites_…`, `models_…`, `ci_…`, `contract_…`.
- A Worker's test job runs its prefix plus `contract_`.
- A change to smoke.rs runs the jobs of the Workers whose scenarios it changed.
- It never forces a core-first deploy.

The whole binary still compiles in every job, so a compile error anywhere fails all of them.

The map is under `ci/`, so a change to it is a `change_gate`, which rule `gate-changes`
gives to the person's delegate. No new rule is needed. (`.pigeon/` was the first choice, but no
rule covers a new file there, so every change would have waited for the person's passkey.) The
Lander reads the map at main, so a branch never chooses its own lane.

## Landing in lanes

The Lander keeps its one Durable Object and its state machine. It gains a queue and splits the
lock.

```
land ─► checking ─► testing ─► [awaiting_approval] ─► ready ─► merging ─► merged ─► deploying(w…) ─► landed
                   │            │                                 │ main moved in overlapping paths: retest
                   ▼            ▼                                 ▼ conflict on a path both changed
                refused     test_failed                        rebase_refused
```

Rules:
1. **Queue, never refuse.** `land` always inserts, and the landing starts `checking` at once.
   - A branch that already has a live landing is refused until that one is cancelled.
   - `land_status` shows the queue (every live landing, oldest first).
   - `land_cancel` works until `merging` (`ready` included).
2. **Classify at check.** Changed paths come from `main..tip` against the map read at main.
   - Every path matches `docs`: the landing is docs-only.
   - Otherwise it is a set of affected Workers, with `shared` meaning all of them.
   - Guarded paths go to the policy exactly as today (passkey or delegate). Nothing about
     approvals changes.
3. **Test outside the lock, in parallel.**
   - Each landing past `checking` starts its affected Workers' test jobs at once, at its
     own tip.
   - Docs-only runs no job: nothing it touches is compiled or deployed.
   - Several landings test at the same time, bounded by the CI connectors' capacity.
4. **The lock covers only the swap of main.** One landing at a time, in queue order among those
   `ready`:
   - **Tip still a fast-forward:** move main (compare-and-swap on the old id).
   - **Main moved:** the Lander rebases the branch itself, on path sets.
     - Let B be the paths main changed since the landing's base, and A the paths the branch
       changed.
     - If A ∩ B = ∅, the new tree is main's tree with A's blobs, committed with the branch's
       message and author, committer `pigeon <land@pigeon.dev>`, and parent main. No content
       merge happens.
     - If they overlap only in `tests/smoke.rs` or `docs/plans/README.md`, a line-level three-way
       merge is tried (`pigeon_core::merge3`). A clean merge counts as overlap; a conflict is `rebase_refused`, with the
       paths named.
     - Other overlap is also `rebase_refused`. The agent rebases and pushes again; rewriting a
       session branch is already allowed.
   - **Retest only on overlap.** If B touches a Worker the branch affects, or anything `shared`,
     the landing goes back to `testing` at the rebased commit, which the Lander pushes as
     `refs/heads/claude/land/<id>` so CI can fetch it as a branch tip. Otherwise it merges at once.
   - Agents never force-push to make a landing fit.
5. **Deploy after merge, in lanes.**
   - Each affected Worker gets a deploy in its lane: one at a time per Worker, parallel across
     Workers.
   - **Deploy latest.** A lane that finds a newer merged commit already queued for it skips to
     that one. A landing whose deploy was superseded ends `landed (deployed by <id>)`.
   - **Shared changes** deploy core first. Then the other lanes run, at the same commit.
   - **Docs-only** ends `landed` at merge. The GitHub mirror still runs.
   - **Post-deploy steps per Worker** (`after`):
     - core: the architecture page check (`deploy.sh`'s `published`) and the portal sync;
     - ci: the Buildkite setup (today's `setup_connectors`) and the portal sync;
     - models and sites: the portal sync after core's 60 s manifest cache has expired, or core is
       asked to drop it (`manifest_refresh`, a core tool).
6. **What stays.**
   - main is forward-only, and every commit on it passed the tests of what it touched: either
     at that exact tree, or at a tree that differs from it only in paths of other Workers whose
     own tests passed.
   - A note under `refs/notes/land` names which tree each Worker was tested at.
   - Live checks after each deploy stay.
   - A failed deploy leaves main where it is, as today. The lane stops and later landings for
     that Worker wait until a person or a fix lands.
7. **Deploy grants per Worker.**
   - `CfProxy` allows the scripts named in the map at the deployed core, not one constant.
   - Its container calls are scoped to the script being deployed.
   - The deploy job token's grant names `{landing, worker, commit}`, and the proxy takes it only
     for that script.

## Phases

Each phase is one or a few landings, checked live before the next starts. Executors are AI
Gateway jobs (`models_submit`): `claude-sonnet-5-5` for code, `claude-haiku-5-5` for mechanical
moves. Each task gets whole files, the contract and the surrounding code. I read every line
returned and run `cargo fmt`, clippy with warnings denied, the smoke suite and the WASM check
myself before committing.

### Phase 1: the contract, dispatch from bindings, and the Lander's queue

Two branches that land back to back. They touch different files and can be built in parallel.

**1a `claude/split-contract`** (core and shared; no live change beyond core):

| Task | Files | Executor |
| --- | --- | --- |
| T1 | `src/contract`: wire types, `CallerContext` sign and verify (HMAC through WebCrypto, as `crypto.rs`), `Service` trait, `serve()` router, error kinds | sonnet |
| T2 | `pigeon-capabilities::remote`: `Remote` from a manifest, manifest cache (60 s, per isolate), `credential` and `dispatch` handlers on core | sonnet |
| T3 | `dispatch`, `owner`, `mcp_post`'s tool list, console sections, secrets page, queue fan-out and the architecture model all merge remote capabilities; `manifest_refresh` tool | sonnet |
| T4 | Smoke: `contract_sign_verify`, `contract_skew`, `contract_merge_tools` (pure; no Workers runtime) | haiku |
| T5 | `CALLER_KEY` setup (as `LINK_KEY` in `linked_setup`), bound after it exists | me |

With no bindings configured yet, behaviour is unchanged. That is the safety of 1a.

**1b `claude/land-queue`** (core):

| Task | Files | Executor |
| --- | --- | --- |
| T6 | `land/state.rs`: `queued`, `ready`, `merged`, `rebase_refused`, and the moves | sonnet |
| T7 | `ci/workers.toml` reader in `pigeon-core` (pure: map, classify, docs-only, overlap), `docs` job | sonnet |
| T8 | `land.rs`: queue, parallel test jobs outside the lock, CAS merge, path-set rebase (tree build with `gitobj`), `refs/land/<id>`, docs-only skip | sonnet, then me (the Lander is the riskiest file) |
| T9 | Smoke: `core_land_queue_order`, `core_land_rebase_disjoint`, `core_land_rebase_overlap_refused`, `core_land_docs_only` | haiku |

These rules need care:
- `ci/workers.toml` and the `docs` job in `.pigeon/ci.toml` are gate changes (delegate).
- The first landing with a new Lander is landed by today's Lander, so this phase uses today's
  flow.
- The Lander's SQLite gets migrations 5 and on (only appended, never edited).
- Live landings in today's states map one to one.

### Phase 2: sites, the pilot Worker

This waits on the blog and linked-accounts planners: `pigeon_core::blog` has landed;
`instance_plan`/`instance_apply` have landed in core.

Refined 2026-11-22 after reading the code. Phase 2 is three landings, in this order.

1. **2a, core learns a second Worker.** Core only, no new Worker yet.
   - The map lists `pigeon-sites` (dir `src/sites`, deploy job `deploy-sites`).
   - Deploy jobs per Worker.
     - The Lander deploys a landing's affected Workers one by one, in `deploy_order`.
     - The deploy lane deploys the newest merged commit for the union of the Workers that it
       and every older merged landing it supersedes affected, so no Worker's change is lost.
   - The deploy grant names `{landing, worker, commit}`. The deploy proxy takes a job token
     only for that Worker's script.
     - A session's proxy credential may deploy any script in the map built into core.
     - Each script's queues and container classes come from its own `wrangler.toml`.
   - CI:
     - `ci/deploy/deploy.sh` takes the Worker's directory (`PIGEON_WORKER_DIR`, default
       `src/platform`); the architecture check runs for core only.
     - `ci/deploy/deploy-sites.sh` sets it.
     - `[jobs.deploy-sites]` in `.pigeon/ci.toml`.
     - All three are gate changes (delegate).
   - `workers_setup` runs live once afterwards, to make `CALLER_KEY`.
2. **2b, the `pigeon-sites` Worker.** `src/sites` only.
   - The crate, with `wrangler.toml`: no routes, `workers_dev = false`, `CALLER_KEY`, and a
     `PLATFORM` service binding to `pigeon-platform`, which already exists.
   - `serve()` implements `pigeon_contract::Service`.
   - The blog renderer moves from `pigeon_core::blog` as it is.
   - First tool: `site_render`, a dry run. It renders a consumer repository at a commit (read
     through core's `dispatch` of `source`/GitHub reads) and answers the files and digest. It
     proves a remote tool end to end.
   - Its smoke scenarios are prefixed `sites_`.
   - It deploys only `pigeon-sites`.
3. **2c, core binds it.** `[[services]] SITES = pigeon-sites`, `CALLER_KEY`, and
   `BINDINGS = [SITES]`. Core only. Live check: `tools/list` holds `site_render`, and calling
   it round-trips the signed caller.

Then the first independent landings: a docs change and a sites change landed at the same time,
the second deploying only `pigeon-sites`.

Departure from the first design: `instance_plan`/`instance_apply` stay in core for now. They
need the instance record, its ledger and the passkey gate, which are core's. They move once
those are reachable through the contract (`dispatch` of a record read and a ledger write). The
new deploy step (render, R2 upload, `live`) is built directly in `pigeon-sites` with the
linked-accounts planner, using `PLATFORM.credential`.

Deploy order: core first, as agreed. A Service Binding's target must exist before the binder
deploys, which is why the binding is a landing of its own (2c).

### Phase 3: models

| Task | What |
| --- | --- |
| T16 | `src/model-gateway` crate, moving `models.rs`, `models/*` and `ai_gateway` callers. Gateway and parent tokens come through `credential` |
| T17 | Rehearsal, before anything live: two throwaway scripts in pigeon's account (`pigeon-dotest-a`, `pigeon-dotest-b`) with a SQLite class holding rows and a pending alarm. Transfer a→b with `transferred_classes`, then check rows, ids (`idFromName`) and the alarm. Then delete both. These deploys need names the proxy does not allow, so the person deploys them once with a `deploy_token`, or the map gains them for the rehearsal |
| T18 | `pigeon-model-gateway` with `[[migrations]] tag = "v1"`, `transferred_classes = [{from = "ModelJobs", from_script = "pigeon-platform", to = "ModelJobs"}]`. Core's `MODEL_JOBS` binding gains `script_name = "pigeon-model-gateway"` in the same release and stops exporting the class after the transfer |

Order, rollback and fallback:
- **Order:**
  1. Drain: no running model job. `models_status` shows none, and `models_submit` refuses for
     the window.
  2. Deploy `pigeon-model-gateway` (the transfer happens here).
  3. Deploy core with the `script_name` binding and `MODELS` service binding.
  4. Live check: `models_submit` haiku, then `models_result`.
- **Rollback:** a transfer cannot be rolled back. Before step 2, cancelling is "redeploy without
  the transfer". After it, roll back by fixing forward in `pigeon-model-gateway`. Core's binding keeps
  pointing there. If `pigeon-model-gateway` cannot serve, a new class `ModelJobs2` in core
  (`new_sqlite_classes`) takes new jobs, and history stays readable through the old binding.
  Model jobs are minutes long, so losing in-flight ones is the worst case, and step 1 avoids it.
- **Fallback if the rehearsal shows ids or alarms lost:** no transfer. `pigeon-model-gateway` gets a new
  class. Core keeps `ModelJobs` read-only until its last job ends, then a later release deletes
  it (`deleted_classes`).

### Phase 4: ci

| Task | What |
| --- | --- |
| T19 | `src/ci-gateway` crate: `ci.rs`, `ci/*`, the Buildkite setup, the containers connector and the scheduler and runner shims from `src/platform/src/lib.rs`. The Buildkite webhook moves to a forwarded route |
| T20 | The Lander's CI use goes through the contract: `dispatch` of `ci_submit`/`ci_status`/`ci_cancel` and a `job_grant` op. It no longer reads `CI_SCHEDULER` directly |
| T21 | The deploy proxy's `deploy_caller` asks ci for job grants over the binding |

Durable Objects:
- **`CiScheduler` (state: jobs, grants, notes)** is transferred like `ModelJobs`, after phase 3
  proved the move. It is drained first: no running or queued job, and `ci_submit` refuses for the
  window. A landing's test can't run during the window, so it is scheduled when the queue is
  empty.
- **`CiRunner` (container)** is not transferred; the docs say nothing about container classes.
  `pigeon-ci-gateway` declares a new class `CiRunner` with `new_sqlite_classes` and its `[[containers]]`.
  Runner objects are per job attempt, so nothing needs to survive. Core deletes its `CiRunner`
  (`deleted_classes`) one release later, once the drain shows no live attempt.
- **Order:**
  1. Drain.
  2. Deploy `pigeon-ci-gateway` (transfer plus the new runner class). The container image is pushed by
     the same deploy.
  3. Deploy core with `CI_SCHEDULER script_name = "pigeon-ci-gateway"`, the `CI` service binding and the
     forwarded webhook route.
  4. Live: the `smoke` job on Containers and on Buildkite, then a landing end to end.
- **Rollback:** before step 2, nothing has moved. After it, fix forward in `pigeon-ci-gateway`. If the
  scheduler cannot serve, core's old Lander can still land through a hand-made `deploy_token`
  deploy (docs/proxies.md, break-glass), which needs no CI.
- `CI_EVENTS` moves with ci as a producer. Its consumer stays core's queue handler, which fans
  out to remote `wants` through `events`.

### Phase 5: lanes per Worker

Phase 1 already queues, tests in parallel and skips docs. Phase 5 turns on per-Worker deploy
lanes once two or more Workers exist:
- deploy latest;
- shared changes go core first;
- `after` steps run per Worker;
- deploy grants are per script.

Live check: three landings at once (docs, sites, core) with no refusal, each deploying only its
own Worker.

Status: **built on `claude/track-lanes`** (2026-11-22), not landed. How it works:
- **A lane per Worker.** The Lander's new `deploys` table (migration 12) holds each deploy of
  one Worker at one commit: `worker`, `landing` (whose commit), `commit_sha`, `covers` (that
  landing's `merged_at`), `carries` (the landings it carries), `job`, `state` (`running`,
  `succeeded`, `failed`, `error`), `tries`, `detail`. A lane has at most one `running` deploy;
  lanes run in parallel.
- **Deploy latest.** An idle lane deploys the newest merged landing that affects its Worker
  and that it has not yet deployed, carrying the older ones. `merged_at` is now strictly
  increasing, so "merged at or before `covers`" is "in that commit".
- **Core first.** A lane other than core deploys no commit at or after a landing that affects
  core and it, until core has deployed one covering that landing. This is today's
  `deploy_order` rule (core first whenever core is affected, not only for shared changes).
- **Landing states are unchanged.** A landing is `deploying` once a deploy carrying it starts;
  `landed` once every Worker it affects deployed a commit at or after its merge;
  `deploy_failed` when an incident (below) blames it, finds its commit holds the cause, or
  cannot isolate the cause (`error` when the gateway could not run a lane's deploy). The pure
  rules are `land::lanes::{starts, outcome, bisect}`.
- **Attribution.** Each deploy records the landings it included with their commits
  (`deploys.included`, migrations 13 to 15 add `included`, `purpose` and `incident`).
  `land_status` shows every deploy that carried the landing or judged it, with `included`,
  `purpose` (`lane`, `rollback`, `bisect`, `restore`) and `incident`. The landing's
  `deploy_job`, and so its note's `deploy=`, lists each such deploy as
  `worker:purpose:job@commit[L1@commit+L2@commit]`.
- **Incidents: a failed deploy is attributed.** "Deploy latest" lets one deploy carry several
  landings, so when a lane's deploy fails, the lane opens an incident (table `incidents`,
  migration 16) and no other deploy of that Worker starts until it ends:
  1. Redeploy the Worker's last good build (the commit of its last succeeded deploy) at once.
  2. Deploy the included landings' commits one at a time, oldest first, until one fails. A
     landing's commit on main holds every landing merged before it, so deploying landing X's
     commit tests X together with everything before it. Oldest first therefore finds the
     first commit that fails, and the landing that made it is the cause. The newest included
     commit already failed, so it is never deployed again: when every earlier one passes, the
     newest is the cause.
  3. After a step fails, redeploy the newest good build (the last that passed, or the last
     good), so the Worker is not left on the failing build.
  4. The cause ends `deploy_failed` with the reason. The landings after it end
     `deploy_failed` too: their commits hold the cause, so they cannot deploy without it.
     They go out with a later landing that fixes it. Those that passed are deployed; they
     end `landed` once their other Workers have deployed. Then the lane goes on from the
     newest good build.
  5. At most 4 redeploys per incident (rollback, steps and restores together). A step is
     only tried when a restore after it still fits. If the cause is not found in budget, the
     Worker stays on the newest good build, the untested landings end `deploy_failed` with
     "could not isolate", and the incident stays in `land_status` (`incidents_to_see`, state
     `unisolated`). An incident that could not get the Worker back onto a good build (no
     earlier good build, a redeploy the gateway could not run, or the good build failing to
     redeploy within budget) is `stuck`, and is shown there too.
  - **Core is different, because the Lander runs in core** (`lanes::core_incident`). Each
    core deploy restarts the Lander, so core is never bisected. Core is rolled back only to a
    build that holds this Lander. The first core deploy this Lander sees pass is recorded in
    the Lander's settings (`core_lander_since`: its commit and the `merged_at` it covers).
    Main moves forward only, so any core build covering that merge or later holds this
    Lander, and only such a build is a rollback target. Every landing the failed deploy
    carried ends `deploy_failed` with "core deploy failed; not bisected (core runs the
    Lander)". With no such target, or a rollback that does not pass (it is not tried again),
    the incident is `stuck`. Its summary says the core deploy at commit X failed, core is
    left as the job left it, and recovery is by hand (docs/proxies.md, break-glass). A
    rollback that passes leaves the incident `unisolated`, shown in `incidents_to_see`. Until
    the first core deploy with this Lander passes, a failed core deploy is always `stuck`.
  - What counts as failing: the deploy job's result. Core's live check (the architecture
    page) runs inside its deploy job, so it fails the job. The `after` steps (`portal_sync`,
    `connectors_setup`) are best-effort and never fail a deploy. No Worker has a Lander-side
    live check today.
  - The deploy grant answers for any running deploy's job and commit, rollback, bisect and
    restore deploys included. They run as the failed deploy's landing.
- **Grants.** `deploy_grant` answers for the running deploy with that job and commit, with its
  Worker's script; a Worker the map lacks gets no grant.
- **`after` steps** run when a Worker's deploy passes and write to that deploy's detail.
- **In flight.** A landing an older Lander left `deploying` (its `deploys` list set) is adopted
  on the first alarm: the Workers it already deployed become `succeeded` deploys at its
  commit, its running job a `running` deploy, and the lanes deploy the rest. Until then
  `deploy_grant` still answers it the old way.
- **Tests stay whole.** Every Worker's `test_job` is `test`, and a landing submits it once. It
  runs `ci/runner/test.sh`: fmt and clippy over the whole workspace, the smoke suite (one
  binary, `tests/smoke.rs`, which links every crate) and the WASM check. A per-Worker test job
  would need either a smoke binary per Worker or a gate that knows which scenarios a Worker's
  crates reach, and a change to `tests/smoke.rs` or a shared crate would still need all of
  them. That is a gate redesign, so `test` stays one job; the map's `test_job` is where a
  per-Worker job would go later.

### Phase 6: proxy (`pigeon-proxy`)

The person named it, so it is planned. `/git/` and `/cf/` move to `pigeon-proxy`, served on
proxy.pigeon.dev (its custom domain moves from `pigeon-platform`: the one capability Worker with
a public hostname, since the proxy credential is checked there). The proxies need the Directory
(proxy credentials) and the Lander and `pigeon-ci-gateway` (deploy grants). So:
- `pigeon-proxy` asks core `verify {credential}` over `PLATFORM` and caches the answer for 60 s,
  per isolate, keyed by a hash of the credential. Revocation therefore takes up to 60 s.
- A deploy grant is never cached: it is asked each time.
- The cost is one extra invocation per git or API request. The gain is that a change to the
  allowlist or git proxy deploys without core.
- The `deploy.sh` caveat (a proxy change that the deployed proxy refuses before the upload)
  stays, but now only for `pigeon-proxy`'s own deploys.
- Cutover: deploy `pigeon-proxy` with no domain, check it on its own, then move the custom
  domain in one call (`setup_proxy_host` names the new script). Rollback is moving it back.

## Parallel work and landings

| Wave | In parallel | Lands as |
| --- | --- | --- |
| A | T1, T2, T3, T4 (contract) ‖ T6, T7, T9 (queue), then T8 | two landings, back to back today; 1b first if the blog and linked planners also want to land, since it queues them |
| B | T10–T15 (sites) ‖ T16 (models code move, no transfer yet) ‖ T17 rehearsal | sites in one landing; models code waits on T17 |
| C | T18 (models transfer) ‖ T19–T21 (ci code) | models, then ci (drained window) |
| D | phase 5 ‖ phase 6 code | lanes, then proxy with its domain move |

Before each landing I check `land_status` and the other planners' branches, and land nothing
over their in-flight work.

## Coordination

- **Blog planner.** `blog@1` lands now in `pigeon-core` as a leaf module (no other core module,
  nothing calls it). T11 moves it unchanged to `src/sites/src/blog/`. Until then a change to it
  is a `shared` change, so keep it rare.
- **Linked-accounts planner.** `instance_plan`/`instance_apply` and site deploys move to
  `pigeon-sites`. The OAuth tokens stay in core's Directory. Sites asks
  `PLATFORM.credential {kind: "linked", instance}` for a short-lived access token.
- **Parent.** Reports go to the parent: the design, each phase with executor cost, and the end.

## Open questions for the person

1. Model job runner: may `pigeon-model-gateway` hold a minted gateway token for a job's lifetime (up to
   about 15 min)? The alternative is asking core per stream chunk.
2. The DO rehearsal (T17) deploys two throwaway scripts. Either you deploy them once with a
   `deploy_token`, or the map lists them for the rehearsal and removes them after. The second
   way is a gate change (delegate).

## Log

- 2026-11-22: phase 4's transfer done; the split is complete.
  - B2-prep (`a9c23d0`): with `Accounts::Scoped` and no one calling, CI asks core as the
    Worker itself (`worker:pigeon-ci-gateway`, key from its own `CALLER_KEY`), so the
    scheduler's alarm work (notes pushes, connector setup) can run in the gateway.
  - B2 (`zZpvzqNsgF9yWeqK`, `f604143`), with the queue held: the gateway exports
    `CiScheduler`, binds it locally, and its v1 migration transfers it from pigeon-platform.
    `CI_RUNNER` keeps `script_name = "pigeon-platform"`. The landing's own deploy job ran in one
    of core's runners across the transfer and was reported `succeeded` by the moved scheduler.
  - B2c (`SvlTnf_GTaVlvOE1`, `233052e`): core's `CI_SCHEDULER` names pigeon-ci-gateway. Its test
    and deploy jobs ran through the moved scheduler, and `ci_pool` answers from it.
  - Incident 5, a correction to the entry below: once a class has moved, the source Worker can no
    longer upload a local binding to it ("Cannot create binding for class … not currently
    configured to implement Durable Objects", 10061).
    - Core's running build, uploaded before the ModelJobs transfer, kept serving and reaching
      the class. But the next core deploy (B2-prep) and D's rollback were both refused, and
      the incident went `stuck`, as designed, with core still serving `fdeecae`.
    - Fixed forward by `MODEL_JOBS` naming the gateway (`gw_W9c5Boz17m8SX`, `8a12749`).
    - The rehearsal missed it because R4 rebound a's binding before a was ever redeployed.
    - Rule: the source's binding gains `script_name` in the landing right after the transfer,
      before any other deploy of the source. B2c did this.
    - B2-prep's non-core lanes were not deployed (its landing ended `deploy_failed`). They
      catch up on the next shared landing; only pigeon-ci-gateway uses the change, and it
      has it.
  - Open items, added to those below:
    - **CI runners.** `CiRunner` (the job containers) is still core's. A container class
      cannot be transferred, and the deploy job that switches the runner binding runs in one
      of the old runners. Handover:
      1. The gateway gets a new runner class with its own container config.
      2. The scheduler dispatches new attempts to it, while a job row records which binding
         its runner is on.
      3. Old attempts finish on core's runners.
      4. Then core's class is deleted (`deleted_classes`).
    - **Cleanup.** Core stops exporting `ModelJobs` and `CiScheduler`. `src/models` and
      `src/ci` move from shared into their gateways' lanes. The rehearsal Workers are removed
      (dashboard), along with their map entries and crates.

- 2026-11-22: phase 3 done: `ModelJobs` moved to pigeon-model-gateway.
  - First, the job objects needed a token the gateway may hold. They streamed with the parent
    token. Now `runner::Settings.token` is `Token::Parent` (core, unchanged) or
    `Token::Scoped`: a per-job `ai_gateway` token from core's `credential` op, asked as the
    Worker itself (`worker:pigeon-model-gateway`), as pigeon-proxy does
    (`q9ZLdib17L0q0su3`).
  - Drain: the only user of model jobs, the AI agents planner, had none queued or running and
    held new ones for the window.
  - The transfer (`8M7sAuFZ1mejusXZ`, main `fdeecae`): the gateway exports the class, binds
    `MODEL_JOBS` locally, and its first migration is `transferred_classes` from
    pigeon-platform. It went green on its first try.
    - Core's binding, unchanged, reached the class at runtime, but could not be uploaded
      again (incident 5, above). Core keeps its v4 migration and still exports the class.
    - The lockfile entry made the landing shared, so core deployed first, unchanged.
  - Live: `models_submit` → `models_result` succeeded (`hWTz-yJJhi90biTSlsvTPU`).
  - Left for later landings: core's binding gains `script_name = "pigeon-model-gateway"` and
    stops exporting the class; `src/models` moves from `shared` to the gateway's lane.
- 2026-11-22: the DO transfer rehearsal (T17) passed. Steps:
  - pigeon-dotest-a was deployed and seeded.
  - R2 added dotest-b's crate and map entry without deploying it.
  - R3, touching only `src/dotest-b`, deployed it, and that first deploy was the transfer.
  - On both URLs afterwards: `id` and `id_from_name` unchanged (`4cc57434…`), the three rows
    with their original `at`, the alarm still pending a day ahead with `alarm_fired` 0 ten
    minutes later, and `host` pigeon-dotest-b.
  - a's binding without `script_name` already reached the class in b.
  - R4 named b explicitly. a redeployed with its old v1 migration after the transfer.
  - The rehearsal Workers answer on workers.dev; this session's environment allows their two
    hostnames. Removing them is by hand (dashboard), with their map entries in a later
    landing.
- 2026-11-22: what the first lane incidents taught.
  - Incidents 1–3: every core deploy from main failed while the stand-in lacked the
    `PUT …/schedules` route that the new cron trigger needs. Each time, core rolled back to
    `592704d` and the landings ended "not bisected". A non-core lane holds a landing that
    also affects core until core covers it, so sites, the gateways and the proxy waited.
    Once the fix landed (`e_GFBZAzgT1ZDO-4`), core and every lane deployed it.
  - Incident 4: pigeon-dotest-a's first real deploy uploaded, then Cloudflare answered
    wrangler's lookup with 10007 "This Worker does not exist", a read just after the script
    was created. D blamed the only landing it carried, the stand-in fix, which was innocent.
    `deploy.sh` now deploys once more 30 s later when the upload went through and the answer
    was 10007 (`yqI-aw_Ljy7vxMO8`). The incident needs nothing from the person: only open
    incidents hold a lane.
  - Open items:
    - The deploy step looks Workers up in the map compiled into the running core, and
      lanes come from main's map. A newly mapped Worker can deploy only after core has
      deployed a build that lists it. Resolve from main's map, falling back to the compiled
      one.
    - D cannot tell a first-deploy environment failure from a code fault.
    - Per-Worker caller keys.
    - Skip the rollback when the failed job never uploaded.

- 2026-11-22: live checks after the binds and the cutover.
  - The person had the parent run the proxy cutover. Through pigeon-proxy, `git ls-remote`
    takes 1.9–2.3 s (4.1 s cold), against 5.6–13.9 s through core. `git fetch` takes 2.0 s and
    `/health` 0.1–0.5 s. Option (b), no parent token in the proxy, costs nothing measurable:
    it reuses one 15-minute scoped token rather than minting and revoking one per request.
  - The first core deploy through pigeon-proxy (models bind) uploaded and deployed, then
    failed: wrangler said the version was "not found" and the container settings were "not
    fully applied". The next core deploy (CI bind) went through the same path cleanly, so it
    was transient.
  - `models_providers` answers through pigeon-model-gateway (credential from core), and
    `ci_pool` through pigeon-ci-gateway.
- 2026-11-22: phase 5 gained attribution, as the person asked: each deploy records the
  landings and commits it included (`land_status`, the note's `deploy=`). A lane's failed
  deploy opens an incident: roll back to the last good build, bisect the included commits
  oldest first, blame the first failing one, restore the newest good build, at most 4
  redeploys (`lanes::bisect`). Migrations 13 to 16 are appended. Smoke:
  `land_attributes_a_failed_deploy`; 26 scenarios, 11 s.
  - Core guard: core is never bisected. It is rolled back only to a build at or after the
    first core deploy this Lander saw pass (`core_lander_since` in settings); otherwise, or
    when the rollback fails, the incident is `stuck` and recovery is by hand (break-glass).
    Its carried landings end `deploy_failed` "not bisected (core runs the Lander)".
- 2026-11-22: the three gateways deployed, and core bound them.
  - The landing that added them was deployed before core's map listed them, and the old Lander
    skips unknown Workers silently. A one-line landing touching only their crates deployed
    them (`Ms0Q5D6Geo0IlaC8`). Track D turns that skip into an error.
  - Core bound `MODELS` and dropped its local models tools (`z-NDv9ZFLGM84OnJ`).
  - Core binds `CI` (`claude/bind-ci`): `/mcp` and the `/ci/` routes go to
    pigeon-ci-gateway. The Lander keeps CI's tools locally (`settings::lander_capabilities`;
    a local tool wins a clash), so landing never depends on the CI gateway answering.
  - The proxy cutover (`setup_proxy_host {"worker": "pigeon-proxy"}`) was refused by this
    session's permission check as a destructive action. It was handed to the person through
    the parent.
  - Baseline with core serving the proxy: `/health` 0.1–0.6 s; `git ls-remote` 5.6–13.9 s
    (mostly minting and revoking a git token per request).
- 2026-11-22: phase 3 code built on `claude/track-models` (T16; nothing deployed, no transfer).
  - `models.rs` and `models/*` moved (`git mv`) to a new library crate, `pigeon-models`
    (`src/models`), which does not depend on `pigeon-capabilities`. Its functions take the
    account and the caller as core signs it (JSON), so one logic serves both sides:
    `ModelGateway` (a `pigeon_contract::Service`: the same 8 tools, the `cost` section, the
    provider key slot, footprint `{"gateways": ["pigeon"]}`; call, section and secret ops) and
    `runner::ModelJobs`, unchanged.
  - The `pigeon-model-gateway` Worker (`src/model-gateway`, wasm only, shaped as
    `pigeon-sites`). Each Cloudflare call acts on a `scoped` credential from core
    (`ai_gateway`; `models_setup` adds `secrets_store`), as an `Account` bearer. It reaches the
    job objects over `MODEL_JOBS` with `script_name = "pigeon-platform"`, no migrations.
  - Core still hosts the `ModelJobs` class (over `pigeon_models::runner`, with the parent token)
    and, until its binding to the gateway lands, the models tools: `src/capabilities/src/models.rs`
    is now a thin `Capability` over `pigeon_models::Models`, so nothing vanishes in between.
  - The map lists `model-gateway`; `src/models/*` is shared (core links it), so a shared change
    deploys core, sites, then the gateway.
  - Departures: core keeps `Models` in `settings::capabilities()` (the coordinator's
    correction); `pigeon-models` is a normal dependency of `pigeon-capabilities` (so no
    dev-dependency); the smoke lanes scenario also expects a shared change to deploy
    `model-gateway`; the secret `set` op asks `ai_gateway` only, as it writes no secret.
- 2026-11-22: track B step 1 (phase 4, T19 in part) built on `claude/track-ci`, on top of T0.
  Nothing moves live: core keeps serving CI and hosting both objects.
  - `pigeon-ci` (`src/ci`) is the CI code, moved from `src/capabilities/src/ci*`. It does not
    depend on `pigeon-capabilities`. The caller is JSON in `users::Caller`'s shape. Slots and
    the footprint are contract specs.
  - `Accounts`: `Parent(binding)` in core (the DOs, and core's local CI) or `Scoped` in the
    gateway. `Scoped` asks core's `credential` op (kind `scoped`, purposes `artifacts` for the
    `ci.toml` read, `mcp_portal` for Buildkite's setup) with the signed caller. The notes push
    and the setup run in the scheduler, so they stay on the parent token until it moves.
  - `pigeon_ci::pigeon::settings` holds pigeon's CI values (the Buildkite constants included)
    for both Workers. Core asserts at build time that its hostnames are the same.
  - `CiGateway` is the `Service`: the manifest (every ci tool, `/ci/`, the
    `BUILDKITE_API_TOKEN` slot, footprint `pigeon-ci-events`), `call`, `fetch` and `secret`
    (`expiry` from the connector).
  - `pigeon-ci-gateway` (`src/ci-gateway`) serves it. It binds `CI_SCHEDULER` and `CI_RUNNER`
    to core's classes (`script_name = "pigeon-platform"`), with no migrations or containers.
    It also has `PLATFORM`, `CALLER_KEY`, `BUILDKITE_API_TOKEN` and the `CI_EVENTS` producer.
  - Core keeps `ci::Local`, a thin `Capability` over `pigeon_ci::Ci`, and re-exports what
    the Lander and the proxies use (`job_grant`, `token`, `setup`, `same_build`,
    `setup_summary`). Core has no `CI` binding yet.
  - The map: `src/ci/*` is shared while core links it. The `ci-gateway` entry has queue
    `pigeon-ci-events` and no containers yet.
  - The architecture page reads a capability's bindings from its own crate (`src/{name}/src`)
    too, so CI's bindings are not shown as unused.
  - Smoke: the ci scenario checks the gateway's manifest against core's tools.

- 2026-11-22: phase 6 code (track C) built on `claude/track-proxy`. Core's behaviour is
  unchanged: it still serves proxy.pigeon.dev.
  - The person's decision: `pigeon-proxy` never holds the parent token. It asks core over
    `PLATFORM`, signed with `CALLER_KEY`.
  - `pigeon-proxies` (`src/proxies`, shared): the git and deploy proxies' request handling,
    moved from pigeon-capabilities, behind a `Host` trait (verify a proxy credential, a job
    grant, a deploy grant, an account to mint with). Core's host is `proxy::Local` (its
    Directory, `ci::job_grant`, `land::deploy_caller`, the parent token). The old paths
    re-export everything, so the Lander and CI are untouched. The built-in policy
    (`decide`, `request`, `audit_line`) moved to `pigeon_core::policy_file`.
  - New core ops: `verify {credential}`, `grant {token, repo}`, `deploy_grant {token, repo}`,
    signed with `CALLER_KEY` for no caller (only these three). New scoped purpose `cf_deploy`:
    the union of the allowlist's permissions (the smoke suite checks it stays the union).
  - `pigeon-proxy` (`src/proxy`): keeps each scoped credential until 60 s before it expires and
    each verified proxy credential 60 s, by its hash (`pigeon_proxies::cache`); never a job or
    deploy grant. Its `/cf/` calls use the `cf_deploy` token rather than one minted per call.
  - The map lists `proxy` (`deploy-proxy`, `after = []`) and `src/proxies/*` is shared. Smoke:
    `proxy_worker_caches_and_core_ops`; 23 scenarios, 13 s. `allowlist.sh` for `src/proxy`:
    all 16 calls on the allowlist.
  - Departures:
    - core's `credential` op still needs a signed caller, so `pigeon-proxy` signs its
      `credential` requests as itself: a service caller `worker:pigeon-proxy`, acting for no
      one. It names no person.
    - a job token whose grant is for another repository now reads "unknown or has expired"
      (both hosts), since `grant` answers only for the repository asked; still 403.
    - `setup_proxy_host` takes an optional `worker` (`PublicHost.also`), so the cutover and
      the rollback need no landing: a broken proxy would block the landing that undoes it.
      Without the argument it attaches to core, as before.
    - core does not bind `CALLER_KEY` yet (2c). Until it does, core answers the proxy
      Worker's ops 500 and the proxy Worker refuses everything but `/health`.
  - **Cutover**, after `pigeon-proxy` has deployed (its first deploy goes through core's
    deploy proxy, which allows it from the map) and core binds `CALLER_KEY`. `setup_proxy_host`
    takes an optional `worker`: `pigeon-platform` (the default, `settings::PROXY_WORKER`) or
    `pigeon-proxy` (`PROXY_WORKER_NEXT`). No landing is needed to move the host either way.
    1. Call `setup_proxy_host {"worker": "pigeon-proxy"}`: it attaches proxy.pigeon.dev to
       `pigeon-proxy`.
    2. Check: `git ls-remote https://proxy.pigeon.dev/git/pigeon-security/pigeon.git`, a push
       to a `claude/` branch and its delete, a CI job's fetch (the `smoke` job), and a
       landing's deploy (`GET /cf/grant`, then the deploy). Audit lines now come from
       `pigeon-proxy`'s logs.
    3. Latency: time `git ls-remote` ten times before and after
       (`for i in $(seq 10); do /usr/bin/time -f %e git ls-remote <remote> >/dev/null; done`).
       The first request per isolate costs one `verify` and one `credential` round trip to
       core; later ones within a minute, none beyond core's own. Expect the median within
       about 50 ms of before; a deploy's `/cf/` calls, about the same.
    4. Once it holds, land `PROXY_WORKER = PROXY_WORKER_NEXT` (one line, core), so the default
       names the Worker that serves the host and a later `setup_proxy_host` keeps it there.
  - **Rollback**: call `setup_proxy_host {"worker": "pigeon-platform"}`. Core never stopped
    serving the proxies, and the call goes through `/mcp` on platform.pigeon.dev, so it works
    even when `pigeon-proxy` cannot serve git (and so nothing can land). If step 4 had landed,
    land it back afterwards.
- 2026-11-22: phase 5 (track D) built on `claude/track-lanes`, on `claude/split-prep`.
  - Deploy lanes per Worker: the `deploys` table (migration 12, appended), the pure rules in
    `land/lanes.rs` (`starts`, `outcome`), and the Lander's `lanes` step in place of the one
    deploy lane (`take_deploy_lane`, `settle_older` and the `deploying` walk of `deploys` are
    gone). Landing states and moves are unchanged.
  - `merged_at` is strictly increasing (one more than the last when the clock ties).
  - The alarm keeps running while a deploy runs, even when every landing it carried has
    ended.
  - A landing left `deploying` by the Lander before this one is adopted, so the landing that
    deploys this change finishes.
  - Tests stay one `test` job (phase 5, "Tests stay whole"); `ci/runner/test.sh` is
    unchanged.
  - Smoke: `land_deploys_in_lanes_per_worker`. 23 scenarios, 24 s.
  - Departure: a landing naming a Worker the deployed Lander's map lacks now ends `error`
    after its retries, where before that Worker was skipped.
- 2026-11-22: phase 1 (1a and 1b) landed together, as landing `xKIrTa8WmsUYZ02v` (main at
  `520e778`).
- 2026-11-22: the person asked for the remaining Workers in parallel. T0, the shared prep, was
  built on `claude/split-prep`, to land alone before the tracks.
  - Helpers moved to `pigeon-core` (old paths re-export them): `notify`, `job_token` (CI job
    tokens), `expiry::{Expiry, from_field, parse_time}`, `account_from_env`,
    `proxy_caller::{ProxyCaller, ID_LEN}` and `artifacts::read_file`.
  - Credentials: kind `scoped` mints a 15-minute token for fixed purposes (`ai_gateway`,
    `artifacts`, `secrets_store`, `mcp_portal`); a capability Worker never names a
    permission.
  - The map takes each Worker's deploy scope (`queues`, `containers`). The deploy proxy scopes
    account-wide calls to the Worker being deployed.
  - The Lander runs tools on capability Workers too.
  - The `deploy-model-gateway`, `deploy-ci-gateway` and `deploy-proxy` jobs and scripts are
    added. Each track adds its own map entry with its crate: a shared change deploys every
    Worker in the map, so an entry before its crate exists would fail every deploy.
  - Read-only mapping of models, ci and proxy found:
    - every track needs these helpers;
    - ci and the proxy need parent-token power (git tokens, the notes push, every proxied
      request): scoped credentials for ci; the proxy's choice is with the person;
    - the Buildkite hook path is `/ci/hooks/buildkite`.
- 2026-11-22: phase 2b deployed `pigeon-sites` (its first deploy went through the deploy proxy).
  The first try was cancelled and landed again: the old Lander had checked it before the map
  listed Workers, so it would have deployed core alone.
- 2026-11-22: phase 2a built on `claude/lanes-deploy`.
  - The map lists `sites`.
  - The Lander deploys each affected Worker in turn with its own job, and runs that Worker's
    `after` steps. The deploy lane deploys the union of what the merged landings it carries
    affected.
  - The deploy grant names the Worker's script. The deploy proxy (`CfProxy.others`,
    `target_script`) takes a job token only for that script, and a proxy credential for any
    script in the map built into core. Another Worker gets no queues or container namespaces.
  - `deploy.sh` and `allowlist.sh` take `PIGEON_WORKER_DIR`; `deploy-sites.sh`;
    `[jobs.deploy-sites]`.
  - Phase 2b (the `pigeon-sites` crate, `site_render`; the renderer becomes the `pigeon-blog`
    crate) is built on `claude/sites-worker`.
- 2026-11-22: phase 1a built on `claude/split-contract`, on top of 1b. It is dormant until a
  binding exists: `settings::BINDINGS` is empty, so behaviour is unchanged.
  - `remote::Remote` is a capability Worker's capability as a `Capability`. Its manifest is kept
    60 s per isolate, a failure 10 s. Strings are interned once.
  - `split::all` merges remote capabilities after the local ones. A remote one whose name, a
    tool or a route clashes with a local one is left out, so a capability Worker never shadows
    core. The merge is used for `/mcp`, routes and the queue.
  - `split::serve` answers `credential` (kind `linked`: the instance's refreshed access token,
    handed out for 10 minutes) and `dispatch` (through the same gate). A request reaches them
    on host `core`, which only a service binding can send, with a signed caller.
  - Tools: `workers_setup` (makes `CALLER_KEY` once, scoped to Workers) and
    `manifest_refresh`. `CALLER_KEY` is bound in wrangler.toml only after `workers_setup` has
    run live, in phase 2.
  - Departure: the Lander still runs CI's tools through local capabilities only. That holds
    until phase 4, when ci moves.
  - Executor: one `claude-sonnet-5-5` job (`remote.rs`), used with one fix (`Tool` is not
    `Clone`).
- 2026-11-22: phase 1b built on `claude/land-queue`, with the contract crate (`src/contract`,
  not yet used by a Worker).
  - New pure modules in `pigeon-core`: `lanes` (the map), `rebase` (path-level rebase, merge
    base) and `merge3`. The Lander uses them through `land/merge.rs`.
  - Many landings are live at once. The alarm steps each, then gives the merge lock to the
    oldest `ready` and the deploy lane to the newest `merged`.
  - Checks now read the branch's base on main and what it changed since. Approvals come from
    the guarded paths among those changes (`state::guarded_in`), not from comparing with main.
  - A push to main whose answer was lost is found on the retry (main already holds every
    change), so no empty commit lands.
  - Departures:
    - no `queued` state: a landing is checked at once;
    - a branch with a live landing is refused rather than replaced;
    - docs-only runs no job;
    - the map is `ci/workers.toml`, not `.pigeon/workers.toml` (above);
    - a rebased commit under test is pushed to `refs/heads/claude/land/<id>`, which nothing
      deletes yet.
  - Smoke: `land_from_check_to_landed` covers the new states;
    `land_rebases_disjoint_branches_and_lanes` checks the Lander's rebase makes the same tree as
    `git cherry-pick`; `contract_signed_callers_and_skew`. 21 scenarios, 13 s.
  - Scenario names: core's stay as they are; a capability Worker's start with its name
    (`sites_`, `models_`, `ci_`, `proxy_`).
  - Executors: four `claude-sonnet-5-5` jobs (lanes, merge3, rebase, contract). Rebase and
    contract hit the token cap; I finished rebase's last three helpers. I reviewed every line,
    lowered merge3's table cap and reworded docs.
- 2026-11-22: the person named the Workers: pigeon-platform, pigeon-ci-gateway,
  pigeon-model-gateway, pigeon-sites, pigeon-proxy (proxy is planned, no longer optional). The
  blog planner lands `pigeon_core::blog` as a leaf. The linked-accounts planner agreed the
  `credential` seam.
- 2026-11-22: design. Read the plans, `policy.toml`, `ci.toml`, `ci/`, `src/platform` and the
  capabilities named in the brief. Read the blog and instance branches in flight. Checked
  Cloudflare's DO migration, Containers and Service Binding docs.

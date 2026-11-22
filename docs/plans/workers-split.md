# Workers split: capabilities as their own Workers, landing in lanes

Status: **designed, awaiting the person's go-ahead** (2026-11-22). Nothing built yet. Planner
session `session_015XFA1nSZXE9BNYuPF5XL2j`, seeded by the parent `session_015XFA1jgveJDEE8NJjTaHKg`.

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

Four platform Workers in one repository, with an optional fifth:

| Worker | Script | Owns | Durable Objects | Secrets |
| --- | --- | --- | --- | --- |
| core | `pigeon-platform` | identity (users, passkeys, oauth), policy, approvals, dispatch, secrets, instances records, linked accounts, land, source and mirror, the console shell, architecture, the pigeon.dev page, the proxies (until phase 6) | `Directory`, `Lander` | `CF_PARENT_TOKEN`, `GITHUB_MIRROR_TOKEN`, `CF_OAUTH_CLIENT_SECRET`, `LINK_KEY`, `CALLER_KEY` |
| ci | `pigeon-ci` | gateway, scheduler, runner and container, Buildkite setup and webhooks, `ci_*` and `containers_images` | `CiScheduler`, `CiRunner` (+ container) | `BUILDKITE_API_TOKEN`, `CALLER_KEY` |
| models | `pigeon-models` | AI Gateway, `models_*`, model jobs | `ModelJobs` | `CALLER_KEY` |
| sites | `pigeon-sites` | `blog@1` renderer, instance site deploys (render, R2 upload, routes, subdomains), `instance_plan` / `instance_apply` | none | `CALLER_KEY` |
| proxy (optional) | `pigeon-proxy` | `/git/`, `/cf/` on proxy.pigeon.dev | none | decided in phase 6 |

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
| `credential` | `{caller, purpose: {kind, instance?, scopes}}` | `{token, expires_at}`: a minted Cloudflare token or a linked account's access token, under 15 min |
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
src/models/        pigeon-models        models Worker (phase 3)
src/ci/            pigeon-ci            ci Worker (phase 4)
```

A capability Worker's crate depends on `pigeon-core` and `pigeon-contract` only, never on
`pigeon-capabilities`. Otherwise a core change would silently change its binary.

`.pigeon/workers.toml` is the path map:

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

The map is under `.pigeon/`, so a change to it is already a `change_gate` (delegate approval) by
the existing guarded paths and rule `gate-changes`. No new rule is needed. The Lander reads it at
main, so a branch never chooses its own lane.

## Landing in lanes

The Lander keeps its one Durable Object and its state machine. It gains a queue and splits the
lock.

```
land ─► queued ─► checking ─► testing ─► [awaiting_approval] ─► ready ─► merging ─► merged ─► deploying(w…) ─► landed
                   │            │                                 │ main moved in overlapping paths: retest
                   ▼            ▼                                 ▼ conflict on a path both changed
                refused     test_failed                        rebase_refused
```

Rules:
1. **Queue, never refuse.** `land` always inserts and answers `queued` with a position.
   - The same branch landed again replaces its queued entry and keeps its place.
   - `land_status` shows the queue.
   - `land_cancel` works until `merging`.
2. **Classify at check.** Changed paths come from `main..tip` against the map read at main.
   - Every path matches `docs`: the landing is docs-only.
   - Otherwise it is a set of affected Workers, with `shared` meaning all of them.
   - Guarded paths go to the policy exactly as today (passkey or delegate). Nothing about
     approvals changes.
3. **Test outside the lock, in parallel.**
   - Each queued landing past `checking` starts its affected Workers' test jobs at once, at its
     own tip.
   - Docs-only runs the `docs` job only: markdown links and nothing compiled, about 20 s.
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
       merge is tried. A clean merge counts as overlap; a conflict is `rebase_refused`, with the
       paths named.
     - Other overlap is also `rebase_refused`. The agent rebases and pushes again; rewriting a
       session branch is already allowed.
   - **Retest only on overlap.** If B touches a Worker the branch affects, or anything `shared`,
     the landing goes back to `testing` at the rebased commit, which the Lander pushes as
     `refs/land/<id>` so CI can fetch it. Otherwise it merges at once.
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
| T7 | `.pigeon/workers.toml` reader in `pigeon-core` (pure: map, classify, docs-only, overlap), `docs` job | sonnet |
| T8 | `land.rs`: queue, parallel test jobs outside the lock, CAS merge, path-set rebase (tree build with `gitobj`), `refs/land/<id>`, docs-only skip | sonnet, then me (the Lander is the riskiest file) |
| T9 | Smoke: `core_land_queue_order`, `core_land_rebase_disjoint`, `core_land_rebase_overlap_refused`, `core_land_docs_only` | haiku |

These rules need care:
- `.pigeon/workers.toml` and the `docs` job in `.pigeon/ci.toml` are gate changes (delegate).
- The first landing with a new Lander is landed by today's Lander, so this phase uses today's
  flow.
- The Lander's SQLite gets migrations 5 and on (only appended, never edited).
- Live landings in today's states map one to one.

### Phase 2: sites, the pilot Worker

This needs the blog and linked-accounts planners first: their branches land, or are rebased to
put their code in `src/sites`.

| Task | What | Executor |
| --- | --- | --- |
| T10 | `src/sites`: crate, `wrangler.toml` (`pigeon-sites`, no routes, `workers_dev = false`, `CALLER_KEY`, `PLATFORM` service binding), `lib.rs` with `serve()` | haiku |
| T11 | Move `pigeon_core::blog` to `pigeon-sites::blog` as it is | haiku |
| T12 | Move `instance_plan`/`instance_apply`, `site_worker.js` and the R2/workers upload callers into sites. Linked tokens come through `PLATFORM.credential` | sonnet |
| T13 | Core: `[[services]] SITES = pigeon-sites`, the `Remote` entry, `CfProxy` scripts from the map | me |
| T14 | CI: `test-sites` and `deploy-sites` jobs, `deploy.sh` taking the Worker's dir and script, `test.sh` taking the scenario prefix | me (gate change, delegate) |
| T15 | Smoke: `sites_*` scenarios moved from the blog and instance ones | haiku |

Order and checks:
- **Deploy order:** `pigeon-sites` first (a binding's target deploys first), then core. The first
  deploy of `pigeon-sites` needs `CfProxy` to allow the script, so core with the new allowlist
  lands first, with `SITES` not yet bound.
- **Live check:** `tools/list` holds `instance_plan` from `pigeon-sites`. Then a docs change and
  a sites change land at the same time, the second deploying only sites.

### Phase 3: models

| Task | What |
| --- | --- |
| T16 | `src/models` crate, moving `models.rs`, `models/*` and `ai_gateway` callers. Gateway and parent tokens come through `credential` |
| T17 | Rehearsal, before anything live: two throwaway scripts in pigeon's account (`pigeon-dotest-a`, `pigeon-dotest-b`) with a SQLite class holding rows and a pending alarm. Transfer a→b with `transferred_classes`, then check rows, ids (`idFromName`) and the alarm. Then delete both. These deploys need names the proxy does not allow, so the person deploys them once with a `deploy_token`, or the map gains them for the rehearsal |
| T18 | `pigeon-models` with `[[migrations]] tag = "v1"`, `transferred_classes = [{from = "ModelJobs", from_script = "pigeon-platform", to = "ModelJobs"}]`. Core's `MODEL_JOBS` binding gains `script_name = "pigeon-models"` in the same release and stops exporting the class after the transfer |

Order, rollback and fallback:
- **Order:**
  1. Drain: no running model job. `models_status` shows none, and `models_submit` refuses for
     the window.
  2. Deploy `pigeon-models` (the transfer happens here).
  3. Deploy core with the `script_name` binding and `MODELS` service binding.
  4. Live check: `models_submit` haiku, then `models_result`.
- **Rollback:** a transfer cannot be rolled back. Before step 2, cancelling is "redeploy without
  the transfer". After it, roll back by fixing forward in `pigeon-models`. Core's binding keeps
  pointing there. If `pigeon-models` cannot serve, a new class `ModelJobs2` in core
  (`new_sqlite_classes`) takes new jobs, and history stays readable through the old binding.
  Model jobs are minutes long, so losing in-flight ones is the worst case, and step 1 avoids it.
- **Fallback if the rehearsal shows ids or alarms lost:** no transfer. `pigeon-models` gets a new
  class. Core keeps `ModelJobs` read-only until its last job ends, then a later release deletes
  it (`deleted_classes`).

### Phase 4: ci

| Task | What |
| --- | --- |
| T19 | `src/ci` crate: `ci.rs`, `ci/*`, the Buildkite setup, the containers connector and the scheduler and runner shims from `src/platform/src/lib.rs`. The Buildkite webhook moves to a forwarded route |
| T20 | The Lander's CI use goes through the contract: `dispatch` of `ci_submit`/`ci_status`/`ci_cancel` and a `job_grant` op. It no longer reads `CI_SCHEDULER` directly |
| T21 | The deploy proxy's `deploy_caller` asks ci for job grants over the binding |

Durable Objects:
- **`CiScheduler` (state: jobs, grants, notes)** is transferred like `ModelJobs`, after phase 3
  proved the move. It is drained first: no running or queued job, and `ci_submit` refuses for the
  window. A landing's test can't run during the window, so it is scheduled when the queue is
  empty.
- **`CiRunner` (container)** is not transferred; the docs say nothing about container classes.
  `pigeon-ci` declares a new class `CiRunner` with `new_sqlite_classes` and its `[[containers]]`.
  Runner objects are per job attempt, so nothing needs to survive. Core deletes its `CiRunner`
  (`deleted_classes`) one release later, once the drain shows no live attempt.
- **Order:**
  1. Drain.
  2. Deploy `pigeon-ci` (transfer plus the new runner class). The container image is pushed by
     the same deploy.
  3. Deploy core with `CI_SCHEDULER script_name = "pigeon-ci"`, the `CI` service binding and the
     forwarded webhook route.
  4. Live: the `smoke` job on Containers and on Buildkite, then a landing end to end.
- **Rollback:** before step 2, nothing has moved. After it, fix forward in `pigeon-ci`. If the
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

### Phase 6 (optional): proxy

This is worth it only if proxy deploys turn out to be the frequent ones. `/git/` and `/cf/` read
the Directory (proxy credentials) and the Lander (deploy grants), so a proxy Worker would call
core on every request. Decide after phase 5, using the landing history.

## Parallel work and landings

| Wave | In parallel | Lands as |
| --- | --- | --- |
| A | T1, T2, T3, T4 (contract) ‖ T6, T7, T9 (queue), then T8 | two landings, back to back today; 1b first if the blog and linked planners also want to land, since it queues them |
| B | T10–T15 (sites) ‖ T16 (models code move, no transfer yet) ‖ T17 rehearsal | sites in one landing; models code waits on T17 |
| C | T18 (models transfer) ‖ T19–T21 (ci code) | models, then ci (drained window) |
| D | phase 5 | one landing |

Before each landing I check `land_status` and the other planners' branches, and land nothing
over their in-flight work.

## Coordination

- **Blog planner.** `blog@1` stays as built. Its home after phase 2 is `src/sites/src/blog/`.
  Their landing is not held for this. If phase 2 is ready first, I move their code; if theirs is,
  T11 is the move.
- **Linked-accounts planner.** `instance_plan`/`instance_apply` and site deploys move to
  `pigeon-sites`. The OAuth tokens stay in core's Directory. Sites asks
  `PLATFORM.credential {kind: "linked", instance}` for a short-lived access token.
- **Parent.** Reports go to the parent: the design, each phase with executor cost, and the end.

## Open questions for the person

1. Model job runner: may `pigeon-models` hold a minted gateway token for a job's lifetime (up to
   about 15 min)? The alternative is asking core per stream chunk.
2. The DO rehearsal (T17) deploys two throwaway scripts. Either you deploy them once with a
   `deploy_token`, or the map lists them for the rehearsal and removes them after. The second
   way is a gate change (delegate).
3. Phase 6 (proxy): decide later, as above?

## Log

- 2026-11-22: design. Read the plans, `policy.toml`, `ci.toml`, `ci/`, `src/platform` and the
  capabilities named in the brief. Read the blog and instance branches in flight. Checked
  Cloudflare's DO migration, Containers and Service Binding docs.

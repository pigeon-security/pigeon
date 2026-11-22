# Architecture: one source, regenerated on every deploy

Status: deployed (phases 1 to 4; landings `csQqlS0Y87gIBlKK` and `j9beqhkc55Y2Z4sp`). Every deploy
regenerates the model and checks that https://pigeon.dev/architecture.json names the deployed commit.

## Problem

pigeon's architecture is pictured in four places, and they disagree with each other and with
what is deployed.

| Where | How it is made | What it shows today |
| --- | --- | --- |
| `pigeon.dev/` and `/architecture.json` | `site_publish` renders `architecture::page` into a separate `pigeon` Worker, by hand | Very stale: 10 capabilities from 31 files. No CI, landing, models, proxies or policy, no `proxy.pigeon.dev`, one Durable Object (`DIRECTORY`), one queue |
| `architecture_describe` (MCP) | `architecture::describe` over every embedded source, on each call | Answers 503 live (`/health` is 200) |
| The console's Architecture tab | `Architecture::section_data`: the same `describe` plus `render`, on each request | The same model as the tool, rebuilt for every view |
| Each capability's `footprint()` | Written by hand | Repeats `wrangler.toml` and misses parts of it (below) |

Docs describe the architecture only in prose (`docs/proxies.md`, the plans). No doc holds a
diagram, so docs stay prose and link to the page.

### What the footprints say compared with `src/platform/wrangler.toml`

| Deployed (wrangler.toml, settings.rs) | In the model? |
| --- | --- |
| Durable Objects `DIRECTORY`, `CI_SCHEDULER`, `CI_RUNNER`, `MODEL_JOBS`, `LANDER` | Only as each capability's hand-written `BINDING`. `CI_RUNNER` is not drawn as a container |
| Container `CiRunner`, image `pigeon-ci-base@sha256:…` | Not modelled at all |
| Queue producer `pigeon-ci-events`; `pigeon-source-events`, made by `setup_source` | Both, by hand |
| Secrets Store secrets `CF_PARENT_TOKEN`, `GITHUB_MIRROR_TOKEN` | Read from wrangler.toml, and also repeated by `mirror` |
| `proxy.pigeon.dev` (`/git/`, `/cf/`, `/health`) | Missing: no proxy capability has a footprint |
| AI gateway `pigeon` | From `models`' footprint |
| `CfProxy { queues, containers }` in settings.rs | A hand copy of wrangler.toml ("The queue producers in wrangler.toml") |

## Goal

There is one model, `architecture::describe`. It is derived only from the code the Worker was
built with and from `wrangler.toml`, so nothing in it is written twice. Every view comes from
that one model:

- `architecture_describe` returns the model;
- the console tab returns its HTML;
- `pigeon.dev` returns its page and its JSON.

Every deploy regenerates it, because the model is part of the deployed Worker. Nothing is
published separately.

Constraints: no new long-running jobs, no new CI job, and no tests outside `tests/smoke.rs`, which
must stay under 60 s. No new policy rules. `policy_check add_test tests/smoke.rs` answers `deny`
(not in policy), so this work adds no test. Whether the architecture path gets a smoke scenario
is the person's decision.

## Design

1. **wrangler.toml is the authority for bindings.** `Toml::read` also reads the
   `[durable_objects]` bindings (binding to class), `[[containers]]` (class, image keys and
   digests), `[[queues.producers]]` and consumers, and `[vars]` hosts. The model gains
   `resources.containers`. `resources.objects` becomes `BINDING (Class)`. Each binding is
   attributed to the capabilities whose code names it as a string literal. A binding that no
   code names is listed under `unused`. That is a drift signal shown on the page, not a test.
2. **Footprints keep only what wrangler.toml cannot say**: hostnames a capability serves
   (Access, public paths), what it sets up at runtime (`pigeon-source-events`, the Access
   service-token secrets), repos, mirrors and gateways. The `objects` field goes away. The
   `secrets` entries that wrangler.toml already declares go away. The proxies get a footprint
   for `proxy.pigeon.dev`.
3. **settings.rs stops copying wrangler.toml.** `CfProxy`'s queues and container classes are
   read from the embedded wrangler.toml with the same reader.
4. **Cheap**: the model, its JSON and its HTML are built once per isolate (`OnceLock`) and
   shared by the tool, the tab and the apex. The 503 is diagnosed first (phase 1), because a
   memo only helps if one build fits the request's limits.
5. **Provenance**: `deploy.sh` exports `PIGEON_COMMIT`, and the build embeds it as `model.commit`
   (`option_env!`, empty for a local build). The page shows the commit and the digest.
6. **Published by deploying.** Once the pigeon.dev Worker is merged into the platform (the merge
   planner's work), the platform serves `GET /` and `GET /architecture.json` on the apex from the
   memo. `site_publish` and the separate `pigeon` Worker are retired. After `wrangler deploy`,
   `deploy.sh` makes one `curl` to `https://pigeon.dev/architecture.json` and checks that its
   `commit` is the commit just deployed. On a mismatch it says so and fails the deploy job. That
   is the whole publish step: no new job and no MCP call from CI. A job token cannot reach MCP
   anyway.

   If the merge has not landed by phase 3, the fallback is that the Lander's step after a
   successful deploy calls the site capability's publish in-process. I prefer not to need this.
7. **The console tab's contract is unchanged**: section `architecture` answers `{html}`. I have
   asked the console planner to confirm.

## Phases

| Phase | What | Files | Lands as |
| --- | --- | --- | --- |
| 1 | Diagnose the 503; wrangler.toml as the authority (containers, objects with classes, queues, hosts); footprint clean-up; proxy hostname; `CfProxy` reads wrangler.toml; memo per isolate; commit provenance | `architecture.rs`, `lib.rs` (Footprint), the footprints, `settings.rs`, `platform/build.rs`, `platform/src/lib.rs` | one landing |
| 2 | Draw what is new: grouped Durable Objects, the container, the AI gateway, the proxy host, unused bindings | `architecture.rs` (render) | with phase 1 |
| 3 | `deploy.sh` checks that the live `/architecture.json` names the deployed commit. Serving the apex from the platform is the one-worker plan's (`docs/plans/one-worker.md`): `site::render` reads this plan's memo | `ci/deploy/deploy.sh` | one landing, after the cutover |
| 4 | Docs: this plan's status, the README table, `console.md` and `site.rs` wording, the `docs/proxies.md` link | docs | with phase 3 |

Each phase passes `ci/runner/test.sh` locally before it lands. After each landing, I check
`architecture_describe`, the console tab, and (from phase 3) `https://pigeon.dev/architecture.json`.

## Executors

Fixed-length tasks go to AI Gateway executors (`models_submit`). Each task carries the full text
of the functions it changes and the exact output it should return (whole replacement functions).
I check each result, apply it, and run fmt, clippy and the smoke suite myself.

| Task | Model | Phase |
| --- | --- | --- |
| E1: `Toml::read` reads DO bindings, containers, queues and vars | claude-sonnet-5-5 | 1 |
| E2: footprint clean-up across the capabilities (mechanical) | claude-haiku-5-5 | 1 |
| E3: diagram additions in `render`/`diagram` | claude-sonnet-5-5 | 2 |
| E4: docs wording | claude-haiku-5-5 | 4 |

## Coordination

- Merge planner (pigeon.dev into the platform): agreed. `site` serves the apex by host, from
  `architecture::built`, and owns `site.rs`, Site's settings and its footprint.
- Console planner: agreed. The tab answers `{html, built_from}`, and I don't touch `console.rs`,
  `sections.rs` or `app.js`.
- Policy planner: no overlap expected. I add no policy rules.

## Log

- 2026-11-21: inventory and plan. The live `architecture_describe` answers 503. pigeon.dev shows
  the 10-capability model.
- 2026-11-21: phases 1 and 2 built on `claude/architecture`, and the gate passes locally (smoke
  suite 7 s). The 503's cause was `depth_of`, an unmemoized walk that is exponential on
  capabilities calling each other in cycles (natively over 120 s; now 6 ms for 74 files).
  Executors: E1 (the wrangler.toml reader), E3 (layer-4 cards) and E5 (one card per Worker), all
  claude-sonnet-5-5 and all applied as returned. I made the footprint clean-up (E2) by hand
  instead: it was smaller than its spec. Held for the parent's landing hold.
- 2026-11-21: rebased on `claude/one-worker` and landed as `claude/architecture-2` (this session
  may not force-push), landing csQqlS0Y87gIBlKK. Live `architecture_describe` answers at once
  with commit 0c9db6067d2d, 5 Durable Objects, the container, `proxy.pigeon.dev` and no unused
  bindings. Phase 3's check is built on `claude/architecture-3`.
- 2026-11-22: after the Workers split. The model still describes one Worker. Findings and the
  fix plan are in "After the split" below. Nothing is landed: the person reviews first.
- 2026-11-22: phase 5 was not built. Three executor runs (`IOLExAeb1AJj`, `03Lpp6kAJPqz`,
  `Ce7SwxUH5L93`) failed before they started: their sandboxes could not clone ("destination
  path '/work' already exists"). No code had changed then. Phases 5 to 8 were still to do,
  with the phase 5 brief as given to those runs. (Superseded by the next entry.)
- 2026-11-22: phase 5 built on `claude/ai-agent-avxmfnk1V4IV`: the model draws one Worker per
  `ci/workers.toml` entry, with tools by Worker, object ownership, service bindings, externals,
  the portal host, `PROXY_WORKER = pigeon-proxy`, the ai-agents footprint fixed and the memo
  rebuilt when the capabilities change. Smoke scenario
  `core_architecture_draws_every_worker_in_the_map`. Reviewed by the Workers split planner and
  gated; landed with one fix: whether a binding is used is read from every crate the Worker is
  built from (its own, the crates its map entry lists, and src/core and src/contract), not its
  own crate alone, and the scenario loads every crate as core's build.rs embeds them. Phases 6
  to 8 are still to do; instance hostnames (noisypigeon.com, noisypigeon.pigeon.dev) wait for
  phase 8, as they are attached at run time, not declared.

## After the split (2026-11-22)

pigeon now runs as six Workers (`ci/workers.toml`). The model is still built for the Worker
that hosts the `architecture` capability (core). It reads only core's `wrangler.toml`. The
other Workers show up only as names in `split`'s footprint. Checked against the live
`architecture_describe` at commit `126b615e6dc7` (digest `599cd96c`).

### How it is produced today

- `src/platform/build.rs` embeds every `src/*/src/**.rs` and every `src/*/wrangler.toml` as
  `SOURCES`. It does not embed `ci/workers.toml`. Core reads that file only for its lanes
  (`settings::map`).
- `architecture::describe(sources, hosted)` finds the one `wrangler.toml` whose `name` is
  `hosted.worker` and draws one Worker from it. Every other Worker, hostname and resource comes
  from the `footprint()` of each hosted capability, local or remote (the manifests).
- `architecture::built` keeps the first result per isolate in a `OnceLock`. It is served by
  `architecture_describe`, the console tab and the apex (`/`, `/architecture.json`).

### What is wrong or missing

| Area | Live model says | Truth |
| --- | --- | --- |
| Workers | `pigeon-platform`, and four empty cards "published by split" | Six Workers. **`pigeon-proxy` is missing**, because core has no binding to it (it calls core over `PLATFORM`) |
| Capability Workers | No routes, handlers, secrets, Durable Objects, containers or tools | Each has its own `wrangler.toml`, `CALLER_KEY`, a `PLATFORM` binding, and its own tools |
| Tools | Every tool is on `pigeon-platform` | `models_*` are the model gateway's, `ci_*` the CI gateway's, `ai_agents_*` the AI agents Worker's, `site_render`/`site_deploy` the sites Worker's. Core merges them into `/mcp` |
| Service bindings | Not modelled | Core binds `SITES`, `MODELS`, `CI`, `AI_AGENTS`. Each of the five others binds `PLATFORM` to core. Calls carry a caller signed with `CALLER_KEY` |
| Durable Object ownership | `CI_SCHEDULER (CiScheduler)` and `MODEL_JOBS (ModelJobs)` drawn as core's | `CiScheduler` belongs to `pigeon-ci-gateway` and `ModelJobs` to `pigeon-model-gateway` (classes moved today). Core binds them with `script_name`. The reader ignores `script_name` |
| Durable Objects missing | | `AiAgentRuns` and `Sandbox` (ai-agents). The ci-gateway's `CI_RUNNER` points at core's `CiRunner` |
| Containers | `CiRunner` only | Also `Sandbox` (image `pigeon-ai-agent-sandbox`), one per AI agent run, in `pigeon-ai-agents` |
| Queues | `pigeon-ci-events` as core's | Both core and the CI gateway produce to it |
| Secrets | One list of 12, all core's | Core's 12 plus `CALLER_KEY` in every Worker and `BUILDKITE_API_TOKEN` in the CI gateway. Only core holds `CF_PARENT_TOKEN` |
| `proxy.pigeon.dev` | Served by `pigeon-platform` | Served by `pigeon-proxy` since the cutover (workers-split log, 2026-11-22). `settings::PROXY_WORKER` still names core: step 4 of that cutover ("land `PROXY_WORKER = PROXY_WORKER_NEXT`") was never done |
| Sites hostnames | Only `pigeon.dev` (core) | `pigeon-sites` serves the instance subdomains on pigeon.dev, such as `noisypigeon.pigeon.dev`. `noisypigeon.com` is served by the instance Worker in the linked account, from its own R2 bucket. These are instance records (Directory data), not code |
| MCP Server Portal | Not drawn | Claude reaches `/mcp` through the Cloudflare MCP Server Portal at `mcp.pigeon.dev` (servers `pigeon` and `buildkite`). The Lander syncs it after deploys (`portal_sync`) |
| External services | AI gateway `pigeon` and the GitHub mirror only | Also Buildkite (cluster `pigeon`, hosted Linux and macOS queues), Cloudflare Containers as the second CI connector, the GitHub App, and linked Cloudflare accounts (OAuth) |
| The Lander | A capability card | Per-Worker deploy lanes from `ci/workers.toml`, with attribution, rollback and bisect. The map is not drawn |
| `ai-agents` footprint | | Sends `{"durable_objects": [...]}`, a key `Footprint` does not have, so it is dropped silently |
| Memo | | The `OnceLock` keeps the first build. If a capability Worker's manifest failed on that request, its tools stay missing until the isolate ends |
| Staleness across lanes | | Core embeds every Worker's `wrangler.toml` when core builds. A landing that changes only a capability Worker deploys only that Worker's lane. Core's picture of that Worker stays old until core next deploys |
| Stale summaries | `models`: "as core hosts them until its binding … lands" | Out of date wording in `src/capabilities/src/models.rs`. Core also still exports a `ModelJobs` class that nothing binds now. Noted for the workers-split owner. Not fixed here |

Nothing secret is drawn today. `wrangler.toml`'s `account_id`, Secrets Store id and
`GITHUB_APP_CLIENT_ID` are read by the reader but are not in the model. The fix keeps it so,
and a smoke check enforces it.

### Fix plan

The authority for each fact, so nothing is written twice:

| Fact | Source |
| --- | --- |
| Which Workers exist, their lane, deploy job and `after` steps | `ci/workers.toml`, embedded in core |
| Each Worker's bindings, Durable Objects (and which script owns each class), containers, queues, secrets, service bindings, crons | That Worker's `wrangler.toml`. Live from its manifest when it answers; core's embedded copy otherwise, marked as such |
| Each Worker's routes and handlers | Its crate's Rust (`(Method::X, "/path")`, `#[event]`) |
| Which tools each Worker serves | The capability registry: core's `settings::capabilities()` and each binding's manifest. A remote capability names its Worker |
| Hostnames | Footprints (`public_host`, `site`, the proxies), with `PROXY_WORKER` fixed |
| External services (Buildkite, the portal, GitHub App, linked accounts, AI gateway, mirror) | A new `Footprint.externals`, filled from the settings each capability already uses (CI's connectors, the Lander's portal servers, `github`, `linked`, `models`, `mirror`) |
| Instance sites (`noisypigeon.*`) | The instance records, only on the console tab and `architecture_describe`. Never on the public apex page (linked-accounts plan, §8) |

Phases, each an executor branch built on the one before:

| Phase | What | Branch |
| --- | --- | --- |
| 5 | Model: one Worker per `ci/workers.toml` entry from its `wrangler.toml` (`services`, `script_name`, crons read); tools attributed by Worker; Durable Object ownership; service-binding and cross-Worker object links; `externals`; the portal host; `PROXY_WORKER` set to `pigeon-proxy`; the ai-agents footprint fixed; the memo rebuilt when the capabilities change. Smoke scenario `core_architecture_draws_every_worker_in_the_map` | executor |
| 6 | Drawing: a Workers layer with core and the five capability Workers, binding arrows with their names, each Worker's objects, containers and secrets; an external services layer; the lanes | executor |
| 7 | Live per-Worker facts: each capability Worker sends its own `wrangler.toml` summary and commit in its manifest, so a capability-only landing shows at once, without a core deploy | executor |
| 8 | Instances layer (console and `architecture_describe` only): accounts as home or linked, zones, the instance Worker, buckets, hostnames. No account ids, titles or installation ids | executor |

Gate: no change is needed for phases 5 to 8. For the record, if the person prefers not to rely
on phase 7, the alternative is a gate change: add each capability Worker's `wrangler.toml` to
core's `paths` in `ci/workers.toml`, so a binding change redeploys core. I do not propose it:
it makes core deploy for every capability Worker's binding change. The proxy Worker has no
binding from core and so sends no manifest; its card stays drawn from core's build and says so.

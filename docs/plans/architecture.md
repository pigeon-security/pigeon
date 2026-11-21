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

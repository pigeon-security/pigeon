# Cloudflare Workflows as a basis for CI

Status: evaluated 2026-11-21; recommendation **don't adopt now**. Revisit when workers-rs ships
Workflow bindings, and then only for orchestration, starting with the Lander. Nothing was built.

The question: would Cloudflare Workflows be a better basis for pigeon's CI than today's
Containers setup? Today the gateway ([ci-gateway.md](ci-gateway.md)) routes `.pigeon/ci.toml`
jobs to connectors. Jobs run in the `pigeon-ci-base` image inside a Container that a
`CiRunner` Durable Object owns. The `CiScheduler` Durable Object queues, routes, retries and
writes `refs/notes/ci`. The `Lander` Durable Object takes a branch through checking, testing,
approval, merging and deploying, and waits on those notes.

Sources: developers.cloudflare.com pages read on 2026-11-21 (Workflows
[limits](https://developers.cloudflare.com/workflows/reference/limits/),
[pricing](https://developers.cloudflare.com/workflows/reference/pricing/),
[sleeping and retrying](https://developers.cloudflare.com/workflows/build/sleeping-and-retrying/),
[events and parameters](https://developers.cloudflare.com/workflows/build/events-and-parameters/),
[Workers API](https://developers.cloudflare.com/workflows/build/workers-api/); Containers
[pricing](https://developers.cloudflare.com/containers/platform/pricing/),
[limits](https://developers.cloudflare.com/containers/platform/limits/),
[FAQ](https://developers.cloudflare.com/containers/faq/)), the published `worker` 0.9.0 and
`worker-macros` 0.9.0 crates (pigeon's pinned version, released 2026-11-11, the latest on
crates.io), and `ci_jobs` for pigeon's last landings.

## What Workflows can and can't do

| | Workflows (Workers Paid) | Source |
| --- | --- | --- |
| Durable steps | `step.do(name, {retries, timeout}, fn)`. A step's result (at most 1 MiB) is persisted, and a completed step is not run again after a restart. Code outside steps runs again on every replay. | limits, rules of Workflows |
| Retries | Per step: `limit` (at most 10,000), `delay` (fixed, linear, exponential or a function), and `timeout` per attempt. `NonRetryableError` fails at once. Rollback handlers. | sleeping and retrying |
| Sleep | `step.sleep` / `sleepUntil`, up to 365 days. A sleep is not a step and does not hold a concurrency slot. | limits |
| Wait for an event | `step.waitForEvent(name, {type, timeout})`: 1 s to 365 days, 24 h by default. A timeout throws (catch it to carry on). `instance.sendEvent({type, payload})` comes from a binding or the REST API. Payloads are at most 1 MiB. | events and parameters |
| CPU per step | 30 s by default, configurable to 5 min of **active** CPU (`limits.cpu_ms`) | limits |
| Wall time per step | Unlimited (I/O waits don't count) | limits |
| Steps per instance | 10,000 by default, up to 25,000 | limits |
| State per instance | 1 GB; kept 7 days after completion by default, up to 30 | limits |
| Concurrency | 50,000 running instances per account. A per-Workflow cap is configurable, and instances above it queue. Waiting instances don't count toward either. | limits |
| Subrequests | 10,000 per instance by default, up to 10 million | limits |
| Observability | Instance status and per-step output from the binding, the REST API, wrangler and the dashboard. A step visualizer in the dashboard. Step durations and error rates in GraphQL analytics. | metrics, visualizer |
| Languages | TypeScript/JavaScript and Python. **No Rust:** `worker` 0.9.0 and `worker-macros` 0.9.0 have no Workflow types, entrypoint macro or binding (the only "workflow" in either crate is the README's release-process link). | crate source |

**Can a step run cargo, clippy or wrangler? No.** A Workflow is a Worker script: a V8 isolate
with no process spawning, no filesystem for a toolchain, Worker memory, and at most 5 minutes
of CPU per step. pigeon's `test` job alone runs about 80 s of 4-vCPU compilation. Workflows can
only **orchestrate** jobs that run elsewhere. A step would start a job in a Container through a
Durable Object binding, or in Buildkite through its API, then `waitForEvent` for the runner's
result. A Workflow cannot own a Container either (each Container belongs to one Durable
Object), so `CiRunner` stays in any design.

## Where Workflows would beat the hand-built state machines

| Object | Lines | What a Workflow would replace | Fit |
| --- | --- | --- | --- |
| `Lander` (`land.rs`, `land/state.rs`) | 1,096 + 326 | The alarm-driven `check`/`step`/`apply`/`retry_or_fail`/`reschedule` loop (about 400 lines) maps directly onto steps: `check` → `submit test` → `waitForEvent(ci-finished)` → `waitForEvent(approval)` → `merge` → `submit deploy` → `waitForEvent`. A per-Workflow concurrency cap of 1 gives "one landing at a time" with a real queue. | **Good.** It is linear and long-waiting, which is what Workflows is for. |
| `CiScheduler` (`ci/scheduler.rs`) | 1,179 | Per-job lifecycle only: `pump`'s dispatch, `overdue`, `requeue`/`finish`/`apply`, the outbox's retries for notify, queue and notes, and `reschedule` (about 550 lines). | **Partial.** The pool cache, per-connector live limits, routing across jobs, job-token grants and the serialized notes push are shared state across jobs. They stay in a Durable Object. |
| `CiRunner` (`ci/containers.rs`) | 385 | Nothing: it owns the container. | None |

What would actually improve:

- **Reliability.** Step-level persistence and retries replace hand-written outbox bookkeeping.
  A lost alarm or a half-applied transition can't strand a landing. But the record so far is
  that the state machines have not been the failure mode. In the last 9 landings every `test`
  and every `deploy` reached a final state on its first attempt. The earlier failures
  (`exit 143`, the 40-second kill) were in the container, which Workflows doesn't touch.
- **Observability.** The dashboard's step graph and per-step timings come free. pigeon already
  has `land_status`, `ci_status`/`ci_jobs` with events, and notes on the commit. Those tools
  stay, because sessions read them through MCP, not the dashboard.
- **Code removed.** About 400 lines from the Lander and about 550 from the scheduler, at best.
  But they would be replaced by roughly 300 to 450 lines of TypeScript (see below), in a second
  language, plus glue in Rust. The pure, smoke-tested decision functions (`state.rs`,
  `land/state.rs`, `route.rs`) would stay.

## Cost, cold start and job runtime

Measured from `ci_jobs`: 9 landing `test` jobs and 7 landing `deploy` jobs on Containers, class
`linux-amd64-medium` (standard-4: 4 vCPU, 12 GiB, 20 GB).

| | Queued → script started (median, range) | Script (median, range) | Queued → final (median) |
| --- | --- | --- | --- |
| test | 13.4 s (7.1–19.8) | 82 s (67–115) | 90 s |
| deploy | 12.5 s (9.1–20.3) | 174 s (156–195) | 189 s |

- **Job runtime: unchanged.** The jobs would run in the same containers. Workflows adds one
  hop: instance creation, then a step that calls the Durable Object. That is milliseconds to a
  second, against a 7–20 s container start and git fetch that stays either way. (Unmeasured.
  The docs give no latency figure, and a spike would not change the decision; see below.)
- **Container cost per job** (Containers pricing, beyond the included allowance; memory and
  disk billed as provisioned, CPU as used; the upper bound assumes all 4 vCPU busy the whole
  time):
  - test, about 90 s: memory $0.0027 + CPU ≤ $0.0072 + disk $0.0001 ≈ **≤ $0.010**
  - deploy, about 190 s: $0.0057 + ≤ $0.0152 + $0.0003 ≈ **≤ $0.021**
  - so **≤ $0.031 a landing**. The Workers Paid allowance (375 vCPU-minutes and 25 GiB-hours a
    month) covers roughly 20 to 27 landings a month at that upper bound.
- **Workflows cost per landing:** 1 invocation, about 10 to 15 steps, a few ms of CPU, and
  kilobytes of state. That is far inside the included 10 M requests, 30 M CPU-ms, 500,000 steps
  and 1 GB-month. Past the allowance it would be about $0.0001. Step and storage billing began
  2026-08-10.
- **Verdict on cost: not a differentiator.** Containers is the cost, and it is the same in
  every design.

## Rust and workers-rs: what a migration would touch

workers-rs 0.9.0 can neither define a Workflow class nor call a Workflow binding. A migration
today means one of:

1. **A separate TypeScript Worker** (for example `pigeon-flows`) that holds `LandWorkflow` and
   `JobWorkflow`. It reaches the platform through a service binding or cross-script Durable
   Object bindings (`script_name`), and the platform creates and signals instances through
   the REST API (`/accounts/{id}/workflows/{name}/instances`, `…/events/{type}`). This adds:
   - a second language and a second build;
   - a second script for the `deploy` job to deploy (`ci/deploy/deploy.sh`, `allowlist.sh`, the
     stand-in `standin.mjs`);
   - new Cloudflare API paths for wrangler's Workflow deploy calls in the deploy proxy's
     allowlist (`cf_proxy.rs`);
   - a `[[workflows]]` binding;
   - the base image already has Node 22, so the image is untouched.
2. **A hand-written JS shim** in the platform Worker. It exports a `WorkflowEntrypoint` class
   whose steps call into the Rust module. This depends on worker-build's generated entry,
   which pigeon does not control, and every step crosses the JS/WASM boundary by hand. It is
   fragile, and the next worker-build upgrade can break it.

Either way, the Lander's and scheduler's SQLite tables stay as the record `land_status`,
`ci_status`, `ci_jobs`, `ci_commit` and the console read. Workflow instance state is kept for
7 days and is not queryable across instances the way those tools need. So a Workflow would
**add** a second source of truth to keep consistent, not replace one.

Files a migration would touch: `src/capabilities/src/land.rs` (the alarm loop out, instance
create and `sendEvent` in), `ci/scheduler.rs` (`pump`, `overdue`, `outbox` and `reschedule`
out), `ci.rs` (the runner route signals the instance), `src/platform/src/lib.rs`,
`wrangler.toml`, `cf_proxy.rs`, `ci/deploy/*`, the new TypeScript Worker, and `tests/smoke.rs`
(scenarios that drive the decision functions are unaffected; the alarm-loop paths aren't
covered there today either).

## Recommendation

**Don't adopt Workflows now.**

1. It cannot run the jobs. Containers (and Buildkite, when it is on) stay regardless, and so
   does `CiRunner`.
2. There is no Rust support. Adopting it means a second language and a second deployed Worker
   in the one path that deploys pigeon itself. That raises the risk of exactly the thing CI
   protects, for a gain the record doesn't show is needed.
3. The win is limited to orchestration code, about 950 Rust lines at best. Those would be
   replaced by about 400 TypeScript lines and glue, while the SQLite records stay for the
   tools.
4. Cost and latency are a wash, both under a few cents a landing and dominated by the
   container.

**When to revisit:** when workers-rs releases Workflow support (an entrypoint macro and
binding), or if the Lander's or scheduler's alarm loop turns out to strand or duplicate work in
practice. At that point the right shape is **orchestration only, Containers and Buildkite still
running the jobs**. Phases (not to start without the person's go-ahead):

1. **Lander first.** A `LandWorkflow` with steps for check, submit test, wait for `ci-finished`
   (timeout = the job's deadline), wait for `approval` (only when required; timeout = today's
   expiry), merge, submit deploy and wait. The per-Workflow concurrency is 1. The Lander
   Durable Object keeps its table as the record and becomes a thin writer. The scheduler sends
   `ci-finished` to the instance alongside the note. One landing runs both ways in shadow
   (read-only) before the switch.
2. **Per-job lifecycle in the scheduler.** A `JobWorkflow` per job for dispatch → wait for
   runner → deadline poll → infra retry once → notify and notes. The shared pool, limits,
   routing and token grants stay in `CiScheduler`.
3. **Remove the dead alarm paths** once two weeks of landings show no divergence.

## Spike and executor cost

No spike was built, and no AI Gateway executor was used ($0). The deciding facts are static:
no Rust bindings in the pinned and latest workers-rs, and no process execution in a Workflow
step. Neither a measurement nor generated code would change them, and the only figure a spike
could add (instance start latency) is small next to the 7–20 s container start in every design.

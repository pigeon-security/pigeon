# Moving the CI runners to the CI gateway

Status: plan only, nothing built (cleanup item 13, branch R2 of docs/plans/cleanup.md, "Second
pass: the branch plan"). Do not land code for it until the rehearsals below pass.

## Goal

`CiRunner`, the Durable Object class that holds each CI job attempt's container, is the last
class core hosts for another Worker. Move it to `pigeon-ci-gateway`, which already hosts the
scheduler, without a window in which CI cannot run jobs.

## What is true today

- Core defines and exports `CiRunner` (`src/platform/src/lib.rs`, wrapping
  `pigeon_ci::containers::Runner`). Its migration is v3 (`new_sqlite_classes`), and its
  `[[containers]]` block uses the `pigeon-ci-base` image (`src/platform/wrangler.toml`).
- The CI gateway binds it as `CI_RUNNER`, `script_name = "pigeon-platform"`
  (`src/ci-gateway/wrangler.toml`). The scheduler there reaches it through the containers
  connector (`src/ci/src/containers.rs`, `BINDING = "CI_RUNNER"`, handle `<job>-<attempt>`).
- Connectors are tried in the order `pigeon_ci::pigeon::settings` lists them (Containers, then
  Buildkite). A job can be pinned to one connector (`jobs.pin`), and a pinned job retries on it
  after an infrastructure loss.
- `ci/workers.toml`: ci-gateway has `containers = []`. The deploy proxy builds its scope from
  this map compiled into it, so today it refuses container application calls for ci-gateway.
- `src/ci/*` is in both core's and ci-gateway's lanes, so a change there deploys both.

## Why a handover, not a transfer

A container class cannot be moved with `transferred_classes`. And every deploy job, including
the one that switches the runners, runs in one of the runners. So the gateway gets a new class
beside core's, new attempts go to it, old attempts drain on core's, and core's class goes last.

## Steps, each its own landing

1. **The map (a gate change, the person's).** ci-gateway's entry in `ci/workers.toml` gets
   `containers = ["CiGatewayRunner"]`. `ci/` is shared, so this deploys all six Workers; the
   map is core first. Afterwards the proxy allows container calls for that class's namespace.
2. **The class, unused.** In `src/ci-gateway`: export `CiGatewayRunner` (the same
   `pigeon_ci::containers::Runner`), migration v2 `new_sqlite_classes = ["CiGatewayRunner"]`, a
   binding `CI_RUNNER_GW`, and a `[[containers]]` block with core's image and
   `[containers.observability] enabled = false`. Nothing dispatches to it yet. This is
   ci-gateway's lane alone. Its deploy creates the container application through the proxy.
3. **A second connector, last in order.** `Containers` gets a name and a binding as fields.
   A second instance, `containers-gw` on `CI_RUNNER_GW`, is listed after Buildkite, so only a
   job pinned to it runs there. The scheduler already records the connector per job, so
   `status` and `cancel` go to the right binding with no new column. This changes `src/ci`,
   so it deploys core and ci-gateway. Then run the `test` job pinned to `containers-gw` (the
   canary): it must pass, and report back through `/ci/runner` as core's runners do.
4. **Switch the order.** `containers-gw` first, core's `containers` after it. New attempts go
   to the gateway; running ones finish on core's. Undo is this one-line order change.
5. **Drain.** Wait until no live job has connector `containers` (`ci_pool`, `ci_status`).
6. **The gateway stops using core's class.** Remove the `containers` connector and the
   `CI_RUNNER` binding from ci-gateway. It must deploy before step 7: a binding to a deleted
   class is refused.
7. **Core deletes its class.** Remove `CiRunner` from `src/platform/src/lib.rs`, its binding and
   its `[[containers]]` block, and add migration v6 `deleted_classes = ["CiRunner"]` (it was
   never transferred, so deleting it is right). Then, a gate change: core's `containers` entry
   in `ci/workers.toml` empties. Core's old container application may be left behind; the
   proxy has no DELETE for it, so the person removes it in the dashboard.

## Rehearsals before code lands

- **Step 2's deploy against the stand-in.** Run `ci/deploy/allowlist.sh` for ci-gateway with
  the new `[[containers]]` block. Every call wrangler makes must be one the proxy allows and
  `ci/deploy/standin.mjs` answers (image preparation, applications list, create). If a route is
  missing, add it to `src/proxies/src/cf_proxy.rs` and `standin.mjs` in the same branch. That
  is a gate change, and the cf_proxy change makes the landing proxy first (branch L4).
- **Step 7's deploy against the stand-in,** with `deleted_classes` and no `[[containers]]`.
- **The canary in step 3** is the rehearsal of the runner itself. Any failure there stops
  the move with nothing switched.

## Risks

- CI is how every fix lands. If step 4 breaks dispatch, the Lander cannot test or deploy a fix.
  This is why the switch is an order change alone, after a passing canary, and why core's
  runners stay until step 7. A ci-gateway rollback (the Lander's incident) still has core's
  runners to deploy with.
- Rolling ci-gateway back after step 4 strands any attempt on `containers-gw`: its status calls
  fail, and the scheduler's infrastructure-loss retry runs it again elsewhere.
- Steps 1 and 7's map changes are the person's gate changes.

## Follow-ups for the person

1. Approve step 1's map change when the branch is ready.
2. After step 7, delete core's old container application in the dashboard.

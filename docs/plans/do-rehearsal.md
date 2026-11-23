# Durable Object transfer rehearsal

T17 of [workers-split.md](workers-split.md), phase 3: before `ModelJobs` moves from
`pigeon-platform` to `pigeon-model-gateway` with `transferred_classes`, two throwaway Workers
check what Cloudflare's docs leave unsaid: whether object ids, rows and a pending alarm survive a
transfer.

- `pigeon-dotest-a` (`src/dotest-a`): the source. A SQLite class `Probe` (migration v1,
  `new_sqlite_classes`), binding `PROBE`.
- `pigeon-dotest-b` (`src/dotest-b`): the destination. The same `Probe` code; its only migration
  is v1 with `transferred_classes = [{from = "Probe", from_script = "pigeon-dotest-a", to =
  "Probe"}]`. In the same commit, a's `PROBE` binding gains `script_name = "pigeon-dotest-b"`;
  a keeps its v1 migration and still exports the class.

Both answer on workers.dev (`workers_dev = true`): no secrets, only dummy rows. Every route asks
for the header `X-Rehearsal: dotest` (a speed bump, not security):

- `POST /seed`: the object named `probe` writes rows 1 to 3 (once) and sets an alarm 24 h ahead
  if none is set.
- `GET /state`: `{worker, id_from_name, id, host, rows, alarm, alarm_fired}`. `worker` is the
  Worker that took the request, `id_from_name` the id its binding gives `probe`, `id` and
  `host` the object's own id and the Worker whose code runs it, `alarm` the pending alarm in ms
  (or null), `alarm_fired` how many alarms have run.

## Deploying them

The Lander deploys from the map built into the deployed core, so a landing that adds a Worker's
map entry does not deploy that Worker: it is not in the map yet (the Log, 2026-11-22). Each
landing here also changes shared paths (`Cargo.toml`, `ci/`, `.pigeon/`), so it redeploys core
and every Worker already in the map.

1. Land `claude/do-rehearsal-a`. Then a landing touching only `src/dotest-a/` (a one-line
   comment) deploys a alone.
2. Land `claude/do-rehearsal-b`. In that landing a (already mapped) deploys its rebinding, and b
   (not mapped yet) does not deploy. Its binding names a script that does not exist yet: either
   Cloudflare refuses a's deploy, or a's `/state` fails until b deploys. a's data is untouched
   either way, as no transfer has happened. A
   landing touching only `src/dotest-b/` then deploys b, and the transfer happens there.
   Splitting b's commit (b's crate and map entry first, a's rebinding in a later landing that
   touches only `src/dotest-a/wrangler.toml`) avoids the gap and also checks that a's old
   binding forwards to b.

The map lists `dotest-b` before `dotest-a`, so any later landing that deploys both deploys the
destination first.

workers.dev needs one more allowlist entry: with `workers_dev = true`, wrangler reads the
account's workers.dev subdomain (`GET /accounts/{account}/workers/subdomain`) after the upload
and before it enables the script's subdomain, and the deploy proxy does not allow it. Until it
does, the deploy fails after the upload.

## Running it

With `A=https://pigeon-dotest-a.<subdomain>.workers.dev` and
`B=https://pigeon-dotest-b.<subdomain>.workers.dev`:

1. After a deploys:
   `curl -sS -H 'X-Rehearsal: dotest' -X POST $A/seed`, then
   `curl -sS -H 'X-Rehearsal: dotest' $A/state`. Record the answer: `id`, `rows`, `alarm`.
2. After b deploys (the transfer): `GET /state` on both `$A` and `$B`, and compare with step 1.
3. A few minutes later, `GET /state` on both again: the alarm is still pending.

It passes when, on both URLs after the transfer:

- `id` and `id_from_name` equal step 1's `id` (ids survive the transfer);
- `rows` are step 1's three rows, with the same `at` (data survives);
- `alarm` is step 1's value, still pending in step 3, and `alarm_fired` is 0 (the alarm was
  carried, not lost or fired early);
- `host` is `pigeon-dotest-b` (the object runs b's code), whichever URL answered.

Any other result is the fallback in workers-split.md, phase 3: no transfer, a new class in
`pigeon-model-gateway`. Record the answers in workers-split.md's Log either way.

## Removing them

Done on 2026-11-22. Their map entries, jobs, deploy scripts and crates came out in the
delivery-speed batch (workers-split.md, Log). Each shared landing had spent two of CI's four
containers deploying them.

The deploy proxy has no script DELETE, so the scripts are deleted by hand in the dashboard:
`pigeon-dotest-a` first (its `PROBE` binding names the class in b), then `pigeon-dotest-b`. Left
alone they are inert: dummy rows, and an alarm that fires once. The two workers.dev hostnames
can then leave the session environment's network allowlist.

## Second rehearsal: a source stops exporting a transferred class

Cleanup item 6 (docs/plans/cleanup.md, branch R1). Core still exports the shims `CiScheduler`
and `ModelJobs` (`src/platform/src/lib.rs`) and builds them `settings::ci()` and
`settings::model_jobs()` (`src/platform/src/settings.rs`). Both classes moved to the gateways
with `transferred_classes`. Core's bindings already name the new hosts with `script_name`
(`src/platform/wrangler.toml`), and core keeps migrations v2 and v4, which once created them.

The question: does Cloudflare take an upload of a script whose migrations once created a class
that it no longer exports, when that class was transferred away? Incident 5 (workers-split.md,
Log) shows an upload can be refused over a moved class (10061), and core runs the Lander, so
this is asked of a throwaway pair first.

### The pair

The first pair is reused if it still exists: `pigeon-dotest-a` exports `Probe` with migration
v1, its `PROBE` binding names `pigeon-dotest-b`, and b hosts the class since the transfer. Check
in the dashboard before anything else (cleanup.md, Follow-ups for the person, 4).

- **If both still exist.** Only a is needed. Restore `src/dotest-a` from git history (the
  delivery-speed batch removed it; `git log --diff-filter=D -- src/dotest-a`). Change one thing:
  a no longer exports `Probe` (drop the class's `#[durable_object]` struct and its `impl`). Keep
  a's v1 migration and its `PROBE` binding with `script_name = "pigeon-dotest-b"`. Never add
  `deleted_classes` for a transferred class: it would ask Cloudflare to delete b's objects.
- **If they were deleted.** Rebuild both from history and run the first rehearsal's three
  landings (Deploying them, above). Then do the step above.

### Landings

1. A gate change (the person's approval): a's crate as it was, a's `[[worker]]` entry in
   `ci/workers.toml`, `ci/deploy/deploy-dotest-a.sh` and `[jobs.deploy-dotest-a]` in
   `.pigeon/ci.toml`. It deploys core and the proxy (the map), not a (cleanup item 4).
2. A landing touching only `src/dotest-a/`, the export removed. It is a's lane alone.

### What to record

- Did a's deploy upload? The deploy job's output: accepted, or refused with which code and text.
- After it: `GET /state` on `$A` and `$B` (Running it, above). `host` is `pigeon-dotest-b` and
  `id`, `rows` and `alarm` are unchanged.
- A second deploy of a (a one-line comment in `src/dotest-a/`). Incident 5 was missed because
  the first rehearsal never redeployed the source; this one does.
- A deploy of a's previous build, which still exports `Probe`, as the Lander's rollback would.
  It must also be accepted, so a failed R1 landing can be rolled back.

It passes when all four answers are as above. If a's upload is refused, R1 is not built: core
keeps the shims, and the refusal's code and text go in cleanup.md's Log.

### Then the code (branch R1, after it passes)

- `src/platform/src/lib.rs`: remove the `CiScheduler` and `ModelJobs` structs and their
  `impl`s, and correct the module's comments.
- `src/platform/src/settings.rs`: remove `ci()` and `model_jobs()`. Drop `pigeon-models` from
  `src/platform/Cargo.toml` only if nothing else in the crate uses it.
- `src/platform/wrangler.toml`: keep migrations v2 and v4 and both bindings with `script_name`.
  Only the comment that says "This Worker keeps both classes' migrations and exports" changes.
- Lanes: `src/platform/*` is core's alone. No smoke scenario: the crate builds for WASM only, and
  `cargo check -p pigeon-platform` in the gate covers it.
- Land on a quiet queue, with no deploy of core in flight. If core's upload is refused anyway,
  the Lander's incident keeps core on its good build (as in incident 5); the fix is to revert.

A cheaper option, for the person to choose instead: skip the pair, and remove only `ModelJobs`
from core first, on a quiet queue. A refused upload changes nothing on Cloudflare (incident 5:
core kept serving its build), and the only user of model jobs is the AI agents planner. Then
`CiScheduler` in a second landing. The plan above is the conservative one, as do-rehearsal did.

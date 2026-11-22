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

The deploy proxy has no script DELETE. Delete `pigeon-dotest-a` and `pigeon-dotest-b` by hand in
the dashboard (b first: it holds the class a's binding names), or leave them inert: they hold
only dummy rows, and an alarm that fires once. Their map entries, jobs, deploy scripts and crates
come out in a later landing, before or after the scripts are deleted.

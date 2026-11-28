# Ingest index: only stored blobs, and `head-many` from D1 first

Status: **building** (2026-11-28). Approved by the person the same day: plan, build and land.
Related: [ingest.md](ingest.md) decision 13.4 (the dedupe index),
[ingest-throughput.md](ingest-throughput.md) (the batched existence check and its deviation),
[email.md](email.md) ("D1 table names").

## What was wrong

`index::apply_part` turned every manifest line into D1 rows, whatever kind of run wrote it:

| Line (`verdict` / `reason`) | Run | `blobs` row | `sightings` row | Is the blob in R2? |
| --- | --- | --- | --- | --- |
| `upload` / `new`, `unattested` | live | yes | yes | yes: the PUT succeeded |
| `duplicate` / `in_r2` | live | yes | yes | yes: HEAD or a create-only `Exists` saw it |
| `duplicate` / `seen_in_run` | live | yes | yes | yes: the run's first sighting stored it (a failed upload is forgotten, so the next sighting becomes canonical) |
| `would_upload` / any | dry | no | yes (`would_upload`) | no |
| `duplicate` / `seen_in_run` | dry | **yes** | yes | **no**: the first sighting was only a `would_upload` |
| `duplicate` / `in_r2` | dry | yes | yes | yes at that time, but it is a dry run's result |
| `conflict`, `failed`, `skipped` | either | no | yes | no (a conflict goes to `quarantine/`) |

So a `blobs` row was not proof that R2 holds the blob: a dry run's repeated would-be uploads were
indexed as stored. That is why `head-many` could not answer from D1, and anything reading
`blobs` or the dry-run `sightings` would show content that was never stored.

### willowgraysen, read-only check

Run `20261128-b2-6f65616b335f326c6a3438616d507530` (dry run, Cloudflare Container) ended with
1,255 lines, all `would_upload`, no duplicates (`data_ingest_status`). Its parts were **not applied**
to willowgraysen's D1, and could not have been:

- The instance plan creates no `{instance}-ingest-manifests` queue and no R2 notification for it
  (only the backup queue); nothing delivers part events to the consumer.
- willowgraysen's D1 `willowgraysen-data` holds the lite store (`data_email_status` reads its
  counts). Its migrations refuse a database that holds `blobs`, `hints` or `applied`, and the
  index schema was never run on it, so an apply would have failed on the missing `applied`
  table, as one D1 batch (all or nothing).
- Even applied, that run would have made no `blobs` rows (no `duplicate` lines), only 1,255
  `would_upload` sightings.

There was no D1 query tool to look at the tables directly; the above is from the code, the
plan's resources and the run's counters. The same holds for pigeon.dev (`pigeon-data`) and any
other lite store: no instance has index rows today.

## The fix

1. **Own tables in the lite store.** Migration `0007_ingest_index` creates the index in the
   instance's D1 with prefixed names (email.md's option): `index_blobs`, `index_sightings`,
   `index_hints`, `index_runs`, `index_applied`. They cannot clash with the lite `sightings`,
   and `migration_refusal` still refuses an old unprefixed index.
2. **Dry runs index nothing as stored.** `apply_part(part, lines, dry_run, now)` marks a dry
   run's part in `index_applied` and writes nothing else: its manifest and report stay in R2.
   A live run's part writes `index_blobs` only for `upload` and `duplicate` lines with a hash,
   and a sighting for every line (duplicates included, pointing at the canonical blob by SHA-256).
3. **Who says it is a dry run.** For a managed run (the Cloudflare Container) the data Worker
   indexes a manifest part when the runner writes it through `put-object`, and the dry-run flag
   comes from the run's signed grant, not from the runner. The queue consumer (a standalone
   deployment, ingest.md 13.4) reads `job.json`, which core writes and the runner may not, and
   skips a part whose job it cannot read. Both index only when `0007_ingest_index` is applied.
4. **Cleanup.** `index::cleanup_candidates` and `index::cleanup` repair an index made by the old
   code, for either table set (`Tables::LITE` or `Tables::LEGACY`):
   - the dry runs' sightings and hints are deleted (by run id);
   - a blob whose `first_run` has no stored-proof sighting (`upload`, or `duplicate` + `in_r2`)
     for it but another run has one is re-marked with that run and time;
   - a blob with no such sighting anywhere is a candidate: the caller HEADs it in R2 and passes
     the ones R2 does **not** hold, and only those rows are deleted. A row whose blob R2 holds is
     never deleted.
   It is idempotent. No instance needs it today (see above); the new tables only ever get rows
   from the fixed code.
5. **`head-many` D1 first.** When `0007_ingest_index` is in `migrations`, the data Worker looks the
   batch up in `index_blobs` (one D1 batch), answers the hits from it, and HEADs R2 only for the
   misses. Before the migration it HEADs every key, as today. A D1 hit means a live run stored
   the blob; blobs are never deleted (decision 9), and a miss falls back to R2, so the answers
   match R2's. Create-only uploads remain the final guard for every blob D1 does not know.

## What reaches an instance, and what the person approves

The data Worker code and the migration reach an instance only with its next instance plan and
apply, which the person approves with their passkey. Per lite-store instance (willowgraysen,
pigeon.dev; noisypigeon only if it has a lite store), the plan shows:

- `Migrate` with `0007_ingest_index` (creates the five empty `index_` tables);
- the data Worker upload at this revision (the `put-object` indexing and D1-first `head-many`).

Nothing runs the cleanup: there are no old rows. Until an instance is applied, `head-many` keeps
HEADing R2 for every key.

## Phases

| Phase | What | State |
| --- | --- | --- |
| 1 | Plan, `index_` tables + migration `0007`, `apply_part` dry-run aware, cleanup, D1-first `head-many`, smoke tests | landing |
| 2 | Instance plan/apply per instance (person, passkey) | waiting |

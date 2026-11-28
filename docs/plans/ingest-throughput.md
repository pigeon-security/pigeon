# Ingest throughput: run time from file count, a parallel engine, live runs past one page

Status: **building** (2026-11-28). Approved by the person the same day: plan, build and land, for
all three hosts (Backblaze B2 on a Cloudflare Container, Scaleway Object Storage on a Scaleway
Instance, DigitalOcean Spaces on a Droplet). Related: [ingest.md](ingest.md) (decision 13, the
runner), [ingest-bucket-connections.md](ingest-bucket-connections.md),
[scaleway-ingest.md](scaleway-ingest.md), [digitalocean-ingest.md](digitalocean-ingest.md).

## Why

The willowgraysen dry run `20261128-b2-6f65616b335f326c6a3438616d507530` (Cloudflare Container,
B2 `ca-east-006`, prefix `txt/`, about 471,000 files of about 640 bytes each) ran at about
**3.5 files a second** (about 70 ms a file) with no failures. Three things were wrong:

1. **The deadline came from bytes alone.** 300 MB at 80 MB/s is about four seconds, so the plan
   gave the run its 6-minute floor; at 3.5 files a second the prefix needs about 37 hours.
2. **The engine is serial.** For each object it opens the source, spools, hashes, HEADs the
   destination and PUTs, one round trip after another; `job.threads` was ignored.
3. **A live run was limited to one listing page** (1,000 objects) of objects at most 16 MiB,
   with three copies of the same guard (Backblaze, Scaleway, DigitalOcean).

## Design

### 1. Run time from the file count (slice a)

- `estimate::estimate_run` takes the bytes, the **objects**, the unique share and a
  `Throughput` (per-object latency, concurrency, and the last **measured** rate, if any). Time is
  `max(bytes / read rate, objects / files-per-second)`. Files a second is the measured rate when
  there is one, else `concurrency × 1000 / per-object ms` (Cloudflare 70 ms, Droplet and
  Scaleway Instance 50 ms, local 20 ms). The estimate says which bound won (`bound`: `bytes` or
  `files`), its `files_per_s`, the measured run it used, and `confidence: low` when the object
  count was extrapolated from a sample of small objects (under 64 KiB on average).
- **Measured rate.** Every report now records `files_per_s` and `bytes_per_s`. A plan reads the
  newest finished or stopped run of the same host on the same instance (its `report.json`, else
  its `status.json`, so the willowgraysen dry run above already counts) and prefers that rate.
  A run of fewer than 100 files or under a minute is not used.
- **`objects`.** Every plan accepts `objects` (the provider console's object count) next to
  `size_bytes`. Without it the count is extrapolated from the first listing page.
- **Deadline** = `1.5 × high estimate + 10 minutes`, clamped to the host's ceiling: Cloudflare
  24 hours (the container limit), DigitalOcean 24 hours, Scaleway its fixed 24-hour lifetime.
  DigitalOcean's `max_minutes` becomes an optional override of the computed default.
- **The runner stops itself** shortly before the approved deadline (`job.max_minutes`, 5 minutes
  or a tenth of the run before it, whichever is less), so the run ends `stopped` with a
  checkpoint rather than being killed.
- **Continue.** `data_ingest_plan {…, continue: "<run id>"}` plans a continuation of a run that
  stopped (by its deadline, a stop, or a spent grant). The plan reads the prior run's `job.json`
  and `checkpoint.json` from the Data bucket, keeps its source, prefix and dry-run setting, and
  carries the checkpoint in the new job (`job.resume`). It needs a new approval.
  **Deviation from the approved wording ("same run id"):** the continuation gets a **new run id**
  linked to the old one. A run id keys its destination grant (the D1 `ingest_grants` row only
  accepts its first expiry), its immutable `report.json`, its terminal and Jobs audit record,
  and the approval subject; reusing it would make the continued run's writes and report be
  refused. The engine starts from the carried checkpoint (its `after_key` and counters), so no
  object is skipped and nothing done is redone; the report names the run it `continues`.
- **Bug fixed:** a named Scaleway connection ignored `size_bytes` and estimated (and budgeted)
  only the first page. It now honours `size_bytes` and `objects` like the other hosts.

### 2. Speed (slice b)

- `job.threads` is a worker pool (default 32 on the 4-vCPU hosts). Workers only **fetch and
  hash** (and upload, see below). A single **coordinator** takes their results in listing order
  (sequence numbers and a reorder buffer, at most `4 × threads` objects in flight) and makes
  every decision exactly as the serial engine does: the first sighting of a SHA-256 is canonical
  (upload), later ones are duplicates; zip archives are opened by the coordinator in order.
  A later object whose SHA-256 has an upload in flight waits for that upload's result, so a failed
  upload makes the next sighting canonical, as in the serial engine.
- **Uploads** of distinct blobs run on the pool. A manifest line is kept only when its object and
  everything before it are finished, so the manifest, its part boundaries and the checkpoint (the
  contiguous-finished watermark) are the same as the serial engine's.
- **Batched existence check.** The coordinator asks for up to 500 first-seen SHA-256s at once
  (`Dest::head_many`; on the Data Worker the `head-many` operation, payload keys of the run's
  grant only, answered by concurrent R2 HEADs inside the Worker: one runner round trip per batch).
  It is a hint: a missing or failed batch falls back to one HEAD per blob, and an older Data
  Worker without the operation still works.
  **Deviation from the approved wording ("D1 index first"):** the D1 `blobs` table is not
  certain evidence. `index::apply_part` indexes every `duplicate` line as a stored blob, and in a
  dry run a repeated would-be upload is a `duplicate` (seen in run) of a blob R2 never received,
  so a D1 hit could leave a file behind that R2 does not hold (decision 13.1 forbids it). R2 HEADs
  inside the Worker keep the batching benefit. Follow-up: index only blobs a live run stored,
  then D1 can answer first. Done in [ingest-index-dry-run.md](ingest-index-dry-run.md): D1 answers
  first once an instance has migration `0007_ingest_index`.
- **Create-only uploads.** Every blob PUT is create-only (`If-None-Match: *` on R2's S3 API; the
  managed destination already was). An upload refused because the blob already exists with the
  same identity is recorded as a **duplicate** (`in_r2`), never an error. The D1 index stays keyed
  by SHA-256, and every source object still gets its sighting pointing at the canonical blob.
- **Listing prefetch** (the next page is fetched while this one is worked on), files up to 1 MiB
  stay in memory instead of the spool, and HTTP connections are kept and reused (up to 128 idle per
  host).
- R2's S3 API (the non-managed destination): `If-None-Match: *`; on 412 the HEAD decides: the
  same identity is `Exists`, an object with no SHA-256 (unattested) is replaced on the retry as
  before, anything else is an error. Multipart writes HEAD first.
- Expected: Cloudflare, ~70 ms per small file serial → about 32 in flight; the per-file cost moves
  from round trips to the coordinator's ordering and the batched check, so 100-400 files a second
  is expected for small files (to be measured: the first run reports it, and later plans use it).

### 3. Live runs beyond one page (slice c)

- One guard, `pigeon_ingest::managed_destination::live_guard`, used by all three hosts. A live run
  may cover many pages when the whole prefix's `size_bytes` is declared (a truncated listing
  without it is refused, since the grant would cover one page); its grant (write bytes and write
  attempts) is sized from the declared totals with headroom, and the Data Worker enforces it.
  When it is spent (HTTP 429, `Error::Exhausted`) the run stops **before** the object that found
  it, records nothing from there on, writes its checkpoint where possible and ends `stopped`
  with `budget_spent: true`; a continuation with a new grant finishes the rest.
- Objects over 16 MiB go through the multipart publisher when the Data Worker's readiness reports
  `multipart_configured` (up to its 160 GiB limit); otherwise the run **skips them unread and
  lists them** in its report (flag `too_large`, report list `too_large`), rather than refusing the
  run. The job carries the limit as `skip_over`; zip members over it are skipped the same way.
- **Live cap.** A live run may declare at most 10 GB of source bytes. The plan's `max_live_bytes`
  (up to 100 TB) raises it for one run; it appears in the plan the person approves with their
  passkey, so raising it is their call.

## Rollout

- **Compatibility.** Every new job field (`max_minutes`, `resume`, `skip_over`) is omitted when
  unset, so older approvals keep their digests. An older runner ignores them: it does not stop
  itself early, starts a continuation from the beginning of the prefix (safe: blobs already
  stored are duplicates, just slower) and does not skip large objects (they fail as before). Plan
  continuations and large live runs only once the host runs this revision.

- The Data Worker's `head-many` operation reaches an instance with its next reviewed instance
  plan/apply (passkey). Until then the runner falls back to one HEAD per blob.
- **Cloudflare (willowgraysen):** the runner image is built and pushed by hand into the instance
  account's registry and pinned in the instance record; see the steps in the final report and
  [ingest.md](ingest.md). DigitalOcean and Scaleway build the runner at the approved commit, so
  they get the new engine with the next plan.

## Phases

| Phase | What | State |
| --- | --- | --- |
| a | Estimate from file count, `objects`, measured rate, deadline, runner self-stop, continue, Scaleway `size_bytes` fix | landed |
| b | Parallel engine, `head_many` (+ Data Worker op), create-only uploads, prefetch, memory spool, connection reuse | landed |
| c | Shared live guard, grant from declared totals, >16 MiB multipart or skip-and-report, live cap, spent grant stops for a continuation | landed (BjS0aK6AtNkKP7Bv) |

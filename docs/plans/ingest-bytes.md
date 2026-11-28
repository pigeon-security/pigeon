# Ingest bytes: where the time goes, and sharded runs

Status: **building** (2026-11-28). Approved by the person the same day: investigate, plan, build
and land. Related: [ingest-throughput.md](ingest-throughput.md) (the parallel engine),
[ingest.md](ingest.md) (decision 13, the runner).

## Findings

The live run `20261128-b2-53426b6b6f3575734248697746656444` on willowgraysen (B2 bucket
`wg-2tr6qn-snapshot-2026-10-17`, 145,991 files, 211.4 GB, about 1.4 MB a file; Cloudflare
Container `standard-4`, runner revision 9de99a71, 32 worker threads, owning-account source and
the Data Worker's managed destination):

| After | Files | Bytes read | Rate | Failures |
| --- | --- | --- | --- | --- |
| ~6 min | 7,136 | 2.24 GB | ~21 files/s, ~6.6 MB/s | 0 |
| ~8 min (status) | 10,743 (830 duplicates, 16 too large) | 3.05 GB | 6.17 MB/s | 0 |

The plan assumed 80–120 MB/s, so the 111-minute cap covers about 40–45 GB and the bucket needs
about five continuations. A dry run of small text files did ~36 files/s (1.4 MB/s).

### What one object costs (from the code)

With 32 threads and ~21 files/s, each worker spends **about 1.5 s per object** moving 1.4 MB:
about 1 MB/s per thread. The transfer itself is a small part of that; the run is
**latency-bound per object**, and the in-flight window is fixed by the thread count. Per object
the runner makes, one after another:

1. **A signing call** to the owning Data Worker (`/ingest/source/read`, `get`): a Worker
   request, a Secrets Store read and a presign, for every object.
2. **A B2 GET** of the presigned URL. The download agent (and the signing agent) were built with
   ureq's defaults: **one idle connection per host**. With 32 threads nearly every GET and
   every signing call opens a **new TCP and TLS connection** (two to three extra round trips
   each) before B2's time to first byte.
3. **The body transfer**, hashed (SHA-256 and BLAKE3); files over 1 MiB are written to the
   spool on disk and read back to upload.
4. **The existence check**: the coordinator's batched `head-many` ran **on the coordinator
   thread, synchronously**, and with fewer than two new hashes it fell back to a single HEAD,
   also on the coordinator. While it waits no object is decided and no upload is queued.
5. **The upload** to the Data Worker (`put-blob`): the Worker charges the run's budget in D1
   (a D1 round trip), then reads the whole body, hashes it in Wasm, and writes R2. The D1 charge
   and the body read were sequential.

Nothing here is bandwidth-bound: a `standard-4` container (4 vCPU, 12 GiB, 20 GB disk) has no
published per-instance bandwidth limit, B2 publishes no per-connection download cap (it
recommends parallel connections), and the Worker's request body limit (100 MB) is above the
16 MiB single-write limit. `standard-4` is already the largest Cloudflare Containers instance
type, so a larger instance is not an option; more objects in flight, fewer round trips per
object, and more containers are.

### What the runner did not tell us

`status.json` and `report.json` gave counters and an overall rate only, so the split above is
from reading the code. Slice a makes the runner report it.

## Design

### Slice a: stage timings and the fixes

**Stage timings.** `status.json` and `report.json` gain `timings` (absent in older runs):

| Stage | What is timed |
| --- | --- |
| `list` | each listing page (a Worker call and a provider listing) |
| `source_open` | `Source::open` up to the response headers: signing plus B2 time to first byte |
| `source_read` | reading the body (bytes = bytes transferred) |
| `spool_write` | writing a file over the memory limit to the disk spool |
| `hash` | SHA-256, BLAKE3 and the kind sniff |
| `lookup` | existence checks (`head-many` batches and single HEADs) |
| `upload` | `put_blob`: the Worker round trip and the transfer (bytes = bytes sent) |
| `control` | manifest parts, checkpoints and status writes |
| `coordinator_wait` | the coordinator waiting for workers with room in the window |
| `window_full` | the coordinator waiting with the in-flight window full |

Each stage has `count`, `ms` (summed over threads), `bytes`, and, for the per-object stages
(`source_open`, `upload`, `lookup`), `p50_ms` and `p95_ms` from a fixed log-bucket histogram
(atomic counters, no allocation per object). `timings.threads` and `timings.elapsed_ms` let a
reader turn summed milliseconds into a share of worker time, and `timings.bottleneck` names the
worker stage with the largest share (or `window` when the coordinator mostly waited with the
window full). The clock is a monotonic microsecond clock (`Clock::now_us`), zero by default, so
tests and older clocks are unaffected.

**Fixes** (runner image; the Data Worker items need an instance plan):

1. **Keep connections.** The owning-account source's signing and download agents keep up to 128
   idle connections per host, like the destination: one TLS handshake per thread, not per object.
2. **Sign ahead.** The source asks the Data Worker for up to 64 presigned URLs at once
   (`get_many`, the next keys of the listing), cached for 40 seconds (they last 60). A Data Worker
   without `get_many` answers 400 and the runner falls back to one `get` per object for the rest
   of the run. One Secrets Store read and one Worker request per 64 objects.
3. **Lookups off the coordinator.** Existence checks run on their own small pool (2 to 8
   threads) in batches of up to 500; the coordinator sends a batch and keeps deciding objects
   whose answers are in. An object whose first-seen hash is still being looked up waits, as one
   whose hash is uploading does. A failed batch falls back to single HEADs on the lookup thread.
   The decisions are unchanged (the answer is the same hint the batched check already gave).
4. **No disk round trip for files up to 16 MiB.** Files up to the spool's memory limit (16 MiB
   on the runner, the single-write limit) are staged in memory within a memory budget (3 GiB on
   the runner, `SPOOL_MEMORY_BYTES`); past the budget they go to the disk spool as before. The
   bytes uploaded are the bytes hashed, from memory or disk alike. True streaming (upload while
   reading) is not possible: the blob key is the SHA-256, known only at the end of the read, and
   the decision (upload or duplicate) needs it.
5. **More in flight.** Cloudflare runs get 64 threads (window 256 objects); memory staging is
   bounded by the budget above, not by the window.
6. **Data Worker `put-blob`:** the D1 budget charge and the body read run concurrently (one D1
   round trip less latency per upload). The body is still buffered and both hashes checked before
   the create-only write: the stored identity must be proven, and 16 MiB fits the Worker's
   memory.
7. **Estimate.** A measured run whose average object is at least 256 KiB sets the byte rate too
   (its bytes a second, and 1.25 times that as the fast rate), not only the files a second. Before
   any measurement Cloudflare assumes 20–50 MB/s, not 80–120.

Expected: per object, the fixes remove two to four round trips of setup (signing and TLS), the
coordinator's serial waits and the spool round trip; with 64 threads that is roughly 3–6 times
the measured rate, **about 20–40 MB/s per container** for 1.4 MB files. The timings of the first
run on the new image will say which stage is left (most likely B2's time to first byte and the
Worker's upload round trip), and later plans use its measured rate.

### Slice b: sharded runs

A plan option `shards` (1 to 8, default 1) splits one run into N containers on disjoint parts of
the same prefix, under one approval.

- **Partition.** Shard `i` of `N` takes the keys whose `shard_of(key, N) == i`, where `shard_of`
  is the first 8 bytes of the key's SHA-256, big-endian, modulo N. Every key belongs to exactly
  one shard, and shards are balanced by count (and, for many files, by bytes) whatever the key
  layout. Each shard lists the whole prefix and keeps its own keys (N listings: 146 pages each
  for 146,000 files, negligible next to the reads). Prefix boundaries were rejected: they need a
  full listing at plan time, and one hot prefix makes one long shard.
- **Runs.** Shard runs are named `{run}-s{i}` (the parent run id is at most 88 characters). Each
  has its own job (`job.shard = {index, count, parent}`), container (its own Durable Object), grant,
  checkpoint, manifest, status and report. The approval covers the parent run and lists the shard
  runs; `data_ingest_start` starts all of them.
- **Budget.** Each shard's grant gets `floor(approved / N)` write bytes and write attempts, so the
  sum never exceeds the approved cap. A shard that spends its share stops cleanly
  (`budget_spent`) and is continued like any run.
- **Status, report and audit.** `data_ingest_status` on the parent run answers each shard's
  state and status and their sum (counters added, the slowest ETA, timings summed); the combined
  report is the shards' reports merged. The Jobs audit gets one terminal record for the parent,
  written when the last shard ends, with the summed counts and the worst outcome.
- **Continuation per shard.** `continue: "{run}-s{i}"` continues one shard (its job keeps the
  shard), and `continue: "{run}"` continues every shard that did not finish.
- **Duplicates across shards.** Each shard decides its own keys in order, exactly as a serial
  run would for that subset. A blob seen in two shards is uploaded create-only by whichever gets
  there first; the other is told it exists and records a `duplicate` (`in_r2`). So the **stored
  blobs and the sightings are the same as a serial run's**; what may differ is **which sighting
  is the upload** (the "first") and whether a later sighting's reason is `seen_in_run` or
  `in_r2`. The D1 index keys blobs by SHA-256 and records every sighting; its canonical sighting
  is the one that uploaded, so it too may differ from a serial run only in which sighting that
  is, never in what is stored.
- **Limits.** Cloudflare publishes no limit on concurrent container instances, only account
  totals (15,000 vCPU, 60 TiB memory); 8 `standard-4` shards are 32 vCPU and 96 GiB. N is
  bounded at 8. The plan shows N, the shard runs, and the cost: N containers at ~$0.40 an hour
  each for the (shorter) per-shard time.

## Phases

| Phase | What | State |
| --- | --- | --- |
| a | Stage timings in status and report; keep-alive, sign-ahead, lookups off the coordinator, memory staging to 16 MiB, 64 threads, concurrent D1 charge, byte-rate estimate | landing |
| b | Shards: partition, shard jobs, per-shard grants within the cap, combined status/report, one audit, per-shard continuation | planned |

## Rollout

- The runner changes reach willowgraysen when the person builds and pushes a new runner image and
  re-pins it (the steps are in the report). The Data Worker's `get_many` and the concurrent charge
  reach it with an instance plan; until then the runner falls back to one `get` per object.
- Every new field (`timings`, `job.shard`) is omitted when unset, so older jobs keep their
  approval digests and older readers ignore it.

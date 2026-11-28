# Ingest key scope

Status: built on `claude/ingest-key-scope`; landing, then live verification on willowgraysen.

A named data-ingest bucket connection (`src/capabilities/src/ingest/connections.rs`) keeps its
source key in the Directory, sealed with AES-GCM whose associated data is the connection's
*scope* fingerprint. Until now that fingerprint (`data-ingest-bucket-v1`) hashed the whole
binding. A Backblaze binding carries `runtime` (the pinned ingest runner image and revision), so
re-pinning the runner image changed the scope and `open()` refused the key with "Replace the
saved credential for the newly approved bucket scope." The person had to set the key again for
a change that does not touch what the key may read.

## Decision: what is access scope

The fingerprint covers what decides which objects the key may be used against, and nothing else:

| Field | In scope | Why |
| --- | --- | --- |
| instance id, instance account (`account`) | yes | the key belongs to one instance in one account |
| connection name (the map key) | yes | the Directory slot and the pin name |
| provider | yes | a different provider is a different endpoint and key space |
| region | yes | selects the endpoint (B2, Scaleway) or Spaces region |
| bucket | yes | |
| prefix | yes | the approved read boundary; narrowing or widening both invalidate |
| DigitalOcean team | yes | Spaces keys are checked against the approved team |
| Scaleway project | yes | the project that owns the bucket |
| Backblaze `runtime` (`image`, `revision`) | **no** | which runner reads, not what it may read |
| Backblaze `name` | **no** | a provenance label (`backblaze:<name>` on the run), not access |

The v2 fingerprint is `hash(("data-ingest-bucket-v2", id, account, name, provider, owner, region,
bucket, prefix))`, the same tuple shape the owned-source scope (`owned-ingest-source-v1`) already
uses; both now come from one `access()` helper. Any future binding field is outside the scope
unless added to `access()` on purpose.

## Migration without re-entry

Saved keys are sealed under v1. `connections::sealed(record, name, saved_scope)` classifies a
saved scope as:

- `Current`: the v2 scope of the record as it is.
- `Legacy`: a v1 scope of the record as it is, either with its current binding or, for Backblaze,
  the same binding with no runtime pin (keys saved before the first pin). Both name exactly the
  current access scope.
- `Changed`: anything else. Refused as before; the key must be set again.

`open()` (used by `data_ingest_connection_verify`, `data_ingest_plan` and, through
`open_pinned`, by approved starts) opens a `Legacy` key with its old scope as associated data
(AES-GCM authenticates that the ciphertext was sealed to that exact v1 scope), seals the same
plaintext to the v2 scope, and writes it back through a dedicated Directory op,
`data-ingest.bucket.reseal`. The Directory swaps it in only if the instance version is still the
one opened, the stored ciphertext generation is still the one opened, the new scope is the
current v2 scope, and the stored scope is still `Legacy` for the current record. A lost race
answers "changed while it was re-sealed. Try again." and writes nothing.

What this can never do: accept a key sealed for another access scope (its v1 scope does not match
any candidate, and its AAD would not open under ours), or change the plaintext (the re-seal
writes back exactly what it opened). A v1 key saved under a runtime other than the current one
(or none) cannot be recognised, because the old runtime is not recorded; such a key still needs
setting again. That is why the fix lands before the runtime re-pin below.

### Generations and pins

Plans and approvals pin `{name, scope, generation}`, where `generation` hashes the ciphertext.
A re-seal changes both, so a plan or approval made before the migration is refused by
`open_pinned`, with "The saved bucket credential was re-sealed to its access scope after this
plan. Make a new ingest plan." This is deliberately conservative: an old pin never authorises
the re-sealed key. New plans pin the v2 scope and the new generation, and later runtime re-pins
keep both.

`data_ingest_connections` reports a `Legacy` key as `configured` with `reseal_pending: true`; the
listing itself never writes. Setting a key (`data-ingest-connection-key-set`) seals to v2.

## Live state (willowgraysen)

- `b2-wg-grmwyl-deduplicated-misc`: runtime pinned (`…@sha256:bdf7c8f5…`, revision `48512c5…`),
  key saved after that pin. Its v1 scope includes that runtime, so it is `Legacy` and migrates on
  the first verify. The person then re-pins the runtime (`…@sha256:f49fdd64…`, revision
  `9de99a7…`); the v2 scope is unchanged by that.
- `b2-wg-ooeami-snapshot-2026-10-09`: no runtime, prefix `txt/`, key saved without one: `Legacy`,
  migrates on first verify, and a later runtime pin keeps it.

## Phases

| Phase | What | State |
| --- | --- | --- |
| 1 | v2 access-scope fingerprint, `sealed()` classification, re-seal on open, `data-ingest.bucket.reseal`, refused-pin message, smoke test | Built |
| 2 | Land before the runtime re-pin | Pending |
| 3 | Verify both willowgraysen connections with `data_ingest_connection_verify` (migrates them) | Pending |

## Tests

`data_ingest_bucket_key_scope_covers_access_only_and_migrates_v1_keys` in
`tests/smoke/console.rs`: a runtime re-pin, unpin or provenance rename keeps the scope; a bucket,
prefix, region, provider, account, instance or Scaleway project change moves it; a v1 scope of the
current binding (with or without its runtime) is `Legacy`; a v1 scope of another access scope,
another runtime or another connection is `Changed`. The AES-GCM open and re-seal run on Web
Crypto in the Worker and are covered by the live verification.

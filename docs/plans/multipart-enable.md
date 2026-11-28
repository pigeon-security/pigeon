# Enabling the managed-ingest multipart publisher

Status: **building** (2026-11-28). Approved by the person the same day: plan, build and land.
Related: [ingest.md](ingest.md) ("Managed destination multipart publication"),
[ingest-throughput.md](ingest-throughput.md), [linked-data.md](linked-data.md),
[linked-accounts.md](linked-accounts.md).

## Why

Managed ingest skips objects over 16 MiB (`too_large`) until the linked Data Worker's multipart
publisher is configured. Configuring it by hand meant creating an R2 S3 key, hashing its JSON,
naming a Secrets Store slot from two hash prefixes, writing the secret and proposing the
fingerprint. This plan makes it two clicks (manual) or one opt-in scope (automatic), adds the
rollout checks the ingest plan still listed as open (a lifecycle rule and a real canary), and
lets a run pick up exactly the files an earlier run skipped.

## Design

### The credential, both ways

The Data Worker's credential is unchanged: the exact JSON
`{"account":…,"bucket":…,"keys":"ACCESS_KEY_ID:SECRET"}`, its lowercase SHA-256 the
fingerprint, written to the linked Secrets Store (Workers scope) as
`DATA_R2_PUBLISH_<sha256(instance)[:16]>_<fingerprint[:24]>`. Both paths share one builder
(`data_publisher::credential`), so the JSON, fingerprint and slot can only be computed one way.

**Manual.** Console › Instances › *instance* › Capabilities › Data ingest › **Enable large-file
uploads**. The person pastes an R2 Access Key ID and Secret (masked). Core
(`data_publisher::verify_key`) checks the key against the instance's own private Data bucket
only: a PUT and a DELETE of one scratch object under `ingest/canary/key-check/`, and a
ListBuckets that must show no bucket but the Data bucket (or be refused). It then writes the
JSON to the slot through the link (Secrets Store Write) and answers the fingerprint, never the
key. The console proposes `data.multipart_publisher = <fingerprint>` (a record change, the
person's passkey); the auto-run then plans, and approving that plan binds it.

**Automatic (opt-in).** The record sets `data.multipart_mint: true`. The link must also grant
the optional scope **Account API Tokens Write** (`account-api-tokens.write`, capability
`data multipart mint`; re-link to grant it). The instance plan then shows
`mint a bucket-scoped R2 token for <bucket>` as a step. Only an approved plan mints: there is
no tool that mints. Applying it:

1. reads the account's permission groups (`/accounts/{id}/tokens/permission_groups`) and picks
   exactly `Workers R2 Storage Bucket Item Write` and `… Read` (bucket scope) by name;
2. builds the one policy core will ever mint (`data_publisher::mint_policy`): `allow`, those two
   groups, on exactly `com.cloudflare.edge.r2.bucket.<account>_default_<bucket>`;
   `mint_body` refuses anything else (`allowed_policy`), so no other shape can be sent;
3. creates the account-owned token `pigeon-data-multipart-<instance>-<date>` expiring in 365
   days, derives the S3 keys as Cloudflare documents (Access Key ID = token id; Secret =
   SHA-256 hex of the token value), builds the JSON, fingerprint and slot, and writes the slot;
4. records the token's id (never its value), name, fingerprint, slot and expiry in the
   instance's ledger (`multipart`), retires the previous token for revoke
   (`multipart_retired`), and appends a `minted` line to the ledger's `multipart_audit`
   (the last 50 mints and revokes, each with time, token id and bucket).

The mint step is the plan's last data step, so an upload earlier in the same plan still binds
exactly what was approved; the apply re-checks that the record still asks for minting and
that the bucket is the instance's own.

The token value and the derived secret exist only inside that apply: they are never in a tool
result, record, ledger, approval or log. A minted publisher's fingerprint comes from the
ledger, not the record, so the next plan binds it (`UploadDataWorker` with the publisher) — the
second plan approval. Tokens are re-minted 30 days before expiry, or when the slot is gone.

Risk, stated in the console and here: `account-api-tokens.write` lets the link create API
tokens in the account. Pigeon uses it only for the fixed bucket-scoped shape above, only inside
a plan the person approves, and records every mint and revoke. A person who does not want that
leaves the scope ungranted and uses the manual path.

### Rotation and clean-up

A new credential is a new fingerprint and a new slot, planned like any other. Once a plan binds
the current publisher, the same plan (after the upload step, so nothing is removed if the upload
fails) removes every other `DATA_R2_PUBLISH_<instance-hash>_*` slot and, on the automatic path,
revokes each earlier minted token (`DELETE /accounts/{id}/tokens/{token}`), recording each
revoke in the audit.

### Lifecycle rule

While a publisher applies, the data plan ensures one R2 lifecycle rule on the Data bucket,
`pigeon-abort-incomplete-multipart`: enabled, empty prefix, abort incomplete multipart uploads
after 7 days. Other rules are kept as they are (read, merged, written back).

### Canary

`data_ingest_multipart_check {instance}` asks the Data Worker (signed with its data key) to run
its canary: a generated 40 MiB object (parts of 16, 16 and 8 MiB) through the real multipart
path (D1 checkpoints, R2 binding parts, the signed create-only S3 completion), at
`ingest/canary/multipart/<sha256>`. It checks the SHA-256 and size, that a second create-only
completion on the same key is refused (412), reads the object back and hashes it, then deletes
only that object and its checkpoint row. The result (fingerprint, passed, checks, time) is kept
at `ingest/canary/multipart/result.json`. Readiness reports `multipart_configured` only when the
publisher opens **and** the stored canary passed for the current fingerprint. The backup never
copies `ingest/canary/` objects.

### Picking up skipped files

`data_ingest_plan {…, continue: <run>, only: "too_large"}` reads that run's report and plans a
new run of only the objects it listed as `too_large` (`job.only`, at most 200 keys, the
report's cap), once readiness says multipart is ready. The runner lists the source as usual and
processes only those keys; `skip_over` is not set.

## Phases

| Slice | What | State |
| --- | --- | --- |
| a | Credential builder, manual verify-and-write, console flow, lifecycle step, stale-slot removal, Data Worker canary, readiness gated on it, `data_ingest_multipart_check` | landing (pUsOJDxbNd039cfH) |
| b | `multipart_mint`, optional scope, mint step, ledger and audit, revoke on rotation, console opt-in | building |
| c | `continue` + `only: "too_large"` | planned |

## Log

- 2026-11-28: planned. Scope id confirmed with `linked_scopes`: `account-api-tokens.write`
  ("Account API Tokens Write"). Resource form and S3 derivation from Cloudflare's R2 token docs.

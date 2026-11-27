# Instance rollout live fixtures

This addendum to [Instance entry points](instance-entrypoints.md) records the
live checks and fixture requirements for rollout checks 3A–3E. Source findings
refer to `d1d2ec8`.

## Live evidence

The approved instance inventory contained a linked publishing/source instance
and the home Artifacts/CI instance. Neither enabled Storage, Ingest, or
Observability Metrics. The linked site's approved profile was public.
`storage_accounts` returned no configured stores. `github_status` confirmed
that the GitHub App key and webhook secret were configured and the linked
repository had a recorded push. It reported no latest deploy.

On the linked instance, `storage_list_objects` returned “object storage is
disabled for this instance” and `observability_worker_metrics` returned
“observability metrics are disabled for this instance.” Including a direct
`worker` selector returned “only instance-bound metrics arguments are
accepted.” These checks exercise feature enablement and the metrics argument
boundary. Provider access, data-level isolation, ingest execution, gated
publishing, and webhook redelivery remain unverified. No fixture, profile,
release, or deployment was changed during these checks.

## Fixture and checks

Use an approved linked sandbox with a dedicated repository and installation,
Worker, and site and ingest destination buckets. Use synthetic source objects
and an enrolled test visitor. Keep the fixture credentials in the configured
secret bindings and omit provider identifiers and credentials from test
reports.

| Check | Fixture prerequisites | Expected live result |
|---|---|---|
| 3A Storage | A declared read-only store, one allowlisted fixture bucket, and an approved Storage binding to `inside/`. Place synthetic objects under `inside/` and its sibling `outside/`. | List and read inside the approved prefix. Reject an explicit outside-prefix request and a store or bucket override. Pagination retains the approved scope. |
| 3B Ingest | The same read-only source, an approved binding to its exact prefix and a dedicated R2 destination, the instance's `INGEST_R2_<INSTANCE>` slot, and authorization for the selected runner. | An out-of-scope source or destination fails before a plan is submitted for approval. An approved bounded dry run completes without writing destination blobs. Starting even a dry run provisions compute and writes job metadata. Obtain approval for that execution separately from preparing the fixture. [Ingest](ingest.md) |
| 3C Gated publishing | A passkey-only fixture subdomain, a private or authenticated origin, a dedicated site bucket, and a test visitor enrolled on that host. Prepare two small releases, A and B. | An anonymous request cannot read the protected route or the direct origin. The enrolled visitor sees A, then B, then A after rollback. [Sites](sites.md) |
| 3D GitHub delivery | A dedicated GitHub App installation/repository bound to the approved publishing instance, with a deploy-ref push and consent to redeliver it. | A verified push queues and publishes once. Redelivery after completion leaves the terminal result intact. A distinct subsequent push still queues and publishes. |
| 3E Metrics | An approved binding to the sandbox Worker in the linked account, with Account Analytics Read. | A short-window query succeeds for the fixture Worker. An alternate Worker selector is rejected. |

Storage and Metrics fixture checks can run independently. Ingest can reuse
the source fixture once the runner and destination are authorized. The Sites
and GitHub code prerequisites can proceed independently. If the two live
publishing checks share a fixture, run them sequentially so each observes its
own release changes.

## Gated publishing prerequisites

[Instance record validation](../../src/capabilities/src/instances/record.rs)
allows passkey sites only on platform subdomains.
[Deployment target resolution](../../src/sites/src/deploy.rs) derives the blog
base URL from a domain target. Support the approved platform subdomain as the
canonical URL for that profile. The
[public route](../../src/sites/src/public.rs) checks the visitor before
forwarding, while the generated
[origin Worker](../../src/capabilities/src/instances/site_worker.js) reads the
site bucket without verifying a visitor or an authenticated upstream request.
Use a private binding or authenticate the forwarding route at the origin before
reading protected content. Cover anonymous route and origin denial, authorized
reads, caching of protected responses, and unchanged public reads before using
the fixture.

## GitHub replay and retry

The [webhook and scheduler](../../src/capabilities/src/github.rs) verify the
body signature, resolve the approved repository and installation, and bind
scheduled publication to the approved profile. The Directory now keeps a
verified-body fingerprint independently of the latest push and deploy settings.

- The fingerprint is SHA-256 of the exact HMAC-verified bytes. Changing
  `X-GitHub-Delivery` does not change replay identity. Redelivering A after B
  leaves B in the latest-deploy slot.
- One atomic SQLite statement in the Directory inserts the fingerprint, sets
  latest-push, and writes any pending deploy. This records matched valid pushes
  even when their refs or approved profiles preclude publishing. A duplicate
  returns 202 without modifying push history, queue state, attempts, or timing.
  A failed statement leaves the push eligible for redelivery. Fingerprints
  expire after seven days and the Directory prunes expired entries on delivery.
  The global limit is 10,000 fingerprints. The hook acknowledges retained
  duplicates at the limit and returns 503 for new pushes until entries expire.
- Keep scheduler retries on the existing queue row. A transient publishing
  error returns the claimed job to pending with its attempt count preserved.
  An expired claim is recovered through the lease and compare-and-swap. All
  attempts still recheck the approved binding and policy. A terminal job can be
  requeued only through separately authorized retry or a distinct eligible
  event. Webhook redelivery cannot serve as an administrative retry.
- Test concurrent identical deliveries, delivery ID changes with the same
  verified body, redelivery after completion or after a later distinct push,
  transient scheduler retries, lease recovery, and transaction failure. Queue
  deduplication does not guarantee exactly-once publication if a Worker stops
  after publishing and before recording completion. That case needs a stable
  publication idempotency key or reconciliation at the Sites boundary.

## Storage, Ingest, and Metrics canary runbook

The live `noisypigeon-preview-canary` record is version 1 and has an approved
linked Worker. Its Storage, Ingest, and Metrics profiles are absent.
`storage_accounts` still reports zero declared stores. The canary is linked,
but the latest status did not report a fresh grant or the
`account-analytics.read` scope. These three checks can be tracked separately
from the publishing and CI checks. Re-read the whole instance record before
each proposal so a publishing or CI update is preserved. Use only the canary
record and its resources.

| Check | Owner input before a proposal |
|---|---|
| Storage | Select an owned source provider, account name, region or endpoint, bucket, and allowed prefix. Authorize a read-only key for the synthetic fixture bucket and install it through Access in the declared Storage slot. |
| Ingest | Confirm the same source, a dedicated canary R2 destination bucket, its scoped R2 credential, a runner, and the maximum approved runtime or cost. Confirm authorization for cleanup if the runner needs it. |
| Metrics | Authorize a canary-only linked grant with `account-analytics.read`, and confirm the approved Worker is present in that grant's account. |

For Storage, add an allowlisted account to
[`storage_accounts()`](../../src/platform/src/settings.rs) and bind its
Access slot in the platform deployment. Keep the key value out of the
declaration. Once `storage_accounts` lists the account with its key set,
propose `storage: {state: "enabled", store: "<provider>:<name>", bucket:
"<fixture-bucket>", prefix: "inside/"}`. Seed at least two small synthetic
objects under `inside/` and one under `outside/`. On the canary MCP endpoint:

1. Call `storage_list_objects` with `max_keys: 1` and no prefix. Follow
   the returned continuation with the same listing arguments and confirm the
   returned names are relative to `inside/`.
2. Call `storage_get_object` with a returned relative name and compare its
   content to the seeded fixture.
3. Verify rejection of `key: "../outside/sample.txt"` and any supplied
   `account` or `bucket`. Verify a continuation does not carry over to a
   different requested prefix.

For Ingest, confirm that the dedicated destination has no fixture objects.
Propose `ingest: {state: "enabled", source: "<provider>:<name>", bucket:
"<fixture-bucket>", prefix: "inside/", destination_bucket: "<fixture-r2>"}`.
A scoped `data_ingest_plan` supplies the source, bucket, and prefix from this record.
Confirm an explicit selector is rejected before approval creation. Then call
`data_ingest_plan` with `dry_run: true` and a size estimate based on the fixture.
A ready plan creates an approval request. Inspect its scope and estimate
before approving. Once approved, call `data_ingest_start` with the approved `plan` ID. Read the terminal
result using `data_ingest_report` with the returned `run` ID. Compare the destination
before and after. Run metadata is expected, and destination blobs or quarantine
copies should be absent. Use a separately authorized cleanup path if needed.
The platform `data_ingest_status` tool can delete a finished VM.

For Metrics, the current [OAuth setup](../../src/capabilities/src/linked.rs)
keeps one shared client and scope vector, which omits Account Analytics Read.
Running `linked_setup` again keeps the existing client. Approving the
observability record alone cannot supply the Analytics permission to the
[linked Metrics query](../../src/capabilities/src/observability/instance.rs).
Set up a canary-only grant path before proposing `observability: {state:
"enabled"}`. Once a fresh canary grant includes the exact scope, call
`observability_worker_metrics` with `hours: 1`. Confirm the returned Worker
matches the approved canary record, and confirm an alternate `worker`
argument is rejected. A successful empty window is valid for an idle Worker.

### GitHub fixture run

Use `instances_list` and `github_status` to verify the fixture's approved
repository and App installation before any push. The existing preview canary
shares the public repository and has no App installation. Its latest push and
deploy are empty, as expected. Keep the production installation and repository
routing in place.

With a separately authorized, uniquely bound fixture installation and deploy
ref, collect evidence from GitHub's delivery response, `github_status`, and
the active release on the fixture hostname. Use short fixture commits and
snapshot the push and deploy rows by commit, state, attempts, and timestamps.
Omit provider identifiers and credential material from the report.

1. Push fixture commit A. Expect HTTP 202 with `deploy` reporting queued.
   Poll for A in `latest_deploy` and record its terminal result and active
   release. A healthy first attempt ends in `done` with `attempts: 1`.
2. Redeliver A from the fixture App's delivery history. Expect HTTP 202 and
   `duplicate: true`. Compare both rows against their snapshots and confirm
   the active release stays on A.
3. Push a distinct fixture commit B and wait for B to finish. Redeliver A
   within the fingerprint retention window. Expect HTTP 202 and
   `duplicate: true` again, with B's rows and active release unchanged.

The next checks require an isolated deployment with a controllable provider
stub and a way to pause its scheduled publisher between attempts. The
production publishing path has no failure-injection control.

1. Make the stub reject the first provider operation before any release
   writes. Let the fixture scheduler run one tick and verify the same commit
   is `pending` with one attempt. Let the second tick reach the restored
   provider. Expect exactly two attempts, a `done` row for the same commit,
   and one release activation. GitHub redelivery is unnecessary.
2. For a fresh commit, reproduce the first failure and pause before the retry.
   Apply a separately authorized change to the fixture's approved publishing
   profile and resume. Expect a terminal configuration-changed error, with
   the original attempt count and zero new provider calls or release changes.
3. With the provider stub, pause a running claim until its lease expires.
   The next tick can reclaim it while attempts remain. After the fifth claim
   expires, the next tick marks completion unknown and starts no sixth
   attempt. Capture the provider call count and scheduler row separately.

The native smoke suite exercises replay and atomicity with SQLite and runs
the scheduler's retry, lease, and grant checks with synthetic outcomes. Use
those checks until the corresponding isolated fixtures are available.

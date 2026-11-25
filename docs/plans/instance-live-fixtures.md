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
scheduled publication to the approved profile. Every accepted eligible delivery
currently replaces the pending deploy record. That lets redelivery reset a
running or completed publication.

- Derive the delivery fingerprint from the exact HMAC-verified body. Use
  `X-GitHub-Delivery` only for diagnostics. Keep a fingerprint record independent
  of the latest-deploy slot so that redelivering A after B cannot replace B.
- Use one Directory transaction for first-seen insertion, latest-push record,
  and any pending deploy row. Store first-seen records for matched valid pushes
  even when the ref or approved profile does not allow publishing. A duplicate
  returns a successful acknowledgement without modifying push history, queue
  state, attempts, or timing. A failed transaction leaves the push eligible for
  redelivery. Choose and document the retention window and storage bound before
  enabling the live replay check. Replay protection is limited to that window.
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

# Signed instance context across Worker bindings

Status: the context format, the contextual senders for instance MCP calls and queued publishing,
and the Sites and CI callbacks are on main (checked at `a4bb475`, 2026-11-25). What is done per
operation is in [legacy-worker-cutover.md](legacy-worker-cutover.md); which compatibility paths
remain, with live evidence, is in [legacy-worker-retirement-status.md](legacy-worker-retirement-status.md).

## The signed envelope: expiry and nonce

The envelope (`Signed` in `src/contract/src/caller.rs`) carries the caller, `iat`, `exp`, a
`nonce` and, in the contextual format, the `WorkerContext` (recipient and optional instance).
The signature covers it with the operation and the body digest. The receiver refuses a signature
older than `MAX_AGE_MS` (60 s) or dated more than 5 s ahead.

The nonce is a fresh random value per signature (`pigeon_core::webauthn::random`), so two
signatures over the same caller, operation and body differ. It is **not a replay defence**: no
receiver keeps seen nonces, so the same header and body are accepted again until `exp`. That is
accepted because these calls travel only over private service bindings. Core answers capability
Workers only on the host `split::CORE_HOST` (`core`), which a request reaches over a `PLATFORM`
binding and never from the internet (`src/platform/src/lib.rs`, `fetch`). Bounding what a
replayed or forged call can do is the job of per-Worker keys and per-Worker operations
([caller-keys.md](caller-keys.md)).

## Rollout

1. Deploy the receiver support first. Its manifest advertises the context
   version it accepts. Legacy platform requests continue to work. The
   contextual format signs the recipient Worker and optional approved
   instance together with the caller, operation, and body.
2. Deploy the core sender after contextual receivers are available. For
   `/mcp?instance=<id>`, the handler resolves the approved record and
   carries its scope alongside the caller through the shared dispatcher.
   Nested `call` uses the same scope. The remote adapter checks the
   receiver manifest and issues exactly one signed request for that
   Worker and instance. Unsupported receivers are refused before fetch.
   A rejection or transport error never triggers a legacy retry.
3. Deploy the queued publishing receiver before switching its sender.
   Sites accepts the private `site_deploy_queued` call only with matching
   signed instance context, a standard caller, an exact commit, and the
   approved publishing record digest. The existing receiver checks the
   digest before it obtains credentials and rejects an archive whose
   commit differs from the queued one. Existing manual publishing and
   rollback stay on their established routes. The compatibility receiver
   that accepted the old scheduled call on public `site_deploy` is retired
   by `claude/retire-public-publishing-binding`. Public `site_deploy` now
   refuses `publishing_binding`. See the
   [retirement status](legacy-worker-retirement-status.md#pausing-queued-publishing-for-a-rollback)
   for pausing queued publishing during a core rollback.
4. After that receiver deploys, switch the GitHub scheduler to the
   contextual sender. The scheduler rechecks the queued binding, the
   linked account and its owner, and the caller's publishing policy
   before signing the request. Legacy account links identify the person
   by name, so the scheduler requires a unique standard-user match.
   A receiver without context support, an
   older receiver that has no queued handler, or a contextual rejection
   uses the existing bounded queue failure/retry behavior. No request
   retries with a legacy envelope.
5. Once every entry point for a tool carries the scope, call
   `Signed::require_instance` in that receiving tool before resolving its
   resources. Keep deliberate global operations on their own request path.
   This is the point at which the receiver can refuse legacy requests for
   that tool without interrupting older platform callers.

Core's internal service endpoint verifies the recipient on contextual
callbacks and reconstructs the scope from the currently approved record.
A scoped generic dispatch uses the shared gate. A scoped CI lookup must
name the same instance. Other core callback operations keep their current
protocol until they have an operation-specific bound scope.

## Legacy call inventory and migration decisions

This inventory records the sender cutover baseline at `d1d2ec8`. Several rows have moved since (for example `remote::call_owned` is gone, replaced by `remote::call_owned_instance` in `745c2bf`); the current status per operation is in [legacy-worker-cutover.md](legacy-worker-cutover.md). “Legacy signed”
means the operation, body, and caller are signed, with no separately signed
recipient or instance. It does not imply a missing caller check.

### Calls that carry an instance

| Path | Authority and target at the baseline | Migration |
| --- | --- | --- |
| [GitHub cron → Sites `site_deploy`](../../src/capabilities/src/github.rs) via [`remote::call_owned`](../../src/capabilities/src/remote.rs) | Legacy signed as the approved publishing delegate for the linked account. Only an authenticated push matching the approved repository and installation queues a publish. Before dispatch, cron rechecks the pending record binding and evaluates tool policy without a passkey. The call body carries the target instance, commit, and approved binding digest. Sites compares that digest with its current record. | Once the Sites manifest advertises support, sign the recipient and the same instance resolved from the approved queued record. Keep the saved binding and policy checks. A compatibility path may cover an older Worker before any scoped request is sent. A contextual error must never start a legacy retry. |
| [Sites → core during `site_deploy` or `site_rollback`](../../src/sites/src/deploy.rs) | **Retired** (`claude/sites-require-instance-context`). Sites accepts these calls only with signed instance context matching `instance` ([`context.rs`](../../src/sites/src/context.rs)), and asks core only through the typed `sites_scope` callback (Record, Linked, Github), signed for core and that instance. The legacy `instances_list` lookup and `credential` `linked`/`github` requests are deleted, from Sites and from core. | Done. Core binds each callback to the same approved publishing record and allows only the corresponding credentials. |
| [CI gateway → core `ci_scope` read](../../src/ci/src/scope.rs) | Legacy signed as the caller that requested the CI read. The body names the instance. Core resolves the approved CI profile and the gateway checks persisted job ownership, repository, and job type. | Deploy core support for contextual reads first. The gateway uses the incoming verified signed instance and signs the callback for core. The body instance must match. Keep deliberately legacy connector reads during rollout. A contextual read never falls back to legacy. |
| [Core → a capability Worker for instance MCP tools](../../src/capabilities/src/remote.rs) | Already contextual at this baseline. Core derives scope from the approved record, gates the tool, negotiates the receiver's advertised version, and signs its Worker name and the bound instance. The receiving contract checks the recipient and body instance. | Require signed instance on a receiving tool only once all supported entries to it can provide that context, including internal workflow and global connector entries for an instance target. |

### Platform-wide and protocol-specific calls

| Path | Authority and target | Migration |
| --- | --- | --- |
| [Core → Worker `manifest`, `fetch`, and `events`](../../src/capabilities/src/remote.rs) | These use service bindings without a signed application caller. A manifest describes capabilities. A forwarded HTTP route keeps its request headers and the destination route establishes authority. The queue sends an `events` wake-up with an empty messages list to any remote capability whose manifest matches a batch event. | Keep discovery and public route forwarding separate from instance tool transport. A future instance event needs a persisted and rechecked target and producer identity. The empty wake-up cannot by itself authorize a scoped mutation. |
| [Core queue: Artifacts mirror and changelog](../../src/platform/src/lib.rs) | No external tool caller. The configured queue triggers [`mirror`](../../src/capabilities/src/mirror.rs) or [`changelog`](../../src/capabilities/src/changelog.rs), which select the platform repository and their own targets from configuration. | Keep platform-owned. An instance version needs its own approved repository and destination binding before accepting per-instance queue data. |
| [Core cron: ledger sweep](../../src/platform/src/lib.rs) | Platform maintenance invokes the token ledger through its Durable Object binding. No user or instance is selected. | Keep platform-owned. |
| [CI gateway → core ownership attribution and credentials](../../src/ci/src/scope.rs) | `ci_scope` with a repository and job is a legacy platform submission lookup. Unattributed jobs stay in the platform view. [Purpose-scoped credentials](../../src/ci/src/lib.rs) are signed for the current caller, or for the CI Worker when working autonomously, and carry no instance. | Keep attribution and purpose-scoped credentials on their existing platform paths. The contextual read contract must reject attribution requests. Job callbacks and scheduling stay on their own job-token, webhook, or Durable Object paths. |
| [Sites → core public site routes](../../src/sites/src/public.rs) and [apex](../../src/sites/src/apex.rs) | Legacy signed as the Sites Worker itself. The request is for a public architecture view, a hosting map, or a passkey operation. Site routes check the host and their own visitor session and challenge. | Keep platform metadata reads and public sign-in callbacks as separate operations. If a passkey operation is migrated, derive the scope from the canonical verified hostname and the approved site record, never from arbitrary route arguments. |
| [AI Agents, model gateway, and cloud gateway → core callbacks](../../src/ai-agents/src/object.rs) | Legacy signed as the persisted run or tool caller, or the named Worker when its operation allows autonomous work. AI Agents use delegated dispatch, run grants, token release, and configured service users. [Models](../../src/models/src/lib.rs) request purpose-scoped credentials. [Cloud](../../src/cloud/src/link.rs) requests those credentials and caller-bound [approvals](../../src/cloud/src/approvals.rs). | Keep existing platform contracts. A run ID in the grant request's `instance` field identifies the run, which has its own authority. Do not reinterpret that field as a profile ID. Add scoped callbacks only when the corresponding capability gains an approved instance binding. |
| [Proxy → core credential checks](../../src/proxy/src/lib.rs) | Legacy signed with a null caller for `verify`, `grant`, and `deploy_grant`. Core checks the credential presented in the body. A purpose-scoped credential request instead uses the proxy's service identity. | Preserve credential-based authority. No profile should be inferred from a job, repository, or proxy request alone. |
| [Console sections and secret actions → bound Workers](../../src/capabilities/src/remote.rs) | Legacy signed with the console person or current approved caller. These are platform administration requests. | Keep platform-wide paths. Give any future instance-specific section or secret action its own operation contract before enabling contextual dispatch. |
| [Lander alarm → core dispatch and CI](../../src/capabilities/src/land.rs) | In-process loopback recreates a core context from the stored landing caller and invokes the usual dispatcher. A selected remote tool uses its Worker binding. Polling uses the known scheduler job ID. There is no separate HTTP self-call to sign. | Keep the current platform landing lane. If a future instance landing can enter this loopback, persist the approved target and restore it before dispatching. Keep Durable Object delivery distinct from signed application requests. |
| [Core → Sites apex probe](../../src/capabilities/src/site.rs) | A direct binding fetch checks public pages before moving a hostname. The configured Worker binding selects the target and the probe is read-only. | Keep as an HTTP health/readiness check. It confers no tool or instance authority. |

The generated site Worker and external ingest data Worker are separate deployment surfaces.
Their HTTP, queue, and storage trust boundaries remain in the
[instance entry-point audit](instance-entrypoints.md). External OAuth redirects, including
localhost loopback redirects, are browser callbacks authenticated by the OAuth flow and
are outside the Worker binding envelope.

### Cutover order

1. Negotiate the signed recipient and instance for scheduled publishing without relaxing its
   event binding or policy checks. Exercise the scoped and old-manifest cases before enforcing
   new requirements in Sites.
2. Introduce operation-specific Sites callbacks. Deploy the core receivers before Sites sends
   contextual record and credential requests. Exercise publish and rollback with the current
   record and with a changed approved binding.
3. Retain legacy global connector entry points until their instance-targeted tools use a bound
   adapter. Then require matching signed context on the receiving instance operations. Keep
   platform-only submissions, operator tooling, and protocol-specific callbacks available.
Once all scheduler deployments are on the contextual sender, remove the
old scheduled-only publishing binding argument from the general deploy
call. Manual deploys and rollbacks retain their explicit platform path.

## Queued publishing attempt limit

A provider error returns the same queue row to pending until the attempt limit.
A changed approved publishing record fails the queued request before a new
claim. An expired lease on the final attempt records a terminal failure with
unknown completion and never starts another publish. The scheduler cannot
infer whether Sites finished before the lease expired.

## Verification

- An older receiver still handles a legacy platform request.
- The scoped sender refuses a receiver that cannot advertise its context
  version and does not resend after a contextual rejection.
- A contextual request addressed to another Worker is refused.
- The approved instance remains bound through direct and nested dispatch.
  A scoped lookup cannot substitute another instance.
- The receiving tool still validates the approved instance's resource
  bindings. A signed instance identifies the target, while the approved
  record supplies what it may affect.

## Live canaries before sender cutover

- The approved `noisypigeon` Source profile resolved its existing live content
  commit and read `.pigeon/site.toml` through the Pigeon Access connector.
  Overriding the repository was denied, and profile discovery listed only
  the two Source tools and the existing publishing tools and wrapper.
- The approved platform CI profile lists the newly attributed container
  test job. Scoped status returns that job and denies a pre-profile job ID.
- These connector calls verify instance-bound tool behavior. They use the
  global Connector endpoint, so they do not attest to contextual MCP
  transport after cutover. Exercise an authenticated instance endpoint
  separately for the signed-sender canary.
- Storage/ingest require a declared fixture backend; the visitor-access
  canary needs an isolated gated hostname and a protected origin.

## Scoped Sites publishing callbacks

Core accepts the typed `sites_scope` callback for an authenticated caller
with instance context addressed to core. The callback instance must match
the signed instance. A record lookup returns the approved publishing
record. Credential callbacks present the digest of that exact response.
Core validates the active publishing target and rechecks the record after
awaiting the provider.

GitHub credentials are limited to reading contents of the approved
repository. A repository without an approved App installation can use
the existing public archive path. Cloudflare linked OAuth remains
account-scoped at the provider. Core checks the current link against the
approved account before using its credentials. These callbacks inherit
the existing trusted Worker caller-key boundary. Dedicated sender
authentication would require independent keys and rotation.

The legacy publishing callbacks are removed (`claude/sites-require-instance-context`), after
live scoped publishes and rollbacks on `sandbox` through the global connector (2026-11-25):
`site_deploy` succeeded under `Unsupported::Refuse`, `site_rollback` succeeded, a rollback to
another instance's release was refused by Sites, an unknown instance and an extra `bucket`
argument were refused in core, and pigeon-sites logged 0 errors in 205 requests over the hour.
Sites' `site_deploy` and `site_rollback` now require signed instance context matching
`instance` and refuse a legacy envelope before any callback; core's `credential` kinds
`linked` and `github`, and Sites' `dispatch` `instances_list`, are gone from
`split::WORKER_OPS`.

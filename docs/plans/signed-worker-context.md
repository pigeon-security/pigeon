# Signed instance context across Worker bindings

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
3. Keep the platform-wide route and scheduled publishing on their current
   transport during this migration. Scheduled publishing still checks its
   queued binding and policy before dispatch, and Sites still checks the
   approved publishing target. Convert scheduled publishing separately
   by deriving the signed scope from the validated instance record and
   negotiating support before sending.
4. Once every entry point for a tool carries the scope, call
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

This inventory records the sender cutover baseline at `d1d2ec8`. “Legacy signed”
means the operation, body, and caller are signed, with no separately signed
recipient or instance. It does not imply a missing caller check.

### Calls that carry an instance

| Path | Authority and target at the baseline | Migration |
| --- | --- | --- |
| [GitHub cron → Sites `site_deploy`](../../src/capabilities/src/github.rs) via [`remote::call_owned`](../../src/capabilities/src/remote.rs) | Legacy signed as the approved publishing delegate for the linked account. Only an authenticated push matching the approved repository and installation queues a publish. Before dispatch, cron rechecks the pending record binding and evaluates tool policy without a passkey. The call body carries the target instance, commit, and approved binding digest. Sites compares that digest with its current record. | Once the Sites manifest advertises support, sign the recipient and the same instance resolved from the approved queued record. Keep the saved binding and policy checks. A compatibility path may cover an older Worker before any scoped request is sent. A contextual error must never start a legacy retry. |
| [Sites → core during `site_deploy` or `site_rollback`](../../src/sites/src/deploy.rs) | The incoming `Signed` caller is forwarded in a legacy signed envelope, even if the incoming call had context. Core dispatch returns approved records via `instances_list`. Sites selects the requested record, then requests a GitHub installation credential or linked account credential by instance as applicable. Sites checks the account against the publishing target and rechecks the record before activation. | Separate record lookup and credential requests by operation. For an inbound contextual call, callbacks should use its verified instance and sign core as the recipient. Core must bind each callback to the same approved publishing record and allow only the corresponding credentials. Keep the explicit global platform path until it uses the corresponding scoped adapter. |
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

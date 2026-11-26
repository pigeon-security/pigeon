# Instance entry points and shared service boundaries

Baseline audited in the repository at `d75cf0c`. This records what the code enforces, where later phases need a check, and which operations intentionally keep platform scope. It does not attest to deployed configuration or production behavior.

**Superseded since the baseline (checked at `a4bb475`, 2026-11-25).** The tables below still describe `d75cf0c`. Since then:

- Phase 4B's capability-owned registration is done. Each capability declares its instance tools, and one `Scope` binder serves discovery and execution (`CAPABILITIES` in [instances/tools.rs](../../src/capabilities/src/instances/tools.rs)). Besides Publishing, Storage and Source, CI, Metrics (`observability::instance`) and Ingest are now scoped there.
- Queued publishing keeps its provenance. A deploy-ref push records the commit, repository, installation and the approved publishing binding in the pending row ([github.rs](../../src/capabilities/src/github.rs), the webhook handler), and the cron rechecks the binding before it signs `site_deploy_queued` (item 2 of the implementation order).
- The Worker contract has a typed, signed instance context: `Signed.context`, a `WorkerContext` naming the recipient and optional instance ([caller.rs](../../src/contract/src/caller.rs)). See [signed-worker-context.md](signed-worker-context.md) and, per operation, [legacy-worker-cutover.md](legacy-worker-cutover.md) (item 3).
- Core callbacks are checked with the shared `CALLER_KEY` only. `split::verify_caller` would try a Worker's own key first, but no Worker sends `X-Pigeon-Worker`, no `src/*/wrangler.toml` binds a `CALLER_KEY_*` secret, and all seven Workers bind the shared `CALLER_KEY` ([caller-keys.md](caller-keys.md)).

## Shared services

| Service | Source of authority | What a capability should consume |
|---|---|---|
| Caller identity and delegation | An unexpired OAuth access token resolves through the Directory, or a Cloudflare Access assertion is verified for the expected issuer and audience and resolved to a service user. The signed caller carries the resolved person or delegate. [OAuth](../../src/capabilities/src/oauth.rs) · [Access caller](../../src/capabilities/src/users.rs) | A verified caller and acting-for context. Keep authentication and passkey handling in `oauth`, `users`, and `passkeys`. |
| Enablement and resource ownership | An approved instance record is the source of feature enablement and account, repository, bucket, and site bindings. [Record validation](../../src/capabilities/src/instances/record.rs) | A feature-specific view of the approved record. Resolve it again at the point of use when work crosses a queue or changes state. |
| Policy and approval | Shared dispatch finds the tool owner and applies `call_tool` policy. A gated approval is bound to caller and exact arguments, and is spent once. The baseline rules are built into the platform. [Dispatch](../../src/capabilities/src/lib.rs) · [Gate](../../src/capabilities/src/approvals.rs) · [Policy](../../.pigeon/policy.toml) | A policy decision for the resolved caller and bound tool. Authorization to call a tool and authorization to use a particular instance resource are independent checks. |
| Credentials | The platform holds its parent token and relays linked-account grants or leases scoped credentials over its service bindings. Named permission groups are resolved by core. GitHub installation tokens are narrowed to the recorded repository. [Credential issuance](../../src/capabilities/src/split.rs) · [Linked account](../../src/capabilities/src/linked.rs) · [GitHub installation](../../src/capabilities/src/github.rs) | A handle for the approved account and workload. Compare a resolved linked account with the approved account ID before provider I/O. Keep token values outside tool responses, except the existing explicitly gated credential tools. |
| Worker transport | The private service binding carries a caller envelope authenticated with a short-lived HMAC over the operation and exact body. Section, secret, and tool call operations validate the envelope. [Signer](../../src/contract/src/caller.rs) · [Receiver](../../src/contract/src/serve.rs) | A verified envelope, then a capability-local check against the bound instance and current record. Transport authentication covers the signed data. Its signature does not by itself authorize a resource. |

## Entry-point audit

| Entry point | Verified identity and gate | Resource scope and remaining work |
|---|---|---|
| Platform MCP, `POST /mcp` | The platform checks OAuth first when a bearer header is present. Without it, it resolves the Access caller. Invalid or absent credentials fail before tool selection. Shared dispatch applies the policy gate. [Platform request handler](../../src/platform/src/lib.rs) | With no `instance` query value, this is the existing platform-wide endpoint. A tool that also accepts an instance is still responsible for checking the requested instance and its approved bindings. |
| Instance MCP, `POST /mcp?instance=…` | Same authentication and policy. Exactly one instance selector is accepted. The platform uses the approved record to advertise the enabled tools and binds the instance before dispatch. Direct calls and generic `call` use the scope binder. [Registry and binding](../../src/capabilities/src/instances/tools.rs) · [Generic dispatch](../../src/capabilities/src/dispatch.rs) | Publishing, Storage and Source use the scoped registry at the baseline. The selector scopes tools and bindings. It does not establish a separate instance membership model. Caller policy still applies to every dispatch. Move descriptors into capabilities in Phase 4B while preserving one scope binder for both discovery and execution. |
| Bound Worker tool calls and core callbacks | Core signs a typed `CallRequest` body containing the capability, tool, and arguments together with its caller. The capability contract verifies operation, body hash, signature, and expiry. Core callback operations check a signature too, with the shared `CALLER_KEY` (per-Worker keys are not active; see the note above). Proxy checks can carry a null caller where the operation permits it. [Remote adapter](../../src/capabilities/src/remote.rs) · [Core callback receiver](../../src/capabilities/src/split.rs) · [Wire types](../../src/contract/src/wire.rs) | Baseline signing has no separately typed audience or instance field. A nonce is signed, but the generic verifier has no replay store. The instance target is carried in the signed tool arguments where used. A callback to core's shared dispatcher gets a policy check; a call sent directly by an internal workflow to a capability Worker does not revisit that gate. For instance calls, preserve the target across the binding and recheck the approved resources at the receiver. Keep broad credential callbacks private until their operation-specific scope is explicit. |
| Bound Worker manifests, forwarded routes, and event callbacks | The contract uses the private service binding for `manifest`, `fetch`, and `events`; these have no signed application caller. A forwarded HTTP request keeps its original headers and is authenticated by its destination route. [Contract routing](../../src/contract/src/serve.rs) · [Remote forwarding](../../src/capabilities/src/remote.rs) | A manifest is inventory. A forwarded route or event must establish its own source and target, with explicit rules for public routes and callback credentials. Avoid treating a successful internal transport hop as permission to mutate instance state. |
| GitHub push and scheduled publishing | Webhook body HMAC is verified before parsing the event. Repository and installation must match the approved instance. Only a nondeleted push to the deploy ref creates pending work. Scheduled publishing resolves the human who linked the account and calls Sites over a signed binding. [Webhook and cron](../../src/capabilities/src/github.rs) | At the baseline the pending record keeps the commit and timing, with no persisted repository, installation, or approved record digest. Sites resolves and rechecks the current publishing record and linked account before writing. Phase 6C should also bind the original event provenance and refuse stale pending work before any credential or deployment request. The cron's direct Worker call has no shared tool gate. Define a noninteractive policy decision before sending it and defer work when approval is required. |
| Other scheduled and queue work | The scheduled handler runs credential cleanup and pending publishing. Queue delivery is trusted as configured by the platform, and each interested capability handles the event. A queue dispatch has no external tool caller. [Worker handlers](../../src/platform/src/lib.rs) | Keep maintenance such as the ledger sweep explicitly platform-owned. Persist identity, target, and configuration needed for instance jobs, then recheck those facts and feature enablement at execution. Keep restart and retry operations idempotent. |
| CI hooks and runners | Buildkite delivery uses the connector's webhook verifier. Its token flow validates provider OIDC claims. Runner operations use job credentials with stored grants rather than MCP caller context. [CI routes](../../src/ci/src/lib.rs) · [Buildkite verification](../../src/ci/src/buildkite.rs) | Runner grants identify a job. They do not grant access to the instance's MCP job inventory. Phase 4A must derive job ownership from trusted stored metadata and check known IDs as well as lists. Legacy jobs without sufficient owner metadata stay on the main-platform path. |
| Git proxy, `/git/` | Exact proxy host and configured source repo. Ordinary reads and writes use a verified proxy credential. Run grants constrain pushes to their ref family. CI job credentials allow fetch of the job's approved repository and commit, with no push. Pushes are checked against branch policy. [Git request handling](../../src/proxies/src/git_proxy.rs) · [Grants](../../src/capabilities/src/proxy.rs) | The proxy's configured platform repository is independent of instance MCP Source. Keep it that way unless a future instance proxy gets a bound repository and compatible grant. Retain tests for repository routing, job expiry and commit binding, and run ref policy. |
| Cloudflare API proxy, `/cf/` | Exact proxy host. Verified general proxy credential or landing deployment job grant. Run-only Git grants are refused. Request method, path, query, and applicable body checks precede forwarding. [Deploy request handling](../../src/proxies/src/cf_proxy.rs) | The platform account and deployed Worker inventory bound operations. Job grants name one script. Queue and namespace operations use the configured resources of the deployment. Keep this route distinct from arbitrary linked-account API access and keep regressions for account and Worker confinement. |
| Host provisioning and public site routes | Setup tools use platform MCP policy. `setup_platform` is passkey-gated by the baseline policy; `setup_proxy_host` permits the two configured Workers under the normal tool policy. Public-host setup derives zone and hostname from capability configuration. Public-host setup only attaches the host; it retires no Access setup. [Settings](../../src/platform/src/settings.rs) · [Public host](../../src/capabilities/src/public_host.rs) | These are platform setup paths. Instance hosts and target Workers come from approved records and plans. Public visitor fetch and per-host passkey sessions are separate from administrative MCP tools. Phase 6B should verify enrollment and session use cannot move across approved hosts. |
| Portal redirect configuration | The tool may list URIs or prepare a change. An add or remove is tied to a particular portal app, host, action, URI, and before/after values. Apply re-reads the app and consumes the approved digest for the same caller before writing, then reads back the result. [Portal plans and apply](../../src/capabilities/src/portal_oauth.rs) | Platform administration only. Keep the Portal app selection and approval digest unchanged during instance registry migration. A portal redirect approval does not enable any instance capability. |

## Implementation order and regression gates

1. Complete Phase 4B's capability-owned registration. Keep discovery and direct or nested invocation on the same scope binder. Keep receiving tool handlers responsible for resource binding.
2. Complete Phase 6C's event provenance checks. Record the approved repository, installation, account, and publishing configuration identity when work is queued. Re-read those before acting. Put the dispatch policy decision at the cron-to-Sites boundary so it applies before credentials are requested and handles an approval requirement without impersonating an interactive actor.
3. Add a typed bound-instance context to the Worker contract when a second scoped remote capability needs it. Both the sender and receiver must use it. Preserve the existing explicit platform-wide route and roll out receivers before requiring the new context from all callers.
4. Review any new routes that use the binding-only contract. Ask what authenticates the source, what target it can select, and where enablement is checked. Keep shared infrastructure defaults and instance permissions explicit in the architecture inventory.

| Regression exercised | Expected result |
|---|---|
| Scoped discovery and direct and nested call for the same enabled or disabled tool | All agree. An unauthenticated request or an unapproved target is refused before any provider operation. |
| Bound Worker call with missing, expired, or changed signed context | It is refused before execution. A valid envelope naming a disabled or mismatched instance is separately refused by the receiver. |
| Queued push with a changed installation, repository, account, or profile | Pending work is left inactive or recorded as stale. No GitHub credential or publishing call occurs. The ordinary approved push still deploys. |
| Public visitor session for a different host | That host denies the session. An approved visitor and a public site continue to work under their existing routes. |
| CI runner or deploy grant used outside its recorded job scope | The request is refused. Existing approved fetch and deployment paths still pass. |
| Portal app changes between approval and apply | The stale plan is refused before mutation. The unchanged application can process the exact approved change once. |

These checks describe the source-level rollout gates. Validate live service bindings, proxy paths, linked-account permissions, and Portal configuration separately when a rollout includes a change to them.

## Signed Worker-context rollout

The receiver accepts the original envelope and a domain-separated Worker-context
envelope. The new envelope authenticates the deployed recipient and the optional
bound instance along with the caller, operation and exact body. A contextual
tool call's instance must match its arguments. The capability still checks
the current approved record before accessing provider resources.

Deploy receivers before switching senders. Receivers advertise support through
the manifest. Legacy platform-wide and maintenance operations retain their
explicit path. An older or wrong recipient refuses contextual calls, and
contextual calls are never retried with a legacy envelope.

### Scoped MCP CI verification

Use a separate remote MCP connection in the approved ChatGPT workspace. In the
custom connector settings, add a clearly named connection such as **Pigeon
Platform · scoped CI** with the exact server URL
`https://platform.pigeon.dev/mcp?instance=pigeon-security%2Fplatform`. Complete
Pigeon's OAuth consent with the owner's passkey, then attach that connection to
a verification session. Leave the existing Pigeon Access connection in place.
If the custom connector is unavailable, drops the query, or is denied, record
that blocker. Adding a scoped Cloudflare Portal upstream requires its own
approval. An explicit Cloudflare 1010 denial ends that client attempt.

1. Check `tools/list` on the scoped connection. It should expose the approved
   instance tools, including `ci_jobs` and `ci_status` for a profile with CI
   enabled. Confirm a platform-only tool such as `ci_submit` is absent.
2. Run `ci_jobs` with `{"limit":10}`. Run `ci_status` with
   `{"id":"<known owned canary job id>"}` using an existing read-only canary.
   The query string binds the instance, so omit `instance` from both bodies.
   Compare the returned job ID and state with the approved fixture.
3. Run `ci_status` with a synthetic unknown job ID. It must be unavailable.
   Run `ci_jobs` with an extra `repo` argument. It must reject the override.
   If the client rejects the argument locally, record that separately from a
   server-side rejection.
4. Record the deployment revision, time, scoped discovery result, and outcomes
   without recording credentials. A successful instance argument through the
   global Pigeon Access connector verifies that ingress only. Before removing
   legacy CI callback support, also confirm contextual handling using the
   receiver's deployment or trace evidence.

### Scoped CI tool annotations

Instance-scoped MCP discovery marks CI job list and status as read-only
using the MCP `annotations.readOnlyHint` field. This advertises the semantics
of those two operations to clients. The generic dispatcher carries no such
hint and tool execution still uses the approved instance and caller policy.

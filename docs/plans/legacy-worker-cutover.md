# Legacy Worker cutover checklist

This addendum turns the [Worker call inventory](signed-worker-context.md#legacy-call-inventory-and-migration-decisions)
into removal gates. The rows were first written against main at `6b1d5ad`; the status column was
rechecked against main at `a4bb475` (2026-11-25). A code change or a passing local test alone does
not establish that its sender and receiver are live. Which compatibility paths may be removed,
and the live evidence recorded so far, are in
[legacy-worker-retirement-status.md](legacy-worker-retirement-status.md); this file keeps the
gates.

The removal unit is **one instance-bound operation**, including all ingress and callback paths
that reach it. Keep existing platform-wide operations on their explicitly allowed routes.

## Instance-bound operations

| Operation | Status on main at `a4bb475` | Evidence required before refusing legacy |
| --- | --- | --- |
| Core → Sites `site_deploy_queued` | **Done** (`745c2bf`). The GitHub cron sends through `remote::call_owned_instance` (`src/capabilities/src/github.rs`, `run_publish`), which reads the Sites manifest and signs the recipient and the approved instance; it has no legacy transport. The old cron sender is removed. Sites requires the matching signed instance on its private receipt. | Confirm the active receiver accepts the context version, the private tool is absent from MCP discovery, and a real fixture push reaches one terminal publication. Recheck old-manifest refusal before fetch, no retry after contextual rejection, stale approval refusal, and the existing lease and retry behavior. Keep an explicit queue-pause procedure for rolling back the sender. The live results so far are in the retirement status. |
| Core → Sites `site_deploy` | **Partly done.** The instance endpoint (`/mcp?instance=…`) sends context. A global connector call that names an instance in its arguments is still sent with the legacy envelope: `Remote::call` derives an instance for global calls only for CI (`src/capabilities/src/remote.rs`, `call`). Remaining: resolve the approved record and use the bound sender for that global instance target. | A scoped endpoint call and a global connector call reach the same safe fixture via the new envelope. Direct and nested dispatch use the same scope and policy checks. An unavailable or changed profile is refused before credentials are requested. |
| Core → Sites `site_rollback` | **Remaining**, as `site_deploy`: the global connector's instance-targeted call is not yet signed with instance context. Preserve rollback's approved profile and release checks when it moves. | On an authorized test site, roll back to a known immutable release and verify that the serving pointer changes to that release. Verify refusal when the release or target lies outside the bound scope. |
| Sites → core publishing record lookup | **Partly done** (`9b7075a`, `8cb5fd5`). When the inbound call has signed instance context, Sites asks core's typed `sites_scope` `Record` callback (`scoped_core` in `src/sites/src/deploy.rs`; core's receiver in `src/capabilities/src/split.rs` and `src/capabilities/src/sites_scope.rs`), and core requires the callback's instance to match the signed one. Without context (global or manual calls) Sites still calls legacy `dispatch` `instances_list` and picks the record (`approved_record` in `deploy.rs`). | Contextual publish and rollback retrieve exactly their approved profile. A changed record still stops release activation. Keep global `instances_list` for its own authorized consumers. |
| Sites → core publishing credentials | **Partly done** (same commits). With context, Sites asks `sites_scope` `Linked` and `Github`, each carrying the digest of the approved record it read. Without context the legacy `credential` branches remain: `linked` in `linked_account` and `github` in the deploy path of `deploy.rs`. | Verify linked-account and GitHub installation selection separately. Publishing must stop safely if the account or installation binding changed or a required callback fails. Preserve the documented public codeload behavior where applicable, with no legacy transport retry. |
| Core → CI gateway `ci_status` and `ci_jobs` | **Done in code** (`3d3da94`). The scoped endpoint sends context. A global connector call that names an approved instance is sent with context when the CI gateway's manifest advertises it (`ci::approved_connector_instance`, used by `Remote::call` in `remote.rs`). Reads with no instance stay on the platform path. A gateway that does not advertise context still gets the legacy envelope. | On an approved fixture with an attributed job, status and listing both succeed through the scoped endpoint and global connector. Another instance's job and unattributed jobs stay outside the scoped view. Platform status and listing still work without an instance. |
| CI gateway → core `ci_scope` read | **Done in code; receiver not tightened.** The gateway signs the callback with context when the inbound call carried it (`src/ci/src/scope.rs`), and core checks the instance (`verify_ci_scope` in `split.rs`). Core still accepts legacy reads. | Verify the contextual callback from a real scoped CI read and from a global connector read. Require matching signed context on reads only after that evidence. Keep the separate platform attribution request. |
| Core → any later instance-bound remote tool | Unchanged: the generic contextual sender verifies negotiated support and binds the recipient and approved instance. A Worker can host both scoped and platform operations. | Enumerate the specific tool and every caller before changing its receiver. Verify through the public entry point and Worker binding used in production, with the same caller and scope checks as direct dispatch. |

The private queued receipt and the CI callback check are already operation-specific. Avoid adding a
blanket version requirement at the shared `call` or core receiver while any deliberately global
operation still uses that receiver.

## Deliberate platform operations

| Keep available | Existing authority and boundary |
| --- | --- |
| Core's global MCP route, generic dispatch, and Worker `call` for platform-only tools | Existing caller authentication and policy gate. An operation that explicitly targets an instance should migrate to the contextual sender before its receiver is tightened. |
| CI submissions, platform status and listing, `ci_scope` attribution by repo and job, and CI purpose-scoped credentials | Platform caller or CI service authority as applicable. Attribution is not a contextual CI read. Jobs without verified ownership remain platform-only. |
| GitHub push intake, platform ledger sweep, Artifacts mirror and changelog queues | Existing webhook, queue, and platform maintenance authority. Only follow the owned queued publishing record into the signed instance path. |
| Remote manifest, route forwarding, and events | Service binding and operation-specific handling. Discovery and readiness probes have no application caller. Forwarded routes verify their own authority. An empty event wake-up carries no instance permission. |
| Sites public metadata and visitor passkey callbacks | Sites service identity, approved hostname, and visitor session or challenge where applicable. These stay on their own contracts unless explicitly migrated. |
| Cloud approval callbacks, model gateway credentials, and AI Agent dispatch, grants, release, and service-user callbacks | Their current tool, service, or persisted run authority. A run's ID is not an instance profile. |
| Proxy checks and credentials | Preserve the callerless signed checks for `verify`, `grant`, and `deploy_grant`. Purpose-scoped credentials still use the proxy service identity. |
| Console section and secret operations, global record administration, and apex probes | Preserve the existing console caller, policy, or readiness binding. Instance rollback and publishing do not borrow these administration routes. |
| Lander in-process dispatch and CI scheduler or Durable Object delivery | Preserve the stored landing caller and platform job authority. No application envelope is needed for an in-process call. Any future instance loopback must restore a persisted, approved target. |

## Record evidence for each cutover

- [ ] Identify the operation, exact ingress paths, sender and receiver Worker names, landed
  commits, and active deployment versions. Confirm the recipient advertises the expected context
  version wherever the sender negotiates it.
- [ ] Run the positive scoped check through the actual Worker binding. A tool result from a
  global connector using the old envelope verifies behavior but cannot establish signed transport.
  Record the build versions, operation, outcome and time. Do not include credentials or signed
  headers.
- [ ] Run fixture-based checks for wrong recipient or instance, declined or disabled scope,
  outdated approved bindings, and unsupported manifests as applicable. Check failures stop before
  access to protected resources. Prove a rejected contextual call never retries as legacy.
- [ ] Verify deliberate platform and compatibility paths with representative calls. Check the
  expected behavior for queued retries, jobs without attribution, and operator tools. A lack of
  refusal logs is insufficient evidence that every sender was migrated.
- [ ] Keep a rollback plan with the compatible sender and receiver versions. Pause queued
  publishing when rolling back its private contextual sender. Restore the last approved release
  pointer through the normal rollback path if a fixture publication changed it.
- [ ] Record what remains unverified and retain compatibility for that operation. Use the
  [approved fixture prerequisites](instance-live-fixtures.md) before testing live side effects.
  A fixture setup approval does not authorize ingest execution, a publish, or a delivery redrive.

After the evidence is recorded, land receiver enforcement for one operation, confirm its
deployment, and rerun its scope and platform regressions. The compatibility paths in this
checklist can coexist until their corresponding evidence gates pass.

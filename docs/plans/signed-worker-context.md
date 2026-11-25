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

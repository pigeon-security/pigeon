# MCP Federation

Status: first read-only slice, disabled by default.

Use one OAuth MCP connection to pigeon-platform for local tools and explicitly approved Jobs reads on linked instances. Each instance owns its half of the approval in its record and can revoke it independently. An instance owner or platform admin makes each change with their own passkey through Capabilities. The signed record change covers the peer instance ID, person ID, exact app service user ID and tool allowlist. Defining a record and enabling federation are separate actions. Agent instance drafts cannot change federation trust.

## Request and authority

The shared core currently serves both home MCP and the instance-scoped DigitalOcean Jobs tools from the same Directory and dispatch process. This slice resolves the normal OAuth bearer caller, finds its current standard user link, reads both records and the named person's effective instance membership, and passes a scoped request down the existing instance dispatcher. The same person must have a direct viewer-or-higher membership on both sides, including an admin who wants to use federation. There is no cross-Worker or cross-account forwarding of client tokens, service credentials or signing keys. A deployment with a different core or identity directory would require a separate transport design.

The destination must be a current linked instance ID. Direction is home to linked. Neither enablement nor membership alone confers access. An exact person and service-user pair with matching grants in both records is required for each tool. Only the fixed read-only DigitalOcean Jobs tool set can be selected. An OAuth app disconnected or relinked to a different person loses the old path. Access service headers and run identities are outside this bearer-only slice. The direct instance MCP path and local platform tools are unchanged.

Global MCP discovery includes the approved tool names and `federated_instances` only while some usable grant exists. `federated_instances` lists authorized instance IDs and their tool names. The three federated tools require an explicit `instance` argument. Current records and membership are read again on each request so revocation and disable apply to subsequent discovery and calls, even for a client with an old tool list. The destination's own enabled capability, scoped argument rules, checked dispatcher and tool policy still apply. Structured Worker logs record the destination, source instance and Worker, person, app, tool and outcome for dispatches. They omit arguments, responses and credentials.

## Canary

1. On home, open Access → People and expand the person and their connected OAuth app. Copy the person and the app's service user ID. Check the person has a direct role of Viewer or higher on both instances.
2. In the home instance's Capabilities, enable MCP Federation for the linked instance ID and the exact person and app. Review and approve the desired DigitalOcean reads with a passkey.
3. On the linked instance, repeat using the current home instance ID and the same person, app and tool choices. Each approval is on its own instance.
4. Use the selected app's existing OAuth connection to the global MCP endpoint. Refresh its tool list if cached. Check that a home platform read still works, `federated_instances` lists the linked instance, and `digitalocean_droplet_jobs` with that instance ID returns its history. A missing instance, an ungranted tool, or a different connected app must be denied.
5. Revoke a grant on either instance and retry using the same MCP session. The federated tool should stop working immediately. Repeat the check on the other side after re-approving if needed.

# Module inventory and capability scope

The signed-in Architecture console and `architecture_describe` expose the module inventory
read from the same embedded source and hosted manifests as the architecture diagram.

| Field | Source |
| --- | --- |
| Source | Top-level modules in `pigeon-capabilities` and `pigeon-core`, plus capability registrations returned by Worker manifests. |
| Scope | The rollout classification in `src/capabilities/src/architecture/inventory.rs`. |
| Hosted by | The Worker named by each currently registered capability. |
| Built into | The Worker deployment inputs recorded in `ci/workers.toml`, including shared crates. |
| Tools, routes and versions | The currently registered capability and its manifest. The Worker cards still show Worker-level routes and deployment metadata. |
| Code references | Static Rust references to known core and capability modules, including submodules. These are source-level dependencies. |

A source module can have no hosted capability. A crate can also be compiled into several Workers
while its tools are hosted by one Worker. If a manifest is unavailable, the inventory omits its
runtime claims.

Instance features can be enabled on an approved instance profile. Main-platform features retain
their platform scope until a port is declared. The other classifications identify shared services,
platform administration, and routing. These labels describe the rollout. Authorization stays in
the dispatcher and instance tool registry.

Use `architecture_describe {instance: "<id>"}` to see the tools exposed by that approved
instance's scoped MCP endpoint. It uses the same registry filter as the endpoint and returns only
the instance identifier and tool names in `instance_tools`. The public architecture view omits
the detailed module inventory and instance data.

When adding a module, update its intended classification. A new capability with no classification
is displayed as **Unclassified**, so its intended role can be reviewed.

## Read-only CI instance profiles

Opt in on a home-account Artifacts instance with
`"ci":{"state":"enabled","jobs":["test"]}`. The approved repository and declared
job names are the scope. Because the current runners share provider pipelines,
the profile identifies logical jobs within that repository. An overlapping
assignment is refused and ambiguous attribution fails closed.

New jobs record an instance owner only when the core scope matches exactly one
approved instance. Legacy or unattributed jobs remain visible through the
main-platform tools. Scoped listing filters before applying its limit. Scoped
status checks ownership before reading job output or events. Job submission and
cancellation remain main-platform operations.

## Scoped visitor access, queued execution, and Worker metrics

Site visitor sessions and enrollment are bound to the applied site, instance
and hostname. Unscoped legacy site visitor identities require a fresh invite.
This boundary applies on the Pigeon-served hostname and does not change direct
access to a linked Worker's public origin.

An explicitly enabled ingest profile fixes the source and destination.
GitHub-triggered publishing rechecks the approved binding and current policy
before queued executions. See [entry points](instance-entrypoints.md)
for the controls that remain shared.

Worker metrics may be enabled by an approved `observability` profile for the
record's Worker. The query and summarized results are both limited to that
script. The cost figure is an estimate from its usage, and billing, logs, and
token capacity remain platform tools. A linked account requires an analytics
grant; enabling the profile does not extend the account's OAuth permissions.

# Main-platform capability scopes

Phase 6E of the instance capability rollout. The main platform continues to
register its existing capabilities. An approved instance binding enables only
the tools its capability declares for the bound instance endpoint. The
capabilities below have main-platform entry points. An instance profile and
a resource name in a tool argument do not add a new instance entry point.

The main-platform dispatcher applies the existing caller and approval policy.
Each capability then applies its own resource, caller, or approval checks.
Keeping a tool on the main platform leaves those checks in force.

## Entry points and resource boundaries

| Capability | Owner and current entry points | Resource boundary | Shared operations to reuse |
| --- | --- | --- | --- |
| `apple` | `pigeon-platform`. App Store status, iOS release, and APNs tools. Console actions adopt the bundle and create signing identities with passkey approval. | The platform app `dev.pigeon.app`, its App Store Connect team and signing assets, and its registered push devices. | Apple and APNs adapters, platform approvals, and the shared job interface for release work. |
| `ai-agents` | `pigeon-ai-agents`. Agent run and configuration tools. | The platform run store, the source checkout used for runs, and run-specific caller, budget, sandbox, and Git grants. | Model invocation, repository access, jobs, and the existing delegation and grant services. |
| `models` | `pigeon-model-gateway`. Model invocation and job tools, provider setup, usage, logs, and billing views. | Platform gateway configuration and provider credentials. Jobs use their existing caller checks. Account-wide views stay on the platform. | Model invocation and job status through the gateway contract. Callers such as AI Agents can use these operations without exposing model administration tools. |
| `cloud` | `pigeon-cloud-gateway`. Provider status, IAM and object inventory, and plan/apply tools. | Declared platform provider credentials and the projects and regions configured for those credentials. Writes go through the existing plan and approval flow. | Provider adapters and plan/apply operations. Instance-scoped object access belongs to the approved Storage binding. |
| `mirror` | `pigeon-platform`. `source_mirror` and the source push event handler. | The configured Artifacts source, destination Git repository, and permitted refs. Destination updates follow the existing forward-only and no-delete rules. | Git reads and writes, together with a destination credential scoped to the mirror. The mirror owns reconciliation and ref policy. |
| `changelog` | `pigeon-platform`. `changelog`, `changelog_update`, and processing of Artifacts pushes to main or landing notes. | The platform source history and changelog R2 bucket. | Git history and object storage. The feature owns extraction, classification, and incremental processing. |
| `deploys` (retired 26 Nov) | Was `pigeon-platform`. `deploy_token` and `revoke_api_token`, removed: deploys go through `land` or `/cf/`, break-glass through the person's own Cloudflare login (`tool-approvals.md`). | Platform account Cloudflare deployment credentials. Token issuance follows the platform's existing passkey policy. | Scoped credential minting and revocation. Landing and publishing can deploy through their own already-bound Worker operations. |
| `images` (folded into `observability` 26 Nov) | Was `pigeon-platform`. `containers_images`, now a tool of the `observability` capability (`observability/images.rs`), same name, schema and answer. | Read-only inventory of platform container applications and registry images. Registry pull credentials stay inside the Worker. | Container and registry read operations for platform diagnostics. |
| `site` | `pigeon-platform`. Public apex routes and the `site_publish` tool. | Pigeon's public apex, served by `pigeon-platform` alone. | Hostname and Worker operations. The feature owns the public architecture site. Instance content publishing is served by `pigeon-sites`. |

[Platform composition](../../src/platform/src/settings.rs) ·
[AI Agents tools](../../src/ai-agents/src/service.rs) ·
[Model tools](../../src/models/src/lib.rs) ·
[Cloud tools](../../src/cloud/src/tools.rs) ·
[Public apex](../../src/capabilities/src/site.rs)

## Criteria for a later instance port

1. Identify the operation an instance needs and the exact approved account,
   repository, project, bucket, Worker, or workload it can affect.
2. Declare the instance tool on its capability and bind the target before
   dispatch. Verify scope in the owning Worker before reading or writing
   resources, including requests through generic tool calls and jobs.
3. Keep privileged platform administration and existing main-platform tools
   on their current routes. If a feature only needs an internal operation,
   call its shared interface under a verified caller and resource scope.
4. Test discovery and execution together. Cover a permitted target, an
   attempt to select another target, and the corresponding main-platform
   behavior.

No new shared code is required for this classification. Existing Git,
account, storage, registry, and provider adapters already supply the mapped
operations. Extract a narrower domain operation when a concrete second
consumer needs it, with its caller and resource checks intact.

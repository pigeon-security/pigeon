# The Cloud Gateway: cloud providers behind one capability Worker

Status: **Phases 0 to 4 landed** (plan by planner run `vSg5LiNay8Uw`; phase 1 by executor
`KmMJEBZ8UUNA`; phase 2 and the restack by planner run `qzv3qK9SBfNU`). Ships latent: no
Scaleway credential exists yet, and every tool says so until the person sets the slots and the
bindings land (phase 6). The person's morning steps are in "For the person". Open items are
listed before the Log.

The person's words, in short: a new capability Worker, the Cloud Gateway
(`pigeon-cloud-gateway`, short name `cloud-gateway`), that is to cloud providers what the model
gateway is to model providers and the CI gateway is to CI connectors. One Worker holds the
provider credentials, talks to provider APIs, and offers MCP tools through core's capability
manifest, so a person or an agent can view resources and change them. A provider-neutral core
(a provider trait: list, plan and apply per resource kind) with Scaleway as the first and only
provider. First resources: Scaleway IAM API keys (with the applications and policies they
belong to), and Object Storage buckets and objects. Every write is plan then apply, behind the
person's passkey.

**In short.**
- A library crate `pigeon-cloud` (`src/cloud`) holds the provider trait, the Scaleway provider
  and the tools, as `pigeon-models` does for the model gateway. A thin Worker crate
  `src/cloud-gateway` serves it through `pigeon_contract::serve`, as `src/model-gateway` does.
- Reads need no approval. A plan tool reads the provider, builds the change, and creates an
  approval exactly as `portal_oauth_redirects` does. `cloud_apply {plan}` runs only once the
  person approved it with their passkey on the Approvals tab. It re-reads the provider, refuses
  and names the drift if the state moved, makes the change, reads again and reports any
  difference.
- Approvals live in core's Directory, which a capability Worker cannot reach. So core gets one
  new op for capability Workers, `approval` (create, get, take), limited to a fixed table of
  kinds, with the approver always the person the signed caller acts for.
- A new Scaleway API key's secret is never in a tool's answer or a log line. It is written to
  the pigeon secret slot the caller names (`CLOUD_…`), and the answer says which slot.
- Outbound hosts are fixed in code: `api.scaleway.com`, `s3.fr-par.scw.cloud`,
  `s3.nl-ams.scw.cloud`, `s3.pl-waw.scw.cloud`. The transport refuses any other host.

## Sources

Read from this sandbox while planning.

- **Scaleway IAM API** (`api.scaleway.com/iam/v1alpha1`). Checked against Scaleway's generated
  Go SDK, `scaleway/scaleway-sdk-go`, `api/iam/v1alpha1/iam_sdk.go` at `master`, which is
  generated from the same API description as the docs. Auth is the header
  `X-Auth-Token: <secret key>`. Endpoints used:
  - `GET /api-keys` (query `organization_id`, `application_id`, `page`, `page_size`, `order_by`;
    answers `{api_keys, total_count}`; each key has `access_key`, `secret_key` (null on reads),
    `application_id`, `user_id`, `description`, `created_at`, `updated_at`, `expires_at`,
    `default_project_id`, `editable`, `deletable`, `managed`, `creation_ip`).
  - `GET /api-keys/{access_key}`, `DELETE /api-keys/{access_key}`.
  - `POST /api-keys` with `{application_id, expires_at, default_project_id, description}`;
    the answer is the one place the secret key appears.
  - `GET|POST /applications` (`{name, organization_id, description, tags}`),
    `GET|DELETE /applications/{id}`.
  - `GET|POST /policies` (`{name, description, organization_id, rules, tags, application_id}`;
    a rule is `{permission_set_names, condition, project_ids | organization_id}`),
    `DELETE /policies/{id}`.
  - `GET /permission-sets`.
  An unauthenticated `GET https://api.scaleway.com/iam/v1alpha1/api-keys` answers 401 from
  here, so the host and path exist.
- **Scaleway Object Storage**: the S3 API at `https://s3.<region>.scw.cloud` for regions
  `fr-par`, `nl-ams` and `pl-waw`, path-style, signed with AWS Signature Version 4 with the
  region as the signing region and `s3` as the service. `https://s3.fr-par.scw.cloud/` answers
  from here. The calls are S3's own: ListBuckets, ListObjectsV2, HeadObject, CreateBucket,
  ListObjectVersions, DeleteBucket.
- **SigV4**: pigeon already signs S3 requests in `pigeon_core::s3` (used by `storage`), checked
  against the S3 API reference's worked examples. The cloud gateway uses it, not a second
  signer. The smoke scenario checks it against AWS's published examples ("Signature Calculations
  for the Authorization Header", the GET Object, PUT Object, GET Bucket Lifecycle and GET Bucket
  (List Objects) examples, keys `AKIAIOSFODNN7EXAMPLE` and its published example secret).
- **pigeon**: `src/model-gateway`, `src/models` (`ModelGateway`, `credential`, `purposes`,
  `models_providers`), `src/ci-gateway`, `src/ci` (`Buildkite::secrets`, `secret`),
  `src/contract`, `src/capabilities/src/split.rs` (core's ops, `WORKER_KEYS`),
  `src/capabilities/src/portal_oauth.rs` (plan, approval, apply, drift),
  `src/capabilities/src/approvals.rs`, `ci/workers.toml`, `docs/plans/cleanup.md` item 4 (a new
  Worker takes three landings), `docs/plans/model-providers.md` phase 1b (a binding to a secret
  that does not exist yet waits for the person).

## Decisions

1. **Two crates, as the model gateway has.** `src/cloud` (`pigeon-cloud`, a library: the
   provider trait, Scaleway, the tools and the `Service` impl `CloudGateway`) and
   `src/cloud-gateway` (`pigeon-cloud-gateway`, the Worker: reads `CALLER_KEY`, then
   `pigeon_contract::serve`). The smoke suite reaches the library as a dev-dependency of
   `pigeon-capabilities`, as it does `pigeon-ai-agents`.
2. **The provider trait.** One trait, `Provider`, per cloud: its name, its secret slots, its
   resource kinds, `status` (configured and ready, or why not), and per kind `list`, `plan` and
   `apply`. A plan is data: provider, kind, action (`create` or `delete`), the normalised
   arguments, the state read when planning (`before`) and plain lines for the person. All HTTP
   goes through a `Transport` trait, so the smoke suite runs the whole tool flow against a
   mocked Scaleway, and the Worker's transport is `worker::Fetch`.
3. **One host allowlist, in the transport.** `HOSTS` lists the four hosts above. The Worker's
   transport refuses a URL whose host is not on it, before any fetch. Buckets are addressed
   path-style only, so a bucket name never becomes a host. The capability's footprint names
   Scaleway as an external with these hosts, so the architecture page shows them.
4. **Credentials ship latent.** The capability declares four slots, all scope `workers`, named
   as Scaleway's own tools name them: `SCW_ACCESS_KEY`, `SCW_SECRET_KEY`,
   `SCW_DEFAULT_ORGANIZATION_ID`, `SCW_DEFAULT_PROJECT_ID`. The ids are secrets too, so nothing
   commits them. A binding to a secret that does not exist yet may stop a deploy, so the
   bindings land only after the person sets the slots (phase 6), as `ANTHROPIC_ADMIN_KEY` did.
   Until then every tool answers "Scaleway is not set up: set <the missing slots> on the Access
   tab", and says when they are set but not bound yet.
5. **`cloud_status`** (the pattern of `models_providers`): each provider, whether each slot is
   bound and set (never a value), whether it is ready, why not, its hosts, and, when ready, a
   live check (the gateway's own key read from `GET /iam/v1alpha1/api-keys/{access_key}`: its
   expiry and default project).
6. **Plans become approvals through a new core op.** Core's Directory keeps approvals, and only
   core reaches it. Core gets one new op for capability Workers, `approval`, signed for the
   caller like `credential`, with `action` `create`, `get` or `take`:
   - Only kinds in core's fixed table `REMOTE_APPROVAL_KINDS` (today `cloud_change`); any other
     is refused. A Worker never chooses the approver: it is the person the signed caller acts
     for, and a caller acting for no one cannot plan.
   - Core computes the digest itself (`approval::payload_digest(kind, subject, payload)`),
     sets the summary to `{plan: lines}` and the wait to 30 minutes, as `portal_oauth`.
   - `get` answers a record of a table kind that the signed caller made, and reads `expired` once
     it lapses; `take` re-computes the digest from the payload
     the Worker re-planned and spends the approval once for the signed caller, as
     `approval.take` does.
   This adds an op to `CORE_OPS`, which the contract allows (fields and ops are only added). A
   core one version behind answers `unknown_op`, which the gateway reports as "core does not
   take capability approvals yet".
7. **Plan then apply, as `portal_oauth_redirects`.** Each `cloud_*_plan_*` tool reads the
   provider, plans, and creates a `cloud_change` approval whose payload is the plan (provider,
   kind, action, arguments, `before`). Nothing changes at the provider. `cloud_apply {plan}`
   gets the approval, re-reads the provider and re-plans; if the new payload differs it names
   what moved and does not take the approval ("plan again"). Otherwise it takes the approval,
   applies, reads again and reports `differences` from what the plan expected. One apply tool
   serves every kind.
8. **Tokens.**
   - `cloud_tokens_list`: API keys of the default organization, paged, with their
     application's name and policies. The `secret_key` field is dropped from every answer even
     when present.
   - `cloud_tokens_plan_create {name, permission_sets, project_ids, expires_days, slot}`: an
     IAM application `pigeon-<name>` tagged `pigeon`, a policy on it with one rule granting the
     named permission sets on the named projects (the default project if none), and an API key
     for it expiring in `expires_days` (30 by default, 365 at most), with the first project as
     its default project.
   - Permission sets are limited to a fixed allowlist in code, the Object Storage sets
     (`ObjectStorageFullAccess`, `ObjectStorageReadOnly`, `ObjectStorageBucketsRead`,
     `ObjectStorageBucketsWrite`, `ObjectStorageBucketsDelete`, `ObjectStorageObjectsRead`,
     `ObjectStorageObjectsWrite`, `ObjectStorageObjectsDelete`). The gateway's own key must
     manage IAM, so without this list a plan could grant IAM or organization rights.
     Organization-wide rules are refused; a rule names projects.
   - `slot` must be `CLOUD_` then 1 to 56 of `A-Z`, `0-9`, `_`, must not be one of the
     gateway's own four, and must not exist yet in the Secrets Store (no overwrite in this
     cut). On apply, the gateway first checks it can write the slot (a `secrets_store` scoped
     credential, `Writer::open`, `list`), then creates the application, the policy and the key,
     then writes `ACCESS_KEY:SECRET_KEY` (the form `pigeon_core::s3::parse_keys` reads) to the
     slot with scope `workers`. If the write fails, it deletes the new key at once and says so.
     The answer names the slot, the access key, the application and the expiry; never the
     secret.
   - `cloud_tokens_plan_delete {access_key, with_application}`: deletes one API key. It refuses
     the gateway's own key. With `with_application`, it also deletes the key's application and
     its policies, only when the application is tagged `pigeon` and holds no other key.
9. **Object Storage.**
   - `cloud_buckets_list {region?}` (all three regions when none), `cloud_objects_list {region,
     bucket, prefix?, delimiter?, max_keys?, token?}` (ListObjectsV2, paged by continuation
     token, `max_keys` 1 to 1000), `cloud_object_head {region, bucket, key}`.
   - `cloud_buckets_plan_create {region, bucket}`: S3 bucket name rules checked; refused if the
     bucket exists. Applied as `PUT /{bucket}` with `x-amz-acl: private`; no public ACL option
     in this cut. The bucket lands in the gateway key's preferred project (Scaleway's S3 API has
     no project argument), and the plan says so.
   - `cloud_buckets_plan_delete {region, bucket}`: only when empty, checked with
     ListObjectVersions (`max-keys=1`, which also sees old versions and delete markers) when
     planning and again when applying.
10. **Secrets never leave.** Keys are held in types whose `Debug` hides the secret (as
    `pigeon_core::s3::Keys`). Every error from Scaleway passes through one redactor that
    replaces each known value (the gateway's secret key, its access key, a new key's secret)
    and anything shaped like a Scaleway access key (`SCW` and 17 more of `A-Z0-9`) with
    `<redacted>`. Request headers are never part of an error. No `console_log!` of a request or
    an answer body. The organization and project ids appear in tool answers (a person needs
    them to scope a plan) but never in a commit.
11. **Three landings for the Worker**, per cleanup.md item 4, since lanes come from main's map:
    A (crates, map entry, deploy job, the core op), then B (a one-line change in the Worker's
    crate: its first deploy), then C (core binds it as `CLOUD` and lists it in
    `settings::BINDINGS`). Then the tools appear behind the portal, latent.
12. **Overlap with `storage` and `ingest`.** `storage` (in core) reads objects across three
    providers with one `ACCESS:SECRET` secret per account; `ingest` holds its own Scaleway
    project key for spot VMs. The cloud gateway manages resources; it does not move them into
    either. A key the gateway makes can be declared as a `storage` account later, since it is
    stored in the same form. Consolidation is an open item, not this plan.

## Phases

| # | What | Who | Paths | Landing |
| --- | --- | --- | --- | --- |
| 0 | This plan and its README row | planner | `docs/plans/` | docs only |
| 1 | `pigeon-cloud`: provider trait, transport, host allowlist, redaction, Scaleway IAM and S3 request builders and parsers, plan/apply, the tools, `CloudGateway` (`Service`); the Worker crate `src/cloud-gateway` (CALLER_KEY only); workspace member; `ci/workers.toml` entry, `ci/deploy/deploy-cloud-gateway.sh`, `[jobs.deploy-cloud-gateway]`; smoke `cloud_*` with a mocked Scaleway | executor | `src/cloud/*`, `src/cloud-gateway/*`, `Cargo.toml`, `Cargo.lock`, `src/capabilities/Cargo.toml` (dev-dep), `ci/`, `.pigeon/ci.toml`, `tests/smoke.rs`, `tests/smoke/cloud.rs` | A |
| 2 | Core's `approval` op for capability Workers (`CORE_OPS`, `ApprovalRequest`, `REMOTE_APPROVAL_KINDS`), `WORKER_KEYS` gains `cloud-gateway`; smoke | executor, beside 1 | `src/contract/*`, `src/capabilities/src/split.rs`, `tests/smoke/proxy.rs` or `core.rs` | A |
| 3 | First deploy of `pigeon-cloud-gateway` | planner | one line in `src/cloud-gateway/src/lib.rs` | B |
| 4 | Core binds it: `[[services]] CLOUD` in `src/platform/wrangler.toml`, `settings::BINDINGS`; architecture smoke | planner | `src/platform/*`, `tests/smoke/core.rs` | C |
| 5 | Live check, latent: `cloud_status` and each tool answer "not set up" through the portal | planner | none | none |
| 6 | **After the person sets the four slots**: bind them in `src/cloud-gateway/wrangler.toml`, then `cloud_status` live | planner | `src/cloud-gateway/wrangler.toml` | D |

## How to use it

Once phases 0 to 4 have landed (the tools then appear through core's MCP and the portal), and
phase 6 after the person's slots. Until phase 6 every tool answers "Scaleway is not set up: set
<slots> on the Access tab", and `cloud_status` says which slot is missing.

- `cloud_status`: which providers are set up and ready, and why not. When ready it also reads
  the gateway's own key (its expiry and default project).
- Read: `cloud_tokens_list`, `cloud_buckets_list`, `cloud_objects_list`, `cloud_object_head`.
- Change: call a plan tool, such as
  `cloud_buckets_plan_create {region: "fr-par", bucket: "pigeon-backups"}`. It answers the plan
  lines and an approval id. The person approves it on the console's Approvals tab with their
  passkey (it waits 30 minutes, and once approved it can be used for 10). Then
  `cloud_apply {plan: "<approval id>"}` makes the change once, and reports any drift. The
  caller that planned must be the caller that applies, and must act for a person: a caller
  acting for no one cannot plan.
- A new scoped key:
  `cloud_tokens_plan_create {name: "backups", permission_sets: ["ObjectStorageFullAccess"],
  project_ids: ["<project id>"], expires_days: 30, slot: "CLOUD_SCW_BACKUPS"}`, approve,
  `cloud_apply`. The key's secret is in Secrets Store secret `CLOUD_SCW_BACKUPS` as
  `ACCESS_KEY:SECRET_KEY`; it is never shown.

## For the person (morning)

Nothing here is needed for phases 0 to 5: those land and wait. Phase 6 waits on these steps. They
make a Scaleway API key for pigeon that can manage Object Storage in one project and manage IAM
(so it can make scoped keys for other uses). The key's secret goes into pigeon's Access tab and
nowhere else: never paste it into a chat, a command or a file.

1. **An application for pigeon.** Scaleway console (console.scaleway.com) → **IAM** →
   **Applications** tab → **Create application**. Name `pigeon-cloud-gateway`, description
   "pigeon's cloud gateway". Create it. Do not make an API key here.
2. **A policy for it.** **IAM** → **Policies** tab → **Create policy**. Name
   `pigeon-cloud-gateway`. Principal: **Application**, then `pigeon-cloud-gateway`. Add two rules:
   - Scope **Organization**, permission set **IAMManager**. The gateway needs it to list, make
     and delete API keys, applications and policies. The code grants only Object Storage
     permission sets in the keys it makes (Decision 8), but this key itself can do anything in
     IAM: guard it, and give it the shortest expiry you will keep up with.
   - Scope **Access to resources**, the project(s) whose buckets pigeon manages, permission
     set **ObjectStorageFullAccess**.
   Create the policy.
3. **The API key.** **IAM** → **API keys** tab → **Generate an API key**. API key bearer:
   **An application**, then `pigeon-cloud-gateway`. Expiration: one year at most; pick a date
   you will notice. **Will this API key be used for Object Storage?** Yes, and choose the project
   new buckets should land in as the preferred project. Generate it. The page shows the **access
   key** (`SCW` and 17 capitals or digits) and the **secret key** (a UUID, shown once). Keep the
   page open until step 5 is done.
4. **Two ids.** The **organization ID**: console → your organization's name (top left) →
   **Organization settings**, field "Organization ID". The **project ID**: open the project →
   **Project settings**, field "Project ID". Both are UUIDs.
5. **Set the four slots.** pigeon console → **Access** tab → Secrets and credentials. Set each
   one and approve it with your passkey:
   - `SCW_ACCESS_KEY`: the access key from step 3.
   - `SCW_SECRET_KEY`: the secret key from step 3.
   - `SCW_DEFAULT_ORGANIZATION_ID`: the organization ID from step 4.
   - `SCW_DEFAULT_PROJECT_ID`: the project ID from step 4, the same project as the preferred
     project in step 3.
   The console checks only the form of each value (it does not call Scaleway).
6. **Bind them.** Ask an agent: "cloud-gateway.md phase 6: bind the Scaleway slots and land".
   It adds the four secret bindings to `src/cloud-gateway/wrangler.toml` and lands them.
7. **First calls**, from any MCP client signed in as you (or an agent acting for you):
   1. `cloud_status`. Expect `ready: true` for Scaleway, each slot bound and set, and the live
      check showing the key's expiry and default project. If it says a slot is missing or not
      bound, it names the slot.
   2. `cloud_buckets_list` (no arguments: all three regions). Expect your existing buckets, or
      none.
   3. `cloud_buckets_plan_create {region: "fr-par", bucket: "pigeon-first-test-<something
      unique>"}`. Bucket names are global to a region, lower case, 3 to 63 characters. It
      answers the plan and an approval id. Nothing has changed at Scaleway yet.
   4. Console → **Approvals** tab: the plan is listed (kind `cloud_change`). Read it and approve
      it with your passkey, within 30 minutes.
   5. `cloud_apply {plan: "<approval id>"}`. Expect `created` and an empty `differences` list.
      `cloud_buckets_list {region: "fr-par"}` now shows the bucket.
   6. Clean up: `cloud_buckets_plan_delete {region: "fr-par", bucket: "…"}`, approve,
      `cloud_apply`. The bucket must be empty.

## Rollback

- Before phase 6 the gateway can do nothing at Scaleway: it holds no credential.
- At once, without a landing: revoke the API key in the Scaleway console (IAM → API keys), or
  switch the `pigeon_cloud_*` tools off in the portal.
- A landing: revert C to unbind the Worker from core; delete the script by hand if wanted.

## Open items

1. **Phase 5**, the live check through the portal: not done by the planner, which has no
   MCP client for core. The person's first `cloud_status` is the check; before the slots are
   set it must say Scaleway is not set up. Phases 3 and 4 are done: phase 3 needed no landing of its own: the
   one-landing path for a new Worker (`core_land_adds_a_worker_in_one_landing`) deployed
   `pigeon-cloud-gateway` with phases 0 to 2, and landing `HiHMQFgR8wJFVeL0` bound `CLOUD` in core.
2. **The Scaleway slots and their bindings** (phase 6). Waits on: the person (morning steps).
3. **The Access tab lists `CLOUD_…` slots** a plan writes? Today it lists only declared slots;
   a written slot exists in the Secrets Store but is not shown. Later, if wanted.
4. **More resource kinds and providers** (instances for spot compute, DigitalOcean, B2): the
   trait allows them; not this plan.
5. **Consolidate with `storage` and `ingest`** (Decision 12). Not this plan.
6. **Policy rule for `cloud_apply`?** Every apply already needs a passkey-approved plan. The
   person may also add `cloud_apply` to `risky-tools-need-passkey`; only they change the
   policy.

## Log

- Planner run `vSg5LiNay8Uw`: read the model and CI gateways, the contract, `split.rs`,
  `portal_oauth.rs`, `approvals.rs`, `pigeon_core::s3`, `storage` and the plans named in
  Sources; checked Scaleway's IAM endpoints against its generated Go SDK and that both hosts
  answer. Wrote this plan.
- Planner run `qzv3qK9SBfNU`: reviewed phase 1 (`KmMJEBZ8UUNA`) and restacked it onto main.
  Changes: the Secrets Store names are listed once per survey, not once per unbound slot (each
  listing leases a core token); a failed look after a key is made no longer hides the new key.
  Built phase 2 myself: core's `approval` op (`split::remote_approval`, with pure checks
  `remote_plan`, `remote_digest`, `remote_view`; `REMOTE_APPROVAL_KINDS`), `ApprovalRequest` in
  the contract, `CORE_OPS` gains `approval`, `WORKER_KEYS` gains `cloud-gateway`. `get` and
  `take` work only for the caller that made the approval, an addition to Decision 6. Smoke:
  `core_approval_op_for_capability_workers`, and `core_architecture_draws_every_worker_in_the_map`
  draws the cloud gateway's real manifest entry and its four hosts.
- Landing `VMd5RhGvdu9UOtOv` (phases 0 to 2, commit `79497ebed392`) landed and deployed all seven
  lanes, including `cloud-gateway`. Phase 4 follows: `CLOUD` in core's `wrangler.toml` and
  `settings::BINDINGS`, and the architecture scenario checks the link.

## Standalone instance enablement

Cloud Gateway is off by default, including on home. An owner can enable it for
an approved linked instance such as `willowgraysen` from **Capabilities**, or
propose this addition to that instance's existing record and approve the change:

```json
{"capabilities": {"cloud": {}}}
```

Preserve any other `capabilities` entries in the record. On that instance's
scoped MCP endpoint, `cloud_status` reports that the gateway is enabled and
instance provider connections are not configured. Scoped status runs in core
and reads no shared platform credential. The existing gateway's organization,
project, and key slots are shared platform credentials. Linked instances cannot
use those provider tools, including through the generic `call` tool or an
`instance` argument on global MCP. The old shared tools were retired with the home Worker. Opting in exposes
setup status on the scoped endpoint.

No provider connection, secret slot, compute permission, or linked-account
deployment is created by enabling the capability. Instance-specific provider
bindings and runner lifecycle operations are separate follow-up work.

## Retiring the home Worker

Home has no Cloud Gateway by default. Remove the platform Service Binding and
automatic home deploy lane before deleting the legacy Worker in the home
Cloudflare account. Verify the deployed core revision has no CLOUD binding,
then delete only the named Worker. Preserve existing secret values during
this cutover so a rollback can restore the old binding and Worker together.

Linked records use `capabilities.cloud.placement: inline` (the default).
The inline core capability currently provides scoped setup status. The
catalogue also models `own_worker`, but M5 validation must decline it until
a linked-account build is registered, the signed instance context is
verified in that Worker, and it resolves provider credentials in the
approved instance account. The legacy home wrapper is not that build.

## DigitalOcean account connection, phase 1

DigitalOcean is opt-in for each instance and depends on Cloud Gateway being
enabled on the same instance. The approved record carries the expected
DigitalOcean team ID (`account.team.uuid`). The credential is scoped to that instance and
provider, and managed separately from the approved record.

Verification uses the inline Cloud Gateway and its DigitalOcean adapter.
The only provider request is the read-only account endpoint. Resolve the
approved target and credential on the server before any provider request.
A match confirms the token belongs to the approved account. Provider payloads
and secret values are excluded from status and error responses.

Land the capability off by default, then use a separate approved opt-in and
credential setup to verify one account on `willowgraysen`. Test home and
cross-instance denial before treating the canary as complete. Spaces access,
Droplet provisioning, and ingestion are later phases.

## Spaces credential issuance follow-up

DigitalOcean's Spaces Keys API can create bucket-scoped S3 credentials with
a separate issuer credential. The public API does not expose general
personal access token issuance. Keep the existing account verifier and
Spaces access credential separate from the issuer credential.

A future opt-in issuer may create a limited read key on demand for an
approved instance, team, bucket, and job. Record its key identifier for
revocation, revoke it when the job ends, and retry revocation from tracked
state when cleanup fails. Require a passkey bound to the exact bucket and
permission set before issuing a write-capable key. Show the provider's full
grant, including delete if bundled with write. Never return issuer
credentials or persist a newly generated secret in an audit entry.

This is separate from verifying a pre-created read-only Spaces key. The
verification capability does not request key creation or deletion scopes.

## Read-only Spaces verification, phase 2

An approved DigitalOcean profile can name one Spaces region, bucket and
optional prefix. The team token needs `account:read` and `spaces_key:read`
to verify the team and look up the submitted Spaces key's metadata. Pigeon
accepts only a single read grant on the approved bucket, then signs a
`ListObjectsV2` request for the approved prefix with `max-keys=1`. This
checks access without returning object names. Bucket access uses the
separate Spaces S3 credential. The Storage capability can remain off.

The console binds the passkey to the reviewed instance record version and a
digest of the credential. The stored key is sealed against the instance,
team and Spaces source. Changing the approved source requires verifying
and saving the key for the new binding. This rollout does not grant
DigitalOcean write permissions or provision compute.

## DigitalOcean temporary Droplet health check, phase 3

Enable `capabilities.digitalocean.droplet_health: true` in an approved instance
record. Save a **separate** team PAT through the passkey-gated console write.
DigitalOcean's [API specification](https://github.com/digitalocean/openapi/tree/main/specification/resources/droplets)
requires `account:read` for the team check, `droplet:create`, `droplet:read`,
`droplet:delete`, and `tag:create` for a tagged Droplet. Runtime checks the
credential's team; the provider enforces its permissions on each action. The
account and Spaces credentials remain separate and never enter the Droplet.

The console shows the exact Toronto plan, source revision, and estimate before
the operator uses their passkey. The challenge binds the instance, team, record
version and plan fingerprint. The approved test builds the public ingest runner
at the plan's immutable Git commit and runs `--version` with no container
network or archive credentials. The builder images and package repositories
are resolved at runtime. The instance receives a short-lived callback bearer
only and reports startup and exact expected output to the platform over HTTPS.
This verifies the script's own result, not VM attestation.

The Directory persists one active job per instance before the only create POST.
On an uncertain response the minute cron reconciles its exact generated name and
`pigeon-health-check` tag; an unresolved job waits for cleanup and blocks another
create. Before any delete, Pigeon reads the ID and verifies the name and tag.
A later read must confirm absence before closing the job. The approved 30-minute
deadline or result initiates cleanup, and failures stay pending for retry. If the
provider delays cleanup or refuses a request, the estimated charge (about
USD 0.02 for 30 minutes of the fixed size) is not a guaranteed billing cap. The
sealed credential used to create a job is retained only until its cleanup is
confirmed or the create was definitively refused.

This phase uses no Spaces or R2 access. No system creates a Droplet merely
because the capability was enabled. Live provisioning and end-to-end callback
verification require a separate console passkey action; the native tests use a
fake provider. The console can cancel an exact waiting job after a passkey check.
Cancellation invalidates further health callbacks and schedules the same
reconciliation and deletion. Retiring an instance forgets its standing token,
while the outstanding job retains just the sealed copy it needs to finish.
Operator resolution for an indefinitely ambiguous response is follow-up work.

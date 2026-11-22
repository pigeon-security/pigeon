# Buildkite, set up by pigeon: the CI gateway's Buildkite connector redesigned

Status: **phases 1, 2, 2a and 4 deployed** (22 November 2026). pigeon set Buildkite up itself on
landing `rvs7Rnt8h0cMfmzM` (`fc9e911`): cluster `pigeon`, hosted queues `pigeon-linux-small` and
`pigeon-linux-medium`, pipeline `pigeon`, its webhook, and the Cloudflare MCP server `buildkite`.
`ci_buildkite_plan` reads every step `ok`. The first `smoke` job ran on Buildkite (build 1) and
succeeded. **Phase 3** waits on the person: attaching `buildkite` to the `pigeon` portal (§4,
step 4). The person chose one token for everything and Buildkite's MCP able to act for them.

The person's goal: "redesign the CI gateway for BuildKite to use BuildKite MCP through the
portal and server. The goal of this is we can setup everything we need in BuildKite through
platform through MCP without me needing to manually configure things."

**In short.**
- pigeon declares the Buildkite state it needs in `settings.rs`: the cluster, two hosted
  queues, the pipeline and its steps, triggers off, and the webhook.
- After every landing's deploy, the Lander makes Buildkite match, through Buildkite's REST API.
  It touches only resources it created and recorded, and never deletes.
- A read-only pigeon tool, `ci_buildkite_plan`, shows what that run would change.
- Buildkite's own MCP server goes behind the portal for the person's Claude sessions, with
  **the same single API token** and write tools on, so sessions can act in Buildkite for the
  person (the person's decision, 21 November). The platform configures that portal server
  itself from the Secrets Store, so the token is entered once, on the secrets page.
- What the person still does by hand: create the Buildkite organization (if it doesn't exist),
  create **one** API token and set it on the console's secrets page, and attach the
  platform-made `buildkite` server to the portal once in the dashboard. No OAuth grant.

The design does not use Buildkite's MCP server for the setup itself. The reasons are in §1,
from the server's own source. The setup still happens "through the platform, through MCP": the
person and agents drive and see it through pigeon's MCP server (the "server" behind the
portal). Buildkite's MCP is how sessions look at Buildkite.

## Sources

Read 21 November 2026. buildkite.com, api.buildkite.com and mcp.buildkite.com refuse
connections from this session (the network proxy answers 403). Buildkite's facts are therefore
read from its sources on GitHub:

- `buildkite/buildkite-mcp-server` at `f397e10e320d` (2026-11-20): `pkg/toolsets/toolsets.go`,
  `pkg/buildkite/*.go`, `internal/commands/http.go`.
- `buildkite/docs` at `3c25db38c512` (2026-11-20): `pages/apis/mcp_server*.md`,
  `pages/apis/rest_api/{pipelines,clusters/queues,clusters,access_token}.md`,
  `pages/apis/rest_api/organizations/notification_services.md`,
  `pages/apis/managing_api_tokens.md` and `pages/platform/security/oauth.md`.

Each `pages/…` path is published at `https://buildkite.com/docs/…`. Cloudflare's facts come from
developers.cloudflare.com: the [MCP portals page](https://developers.cloudflare.com/cloudflare-one/access-controls/ai-controls/mcp-portals/)
(modified 2026-11-05), the [GA changelog](https://developers.cloudflare.com/changelog/post/2026-09-24-mcp-portals-ga/)
of 24 September, the [service-token changelog](https://developers.cloudflare.com/changelog/post/2026-06-26-mcp-portal-service-tokens/)
and the API reference under `zero_trust/access/ai_controls/mcp`. The key quotes and code paths
were checked twice: by a research agent, then again by the planner against the files.

## 1. Buildkite's MCP server

### Where it runs and how it authenticates

| Endpoint | Auth | For |
| --- | --- | --- |
| `https://mcp.buildkite.com/mcp` (and `/mcp/readonly`) | OAuth: the user picks an organization and selects **Authorize**. Access tokens last up to 1 h and refresh tokens up to 30 d. Scopes are "pre-set by the Buildkite platform", read and write; `/readonly` gets read scopes only. | People's AI tools. The docs: "not suitable for automated workflows that cannot complete interactive OAuth". |
| `https://mcp.buildkite.com/direct` (and `/direct/readonly`) | `Authorization: Bearer <API token>`, passed through to the REST API. The token's scopes decide what succeeds. Read-only routing hides write tools but does not remove the token's write scopes. | "headless agents and background services" |
| `/…/x/{toolset}[/readonly]`, or the headers `X-Buildkite-Toolsets` / `X-Buildkite-Readonly` | as above | Narrowing the tool list |
| Self-run (`buildkite/mcp-server` image, or the binary's `stdio` / `http` subcommands) | `BUILDKITE_API_TOKEN`, or per-request `Authorization` passthrough. Flags `--enabled-toolsets`, `--read-only`, `--minimum-access`. | Pinning a version. The hosted service can't be pinned and "breaking changes" are expected. |

Transport is Streamable HTTP. The docs say nothing on dynamic client registration (DCR) for
`/mcp`; this is **unconfirmed** and phase 3 finds out.

Rate limit: **50 requests a minute per user**, on both `/mcp` and `/direct`, separate from the
REST limit. The REST limit is 200 a minute per organization and 50 a minute per user.

### What its tools can and can't do

The server has 83 tools in 15 toolsets. The ones that matter for setup:

| Need | MCP tool | Enough? |
| --- | --- | --- |
| Cluster | `create_cluster`, `update_cluster` [write_clusters] | Yes |
| **Hosted** queue with an instance shape | `create_cluster_queue` takes only `org_slug`, `cluster_id`, `key` and `description` (`pkg/buildkite/cluster_queue.go`). `update_cluster_queue` changes only the description and retry affinity. | **No.** It makes self-hosted queues only. |
| Pipeline with steps YAML, in the cluster, triggers off | `create_pipeline` and `update_pipeline` with `configuration`, `cluster_id` and `provider_settings.trigger_mode` (`code`, `deployment`, `fork` or `none`) [write_pipelines] | Yes |
| Webhook to `/ci/hooks/buildkite` (a notification service) | none. `create_pipeline_webhook` is the GitHub repository webhook, a different thing. | **No** |
| Builds: create, read, cancel, wait | `create_build`, `get_build`, `cancel_build`, `wait_for_build` (about 45 s per call) | Yes, but its responses are trimmed subsets of the REST fields. |
| Logs, failures | `read_logs`, `tail_logs`, `search_logs`, `get_build_failure_summary`, `compare_builds` | Yes. This is its strength. |
| API tokens, agent tokens, teams, organizations, deleting anything | none | **No** |

### Done by API instead of MCP

| Need | Route |
| --- | --- |
| Hosted queue | `POST /v2/organizations/{org}/clusters/{cluster}/queues` with `{"key", "description", "hostedAgents": {"instanceShape": "LINUX_AMD64_2X4"}}` [write_clusters] (`rest_api/clusters/queues.md`) |
| Webhook | `POST /v2/organizations/{org}/services` with `provider: "webhook"` and `settings: {url, token, token_mode: "signature", events: [...]}`. Read it back with `GET`; change it with `PATCH …/services/{uuid}` [read/write_notification_services]. **The token's owner must be an organization admin**, unless "Manage Notification Services" is on for members. `GET` returns webhook tokens in full. (`rest_api/organizations/notification_services.md`) |

### What has no API at all, so stays with the person

- **The organization.** The REST and GraphQL APIs only read organizations.
- **The API token.** Created in the UI only: Personal Settings → API Access Tokens. The API
  only reads and revokes the token in use (`GET`/`DELETE /v2/access-token`).
- Nothing else: with one token, the portal server needs no OAuth grant.

### Why the setup and the connector call the REST API, not Buildkite's MCP

1. **Two of the five resources can't be made through MCP**: the hosted queues and the webhook.
   An MCP-based setup would still need REST for them, so it would run two clients for one job.
2. **Calling MCP from the Worker would be worse at every step.**
   - The portal times out every tool call at 60 s.
   - Its service-token sessions use the portal admin's OAuth credential, which "can expire
     silently". They also skip servers that require user auth.
   - `/direct` limits each user to 50 requests a minute.
   - Its responses are trimmed.
   - It can't be pinned to a version.
3. **REST is already what the connector uses.** `pigeon_core::buildkite` makes builds, reads
   them and cancels them today. It is one module, with errors that never echo the token.

So Buildkite's MCP is for **people and sessions working in Buildkite by hand**: logs, failure
summaries, build history, and (by the person's choice) acting there: rebuilding, cancelling,
editing pipelines. pigeon's own setup and connector stay on REST.

## 2. Who reaches Buildkite, and how

```
claude.ai "Pigeon MCP" ──OAuth (Access)──▶ mcp.pigeon.dev/mcp  (portal "pigeon")
 (cloud sessions inherit it)                 ├─ server "pigeon"    → platform.pigeon.dev/mcp
                                             │    ci_buildkite_plan, ci_status, …  (pigeon's OAuth)
                                             └─ server "buildkite" → mcp.buildkite.com/direct
                                                  buildkite_read_logs, buildkite_create_build, …
                                                  (bearer: the one BUILDKITE_API_TOKEN, set on
                                                   the server by the platform, never typed)

pigeon-platform Worker ──REST, BUILDKITE_API_TOKEN──▶ api.buildkite.com/v2
   setup (after each deploy): cluster, queues, pipeline, notification service
   connector (each job): create, read and cancel builds
Buildkite ──signed webhook──▶ platform.pigeon.dev/ci/hooks/buildkite
Buildkite runner ──OIDC token──▶ platform.pigeon.dev/ci/token  (unchanged)
```

| Caller | Path | Credential, and where it lives |
| --- | --- | --- |
| The person's Claude sessions (the "Pigeon MCP" connector) | Portal → server `buildkite` → `https://mcp.buildkite.com/direct`, `auth_type: "bearer"` | `BUILDKITE_API_TOKEN`: the platform copies it from the Secrets Store into the server's `auth_credentials` through Cloudflare's API (§3, "The portal server"). Cloudflare stores it encrypted and never returns it. |
| The same sessions, for setup | Portal → server `pigeon` → `ci_buildkite_plan` (read) | pigeon's OAuth, as today |
| The platform's setup and connector | Worker → `api.buildkite.com` directly | `BUILDKITE_API_TOKEN` in the Secrets Store, set once on the console's secrets page. It is never in chat or in a command. |
| Buildkite webhooks | → `/ci/hooks/buildkite` | The webhook key. **pigeon generates it**, keeps it in the scheduler's SQLite (`buildkite_hook_key`) and gives it to the notification service. Nobody copies it, and it is not a Secrets Store secret (see §3, "The webhook key"). |
| Buildkite runners | → `/ci/token` | The step's OIDC token (unchanged) |

**What the single token opens to sessions.** With write tools on, any session using "Pigeon MCP"
can, through `buildkite_*` tools and the token's scopes:

| Can | Tools | Scope |
| --- | --- | --- |
| Start, cancel, rebuild builds; retry, cancel or unblock jobs; annotate | `create_build`, `cancel_build`, `rebuild_build`, `retry_job`, `retry_failed_jobs`, `cancel_job`, `unblock_job`, `create_annotation` | `write_builds` |
| Create and edit pipelines, their steps, triggers and schedules; GitHub webhook processing | `create_pipeline`, `update_pipeline`, `create_pipeline_schedule`, `update_pipeline_schedule`, `create_pipeline_webhook`, `enable/disable_pipeline_github_webhook_processing` | `write_pipelines` |
| Create and edit clusters, queues (not hosted shapes), cache registries; pause a queue | `create_cluster`, `update_cluster`, `create_cluster_queue`, `update_cluster_queue`, `pause/resume_cluster_queue_dispatch`, cache-registry tools | `write_clusters` |
| Cannot | delete anything; touch notification services, API or agent tokens, teams, the organization; create cluster secrets (no `write_secrets`) | — |

Consequences, said plainly:
- These writes go **around pigeon's policy and scheduler**: the portal's per-tool switches are
  the only control on them, and they are the person's dashboard decision, not a policy rule.
- A build a session starts by hand on pipeline `pigeon` carries no `pigeon_job` meta-data, so
  the gateway ignores its webhooks; without `PIGEON_*` env it runs no step and passes empty.
- A session's edit to a pigeon-made resource (the `pigeon` pipeline's steps, a queue) is drift:
  `ci_buildkite_plan` shows it, and the next landing's deploy puts it back. The marker in each
  description says so. Sessions should make their own pipelines for experiments.
- Every portal user shares the token. While the portal's Access policy admits only the person,
  that is the person. Before anyone else is admitted, this server needs per-user auth (OAuth,
  `/mcp`) or its own Access policy.
- The portal's dashboard log records each call (server, tool, status), not its arguments;
  Buildkite's audit log names the token.

**Tool names.** Server ID `buildkite`, so tools appear as `buildkite_get_build` and so on. No
aliases; every tool on (`default_disabled` off), as the person chose. The settings generator does
**not** pre-approve any `buildkite_*` tool, so `.claude/settings.json` is unchanged and no
passkey landing is needed. Each Buildkite call prompts until the person decides otherwise.

## 3. Declarative setup

### What is declared

It goes in `src/platform/src/settings.rs`, next to the queue table, which it replaces:

```rust
const BUILDKITE: buildkite::Declared = buildkite::Declared {
    org: "",                                   // the organization's slug; "" = connector off
    cluster: "pigeon",
    pipeline: "pigeon",
    queues: &[
        // key, instance shape, class size; each runs the class with and without docker
        ("pigeon-linux-small",  "LINUX_AMD64_2X4",  Size::Small),
        ("pigeon-linux-medium", "LINUX_AMD64_4X16", Size::Medium),
    ],
    hosted_only: true,
};
```

Derived from it, never declared twice:
- each queue's `Offered` limits, from a shape table in the connector (2X4 → 2, 4, 47; 4X16 → 4,
  16, 95);
- the steps YAML (`steps_yaml`, as built);
- the hook URL `https://{HOST}/ci/hooks/buildkite`.

The pipeline settings are fixed in code:

| Setting | Value |
| --- | --- |
| Cluster | the declared one |
| `repository` | `https://github.com/pigeon-security/pigeon.git`. Required by the API, but never cloned, since every step skips checkout. |
| `provider_settings` | `trigger_mode: "none"`, `build_branches: false`, `build_pull_requests: false`, `build_tags: false`, `publish_commit_status: false` |
| `skip_queued_branch_builds`, `cancel_running_branch_builds` | `false` |
| `default_branch` | `main` |

The notification service is `provider: "webhook"`, scoped `some_projects` to this pipeline, with:

| Field | Value |
| --- | --- |
| `url` | the hook URL |
| `token` | the generated webhook token |
| `token_mode` | `"signature"` |
| `events` | `["build.running", "build.finished", "ping"]` |
| `tls_verify` | `true` |

`settings.rs`, not `.pigeon/ci.toml`: `ci.toml` says what jobs run and is read at the commit
being run; Buildkite's shape is platform configuration that has to match the deployed connector.
A change to it is an ordinary landing.

### Ownership: touch only what pigeon made

This is the lesson from the Access incident, where setup code deleted applications chosen by
domain alone.

- **A record.** The scheduler's SQLite gets a table, `buildkite_owned (kind, key, id,
  created_at, spec_hash)`. One row per resource pigeon created: `cluster/pigeon`,
  `queue/pigeon-linux-small`, `pipeline/pigeon`, `service/hook`. Migrations are appended, as
  always.
- **A marker.** Each resource's `description` carries `managed by pigeon (platform.pigeon.dev);
  changes here are overwritten`, plus `pigeon:<kind>:<key>`.
- **The rule.** The setup updates a resource only when its **id is in the record and** its
  description carries the marker. A resource that matches by name but not by record is never
  changed, adopted or deleted. The setup stops at that resource with "a Buildkite <kind> named
  <key> exists and is not pigeon's: rename it, or delete it, then deploy again". Resources
  further down that depend on it are not attempted.
- **No deletes.** The setup creates and updates only. A queue or pipeline removed from
  `settings.rs` is reported as `orphaned` and left alone. Deleting it is the person's job in
  Buildkite. A later, optional passkey-gated console action could do it.
- **Lost record.** If the record is lost (a reset scheduler), every resource reads as "exists,
  not pigeon's". Nothing is changed, and the person is told. Re-adoption is a passkey-gated
  console action (phase 5), never automatic.

### The plan and the apply

`pigeon_capabilities::ci::buildkite::setup`:

- `observe(cx) -> Observed`. Reads only: `GET /v2/access-token` (scopes and expiry), the
  cluster, its queues, the pipeline and the organization's services. About 5 requests.
- `plan(declared, observed, owned) -> Vec<Step>`. A **pure function**, so it is tested in the
  smoke suite. Each `Step` is one of `create`, `update {fields}`, `ok`, `conflict {why}`,
  `orphaned`, or `blocked {why}` (for example a missing scope, a plan that refuses a shape, or
  an earlier conflict). An update names only the fields that differ, and never a secret's value.
- `apply(cx, steps)`. Runs `create` and `update` in dependency order (cluster → queues →
  pipeline → service). It records each created id before going on. It stops at the first
  failure and reports what was done. It is idempotent: a second run right after plans all `ok`.

**The webhook key.** On the service's first `create`, `apply` generates 32 random bytes
(base64url), stores them in the scheduler's SQLite (`buildkite_hook_key`, one row), then creates
the service with them in signature mode. The webhook check reads the key from there.

- **Why not the Secrets Store.** A Worker reads a store secret only through a binding, and the
  binding's secret must exist; but only the person sets store secrets with a value the Worker
  then reads. With the key in the store, the platform would need a two-step dance (create the
  secret by API, then land a binding to it). The key is a shared HMAC key that Buildkite already
  holds and returns in full on `GET …/services`, so keeping it in the scheduler's storage (only
  the Worker can read it, encrypted at rest) loses nothing.
- **Drift.** If the service is pigeon's but its token differs from the stored key (someone
  edited it), the step is `update {token}`, and the stored key wins.
- **Lost key.** If the row is lost, the next apply generates a new key and updates the service.
- The `BUILDKITE_WEBHOOK_TOKEN` slot, its binding on the old branch, and its row in
  `credentials_expiry` are dropped.

**Who calls it.** There is no policy action kind for "change an outside CI provider", and no
new rule is added. So the setup writes only from state that has already passed the gate:

| Trigger | Does | Gate |
| --- | --- | --- |
| A landing's deploy: `Lander::reconcile_buildkite(id, detail)`, its own async fn behind its own `land::Settings` field, called right after the portal planner's `sync_portal` in the Deploying arm (after `apply(JobEnded { passed: true })`) | `observe` → `plan` → `apply`. The outcome goes in the landing's detail, as the portal sync's does: `buildkite: 2 created, 3 ok` or `buildkite: blocked (why)`. **A failed setup never fails a landing.** The connector then reads as not healthy, and jobs go elsewhere. | `land-and-deploy` (allow) |
| `ci_buildkite_plan` (pigeon MCP, new, read-only) | `observe` → `plan`. Answers the steps, the token's missing scopes, its expiry, and the owned record. Never applies. | Reading; no action kind |
| The console's CI section (phase 5, optional) | "Apply now", with a passkey approval bound to the plan's hash: for the first run, or after the person fixed something in Buildkite. | The person's passkey |

The Lander's post-deploy step runs in the newly deployed code: a deploy resets Durable
Objects, and the Lander's next alarm runs the new version. So the state applied is always the
state of the code now live. This is checked live in phase 2.

### The portal server, made by the platform

So the token is typed once, on the secrets page, the setup also keeps a Cloudflare MCP server
in step: `POST`/`PUT /accounts/{account}/access/ai-controls/mcp/servers/buildkite` with
`id: "buildkite"`, `name: "Buildkite"`, `hostname: "https://mcp.buildkite.com/direct"`,
`auth_type: "bearer"`, `auth_credentials: <BUILDKITE_API_TOKEN>` and the marker in
`description`. The call uses a token minted for it alone with `MCP Portals Write`, as the portal
sync's does (`pigeon_core::mcp_portal`).

- Cloudflare never returns `auth_credentials`, so the setup records the SHA-256 of the token
  it last sent (`buildkite_owned.spec_hash`) and sends it again only when the stored token's
  hash differs: a rotated token reaches the portal at the next deploy.
- The same ownership rule holds: a server `buildkite` that exists and is not in the record is a
  conflict, never overwritten.
- The setup writes the **server only, never the portal**: a portal `PUT` with `servers`
  replaces its whole server mapping, which is the portal planner's and the person's. The
  person attaches the server to portal `pigeon` once, in the dashboard (§4).
- Once the server exists (phase 3), `buildkite` is added to `land::Settings::portal_servers` so it is synced like `pigeon`; before that, a sync of it would only fail.

### The connector, with the setup in place

- `available()` is healthy only if both secrets are set, the token holds the **runtime** scopes
  (`read_builds`, `write_builds`), and the last apply left the pipeline `ok`.
- *Deferred to after phase 2a (not built yet):* `offers()` stays declared, but `dispatch` refuses a queue the last apply did not confirm,
  with "queue pigeon-linux-medium is not set up in Buildkite (why)". That is an infrastructure
  failure, retried elsewhere once. For example, the Free plan refuses the 4X16 shape.
- The webhook check reads its key from the scheduler (`setup_key`); with no key yet it refuses
  every delivery, and the gateway still learns of builds by polling every two minutes.
- Everything else is as built: the OIDC exchange, the bootstrap and `steps_yaml`.

### Scopes for the one token

| Used by | Scopes |
| --- | --- |
| Connector, at runtime | `read_builds`, `write_builds` |
| Setup | `read_clusters`, `write_clusters`, `read_pipelines`, `write_pipelines`, `read_notification_services`, `write_notification_services` |

| Buildkite's MCP, for sessions | `read_build_logs`, `read_artifacts`, `read_suites`, `read_agents`, `read_user`, `read_organizations` (with the above) |

One token, fourteen scopes, made by an organization admin: the person's choice, one secret to
make and rotate. The connector only ever calls the build endpoints; `ci_buildkite_plan` names
any missing scope. Left out on purpose: `write_secrets` (cluster secrets), `read_job_env`,
`write_notification_services` for anything but the setup (MCP has no tool for it anyway), and
GraphQL.

**Expiry.** A Buildkite token defaults to **7 days** if no expiry is picked. The person picks
the longest offered. `credentials_expiry` already tracks `BUILDKITE_API_TOKEN` from
`GET /v2/access-token`'s `expires_at`. Buildkite's "Public Key" credentials (preview) never
expire: the person would register an RSA public key and pigeon would hold the private key. That
is a later option, not phase 1.

## 4. What the person still does by hand

Once, in this order.

1. **Buildkite organization.** If you don't have one: sign up at buildkite.com and create the
   organization. Note its slug, from `buildkite.com/<slug>`. Hosted agents need the
   organization's plan to allow them. The Free plan allows only `LINUX_AMD64_2X4`, so the medium
   queue is reported `blocked` until the plan allows 4X16.
2. **One API token.** Buildkite → Personal Settings → API Access Tokens → New API Access Token.
   - Description: `pigeon platform`.
   - Credential type: Token.
   - Organization Access: your organization.
   - Token Expiry: the longest offered.
   - REST API scopes: `read_builds`, `write_builds`, `read_clusters`, `write_clusters`,
     `read_pipelines`, `write_pipelines`, `read_notification_services`,
     `write_notification_services`, `read_build_logs`, `read_artifacts`, `read_suites`,
     `read_agents`, `read_user`, `read_organizations`. No GraphQL.
   - Allowed IP addresses: leave empty, since Workers' egress IPs are not fixed.
   - Create it and copy the value.

   You must be an organization admin, which you are in your own organization.
3. **Set it on pigeon.** Open `https://platform.pigeon.dev/access#credentials` (an agent can
   give you the exact link with `secrets_request {name: "BUILDKITE_API_TOKEN"}`). Set
   `BUILDKITE_API_TOKEN` to the value and approve with your passkey. Then tell the session your
   organization slug. It is not a secret; it goes into `settings.rs` in phase 2's landing.
4. **Buildkite in the portal** (phase 3, after a deploy has made the server). Zero Trust →
   Access controls → MCP Portals → MCP servers → `Buildkite` (id `buildkite`). A server made
   through the API has **no Access application**, and the dashboard shows it as broken ("The
   Access application for this portal was not found"): press **Create application**, and give
   it the Access policy `mcp-portal-owner`. Then MCP Portals → `pigeon` → Edit → Servers →
   **add `buildkite`**, every tool on. Sessions see `buildkite_*` tools through "Pigeon MCP" from
   their next start.

   This stays a dashboard step on purpose: the Access application and its policy decide who
   may use a server that holds the Buildkite write token, which is the person's boundary, and
   pigeon's code writes no Access state but its own `self_hosted` applications (the lesson of
   the Access incident). It is done once; the setup afterwards only keeps the server's URL and
   credential in step.

That's all. No cluster, queue, pipeline, steps YAML, trigger setting, webhook, webhook token or
portal credential is set up by hand, and the token is typed once.

## 5. Phases

| Phase | What | Who | Lands with |
| --- | --- | --- | --- |
| 1 | **Bind `BUILDKITE_API_TOKEN`**: the one binding left from `claude/ci-gateway` (its docs and queue table are already on main; the webhook binding is dropped). It lands **only after the person has set the token** (§4, step 3), because a binding to a store secret that does not exist may stop the deploy. No behaviour change: `BUILDKITE_ORG` stays `""`. | Agent; person first | `land` |
| 2 | **The setup.** Core: queue, cluster and notification-service REST calls, and pipeline create, read and update (`pigeon_core::buildkite`). Capability: `Declared`, `observe`, `plan`, `apply`, the `buildkite_owned` migration, the webhook token's generation, `ci_buildkite_plan`, the connector's queue confirmation, and the setup scopes in `ci_buildkite_plan`'s answer. Lander: the post-deploy step, added after `claude/mcp-portal-sync` lands, in coordination with its planner. Smoke: `plan` scenarios in `tests/smoke.rs` only (all-new, all-ok, drift, conflict by name, orphaned, missing scope), well under the 60 s. Settings: the person's slug in `BUILDKITE.org`. | Agent: AI Gateway executors for the core client and `plan`; the planner for `apply`, ownership and the Lander, reviewed line by line. Person: steps 1 to 3 before this lands. | `land`. No passkey: no policy or settings.json change. |
| 2a | **First apply, live.** Run `ci_buildkite_plan` before the landing and after it. Check: the cluster, queues and pipeline exist with the marker; a `ping` reaches `/ci/hooks/buildkite` and verifies; `ci_submit {job: smoke, connector: buildkite}` succeeds; `smoke-fail` fails; a second landing plans all `ok`. Confirm the open points: RS256 vs ES256, curl on the hosted image, DCR. | Agent | — |
| 3 | **Buildkite MCP behind the portal**, write tools on, bearer with the one token. The setup makes the Cloudflare server `buildkite` (§3) and adds it to `portal_servers`; the person attaches it to the portal (step 4). Check from a session: `buildkite_get_build` and `buildkite_read_logs` on a smoke build; a rotated token reaches the server at the next deploy. | Agent (code, landed with phase 2 or after); person (dashboard, step 4) | `land`, then dashboard |
| 4 | **Docs.** `ci-gateway.md` phase 3: "Needs from the person" becomes a pointer here, the status line is updated, and "Who does what" drops the Buildkite setup. `docs/proxies.md` notes the expiry row's new scopes. The README table gets this plan's row. | Agent | `land` |
| 5 | Optional: the console's "Apply now" and "Re-adopt" (passkey-bound); a split setup token; Public Key credentials; macOS queues with the iOS work. | Person decides | `land` |

**Coordination.** The portal planner (session `…S9tQyZ6y3cuBKdBc`) owns the Lander's
post-deploy portal sync and the settings generator. Phase 2's Lander step goes on top of their
landed sync, after a message to them. Phase 3's code writes the Cloudflare MCP *server* `buildkite`, never the portal; attaching it is the person's dashboard step. The portal's
API replaces the whole server mapping on any `PUT`, so pigeon's code never writes the portal.
This plan does not touch the generator.

**Rollback.**
- To stop using Buildkite: set `BUILDKITE.org = ""` and land. The connector reads as not
  configured and jobs go to Containers. The setup is skipped when `org` is empty. Nothing in
  Buildkite is deleted.
- To undo the setup: in Buildkite, delete the notification service, archive the pipeline and
  delete the cluster (it holds the queues); then revoke the token, or `DELETE
  /v2/access-token`. The `buildkite_owned` rows then read as missing, and the next apply would
  recreate them unless `org` is `""`.
- The portal: detach `buildkite` from the portal in the dashboard (sessions lose the tools at once); remove it from `portal_servers` and the setup in a landing, then delete the server in the dashboard.
- Any phase's code: revert the landing. The setup never deletes, so an old version leaves
  Buildkite as it was.

## As found

- **The first deploy's setup met the old scheduler** (`bYNS6ijvF7NRcFbB`): the Lander, already
  the new version, called `setup` on the CI scheduler, which answered "no operation `setup`":
  it was still the old version, having just finished the deploy job itself. The Lander now waits
  20 s and asks again, up to six times, when the scheduler does not know `setup` yet.
- **The second deploy met it again** (`fIjRWQBuxf6j0r8r`): this time the old scheduler knew
  `setup` and answered with its own settings, `BUILDKITE_ORG` still empty ("no Buildkite
  organization is set"). So the Lander now sends the commit it was built from
  (`PIGEON_COMMIT`, [`ci::BUILD`]); the scheduler refuses a call from another build and names its
  own in every answer, and the Lander waits (20 s, up to four minutes) until the answer is from
  its own build. Older schedulers, which name none, are waited out the same way.

- **The scheduler applies once per build itself.** A landing's post-deploy step runs in the
  Lander that was live before the deploy, so a fix to it takes effect one landing late. The
  scheduler now notes the build that last applied the setup (`setup_builds`); the first request
  to a new build's scheduler (the Lander's polls, a job) brings its alarm forward, and the alarm
  applies the setup once for that build. The Lander's step still runs and reports in the
  landing's detail; `ci_buildkite_plan` shows the last apply (`last_applied`) and the
  scheduler's build. The setup is idempotent, so the two never conflict.
- **The first real apply** (`linked-bindings`, `80822b8`) failed at the cluster, so nothing was
  made: Buildkite accepts only its own emoji names and refused `:pigeon:` (422). The cluster
  now has no emoji, and the steps' labels none either. Queues, the webhook and the portal server
  no longer stop the run when one of them fails (a plan refusing the 4X16 shape still gets the
  pipeline and webhook); a cluster or pipeline failure still does.
- **The build check compared the wrong builds** (`hyarAqBBX6VeHbsw`): its post-deploy step ran
  in the old Lander, which expected its own (old) build and so accepted the old scheduler. The
  Lander now expects the commit the landing deployed (`commit_sha`), whichever build it is
  itself.
- **The first plan on the new code** (`ci_buildkite_plan`, through `call`): no missing scopes;
  create cluster, both queues, pipeline and portal server; and a **conflict** on the webhook: a
  notification service to `/ci/hooks/buildkite` exists that pigeon did not make (the one made by
  hand for the old plan). As designed, it is left alone and blocks nothing else; the person
  deletes it in Buildkite (Organization Settings → Notification Services), and the next deploy
  creates pigeon's.
- **The third deploy's setup reached Buildkite** (`EObYbdM_9HTYjPeJ`) and stopped while
  observing, before planning, so nothing was created: Buildkite lists notification services as
  `{items, links}`, cursor-paginated, not as a bare array like clusters and queues
  (`rest_api/organizations/notification_services.md`). The REST client now reads both shapes and
  follows `links.next`, on Buildkite's API only, for at most ten pages. The token's scopes were
  not the problem (a missing scope answers 403). The services' answers name `provider` as an
  object (`{id, name}`); the plan checks no `provider`, so that difference is harmless.

## 6. Open points

Settled live in phase 2a:
- The API accepted the pipeline with the GitHub mirror as its `repository`, no GitHub app, and
  `trigger_mode: "none"`.
- The OIDC exchange works as built (RS256), and the hosted Linux image has curl: `smoke` on
  Buildkite ran in about 10 s from start to report.
- The plan allows `LINUX_AMD64_4X16`: both queues were created.
- No session needed a Buildkite host; the Worker and the portal reach Buildkite themselves.
- The secret binding: never tried before its secret existed; the token was set first.

Still open, for phase 3:
- Settled: a server created through the API gets **no** Access application; the person creates
  it once in the dashboard (§4, step 4).
- Whether `/direct` accepts the bearer token as the portal forwards it.

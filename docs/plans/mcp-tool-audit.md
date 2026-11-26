# MCP tool audit: what pigeon serves, and what should go

Status: **audited; one code landing for two fixes; no removals** (planner run `A0BAKsX7A3S0`).

The person asked for an audit of every MCP tool pigeon serves. A tool is removed only if it is
**(a)** unable to work any more, **(b)** fully superseded with no dependents left, or **(c)** a
one-time setup whose re-run is a no-op or harmful. "Infrequently used" is not a reason.
Break-glass, owner-only, passkey-gated and recovery tools stay, and so do the Buildkite upstream
tools.

## Result

| Verdict | Count |
|---|---|
| keep | 132 |
| fix | 2 |
| remove | 0 |
| proposed removals (need the person's passkey) | 0 |

134 tools were read (the brief said about 133). Nothing meets (a), (b) or (c). Two tools have an
argument or a message that is wrong. Both are small fixes.

**Later (26 November):** the person retired the `deploys` capability (`deploy_token`,
`revoke_api_token`) as superseded; see "Break-glass" in `tool-approvals.md`. The counts above are
the audit's, as it was.

## How it was checked

- Every `Tool` declaration in `src/capabilities/src`, and the tool lists of `src/ci`,
  `src/cloud`, `src/models`, `src/sites` and `src/ai-agents/src/service.rs`, was read.
- **(a)** `src`, `tests`, `ios` and `docs` were searched for the retired `pigeon` gateway, the
  slot `pigeon_workers_ai_token` and the old OpenAI slot `pigeon_openai_default`.
  - The retired gateway is still declared as `GatewayKind::Retired`
    (`src/model-gateway/src/lib.rs:40`) so old logs stay readable. `models_logs` and
    `models_usage` need that.
  - `pigeon_workers_ai_token` is gone from the code, and a smoke check keeps it gone
    (`tests/smoke.rs:2065`).
  - The old OpenAI slot is gone too (`tests/smoke.rs:887`). Only prose in
    `docs/plans/model-providers.md` still names it.
  - No tool depends on any of the three.
- **(b)** For each pair that looks alike, the dependents were searched. None is fully
  superseded.
  - `models_daily` feeds `cost_daily` (`src/capabilities/src/cost.rs:2`, `:222`).
  - `models_cost` feeds the agent budget (`src/ai-agents/src/budget.rs`, `object.rs:2057`).
  - `approvals_list` and `approvals_status` answer different questions. Both are used by
    `ios/Pigeon/Views/NeedsYouView.swift` and `tests/smoke/policy.rs`.
- **(c)** The five candidates are read below.
- **Stale descriptions.** Every tool name in a description or error message was compared with
  the declared set. None names a tool that does not exist.
- **Enums.** Every `"enum"` in a tool schema was read against the code behind it.

## The (c) candidates

| Tool | Verdict | Why |
|---|---|---|
| `setup_proxy_host` | keep | A re-run attaches the host again and nothing else (`PublicHost` has no Access retirement since it was removed as dead code). It is the documented rollback of the proxy cutover (`settings.rs:194`) and is called in `tests/smoke/proxy.rs:927` and `tests/smoke/core.rs:575`. Not a no-op: its `worker` argument moves the host. |
| `setup_source` | keep | An ensure of repository, queue, consumer and subscription (`src/capabilities/src/source.rs:87`). On a healthy platform it changes nothing. If a queue or subscription is lost, it is the only way to make them again. That is recovery, not a finished migration. |
| `users_bootstrap` | keep | It refuses once a standard user exists (`src/capabilities/src/users.rs:1321`). But `users_remove` stops only a user removing themselves (`users.rs:1391`), so a service caller can remove the last person. Then `users_bootstrap` is the only way back in. A recovery tool. |
| `models_setup` | keep | Used to attach each new provider key. `models_providers` only reads (`src/models/src/lib.rs:1498`, `:1504`). Error messages tell the person to run `models_setup` (`src/core/src/ai_gateway.rs:280`). |
| `ai_agents_setup` | keep | Not one-time. It says whether each of the two service users is linked and gives a fresh link to approve when not (`src/ai-agents/src/service.rs:347`). It is how a lost link is made again. |

Also looked at, not on the candidate list. `workers_setup` keeps an existing key, but
`src/capabilities/src/split.rs:112` and `:115` tell the person to run it when `CALLER_KEY` is
lost. `linked_setup` is safe to run twice, and every instance link depends on its OAuth client
(`src/capabilities/src/linked.rs:624`). `setup_platform` is passkey-gated
(`.pigeon/policy.toml:85`).

## Fixes

| Tool | Problem | Fix |
|---|---|---|
| `models_billing` | `provider` is an enum of every provider, but only `anthropic` has a billing source (`src/models/src/lib.rs:1402`). The other values always answer "has no billing source". | Offer only providers that have a billing source. |
| `ai_agents_register` | The error for a bad `role` says "planner or executor", but `mechanical` is accepted and listed in the schema (`src/ai-agents/src/service.rs:318`, `:322`). | Say "planner, executor or mechanical". |
| Docs only | `docs/plans/model-providers.md` (lines 193, 278, 285) names the old slot `pigeon_openai_default`. The slot is `pigeon-openai_openai_default`. | A short note in that plan. |

## Planned landings

1. This document, docs-only.
2. One code landing: the two fixes, with one smoke scenario for `models_billing`
   (`models_billing_provider_enum` in `tests/smoke/models.rs`) and a check of the `ai_agents_register`
   message. It affects the models and ai-agents Workers.
3. A docs-only note for the stale slot name.

No removal means no change to architecture coverage, iOS references or the AI agents' tool
lists.

## Tool table

Evidence is the line that declares the tool. "none met" means none of (a), (b), (c) applies.

| Tool | Verdict | Criterion | Evidence | Replacement |
|---|---|---|---|---|
| `call` | keep | none met | `src/capabilities/src/dispatch.rs:48` | none |
| `apple_status` | keep | none met | `src/capabilities/src/apple.rs:815` | none |
| `apns_register` | keep | none met | `apple.rs:923` | none |
| `apns_unregister` | keep | none met | `apple.rs:934` | none |
| `apns_status` | keep | none met | `apple.rs:940` | none |
| `apns_send` | keep | none met | `apple.rs:950` | none |
| `ios_releases` | keep | none met | `apple.rs:1513` | none |
| `approvals_list` | keep | none met | `src/capabilities/src/approvals.rs:517` | none |
| `approvals_status` | keep | none met | `approvals.rs:523` | none |
| `architecture_describe` | keep | none met | `src/capabilities/src/architecture.rs:63` | none |
| `mcp_conformance` | keep | none met | `architecture.rs:79` | none |
| `changelog` | keep | none met | `src/capabilities/src/changelog.rs:295` | none |
| `changelog_update` | keep | none met | `changelog.rs:305` | none |
| `deploy_token` | removed later (26 Nov) | (b): superseded by the proxy credential and `land`; break-glass is the person's own Cloudflare login | was `src/capabilities/src/deploys.rs` | none |
| `revoke_api_token` | removed later (26 Nov) | revoked only `deploy_token`'s tokens | was `deploys.rs` | none |
| `github_status` | keep | none met | `src/capabilities/src/github.rs:343` | none |
| `containers_images` | keep | none met | `src/capabilities/src/images.rs:28` | none |
| `instances_list` | keep | none met | `src/capabilities/src/instances.rs:738` | none |
| `instances_tabs` | deleted (console decommission, 2026-11-25) | replaced by `console_manifest` | none | none |
| `instance_plan` | keep | none met | `instances.rs:747` | none |
| `instance_apply` | keep | none met | `instances.rs:761` | none |
| `instance_subdomains` | keep | none met | `instances.rs:773` | none |
| `instance_propose` | keep | none met | `instances.rs:778` | none |
| `land` | keep | none met | `src/capabilities/src/land.rs:381` | none |
| `land_status` | keep | none met | `land.rs:409` | none |
| `land_list` | keep | none met | `land.rs:425` | none |
| `land_watch` | keep | none met | `land.rs:438` | none |
| `work_status` | keep | none met | `land.rs:450` | none |
| `land_cancel` | keep | break-glass | `land.rs:467` | none |
| `portal_sync` | keep | none met | `land.rs:477` | none |
| `portal_oauth_redirects` | keep | none met | `land.rs:483` | none |
| `portal_oauth_redirects_apply` | keep | none met | `land.rs:501` | none |
| `linked_setup` | keep | none met | `src/capabilities/src/linked.rs:624` | none |
| `linked_check` | keep | none met | `linked.rs:630` | none |
| `linked_scopes` | keep | none met | `linked.rs:644` | none |
| `linked_status` | keep | none met | `linked.rs:651` | none |
| `oauth_clients` | keep | none met | `src/capabilities/src/oauth.rs:413` | none |
| `oauth_revoke` | keep | passkey-gated | `oauth.rs:417` | none |
| `oauth_issue_token` | keep | none met | `oauth.rs:426` | none |
| `observability_monitors_plan` | keep | none met | `src/capabilities/src/observability.rs:658` | none |
| `observability_monitors_apply` | keep | none met | `observability.rs:680` | none |
| `observability_summary` | keep | none met | `observability.rs:690` | none |
| `observability_errors` | keep | none met | `observability.rs:701` | none |
| `tokens_status` | keep | none met | `observability.rs:717` | none |
| `cost_daily` | keep | none met | `observability.rs:730` | none |
| `passkeys_list` | keep | passkeys tool | `src/capabilities/src/passkeys.rs:366` | none |
| `passkeys_remove` | keep | passkey-gated | `passkeys.rs:374` | none |
| `policy_check` | keep | none met | `src/capabilities/src/policy.rs:122` | none |
| `secrets_list` | keep | none met | `src/capabilities/src/secrets.rs:347` | none |
| `credentials_expiry` | keep | none met | `secrets.rs:353` | none |
| `secrets_request` | keep | none met | `secrets.rs:360` | none |
| `site_publish` | keep | none met | `src/capabilities/src/site.rs:166` | none |
| `site_passkey` | keep | none met | `src/capabilities/src/site_passkeys.rs:106` | none |
| `site_render` | keep | none met | `src/sites/src/lib.rs:116` | none |
| `site_deploy` | keep | none met | `src/sites/src/lib.rs:139` | none |
| `source_token` | keep | break-glass, passkey-gated | `src/capabilities/src/source.rs:257` | none |
| `source_tokens` | keep | none met | `source.rs:270` | none |
| `source_revoke` | keep | revoke tool | `source.rs:274` | none |
| `branch_patch` | keep | break-glass | `source.rs:283` | none |
| `setup_source` | keep | (c) not met | `source.rs:301` | none |
| `source_mirror` | keep | none met | `src/platform/src/settings.rs:216` | none |
| `setup_platform` | keep | passkey-gated | `settings.rs:187` | none |
| `setup_proxy_host` | keep | (c) not met | `settings.rs:197` | none |
| `workers_setup` | keep | (c) not met | `src/capabilities/src/split.rs:832` | none |
| `manifest_refresh` | keep | none met | `split.rs:838` | none |
| `storage_accounts` | keep | none met | `src/capabilities/src/storage.rs:413` | none |
| `storage_list_buckets` | keep | none met | `storage.rs:417` | none |
| `storage_list_objects` | keep | none met | `storage.rs:423` | none |
| `storage_head_object` | keep | none met | `storage.rs:434` | none |
| `storage_get_object` | keep | none met | `storage.rs:440` | none |
| `users_whoami` | keep | none met | `src/capabilities/src/users.rs:1487` | none |
| `users_list` | keep | none met | `users.rs:1491` | none |
| `users_people` | keep | none met | `users.rs:1495` | none |
| `users_bootstrap` | keep | (c) not met, recovery | `users.rs:1499` | none |
| `users_invite` | keep | none met | `users.rs:1506` | none |
| `users_enrol` | keep | none met | `users.rs:1517` | none |
| `users_link` | keep | none met | `users.rs:1527` | none |
| `users_unlink` | keep | none met | `users.rs:1534` | none |
| `users_remove` | keep | passkey-gated | `users.rs:1540` | none |
| `users_rename` | keep | none met | `users.rs:1547` | none |
| `capabilities_versions` | keep | none met | `src/capabilities/src/versioning.rs:126` | none |
| `capabilities_upgrade_plan` | keep | none met | `versioning.rs:141` | none |
| `ingest_plan` | keep | none met | `src/capabilities/src/ingest.rs:903` | none |
| `ingest_start` | keep | none met | `ingest.rs:922` | none |
| `ingest_status` | keep | none met | `ingest.rs:928` | none |
| `ingest_report` | keep | none met | `ingest.rs:934` | none |
| `ingest_stop` | keep | none met | `ingest.rs:940` | none |
| `ci_pool` | keep | Buildkite upstream | `src/ci/src/lib.rs:1039` | none |
| `ci_route` | keep | Buildkite upstream | `ci/src/lib.rs:1044` | none |
| `ci_submit` | keep | Buildkite upstream | `ci/src/lib.rs:1060` | none |
| `ci_status` | keep | Buildkite upstream | `ci/src/lib.rs:1078` | none |
| `ci_jobs` | keep | Buildkite upstream | `ci/src/lib.rs:1086` | none |
| `ci_cancel` | keep | Buildkite upstream | `ci/src/lib.rs:1100` | none |
| `ci_commit` | keep | Buildkite upstream | `ci/src/lib.rs:1108` | none |
| `ci_buildkite_plan` | keep | Buildkite upstream | `ci/src/lib.rs:1118` | none |
| `cloud_status` | keep | none met | `src/cloud/src/tools.rs:17` | none |
| `cloud_tokens_list` | keep | none met | `cloud/src/tools.rs:18` | none |
| `cloud_tokens_plan_create` | keep | none met | `cloud/src/tools.rs:19` | none |
| `cloud_tokens_plan_delete` | keep | none met | `cloud/src/tools.rs:20` | none |
| `cloud_buckets_list` | keep | none met | `cloud/src/tools.rs:21` | none |
| `cloud_objects_list` | keep | none met | `cloud/src/tools.rs:22` | none |
| `cloud_object_head` | keep | none met | `cloud/src/tools.rs:23` | none |
| `cloud_buckets_plan_create` | keep | none met | `cloud/src/tools.rs:24` | none |
| `cloud_buckets_plan_delete` | keep | none met | `cloud/src/tools.rs:25` | none |
| `cloud_apply` | keep | none met | `cloud/src/tools.rs:26` | none |
| `models_setup` | keep | (c) not met | `src/models/src/lib.rs:1498` | none |
| `models_providers` | keep | none met | `models/src/lib.rs:1504` | none |
| `models_run` | keep | none met | `models/src/lib.rs:1509` | none |
| `models_submit` | keep | none met | `models/src/lib.rs:1531` | none |
| `models_status` | keep | none met | `models/src/lib.rs:1561` | none |
| `models_result` | keep | none met | `models/src/lib.rs:1573` | none |
| `models_cancel` | keep | none met | `models/src/lib.rs:1586` | none |
| `models_logs` | keep | none met | `models/src/lib.rs:1597` | none |
| `models_cost` | keep | (b) not met | `models/src/lib.rs:1617` | none |
| `models_daily` | keep | (b) not met | `models/src/lib.rs:1639` | none |
| `models_billing` | fix | enum can fail | `models/src/lib.rs:1650` | none |
| `models_usage` | keep | none met | `models/src/lib.rs:1664` | none |
| `ai_agents_start` | keep | none met | `src/ai-agents/src/service.rs:436` | none |
| `ai_agents_estimate` | keep | none met | `service.rs:461` | none |
| `ai_agents_status` | keep | none met | `service.rs:473` | none |
| `ai_agents_stop` | keep | none met | `service.rs:492` | none |
| `ai_agents_pause` | keep | none met | `service.rs:505` | none |
| `ai_agents_resume` | keep | none met | `service.rs:512` | none |
| `ai_agents_inbox` | keep | none met | `service.rs:517` | none |
| `ai_agents_answer` | keep | none met | `service.rs:534` | none |
| `ai_agents_extend` | keep | none met | `service.rs:546` | none |
| `ai_agents_note` | keep | none met | `service.rs:565` | none |
| `ai_agents_register` | fix | wrong message | `service.rs:582` | none |
| `ai_agents_report` | keep | none met | `service.rs:600` | none |
| `ai_agents_handoff` | keep | none met | `service.rs:618` | none |
| `ai_agents_setup` | keep | (c) not met | `service.rs:630` | none |
| `ai_agents_models` | keep | none met | `service.rs:638` | none |
| `ai_agents_config` | keep | none met | `service.rs:670` | none |
| `ai_agents_compare` | keep | none met | `service.rs:688` | none |

Line numbers are those of `main` at `efad0b6`. If a line has moved, search for the tool name.

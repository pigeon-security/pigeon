# Cloudflare Observability for every pigeon Worker

Status: (a), (b) and (c) landed and deployed (core at bbbdb96 serves all three tools). Still on
branches, not landed: `spike_after_deploy` and the CI price, and the console's Platform
section (see the latest Log entry). The live checks in the handoff entry have not been run yet.
The person approves each landing, and does the follow-ups listed under "For the person".

The request: turn on Cloudflare Observability (logging, traces, analytics, monitors, usage) for
every Worker, and add one tool that gives high-level error stats and one view of cost.

Sources: developers.cloudflare.com pages read on 2026-11-22. The date after each link is the
page's own `dateModified`.

- Workers Logs, its settings, sampling, limits and pricing:
  https://developers.cloudflare.com/workers/observability/logs/workers-logs/ (2026-11-18)
- Wrangler `[observability]` and container `observability`:
  https://developers.cloudflare.com/workers/wrangler/configuration/#observability (2026-11-12)
- Workers Traces: https://developers.cloudflare.com/workers/observability/traces/ (2026-11-12);
  its spans and attributes:
  https://developers.cloudflare.com/workers/observability/traces/spans-and-attributes/
  (2026-10-16); known limitations (open beta):
  https://developers.cloudflare.com/workers/observability/traces/known-limitations/ (2026-06-16)
- Cloudflare Observability pricing, from 2026-12-01:
  https://developers.cloudflare.com/observability/pricing/ (2026-11-11)
- Workers metrics, invocation statuses:
  https://developers.cloudflare.com/workers/observability/metrics-and-analytics/ (2026-11-12)
- GraphQL Analytics API, Workers metrics:
  https://developers.cloudflare.com/analytics/graphql-api/tutorials/querying-workers-metrics/
  (2026-04-23); token permission `Account Analytics Read`:
  https://developers.cloudflare.com/analytics/graphql-api/getting-started/authentication/api-token-auth/
  (2026-04-23)
- SQL API and its datasets (`logs.workersLogs`):
  https://developers.cloudflare.com/analytics/sql-api/ (2026-10-02),
  https://developers.cloudflare.com/analytics/sql-api/datasets/ (2026-10-27), and its Workers
  binding: https://developers.cloudflare.com/analytics/sql-api/workers-binding/ (2026-10-02)
- Workers Observability query API (`POST /accounts/{id}/workers/observability/telemetry/query`):
  https://developers.cloudflare.com/api/resources/workers/subresources/observability/subresources/telemetry/methods/query/
- Monitors: https://developers.cloudflare.com/notifications/monitors/ (2026-10-21); platform
  notifications: https://developers.cloudflare.com/notifications/notification-available/
  (2026-10-21); the policies API:
  https://developers.cloudflare.com/api/resources/alerting/subresources/policies/methods/create/
- Billable usage (alpha):
  https://developers.cloudflare.com/api/resources/billing/subresources/usage/methods/get_account_usage_v1/
- Workers pricing: https://developers.cloudflare.com/workers/platform/pricing/ (2026-11-18);
  Containers pricing: https://developers.cloudflare.com/containers/pricing/ (2026-11-13);
  Analytics Engine pricing: https://developers.cloudflare.com/analytics/analytics-engine/pricing/
  (2026-04-23)
- Tail request redaction (for what logs and traces may hold):
  https://developers.cloudflare.com/workers/runtime-apis/handlers/tail/ (2026-06-15)
- API token permission groups:
  https://developers.cloudflare.com/fundamentals/api/reference/permissions/ (2026-11-15)

## What Cloudflare offers

| Piece | What it is | How it is turned on or read | Notes |
| --- | --- | --- | --- |
| Workers Logs | `console.*` lines and one invocation log per request, kept 7 days, queryable in the dashboard and by API | `[observability] enabled = true`; `head_sampling_rate` 0 to 1 (default 1); `[observability.logs] invocation_logs = false` drops the per-request log | 5 billion logs per account per day, then 1% sampling for the rest of the day. One log is at most 256 KB. |
| Invocation logs | One event per invocation; its message is `<Method> <URL>` for fetch | On with logs unless `invocation_logs = false` | The URL is kept whole, query included. |
| Workers Traces | Spans for the handler, `fetch` subrequests, bindings; open beta | `[observability.traces] enabled = true`, own `head_sampling_rate` (default 1) | Spans hold `url.full` and `url.query`, and only a fixed set of headers (`content-type`, `accept`, `accept-encoding`, ...): never `authorization` or `cookie`, never bodies (sizes only). Names may still change. |
| Container logs | stdout and stderr of a Worker's containers | A `[[containers]]` block's `observability`; **if it is absent, the top-level `[observability]` applies** | CI job output and agent sandboxes would be kept unless turned off. |
| Issues | Groups recurring failures | `[observability.issues] enabled = true` | Not asked for; not turned on here. |
| Metrics | Requests, errors (exception, exceeded resources, internal error), subrequests, CPU and wall time quantiles, per script | Always on. GraphQL `workersInvocationsAdaptive`, permission `Account Analytics Read` | Up to one month per query, three months back. |
| Query API | Workers Logs and Traces by key (`$metadata.service`, `$metadata.error`, `$metadata.message`, `$metadata.level`) with counts, group-bys and percentiles | `POST /workers/observability/telemetry/query`, view `calculations` | Accepts only `Workers Observability Write`: there is no read-only permission for it. |
| SQL API | SQL over datasets, `logs.workersLogs` among them | `POST /client/v4/analytics/sql`, `Account Analytics Read` (some datasets ask more); or an `analytics` Workers binding with no token | Column names of `logs.workersLogs` are only given by the introspection endpoint, not in the docs. |
| Analytics Engine | A Worker writes its own data points | A binding | Paid: 10 M points written and 1 M read queries a month included. Not needed: the metrics above cover requests and errors. |
| Monitors | A SQL API query on a schedule: threshold, anomaly or SLO, to email, webhook or PagerDuty | **Dashboard only** (Alerts > Monitors); no API is documented | Sampling can hide single events, so a monitor on rare errors wants logs at 100%. |
| Platform notifications | Fixed alert types, by API: `alerting/v3/policies`, `Notifications Write` | Alert types include `workers_observability_alert`, `workers_observability_real_time_issue`, `billing_usage_alert`, `billing_budget_alert` | Filters per type come from `GET alerting/v3/available_alerts` (`Notifications Read`). |
| Billable usage | Per product: quantity, unit, billed and list cost, in the FinOps FOCUS shape | `GET /accounts/{id}/billable-usage` (alpha), current billing period by default | Rows also carry the billing account's name: never shown. |
| Spending limit | Blocks new Worker invocations past a spend | Dashboard or `cf` CLI | An account setting: the person's choice. |

### Pricing and quotas

Until 2026-11-30, Workers Logs on Workers Paid: 20 million log events a month included, then
$0.60 a million. From 2026-12-01, Cloudflare Observability pricing applies to Workers Logs,
Workers Traces, container logs, AI Gateway logs and Issues together: 50 GB ingested and
10 GB-month stored a billing cycle included, then $0.25 a GB ingested and $0.10 a GB-month.
With 7-day retention, storage is about ingestion × 7 / 30, so storage charges start at about
43 GB ingested a cycle. Metrics, the GraphQL API and the SQL API cost nothing extra.

For the rest of the cost view: Workers Paid includes 10 M requests ($0.30 a million after) and
30 M CPU ms ($0.02 a million after); Containers include 25 GiB-hours of memory, 375 vCPU-minutes
and 200 GB-hours of disk a month ($0.0000025 a GiB-second, $0.000020 a vCPU-second,
$0.00000007 a GB-second after).

### What it will cost pigeon

pigeon's traffic is small: core's cron runs every minute (about 43,200 invocations a month),
plus MCP calls, console loads, CI and landing alarms, and service-binding calls between the six
Workers. Even at 1 million invocations a month across all six, with invocation logs off and
about three `console` lines of about 1 KB each per invocation, logs are about 3 GB a month, and
traces at 10% of invocations, about ten 0.5 KB spans each, about 0.5 GB. That is under a tenth
of the included 50 GB and about 0.8 GB-month of the included 10 GB-month: **an expected $0 a
month above the Workers Paid subscription**. Before 2026-12-01 the same is about 3 million log
events, inside the 20 million included. The risk is container output, which is why container
logs stay off. `observability_summary` reports the billed figure once a month has run.

## What each Worker turns on

One block, the same in every Worker's `wrangler.toml` except core's `traces.enabled` (wrangler
has no include, so the block is copied and the smoke suite keeps the copies equal):

```toml
# Cloudflare Observability (docs/plans/observability.md). Every Worker carries this same block;
# the smoke scenario core_observability_on_every_worker fails when one does not.
[observability]
enabled = true
head_sampling_rate = 1

[observability.logs]
# The per-request log keeps each URL whole, and some URLs carry one-use codes
# (/auth/enrol?code=, OAuth callbacks): off. console lines are still kept.
invocation_logs = false

[observability.traces]
enabled = true
head_sampling_rate = 0.1
```

and, in each `[[containers]]` block, container logs off, since the top-level block would
otherwise apply to them:

```toml
[containers.observability]
enabled = false
```

The choices:

- **Logs at 100%.** Volume is small, and a monitor on rare errors needs every one.
- **No invocation logs.** Each holds the request URL whole. `/auth/enrol?code=` (an enrolment
  link), `/linked/callback?code=` and OAuth redirects carry codes in the query. Request
  headers are not documented as kept or redacted in Workers Logs, so they are not relied on.
  Errors are still counted by the metrics, and pigeon's own `console_error!` lines carry the
  messages.
- **Traces at 10%, except core.** Traces never hold `authorization`, cookies or bodies, but
  they hold `url.query` of the incoming request. Only core (`pigeon-platform`) serves the
  enrolment and OAuth URLs, so core's block sets `[observability.traces] enabled = false` until
  those codes leave the query string (a follow-up). The five other Workers trace at 10%.
- **Container logs off** on core (the CI runners) and `pigeon-ai-agents` (`Sandbox`): job and
  agent output may hold what the CI output redaction (`src/ci/src/output.rs`) misses, and it is
  the one source that could be large.
- **No Logpush, no tail consumers, no Issues.** None was asked for.
- **What pigeon logs.** Logs were only seen live before; now they are kept 7 days. Slice (a)
  reads every `console_*!` call and keeps people's names and emails out of them: the proxy's
  audit line names the caller by credential and user id, not by label and person name.

Deploys: wrangler sends the settings in the upload and in `PATCH .../script-settings`, and a
container's `observability` in the application `PATCH`. All three are already in the deploy
proxy's allowlist (`src/proxies/src/cf_proxy.rs`, `SETTINGS_KEYS` has `observability`), so
**no allowlist or stand-in change is needed**. The `allowlist` CI job proves it on the branch.

The coverage check: smoke scenario `core_observability_on_every_worker` reads `ci/workers.toml`
(the map every lane is built from), opens each `[[worker]]`'s `dir/wrangler.toml` and fails
unless it has the block above (logs on at a rate in (0, 1], invocation logs off, traces set
either way), with traces off only where its table of exceptions says (core, with the reason),
and every `[[containers]]` block has container logs off. A new Worker added to the map without
the block fails the gate.

## How core reads it

A new capability in core, `observability` (`src/capabilities/src/observability.rs`), changes
nothing in `src/core` and needs no new secret. Each call mints one token from the parent token
(`pigeon_core::tokens::Scoped::open_for`, 300 s), uses it, and revokes it:

| Permission | For | Read-only |
| --- | --- | --- |
| `Account Analytics Read` | GraphQL `workersInvocationsAdaptive`: requests, errors by status, CPU and wall time p50/p99, per script | yes |
| `Workers Observability Write` | The query API: top error messages per Worker (`$metadata.level = error`, grouped by `$metadata.service` and `$metadata.message`/`$metadata.error`) | no: the only permission the endpoint accepts; the token is used for that one `POST` only and lives five minutes |
| `Billing Read` | `GET /billable-usage` for the current period, per product | yes; it also reads the billing profile, so only whitelisted fields are kept |

When a permission group name is not found or a source refuses, the tool answers the rest and
says which source is missing and why. Dropping `Workers Observability Write` for the SQL API on
`logs.workersLogs` (`Account Analytics Read` only) is a follow-up once that dataset's column
names are read from the live introspection endpoint.

## The tool: `observability_summary`

Read-only. Arguments: `hours` (default 24, at most 168, the logs' retention). Answers:

- `window`: from and to, UTC.
- `workers`: one entry per Worker in `ci/workers.toml` (compiled in), by script name:
  `requests`, `errors`, `error_rate`, `by_status` (exception, exceeded resources, internal
  error), `cpu_ms_p50`/`p99`, `wall_ms_p50`/`p99` (source: metrics); `top_errors` (at most 5,
  message cut to 200 characters, from logs); `deploys` in the window (the Lander's deploy id,
  landing id, commit, state, finished time); `spike_after_deploy` when the error rate in the
  hour after a deploy is over twice the window's rate.
- `cost`, every line tagged `billed` or `estimated`, with its source:
  - Cloudflare, billed: the current billing period's billable usage per product (service name,
    quantity, unit, billed cost); never the billing account's name or ids.
  - Workers, estimated: the window's requests and CPU ms at list price, before included usage.
  - Models, estimated by the AI Gateway: `models_usage` for the window.
  - AI agent runs, metered by pigeon: run spend from the AI agents Worker for runs in the
    window.
  - CI, estimated: Containers time of CI jobs in the window from the CI scheduler, at the
    instance size's list price; Buildkite: not available (Buildkite's API has no cost data);
    said so.
  - `totals`: estimated total for the window; billed total for the period, kept apart.
- `unavailable`: each source that failed, and why.

No personal details or secrets: no user names or emails, no URLs with queries, no tokens.
Error messages are pigeon's own `console_error!` text, which slice (a) keeps clean.

The console: the Cost tab gains a "Platform" section with the same data, from the cost
snapshot (10 minutes), when its renderer is small; otherwise that is a follow-up.

## Monitors

Cloudflare's Monitors (SQL threshold, anomaly, SLO) are documented for the dashboard only, so
pigeon cannot create them by API; the platform notification policies can be. Slice (c):

- `observability_monitors_plan`: reads the account's available alert types and existing
  policies (`Notifications Read`), and plans the missing ones, by name `pigeon: ...`:
  - `workers_observability_alert` and `workers_observability_real_time_issue` for pigeon's
    scripts, when the available alerts allow it;
  - `billing_usage_alert` for Workers, Containers and Cloudflare Observability ingestion at
    thresholds in the plan (twice the expected month);
  - delivery by email to the signed-in person's address (shown to them on the approval, not in
    tool output).
  Like `instance_plan`, a plan with changes asks the person's approval on the console.
- `observability_monitors_apply {plan}`: once approved, creates exactly the planned policies
  (`Notifications Write`), touching only policies named `pigeon: ...`; never deletes.
- For the person, in the dashboard (Alerts > Monitors), the monitors pigeon cannot create, with
  the SQL to paste: error rate per Worker over 5 minutes above 5% (SLO 99%), and any
  `console_error!` line from the Lander or the CI scheduler in 15 minutes.

## Slices

| Slice | Branch | Changes | Lanes |
| --- | --- | --- | --- |
| (a) | executor branch, from this plan | the block in six `wrangler.toml` files, container logs off, the smoke scenario, personal details out of log lines | every Worker |
| (b) | executor branch, from this plan | `observability` capability, the tool, a Lander `deploys` read, the Cost section | core |
| (c) | executor branch, from (b) | monitors plan and apply, the dashboard monitors written up | core |

Each is harmless until reviewed: (a) only keeps logs; (b) is read-only; (c) creates nothing
until the person approves a plan.

## For the person

- (a), (b) and (c) are landed. Approve the part-3 landings (core only).
- Apply the monitors plan on the console only after the live shape check (Log, open item 1).
- In the dashboard, create the Monitors listed above, and add email delivery to the
  `pigeon: ...` notification policies that (c) creates (pigeon holds no person's email).
- Optional: a Workers spending limit (an account setting).
- Optional: if the parent token cannot grant `Account Analytics Read`, `Workers Observability
  Write`, `Billing Read`, `Notifications Read` or `Notifications Write`, the tool lists that
  source as unavailable; granting them is an account setting for the person.
- Later: move enrolment and OAuth codes out of URL queries, then turn core's traces on; replace
  `Workers Observability Write` with the SQL API once its columns are confirmed.

## Log

- 2026-11-22: planned.
- 2026-11-22: slice (b), first round, on `claude/ai-agent-QVk5DseSkLN4` @ ac183ca (executor
  QVk5DseSkLN4): `observability_summary` in `src/capabilities/src/observability.rs` and the
  Lander's read-only `deploys_since` op.
- 2026-11-23: planner QrV7alyFFsv6 took over from Zzn8ssZs4JsC (its run ended on a deploy).
  Review of ac183ca, CI `test` green (job bHxZW1Umh1K7JCGkOVaWK2): the Lander op, the tool's
  registration and the folds read right; findings, sent to a second round of (b):
  1. The permission consts are not named `*PERMISSION*`, so `architecture_describe` misses them.
  2. One token for all three permissions: a missing group drops logs and billing together, and
     a failed analytics mint fails the whole tool, deploys and model cost included. Mint per
     source and degrade.
  3. `fold_billing` sums `ConsumedQuantity` across units of one service (requests plus CPU ms).
  4. Model cost drops `models_usage`'s `truncated`.
  5. Not built: AI agent run spend, CI Containers time, `spike_after_deploy`, the console
     section. The Log line said the smoke suite does not link the capabilities crate; it does
     (`--test smoke` is pigeon-capabilities'), so the folds get a scenario.
  6. Unchecked against the live API: the telemetry query's result shape, `wallTime*` field
     names, the billable-usage row shape. The tool lists a failed source rather than failing.
  Agent run spend is model use already in the Models line, so it is shown but kept out of the
  totals.
- 2026-11-23: slice (a) on the planner's branch `claude/ai-agent-QrV7alyFFsv6`: the block in
  all six `wrangler.toml` files (core with traces off), `[containers.observability] enabled =
  false` on `CiRunner` and `Sandbox`, the smoke scenario `core_observability_on_every_worker`.
  Log lines: the deploy proxy's audit line no longer names the credential's label and person
  (the token id stays); the policy audit line drops `actor` (a user's or credential's name;
  the console's decision list keeps it). The other `console_*!` lines carry ids and error
  text only. The container key is as the wrangler configuration page gives it; the pinned
  wrangler (4.166.0) was not installed in the sandbox, so the `allowlist` job is what proves
  wrangler takes it.
  CI: `test` PT3Hd8F9xASEdbfPmtg-qp failed on 2c5d5e3 (the scenario named `lanes::parse` from
  the wrong module), then `test` 68-ckFNmO1kvef8KmMNvtP green on 137ea69; `allowlist`
  GY5wzGTZ5rGSXgZUSw6bv7 green on 2c5d5e3 (core's dry deploy, all 28 calls allowed). The
  allowlist job deploys core only, so the other five Workers' settings PATCH is proved by the
  same `SETTINGS_KEYS` entry, not by a run.
- 2026-11-23: slice (b), second round, on `claude/ai-agent-tOTEYTUOTkNp` @ 3a2d4f6 (from
  ac183ca), CI `test` xOxLg_VYt-PAmPND0BmC3X green. Fixed findings 1 to 4; added the agent run
  line (kind `metered`, outside the totals) and CI Containers seconds per class (no price: the
  class sizes sit inside the connector, so `usd` is null), and `core_observability_summary_folds`.
  Left: `spike_after_deploy` (needs hourly metrics), the CI price, the console Cost section.
  Reviewed: the folds read only counts, dollars and whitelisted fields.
- 2026-11-23: slice (c) on `claude/ai-agent-E57UIF16D9q8` @ 11f47c2 (from 3a2d4f6), CI `test`
  nt71FPNDEucTHphLbuGOGu green: `observability_monitors_plan` and `_apply`, an approval of kind
  `observability_monitors` (no `src/core` change), `core_observability_monitors_plan`. Apply
  creates only `pigeon: ...` policies and never updates or deletes. Unverified against the live
  API: the `available_alerts` shape, the script filter key, the billing product and filter
  names (plan lines say so). Delivery: the directory keeps no email, so policies are created
  with `mechanisms: {}`; Cloudflare may refuse that, and then apply returns its error. A
  failure part-way leaves the policies made so far; planning again skips them.
- 2026-11-22: Workers-split planner's review of (a), (b) and (c) for the person, who chose:
  land (a), fix and land (b), fix (c). (b), third round, on `claude/obs-b-fixes` (from 3a2d4f6,
  on (a)): `top_errors` messages go through `redact` (URL queries, emails, token-like strings)
  before the cut; the sources run together, each within 25 s (mint, call, revoke), so a hung
  upstream costs one `unavailable` line rather than the tool; a second logs query counts events
  by Worker, and a Worker that served requests with no events at all gets `top_errors: null`
  and an `unavailable` line ("its Workers Logs may be off"); Cloudflare calls use `call_fresh`
  and `why` texts show `{account}` for the account id. Smoke: `core_observability_summary_folds`
  covers the redaction and the coverage fold.
- 2026-11-22: (c), second round, on `claude/obs-c-fixes` (on (b)'s third round): the plan
  refuses an `available_alerts` answer with no recognised type, or a policies answer that is
  not a list, with "couldn't read", rather than "Everything is already in place"; apply posts
  each policy on its own and, on a refusal, answers `created` so far plus `failed`; the
  approval waits 30 minutes, like a linked plan (the directory's `approval.create` takes a
  `pending_ms`, at most 30 minutes); the billing usage watches are dropped until their product
  and filter names are checked live. `observability_monitors_plan {check: true}` reads the
  alert types and policies and answers their shape only (groups, filter keys, counts, field
  names), asking no approval: the live check to run before the person's first apply.
- 2026-11-22: handoff from the Workers-split planner (the person made the Claude chat the only
  entry point). (a) landed `I5VaqvJa7n93nkCM`: all six Workers green, Cloudflare took the
  settings from the deploy token. (b), third round, landed `MpWuu6RbcV_Z6tKf`. (c), second
  round, `qMApWFZOGahHO27l` (`2e9d2e5`), deploying at handoff. Next, for a gateway run:
  1. `observability_monitors_plan {check: true}` and compare: `available_alerts.is` should be
     `object` with groups of entries; `wanted_absent` should be empty; `filter_keys` for the two
     `workers_observability_*` types should include one of `services`, `worker_name`,
     `script_name` (else the policies go out with no script filter); `policies.mechanisms_fields`
     shows what a real policy's delivery holds (the POST sends `mechanisms: {}`, which a
     read-only check cannot prove Cloudflare accepts). Fix mismatches on a branch before the
     person's first apply. The billing watches come back only with the product and filter names
     the check shows under `filter_keys.billing_usage_alert`.
  2. `observability_summary` once: an `unavailable` entry per source shows what the live API
     shapes (telemetry result, `wallTime*`, billable usage rows) did not match. A Worker that
     logs nothing in the window shows "its Workers Logs may be off": a known false positive.
  3. Still not built: `spike_after_deploy`, the CI price, the console Cost section.
- 2026-11-23: gateway planner 4yjseGrHCRkt. (c) is deployed: `architecture_describe` reports
  core at bbbdb96, which carries 2e9d2e5 and lists `observability_monitors_plan` and
  `observability_summary`. The planner's toolset cannot call pigeon's own MCP tools, and the
  person did not answer within the wait, so handoff steps 1 and 2 (the live shape check and one
  summary run) were **not run**; they stay open below. Part 3 went to two executors:
  `spike_after_deploy` and the CI price on `claude/ai-agent-7cMSL39zrAsx`, the console's
  Platform section on `claude/ai-agent-mUxMmmZOg9xH`. See the planner's report for their tips
  and CI.

Open items:
1. Run `observability_monitors_plan {check: true}` and compare it as handoff step 1 says, before
   anyone runs `observability_monitors_apply` (the person applies, on the console).
2. Run `observability_summary {hours: 24}` once. Each `unavailable` entry that names a mint
   ("the token was not minted") is a permission the parent token cannot grant; any other entry
   is a live shape to fix.
3. Review and land the two part-3 branches. The Platform section adds nothing to the
   overview's total, because its Models line is already the Models section's cost.
- 2026-11-23: the person ran both live checks (core at ca548c9). Monitors check: alert types
  read as an object (61 types), `wanted_absent` empty; filter keys: `workers_observability_alert`
  only `status`, `workers_observability_real_time_issue` none, `billing_usage_alert` `product`
  and `limit`, `billing_budget_alert` `total_spend_dollars`; 3 policies, none `pigeon: `, a
  policy's fields `alert_type, created, enabled, id, mechanisms, modified, name`, its
  `mechanisms` holds `email`. So the two Workers policies go out with no script filter and
  watch every Worker in the account; the plan line now says so. Whether Cloudflare accepts
  `mechanisms: {}` is still unproved: the first apply shows it. Summary: every source answered.
  Fixes on the planner's branch: billing rows with an empty `ConsumedUnit` now key on
  `PricingUnit`, then `ChargeDescription`. The Models line was below the agent run line ($48.56
  against $164.47) because AI Gateway's logs carry Anthropic's uncached `input_tokens` only, and
  its cost leaves out prompt-cache reads and writes, which the agents' budget prices
  (`src/ai-agents/src/budget.rs`). The texts now say so. Counting cache tokens in
  `models_usage` is open item 4.
  Read from the top errors: "remote capability ci clashes with a local one" is expected while
  core still serves CI itself (workers-split phase 4), so it is noise, but it logs at error
  level on every isolate start; the `fetch 401` lines on model-gateway, ci-gateway and proxy
  are unauthenticated or stale-key calls refused (no error rate, since they are answered
  responses); the one Artifacts 401 and the old changelog R2 403 are past.
4. **Built (2026-11-24).** `models_usage` (pigeon-models) to count `cache_read_input_tokens` and
   `cache_creation_input_tokens`, and to price them, if the gateway's logs carry them.
5. A `pigeon: ` billing usage watch, now that the live check gives `product` and `limit`: the
   product names are still to be read from Cloudflare.

- 2026-11-24: fix to `observability_monitors_apply`: creating `pigeon: Workers errors` failed
  live with Cloudflare 400 code 17103 "Filters selection must be provided". The live
  `available_alerts` check gave `filter_keys` workers_observability_alert=[status],
  workers_observability_real_time_issue=[]. `plan_monitors` now sends
  `{"status": [...]}` for the errors alert: the values of `status` the API lists
  (`AvailableValues`) that contain error, fail or exception, else the documented `error`. The
  real-time issue policy keeps an explicit `{}`; not yet confirmed live, so if apply refuses it
  too, drop it from the plan. `observability_monitors_plan {check:true}` now also reports
  `filter_values` (the listed values per filter key) so the next mismatch is visible. Open:
  read `filter_values` from the check and confirm `error` is a listed value.
- 2026-11-24: Open item 4 built. The model gateway's index object keeps one row per finished
  job that used the prompt cache (new table `cache_usage`, migration 8, pruned past 744 h):
  time, provider, canonical model, the request's tags, cache read tokens, 5-minute and 1-hour
  write tokens, and the cache cost in micro-dollars. `stream.rs` now keeps the 5m/1h split.
  `src/models/src/cache.rs` holds the price table (same numbers, prefix match and dearest
  fallback as `budget.rs`; pigeon-models cannot depend on pigeon-ai-agents), and the merge.
  `models_usage` adds `cache_read_tokens`, `cache_write_tokens`, `cache_cost` and
  `cost_with_cache` at the top and per group; `cost` stays the gateway's. The Models line shows
  `cost_with_cache`. Departures: the rows go in the index object because `end` there already
  reads the job's request tags, so no per-job data moves; a job with no cache tokens makes no
  row; `failed` keeps no cache rows, as a row cannot tell a failure. Left: synchronous
  `models_run` bypasses the runner and carries no cache count; jobs finished before this
  deploys have no rows; the console's daily Cost chart (`src/models/src/daily.rs`) still shows
  the gateway's cost only; not run live.

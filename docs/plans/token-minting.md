# Token minting: stay under Cloudflare's 500-token cap

Status: **planned (2026-11-24)**; phases land one by one, each noting its before and after in the
Log. Planner run `oT2Ty60eh5U2`.

## Goal

pigeon never fails because the Cloudflare account holds too many API tokens (the cap is 500
live tokens, `POST /accounts/<id>/tokens` answers 400 code 1001), and when it runs short it
queues work instead of failing it.

## The incident (2026-11-24, 05:04 to about 06:00 UTC)

See the Log. In short: about 16 agent runs were live (6 planners, 10 executors) and every model
turn, every landing step and the git proxy's credential failed with the quota error. It cleared
within the hour on its own. All six planners died mid-work.

## What was measured: every place core mints a Cloudflare API token

Every mint is `pigeon_core::tokens::mint`, named `pigeon:<purpose>:<RFC 3339 time>`. A token is
revoked by whoever opened it, by `tokens::revoke` with the parent token. The two ways in are
`Scoped::open` / `open_for` (mint, use, `finish` revokes) and core's `credential` op, kind
`scoped`, which mints for a capability Worker and hands it the value.

### A. Minted in the Worker that holds the parent token (core, the Lander, capabilities)

`Scoped::open` (TTL 3600 s) or `open_for` (TTL as shown), one token per call, revoked by
`finish` when the call ends. If the Worker is cut off before `finish`, the token lives to its
TTL. A linked account (`account.bearer` set) never mints: it uses its granted bearer.

| Where | Purpose (name) | TTL | Revoked |
| --- | --- | --- | --- |
| `core/artifacts.rs` (7 call sites: ensure repo, mint/revoke git token, list, delete ref, notes) | `artifacts` | 3600 s | `finish`, after each call. The Lander's note pushes and test-ref deletes land here, one mint per call |
| `core/ai_gateway.rs` (7 sites: setup, reads, secret set) | `ai-gateway` | 3600 s | `finish` |
| `core/ai_gateway.rs` (a run, and a stream with `STREAM_TOKEN_TTL`) | `ai-gateway-run` | 3600 s, and 1200 s for a stream | `finish` |
| `core/workers.rs` (7 sites) | `upload worker`, `check worker`, `enable workers.dev`, `list routes`, `add route`, `delete route`, `list custom domains` | 3600 s | `finish` |
| `core/access.rs`, `custom_domain.rs`, `mcp_portal.rs`, `queues.rs`, `r2.rs`, `registry.rs`, `secrets_store.rs`, `zones.rs` | one purpose each | 3600 s | `finish` |
| `capabilities/observability.rs` (2 sites) | `observability` | 300 s | `finish` |
| `capabilities/secrets.rs` (check a pasted key) | a probe | 60 s | revoked at once |
| `capabilities/deploys.rs` (`deploy_token`, for the person) | the person's purpose | the person's TTL | by the `deploy_token` revoke tool, or at TTL: the person's, never swept early |
| `capabilities/linked.rs` (2 sites) | linked-account work | 3600 s | `finish` (or none: a linked account's bearer) |
| `models/lib.rs` `open_call_token` with a parent token | `worker-workers_ai` | 900 s | `close_call_token` after the call |

These are transient: a token lives for one call. They peak when many calls run at once (the
Lander and 16 runs' tools), but each is gone within seconds.

### B. Minted by core for a capability Worker: `credential` kind `scoped`

`split.rs::scoped` mints `pigeon:worker-<purposes>:<time>`, TTL 900 s, and answers its value and
its id. The permissions are fixed per purpose (`purpose_permissions`: `ai_gateway`,
`artifacts`, `secrets_store`, `workers_ai`, `mcp_portal`, `cf_deploy`). **One token per ask, and
core remembers nothing.** Who asks, how often, and who revokes:

| Asker | Asks | Revoked |
| --- | --- | --- |
| Model jobs (`models/runner.rs::account`) for an agent turn: purpose `ai_gateway` | once per model stream, and again after a refused token (401 code 2009) | **never**: lives its 900 s |
| Model gateway tools (`models/lib.rs::run_tool`, `cost`, `key`): `purposes(tool)` or `ai_gateway` | once per tool call | **never** |
| Workers AI call (`open_call_token`, no parent token): `workers_ai` | once per attempt | `close_call_token` through `credential_revoke` (name check passes) |
| CI gateway (`ci/lib.rs::account`, `Accounts::Scoped`) | once per `account()` call, many per job | **never** |
| Proxy Worker (`proxy/lib.rs::account`): `artifacts` for git, `cf_deploy` for `/cf/` | once per isolate and purpose per TTL (cached in the isolate) | **never** (TTL) |
| Sites Worker: `github` and `linked` kinds | none are Cloudflare tokens | not applicable |

AI agent runs also use `credential` kind `run_grant`: that is pigeon's own git grant, not a
Cloudflare token. It is revoked at run end through `credential_revoke`.

### Before: mints per agent turn

An agent turn is one model job. It costs **1 `ai_gateway` token** (kind B, never revoked, lives
900 s), **2** when the gateway refused the first token, and **+1 `worker-workers_ai`** (minted
and revoked) when the model is on Workers AI. A turn lasts roughly 20 to 60 s, so one run holds
15 to 45 unrevoked tokens at any moment, and 16 runs hold 240 to 720. That is the incident:
the 500 cap was crossed by the turns alone, and then every other mint (landing steps, the proxy
credential, the tools) hit the quota error with nothing left to free until tokens expired.

### Why revocation does not help yet (the 2026-11-24 `pigeon:worker-` check)

Commit `ebfead9` (2026-11-24 03:32) made `credential_revoke` read a token and refuse any whose
name does not start with `pigeon:worker-`. I checked what it stops:

- Every token minted by `scoped` is named `pigeon:worker-…`, so the check accepts all of
  them. It did **not** block any revoke that was being made.
- Core's own revokes (kind A) call `tokens::revoke` directly, with no check. They were not
  affected either.
- The tokens that piled up are the kind B tokens **nobody revokes** (turns, tools, CI, proxy).
  The check is not the cause; the missing revoke calls are.
- The check is by name prefix only, so it is only as good as the naming. Nothing yet says
  "this name was minted by pigeon" in one place, which the sweeper needs.

## Decisions

1. **One name rule, one function.** A pigeon-minted token is named
   `pigeon:<purpose>:<time>` where `<time>` is `YYYY-MM-DDTHH:MM:SSZ` and `<purpose>` is
   non-empty and holds none of `:`. A pure function `tokens::minted(name)` parses it and answers
   the purpose (`None` for any other name). Core's revoke (`tokens::revoke_minted`, which reads the
   token first) and the sweeper accept only names it accepts. The parent token, a person's own
   tokens and a job's tokens do not match.
2. **A capability Worker may revoke less than core may.** `credential_revoke` for a Worker keeps
   its narrower rule (`pigeon:worker-`: only what core minted for Workers), via
   `scoped_token_revocable`. Kind A tokens are core's own and are revoked in core. Nothing else
   is revocable by anyone.
3. **Reuse in core.** `credential` kind `scoped` returns a cached token for the same
   (caller, permission set) while it has at least 120 s left, and mints a new one otherwise.
   - The key is the caller's user id, the person it acts for (or none), and the sorted
     permission set. Never a different caller or set.
   - The cache is a Durable Object in core (`TokenLedger`, one named instance), so every
     isolate shares it. Concurrent asks for one key produce one mint.
   - The token's **value is held in the object's memory only**. Storage keeps the id, key
     hash, name, purpose, expiry and caller. If the object restarts, the next ask revokes the
     orphan by id and mints again.
   - A token is revoked when it expires from the cache, when a replacement is minted, and when
     the caller's last live run ends (see 6). A taker never revokes a shared token:
     `credential_revoke` kind `scoped` on a shared token answers `{revoked: 0, kept: "shared"}`.
     Takers that revoked after use (`close_call_token`) stop doing so for core-issued tokens.
   - Kind A tokens stay as they are (a call-length token revoked by `finish`), except that they
     count in the ledger while they live.
4. **Sweeper.** The platform's minute cron, at most every 5 minutes, lists the account's tokens
   (paged) and deletes those that match `tokens::minted` and are expired, or are named in the
   ledger as released or past their expiry. A token of any other name is never touched, and nor
   is an unexpired token the ledger does not know (a person's `deploy_token`). The sweep also
   recounts the gauge from the list.
5. **Ceiling.** The gauge is the number of unexpired tokens named by `tokens::minted`, from the
   last sweep's list plus the ledger's mints since. Soft ceiling 300 (stored, default 300; the
   hard cap is 500). Above it, a new agent run or executor is `queued` with a reason in
   `ai_agents_status`, and nothing already in flight is queued or refused: minting for in-flight
   work keeps going up to the hard cap. The gauge shows in `observability_summary` and
   `work_status`. A mint that still meets the quota error is retried after a sweep (the sweeper
   runs at once) and then fails with a clear message, never silently.
6. **Run end releases.** When a run ends and its caller has no other live run, ai-agents asks
   core to release that caller's cached tokens (revoke them). This is the "revoke on run end".
7. **Concurrency caps.** Stored config in the ai-agents index: `max_live_runs` (default 8, all
   roles), `max_executors_per_planner` (default 3), `executor_reserve` (default 3: planners
   start only while live runs are fewer than `max_live_runs - executor_reserve`, so executors
   keep room). Runs beyond a cap are `queued`, with the reason in `ai_agents_status`, and start
   as slots free. `MAX_LIVE` (32) and `EXECUTOR_RESERVE` (8) become these defaults. A tool,
   `ai_agents_config`, reads and sets them.
8. **Tests.** Every logic path takes a small `TokenApi` (list, create, delete) trait. Production
   implements it over `cloudflare::call`; the smoke suite implements it with a mock that holds
   tokens, enforces a cap (answering the quota error, code 1001) and records calls. No test
   reaches Cloudflare. Scenarios are in the area modules under `tests/smoke/`, named by Worker
   (`core_…`, `ai_agents_…`).
9. **No secret, token id or account id** in any commit, doc, log line or test. Tests use
   made-up ids.

## Phases

Each phase lands alone, with the smoke suite green. Order: 1, 4, 2, 3 (phase 4 touches only
ai-agents and can run beside phase 1).

1. **Names, the revoke rule and the sweeper** (decisions 1, 2, 4 without the ledger). Add
   `tokens::minted` and `TokenApi`; make `scoped_token_revocable` use the shared prefix
   constant; write `tokens::sweep_plan(list, now)` (pure) and the sweep that runs it; wire it into the
   platform's `scheduled` handler; add the `core_token_*` smoke scenarios with the mock (quota
   error included): a name that does not match is never deleted, an expired match is, an
   unexpired match is kept.
2. **Reuse and release** (decisions 3, 6). `TokenLedger` Durable Object (a migration in the
   platform's `wrangler.toml`), `credential scoped` through it, `credential_revoke` of a shared
   token a no-op, `credential_release` for a caller, called by ai-agents when a run ends with no
   other live run for its caller. The sweeper reads the ledger. Smoke: two asks, one mint;
   different caller or set, two mints; expiry remint; quota error on mint is retried once after a
   sweep, then returned.
3. **Gauge and ceiling** (decision 5). `tokens_status` (core tool, read): gauge, soft ceiling,
   hard cap; `ai_agents` queues new runs above the ceiling; the gauge in `observability_summary`
   and `work_status`.
4. **Planner concurrency** (decision 7). Stored caps and `ai_agents_config`; the queue reason in
   `ai_agents_status`.

## Open items

- Whether Cloudflare counts expired tokens toward the 500. The incident cleared as tokens
  expired, so assume not; the sweeper deletes expired ones anyway.
- Cloudflare's list page size for tokens: the sweep pages until empty and does not assume it.
- The soft ceiling and caps are stored values, so the person can change them without a deploy.

## Log

- **2026-11-24 05:04 to about 06:00 UTC, incident.** With about 16 runs live (6 planners and
  about 10 executors), every model turn, every landing step (notes pushed, test refs deleted)
  and the proxy's credential failed with `POST /accounts/<id>/tokens: 400 code 1001 "Quota limit
  exceeded. You can create no more than 500 tokens"`. It cleared within the hour as tokens
  expired. All six planners died mid-work. Cause, from the inventory above: each turn minted a
  900 s token for the model job that nobody revoked, so live tokens grew with turns times runs.
  The `pigeon:worker-` revoke check (2026-11-24 03:32) was not the cause.
- **2026-11-24, this plan.** Inventory written; phases 1 to 4 planned.
- **2026-11-24, phase 1.** Added `tokens::minted`, `tokens::name`, `TokenApi`, `sweep_plan` and `sweep`;
  `scoped_token_revocable` uses the shared `WORKER_NAME_PREFIX`; the platform cron sweeps every fifth minute; `core_token_*` smoke scenarios.

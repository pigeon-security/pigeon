# Token minting: stay under Cloudflare's 500-token cap

Status: **phases 1 to 4 landed (2026-11-25)**; each phase noted its before and after in the
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
| `capabilities/deploys.rs` (`deploy_token`, for the person; retired 26 Nov) | the person's purpose | the person's TTL | by `revoke_api_token`, or at TTL: the person's, never swept early |
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

After phase 2: one shared token per (caller, permission set), so 0 mints per turn in steady
state and at most one per 900 s per caller and set.

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
   (caller, permission set) while it has at least 300 s left, and mints a new one otherwise. The token it replaces is left to expire (a taker may still be using it); only orphans are revoked at once.
   - The key is the caller's user id, the person it acts for (or none), and the sorted
     permission set. Never a different caller or set.
   - The cache is a Durable Object in core (`TokenLedger`, one named instance), so every
     isolate shares it. Concurrent asks for one key produce one mint.
   - The token's **value is held in the object's memory only**. Storage keeps the id, key
     hash, name, purpose, expiry and caller. If the object restarts, the next ask revokes the
     orphan by id and mints again.
   - A token is revoked when it is an orphan (the object restarted), and when
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
- **2026-11-25, phase 4 (planner concurrency).** Stored caps `max_live_runs` (8), `max_executors_per_planner` (3) and `executor_reserve` (3) replace `MAX_LIVE` and `EXECUTOR_RESERVE`; `ai_agents_config` reads and sets them; queued runs show the reason in `step`.
- **2026-11-24, phase 2.** Added the `TokenLedger` Durable Object (platform Worker, migration
  v6; logic in `capabilities/ledger.rs`, a state machine with a `TokenApi` seam). `credential`
  kind `scoped` leases through it; `credential_revoke` of a leased token answers
  `{revoked: 0, kept: "shared"}`; `close_call_token` no longer revokes a core-issued token; new
  core op `credential_release`, called by ai-agents when a run ends and its caller has no other
  live run (new index op `live_for`); the cron sweep also deletes released ledger tokens and
  forgets expired rows. Deviations: a lease key hashes with `oauth::hash` (base64url
  SHA-256); a mint failure is returned as it came, and the quota retry runs `tokens::sweep`
  once (it deletes only expired tokens, so a ledger-live token is not freed by it); a failed
  revoke of an older token never fails the lease: the row stays `released` for the sweeper;
  `models::revoke_scoped` was removed, nothing called it any more.
- **2026-11-25, phase 3 (gauge and ceiling).** The `TokenLedger` object keeps the last sweep's
  count and time and the soft ceiling (migration 2: a `gauge` key/value table and a nullable
  `created_at` on `tokens`). New ledger ops `gauge_set`, `gauge` and `ceiling_set`; the read is
  the last count plus the ledger's mints since, never below its live rows, and a count older than
  120 s is refreshed from `tokens::count_live` first (a failed refresh falls back and says so in
  `source`). The cron sweep now runs every minute and stores `kept_pigeon`. New core tool
  `tokens_status` (in the `observability` capability): reads the gauge, and sets `soft_ceiling`
  (10 to 450) for a person or an agent acting for one. The gauge shows in
  `observability_summary` (`tokens`) and `work_status` (`tokens`, with a `note` at or above the
  ceiling). Deviations: the run object (not the index's `slot()`) asks `tokens_status`, as the
  run's caller, just before it calls `slot`, and passes the answer in; `slot()` stays
  synchronous, so its checks stay atomic and the index row needs no caller. Only a run still
  `queued` asks. A failed call logs a warning and does not hold the run. `queue_reason` takes the
  gauge as `Option<(live, ceiling)>` and names it after the run caps and before "older queued
  runs go first".

## Mint-time account admission and recovery

The incident above happened before shared credentials. In the deployed design that followed,
the soft ceiling still only applied at run admission. The ledger also wrote the token ID
after Cloudflare returned and after attempting older-token cleanup. An interruption or a
storage failure in that window could leave a live token outside the ledger. The old gauge
was scoped to Pigeon-shaped names and a cron tick did not prove that every deletion succeeded.
A count taken hours later cannot reconstruct the account's token inventory at the incident.

All home-account mint paths with the platform ledger binding use the same Durable Object,
including shared leases, ordinary scoped calls, and deploy tokens. The ledger serializes
new mints, counts every account token record conservatively, and caches the count for
five minutes, adding its own creates and taking out its own deletes as they happen. Reusing an existing shared lease consumes no capacity.
The configured ceiling applies to each new mint. At the ceiling, or on Cloudflare quota
refusal, the ledger sweeps and reevaluates capacity once. An incomplete list blocks a new
mint. An agent turn encountering token capacity waits a minute and retries on the same model.
Cancellation and the run budget still apply.

Before a Cloudflare create, the ledger commits a reservation with a unique token name.
The returned ID replaces the reservation. A lost response leaves the durable reservation
available to the minute sweep, which can find the exact name even after a Worker restart.
If the ID cannot be saved, deletion is attempted immediately. Failed deletions remain
eligible for the next sweep. Transient callers revoke by ID when they finish. If a caller
disappears after receiving its token, the sweep deletes it after expiry because it cannot
distinguish a dead caller from a slow one. Shared tokens are released after the caller's
last run ends, and shared rows loaded after a ledger restart can be reclaimed.

The automated sweep deletes by ledger-owned ID or an exact unique reserved name. It leaves
untracked legacy records alone, including names that resemble Pigeon tokens. Standalone
parent-token use without the ledger binding checks a fresh account list against the default ceiling and relies on explicit
revocation and token expiry. The candidate-parent credential probe uses the ledger and
tests with the candidate credential, which is held only for the internal request. Cloudflare remains the authority if an external client changes
the account between listing and minting.

`tokens_status` includes the account total and its observation time, and the last successful
sweep time. The total is the last complete list and is null if that list is stale or unavailable. Admission also accounts for unlisted reservations and fresh mints. The sweep timestamp
advances only when reconciliation, deletion, and the refreshed listing all succeed.

### The account's API budget (2026-11-26)

Listing the account's tokens is one Cloudflare API call per 50 records plus an empty last page,
and those calls share the account's API rate limit with `wrangler deploy` (whose pre-upload
`GET /workers/services/{script}` was refused with `10429`). Three changes keep the ledger's
own traffic small:

- `Scoped::open` mints through the ledger, as `Scoped::open_for` does. It used to mint around
  it: a full account list before every create, and a revoke the ledger could not match to a row,
  so the token stayed live until its hour ran out.
- A revoke takes the token out of the cached count instead of discarding the count, so the next
  mint does not list again. The count stands for five minutes (`ACCOUNT_CACHE_MS`, also the
  gauge's refresh age).
- The minute sweep deletes released, expired and orphaned rows by ID and lists the account only
  when a reservation needs matching by name or the count is five minutes old; one list serves
  both the matching and the new count (it used to list twice a minute). A mint that finds the
  account full still lists before it sweeps.

A transient token therefore costs one create and one delete; the account list is about three
calls every five minutes rather than six a minute plus three before most mints.

The `10429` on that lookup turned out to be per Worker as well: on 2026-11-26, for Workers
deployed or rolled back many times that day (`pigeon-platform`, `pigeon-model-gateway`),
Cloudflare refused `GET /workers/services/{script}` for 12–15 minutes straight while other
Workers deployed (CI job `VLX0RWKanYN05cYfZiqqIq`, about 07:47–08:00 UTC), and deploy.sh's
retries added to the count. The deploy proxy now remembers each successful answer to that
lookup for a day and answers a `10429` on it with the remembered one, marked
`x-pigeon-served-from-cache: 10429` (docs/proxies.md, "The Worker lookup, remembered").

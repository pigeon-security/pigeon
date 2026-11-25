# AI agent costs: the gateway's cost as the truth for runs, budgets, estimates and reports

Status: **landed** (phases A to D, reapplied onto main as `9c1e4e4`, 2026-11-23). Planner run
`HbZw1oGgUpTn`, overnight. Phases and branches are listed below; the Log says what landed.

## Goal

The person's words: the cost the AI agents Worker reports for planner and executor runs
(`spent.cost`, budgets, the dollars in reports and in `observability_summary`'s "AI agent runs"
line) is widely off from what Cloudflare AI Gateway and the Claude Platform console show. Make the
gateway the source of truth, keep budgets close to it while a run is live, estimate a run's cost
from past runs, and make the reports agree.

Hard lines: no policy rules, no change to `.pigeon/policy.toml`, `.claude/settings.json`,
`ci/workers.toml` or `.pigeon/ci.toml`; tests only in `tests/smoke.rs`; live runs' tool lists stay
as they are (no change to `src/ai-agents/src/tools.rs`).

## What is true today (main at `dc5814e`, read 2026-11-23)

- Every AI agent turn is a `models_submit` job tagged `task = ai-agent/<run>/<role>/<turn>`. The
  model gateway's job object streams it through AI Gateway (`ai_gateway::stream`). The reply's
  `usage` (uncached input, output, cache read, cache writes with the 5m/1h split) comes back in
  `models_result`.
- The run prices each **successful** turn itself: `budget::cost_micros(model, usage)` with the
  price table in `src/ai-agents/src/budget.rs`, and adds it to `spent.cost_micros` (`object.rs`
  `read`).
- A planner's `spawn_executor` adds the **child's whole cost budget** to the planner's
  `spent.cost_micros` (`object.rs:1758-1761`, "set aside"). Nothing gives it back when the child
  ends.
- `ai_agents_status` shows `spent.cost` per run. `observability_summary`'s "AI agent runs" line adds
  up `spent.cost` over every run created in the window, planners and executors alike
  (`fold_agent_runs`).
- `models_usage` reads AI Gateway's logs, newest first, **at most 2,000**, and sums the logs'
  `cost`. Since 2026-11-23 03:18 it also adds `cache_cost`, which is cache tokens priced by
  `src/models/src/cache.rs` (a copy of the budget's table), into `cost_with_cache`. The
  observability Models line shows `cost_with_cache`.

### Live evidence (`models_usage`, 2026-11-23 07:10 UTC)

- A planner turn, `ai-agent/04O5pJP1q2fz/planner/78`: gateway `tokens_in` 2, `tokens_out` 234,
  `cost` **$0.8303**. At Opus's $20 per million output tokens, 234 tokens cost $0.0047. So the
  gateway's `cost` **includes the prompt-cache reads and writes**, even though its `tokens_in` shows
  only the uncached input.
- This run's own turns: turn 2 had 2 tokens in, 140 out and cost $0.00765, against $0.0028 for the
  output alone. Turn 1 had 4 tokens in, 154 out and cost $0.0179. These fit the cache prices in
  `budget.rs`: reads at 0.05× input, 1-hour writes at 2× input, with a planner prefix of about
  24K tokens already in the cache. So the gateway prices cache tokens about as Anthropic does.
- `models_usage {task_prefix: "ai-agent/", hours: 24}` came back `truncated: true` at 2,000
  requests with $56.68. AI agent turns alone fill the 2,000-log window in less than 24 hours.
- `cache_read_tokens` and `cache_write_tokens` were 0 over that window, so the model gateway has
  kept no cache rows yet. Whether the Worker that keeps them has deployed is not known (the
  models-gateway executor checks).

## Why the numbers differ (the finding)

1. **Executor budgets are counted as planner spend and never trued up (the largest error,
   too high).** `spawn_executor` adds the child's full budget, $5 by default, to the planner's
   `spent.cost`. A planner that spawns 8 executors shows $40 more than its own turns cost, whatever
   the executors really spent. The "AI agent runs" line then adds each executor's own spend on
   top, so executor cost is counted twice: once at budget size and once as spent. The earlier
   gap of $164.47 (agents) against $48.56 (Models), recorded in observability.md on 2026-11-23,
   has this shape.
2. **The Models line is cut at 2,000 logs (too low).** `models_usage` pages the gateway's logs up to
   2,000 entries. The day's agent turns alone pass that, so the line is a lower bound. It says so
   only through `truncated`.
3. **The cache add-on would double count (latent, too high).** `cache.rs` and the observability
   texts assume AI Gateway's cost leaves out cache tokens. The evidence above shows that it
   includes them. Today nothing is added because no cache rows exist, but every row would add the
   cache cost a second time.
4. **Turns that fail, time out or are lost are paid but never counted by the run (small, too
   low).** `read` prices only successful turns. A stream that the provider cut off, or that ran
   past its timeout, is billed by the gateway for what it produced.
5. **The price table is hand-copied twice and never checked (drift).** `budget.rs` and `cache.rs`
   hold the same numbers with no source or date. Any difference from the gateway's prices stays
   uncorrected for the life of the run.

These were checked and are **not** causes:
- **Thinking tokens.** Anthropic counts thinking in `output_tokens`, and both sides price it as
  output.
- **Re-counting the transcript on every turn.** Each turn's `usage` belongs to that request alone.
  Re-sending the transcript is real cost (cache reads) and is paid once per request.
- **Cache writes priced as input.** The 5m/1h split is kept by `stream.rs` and priced as writes.

The pre-turn estimate (`estimate_micros`: all input priced as fresh, plus `max_tokens` of output)
is a deliberate upper bound. It only decides when the last turn is due. It stays as it is.

## Decisions

### 1. The source of truth: the AI Gateway log of each turn, by its log id

AI Gateway answers every request with a `cf-aig-log-id` header. The log
`GET /accounts/{id}/ai-gateway/gateways/{gateway}/logs/{log_id}` carries the gateway's own `cost`
for that one request, cache included. Reading one log per turn is exact, and the 2,000-log window
cannot cut it short. Scanning logs by task tag would cost dozens of list calls per run per
reconcile, and filtering by metadata on the server is not documented well enough to rely on.

- The model gateway keeps each job's log id (new `jobs.log_id`). `models_status` and
  `models_result` answer it as `log_id` (absent or null when the gateway gave none). This holds for
  failed and timed-out jobs too, when the stream opened.
- A new tool, `models_cost {log_ids: [..]}` (1 to 50 ids), reads each log with one minted read
  token. It answers
  `{logs: [{log_id, found, cost_usd, model, tokens_in, tokens_out}]}`, where `found: false` means
  the log is not there yet. Gateway logs are written shortly after the request ends. A null
  `cost_usd` means the gateway priced nothing.
- `models_usage` stops adding cache cost. `cost` is the gateway's cost, which already includes the
  cache. `cost_with_cache` stays one release as an alias equal to `cost`, so readers do not break.
  `cache_cost` is dropped. The cache token counts stay, as information.
- **One shared price table**, `pigeon_core::model_prices`, used by `budget.rs` and `cache.rs`. Its
  doc comment gives the source (Anthropic's pricing and prompt-caching pages, as read for
  ai-agents.md on 2026-11-22) and the date. Neither Cloudflare nor Anthropic publishes a
  machine-readable price list that a Worker could read, so a table cannot be avoided. Its only
  job now is the fast local estimate between reconciles. The gateway corrects it per turn.

### 2. Budgets: a live run's `spent.cost` tracks the gateway

- `Spent` gains four fields, each `#[serde(default)]` so stored runs still read:
  - `gateway_micros`: the part of `cost_micros` read from the gateway.
  - `unreconciled`: turns still at the local estimate.
  - `executors_micros`: for planners, what their executors hold against the budget.
  - `v`: the format; 1 means `cost_micros` holds no executor reservations.

  `cost_micros` becomes **the run's own model cost**. For each turn it is the gateway's cost once
  read, and the local estimate until then.
- Each turn is a row in a new `turn_costs` table in the run's object: job id, log id, model, local
  micros, gateway micros (null until read). Failed, timed-out and lost turns get a row as well,
  with a local estimate of 0, so the gateway's cost for them is counted when it is read.
- **Reconcile** sends `models_cost` for the oldest rows that have a log id and no gateway cost, up
  to 50 at a time. For each log found with a cost,
  `cost_micros = cost_micros - local + gateway` (saturating) and `gateway_micros += gateway`.
  It runs:
  - before each turn, when any row is older than 20 s and the last reconcile was at least 60 s ago;
  - after the run ends: up to 5 tries, at 1, 2, 5, 10 and 30 minutes, while rows are still
    unread. Each try tells the index the new `spent`.

  A failed `models_cost` (tool missing, gateway down) only logs and keeps the estimates.
- **Executors are no longer spend.** A planner's `executors_micros` is computed from the index's
  `children` answer. A live child counts the larger of its cost budget and its own cost. An ended
  child counts its own cost. It is refreshed before each turn, on `spawn_executor` and while
  waiting for executors. Every budget check, `Spent::left`, `last_turn_due` and the last-turn
  text uses `cost_micros + executors_micros`.
- **Migration**: a stored planner with `v == 0` has its children's cost budgets subtracted from
  `cost_micros` once (each spawn added exactly that), then gets `v = 1`. Executors and new runs
  start at `v = 1`.

### 3. Estimates

- `ai_agents_estimate {brief, role}` estimates from similar past runs, using the index's ended
  runs of the same role (states `done`, `failed` and `over_budget`; stopped runs with no turns are
  skipped):
  - Similarity: brief size (log ratio of bytes) and brief kind, a keyword class of `docs`, `fix`,
    `test`, `mechanical` or `build`.
  - A run's actual cost is its own cost plus, for a planner, its executors' own costs (from
    `parent_run`). Old planners with `v == 0` are corrected as in the migration.
  - The answer: the nearest 10 runs (fewer if there are fewer), giving `median_usd`, `low_usd` and
    `high_usd` (the 10th and 90th percentiles), `samples`, `executors` (median and high, for
    planners) and `suggested_budget {cost_usd, turns, executors?}`. The suggestion is the high
    cost × 1.25, rounded up to $0.50 and kept between $0.50 and the hard cap. With fewer than 3
    samples the answer says so and suggests the role's default.
- `ai_agents_start` adds `estimate` and `suggested_budget` to its answer. **It only suggests: it
  never changes the budget it was given and never refuses.** An estimate that fails is left out.
  The estimate is kept on the run's index row (new column `estimate`), and `ai_agents_status`
  shows it.

### 4. Reports

- `ai_agents_status` `spent` shows:
  - `cost` (own, best known, as today) and `cost_usd` (the same, as a number);
  - `gateway_cost`, the part read from the gateway;
  - `unreconciled_turns`;
  - `cost_source`, one of `gateway`, `gateway+estimate` or `estimate`;
  - for planners, `executors_cost` and `total_cost`.

  It also shows the run's `estimate`.
- `observability_summary`:
  - The Models line is the gateway's `cost`. It no longer adds a cache cost, and its text says the
    gateway's cost includes prompt-cache reads and writes. When cut at 2,000 logs it is still
    marked as a lower bound.
  - The "AI agent runs" line sums each run's own `cost_usd` (else `cost`). This is AI Gateway's
    cost per turn, with the estimate only for turns not yet read. It adds no executor twice, and
    its detail counts the unreconciled turns.

### Not done, and why

- A GraphQL total for the Models line, which would avoid the 2,000-log cut: the dataset's field
  names could not be checked tonight without a live call. It stays a lower bound and says so.
- Synchronous `models_run` calls carry no log id into any run. They are not agent turns.

## Phases and branches

| Phase | What | Who | Touches |
| --- | --- | --- | --- |
| 0 | This plan, docs only | planner | `docs/plans/agent-costs.md` |
| A | Log id per job, `models_cost`, no cache add-on, shared price table | executor | `src/core/src/ai_gateway.rs`, new `src/core/src/model_prices.rs`, `src/models/*`, the price section of `budget.rs` only, smoke `models_*` |
| B | `turn_costs`, reconcile, executors no longer spend, migration, status fields | executor | `src/ai-agents/src/{budget.rs (Spent and checks), object.rs (run object), prompts.rs}`, smoke `ai_agents_*` |
| C | Estimator, `ai_agents_estimate`, suggestion at start, `estimate` on status | executor | new `src/ai-agents/src/estimate.rs`, `service.rs`, the index in `object.rs`, smoke `ai_agents_*` |
| D | Observability lines | planner | `src/capabilities/src/observability.rs`, smoke `core_observability_*` |

A, B and C run in parallel. B and C are written against the `models_cost` contract above and
handle its absence. The landing order is A, B, C, D, one at a time.

## Open items (for the person)

1. After A and B deploy: run `ai_agents_status` on a live run and check that `cost_source` turns
   `gateway` within a few minutes of a turn. A run that stays `estimate` means AI Gateway sent no
   `cf-aig-log-id` on streamed requests. Report it, and per-task reconcile becomes the fallback.
2. Compare one finished run's `gateway_cost` with `models_usage {task_prefix: "ai-agent/<run>/"}`
   and with the Claude Platform console for the same hour.

## Log

- 2026-11-23: plan written from the code and the live `models_usage` evidence above.
- 2026-11-23: phases A (executor run ryF79luXxZNf), B (22FzNTrA8Xhm), C (RuhgSRVEnBEe) and D, put together on the planner's branch for one landing. The executors' branches each had CI green. B's own branch, and then the planner's, were refused by the Lander ("the pack is malformed: it inflates too large"), and other planners' branches hit the same error. C's index migration was moved after B's three. A and B both edited `budget.rs`'s price section, resolved to A's re-exports.

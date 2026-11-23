# Model providers: Anthropic's own cost, OpenAI through the gateway, models chosen per run

Status: **Part 1 deployed; Part 2 landing** (2026-11-24). Planner run `vvPiX6NZeIW4`. Phase 1b (the ANTHROPIC_ADMIN_KEY binding) waits for the person to set the key.

## Goal

The person's words, in short:

1. **Anthropic's cost, from Anthropic.** Pigeon sends Anthropic requests through Cloudflare AI
   Gateway with its own Anthropic key (BYOK). The gateway's cost for them is the gateway's own
   estimate. Anthropic bills the key, and the Claude Platform console shows slightly different
   numbers. Read Anthropic's usage and cost reports (the Admin API) and treat them as the truth
   for Anthropic spend. The app's Cost screen gains an Anthropic card. The overall total takes
   Anthropic's number in place of the gateway's Anthropic part, so nothing is counted twice.
2. **More than one provider, chosen per run.** Planners and executors can run on OpenAI as well
   as Anthropic, through the same gateway, paid by the token. The person may drop flat
   subscriptions (Claude Ultra, an OpenAI subscription) for this, so each provider's cost must be
   visible. Which model a role uses is a per-run option or a stored default, not code. "A planner
   on OpenAI and executors on Claude" must work. Today's defaults stay: planner Opus, executor
   Sonnet, mechanical Haiku. Every run shows its model and cost, so outcomes can be compared.

Out of scope: other AI Gateway model types (images, audio, media). The provider abstraction must
leave room for them; none is built.

Hard lines: no change to `.pigeon/policy.toml`, `.claude/settings.json` or `.pigeon/ci.toml`;
tests only in the smoke suite.

## What is true today (main at `70da889`, read 2026-11-24)

- **One provider.** `pigeon-model-gateway` (`src/model-gateway/src/lib.rs`) declares one
  `Provider`: `anthropic`, alias `default`, `Dialect::Anthropic`. `Dialect` is an enum with one
  arm; its `path`, `headers`, `body`, `reply` and `check` are the provider seam.
  `stream::Reading::feed` also matches on the dialect.
- **Turns are Anthropic Messages bodies.** An AI agent turn is `models_submit {request}`, where
  `request` is a whole Messages body built by `src/ai-agents/src/transcript.rs`. `turn::turn`
  refuses any model not starting `claude-`. No `provider` is passed, so the first declared is
  used.
- **Models are hard-coded per role.** `Role::models()` in `src/ai-agents/src/lib.rs` is the
  model chain: planner Opus then Sonnet, executor Sonnet then Opus, mechanical Haiku then Sonnet.
  Compaction summaries use `claude-haiku-5-5`, hard-coded in `object.rs`. The run row keeps
  `model_index`. Each turn's model is kept in `turn_costs.model`. The index row (`runs`) keeps no
  model, so `ai_agents_status` cannot show one.
- **Costs.** Per turn, the run reads the gateway's own cost by log id (`models_cost`;
  agent-costs.md). `models_daily` folds the gateway's logs (5,000 at most) into cost per day by
  model family (Opus, Sonnet, Haiku, Other) and by run. `cost_daily`
  (`src/capabilities/src/cost.rs`) joins Cloudflare billing, `models_daily` and the runs' roles
  into the iOS Cost screen's answer, and stores closed days in the Directory's settings.
- **The Cost screen** (`ios/Pigeon/Views/CostScreen.swift`) is a 2×2 grid: total, details,
  Cloudflare chart, AI Gateway chart (by model or by role). It compiles only on a Mac: the
  `ios-check` CI job.
- **Secrets.** A capability declares `SlotSpec`s. The Access tab lists them, sets them with a
  passkey, asks the owning capability to `Check` a value before writing it, and asks it for
  `Expiry` for the credentials expiry list. A Worker reads a secret through a
  `[[secrets_store_secrets]]` binding. A binding to a secret that does not exist may stop the
  deploy, so a binding lands only after the person has set the secret (buildkite-mcp.md,
  storage-providers.md).

## Decisions

### 1. The Anthropic billing connector (`models_billing`)

It lives in the models capability, beside the gateway it is compared with. It is a "billing"
source on a provider: `Provider` gains `billing: Option<Billing>`, with one variant now,
`Billing::AnthropicAdmin`. OpenAI's own usage and costs API can be a second variant later; it is
not built now.

**Credential.** A new declared secret, `ANTHROPIC_ADMIN_KEY`, scope `workers`. It is an Anthropic
Admin API key (`sk-ant-admin…`), made by an organization admin in the Claude Console. Admin keys
exist only for organizations, not individual accounts. Handled like any declared secret:
- set only on the Access tab with the person's passkey, never shown, logged or kept;
- `Check` before writing: the value must start `sk-ant-admin`, and a one-row usage report read
  must answer 200;
- `Expiry`: read through the binding, then the same one-row read. 401 or 403 is
  `Refused { why }`. 200 is `Unknown { why: "Anthropic reports no expiry for admin keys; the key
  works" }`. No binding, or no value, is `Unknown { why: "not bound yet" }`.
- The Worker reads it through a `[[secrets_store_secrets]]` binding named `ANTHROPIC_ADMIN_KEY` in
  `src/model-gateway/wrangler.toml`. **That one binding lands in its own step, after the person
  has set the secret** (phase 1b).

**The Admin API, as known.** Verify on the first live call, and keep the parsing tolerant of
missing fields.
- `GET https://api.anthropic.com/v1/organizations/usage_report/messages`, with headers
  `x-api-key` and `anthropic-version: 2023-06-01`. Query: `starting_at` and `ending_at` (RFC 3339),
  `bucket_width=1d`, `group_by[]=model`, `limit` (at most 31 one-day buckets a page), `page`.
  Answer: `{data: [{starting_at, ending_at, results: [{model, uncached_input_tokens,
  cache_read_input_tokens, cache_creation: {ephemeral_5m_input_tokens,
  ephemeral_1h_input_tokens}, output_tokens, …}]}], has_more, next_page}`.
- `GET https://api.anthropic.com/v1/organizations/cost_report`, query `starting_at`, `ending_at`,
  `group_by[]=description`, `page`. One-day buckets only. Answer: `{data: [{starting_at, ending_at,
  results: [{currency, amount, description, model?, cost_type?, token_type?, …}]}], has_more,
  next_page}`. **`amount` is a decimal string in the lowest currency unit (cents)**: `"123.45"` is
  $1.2345.
- Both follow `next_page` while `has_more`, at most 10 pages each. A failure of either goes to
  `unavailable` and leaves the other.

**The tool.** `models_billing {provider: "anthropic", days: 1..31 (7 by default)}` answers:

```json
{
  "provider": "anthropic",
  "source": "Anthropic Admin API (usage_report/messages, cost_report)",
  "days": ["2026-11-18", "…", "2026-11-24"],
  "total_usd": 12.34,
  "by_day": {
    "2026-11-23": {
      "cost_usd": 3.21,
      "tokens": {"uncached_input": 0, "output": 0, "cache_read": 0, "cache_write": 0},
      "by_model": {"claude-opus-5-5": {"cost_usd": 2.0, "tokens": {"…": 0}}}
    }
  },
  "unavailable": [{"source": "cost_report", "why": "…"}]
}
```

Model names are in `canonical_model` form (`claude-opus-5-5`), so they join with the gateway's.
A cost line with no model goes under `"other"`. Days with no data are left out of `by_day`, which
is not the same as $0: see the reconciliation. An unset key answers an error that names the
Access tab, never a value.

### 2. Reconciliation: how the two are correlated, and how a mismatch shows

The gateway's Anthropic cost is an estimate made by the gateway. Anthropic's cost is what is
billed. They differ for three reasons:
- **pricing**: the gateway's price table against Anthropic's (cache writes, long context, batch,
  service tier);
- **traffic outside the gateway**: other keys in the organization, the console's workbench,
  Claude Code on an API key;
- **cut-short listings**: `models_daily` reads 5,000 gateway logs at most, so a busy window is a
  lower bound (`truncated`).

**Correlation**, per UTC day, and per model within the day (pure, `pigeon_models::billing::reconcile`):
- Gateway side: the day's cost, uncached input tokens and output tokens of logs whose provider is
  `anthropic`. `models_daily` adds `by_provider` and `by_provider_model` for this (below).
- Anthropic side: the day's `cost_usd` and tokens from `models_billing`.
- `difference_usd = anthropic - gateway`. Token agreement:
  `tokens_ratio = anthropic (uncached input + output) / gateway (same)`.
- `status`:
  - `match` when |difference| ≤ max($0.05, 3% of Anthropic);
  - `pricing` when tokens agree within 3% but cost does not: a price difference;
  - `outside_gateway` when Anthropic's tokens exceed the gateway's by more than 3%, and the
    listing was not truncated: usage the gateway never saw;
  - `gateway_truncated` when the gateway's listing was cut short;
  - `anthropic_pending` when Anthropic has no data for the day yet (today, or a lag); the day
    uses the gateway's number and says so;
  - `gateway_only` when the gateway has Anthropic cost and Anthropic answered nothing for a
    closed day.

**The total, without double counting.** For each day:

```
total(day) = cloudflare(day)
           + ai_gateway(day) - gateway_anthropic(day)
           + (anthropic(day) if Anthropic reported the day else gateway_anthropic(day))
```

So Anthropic's number replaces the gateway's Anthropic number wherever Anthropic has spoken, and
the gateway's estimate fills only days Anthropic has not reported yet. When `models_billing` is
unavailable (no key, refused, down), the total is today's total, and `unavailable` and a note say
so. Anthropic usage outside the gateway is included in the total: it is money the person pays.

**How a mismatch is surfaced.**
- `cost_daily` answers `anthropic.reconciliation`: the range's `gateway_usd`, `anthropic_usd`,
  `difference_usd`, and `days: [{day, gateway_usd, anthropic_usd, difference_usd, tokens_ratio,
  status}]` for every day of the range that either side has.
- Each day whose status is not `match` or `anthropic_pending` adds one line to `notes`, such as
  "2026-11-22: Anthropic billed $4.10, the gateway recorded $3.70 (+$0.40, tokens agree: a
  pricing difference)."
- The app's Anthropic card shows the gateway's figure and the difference under Anthropic's
  total, and marks a non-matching range.

### 3. `cost_daily` and `models_daily`

- `models_daily` adds, from the same logs, `by_provider: {provider: {day: usd}}` and
  `by_provider_model: {provider: {model: {day: {cost_usd, tokens_in, tokens_out, requests}}}}`.
  Existing fields do not change.
- Model families: `family()` keeps Opus, Sonnet and Haiku, and names other providers' models by
  provider (`OpenAI`), so the chart's Other stays small. `MODELS` in `cost.rs` and the app's
  order gain `OpenAI` after `Haiku`.
- `cost_daily` calls `models_billing {provider: "anthropic", days}` beside its other sources. A
  day record gains `anthropic: {cost_usd, by_model}` and `gateway_providers: {provider: usd}`.
  Both are stored with closed days like the other keys, so `all` keeps them past 31 days. Its
  answer gains:
  - `ai_gateway.by_provider: [{name, total, by_day}]` (Anthropic, OpenAI, then Other above 0);
  - `anthropic: {total, by_model: [{name, total, by_day}], reconciliation, source}`, or
    `anthropic: null` with an `unavailable` entry;
  - `total` computed as in §2. `ai_gateway.total` stays the gateway's whole cost, unchanged, so
    the app's AI Gateway card still matches the gateway's dashboard.

### 4. OpenAI through the gateway

- A second provider in `pigeon-model-gateway`: `openai`, alias `default`, `Dialect::OpenAi`,
  default model as the person names (Open items), key help "An OpenAI API key, from the OpenAI
  platform's API keys page." Its key is the declared secret `pigeon_openai_default` (scope
  `ai_gateway`), set on the Access tab; `models_setup` and `Set` hand it to the gateway as for
  Anthropic. `Check` calls `GET https://api.openai.com/v1/models` with the key.
- **One canonical turn shape: Anthropic Messages.** Agent transcripts stay as they are. The model
  gateway translates at its edge, in a new pure module `src/models/src/openai.rs`:
  - request: Chat Completions (`chat/completions`), the conventional, stable API, streamed with
    `stream_options.include_usage`. System blocks become one `system` message; tools become
    `{type: function, function: {name, description, parameters}}`; `tool_choice` auto stays
    `auto`; `tool_use` blocks become `tool_calls`, with `input` as a JSON string;
    `tool_result` blocks become `role: tool` messages, right after the assistant message, with
    any text from the same user message after them; `thinking` and `redacted_thinking` blocks,
    `cache_control`, `thinking` and `context_management` are dropped; `output_config.effort`
    becomes `reasoning_effort`; `max_tokens` becomes `max_completion_tokens`; `betas` are refused
    for OpenAI.
  - stream: `choices[0].delta.content` becomes a text block; `delta.tool_calls` become
    `tool_use` blocks, the argument fragments joined and parsed at the end; `finish_reason` maps
    `stop` → `end_turn`, `tool_calls` → `tool_use`, `length` → `max_tokens`, `content_filter` →
    `refusal`; an `error` object is the stream's error; `[DONE]` ends it.
  - usage, in the Anthropic shape the agents price: `input_tokens = prompt_tokens -
    prompt_tokens_details.cached_tokens`, `cache_read_input_tokens = cached_tokens`,
    `output_tokens = completion_tokens` (reasoning included), no cache writes.
- `turn::turn` checks the model against the provider: `claude-` for Anthropic; for OpenAI, a
  plain model id (letters, digits, `.`, `-`, `_`, `:`; 100 characters at most), so a new model
  can be tried without a code change.
- `pigeon_core::model_prices` gains the OpenAI models the person names. An unknown model keeps the
  dearest price for the local estimate. The gateway's per-turn cost corrects it, as for Claude.
- `models_run` (one request, answered while waiting) gets the same translation through
  `Dialect::OpenAi`'s `body` and `reply`.
- Extensible: a third provider is a `Provider` entry and, if it does not speak one of the two
  dialects, a new `Dialect` arm. Non-text model types would be a new kind of request, not a
  dialect, and are not built.

### 5. Choosing models per run (`pigeon-ai-agents`)

- **A model choice** is `provider/model` (`openai/gpt-…`, `anthropic/claude-opus-5-5`), or a bare
  `claude-…` name, which is Anthropic. Parsed and checked in a new pure module
  `src/ai-agents/src/models.rs`.
- **Defaults, stored, not compiled in.** The index keeps `models.defaults` in its `flags` table:
  `{planner, executor, mechanical, summary}`. When none is stored, today's: planner
  `anthropic/claude-opus-5-5`, executor `anthropic/claude-sonnet-5-5`, mechanical
  `anthropic/claude-haiku-5-5`, summary `anthropic/claude-haiku-5-5`. A new tool
  `ai_agents_models {set?: {planner?, executor?, mechanical?, summary?}}` shows them, sets the
  ones given (checked), and says which are stored and which are built in.
- **Per run.** `ai_agents_start` gains:
  - `model`: this run's model;
  - `models: {executor?, mechanical?}`: for a planner, what its executors use unless it says
    otherwise.
  Both are kept on the run (`run.model`, `run.child_models`) and on the index row (`runs.model`).
  A planner's `spawn_executor` gains `model`. Its executors otherwise use the planner's `models`
  for their kind, then the stored default. So "a planner on OpenAI, executors on Claude" is
  `ai_agents_start {role: "planner", brief, model: "openai/…", models: {executor:
  "anthropic/claude-sonnet-5-5"}}`.
- **Fallback** stays within the provider: the chosen model, then the role's built-in fallback
  only when it is the same provider. A run on OpenAI with no OpenAI fallback has none. A turn's
  thinking blocks are stripped when the model changes (as today on fallback).
- **Turns** send `provider` with `models_submit`. `transcript::thinks` is true only for Claude
  models; the model gateway drops what OpenAI does not take, whatever is sent.
- **Seeing and comparing.** `ai_agents_status` shows each run's `model` (and `models` for a
  planner). Turns that fell back show in `turn_costs`. A new tool `ai_agents_compare {days?
  (30), role?}` groups the index's ended runs by role and model: `runs`, `done`, `failed`,
  `over_budget`, `stopped`, `cost_usd {total, median, high}`, `turns {median}`, `minutes
  {median}`. The cost is the run's own cost (agent-costs.md), reconciled with the gateway.

## Phases and branches

| Phase | What | Who | Touches |
| --- | --- | --- | --- |
| 0 | This plan | planner | `docs/plans/model-providers.md`, `docs/plans/README.md` |
| 1 | `models_billing` (Anthropic Admin API), `ANTHROPIC_ADMIN_KEY` slot, check and expiry, `billing::reconcile`, `models_daily` `by_provider` and `by_provider_model`, `family()` naming other providers | executor | `src/models/src/{billing.rs (new), daily.rs, lib.rs}`, `src/model-gateway/src/lib.rs`, smoke `models_*` |
| 1b | The `ANTHROPIC_ADMIN_KEY` binding | planner, **after the person sets the secret** | `src/model-gateway/wrangler.toml` |
| 2 | `cost_daily`: Anthropic section, reconciliation, the total without double counting, `by_provider` | executor | `src/capabilities/src/cost.rs`, smoke `platform_cost_daily_*` |
| 3 | The app's Anthropic card; OpenAI in the model order | executor | `ios/Pigeon/Views/CostScreen.swift`; `ios-check` CI job |
| 4 | OpenAI provider: `Dialect::OpenAi`, translation and stream, model check per provider, key check, prices | executor | `src/models/src/{openai.rs (new), lib.rs, stream.rs, turn.rs}`, `src/model-gateway/src/lib.rs`, `src/core/src/model_prices.rs`, smoke `models_*` |
| 5 | Model choice per run and stored defaults, `ai_agents_models`, `ai_agents_compare`, `spawn_executor.model`, status shows model | executor | `src/ai-agents/src/*`, smoke `ai_agents_*` |
| 6 | Docs: this plan's Log, the architecture of the flow, the person's steps | planner | `docs/plans/*` |

Phases 1 and 5 run together (disjoint crates). Phase 2 and 4 build on phase 1's branch; phase 3
builds on phase 2's answer shape. The landing order is 1, 5, 2, 3, 4, then 1b when the key is set.

## How to use it

- Choose models per run: `ai_agents_start {role: "planner", brief, model: "openai", models: {executor: "anthropic"}}`. This is a planner on OpenAI's top tier (gpt-6.2-astra) whose executors use Claude Sonnet. A model can also be named in full: `openai/gpt-6.2-sol`, `anthropic/claude-opus-5-5`, or a bare `claude-…` name. A planner's `spawn_executor` takes `model` too.
- See or change the defaults: `ai_agents_models`, and `ai_agents_models {set: {executor: "openai/gpt-6.2-sol"}}`. Built in: planner Opus, executor Sonnet, mechanical Haiku, and summaries Haiku.
- Compare models: `ai_agents_compare {days: 30}`, ended runs by role and model, with their outcomes, cost, turns and minutes. `ai_agents_status` shows each run's model.
- Cost: `cost_daily` gives the gateway's cost `by_provider` (Anthropic, OpenAI). Its `anthropic` section is what Anthropic billed, reconciled with the gateway. `models_billing` is Anthropic's own report.
- OpenAI needs the key `pigeon_openai_default` set on the Access tab. Until then, an OpenAI turn fails and the run says so.

## Open items (for the person)

1. **Anthropic Admin key.** Make an Admin API key in the Claude Console (Settings, Admin keys;
   it takes an organization admin), and set it as `ANTHROPIC_ADMIN_KEY` on the Access tab once
   phase 1 is deployed. Then phase 1b binds it.
2. **OpenAI key.** Set `pigeon_openai_default` on the Access tab once phase 4 is deployed.

## OpenAI models and prices (the person, 2026-11-24)

OpenAI models map to the Claude tiers, to be tuned later. Prices per million tokens (input /
cached input / output), standard short context, then long context (over 272K input tokens):

| Tier | Model | Short | Long |
| --- | --- | --- | --- |
| Opus (planner) | `gpt-6.2-astra` | $8.00 / $0.40 / $40.00 | $16 / $0.80 / $60 |
| Sonnet (executor) | `gpt-6.2-sol` | $2.00 / $0.10 / $10.00 | $4 / $0.20 / $15 |
| Haiku (mechanical) | `gpt-6.1-terra` | $1.00 / $0.05 / $5.00 | $2 / $0.10 / $7.50 |
| (cheaper option) | `gpt-6-luna` | $0.10 / $0.01 / $0.50 | $0.20 / $0.02 / $0.75 |

The gateway's real cost per turn is preferred whenever it is read. The table is only the local
estimate. Org-wide Anthropic spend counts in the total (confirmed).

## Log

- 2026-11-24: plan written from main at `70da889`.
- 2026-11-24: the person did not answer the budget question in time. The run has $25 and 30
  sandbox minutes for itself and all its executors, so only Part 1 is built now: phases 1 and 2 in
  one executor (`QnSjNZEK85EP`), with CI as the gate to save sandbox time, and phase 3 in another
  (`dDPNCfmPH3ZS`). Phases 4 and 5 (Part 2) stay planned for a run with a larger budget. New smoke
  scenarios go at the end of `tests/smoke.rs`, because the policy's `smoke-suite-fixed` allows
  `add_test` only on that file.
- 2026-11-24: the person extended the budget ($70, 330 sandbox minutes) and named the OpenAI
  models and prices (above). Part 2 is built too: phase 5 (executor `G2YZBsyBTIkW`) now, and
  phase 4 after phase 1, on its branch. Phase 3 (the app's card) was built by the planner after
  its executor's 4 sandbox minutes ran out; `ios-check` passed on `f0ea56d`. A bare provider name
  (`model: "openai"`) picks that provider's model for the role's tier.
- 2026-11-24: phases 1 and 2 built by `QnSjNZEK85EP` (CI `test` green on `6203434`, job
  `ZwfI3ayr2LNkbpFuBgexSA`), merged into the planner's branch. Two choices made there: a day
  whose cost differs while Anthropic counted fewer tokens is `pricing`; and if Anthropic's cost
  report fails, `cost_daily` uses none of Anthropic's figures and lists "Anthropic" as
  unavailable.
- 2026-11-24: main rewrote the app's screens (`f0d28d4`) while this branch was in the landing
  queue. The branch was rebased onto main, and the Anthropic card was rebuilt on the new
  CostScreen. The difference from the gateway sits beside Anthropic's total, because the plot's
  fixed height leaves no room for a line under it. The rebased branch keeps its old tip as an
  `ours` merge parent, because the push of the rewritten branch was refused.
- 2026-11-24: Part 1 landed as one linear commit (claude/ai-agent-qX99nrI8eQRn, the tree of the planner's tested tip), because the planner's sandbox could not force-push its rebased branch.
- 2026-11-24: phases 4 (jX4T2h1nihxH) and 5 (G2YZBsyBTIkW) put together on one branch for one landing. Phase 4 also changed canonical_model to keep the dots of non-Claude names (gpt-6.2-sol) and added price_at for OpenAI's long-context price. Phase 5 stores defaults in the index's flags (models.defaults) and adds runs.child_models so status shows a planner's models.

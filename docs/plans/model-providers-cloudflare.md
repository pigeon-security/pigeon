# Model providers, Part 3: models paid with Cloudflare credits

Status: **Phase 1 landed** (2026-11-25, `934a532`). Phases 2 and 3 in progress (planner run `A2apepRW1Vyb`). Open item 1 is done; phase 4 lands after phases 2 and 3. Follows [model-providers.md](model-providers.md) (Parts 1 and 2).

## Goal

The person's words: "Part two of enabling Cloudflare AI Gateway models to be used. I would like to
use models from there for planners and executors. This is not BYOK but direct Cloudflare AI
Gateway models."

1. Planner, executor, mechanical and summary turns can run on models that Cloudflare pays for and
   bills to the account (AI Gateway Unified Billing, paid from credits). Pigeon holds no provider
   key for these. Cloudflare's own Workers AI models come after that, if one is good enough for
   agent turns with tools.
2. A model's name says who pays. "A planner on credits, executors on Claude BYOK" works, and so
   does the reverse. Today's defaults stay: planner Opus, executor Sonnet, mechanical and summary
   Haiku, all Anthropic BYOK.
3. Each dollar counts once in `cost_daily`. Cost is shown by provider and by route (BYOK or
   credits). Runs keep their per-turn cost, and `ai_agents_compare` covers the new models.
4. Gateway settings are explicit, `drift()` covers them, and steps only the person can do are in
   Open items.

Hard lines: no change to `.pigeon/policy.toml`, `.claude/settings.json` or `.pigeon/ci.toml`.
New tests go only in the smoke suite, appended at the end of `tests/smoke.rs`.

## What is true today (main at `d4220be`, read 2026-11-25)

- **One gateway, `pigeon`.** `pigeon_core::ai_gateway::spec` creates it with `authentication:
  true`, `byok_only: true`, `collect_logs: true` and the account's Secrets Store. `ensure` never
  changes a gateway that exists. `drift()` lists the settings of `authentication`, `byok_only`,
  `collect_logs` and `store_id` that differ from the spec.
- **Providers** (`src/model-gateway/src/lib.rs`): `anthropic` (key `pigeon_anthropic_default`,
  set) and `openai` (key `pigeon_openai_default`, declared, **not set**). A provider's `slug` is
  two things at once: the gateway's provider path and the name callers select it by.
  `Models.gateway` is one string, and every call uses it: `run`, `setup`, `secret_set`, logs,
  `models_cost`, `models_daily`. The job object's gateway is fixed in `runner::Settings`.
- **Keys.** `models_setup` and `secret_set` hand every declared key that exists in the store to
  the gateway (`ensure_provider`). So a key slot that is still declared would be put back on its
  gateway after someone removed it from the dashboard.
- **Choices** (`src/ai-agents/src/models.rs`): `PROVIDERS = [anthropic, openai]`; a choice is
  `provider/model`, a bare `claude-…` name, or a bare provider name (that provider's model for the
  slot's tier).
- **Cost.** `models_daily` folds one gateway's logs, at most 5,000. `cost_daily` adds Cloudflare
  billing (lines by `ServiceName`: "Workers AI" would fall into the "Workers" line today), the
  gateway, and Anthropic's own report. Anthropic's report replaces the gateway's `anthropic`
  provider figure.
- **Both live planners**, this run included, send their turns through `pigeon` on the Anthropic
  key.

## What Cloudflare's docs say (read 2026-11-25)

Sources: developers.cloudflare.com/ai-gateway: features/unified-billing,
usage/rest-api, usage/chat-completion, configuration/authentication,
configuration/bring-your-own-keys, features/spend-limits, reference/troubleshooting,
reference/pricing, observability/logging; the API reference for creating a gateway; the model
catalog (developers.cloudflare.com/ai/models).

- **Unified Billing.** Credits are bought in the dashboard, with a 5% fee on each purchase. Tokens
  are billed at the provider's own price, with no markup. A balance can go negative; Cloudflare
  then charges the card at the start of the month. Unified Billing needs an authenticated
  gateway.
- **Which credentials a request uses**, in order: (1) a provider key sent with the request;
  (2) a key stored on *that gateway* under the alias `default`; (3) Unified Billing, unless the
  gateway has `byok_only: true` ("Require provider credentials"). With `byok_only` on and no key,
  the gateway answers 400, code 2044. **So a gateway that holds a stored key for a provider never
  uses credits for that provider.** BYOK Claude and credits Claude cannot share one gateway.
- **Endpoints.** Two endpoints accept credits:
  - provider-native endpoints on `gateway.ai.cloudflare.com/v1/{account}/{gateway}/{provider}/…`
    with `cf-aig-authorization` (an AI Gateway Run token). This is what pigeon uses today.
    Unified Billing lists OpenAI, Anthropic, Google AI Studio, Vertex, xAI and Groq.
  - the REST API, `api.cloudflare.com/client/v4/accounts/{account}/ai/v1/{messages,
    chat/completions,responses}` and `/ai/run`. It needs a token with **Workers AI Read**; a token
    with only AI Gateway permissions gets 401, code 10000. The gateway is chosen with the
    `cf-aig-gateway-id` header (otherwise the account's `default` gateway). This is the only
    documented way to reach Workers AI (`@cf/…`) models apart from the Worker binding.
    `/compat/chat/completions` is deprecated for single-model calls.
- **Errors for credits:** 403 code 2049 (Unified Billing not possible: no authentication, keys
  required, not enabled for the gateway, or no balance), 402 code 2052 (balance too low; `extra.
  availableBalanceCents`), 429 code 2018 (Unified Billing rate limit), 503 code 7005 (not
  available for the provider right now).
- **Gateway settings via the API:** `byok_only`, `authentication` (it cannot be turned off once
  on), `collect_logs`, `store_id`, `zdr` (Zero Data Retention: credits traffic goes only to
  provider endpoints that keep no prompts), `workers_ai_billing_mode` (`postpaid` or `unified`),
  `spend_limits {enabled, rules: [{limit, limitType: "cost", window, technique: fixed|sliding,
  provider?, model?, metadata?}]}`. A spend limit answers 429 once reached. Limits are eventually
  consistent, apply to BYOK and credits alike, and a gateway takes 20 rules at most.
- **Tokens are account-wide.** An AI Gateway Run token works on every gateway in the account.
  Scoping a token to one gateway is not possible.
- **Cost in logs and billing.** Each log keeps model, provider, tokens and cost. For credits
  requests the gateway "always logs the request billing details", even when logging is off. The
  cost metric is documented as an estimate. Credit purchases appear on Cloudflare invoices. A
  monthly AI Gateway usage invoice shows one line per model (`anthropic/claude-haiku-4.5:
  $0.16`; changelog 2026-09-01). Names are standardised as `provider/model` with dots
  (`anthropic/claude-sonnet-5.5`), which `canonical_model` already reads.
- **Models in the catalog**, for credits: Anthropic `claude-opus-5.5`, `claude-sonnet-5.5`,
  `claude-haiku-5.5` (Anthropic Messages format, ZDR); OpenAI `gpt-6.2-astra`, `gpt-6.2-sol`,
  `gpt-6.1-terra`, `gpt-6-luna`; and others (Google, xAI, Moonshot, Qwen, DeepSeek). Workers AI
  models documented with function calling and reasoning:
  `@cf/zai-org/glm-5.3` (1M context; $1.40 / $0.26 cached / $4.40 per M), `@cf/moonshotai/
  kimi-k2.7-code` (262K; $0.95 / $0.19 / $4.00), `@cf/deepseek-ai/deepseek-v4-pro-0813` (1M;
  $1.32 / $0.044 / $3.96). Each needs Workers Paid or credits.
- **Secret names.** A stored key must be named `{gateway_id}_{provider_slug}_{alias}`; Cloudflare's
  own example is `my-gateway_anthropic_default`. So a hyphen in the gateway part is valid. The
  `secret_id` is not used for lookup, so a key cannot be shared between two gateways: the
  Anthropic key must be set again under the new name.

**Not verified live yet.** This planner has no tool that makes a model call. The live check is
phase 5: an executor run on a credits model after phases 1 and 2 are deployed.

## Decisions

### 1. Two gateways; `pigeon` becomes the credits gateway (the person, 2026-11-25)

| Gateway | Route | `byok_only` | Stored keys | Other settings |
| --- | --- | --- | --- | --- |
| `pigeon` | credits (Unified Billing) | **false** | **none** | `authentication: true`, logs on, `zdr: true`; no spend limit from pigeon (the person's decision) |
| `pigeon-anthropic` (new) | Anthropic BYOK | true | `pigeon-anthropic_anthropic_default` | `authentication: true`, logs on, the account's store |

No `pigeon-cloudflare` gateway is made.

Why two gateways: a stored key always wins over credits on its gateway (above). Why `byok_only`
stays on for `pigeon-anthropic`: if its key is missing or refused, an Anthropic BYOK turn fails
loudly (2044) and never bills credits by accident.

**Security.**
- **Who can spend credits:** anyone holding a token with AI Gateway Run on this account (any
  gateway without `byok_only`), or Workers AI Read (through the REST API). Pigeon holds no such
  standing token: core mints one per call or job, for at most 20 minutes, and revokes it after.
  The parent token stays in core. The person's own dashboard tokens are the other holders.
- `authentication` stays on for both gateways. It is required for Unified Billing anyway, and it
  cannot be turned off.
- Pigeon neither sets nor requires a spend limit (the person's decision). The person may set one
  on `pigeon` in the dashboard to cap a leaked token or a runaway run; drift never reports it.
- `zdr: true`: credits traffic goes only to provider endpoints that keep no prompts. Pigeon
  already keeps no payloads in its logs (`cf-aig-collect-log-payload: false`).
- The account's auto-created `default` gateway is a risk once credits are loaded: a Workers AI
  Read token sending REST calls without `cf-aig-gateway-id` would land there. Pigeon always names
  its gateway. The person may turn on "Require provider credentials" on `default` (Open items).

**Spec and drift.** `spec` takes the gateway's kind (`Byok` or `Credits`). `drift()` compares
the settings in the table, and it also reports:
- for a credits gateway: any stored provider key ("holds a stored key for anthropic: credits
  requests for it would bill that key instead");
- for a BYOK gateway: a missing key for a declared provider.

`ensure` still creates a missing gateway from its spec and never changes one that exists. The
move of `pigeon` from BYOK to credits is done by the person in the dashboard (Open items), and
drift shows what is left.

**Credits guard.** Before the first request of a run or job on a credits provider, the model
gateway reads the credits gateway's settings and provider configs (cached in the isolate for
5 minutes). It refuses the request, naming the Open item, if `byok_only` is still on or a stored
key exists for that provider. So a credits turn can never bill a BYOK key by mistake, and no
landing has to wait for the person's dashboard steps.

### 2. Model names: who pays is in the name

A choice is `provider/model`, and the provider names its route:

| Choice provider | Paid by | Gateway | Path and dialect | Tiers (top / middle / small) |
| --- | --- | --- | --- | --- |
| `anthropic` | the person's Anthropic key (BYOK) | `pigeon-anthropic` | `anthropic`, Messages | `claude-opus-5-5` / `claude-sonnet-5-5` / `claude-haiku-5-5` |
| `cf-anthropic` | Cloudflare credits | `pigeon` | `anthropic`, Messages | the same Claude models |
| `cf-openai` | Cloudflare credits | `pigeon` | `openai`, Chat Completions | `gpt-6.2-astra` / `gpt-6.2-sol` / `gpt-6.1-terra` |
| `workers-ai` (phase 6, not yet) | Cloudflare credits | `pigeon` | REST `ai/v1/chat/completions`, OpenAI dialect | the person to choose |

- A bare `claude-…` name stays Anthropic BYOK. A bare provider name picks its tier model, so
  `model: "cf-anthropic"` on a planner is `cf-anthropic/claude-opus-5-5`.
- **`openai/…` is retired** (assumed, see Log). The OpenAI key was never set. A choice
  `openai/…` is refused with "OpenAI is paid with Cloudflare credits: use cf-openai/…". A stored
  default `openai/…` reads as unset, so the built-in default applies, and `ai_agents_models`
  says so. Pigeon never moves a choice from one payer to another by itself.
- Fallback stays within the choice's provider, and so within its route. A credits run never
  falls back to BYOK, and the reverse.
- In the model gateway, a `Provider` gains `name` (the choice's provider), `gateway` and `route`
  (`Byok` or `Credits`). `slug` stays the gateway's provider path. `models_submit {provider}`
  and `models_run {provider}` select by `name`. The first provider stays the default:
  `anthropic`.
- Defaults are unchanged: planner `anthropic/claude-opus-5-5`, executor
  `anthropic/claude-sonnet-5-5`, mechanical and summary `anthropic/claude-haiku-5-5`, now through
  `pigeon-anthropic`.

So "a planner on credits, executors on Claude BYOK" is `ai_agents_start {role: "planner", brief,
model: "cf-anthropic", models: {executor: "anthropic"}}`. The reverse is `model: "anthropic",
models: {executor: "cf-openai"}`.

### 3. The migration: no in-flight run breaks, no Anthropic call bills credits

Turns are separate jobs. A job's gateway is fixed when it is submitted (a new `gateway` field on
the job; a job stored without one uses `pigeon`, which is today's behaviour). A deploy therefore
changes only the turns that start after it.

| Step | Who | What | `anthropic` turns go to | Safe because |
| --- | --- | --- | --- | --- |
| A | phase 1 lands | Both gateways declared. `pigeon-anthropic` is made, and the slot `pigeon-anthropic_anthropic_default` is declared and handed to it when set. The Anthropic route stays on `pigeon`. Credits providers are declared but stopped by the guard while `pigeon` holds a key. | `pigeon` (its key) | nothing moves |
| B | **the person** | Sets `pigeon-anthropic_anthropic_default` on the Access tab with their passkey (a new Anthropic key; done 2026-11-24). `secret_set` adds it to `pigeon-anthropic`. | `pigeon` (the old key) | both gateways hold a key: `pigeon` the old one, `pigeon-anthropic` the new one |
| C | planner lands phase 4 | The Anthropic route points at `pigeon-anthropic`. `pigeon_anthropic_default` is no longer declared, so `models_setup` stops putting it back on `pigeon`. `CREDITS_SINCE` is set to this deploy's time. | `pigeon-anthropic` | a turn in flight finishes on `pigeon` with the old key; the next turn uses the new gateway, which has the key; if the key were missing, `byok_only` would refuse the turn, never bill credits |
| D | **the person** | In the dashboard, on `pigeon`: deletes the stored Anthropic key, turns off "Require provider credentials", turns on ZDR. Deletes the secret `pigeon_anthropic_default`. | `pigeon-anthropic` | no Anthropic BYOK traffic uses `pigeon` any more; the guard frees the credits route only now |
| E | person, then planner | Buys credits, then the planner runs the live check (phase 5). | | |

The OpenAI key slot `pigeon_openai_default` is no longer declared from step A. **No key is ever
set on `pigeon`.**

### 4. Cost: two gateways' logs, each dollar once

**`models_daily`** reads the logs of every declared gateway (5,000 at most each, `truncated` per
gateway), and labels each log with a provider `name` and a `route`:
- on `pigeon-anthropic`: `anthropic`, BYOK;
- on `pigeon`: if before `CREDITS_SINCE`, BYOK under its own provider (`anthropic`, `openai`);
  from then on, credits: `cf-anthropic`, `cf-openai`, or `cf-{provider}` for any other provider.
  `CREDITS_SINCE` is an RFC 3339 constant beside the gateway declaration. It is far in the future
  until step C sets it.

New fields, existing ones unchanged:
- `by_route: {byok: {day: usd}, credits: {day: usd}}`;
- `by_provider` and `by_provider_model` keyed by the provider `name`;
- `gateways: {id: {logs, truncated}}`.

`family_of` reads `cf-openai` as `OpenAI` and `cf-anthropic` as the Claude family.

**`models_cost`** (each turn's cost, by log id) looks each id up on every declared gateway, so
callers need not know which gateway served the turn.

**`cost_daily`**:
- Anthropic's report is reconciled only against provider `anthropic` (BYOK). Cloudflare pays
  Anthropic for `cf-anthropic` on its own account, so that spend is never in Anthropic's report
  and is never subtracted from the gateway's figure.
- Cloudflare billing rows whose `ServiceName` names AI Gateway (credit purchases, the monthly
  AI Gateway usage invoice) are **left out of the Cloudflare lines and of the total**. The
  gateway's logs count the same spend per request and per day. Those rows show as
  `credits.billing_usd` for checking. "Workers AI" rows get their own line, not "Workers".
- `credits: {usage_usd, fee_usd, billing_usd, by_provider}`. `usage_usd` is the credits route's
  log cost. `fee_usd` is 5% of that, an estimate of the purchase fee spread over use.
- `total(day) = cloudflare(day, without AI Gateway rows) + byok_gateway(day) - gateway_anthropic(day)
  + anthropic_or_gateway(day) + credits_usage(day) + credits_fee(day)`.
- `ai_gateway.by_route: [{name: "BYOK"|"Credits", total, by_day}]`. `ai_gateway.by_provider`
  names `Anthropic`, `Anthropic (credits)`, `OpenAI (credits)`, then Other. `ai_gateway.total`
  stays the sum of both gateways' logs.
- Closed days stored before this change have no route. They read as BYOK, which is true:
  credits did not exist before step D.

Runs keep their per-turn cost (gateway cost by log id, or the local estimate).
`pigeon_core::model_prices` needs nothing new for `cf-anthropic` and `cf-openai`: the model ids
are the same and the prices are the provider's. `ai_agents_compare` groups by the choice text
(`cf-anthropic/claude-sonnet-5-5` and `anthropic/claude-sonnet-5-5` are two rows) and adds a
`route` field.

### 5. Workers AI: later, behind a live check

Three Workers AI models are documented with function calling and reasoning (above). Using them
takes:
- the REST endpoint (a third request shape in `ai_gateway`);
- a token with Workers AI Read (a change to core's purpose table, `split.rs`, and a check that
  the parent token may mint it);
- `workers_ai_billing_mode: "unified"` on `pigeon`;
- tier models chosen by the person.

It is phase 6, approved by the person (2026-11-25) with the tiers above. It is built in
parallel once phase 2 has landed and lands after phase 4; it stays off until its token slot is
set. Until then, `workers-ai/…` is refused with "not yet".

## Phases and branches

| Phase | What | Who | Touches |
| --- | --- | --- | --- |
| 0 | This plan | planner | `docs/plans/model-providers-cloudflare.md`, `docs/plans/README.md` |
| 1 | Gateways and routes in the model gateway: `Gateway {id, kind}`, `Provider {name, gateway, route}`, spec and drift per kind, the credits guard, per-job gateway, two-gateway logs, `models_daily` `by_route`, `models_cost` across gateways, step A's declaration | executor | `src/core/src/ai_gateway.rs`, `src/models/src/*`, `src/model-gateway/src/lib.rs`, smoke `models_*` |
| 2 | Choices: `cf-anthropic`, `cf-openai`, `openai/` retired, route in `ai_agents_models`, `ai_agents_status` and `ai_agents_compare` | executor | `src/ai-agents/src/*`, smoke `ai_agents_*` |
| 3 | `cost_daily`: AI Gateway billing rows out, Workers AI line, `credits` section, `by_route`, reconciliation on BYOK Anthropic only | executor | `src/capabilities/src/cost.rs`, smoke `platform_cost_daily_*` |
| 4 | Step C: the Anthropic route to `pigeon-anthropic`, `CREDITS_SINCE`, the old slot no longer declared | planner, **after the person sets the key** | `src/model-gateway/src/lib.rs` |
| 5 | Live check: an executor on `cf-anthropic/claude-haiku-5-5`; read its turn's log and cost | planner, **after step D and credits** | none |
| 6 | Workers AI (decision 5) | executor, if phase 5 passes and budget remains | `src/core/src/ai_gateway.rs`, `src/models/src/*`, `src/capabilities/src/split.rs`, `src/ai-agents/src/models.rs` |
| 7 | Docs: this plan's Log, how to use it | planner | `docs/plans/*` |

Phases 1, 2 and 3 run together, on disjoint crates. Phase 3 builds against the `models_daily`
shape above. The landing order is 1, 2, 3. Each branch is rebased onto main before it lands.
Their smoke scenarios all append to the end of `tests/smoke.rs`, so the second and third need a
trivial rebase.

## How to use it (once deployed)

- Credits models: `model: "cf-anthropic/claude-sonnet-5-5"`, `"cf-openai/gpt-6.2-sol"`, or a bare
  `"cf-anthropic"` / `"cf-openai"` for the role's tier. BYOK Claude: `"anthropic/…"` or a bare
  `claude-…` name.
- Defaults: `ai_agents_models` shows each slot's choice and route.
- Cost: `cost_daily` gives `ai_gateway.by_route`, `ai_gateway.by_provider` and `credits`.

## Open items (for the person)

1. **Done (2026-11-24).** The person set `pigeon-anthropic_anthropic_default` on the Access tab:
   a NEW Anthropic key, not the one in `pigeon_anthropic_default`. So until phase 4 deploys,
   Anthropic BYOK turns still use the old key on `pigeon`; from then on they use the new key on
   `pigeon-anthropic`.
2. **Make `pigeon` the credits gateway** (step D, after phase 4 is deployed). Cloudflare dashboard,
   AI, AI Gateway, gateway `pigeon`:
   1. Provider keys (BYOK): delete the Anthropic key (alias `default`; the OLD key). Also delete
      an OpenAI key if one is listed.
   2. Settings: turn off **Require provider credentials**. Leave **Authenticated Gateway** on.
   3. Settings: turn on **Zero Data Retention** if it is offered.
   4. Secrets Store (Account, Secrets Store): delete the secret `pigeon_anthropic_default` (the
      old key; revoke it in the Anthropic console too if it is not used elsewhere). Delete
      `pigeon_openai_default` if it exists.

   Pigeon sets no spend limit. You may add one under Settings, Spend limits, if you want a cap.
3. **Buy credits.** AI Gateway page, **Credits Available**, **Manage**, **Top-up credits**: enter
   the amount and confirm (5% fee; a credit or debit card set as the primary payment method, not
   PayPal or Apple Pay). Auto top-up is optional, from the same card.
4. **Optional:** on the account's auto-made `default` gateway, if it exists, turn on **Require
   provider credentials**, so nothing can spend credits through it.
5. **Decided (2026-11-25):** no spend limit in pigeon; OpenAI on credits only (`cf-openai/…`;
   `openai/…` is refused and points there); Workers AI yes, as phase 6, with tiers top
   `@cf/zai-org/glm-5.3`, middle `@cf/moonshotai/kimi-k2.7-code`, small none.

## Log

- 2026-11-25: Cloudflare's docs read (the list above). A live call is not possible from the
  planner's tools; it is phase 5.
- 2026-11-25: first design: a second gateway `pigeon-cloudflare` for credits. The person
  changed it: `pigeon` becomes the credits gateway, and a new `pigeon-anthropic` holds the
  Anthropic key. Decisions 1 and 3 follow the person's design.
- 2026-11-25: the spend-limit question got no answer in 5 minutes. Assumed: $20 a day, now on
  `pigeon`.
- 2026-11-25: the OpenAI question got no answer in 5 minutes. Assumed: OpenAI on credits only
  (`cf-openai/…` through `pigeon`), the OpenAI key slot retired, `openai/…` refused. If the
  person wants BYOK OpenAI later, it is a gateway `pigeon-openai`, built the way
  `pigeon-anthropic` is.
- 2026-11-25: phase 1 built by executor `Os2NbqLYAAvr` (CI `test` green, job
  `pkmQI1dPVzg34T3x5JASnY`) and landed as `934a532` (landing `RbM8IQWXmPw_v8JW`). Step A of
  decision 3 is now live:
  - both gateways are declared; the `anthropic` provider is still on `pigeon`;
  - the slot `pigeon-anthropic_anthropic_default` is declared (staged); `pigeon_openai_default`
    is not;
  - `cf-anthropic` and `cf-openai` exist in the model gateway, and the credits guard stops them
    while `pigeon` holds a key.

  Choices the executor made: `models_setup` now answers `{gateways: [{kind, gateway}],
  providers}`. `src/capabilities/src/models.rs` lists every gateway in the footprint. A job's
  cache-token rows stay keyed by the provider slug.
- 2026-11-25: the sandbox-time question got no answer. Assumed: no extension. The run had 30
  sandbox minutes for itself and its executors. Phase 2 (`55NtNZ02oPRu`) and phase 3
  (`FiCE7Jo8QHGw`) were given 8 minutes each, and both ran out before compiling anything.
  - Phase 2's branch `claude/ai-agent-55NtNZ02oPRu` (tip `2af7558`) holds the uncompiled
    ai-agents changes. Its report lists what is left: route in `ai_agents_status`, prompts.rs,
    the smoke assertions that use `openai/…`, and the new scenario.
  - Phase 3 has no code. Its executor confirmed that the Anthropic reconciliation already reads
    only `by_provider_model["anthropic"]`.
- **Until phase 2 lands**, an `openai/…` choice fails at `models_submit` ("no provider
  `openai`"), and `cf-…` choices are refused by ai-agents. Defaults (Anthropic BYOK) are not
  affected.
- **Next run:** finish phase 2 from its branch (start with `cargo check -p pigeon-ai-agents`).
  Build phase 3 from its brief in this plan. Then phase 4 after Open item 1, and phase 5 after
  Open items 2 and 3. Give it at least 60 sandbox minutes.
- 2026-11-25: planner run `A2apepRW1Vyb`. The person's decisions: no spend limit in pigeon
  (removed from the credits gateway's spec, from drift and from Open item 2); OpenAI on credits
  only; Workers AI yes (phase 6). Open item 1 is done with a new Anthropic key. Phase 2
  (executor `xXp120MbTCD2`) and phase 3 (executor `s26pnPrL6YvC`) run in parallel, 70 sandbox
  minutes each.
- 2026-11-24: phase 2 (executor `xXp120MbTCD2`, finishing `55NtNZ02oPRu`'s branch) landed as
  `aaf8c9a` (landing `MTnYTSjVVFHaafyO`); the spend-limit removal landed first as `d23563d`
  (landing `DWckQF2NNNu_9--T`). Phase 3 (executor `s26pnPrL6YvC`) landed as `2ba4b42` (landing
  `YAoNp0t9MW-KNhni`). Choices the executors made: a bare `openai` is refused like `openai/…`;
  `anthropic` and `cf-anthropic` take only `claude-…` names; old BYOK `openai` logs fall under
  Other in `cost_daily`'s `ai_gateway.by_provider`; `Workers AI` is an always-present
  Cloudflare line.
- 2026-11-24: phase 4 (step C): `anthropic` goes through `pigeon-anthropic` with the new key;
  `pigeon_anthropic_default` is no longer declared; `CREDITS_SINCE` is `2026-11-24T01:45:00Z`.

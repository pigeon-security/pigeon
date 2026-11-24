# Model providers, Part 3: models paid with Cloudflare credits

Status: **Option B** (the person, 2026-11-24): three gateways, each with one payer. Phases 1 to 4
landed under the earlier design; phases 8 to 10 rework them for option B (planner run
`flYJq203hPLb`). Follows [model-providers.md](model-providers.md) (Parts 1 and 2).

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

## What was true before Part 3 (main at `d4220be`, read 2026-11-25)

This section is history. Decision 1 describes the gateways under option B.

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

### 1. Three gateways, each with one payer (option B, the person, 2026-11-24)

| Gateway | Kind | Route | `byok_only` | Stored keys | Other settings |
| --- | --- | --- | --- | --- | --- |
| `pigeon-anthropic` | BYOK | Anthropic on the person's key | true | `pigeon-anthropic_anthropic_default` | `authentication: true`, logs on, the account's store |
| `pigeon-cloudflare` (new) | Credits | `cf-anthropic`, `cf-openai`, Workers AI, paid with credits | **false** | **none, ever** | `authentication: true`, logs on, `zdr: true`; no spend limit from pigeon |
| `pigeon` | Retired | none: no new traffic | true | none, once the person deletes the old key | its logs are still read, and always count as BYOK |

Why three: Cloudflare cannot rename a gateway, and the earlier design (turn `pigeon` itself into
the credits gateway) would have mixed two payers in one gateway's logs, told apart only by a
time (`CREDITS_SINCE`). Now the payer follows from the gateway alone. A stored key always wins
over credits on its gateway (above), so the credits gateway must never hold one. `byok_only`
stays on for `pigeon-anthropic`: if its key is missing or refused, an Anthropic BYOK turn fails
loudly (2044) and never bills credits by accident. `pigeon` keeps `byok_only` on as well, so
nothing sent to it by mistake could bill credits.

**Security.**
- **Who can spend credits:** anyone holding a token with AI Gateway Run on this account (any
  gateway without `byok_only`), or Workers AI Read (through the REST API). Pigeon holds no such
  standing token: core mints one per call or job, for at most 20 minutes, and revokes it after.
  The parent token stays in core. The person's own dashboard tokens are the other holders.
- `authentication` stays on for every gateway. It is required for Unified Billing anyway, and it
  cannot be turned off.
- Pigeon neither sets nor requires a spend limit (the person's decision). The person may set one
  on `pigeon-cloudflare` in the dashboard to cap a leaked token or a runaway run; drift never
  reports it.
- `zdr: true`: credits traffic goes only to provider endpoints that keep no prompts. Pigeon
  already keeps no payloads in its logs (`cf-aig-collect-log-payload: false`).
- The account's auto-created `default` gateway is a risk once credits are loaded: a Workers AI
  Read token sending REST calls without `cf-aig-gateway-id` would land there. Pigeon always names
  its gateway. The person may turn on "Require provider credentials" on `default` (Open items).

**Spec and drift.** `spec` takes the gateway's kind (`Byok`, `Credits` or `Retired`). `drift()`
compares the settings in the table, and it also reports:
- for the credits gateway: any stored provider key ("holds a stored key for anthropic: credits
  requests for it would bill that key instead");
- for a BYOK gateway: a missing key for a declared provider;
- for the retired gateway: any stored key ("delete it to retire this gateway"), and
  `byok_only` if it is off. Never "should be credits".

`ensure` creates a missing BYOK or credits gateway from its spec and never changes one that
exists. It never creates the retired gateway. `models_setup` hands keys only to BYOK gateways:
it never adds a key to `pigeon-cloudflare` or to `pigeon`.

**Credits guard.** Before the first request of a run or job on a credits provider, the model
gateway reads `pigeon-cloudflare`'s settings and provider configs (cached in the isolate for
5 minutes). It refuses the request, naming the gateway, if `byok_only` is on or the gateway
holds any stored key. So a credits turn can never bill a key by mistake, and no landing has to
wait for the person's dashboard steps.

**BYOK readiness.** Before a request on a BYOK provider (a run, a job's submit and its start),
the model gateway checks that the provider's gateway exists, has `byok_only` on, and holds a
stored key for the provider. A secret set in the store is not enough: that is how phase 4 went
wrong (Log, 2026-11-24). Only a success is cached (5 minutes). A refusal says what to do. A
failure to read the gateway lets the request go ahead, so a Cloudflare API error does not stop
every agent turn; the request then fails or succeeds on its own. `models_providers` answers each
provider's readiness (`ready`, `why`).

### 2. Model names: who pays is in the name

A choice is `provider/model`, and the provider names its route:

| Choice provider | Paid by | Gateway | Path and dialect | Tiers (top / middle / small) |
| --- | --- | --- | --- | --- |
| `anthropic` | the person's Anthropic key (BYOK) | `pigeon-anthropic` | `anthropic`, Messages | `claude-opus-5-5` / `claude-sonnet-5-5` / `claude-haiku-5-5` |
| `cf-anthropic` | Cloudflare credits | `pigeon-cloudflare` | `anthropic`, Messages | the same Claude models |
| `cf-openai` | Cloudflare credits | `pigeon-cloudflare` | `openai`, Chat Completions | `gpt-6.2-astra` / `gpt-6.2-sol` / `gpt-6.1-terra` |
| `workers-ai` (phase 6) | Cloudflare credits | `pigeon-cloudflare` | REST `ai/v1/chat/completions`, OpenAI dialect | `@cf/zai-org/glm-5.3` / `@cf/moonshotai/kimi-k2.7-code` / none |

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

Turns are separate jobs. A job's gateway is fixed when it is submitted (the job's `gateway`
field; a job stored without one uses the first declared gateway, `pigeon-anthropic`). A deploy
therefore changes only the turns that start after it.

| Step | Who | What | `anthropic` turns go to | Safe because |
| --- | --- | --- | --- | --- |
| A | phase 1 landed (`934a532`) | `pigeon-anthropic` declared, its key slot declared. | `pigeon` (old key) | nothing moved |
| B | **the person** (done) | Set `pigeon-anthropic_anthropic_default` (a new Anthropic key). | `pigeon` (old key) | |
| C | phase 4 landed (`03b263b`) | The Anthropic route moved to `pigeon-anthropic`. | `pigeon-anthropic` | **it was not**: the gateway did not exist yet, and turns failed with 2001 (Log). Fixed by hand. Phase 8 adds the readiness check that refuses such a turn with a plain message. |
| D | phase 8 lands | Option B: `pigeon-cloudflare` is the credits gateway, created by `models_setup`; `pigeon` is declared retired; `CREDITS_SINCE` is removed; the BYOK readiness check. | `pigeon-anthropic` | `pigeon-anthropic` exists and holds the key, checked before turns (cached 5 minutes) |
| E | **the person** | Retires `pigeon` (deletes its stored key and the old secret), confirms `pigeon-cloudflare`'s settings, buys credits (Open items 1 to 3). | `pigeon-anthropic` | nothing routes to `pigeon` any more |
| F | planner | The live check (phase 5): one turn on `cf-anthropic/claude-haiku-5-5`. | | the credits guard lets it through only once `pigeon-cloudflare` is right |

**No key is ever set on `pigeon-cloudflare`, and none is ever added again to `pigeon`.**

### 4. Cost: three gateways' logs, each dollar once

**`models_daily`** reads the logs of every declared gateway (5,000 at most each, `truncated` per
gateway), and labels each log with a provider `name` and a `route` **by its gateway alone**:
- on `pigeon-anthropic` (BYOK): `anthropic`, BYOK;
- on `pigeon` (retired): BYOK under the log's own provider (`anthropic`, `openai`), whatever its
  time. Every request it served was paid with a stored key: it never had credits on;
- on `pigeon-cloudflare` (credits): `cf-anthropic`, `cf-openai`, `cf-{provider}` for any other
  provider, and `workers-ai` (phase 6), all credits.

There is no time-based labelling any more: `CREDITS_SINCE` is gone. It was
`2026-11-24T01:45:00Z`, set by phase 4, and option B removed it before any stored day could be
labelled credits by it.

Fields:
- `by_route: {byok: {day: usd}, credits: {day: usd}}`;
- `by_provider` and `by_provider_model` keyed by the provider `name`;
- `gateways: {id: {logs, truncated}}`.

`family_of` reads `cf-openai` as `OpenAI` and `cf-anthropic` as the Claude family.

**`models_cost`** (each turn's cost, by log id) looks each id up on every declared gateway, so
callers need not know which gateway served the turn.

**`cost_daily`**:
- Anthropic's report is reconciled only against provider `anthropic` (BYOK: `pigeon-anthropic`
  and the retired `pigeon`). Cloudflare pays Anthropic for `cf-anthropic` on its own account, so
  that spend is never in Anthropic's report and is never subtracted from the gateway's figure.
- Cloudflare billing rows whose `ServiceName` names AI Gateway (credit purchases, the monthly
  AI Gateway usage invoice) are **left out of the Cloudflare lines and of the total**. The
  gateway's logs count the same spend per request and per day. Those rows show as
  `credits.billing_usd` for checking. "Workers AI" rows get their own line, not "Workers"; for
  Workers AI calls through `pigeon-cloudflare`, see decision 5.
- `credits: {usage_usd, fee_usd, billing_usd, by_provider}`. `usage_usd` is the credits route's
  log cost. `fee_usd` is 5% of that, an estimate of the purchase fee spread over use.
- `total(day) = cloudflare(day, without AI Gateway rows) + byok_gateway(day) - gateway_anthropic(day)
  + anthropic_or_gateway(day) + credits_usage(day) + credits_fee(day)`.
- `ai_gateway.by_route: [{name: "BYOK"|"Credits", total, by_day}]`. `ai_gateway.by_provider`
  names `Anthropic`, `Anthropic (credits)`, `OpenAI (credits)`, then Other. `ai_gateway.total`
  is the sum of all three gateways' logs.
- Closed days stored before option B read as BYOK where they hold no credits figure, which is
  true: no credits request had succeeded before `pigeon-cloudflare` existed.

Runs keep their per-turn cost (gateway cost by log id, or the local estimate).
`pigeon_core::model_prices` needs nothing new for `cf-anthropic` and `cf-openai`: the model ids
are the same and the prices are the provider's. `ai_agents_compare` groups by the choice text
(`cf-anthropic/claude-sonnet-5-5` and `anthropic/claude-sonnet-5-5` are two rows) and adds a
`route` field.

### 5. Workers AI, through `pigeon-cloudflare`

Three Workers AI models are documented with function calling and reasoning (above). Using them
takes:
- the REST endpoint (a third request shape in `ai_gateway`), always with `cf-aig-gateway-id:
  pigeon-cloudflare`, so the account's `default` gateway never serves it;
- a token with Workers AI Read, set by the person in the slot `pigeon_workers_ai_token`;
- `workers_ai_billing_mode: "unified"` on `pigeon-cloudflare` (in its spec and checked by the
  credits guard);
- tiers chosen by the person (2026-11-25): top `@cf/zai-org/glm-5.3`, middle
  `@cf/moonshotai/kimi-k2.7-code`, small none.

It is phase 6. It stays off until the token is set: until then `workers-ai/…` is refused with a
message naming the slot.

**Counted once.** A Workers AI call through `pigeon-cloudflare` shows in the gateway's log (with
its cost, provider `workers-ai`, route credits). With Unified Billing it is paid from credits,
whose billing rows are AI Gateway rows and already left out. But Cloudflare's billing may also
list it under `ServiceName` "Workers AI" (postpaid mode, or if Cloudflare books it that way),
and that is not verified. So `cost_daily` takes, per day, the gateway's `workers-ai` log cost off
the Cloudflare "Workers AI" line, never below zero, and shows what it took off as
`credits.workers_ai_overlap_usd`. Whichever way Cloudflare books it, each dollar counts once:
under credits, from the gateway's log. Pigeon makes no other Workers AI calls, so the rest of
the "Workers AI" line is other use of the account.

## Phases and branches

| Phase | What | Who | Touches |
| --- | --- | --- | --- |
| 0 | This plan | planner | `docs/plans/model-providers-cloudflare.md`, `docs/plans/README.md` |
| 1 | Gateways and routes: `Gateway {id, kind}`, `Provider {name, gateway, route}`, spec and drift per kind, the credits guard, per-job gateway | executor, **landed** `934a532` | `src/core/src/ai_gateway.rs`, `src/models/src/*`, `src/model-gateway/src/lib.rs` |
| 2 | Choices: `cf-anthropic`, `cf-openai`, `openai/` retired, route on the ai-agents surfaces | executor, **landed** `aaf8c9a` | `src/ai-agents/src/*` |
| 3 | `cost_daily`: AI Gateway billing rows out, Workers AI line, `credits`, `by_route`, reconciliation on BYOK Anthropic only | executor, **landed** `2ba4b42` | `src/capabilities/src/cost.rs` |
| 4 | The Anthropic route to `pigeon-anthropic`; `CREDITS_SINCE` set | planner, **landed** `03b263b` (too early: Log) | `src/model-gateway/src/lib.rs` |
| 8 | **Option B**: kinds BYOK / Credits / Retired; `pigeon-cloudflare` created and used by the credits providers; `pigeon` retired; `CREDITS_SINCE` and all time-based labelling removed; the BYOK readiness check; `models_providers` readiness | executor | `src/core/src/ai_gateway.rs`, `src/models/src/*`, `src/model-gateway/src/lib.rs`, `src/platform/src/settings.rs`, smoke |
| 6 | Workers AI (decision 5), rebased onto phase 8: through `pigeon-cloudflare`, counted once in `cost_daily` | executor, after phase 8 | `src/core/src/ai_gateway.rs`, `src/models/src/*`, `src/model-gateway/*`, `src/ai-agents/src/models.rs`, `src/capabilities/src/cost.rs`, smoke |
| 9 | Docs: this plan for option B | planner | `docs/plans/model-providers-cloudflare.md` |
| 5 | Live check: an executor turn on `cf-anthropic/claude-haiku-5-5`; read its log on `pigeon-cloudflare` and its cost under credits | planner, **after Open items 1 to 3** | none |

The landing order is 9 (or with 8), 8, 6, then 5. Phase 6 starts from phase 8's branch once it
is green, and is rebased onto main before it lands.

## How to use it (once deployed)

- Credits models (through `pigeon-cloudflare`): `model: "cf-anthropic/claude-sonnet-5-5"`,
  `"cf-openai/gpt-6.2-sol"`, `"workers-ai/@cf/zai-org/glm-5.3"` (phase 6, once its token is set),
  or a bare `"cf-anthropic"` / `"cf-openai"` / `"workers-ai"` for the role's tier.
- BYOK Claude (through `pigeon-anthropic`): `"anthropic/…"` or a bare `claude-…` name. These
  are the defaults.
- Nothing routes to `pigeon`; its old logs still count, as BYOK.
- Defaults: `ai_agents_models` shows each slot's choice and route.
- Readiness: `models_providers` lists each gateway (kind, whether it exists, its keys) and each
  provider's `ready` and `why`. `models_setup` creates a missing BYOK or credits gateway, hands
  keys to BYOK gateways only, and reports drift for all three.
- Cost: `cost_daily` gives `ai_gateway.by_route`, `ai_gateway.by_provider` and `credits`.

## Open items (for the person)

1. **Retire `pigeon`**, only once nothing routes to it (true from phase 8's deploy). Cloudflare
   dashboard, AI, AI Gateway, gateway `pigeon`:
   1. Provider keys (BYOK): delete the stored Anthropic key (alias `default`, the OLD key), and
      an OpenAI key if one is listed.
   2. Settings: keep **Require provider credentials** ON, so nothing sent there can bill credits.
   3. Account, Secrets Store: delete the secret `pigeon_anthropic_default` (the old key; revoke
      it in the Claude Console too if nothing else uses it), and `pigeon_openai_default` if it
      exists.

   `models_setup` then reports no drift for `pigeon`.
2. **Confirm `pigeon-cloudflare`'s settings** (`models_setup` creates it): **Require provider
   credentials** OFF, **Authenticated Gateway** ON, **Zero Data Retention** ON, no provider keys.
   Pigeon sets no spend limit; you may add one under Settings, Spend limits.
3. **Buy credits.** AI Gateway page, **Credits Available**, **Manage**, **Top-up credits**: enter
   the amount and confirm (5% fee; a credit or debit card set as the primary payment method, not
   PayPal or Apple Pay). Auto top-up is optional, from the same card.
4. **Optional:** on the account's auto-made `default` gateway, if it exists, turn on **Require
   provider credentials**, so nothing can spend credits through it.
5. **Optional, for Workers AI:** a Cloudflare API token with Workers AI Read for this account,
   set as `pigeon_workers_ai_token` on the Access tab. Then its binding in
   `src/model-gateway/wrangler.toml` is uncommented and deployed (phase 6's note there).

Decided before (2026-11-25): no spend limit in pigeon; OpenAI on credits only (`cf-openai/…`;
`openai/…` is refused and points there); Workers AI with tiers top `@cf/zai-org/glm-5.3`, middle
`@cf/moonshotai/kimi-k2.7-code`, small none. Done (2026-11-24): the new Anthropic key in
`pigeon-anthropic_anthropic_default`, on `pigeon-anthropic`.

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
- 2026-11-24: **phase 4 landed too early.** The secret `pigeon-anthropic_anthropic_default` was
  set, but the gateway `pigeon-anthropic` did not exist and held no key. Every Anthropic turn
  after the deploy got 400, code 2001 ("Please configure AI Gateway"), and planner run
  `A2apepRW1Vyb` failed on its next turn. The check before landing looked only at the secret.
- 2026-11-24: **fixed by hand.** `models_setup` created `pigeon-anthropic` (BYOK, no drift) and
  added the key (alias `default`); a Haiku call through it succeeded. `models_setup` reported
  drift on `pigeon` (then declared credits): `byok_only` true, `zdr` false, a stored Anthropic
  key. `pigeon` was not changed and still holds the old key.
- 2026-11-24: phase 6 (Workers AI) built and CI-green on `claude/ai-agent-tHKTib1pE9j6`
  (`8bb35bf`), not landed; it targeted `pigeon`.
- 2026-11-24: **the person changed the design to option B** (decision 1): Cloudflare cannot
  rename a gateway, so `pigeon` is retired, a new `pigeon-cloudflare` takes credits, and
  `pigeon-anthropic` keeps the Anthropic key. Each gateway has one payer, so the payer follows
  from the gateway and `CREDITS_SINCE` goes. Planner run `flYJq203hPLb` reworks it: phase 8
  (gateways, routes, the readiness check), phase 6 rebased onto it, phase 9 (this plan).

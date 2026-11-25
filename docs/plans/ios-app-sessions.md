# iOS app: Sessions, Needs you, Settings, and a Cost tab that shows real figures

Status: **landed; release pending** (planner run `claude/ai-agent-vwR5IpnxM9nc`). The Log at the end says what
landed.

The person's brief: build the next iteration of the iOS app (`ios/Pigeon`) from their confirmed
mockup, fix the Cost tab at its source, land phase by phase, then run `ios-release` at main's head.
It follows [ios-app-next.md](ios-app-next.md): data comes only through MCP tools; new data is a new
tool or a new field on an existing tool's answer; no new HTTP routes; every new field is additive
and the app reads it as optional.

## What the mockup asks for

1. Three tabs: **Sessions, Cost, Settings** (were Approvals, Status, Cost, Instances).
2. Every page title is left-aligned on the same row as its top-right control.
3. Only Sessions has the instance switcher (avatar and chevron, top right). Its menu lists the
   instances (current one ticked) and a last row "All instances", which opens Settings.
4. **Sessions**: a top card that opens **Needs you** ("N things need your call" with a count badge
   and a sub-line such as "1 question · 1 decision to check"; or a check icon, "You're all caught
   up", "Your agents have what they need"). Then "Running now": live planners with all their
   executors (finished ones dimmed, failed ones marked) until the planner ends, and live lone
   executors; ended ones hidden behind "N finished sessions today ›". A planner card has its name,
   elapsed time, state chip, budget bar ("$X of $Y budget · n of m executors finished") and Note,
   Extend, Stop inside the card. Nothing sits under the floating tab bar. Names, never run ids.
5. **Needs you**: "Your call" (questions: run name, a one-sentence headline, labelled facts, options
   keyed A, B, C… plus Other, Send answer, Stop run, "No answer in M min S s and the run takes A")
   and "Decided for you" (questions that timed out: "It chose", "Instead of", a facts box, Looks
   right, Correct it, Stop run). Approvals (calls, drafts, plans, other, landings) stay there too.
6. **Settings**: "Instances" first (tap to switch; subtitle repo · linked/core account), then a
   section named after the current instance with today's settings (Users, Secrets, Pause all
   agents, the signed-in card with Sign out). The Instances tab goes away.
7. **Cost**: real figures; a source with no data says so on its card instead of $0.00.

## What is true today (main at `1976477`, read for this plan)

- `ai_agents_status` runs carry `title` (the brief's first line, 80 chars) and `elapsed_ms`; no name.
- `ask_person {question, options?}` makes `Wait::Answer {call, question, options, since}`. After
  5 minutes (`run::ANSWER_PATIENCE_MS`) the index's `assume` stores `{question, at}` on the run and
  the model is told to pick the conventional option. `ai_agents_inbox` answers `assumed` as
  `{run, question, at}`: what the run chose is only in its report.
- `instances_tabs` (since removed: the app reads `console_manifest`) named the tabs `approvals`, `status`, `cost`, `instances` (home) and `home`,
  `instances` (linked). The released app (build 42) reads those names, so the server keeps them;
  the new app maps them (below).
- **Cost.** `cost_daily` joins billable usage, `models_daily` (AI Gateway logs), `models_billing`
  (Anthropic) and the runs' roles, each inside `within` (25 s). A source that fails goes to
  `unavailable`, and the app then draws $0.00. Live evidence for this plan: `models_usage` over
  24 hours answered $98.88 from 2,000 logs, cut short (`truncated: true`). So the gateway has the
  data. `models_daily` pages up to 5,000 logs, 50 per page, one page after another: up to 100
  Cloudflare API calls per answer. A day already has more than 2,000 logs, so the listing likely
  runs past the 25 s limit and lands in `unavailable`. The Cloudflare side (billable usage) needs
  the same check. Phase 3 confirms or rejects this from the code and fixes it.

## Decisions

### 1. A run's name

- `ai_agents_start` and `spawn_executor` take an optional `name`: 1 to 60 characters on one line,
  trimmed. A longer or multi-line name is an error that says so.
- The index keeps it (`runs.name`, a new migration). `ai_agents_status` and the runs inside
  `ai_agents_inbox` answer `name`: the given name, else the title cut to 40 characters on a word
  boundary. So `name` is always a string; `name_given` says whether someone chose it.
- The planner prompt and the `spawn_executor` description ask for a 2 to 5 word name, such as
  "Cost tab fix".

### 2. Structured questions

`ask_person` gains optional parts; `question` stays required and is the full text for older readers:

| Field | Shape | Use |
| --- | --- | --- |
| `headline` | string, at most 140 chars, one line | the one short sentence the app shows as the title |
| `facts` | up to 6 of `{label, value, mono?}`; label at most 24 chars, value at most 300 | labelled rows such as Blocked, Against, Ready; `mono` for commits and file names |
| `options` | 2 to 6 strings, as today | the answers offered |
| `conventional` | integer index into `options` | the option the run takes when no answer comes |

`conventional` without `options`, or out of range, is an error that says so. The `Wait::Answer` keeps
the new parts (`#[serde(default)]`, so stored waits still read) and gains `answer_by` (ms):
`since + 5 min`. On a time out with a known `conventional`, the run is told that option by name and
takes it, so what the inbox says it chose is what it does. Without one, the model picks as today and
`chose` is null.

`ai_agents_inbox`:
- `waiting` runs: their `waiting` object carries `question`, `headline`, `facts`, `options`,
  `conventional`, `since` and `answer_by`, and the run carries `name`.
- `assumed` entries: `{run, name, title, branch, question, headline, facts, options, conventional,
  chose, instead, at}`. `chose` is `options[conventional]` (or null); `instead` is the other
  options. Entries stored before this change have only `question` and `at`; the rest is null or
  empty.

The app falls back to `question` when `headline` is missing, and shows "see its report" when
`chose` is null.

### 3. The app

- **Tabs.** `AppTab` becomes `sessions`, `cost`, `settings`, `home`. A server's `approvals` or
  `status` maps to `sessions` and `instances` to `settings`; a linked instance shows Home and
  Settings. The Home tab keeps the switcher, as it stands in for Sessions on a linked instance.
- **Title row.** One shared header in `Views/Screen.swift`: the title left-aligned at the title
  size, the control on the same row at the right; the system large title is not used. Bottom
  padding clears the floating tab bar (`safeAreaInset` or `contentMargins`), so buttons in the last
  card are never under it.
- **Needs you** answers with `ai_agents_answer` (an option's text, or the Other text), "Correct it"
  with `ai_agents_note`, Stop run with `ai_agents_stop`. "Looks right" hides the entry on this
  device (the inbox cursor in `UserDefaults`, as "Fine, carry on" did). Approvals keep the passkey
  flow (`Approver.swift`) unchanged.
- **Wording.** "You're all caught up" and "Your agents have what they need"; never "Nothing waits
  for you".

### 4. Cost

The fix is made where the figures are lost (phase 3 finds where), not in the app. `cost_daily`
also says, per source, whether it has data: `cloudflare.status` and `ai_gateway.status` are
`ok`, `empty` (the source answered with nothing for the range) or `unavailable` (it failed), with
`why` when not `ok`. The app shows "No data: <why>" on such a card instead of $0.00.

### 5. Tests

New scenarios go in `tests/smoke.rs` itself. `policy_check add_test tests/smoke/ai_agents.rs`
answers deny (rule `smoke-suite-fixed`), so the area modules get no new `#[test]`.

## Phases and branches

| # | What | Paths | Lands |
| --- | --- | --- | --- |
| 0 | This plan | `docs/plans/ios-app-sessions.md`, one Log line in `ios.md` | docs only |
| 1 | Server: run names, structured questions, inbox shapes | `src/ai-agents/src/*`, `tests/smoke.rs` | ai-agents (and core if shared code changes) |
| 2 | App: tabs, title row, switcher, Sessions, Needs you, Settings | `ios/` except `CostScreen.swift`'s body | `ios-check` |
| 3 | Server: Cost fix and per-source status | `src/capabilities/src/cost.rs`, `src/models/*`, `src/core/src/ai_gateway.rs` as needed, `tests/smoke.rs` | core, model-gateway |
| 4 | App: Cost card says when a source has no data; title row | `ios/Pigeon/Views/CostScreen.swift` | `ios-check` |
| 5 | Release | `ios-release` at main's head | TestFlight |

Phases 1, 2 and 3 run at once (2 is written against the contracts above); they land in the order
1, 2, 3, then 4, each rebased on main and CI green (`test`, and `ios-check` for app branches).

## Log

- 2026-11-23: plan written. Read: ios-app-next.md, ios.md, ai-agents.md §6, agent-costs.md, the policy and CI
  files, `cost.rs`, `models_daily`, the run index and `ask_person`. Live: `models_usage` 24 h =
  $98.88 (truncated at 2,000 logs); the last release is build 42 (`f0d28d4`).
- 2026-11-23: phase 2 app built (branch `claude/ai-agent-NeRojTZPJpPG`): three tabs, shared title row, Sessions, Needs you, Settings with Instances, Home; StatusView, ApprovalsView and InstancesView removed. Cost has only its title line changed.
- 2026-11-23: phase 4: the Cost screen shows, per source, "No data" plus the reason when Cloudflare or the AI Gateway is unavailable, "No cost in this range" when empty, and a "Without … : no data" line under the total when a source is missing.
- 2026-11-23: phase 1 landed (`d4220be`, executor `ZzGMsB2dzlB6`): run `name` (`ai_agents_start`,
  `spawn_executor`; `name`/`name_given` on status and inbox runs), `ask_person` `headline`, `facts`,
  `conventional`, `answer_by`; `assumed` entries carry `name`, `branch`, `chose`, `instead`; a
  question that times out with a known conventional option takes that option. Smoke
  `ai_agents_run_names_and_structured_questions`. Deployed ai-agents and core.
- 2026-11-23: phase 2 landed (`75a1d8b`, executor `NeRojTZPJpPG`); `ios-check` green.
- 2026-11-23: phase 3 landed (`9a3583b`, executor `3hjPz2LYhsMg`). Cause of the AI Gateway $0:
  `ai_gateway::logs_with` read up to 100 pages one after another, which ran past `cost_daily`'s
  25 s source limit, so the source landed in `unavailable` and the app drew $0.00. Pages after the
  first are now read 10 at a time. The Cloudflare cause was not found in the code: `billing_days`
  now also reads FOCUS billing-period columns and other row wrappers, and `cloudflare.status` and
  `why` show live whether the call fails or comes back empty. `cost_daily` gains
  `cloudflare.status/why` and `ai_gateway.status/why`. Smoke `platform_cost_daily_sources_status`.
  Not looked at: the `executors_cost` "$0" lead on old planners.
- 2026-11-23: phase 4 (executor `zS5acEv7SuQG`, `ef8af81`) was refused by the Lander after phase 2
  was rebased; it was cherry-picked onto main on the planner's branch and landed from there.

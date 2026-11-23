# iOS app, next iteration: the confirmed mockup

Status: **planned** (2026-11-24, overnight planner run `claude/ai-agent-Hb0AUUN62vc9`). Built by
four executor branches, landed one at a time. The Log at the end says what landed.

The person's brief: build the next iteration of the iOS app (`ios/Pigeon`, SwiftUI) from their
confirmed mockup. Every data operation goes through MCP tools. Where data is missing, add new MCP
tools on the server; add no HTTP routes. The passkey write routes `/console/write/...` stay as
they are. Architecture is removed from the app. The base plan is [ios.md](ios.md) §5.

## What the mockup asks for, and where its data comes from

| Screen | Data | New on the server |
| --- | --- | --- |
| Approvals: one list of what waits on the person | `approvals_list` (calls, drafts, plans, other, landings) and `ai_agents_inbox` (`waiting` runs with an `answer` wait, `assumed` notices) | `plain` on every `approvals_list` item: one sentence saying what approving does |
| Status: a card per planner with its executors | `ai_agents_status` (runs, `parent_run`, `paused`) | `title` and `elapsed_ms` on every run |
| Cost: 2×2 grid, two stacked bar charts | none today gives daily buckets | `cost_daily` |
| Instances: list, switcher, Home for a linked instance | `instances_list` | `instances_tabs`: tabs and facts per instance |
| Settings: who is signed in, counts, pause, sign out | `users_whoami`, `secrets_list`, `ai_agents_status.paused`, `ai_agents_pause`/`ai_agents_resume` | `initials` and `picture` on `users_whoami` |
| Users and user detail | `users_list` has links and passkey counts, not who acts for whom by kind | `users_people` |

Writes the app makes: `ai_agents_answer`, `ai_agents_stop`, `ai_agents_note`,
`ai_agents_extend`, `ai_agents_pause`, `ai_agents_resume` (all MCP), and the existing passkey
writes for approvals (unchanged).

## Decisions

### 1. New data is added as new tools, or as new fields on an existing tool's answer

A field is added where the data belongs to items an existing tool already answers (a run's title,
an approval's sentence, the caller's initials). A tool is added where the answer is new (daily
cost, people, tabs). Every addition is additive: old fields keep their names and shapes, so the
current app and the console keep working. The app reads every new field as optional, so it
also works against a server that does not have it yet.

### 2. The tool contracts

**`approvals_list`**, each item of `calls`, `drafts`, `plans`, `other` and `landings` gains
`plain` (string): one plain sentence, built server side from the record. Examples: "Give a
deploy token to Executor (acting for you)", "Land claude/x at 1a2b3c4", "Apply the plan for
noisypigeon". An unknown tool falls back to "Run <tool> with <n> argument(s)". Items also carry
`created_ms` where the record has it. Pure function, smoke-tested.

**`ai_agents_status`** (and the runs inside `ai_agents_inbox`), each run gains:
- `title`: the brief's first non-empty line, with a leading `#`, `Brief:` or `Task:` removed,
  cut to 80 characters on a word boundary; "Untitled run" when empty.
- `elapsed_ms`: `(ended_at or now) - (started_at or created_at)`, never negative.

**`cost_daily {range}`**, `range` is `today`, `7d` (default) or `all`:

```json
{
  "range": "7d",
  "days": ["2026-11-18", "…", "2026-11-24"],
  "total": 12.34,
  "cloudflare": {
    "total": 4.5,
    "lines": [
      { "name": "CI Containers", "total": 1.2, "by_day": { "2026-11-24": 0.3 },
        "kind": "billed", "granularity": "day" }
    ]
  },
  "ai_gateway": {
    "total": 7.84,
    "by_model": [ { "name": "Opus", "total": 5.1, "by_day": { "2026-11-24": 1.0 } } ],
    "by_role":  [ { "name": "planner", "total": 4.0, "by_day": { "2026-11-24": 0.7 } } ]
  },
  "notes": ["Cloudflare bills some lines per billing period; those are spread evenly over the period's days so far and marked granularity \"period\"."],
  "unavailable": [ { "source": "billable usage", "why": "…" } ],
  "truncated": false,
  "stored_from": "2026-11-01"
}
```

- Days are UTC dates, oldest first. `today` answers one day; `7d` seven; `all` every day from the
  first stored day to today.
- Cloudflare lines, in this order: `CI Containers`, `Workers`, `D1`, `R2`, then `Other` when
  anything else is billed. From Cloudflare's billable usage (the source `observability_summary`
  already reads), mapped by service name. A row whose charge period is one day goes on that day
  (`granularity: "day"`). A row covering longer is spread evenly over the period's days up to
  today (`granularity: "period"`), and `notes` says so. That is the best Cloudflare gives.
- AI Gateway by model: `Opus`, `Sonnet`, `Haiku`, then `Other`, from the gateway's logs, as the
  console's chart already sums them (`pigeon_models::daily`). By role: `planner`, `executor`,
  `other`. A log line whose `task` starts `ai-agent/<run>/` takes that run's role from
  `ai_agents_status`; anything else, or a run not found, is `other`.
- **Daily rollups.** The gateway's logs reach back 31 days and billable usage covers the current
  period, so `all` needs what was seen before. Each call stores every closed UTC day it computed
  (any day before today) in the Directory's settings, one key per month (`cost.daily.YYYY-MM`)
  plus an index of months. `all` reads the stored months and adds the live window on top, the
  live values winning for the days they cover. A day nobody asked about while it was in the
  window is missing; `stored_from` says where the record begins. No cron is added: the minute
  cron belongs to site deploys, and reading is often enough for one person's app.
- Cost is the platform's own account only. The costs another planner is reworking tonight (the
  AI agent runs' own metering) are not read; this tool reads the gateway's logs, which that
  work keeps.

**`users_whoami`** keeps its answer and gains `initials` (one or two letters from the name of the
standard user the caller is, or acts for) and `picture` (always `null`: pigeon keeps no profile
pictures; the app draws the initials).

**`users_people`**, the people who can sign in, each with who acts for them:

```json
{
  "people": [
    { "id": "u1", "name": "Sample Person", "initials": "SP", "picture": null,
      "role": "Person", "added_ms": 1763900000000,
      "passkeys": [ { "id": "k1", "name": "iPhone", "created_at": 1763900000000, "last_used_at": null } ],
      "acting_for_me": [ { "id": "s1", "name": "Pigeon for iOS", "kind": "app", "added_ms": 1763900000000 } ] }
  ],
  "unlinked": 2
}
```

`kind` is `app` for a service user with an OAuth client, else `service`. `unlinked` counts
service users acting for no one; they are not listed. Times are milliseconds.

**`instances_tabs`**, the app's view of each instance:

```json
{
  "current": "pigeon-security/platform",
  "scoping": "app",
  "instances": [
    { "id": "pigeon-security/platform", "title": "Pigeon Platform", "account_kind": "home",
      "zones": ["pigeon.dev"], "tabs": ["approvals", "status", "cost", "instances"],
      "settings": true, "facts": { "website": null, "site": null, "repository": null } },
    { "id": "noisypigeon", "title": "Noisy Pigeon", "account_kind": "linked",
      "zones": ["example.com"], "tabs": ["home", "instances"], "settings": false,
      "facts": { "website": "https://example.com", "site": "example.com", "repository": "owner/name" } }
  ]
}
```

The instance in the home account has the four data tabs and Settings; a linked instance has
Home and Instances. `current` is the home account's instance.

### 3. Switching instances

The data tools (`approvals_list`, `ai_agents_status`, `cost_daily`, the users tools) act on the
platform's own data; none takes an instance argument, and the linked instances run no agents and
bill nothing to pigeon's account today. So there is one MCP server and switching happens in the
app:
- the switcher lists every instance from `instances_tabs`, with the current one ticked;
- choosing the platform shows its four tabs;
- choosing a linked instance shows its tabs from `instances_tabs` (Home and Instances): an empty
  state, the instance's facts, and "Switch to Pigeon Platform";
- "All instances" opens the Instances tab.

The choice is kept on the device (`UserDefaults`). Nothing is fetched from, or sent to, a linked
account. **Follow-up:** when a linked instance gets its own agents, approvals or costs, add an
`instance` argument to those tools on this server and let `instances_tabs` list the tabs it then
has; the app already draws whatever tabs that tool names.

### 4. The app

- **Look.** The mockup's colours are the generated tokens' values already (`Tokens.background`,
  `dot`, `card`, `shadow`, `accent`, `muted`, `warn`, `good`, `chip`, `line`); `tokens.css` does
  not change. The type scale and the chart colours are the app's own, in
  `ios/Pigeon/Design/Components.swift`, as the console keeps its chart colours in `console.css`.
  So nothing in `src/core` changes for the look, and no Worker redeploys for it.
- **Type scale** (`AppFont`): 30 bold titles and the cost Total; 20 bold source totals; 17
  semibold card titles; 17 regular rows; 15 descriptions and buttons; 13 semibold muted labels;
  13 regular times and meta; 11 chart axis and values; 10 tab bar. Sentence case everywhere; no
  capitals labels.
- **Chart colours** (`ChartColour`), light / dark: violet `#4E3FA6` / `#A898F5`, teal `#2E9E92` /
  `#66D6C4`, ochre `#B07A10` / `#E0B04A`, magenta `#9C2462` / `#C9508F`. Checked by simulating
  deuteranopia, protanopia and tritanopia (Machado 2009 matrices, CIELAB distance): the closest
  pair under any of them is 18 ΔE apart (teal and magenta, deuteranopia, never adjacent in a
  stack); the first palette tried failed at 2 ΔE. Cloudflare: CI Containers, Workers, D1, R2 in that order; AI Gateway by model: Opus,
  Sonnet, Haiku; by role: planner, executor, other. `Other` is `Tokens.muted`.
- **Tabs.** (Replaced on 2026-11-23 by the system `TabView` and an instance menu; see the last section.) A custom tab bar: Approvals
  (badge with the count), Status, Cost, Instances. Holding Instances for 0.5 s opens the switcher
  above the bar; a tap opens the tab.
- **Screens** as the mockup says: Approvals, Status, Cost, Instances, Home (linked instance),
  Settings, Users, User detail. Architecture, Access and Passkeys screens are removed.
- "Fine, carry on" on an assumed notice hides it on this device (the inbox's `since` cursor is
  kept in `UserDefaults`); it calls nothing.

## Phases and branches

| # | Branch concern | Paths | Lands |
| --- | --- | --- | --- |
| 0 | This plan | `docs/plans/ios-app-next.md`, one Log line in `ios.md` | docs only |
| 1 | Server: `plain`, run `title`/`elapsed_ms`, `users_people`, `users_whoami` initials, `instances_tabs` | `src/capabilities`, `src/ai-agents`, `tests/smoke.rs` | core and ai-agents deploy |
| 2 | Server: `cost_daily` and its rollups | `src/capabilities` (new module), `tests/smoke.rs` | core deploys |
| 3 | App: shell, tab bar, switcher, Approvals, Status, Instances, Home, Settings, Users | `ios/` | no deploy; `ios-check` |
| 4 | App: Cost | `ios/Pigeon/Views/CostScreen.swift` only | no deploy; `ios-check` |

Phases 3 and 4 start from a shared base commit that adds the type scale, the chart colours and a
placeholder `CostScreen`, so they touch disjoint files. Each server branch is complete without
the app; each app branch reads new fields as optional.

## Not done here

- `ios-release`: the parent cuts the release once the app branches are landed.
- Profile pictures: pigeon keeps none; `picture` is always `null`.
- Instance-scoped data tools: see §3's follow-up.

## Log

- 2026-11-24: plan written.
- 2026-11-24: phase 3 app built (executor `claude/ai-agent-gxYEtGtfkQSo`): tab bar and switcher, Approvals, Status, Instances, Home, Settings, Users; Architecture, Access and Passkeys screens removed.
- 2026-11-24: Cost screen built (`ios/Pigeon/Views/CostScreen.swift`): 2×2 grid, details box, two stacked bar charts from `cost_daily`.

## 2026-11-23: mockup polish and the system tab bar

The overnight build followed the written spec, not the mockup. This section lists what differs,
screen by screen, and how it is fixed. The tokens' colours already match the mockup
(`Tokens.swift` does not change, so `platform_design_tokens_for_ios` stays green). The CI runner
has Xcode 26.6 (`ios/.xcode-version`), so the iOS 26 SDK and Liquid Glass build; the deployment
target stays iOS 18.0, and iOS 26-only calls sit behind `#available`.

### Gap list

| Where | App today | Mockup |
| --- | --- | --- |
| Dot grid | hard 1.1 pt dots | 1.1 pt dots fading out by 1.4 pt, 16 pt grid, fixed behind scrolling content |
| Page | 24 side padding, 18 between cards | 16 side padding (20 on Users and User detail), 10 between cards |
| Cards | 18 padding all round, 8 between rows | 11 top, 14 sides, 12 bottom; rows 4 apart; radius 16 on a large empty-state card |
| Buttons | 12/24 padding, radius 12, 16 pt; outlined second choice | 40 high (36 small), radius 10, 15 semibold; primary, secondary (chip), destructive (chip, warn text), answer option (1 pt accent outline, left aligned); two-button rows split 1:1, gap 8 |
| Labels | Eyebrow in mono capitals still exists | 13 semibold muted, sentence case; section labels indented 4, 10 above |
| Grouped lists | a card per row (Instances, Users) or Divider rows | one card, rows 48/56/60–64 high, 1 pt `line` separators inset to the text, 7×12 chevron in `line`, counts muted |
| Numbers | proportional | tabular figures |
| Tab bar | custom flat bar, accent badge, long press on Instances | system `TabView` (Liquid Glass on iOS 26), `.badge()` on Approvals |
| Headers | drawn in content, nav bar hidden | system navigation bars with large titles set in 30 bold, −0.4 tracking |
| Switcher | custom card above the tab bar, opened by holding Instances | system `Menu` in the navigation bar of every screen, showing the current instance |
| Approvals | count in an accent capsule; sub 13; no clock; notice as a card at the top; Deny and Stop outlined; options outlined at 1.5 pt; "Something else…" outlined; "Fine, carry on" primary; empty text 15 | count badge 34 round, warn, 20 bold; sub 15 muted; 12 pt clock before the time, 13 semibold, warn when urgent; toast below the cards in 13 semibold `good`; Approve with a key icon, primary; Deny and Stop destructive; options 1 pt outline, left aligned; "Something else…" muted text; "Fine, carry on" secondary; empty text 14 |
| Status | Divider between runs; role line under executors; Answer is a primary button; Note, Extend and Stop outlined | planner row: title and elapsed, then state · step, then "Planner · 34% of $25 budget" (warn at 80%); executor rows smaller with state · step · % of budget, Answer when waiting; Note, Extend secondary, Stop destructive |
| Cost | 18 gap; "Where it went" by source; Model/Role toggle; range control beside a drawn title | 10 gap; the details box names each colour with its amount; AI Gateway by model only; the range control one square wide over the details box |
| Instances | a card per instance, id and account lines, outlined Settings | one grouped card; 40×40 tile (radius 10, initial 17 bold, accent when current); title 17 semibold with an accent tick; "repo · core account" 13 muted; chip Settings button 36 high; a footnote |
| Home (linked) | title card only; "Facts" card; primary switch button | empty-state card (64 chip tile with a globe, 17 semibold title, 15 muted text); "This instance" fact list; chip "Switch to Pigeon Platform" 44 high, radius 12 |
| Settings | 64 avatar with the name at 30 bold; plain Toggle tinted accent; "Tap again to sign out" | subtitle "Pigeon Platform"; profile card with a 48 avatar, name 17 semibold, "Standard user · this iPhone"; Access rows with counts and chevrons; "Pause all agents" with its subtitle, switch tinted warn; Sign out in warn in its own card |
| Users | title "Who can sign in · 4" at 30 bold; a card per person; soft avatar | title Users; section label "Who can sign in · 4"; one grouped card, 64-high rows, 40 chip avatar, separators inset 68; a footnote |
| User detail | 64 avatar beside the name; passkeys and delegates as cards | centred 76 avatar (28 semibold initials), name 24 bold, "Standard user · added …" 14 muted; "Passkeys · 2" rows 60 high with a 30×30 soft key tile; "Delegation · acts for …" rows with a chip tile; a footnote |

### Choices

- **Tab bar.** SwiftUI's `TabView` with `Tab` values (iOS 18), which the iOS 26 SDK draws as the
  floating Liquid Glass bar, and minimises on scroll on iOS 26. The Approvals count is `.badge()`.
- **Switcher.** A system `Menu` as a toolbar button at the top right of every screen, its label
  the current instance's initial (a glass button on iOS 26). It lists the instances with a tick
  on the current one, and "All instances", which opens the Instances tab. A plain tap on the
  Instances tab still opens it. `tabViewBottomAccessory` was not chosen: it is iOS 26 only and is
  meant for a persistent mini player, and it takes room on every screen.
- **Titles.** System large titles, set to the mockup's 30 bold with −0.4 tracking. The Users
  title is 30, not 34: one appearance sets every large title.
- **Lists.** Grouped lists stay hand-drawn cards in a `ScrollView`, because a system `List`
  section cannot carry the hard offset shadow. Toggles, menus, alerts and dialogs are system ones.

### Work

1. Foundation (`Design/Components.swift`, `Views/Screen.swift`, `Views/TabBar.swift`,
   `PigeonApp.swift`, `Views/SignInView.swift`): metrics, dot grid, cards, buttons, labels,
   grouped rows, the tab view, title appearance and the switcher menu.
2. Screens A (`ApprovalsView`, `StatusView`, `CostScreen`), on 1.
3. Screens B (`InstancesView`, `SettingsView`, `UsersView`, `Avatar`), on 1.
- 2026-11-23: foundation built (executor `claude/ai-agent-q4ecG2xGhW8A`): metrics, soft dot grid, cards, buttons, grouped rows, system `TabView`, title appearance, instance menu. ios-check green.
- 2026-11-23: screens rewritten to the mockup (executors `claude/ai-agent-nknWRoxcSYRZ`: Approvals, Status, Cost with the drawn Status and Cost values and new chart colours; `claude/ai-agent-UiRpmn9nC59K`: Instances, Home, Settings, Secrets, Users, User detail). Old components removed.

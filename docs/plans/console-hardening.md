# Console hardening

Superseded by [console-redesign.md](console-redesign.md) (2026-11-25): section snapshots and
`/console/api/section` give way to `/app/op` reads, and the panels named here are deleted. This
file is the record of what was built.

Status: deployed 2026-11-21 (landing `slWijyeD5dnMaimB`, main `2038674`). Branch `claude/console-hardening`.
Since `53a0068` the console is the `/app` shell (`src/capabilities/src/console/web/`) and the
root-path tabs redirect into it ([console.md](console.md), "App cutover"); `console/app.js` and
`console/chart.js`, named in the phases below, are deleted. The phases are the record of what
this plan built.
Mockup: [pigeon Console, focused](https://claude.ai/artifact/BLdr2D7rGU344aqP5KcYhv).

The console at `platform.pigeon.dev` is slow to load and sometimes shows a network error. This
plan finds why, makes every section load fast and on its own, and narrows the console to what
the person uses it for: approvals, the architecture, access (secrets and credentials with
expiry, people, delegation) and cost.

## What was measured (2026-11-21, before)

The console needs a passkey, and the `/cf/` proxy allows no telemetry reads, so the server
paths were timed through the MCP tools that run the same code. Times are wall clock from this
session and include about 0.4 s of tool overhead.

| Server path | Console section | Time | Cloudflare calls, all serial |
| --- | --- | --- | --- |
| `users_list` (Directory only) | Users, `/console/api/me` | 0.5 s | none |
| `secrets_list` | Keys (secrets) | 2.1 s | permission groups, mint token, list store, revoke token |
| `credentials_expiry` (cache warm) | Keys, Overview | 2.5 s | the four above, then one Directory read per secret |
| `models_usage`, 7 days (60 logs) | Models chart, Overview cost | 2.0 s | permission groups, mint, one page per 50 logs, revoke |
| `architecture_describe` | Architecture tab | **35 s, then 503**, twice | none: CPU |

Causes, from the code:

1. **The Architecture tab never loads.** `architecture.rs` runs `describe()` over every embedded
   source file and `render()`s it on every request. It exceeds the Worker's CPU limit and the
   runtime answers 503 after about 35 s: the "network connection error" the person sees.
2. **The Overview waits for everything, in series.** `Console::overview` calls each cost section
   and Keys one after another, and the page shows nothing until all have answered. Keys alone
   is two capabilities (`secrets`, `proxy`), also in series.
3. **Every load mints and revokes a Cloudflare token.** `secrets_store::Writer::open` and
   `ai_gateway::logs` each list permission groups, mint a scoped token, use it, and revoke it:
   four round trips before and after the one call that matters. The store list uses `call`,
   not `call_fresh`, so a token that has not reached the Secrets Store yet fails with 401 now
   and then: a section error.
4. **Model logs are read 50 at a time, in series, up to 5,000.** Two pages today; at the cap,
   100 serial calls. Nothing is cached, and the 7- and 30-day views each read again.
5. **Expiry is read one secret at a time** from the Directory cache, in series.
6. **No timeouts anywhere.** A slow Cloudflare call holds the section until the runtime gives
   up; the page shows the raw error text with no way to try again but reloading.
7. **The shell asks `/me` then `/tabs` in series**, and `/me` makes three Directory calls in series.

## Decisions

- **Four tabs.** `Approvals` appears only while one waits for the person (a landing in
  `awaiting_approval`); otherwise there is no tab and no banner. `Architecture`. `Access`:
  secrets and proxy credentials with expiry, people (invite, passkeys), and delegation (the
  service account acting for the person, its links, disconnect). `Cost`: model cost, 7 or 30
  days, total and per model family.
- **Gone from the console:** the Overview's policy-decision list, the CI section, landing
  history and the "require approval" toggle (the land tools keep them), and the Gateways
  wording. Nothing behind them is removed: tools and data stay. Old paths redirect: `/gateways`
  to `/cost`, `/settings` to `/access` (fragment kept, so `/settings#NAME` still lands).
- **Each section loads on its own** with its own request, a skeleton, a 12 s timeout, and on
  failure a short message with a Retry button. One failed section never blanks the page.
- **Served from a snapshot, refreshed behind.** Each slow section's answer is kept in the
  Directory (the existing `setting.get`/`setting.put` with a TTL) with the time it was made.
  A request answers the snapshot at once and, when it is older than its freshness, refreshes it
  after the answer with `wait_until`. The page shows "updated 3 min ago". A write that changes
  the data (setting a secret, issuing or revoking a credential, inviting, disconnecting)
  drops that snapshot. Freshness: cost 10 min, secrets list 5 min, expiry as today (a day).
- **Architecture comes from the architecture planner's per-deploy output.** The model is made
  once per build or deploy, not per request; the console only reads it. Agreed with session
  `session_015XFA1mC8Le3vJWk6HpLW2U` before anything touching `architecture.rs` lands.
- **No new policy rules, no new proxy allowlist entries.** Tests only in `tests/smoke.rs`.

## Phases

### Phase 1: fast and failure-proof server (no visible change)

| Step | Where | What |
| --- | --- | --- |
| 1a | `console.rs` | `timed(ms, fut)`: a section answer races `worker::Delay`; past 10 s it answers `{"error": "timed out", "retry": true}` with status 504. Every `/console/api/*` answer goes through it. |
| 1b | `console.rs` | Several owners of one section, the Overview's sections, and `/me`'s Directory calls run concurrently (`futures_util::future::join_all` / `join3`). |
| 1c | `console/snapshot.rs` (new, pure) + `console.rs` | Snapshot: key `console:snap:{section}:{days}`, value `{at_ms, data}`; `fresh(at, now, ttl)`; serve-then-refresh through `cx.worker.wait_until`; drop on the writes above. |
| 1d | `secrets_store.rs`, `ai_gateway.rs` | Use `call_fresh` for the first call on a minted token; cache the permission-group list in the isolate for an hour. |
| 1e | `expiry.rs` | Read all secrets' cached expiry concurrently. |
| 1f | `architecture.rs` (with the architecture planner) | `section_data` and `architecture_describe` read the precomputed model and HTML. |

Done when: smoke suite passes; live, every section answers in under 1 s warm and under 4 s cold;
Architecture answers at all.

### Phase 2: the narrowed console

| Step | Where | What |
| --- | --- | --- |
| 2a | `console/sections.rs` | Tabs `Approvals`, `Architecture`, `Access`, `Cost`; `/api/tabs` lists Approvals only while one waits. Redirects for `/gateways`, `/settings`, `/`. |
| 2b | `land.rs` | Section `approvals` (tab Approvals): only landings awaiting approval, with Approve (the existing bound passkey write). |
| 2c | `users.rs` | Sections `people` and `delegation` split from `users`: people with passkeys and invite; each service account acting for the person, linked when, its tokens, disconnect. |
| 2d | `secrets.rs`, `proxy.rs` | Section `credentials` (tab Access) keeps Keys's data, expiry first. |
| 2e | `models.rs`, `ci.rs` | `models` moves to tab Cost; the CI section goes. |
| 2f | `console/*.js` | `approvals.js`, `access.js` (from `keys.js` and `users.js`), `cost.js` (from `overview.js` and `chart.js`); `overview.js` and `land.js` go. (Now: the `/app` views in `console/web/`, which mount `approvals.js`, `instances.js`, `keys.js` and `users.js` through `management.js`; Cost is `web/cost.js`.) |

Done when: as phase 1, plus Playwright on the shell with the API stubbed, at 1000 px and 390 px.

### Phase 3: the client

| Step | Where | What |
| --- | --- | --- |
| 3a | `console/app.js` (deleted; now `console/web/shell.js`) | `/me` and `/tabs` in parallel; per-section skeleton, `AbortController` timeout of 12 s, one silent retry on a network error, then the message and a Retry button; "updated … ago" from the snapshot's `at_ms`; the Approvals tab polled every 30 s while open. |
| 3b | `console/console.css` | Skeleton, retry row, the Approvals dot on the tab. |

Done when: live, with one section's source failing, the others still render and the failed one
offers Retry.

## Who does what

- **This session:** the plan, the briefs, reading every line returned, the wiring edits, the
  smoke suite run, landing each phase and timing the live console after it.
- **Executors:** AI Gateway models through `models_submit`: `claude-sonnet-5-5` for whole new
  files (1c, 2f, 3a) and `claude-haiku-5-5` for small mechanical ones (3b). Each brief has the
  goal, the exact interface, the surrounding code, the code style, and asks for one whole file.
  No brief carries a credential or a person's data.
- **Architecture planner:** the precomputed model (1f's source).

## Built, and where it departs from the plan

- **Snapshots refresh from the page, not with `wait_until`.** A stale snapshot is answered with
  `X-Snapshot-Stale: 1`, and the page asks again with `fresh=1` behind what it shows. A refresh
  after `wait_until` would have needed a `'static` copy of the request's context. Writes don't
  drop snapshots; the page's reload after a write asks `fresh=1`.
- **Only Credentials and Cost keep snapshots.** People, Delegation and Approvals read the
  Directory or the Lander and answer in well under a second.
- **Tests: none added.** `policy_check add_test tests/smoke.rs` answers `deny`, "not in policy":
  the `smoke-suite-fixed` rule only excepts the smoke suite, and no rule allows adding to it.
  The existing suite passes. Verification was Playwright against the real scripts with the API
  stubbed, at 1000 px dark and 390 px light.
- **No warning when a credential is about to expire.** The Overview used to show one at the top
  of the page. The Access tab now shows each credential's expiry but nothing calls attention to
  one about to run out; the Approvals banner is only for passkey approvals.
- **Architecture** is left to the architecture planner (session `session_015XFA1mC8Le3vJWk6HpLW2U`):
  same contract, `{"html"}`, made ahead of the request.
- **Executors:** `approvals.js` and the new CSS from `claude-haiku-5-5`, the `users.js` split from
  `claude-sonnet-5-5`, each read and adjusted here. `app.js` and the server side were written
  here.

## Results

Phases 1 to 3 landed together: landing `slWijyeD5dnMaimB`, main `0c9db60` to `2038674`, test and
deploy green, 2026-11-21 20:28 UTC. Timed the same way as before, through MCP, with about 0.4 s of
tool overhead in each:

| Server path | Before | After |
| --- | --- | --- |
| `architecture_describe` | 35 s, then 503 | 0.51 s |
| `users_list` | 0.48 s | 0.42 s |
| `secrets_list` | 2.1 s | 2.45 s |
| `credentials_expiry` | 2.5 s | 2.39 s |
| `models_usage`, 7 days | 2.0 s | 1.60 s |

The Architecture fix is the architecture planner's (`depth_of` was exponential; now memoized),
landed just before this. The MCP paths for secrets and cost changed little: a mint still makes
its calls, and the permission-group cache helps only once an isolate has served one. What the
console gains is not in these numbers: Credentials and Cost answer from their snapshot in one
Directory read and refresh behind it, sections no longer wait for each other, and none can hang.
The signed-in console API could not be timed from a session (it takes the person's passkey);
the public routes answer: `/` and `/access` 0.42 s, `/gateways` 302 to `/cost`, `/console/api/tabs`
401 without a session.

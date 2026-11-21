# Console hardening

Status: mockup approved by the person on 2026-11-21; phase 1 built and landing. Branch `claude/console-hardening`.

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
| 2f | `console/*.js` | `approvals.js`, `access.js` (from `keys.js` and `users.js`), `cost.js` (from `overview.js` and `chart.js`); `overview.js` and `land.js` go. |

Done when: as phase 1, plus Playwright on the shell with the API stubbed, at 1000 px and 390 px.

### Phase 3: the client

| Step | Where | What |
| --- | --- | --- |
| 3a | `console/app.js` | `/me` and `/tabs` in parallel; per-section skeleton, `AbortController` timeout of 12 s, one silent retry on a network error, then the message and a Retry button; "updated … ago" from the snapshot's `at_ms`; the Approvals tab polled every 30 s while open. |
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

## Results

To be filled in after each landing: commit, landing id, the same timings as above.

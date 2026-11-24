# AI agents: planners and executors through pigeon's model gateway

Status: **landed and live** (2026-11-22). Phases 1 to 5 are on main; the `pigeon-ai-agents` Worker runs
planners and executors through the model gateway on sandbox image v2 (`docs/plans/sandbox-v2.md`).
Open items are listed before the Log. Planner session `session_015XFA1p5x6wTEFRENJaCUxN` (handed
off 2026-11-22), seeded by the parent `session_015XFA1jgveJDEE8NJjTaHKg`.

## Goal

The person's words: "design planner and executor agents through the new model gateway by default.
I should retain the ability to say do it in the Claude chat but default we go through model
gateway."

Today every planner and builder is a Claude Code cloud session. A planner fans fixed-size pieces
out to "executors" with `models_submit`, but an executor only returns text: it has no tools, no
checkout and no tests, so the planner applies, builds and verifies everything itself. After this
plan, a planner or an executor is a pigeon **run**: an AI agent loop inside pigeon that calls models
through `pigeon-model-gateway`, acts through pigeon's own tools and a container sandbox, and
reports back as a session does. A Claude Code session stays available when the person asks for
it ("do it in the Claude chat").

Hard lines, unchanged:
- No new policy rules unless the person asks. This plan adds none. Agents act under today's
  `.pigeon/policy.toml`.
- Tests only in `tests/smoke.rs`. Scenarios may be added, prefixed by Worker (`agents_…`), and each
  Worker's share stays under 60 s.
- Landings that change `.pigeon/policy.toml` or `.claude/settings.json` need the person's passkey.
- AI agents never hold the parent token, a git or deploy token, or any provider key.

## What is true today (main at `b03b5b6`, read 2026-11-22)

**Models** (`src/models`, crate `pigeon-models`; Worker `src/model-gateway`):
- One provider, `anthropic` through Cloudflare AI Gateway `pigeon`. The native Messages endpoint
  (`…/anthropic/v1/messages`) is used with a BYOK key in the Secrets Store
  (`pigeon_anthropic_default`). The gateway runs with `byok_only`, `authentication` and
  `cache_ttl: 0` (`ai_gateway.rs:73-85`). Each request carries `cf-aig-collect-log-payload: false`
  and up to 5 `cf-aig-metadata` tags: `caller`, `caller_name`, `for`, `for_name` and `task`.
- `models_run` is synchronous and takes about a minute. Its `raw` argument passes a whole provider
  body through, including `tools` and `cache_control`. That is the only way through today
  (`lib.rs:397-428`).
- `models_submit`, `models_status` and `models_result` are async jobs in the `ModelJobs` DO
  (`runner.rs`). Each job streams from its own object's alarm. The limits are 4 running, 50 open,
  a timeout of 1 to 14 min, and results kept 24 h. The result is **text only**: the SSE reader
  keeps `text_delta`s (`stream.rs:82-86`), so `tool_use` blocks, thinking and cache token counts
  are dropped. A job carries no `tools`, no system blocks and no `cache_control`. A runner that
  restarts mid-stream marks the job `lost` and does not resend it.
- `models_usage` groups AI Gateway logs by model, caller or task, using the gateway's own
  `log.cost`. The console's Cost tab draws cost per day per model family (`daily.rs`).
- There is no fallback: one provider, no second model.
- The split: the gateway Worker is deploying for the first time. Core still serves `models_*`
  until the "bind" landing. `ModelJobs` stays in core until its transfer, and `src/models` is
  shared until then. Code reaches the tools through core's `dispatch`. From another Worker that
  is `pigeon_contract::ask(.., "dispatch", {tool, args}, signed caller)` over `PLATFORM`. Nothing
  chains capability to capability.

**Identity and the gate** (`src/capabilities`):
- `Caller { user, acting_for }`: users are `standard` or `service`, and `links` ties a service user
  to its person (approved with a passkey at `/auth/link`).
- `dispatch` runs `approvals::gate` on every call (`call_tool`, the tool as target).
- Passkey-gated tools answer with a pending approval id. The person approves that exact call on
  the console's Approvals tab, and the caller calls again with `"approval": id`.
  `approvals_status` lets it wait.
- `land` accepts a standard user or a service user acting for one (`land.rs:137`).
- The signed caller (`X-Pigeon-Caller`, HMAC with `CALLER_KEY`, 60 s) is how capability Workers
  call core's `dispatch` as a caller.

**Sandboxes** (`ci/`, `src/ci`):
- The CI base image (Debian, Rust with wasm32, clippy and rustfmt, Node 22, worker-build) runs a
  one-shot entrypoint.
- The entrypoint fetches one commit through `/git/` with a job token (fetch-only, one commit),
  runs one script, and reports the last 4 KiB of output.
- There is no exec channel, and nothing in it can push.
- Instances are `standard-3` and `standard-4`, at most 4 at once, with internet on.

**Sessions**: no pigeon code starts or messages Claude Code sessions. `start_session` and
`message_session` are policy kinds mapped to the claude-code-remote tools in the generated
settings. Sessions are started by the parent with `create_session`.

### Cloudflare and Anthropic, checked 2026-11-22

- **Agents SDK** (developers.cloudflare.com/agents): an Agent is a DO with a scheduler on SQLite
  that survives eviction and deploys. It "does not automatically checkpoint arbitrary JavaScript
  or resume a function where it stopped", and interrupted callbacks re-run, so they must be
  idempotent. It is JS/TS only.
- **Durable Objects**:
  - Alarms are at-least-once and retried with backoff. Alarm handlers have 15 min of wall time.
  - A request has unlimited wall time while connected, and an open outbound fetch keeps the object
    alive for up to 15 min.
  - SQLite holds 10 GB per object.
  - On deploy, in-flight requests that touch storage are stopped, with up to 30 s for runtime
    updates.
- **Workflows**: `waitForEvent` waits up to 365 days, and step results are capped at 1 MiB. No
  Rust support was found.
- **Containers and Sandbox SDK** (`@cloudflare/sandbox`):
  - It offers exec, files, processes, sessions and backups.
  - `sleepAfter` defaults to 10 min.
  - Egress control comes from `enableInternet`, `allowedHosts` and `interceptHttps`, with
    `outboundByHost` handlers that run in the Worker. Those handlers can add credentials the
    container never sees.
  - Instances run from lite to standard-4, billed per 10 ms of active vCPU.
- **AI Gateway**:
  - The native `/anthropic` endpoint passes Messages bodies through, so tools, caching and betas
    work.
  - `cf-aig-request-timeout` counts only to the first chunk.
  - **Dynamic routing (fallbacks, budgets) takes only the OpenAI chat shape. Native Messages
    requests get 400.** So fallback must be ours.
  - Retries are available through `cf-aig-max-attempts` (up to 5).
  - Rate limits are gateway-wide.
- **Anthropic**:
  - Prompt caching: 5 min or 1 h TTL, up to 4 breakpoints plus top-level automatic
    `cache_control`. Reads cost 0.05× on Opus and Sonnet 5.5.
  - Thinking is adaptive, with `output_config.effort`. Thinking blocks are passed back unchanged.
  - Models: `claude-opus-5-5` ($4 / $20 per MTok), `claude-sonnet-5-5` ($2 / $10) and
    `claude-haiku-5-5` ($0.10 / $0.50), all with a 1M context.
  - Betas: compaction (`compact-2026-01-12`, triggering at 150K by default), context editing and
    task budgets.

## Decisions

### 1. The runtime: a Rust Durable Object loop in a new Worker, `pigeon-ai-agents`

The loop is a **step machine in a SQLite Durable Object, `AgentRun`**. One object per run, driven
by its alarm. It runs in a new capability Worker, `pigeon-ai-agents` (crate `src/ai-agents`), which
follows the split's contract: `manifest`/`call`/`section`, no public surface and `CALLER_KEY`.

Why not the alternatives:
- **The Cloudflare Agents SDK** is JS/TS. Every pigeon Worker is Rust with a thin JS shim. The SDK
  would bring its own scheduler beside pigeon's alarm patterns, which are already proven by
  `ModelJobs`, `CiRunner` and the Lander. Even the SDK only re-runs interrupted callbacks, so a
  step machine is needed either way. We take its shape and keep Rust.
- **Workflows** suit long waits, but there is no Rust support, step results are capped at 1 MiB,
  and the waits we need (an approval, an answer, an executor) are alarm polls pigeon already does.
  Revisit with ci-workflows.md.
- **A container per agent** (running the loop inside the sandbox) would put the loop, and with it
  the transcript and the right to call tools, next to untrusted code and a shell. The loop stays
  in the DO. The container is only hands.
- **Hosting runs inside `pigeon-model-gateway`** keeps one hop fewer. But the gateway should stay a
  gateway (keys, tags, jobs, cost), and AI agents need containers, git, dispatch and the console, all
  of which belong elsewhere. A separate Worker also deploys in its own lane.

**The step machine.** A run's state is rows in its object: `run` (brief, role, caller, budgets,
state), `messages` (one row per message, append-only, so no row nears the size limit) and `steps`
(each model turn and tool call, with `started_at`/`done_at`). Each alarm does one step:

```
created → preparing (sandbox, checkout) → thinking (a model turn in flight)
        → acting (tool calls of the last turn, one per alarm) → thinking → …
        → waiting (approval | answer | executors) → thinking
        → reporting → done | failed | stopped | over_budget
```

- **A model turn** goes through the gateway as an async job. The run calls `models_submit` with the
  full request (decision 2) through core's `dispatch` as the run's caller, then polls
  `models_status` from its alarm (1 s, rising to 5 s), then `models_result`. The model gateway's
  job object streams the turn, so the run's object holds no long fetch, and a deploy of
  `pigeon-ai-agents` costs nothing in flight. A notify can't be used: it may not point at the
  platform.
- **Tool calls** each run in their own alarm step. A step is marked `started` before its side
  effect and `done` with its result after. On a restart, a step that is `started` but not `done`
  is retried if the tool is idempotent: reads, `run`, `ci_status`, `land_status`. Otherwise the
  model gets `{"error": "interrupted; state unknown, check before retrying"}`. That covers
  `push`, `land` and `ci_submit`.
- **Pausing and resuming**: `waiting` sets a longer alarm (15 s for approvals and answers, 30 s for
  executors). Resuming reads the transcript from rows. A run can wait days, at no cost while it
  waits.
- **Surviving deploys**: alarms persist across deploys and are at-least-once. Every step is
  idempotent or marked, as above. A model job lost to a gateway deploy (`lost`) is resubmitted:
  a turn has no side effects.

**Long contexts.**
- Models have 1M of context, but cost grows with each turn. The run sends
  `context_management` (clear old tool results: `clear_tool_uses_20250919`) and, past 150K tokens,
  compaction (`compact-2026-01-12`), both through the native endpoint.
- If a beta is refused, the run falls back to its own summary turn on Haiku: it replaces messages
  older than the last 20 with one summary message.
- Tool results are capped at 16 KiB. Long output stays in the sandbox (`/tmp/out/{step}`), where
  the AI agent can `read_file` it.

**Prompt caching.**
- Top-level automatic `cache_control` caches the latest turn.
- An explicit breakpoint caches the tools and system prompt, which never change within a run.
- Executors use the 5 min TTL: their turns are seconds apart.
- Planners use `ttl: "1h"` on the stable prefix, because they wait minutes for executors.
- The run records `cache_read_input_tokens` and `cache_creation_input_tokens` for each turn
  (decision 2), so the Cost tab shows whether caching works.

**Thinking**: adaptive. Effort is `high` for planners and `medium` for executors. Thinking blocks
are kept in the transcript unchanged.

### 2. What the model gateway must add (coordinated with the split planner)

All changes go in `src/models`, which is shared until the `ModelJobs` transfer. I build them on
`claude/ai-agents-models` and ask the split planner before each change. Fields are only added, so
`models_submit` callers today are unaffected.

| Change | Why |
| --- | --- |
| `models_submit {request}`: a whole Messages body (`system` blocks, `tools`, `tool_choice`, `cache_control`, `thinking`, `output_config`, `context_management`, `metadata`), the job version of `models_run`'s `raw`. The gateway sets `model`, `stream: true` and `max_tokens` bounds itself. `stream`, `model` override and unknown top-level keys are refused | turns with tools |
| `betas: [..]` checked against an allowlist in code (`compact-2026-01-12`, `context-management-2025-06-27`, `task-budgets-2026-03-13`) and sent as `anthropic-beta` | compaction, context editing |
| The SSE reader keeps **content blocks**: text, `tool_use` (input JSON joined from `input_json_delta`), thinking with its signature, and compaction blocks. `models_result` answers `content` beside `text`, and `usage` with cache read and write tokens | the loop needs `tool_use` |
| Large bodies: the request and result are stored in the job object, chunked across rows. `index` keeps only the row | a transcript is hundreds of KB |
| `models_cancel {id}` | the kill switch, budgets |
| `lane: "ai-agents"`, a separate running cap (16 since 2026-11-22, first 6; a constant in `src/models/src/jobs.rs`) beside today's 4, so AI agent turns neither starve nor are starved by executor jobs | concurrency |
| Retries inside a job: on 429, 529 or 5xx **before the first byte**, retry the same model (1 s, 4 s, 10 s). A failure after bytes is `failed` with the provider's error type, and the run decides | fallback (decision 6) |
| `models_usage {task_prefix}` and a Cost tab series for `ai-agent/` tasks | cost per run |

The 4 job tags stay. `task` carries the run: `ai-agent/{run}/{role}/{turn}` (100 characters at most).

### 3. Hands

**Tools, through `dispatch`.**
- Every pigeon tool a run calls goes `pigeon-ai-agents → PLATFORM dispatch → gate → tool`, signed as
  the run's caller (decision 4). The policy gate, passkey approvals and audit hold unchanged:
  nothing in pigeon-ai-agents decides what is allowed beyond narrowing.
- **Each role has a tool list** in code. It is a narrowing, not policy: a run is offered only these
  tools, and `dispatch` refuses anything else before the gate.
  - Executor: `policy_check`, `ci_submit`, `ci_status`, `ci_commit`, `land_status`. No `land`:
    see the person's answer 1.
  - Planner: the executor's list, plus `land`, `land_cancel`, `models_usage` and `architecture_describe`.
  - Agent-internal tools are not pigeon tools and never reach `dispatch`:
    - all roles: the sandbox tools, `ask_person`, `report`;
    - planners only: `spawn_executor`, `executor_status`, `executor_result`, `executor_stop`.

**The sandbox: a container per run, from the CI base image.**
- A new container class, `Sandbox`, in `pigeon-ai-agents`. It is a new class, not a transfer.
- The image builds `FROM` the CI base image (same toolchain, so what passes in the sandbox passes
  in CI) and adds an exec server.
- Phase 0 decides which exec server:
  - **(A)** The Sandbox SDK. Its image layer and `@cloudflare/sandbox` go in the Worker's JS shim,
    as core already does for containers. It brings exec, files and outbound handlers.
  - **(B)** A small exec server of pigeon's own (Rust, about 300 lines) in the image, reached
    through the container's port fetch. Its ops are `exec {argv, cwd, timeout_s}`, `read`,
    `write`, `edit {old, new}`, `list`, `grep`.
  - A is preferred, if its base image and our CI layers combine under the 20 GB disk of
    `standard-4`. B is the fallback.
- **Sandbox tools offered to the model:**
  - Files: `read_file`, `write_file`, `edit_file` (exact replace, failing unless unique), `list`,
    `grep`.
  - `run {cmd, timeout_s ≤ 1800}`, answering the exit code and the tails of stdout and stderr.
  - `commit_push {message, force?}`: commits any change, then pushes HEAD (local commits
    included) to the run's branch; `force` pushes with `--force-with-lease`.
- **Instances.** Executors run on `standard-4` (cargo needs it), planners on `standard-1` (reading
  and grepping). At most 4 sandboxes at once (a setting), apart from CI's 4. An idle sandbox
  sleeps after 10 min, and a finished run destroys its own.
- **Disposable.** The run's branch on Artifacts is the checkpoint. `commit_push` is cheap, and the
  system prompt asks for a push after each green step. A lost sandbox is recreated from the branch
  tip.

**Git through the Artifacts proxy, with no token in the container.**
- The sandbox clones and pushes `https://proxy.pigeon.dev/git/pigeon-security/pigeon.git`.
- With (A), an `outboundByHost` handler for `proxy.pigeon.dev`, running in the Worker, adds a
  **run grant**.
- With (B), the grant is put in the container's git config as CI does, readable by the code the
  AI agent runs. That is acceptable only because the grant is as narrow as this:
- **The run grant** extends `job_token` with a new grant kind (`src/core/src/job_token.rs`, the
  git proxy's checks):
  - fetch of the repository;
  - push only to `refs/heads/claude/ai-agent-{run}` and `refs/heads/claude/ai-agent-{run}-*`;
  - expiry at the run's deadline (at most 8 h);
  - revoked when the run ends.
- The git proxy still asks the policy `push_branch` for the branch. `@session/*` covers `claude/*`,
  so no rule changes. Its audit actor is "AI agent run {run} for {person}".

**Landing.** The run calls `land {branch}` through `dispatch`. `may_land` already admits a service
user acting for a person. The Lander tests, asks for the passkey on guarded paths, merges and
deploys as for any session. A run waits on `land_status` like a session does.

**What a run never holds**: the parent token, a proxy credential (`pgn_px_`), `source_token`,
`deploy_token` or a provider key.
- Model calls go through the gateway with core-minted 15-minute tokens on the gateway side.
- Git goes through the run grant.
- Cloudflare is reached only through tools.
- A passkey-gated tool still waits for the person.

### 4. Identity, limits and the kill switch

**Two service users, linked once.**
- "Pigeon planner" and "Pigeon executor" are service users, each linked to the person with a
  passkey at `/auth/link`: the one-time step the person does at setup.
- `ai_agents_setup` asks core to ensure the two users exist and opens their link requests. This is a
  small contract op `service_user {name}` on core, the one core change outside `src/models`.
- A run's caller is `{user: Pigeon executor, acting_for: Willow, run: "r_…"}`. `run` is a field
  added to the signed caller. The contract allows additions; core records it in audit lines and
  approval cards ("Pigeon executor, run r_7Kp…, acting for Willow").
- Who may start runs: the person, or a service user acting for them: the parent session through
  the portal, or a planner run.
- **Caller signing.** pigeon-ai-agents signs callers itself, because a run outlives the 60 s caller
  that started it. Any Worker holding `CALLER_KEY` can sign any caller, so pigeon-ai-agents only ever
  signs the stored caller of a run, which core gave it at `ai_agents_start`.
- **Hardening, raised with the split planner rather than done here** (phase 5):
  - per-Worker caller keys;
  - core checking that a caller signed by pigeon-ai-agents names one of the two agent users, acting
    for the person they are linked to.

**Budgets, per run, set at start, enforced before every turn and tool call.**

| | Executor default | Planner default | Hard cap (code) |
| --- | --- | --- | --- |
| model turns | 60 | 200 | 500 |
| cost (USD, tokens × a price table in `src/ai-agents`, cache-aware) | $5 | $25 | $100 |
| wall time | 90 min | 8 h | 24 h |
| sandbox time (`run` total) | 60 min | 30 min | 6 h |
| executors spawned | — | 12 | 30 |

- A run that would cross a budget stops at `over_budget` and reports what it has. Its branch is
  pushed. The person or its planner can extend the budget with `agents_extend`.
- The price table is only for enforcement. The Cost tab and `models_usage` read the gateway's own
  cost, as today.

**Concurrency** (stored config in the index, set with `ai_agents_config`; changed 2026-11-25, see
`docs/plans/token-minting.md` decision 7):
- live runs: **`max_live_runs` over every role, default 8** (`src/ai-agents/src/limits.rs`). A
  planner starts only while live runs are fewer than `max_live_runs - executor_reserve` (default
  reserve 3), so executors always have room. The reserve is clamped below `max_live_runs`, so one
  planner can always start.
- executors: at most `max_executors_per_planner` (default 3) live per planner, and each planner's
  `executors` budget (12 by default, 30 at most) bounds how many it starts in all. An executor
  beyond the cap is queued, not refused; it does not block other planners' executors.
- sandboxes: one per live run. Cost and the model gateway are the real limits.
- model turns: the `ai-agents` lane, 16; a turn beyond them waits queued in the lane (the run
  polls, and its wall budget keeps counting).
- queued runs: 20 at most (`MAX_QUEUED`); more starts are refused. A queued run says why it
  waits in its `step` (`ai_agents_status`), for example "queued: 8 of 8 live runs (max_live_runs)".

More runs wait `queued` in the `index` object, oldest first within their role (planners among
planners, executors among executors). The kill switch (`ai_agents_pause`, `ai_agents_stop`) is
unchanged.

**Visible.**
- `models_usage {by: task, task_prefix: "ai-agent/"}` gives cost per run.
- The console's Cost tab gains an "ai-agents" series and a per-run table.
- `ai_agents_status` answers each run's tokens, cost, turns, wall time and budget left.

**The kill switch.**
- `ai_agents_stop {run}` stops one run and `ai_agents_stop {all: true}` stops every live run. Both are a
  tool and a console button on a new **AI Agents** tab; stopping needs the signed-in session, no
  passkey.
- Stopping cancels the run's model job (`models_cancel`), destroys its sandbox, revokes its run
  grant and marks it `stopped`. A planner's stop cascades to its executors.
- `ai_agents_pause` sets a flag in `index` that every run checks before each step: no new turns,
  tools or starts until `ai_agents_resume`.
- The backstop outside pigeon is the gateway's rate limit (gateway-wide). Stopping can't be
  undone, but `ai_agents_start` with the same brief and the branch as `base` continues the work.

### 5. Routing: the gateway by default, the Claude chat when the person says so

The choice is made **in the parent session, per task**, from the person's words. It is not made
in the policy (no new rule) or in a file.

- **Default.** When the person asks for work, the parent starts a run:
  `ai_agents_start {role: "planner" | "executor", brief, base?, budgets?}`.
  This answers `{run, state}`. A planner run's executors are always gateway runs: it cannot start
  Claude Code sessions, and pigeon holds no Claude credential.
- **"Do it in the Claude chat"** ("in a Claude Code session", "in chat", "as a session"): the
  parent uses `create_session`, as today. It registers the session as a run with
  `ai_agents_register {session_id, role, brief}`, so the AI Agents tab, `ai_agents_status` and the reports
  are one list.
- The rule for agents goes in `docs/plans/README.md`'s *For agents* section in phase 4. That is
  docs, not policy.
- A planner that is itself a Claude Code session may start gateway executors (as it does with
  `models_submit` today, but with hands) or sessions, per the same words passed down in its brief.
- **Optional, phase 5:** a default-runtime toggle on the AI Agents tab. It is a console write with the
  passkey, stored in the Directory, and the parent reads it with `ai_agents_status {settings: true}`.
  The person's words always win over the toggle.

**One way of reporting back, for both runtimes.** A run is a row in `index`, whatever ran it:

```
{run, runtime: "gateway"|"claude_code", session_id?, role, parent_run?, brief, state,
 branch?, landing?, cost, tokens, started_at, ended_at, report?}
```

- A gateway run writes its `report` itself: what changed, the branch, CI, the landing and its cost.
- A Claude Code session calls `ai_agents_report {run, state, report, branch?, landing?}` at its end,
  told to in its brief.
- `ai_agents_inbox {since?}` answers new reports and open questions since a cursor. The parent reads
  it when the person writes, and on a `send_later` wake-up it sets after each start. Pigeon cannot
  message a Claude Code session itself, and this plan doesn't give it a Claude credential to do so.
- Landings show in `land_status` either way.

### 6. The hand-off with the person

- **Questions.** The `ask_person {question, options?}` tool moves the run to `waiting`.
  - The question appears as a card on the console's **Approvals** tab (answer and dismiss; the
    signed-in session, no passkey) and in `ai_agents_inbox`.
  - The parent relays it in the chat, and the person's reply goes back with
    `ai_agents_answer {run, answer}`. Only the person, or a delegate acting for them, may answer.
  - A question left unanswered for 5 minutes (the person's choice): the run goes on with the
    conventional option, lists it in its report under "Assumed:", and the question shows under
    `assumed` in `ai_agents_inbox` and in the AI Agents section.
- **Approvals.**
  - When a tool answers "needs the person's passkey: approval {id}", the run moves to `waiting`,
    puts the approval in `ai_agents_inbox`, polls `approvals_status` and calls again with
    `"approval": id` once approved. This is today's flow, unchanged.
  - The card already shows the caller, now with the run.
  - A guarded-path landing waits on the Lander's passkey as any landing does.
- **Progress.**
  - The AI Agents tab lists live and recent runs: state, the current step in one line (for example
    "run: cargo test -p pigeon-ci (2 min)"), cost against budget, branch, landing, and a
    Stop button.
  - A run's transcript is shown step by step with tool results truncated.
  - `ai_agents_status {run}` gives the same to the parent, and `land_status` shows landings.

### 7. Models per role, and fallback

| Role | Model | Effort | Fallback chain |
| --- | --- | --- | --- |
| planner | `claude-opus-5-5` | high | `claude-sonnet-5-5` |
| executor (code) | `claude-sonnet-5-5` | medium | `claude-opus-5-5` |
| executor (mechanical: moves, renames, docs) | `claude-haiku-5-5` | — | `claude-sonnet-5-5` |
| compaction summaries | `claude-haiku-5-5` | — | — |

`ai_agents_start` may name a model, from the chain's models only.

**Fallback.**
1. The gateway job retries transient errors before the first byte (decision 2).
2. The run retries a failed turn once on the same model.
3. On a second `overloaded`, `rate_limit` or 5xx, the run moves to the next model in its chain for
   that turn and the next 10. It strips thinking blocks from the history, because they are bound
   to the model.
4. If every model fails, or the gateway itself does (AI Gateway or core unreachable for 5 min),
   the run moves to `waiting` with reason `provider`, retries every 5 min for an hour, and puts a
   note in `ai_agents_inbox`: "the model gateway is failing; say *continue in the Claude chat* to move
   this run to a session".
5. Moving a run to a session is a **hand-off packet** from `ai_agents_handoff {run}`: the brief, the
   branch and its tip, the last report-style summary and the budgets left. The parent passes it to
   `create_session`, then `ai_agents_register`s the session with `parent_run`.

A second provider (Claude through Bedrock or Vertex behind the same AI Gateway) would need the
person's keys and is not in this plan.

## Phases

Each phase is built on its own branch on Artifacts, rebased onto main as the split lands, and
landed only after the split planner says the split is complete.
- Executors for the building are AI Gateway jobs (`models_submit`, Sonnet for code, Haiku for
  mechanical moves), each with whole files and a precise spec.
- I read every line they return and run `cargo fmt`, clippy with warnings denied, the smoke suite
  and the wasm check myself.

### Phase 0: spikes (no landing, nothing deployed but throwaways)

- S1. A Messages turn with `tools`, a cached system prompt and adaptive thinking through
  `models_run {raw}` today. Check `tool_use` blocks and `cache_read_input_tokens` on the second
  call, and whether `anthropic-beta` compaction passes the gateway.
- S2. The sandbox image: `FROM` the CI base image plus (A) the Sandbox SDK's server, or (B) our own
  exec server. Then `cargo test -p pigeon-core` inside it on `standard-4`, the image size, the
  cold start and a Worker deploy while the container runs (does it survive?). This needs a
  throwaway script, so it goes to the person or the split planner's deploy route, as T17 did.
- S3. With (A): an `outboundByHost` handler adding a bearer to a `git push` through the proxy.

### Phase 1: model turns in the gateway (`claude/ai-agents-models`, `src/models`)

Decision 2's table: `request`, `betas`, content blocks and cache usage in results, chunked bodies,
`models_cancel`, the `ai-agents` lane, retries before the first byte, `task_prefix`.

Smoke: `models_turn_request` (the body checks: refused keys, allowlisted betas, bounds),
`models_stream_blocks` (`tool_use` input deltas joined, thinking with signature, usage with cache
tokens) and `models_lane_cap`. All pure.

### Phase 2: the smallest useful slice: one code executor, end to end

The new crate and Worker go on `claude/ai-agents-exec`.
- Its map entry goes in `ci/workers.toml` and its deploy job in `.pigeon/ci.toml`, when the crate
  exists. Both are gate changes (delegate approval).
- Core gains the `service_user` op and the `run` caller field (small, coordinated).
- **Tools**: `ai_agents_setup`, `ai_agents_start {role: "executor", brief, base?, budgets?}`,
  `ai_agents_status`, `ai_agents_stop` and `ai_agents_pause`.
- **Executor**: a sandbox from (A) or (B) and the run grant. The loop goes:
  1. check out `base` (main by default);
  2. edit, build and test in the sandbox;
  3. push `claude/ai-agent-{run}`;
  4. run `ci_submit test` on the tip and wait on `ci_status`;
  5. report (landing stays with the planner or the parent).
- **Not yet**: questions, the AI Agents tab and planners. Approvals already work, through the inbox
  in the status.

**Done when** the parent can say `ai_agents_start {role: "executor", brief: "<a real, small pigeon
task>"}` and later read a report with a green CI job and a branch it did not touch. The live check is one such task, with its cost seen in `models_usage`.

Smoke: `agents_steps` (transitions, idempotent restart of `started` steps), `agents_budget`
(stops before crossing, cache-aware price), `agents_tools` (role lists and refusal before
`dispatch`), `agents_grant` (push only to the run's branches) and `agents_compact_trigger`. All
pure and well under a second.

### Phase 3: the hand-off (`claude/ai-agents-handoff`)

- `ask_person`, `ai_agents_inbox` and `ai_agents_answer`.
- Question cards on the Approvals tab.
- The **AI Agents** tab: runs, steps, cost, Stop and Pause.
- The Cost tab series.
- `ai_agents_register` and `ai_agents_report` for Claude Code sessions.

### Phase 4: planners (`claude/ai-agents-plan`)

- The planner role on Opus with `spawn_executor`, `executor_status`, `executor_result` and
  `executor_stop`. The executors are child runs with `parent_run`, their budgets carved out of the
  planner's.
- A planner reads the code in its own `standard-1` sandbox. It splits the work into briefs,
  reviews each executor's branch diff (`run git diff`), runs CI and lands phase by phase.
- Routing text goes in `docs/plans/README.md` (*For agents*).
- The live check: one plan of three or more executor tasks, planned, built and landed by a run,
  with its report reaching the parent.

### Phase 5: fallback and hardening

- The model fallback chains and the `provider` wait.
- `ai_agents_handoff` to a session.
- Per-Worker caller keys, with the split planner.
- The default-runtime toggle (optional).
- The budget extend flow.

### Phase 6: planners that can see and finish (2026-11-23, from a week of runs)

Evidence (7 days, `ai_agents_compare`): Opus planners cost $721 over 44 runs (19 failed, median
$9.78, median 64 turns). Sonnet executors cost $46 over 148 runs. Haiku mechanical runs cost
$0.07. In the last 24 h the gateway saw 953 Opus requests; the brief counts 182 M cache-read
tokens against 0.6 M output tokens (`models_usage` shows 613 k output tokens and no cache counts
for the same window). That is about 190 k cache-read tokens per planner turn. Planners improvised
around seven gaps. Each step below is one executor, one branch and one landing. Nothing here
changes a stored or built-in model default, `src/model-gateway`'s provider routing or the MCP
schema files beyond adding the new tools. Every new tool is scoped to what the run owns.

**What already exists, so it is not rebuilt.** `wait_executors` (blocks until one executor ends)
and `wait_events {minutes}` (also wakes on a note, event or timeout) are planner tools since phase
4, and a planner's history is already compacted by a Haiku summary
(`transcript::needs_compaction`). So the brief's `executors_wait` is not added. The planner's
prompt tells it to use `wait_executors` instead of polling `executor_status`.

**6a. Compaction that fires (item 6).**
- Finding: `transcript::cut_point` only cuts at a user message with no `tool_result`. In a tool
  loop every user message after the first is a tool result, so there is almost never a cut and a
  planner's context grows without bound. A mean of 190 k cache-read tokens per turn, above the
  150 k trigger, fits that.
- Fix: cut before an assistant message, which is always a clean cut (its `tool_use` blocks and
  their results are both kept). `compacted` already handles a kept list that starts with an
  assistant message, by putting the summary first as a user message.
- Threshold, from the token data and Opus prices (a cache write for 1 hour costs about 20 times a
  read; the summary on Haiku costs about 0.05 USD): after a compaction the context is about 35 k
  tokens (tools, system, summary, last 16 messages). It then grows about 3 k per turn (tool
  results are cut at 16 KB). A compaction costs about 0.40 USD (the new prefix is written again).
  The cost per turn is lowest when a compaction comes every 20 to 25 turns, at a context of about
  100 k, and is flat from 80 k to 120 k. So `COMPACT_AT` goes from 150 k to 100 k and the kept
  tail from 20 to 16 messages. The executor re-checks the growth figure on stored `last_usage`
  data where it can, and says so if it changes the numbers.
- The cache prefix stays stable: the tools and the system prompt are built the same on every turn
  (`transcript::request`), and a compaction only replaces the messages after them.
- Effect to measure: cache-read tokens per planner turn, before (about 190 k) and after, from
  `models_usage {task_prefix: "ai-agent/", by: [model]}` once the Worker is deployed and planner
  runs have used it.

**6b. Read tools for a run (items 1, 2, 3).** Agent and pigeon tools, offered to planners and
executors, all read-only.
- `model_probe {model, prompt?, max_tokens?}`: one bounded request (at most 64 output tokens, a
  prompt of at most 500 characters, 30 s) to a named model through the model gateway, as the run's
  caller, with the task tag `ai-agent/{run}/probe/{n}`. It answers `status`, `latency_ms`, token
  counts, cost and the provider's error text as the gateway gave it. A probe is not a run, takes
  no run id and spends no turn. Its cost counts against the run's own cost budget. At most 10
  probes per run. The model is checked with the same rules as a run's `model`; nothing is added to
  the gateway.
- `run_spend {}`: this run's own turns, tokens and cost so far and the budget left (numbers the
  run already keeps), plus `models_usage` for its own task prefix. No other run's spend.
- `cost_daily` joins the planner and executor pigeon tool lists (a read-only tool core already
  serves; its answer is platform-wide and holds no secrets).
- `observability_errors {worker?, hours?}`: a new read-only tool in
  `src/capabilities/src/observability.rs`. It answers, per Worker, the data
  `observability_summary` already gathers (top error messages, `by_status`, counts), filtered by
  `worker` (a script name). Where a log line names the run (core's audit lines carry the run
  label), `run` narrows to it; where the logs carry no tag, the answer says the filter was not
  applied. It reuses the summary's folding code and adds no new source. Planners and executors
  are offered it.

**6c. `branch_restack` and `branch_combine` (item 4).** A run's git grant lets it push only
`claude/ai-agent-{run}` and `claude/ai-agent-{run}-*`, and the sandbox image is pinned, so no new
sandboxd op is added (that would need the person to rebuild the image). Both are agent tools that
the run's Durable Object carries out over the sandbox ops that exist: `fetch`, `run` (git, in the
checkout) and `commit_push {force}`.
- `branch_restack {}`: fetches `main`, rebases the run's own branch onto it and force-pushes it
  with the lease. On a conflict it aborts the rebase, leaves the checkout clean and answers the
  conflicting paths (`git diff --name-only --diff-filter=U`) without pushing.
- `branch_combine {branches: [..]}` (planners): each name must be a branch of one of the
  planner's own executors (checked against the children the index lists,
  `claude/ai-agent-{child}`), or its own branch. It fetches `main` and each branch, resets the
  checkout to `origin/main`, cherry-picks each branch's commits (`main..branch`, oldest first) in
  the order given, then force-pushes the result to the planner's own branch. A conflict aborts and
  names the files and the branch. It refuses while the planner's own branch holds commits that
  are not in the list (it would drop them), unless the list names the planner's own branch.
- Both refuse any branch outside the run's ownership, and `main` is never moved. The pure part
  (the command lines, the ownership check, parsing the conflict list) is a new module,
  `src/ai-agents/src/branches.rs`, so the smoke suite tests it natively.
- Not built: pushing to an executor's branch. The planner's own branch is the one it lands.

**6d. Warn at 75%, hand off at the end (item 5).**
- Budget: when a planner's cost or sandbox minutes pass 75% of the budget, once, the run adds a
  user message: stop spawning executors, land or hand off what is green, then report. The
  existing last turn at 90% stays.
- Structure: when a planner ends (a `report`, or the over-budget end), the run appends a
  machine-made section, "Unlanded branches", with one row per executor: branch, tip (from the
  executor's last push), the executor's end state and CI status for the tip (`ci_commit`), leaving
  out those that `land_list` shows merged. The model's own text comes first, and the run adds the
  section, so it is there even when the model forgets.

**6e. A `kind` hint for planners (item 7).**
- `ai_agents_start {kind?}`, with `kind` one of `integrate`, `docs`, `fix`, `design`. It is kept
  on the run and shown in `ai_agents_get` and `ai_agents_compare`.
- A stored per-kind planner model: `ai_agents_models` can set one per kind, beside the existing
  slots. Resolution for a planner: the run's own `model`, the stored model for its kind if there
  is one, the stored planner default, the built-in default. **No stored or built-in default is
  changed**, and this work stores no per-kind value. The person decides after comparing tonight's
  Sonnet-planned runs.

Order and landing: 6a to 6e are built in parallel, each from `main`. The plan lands first. Then
each lands on its own after `sh ci/runner/test.sh` and the `test` job are green, and the next
restacks onto the new main. Conflicts are likely in `tools.rs`, `prompts.rs` and `object.rs`,
because each adds a tool or a step; keep both sides. Smoke scenarios go in
`tests/smoke/ai_agents.rs` (prefix `ai_agents_`) and, for the observability tool,
`tests/smoke/observability.rs`.

## The person's answers (2026-11-22, relayed by the parent)

1. Runs push and run CI only; landing stays with the planner or the parent. Phase 2's `land` flag
   is dropped, and executors don't get `land` in their tool list.
2. The default budgets above.
3. Both agent service users are linked to the person with their passkey. The parent tells the
   person when the `/auth/link` step is actually needed, and not before.
4. Words alone choose the runtime for now. There is no toggle.

## Open items (handoff, 2026-11-22 23:40 UTC)

The AI agents planner session (`session_015XFA1p5x6wTEFRENJaCUxN`) is wrapping up: the person
made the Claude chat the only entry point, and open work goes to AI Gateway planners and
executors through it. Each item: state, next step, who it waits on.

1. **A run that spends its budget before reporting** (overnight "What broke" 4). **Built** on
   this branch (Log, 2026-11-22): at 90% of its cost budget, or when a normal turn would cross
   it, a run gets one report-only last turn. The turn and wall limits are covered too (Log,
   2026-11-23). Waits on: review and landing.
2. **Revoke a run's git grant when the run ends** (`cleanup.md` item 12). **Built** on this
   branch (Log, 2026-11-22): core's `credential_revoke` op, called from the run's `end()` in
   every terminal state. Waits on: review and landing.
3. **The run id in core's audit lines** (Log, landing 1 "Departure"). **Built** on this branch
   (Log, 2026-11-22): an optional `run` on `users::Caller`, named in dispatch's policy audit
   lines and kept in an approval's summary. Not on the console's approval card yet
   (`console/approvals.js`). Waits on: review and landing.
4. **Local test runs.** The workspace's default target is wasm32, so `cargo test` natively needs
   `--target x86_64-unknown-linux-gnu` (for example `cargo test --target
   x86_64-unknown-linux-gnu --test smoke apple`). Worth a line in the agents' brief.

## Log

- 2026-11-22: designed. Read the plans, the models, dispatch, approvals, Lander, git proxy and CI
  container code, and the Cloudflare and Anthropic docs listed above. The person said: hold every
  landing, docs included, until the split is complete.
- 2026-11-22: the split planner agreed to sections 2 to 4, with these conditions:
  - **Job object migrations.** New SQLite migrations go at the end of the list only. A
    transferred object applies them on first use.
  - **Rebase after the `ModelJobs` transfer.** After it, `src/models` is the model-gateway lane.
  - **Gateway tokens.** Each job keeps an AI Gateway token, minted by core, of at most 15 min.
  - **The `pigeon-ai-agents` map entry** names `containers = ["Sandbox"]`, so the deploy proxy
    scopes container calls to it.
  - **`service_user`** goes in `CORE_OPS`. Core's ops require a signed caller that reads as
    `users::Caller`.
  - **`run`** is an optional `#[serde(default)]` field on `Signed`, with no contract bump.
  - **Caller keys.** Callers read their key through one function, so that a per-Worker caller key
    (recorded as an open item in workers-split.md) can drop in later.
- 2026-11-22: the person answered the four questions (above) and gave the go-ahead to build in
  parallel. Every landing is held until the split is complete.
- 2026-11-22: **S1 passed.** A turn with `tools`, a cached system prompt and thinking went
  through the gateway as it is today (`models_run {raw}`, Haiku).
  - The model called the tool, and the second turn read 1201 cached tokens.
  - Thinking came back with its signature.
  - Betas are untested: `raw` sends no headers. Phase 1 adds them.
- 2026-11-22: **Phase 1 built** on `claude/ai-agents-models` (`1e30771`, rebased on the gateway's
  bind `afb1515`). Executors wrote `turn.rs` and the new `stream.rs`; I wrote the runner, lanes,
  cancel, retries and wiring. The gate passes, with the smoke suite at 11 s.
- 2026-11-22: **Phase 2, the pure part, built** on `claude/ai-agents-exec` (`6c71b17`, stacked on
  phase 1).
  - The `pigeon-ai-agents` crate has `run.rs` (mine), plus `budget.rs`, `tools.rs` and
    `transcript.rs` (executors, each read line by line).
  - Smoke: `agents_loop_budgets_tools_and_turns`. It also checks that a turn the AI agent builds
    passes the gateway's own `turn` checks.
- 2026-11-22: **The sandbox: option B first**, pigeon's own exec server in the CI base image,
  built and tested natively here. Option A (the Sandbox SDK) stays open for later.
  - S2 with A needs a throwaway deploy and the person's passkey. B needs neither, and its
    container side is the one `CiRunner` already proves.
  - **The run grant waits for the split.** The grant (in `job_token` and the git proxy) touches
    `pigeon-proxy`, which is mid-cutover, so it is built after the split planner says the split
    is complete.
- 2026-11-22: **Phase 2, the Worker side, built** on `claude/ai-agents-exec`. It compiles for wasm
  with clippy clean; nothing is deployed.
  - `pigeon_ai_agents::object`: `AiAgentRuns`, the index plus one object per run, driven by
    `run::next`.
  - `pigeon_ai_agents::sandbox`: `Sandbox`, one container per run.
  - `pigeon_ai_agents::service`: `ai_agents_start` (executors only), `ai_agents_status`, `ai_agents_stop`,
    `ai_agents_pause`, `ai_agents_resume`.
  - `pigeon_ai_agents::prompts` and the thin Worker crate `src/ai-agents-worker` (script
    `pigeon-ai-agents`).
  - `ci/sandbox/`: the image (the CI base image plus `pigeon-sandboxd`), built and pushed by the
    person, as the CI base image is.
  - Departures from the plan:
    - **`run` is capped at 840 s, not 1800.** A run's step happens in an alarm with 15 minutes of
      wall time.
    - **Runs act as their starter for now.** That caller (such as the portal acting for the
      person) is marked with the run's id. The two AI agent service users come with core's
      `service_user` op, after the split.
    - **The run grant is asked of core (`credential` kind `run_grant`)** and is refused until
      core mints it, so a checkout of the private repository fails until then.
  - Found by review: a reminder or a summary left the model's turn unasked. `View::user_last` now
    covers it, and smoke checks it.
- 2026-11-22: **`pigeon-sandboxd` built** (`ci/sandbox/sandboxd`, its own workspace; executor
  output, read line by line).
  - Tested natively against a bare origin:
    - prepare is idempotent;
    - paths can't escape the checkout or reach `.git`;
    - exec and its timeout kill the whole process group;
    - commit_push pushes, and pushes nothing when nothing is staged;
    - neither the sandbox key nor the git credential reaches the AI agent's commands or
      `.git/config`.
  - The branch passes the gate (`526395e`).
- **What deploying `pigeon-ai-agents` needs, in this order, after the split is complete:**
  1. The person builds and pushes the sandbox image (`ci/sandbox/Dockerfile`), and its digest is
     pinned in `src/ai-agents-worker/wrangler.toml`.
  2. Then the map entry (`ci/workers.toml`, with `containers = ["Sandbox"]`), the `deploy-agents`
     job and its script land together (a gate change). They are held until then: with the entry,
     every shared landing would deploy `pigeon-ai-agents`, and that fails without the image. They
     also change the smoke scenarios that list every Worker.
  3. Core binds `AGENTS` (a service binding), mints `run_grant` credentials, and the git proxy
     takes them. Core also gets the `service_user` op.
  4. The person links the two AI agent service users at `/auth/link`.
- 2026-11-22: **The branch stack on Artifacts**, each on the one before it. The gate passes on
  every one.

  | Branch | What |
  | --- | --- |
  | `claude/ai-agents-models` | phase 1, the gateway's turns |
  | `claude/ai-agents-exec` | phase 2: the crate, the Worker, the sandbox and `pigeon-sandboxd` |
  | `claude/ai-agents-handoff` | phase 3, agent side: `ask_person`, `ai_agents_inbox`, `ai_agents_answer`, `ai_agents_register`, `ai_agents_report`, the AI Agents section on the Instances tab |
  | `claude/ai-agents-plan` | phase 4: planners, `spawn_executor`, `executor_*`, `wait_executors`, the routing rule in `docs/plans/README.md` |
  | `claude/ai-agents-fallback` | phase 5, in part: `ai_agents_handoff` |

  Departures:
  - **The console tab.** Questions and runs show as an AI Agents section on the Instances tab,
    drawn by `pigeon-ai-agents`. Answers and stops go through tools (the parent relays them), not
    console buttons: a console write is core's, and core waits for the split.
  - **An unanswered question.** The run goes on with the conventional choice and says so,
    rather than stopping. The person set the wait to 5 minutes.
  - **A child's budget** is set aside from its planner's at spawn and not refunded when the
    child spends less. That is conservative, and refunds come later.
- **Left for after the split** (core and deploy, in order):
  1. Rebase the stack on main.
  2. Core: the `AGENTS` service binding; `credential` kind `run_grant`, with the git proxy
     taking it for fetches and for pushes to `claude/ai-agent-{run}*` only; the `service_user` op
     and the two agent users; `run` in audit lines and approval cards.
  3. The person builds and pushes the sandbox image.
  4. The map entry and the `deploy-agents` job.
  5. Land phase by phase.
  6. The person links the AI agent users at `/auth/link`.
  7. Live check: one executor run on a small, real task.
- 2026-11-22: **Renamed to "AI agents"** (the person: "Yes let's use pigeon-ai-agents"). "Agent"
  alone clashes with Buildkite's CI agents and with runtime agents such as Datadog's.
  - The Worker is `pigeon-ai-agents`, in crates `src/ai-agents` (`pigeon-ai-agents`) and
    `src/ai-agents-worker` (`pigeon-ai-agents-worker`).
  - The tools are `ai_agents_*`.
  - The model gateway's lane is `ai-agents`.
  - The console section is "AI Agents".
  - Task tags are `ai-agent/{run}/…`, and run branches `claude/ai-agent-{run}`.
  - The Durable Object classes are `AiAgentRuns` and `Sandbox`.
  - The sandbox image is `pigeon-ai-agent-sandbox`.
  - This plan is `docs/plans/ai-agents.md`.
  - The branch stack was rewritten in place under new names: `claude/ai-agents-models`, `-exec`,
    `-handoff`, `-plan`, `-fallback`. The old `claude/agents*` branches were deleted. The gate
    passes on the bottom, the middle and the top of the stack.
  - The service users keep their names, "Pigeon planner" and "Pigeon executor". Neither name
    says "agent", so neither clashes.
- 2026-11-22: **Questions wait 5 minutes** (the person: "significantly tighter like 5
  minutes").
  - After that, the model is told to make the conventional choice, say it, and list it in its
    report under "Assumed:".
  - The question is kept on the run's row and shown under `assumed` in `ai_agents_inbox` and in
    the AI Agents section.
  - A late `ai_agents_answer` explains that the run went on, and how to undo it: stop the run,
    or start a new one.
  - Built on `claude/ai-agents-handoff` (`ae76457`), with `-plan` and `-fallback` restacked on
    it. The gate passes.
- 2026-11-22: **Two landings, not five** (the person: "Yes please"). The phase commits stay as
  they are, rebased rather than squashed, so bisect and attribution still work.

  **Landing 1, `claude/ai-agents-land-1`.** Everything except core's binding to the new Worker:
  - phase 1, the gateway's turns;
  - core's `run_grant` credential and the git proxy taking it, the `service_user` op, and the
    run id in audit lines;
  - phases 2 to 5 (`pigeon-ai-agents`, `pigeon-sandboxd`);
  - the `ci/workers.toml` entry (`containers = ["Sandbox"]`) and the `deploy-ai-agents` job,
    which are gate changes;
  - this plan.

  It deploys in the Lander's order: core first (shared paths), then `pigeon-model-gateway`, then
  `pigeon-ai-agents` for the first time. `pigeon-ai-agents` binds `PLATFORM`, which exists, so
  it deploys cleanly. Nothing reaches it yet, because core does not bind it.

  **Landing 2, `claude/ai-agents-land-2`.** Core's `AGENTS` service binding, alone. The
  `ai_agents_*` tools and the AI Agents section appear.

  The binding cannot ride in landing 1. A service binding's target must exist before the caller
  deploys, and a landing with shared paths deploys core first.

  Rollback stays clean:
  - Reverting landing 2 removes the binding, and with it the tools.
  - Reverting landing 1 leaves `pigeon-ai-agents` deployed but unbound.
  - The `ModelJobs` migrations only add columns and a table. Older code ignores them.

  The person's steps, in order:
  1. Before landing 1: build and push the sandbox image. Its digest is pinned in landing 1.
  2. After landing 1: link the two service users at `/auth/link`.
  3. After landing 2: the first live run.
- 2026-11-22: **The split is complete. Landing 1 is built** on `claude/ai-agents-land-1`: the stack
  rebased on `233052e`, plus core's side, plus the lane map entry, plus this plan.
  - **Run grants.** Core's `credential` kind `run_grant` mints a Directory token of kind
    `run_grant` (12 hours) for a caller acting for a person.
    - The git proxy lets it push only `claude/ai-agent-{run}…`.
    - The deploy proxy refuses it outright.
    - `ProxyCaller` gains `branch_prefix`.
  - **Service users.** The core op `service_user` gives the two delegates from a fixed table,
    made on first ask, with a link request for the person while they are unlinked.
    - `ai_agents_setup` answers the links to approve.
    - A run acts as its role's delegate once that delegate is linked to the same person, and
      as its starter until then.
    - Runs are read by the person their caller stands for.
  - **Departure.** The run id is not added to core's audit lines in landing 1. That needs a
    field on core's `Caller`. Audit lines and approval cards name the run's delegate ("Pigeon
    executor"), and the run's id is in its model tasks (`ai-agent/{run}/…`) and its branch.
  - Revoking a run's grant when it ends is left for later. The grant lives 12 hours.
- 2026-11-22: first live executor run through the model gateway (this commit).
- 2026-11-22: **the fixed planner cap goes.** Live runs were capped per role at 2 planners and 6
  executors (`MAX_LIVE`), and a third planner queued behind the two. Now:
  - one ceiling of 32 live gateway runs over every role (`MAX_LIVE`), which is also the ceiling on
    sandboxes; a planner leaves 8 of them for executors (`EXECUTOR_RESERVE`), so planners can
    never take every slot and starve their own executors; each role still starts oldest first;
  - a planner's executors stay bounded by its `executors` budget, and pause and stop are the kill
    switch as before;
  - the model gateway's `ai-agents` lane goes from 6 to 16 turns at once, or it would be the
    next queue;
  - both stay constants, one place each: a setting changeable without a code edit would need a
    new tool and a policy rule for it, not worth it for a number that changes rarely.
  - The smoke suite admits five planners at once, fills to 24 planners and the 8 reserved
    executors, holds the ceiling and keeps each role's queue in order.
- 2026-11-22: **A run reports before its budget runs out** (overnight "What broke" 4, Open items 1).
  - At 90% of its cost budget, or when a normal turn would cross it, a run gets one last turn.
  - The last turn is report only: no thinking, `report` forced as the tool, 4096 output tokens.
    The prompt says what was spent and that this is the last turn.
  - That turn skips the cost and turn-count checks, since the last 10% is kept for it. The
    wall-clock check still applies.
  - If the model answers without calling `report`, the run ends `over_budget`. Its report is
    the model's last text, or a line saying the last turn did not report.
  - The report tool carries no cache marker: tools come before the planner's 1-hour system
    marker, and a 5-minute one before it is refused.
  - The smoke scenario is `ai_agents_last_turn_reports_before_budget`.
- 2026-11-22: **A run's git grant is revoked when it ends** (cleanup.md item 12, Open items 2).
  - New core op `credential_revoke` (body: a `CredentialRequest`, kind `run_grant`, the run as
    `instance`), which asks the new Directory op `run_grant.revoke`. It deletes the grants whose
    scope equals the run's branch prefix, never a prefix match, and answers `{"revoked": n}`;
    a grant already gone is `0`.
  - Who may revoke: the same person rule as minting (`split::grant_person`), so only the person
    the grant was minted for.
  - `end()` revokes in every terminal state (done, failed, stopped, over budget), after the
    sandbox is destroyed. A failure only logs; the grant still expires after 12 hours.
  - The proxy Worker keeps a verified credential for at most a minute, so a revoked grant works
    for up to a minute more.
  - Smoke scenario: `core_run_grant_revoked_when_the_run_ends`. Deploy order: `src/contract` is
    shared, so core deploys first and knows the op before `pigeon-ai-agents` asks it.
- 2026-11-22: **The run id in core's audit lines** (landing 1's Departure, Open items 3).
  - `users::Caller` has an optional `run` (serde default, left out when `None`), so callers
    without it read and write as before.
  - It is a label only. It never decides anything and is not in the policy engine's input.
  - Dispatch's tool-call lines carry it, and an approval keeps it in its summary, so the
    later `ran`, `expired` and console lines for that approval carry it too. Only 1 to 32 ASCII
    letters and digits are kept (`run_label`).
  - The Directory's kept decisions are unchanged, so there is no migration.
  - The smoke scenario is `core_audit_lines_name_the_run`.
- 2026-11-23: **The run id shows on the approval card.**
  - `listing` adds `run` to each call (null when absent). The card shows an "AI agent run" row when it is a non-empty string.
  - It reads `summary.run`, so it shows once `claude/ai-agent-04O5pJP1q2fz` lands (the run in core's audit lines, MXgiXQgySZx77QbT). Until then the row is hidden.
  - The smoke scenario is `platform_approvals_list_shows_run`.
- 2026-11-23: **The last turn also covers turns and wall time** (Open items 1).
  - The last turn is due on the first of: cost (as before), turns (`spent.turns + 1 >=
    budget.turns`, so the next turn is the last the budget allows), or wall time (less time left
    than the reserve).
  - The wall reserve is the larger of 10% of `wall_ms` and 5 minutes, and never more than half of
    `wall_ms` (`budget::wall_reserve_ms`). For 8 hours it is 48 minutes.
  - The `last_turn` column now holds a code: 1 cost, 2 turns, 3 wall. Every `last_turn != 0`
    check still works; no migration. The prompt and the `over_budget` end text name the limit.
  - The last turn still skips the cost and turn-count checks. The wall check still applies to
    every step.
  - Known limit: a long sandbox command that starts just before the reserve can outlast it, and
    then the wall check ends the run before it reports. Not fixed.
  - The smoke scenario is `ai_agents_last_turn_on_turns_and_wall`.
- 2026-11-23: **The last turn no longer forces `report`** (bug: run tOvWHV92KnYa, Anthropic 400
  "tool_choice type tool and any are not supported for this model").
  - The last turn offers only the `report` tool with `tool_choice` `{"type":"auto"}`. Thinking
    stays off, old thinking blocks stay stripped, and the prompt still says to call `report` now.
  - The request is the same for every model, planner or executor, so it is valid for all.
  - A reply with plain text and no `report` still ends the run `over_budget`, with its last text
    as the report.
  - The smoke scenarios `ai_agents_last_turn_reports_before_budget` and
    `ai_agents_last_turn_on_turns_and_wall` now expect `auto`.
- 2026-11-23: **`commit_push` pushes local commits, and can force-push the run's own branch**
  (bug: a rebase or cherry-pick committed with `run` never left the sandbox, since
  `commit_push` pushed only when it had something new to commit).
  - sandboxd (`ci/sandbox/sandboxd`, a gate change): it commits when there is a change, then
    pushes HEAD, whatever it is, to the run's branch. With `force` it pushes with
    `--force-with-lease`. The lease is `refs/remotes/origin/{branch}`, the tip this checkout
    last saw on the remote: set by `prepare`, by each push, and by `fetch` of the branch. It
    answers `committed`, `pushed`, `forced` and `nothing` (nothing to commit or push). It
    refuses while a rebase, merge, cherry-pick or revert is half done. A rejected push says
    whether to fetch first or to push with `force`.
  - The Worker: the `commit_push` tool takes an optional boolean `force`. The branch is still
    the run's own, never one the model names. Image v2's answer, which has no `committed`,
    tells the model that local commits leave only with its next commit and `force` is not
    available yet. Changing the tool list makes live runs retry once (stale thinking).
  - The git proxy: a run's grant now covers only `claude/ai-agent-{run}` and
    `claude/ai-agent-{run}-…`, as the design says. Before, any ref starting with the prefix
    passed, so run `ab` could push or force-push run `abc`'s branch. The policy still allows
    the rewrite (`session-branches`). Main still never moves by a push.
  - **Needs the person:** rebuild and push the sandbox image (`ci/sandbox/Dockerfile`), then pin
    its digest in `src/ai-agents-worker/wrangler.toml`. Until then runs keep image v2's
    behaviour.
  - Smoke scenarios: `git_proxy_run_grant_rewrites_only_its_own_branch` and
    `ai_agents_commit_push_pushes_local_commits_with_lease`. sandboxd was tested by hand against
    a bare origin: a local commit pushed with nothing staged, nothing to push, a rejected
    rewrite, a forced rewrite, a stale lease refused, a rebase onto the fetched branch pushed,
    and a half-done merge refused.
- 2026-11-23: **Sandbox v3 pinned.** `src/ai-agents-worker/wrangler.toml` now pins v3's linux/amd64
  manifest (`sha256:5dcfddbb…`), built by the person from 70da889, so `commit_push` pushes local
  commits and takes `force`. The Worker no longer tells the model about image v2's limits.
- 2026-11-23: **Phase 6 planned** (a week of runs: planners improvised around no probe, no logs, no
  cost view, manual landing trains, budget end-games, long contexts and a missing kind hint).
  `wait_executors` and `wait_events` already exist, so `executors_wait` is not built. Compaction
  never fires in a tool loop because `cut_point` wants a user message without a tool result; 6a
  fixes that. Per-kind planner models are stored but unset, and no default changes.
- 2026-11-26: **Read-only observability tools for runs.** `cost_daily`, `mcp_conformance` and the
  new `observability_errors {worker?, hours?, limit?}` join `EXECUTOR_PIGEON` (now 10) and
  `PLANNER_PIGEON` (now 15), their schemas sit in `prompts::pigeon_tools`, and all three are
  idempotent. Core's dispatch has no per-tool list for callers, so nothing else was needed.
  `observability_errors` reuses the summary's metrics and log folds; log lines carry no run tag, so
  there is no `run` filter and the answer says so. An unknown `worker` answers an empty list and
  the known script names.
- 2026-11-23: **`model_probe` and `run_spend`** added as agent tools for every role (new module
  `probe.rs`). A probe is one `models_run` request (at most 64 tokens, 30 s), tagged
  `ai-agent/{run}/probe/{n}`, never a run or a turn; at most 10 per run, counted in the run's
  `flags` table before it is sent. Its cost is added to `Spent.cost_micros` (and `gateway_micros`
  only if the gateway gave it) with no `turn_costs` row, so reconcile never touches it. The
  gateway's `models_run` gives tokens but no cost, so the cost is normally an estimate. A timed-out
  probe is not charged. `run_spend` reads only what the run keeps.

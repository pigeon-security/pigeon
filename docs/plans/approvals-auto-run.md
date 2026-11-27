# Approved operations run themselves

Status: **in progress** (27 November). Branch `claude/approvals-auto-run`.

The person asked: today every approval-linked operation needs an agent round-trip. A tool call
creates an approval and answers its id; the person approves on the console; then the person goes
back to the session and says "approved", so the agent calls the tool again with the id. A mail
forwarding change takes three such round-trips (`email_forward_set` → record change → plan →
`instance_apply`). From now on: **create the approval → the person approves → the platform runs
the approved operation at once**, and if that run asks for the next approval, the chain continues
the same way.

## What runs on approval

Every kind of approval, with the operation its approval stands for. The operation is always a
tool call, run through the same `dispatch` as `/mcp`, as the approval's original caller:

| Kind | Asked by | Runs on approval | Notes |
| --- | --- | --- | --- |
| `tool_call` | any gated tool (`risky-tools-need-passkey`: `setup_platform`, `users_remove`, `users_admin`, `instance_member_set`, `passkeys_remove`, `oauth_revoke`, `email_forward_set`) | the tool, with the arguments the person approved and `approval: <id>` | **Not** `source_token` (see below) and not the console's own writes (`via: console`), which use their approval in the same request |
| `instance_draft` | `instance_propose` (and `email_address_set`, `email_forward_set` through it) | the draft is applied by the Directory as today; then `instance_plan {instance}` | the plan's own approval is the next step |
| `linked_plan` | `instance_plan` | `instance_apply {plan: <id>}` | re-reads the account and refuses when the plan's digest changed, as today |
| `linked_plan` (retire) | `instance_retire_plan` | `instance_retire_apply {plan}` | a stopped run is resumed by calling again, as today |
| `linked_plan` (rename) | `instance_rename_home` | `instance_rename_home {plan}` | |
| `apps_cleanup` | `apps_cleanup` | `apps_cleanup_apply {plan}` | |
| `portal_oauth_redirects` | `portal_oauth_redirects` | `portal_oauth_redirects_apply {plan}` | |
| `observability_monitors` | `observability_monitors_plan` | `observability_monitors_apply {plan}` | |
| `ingest_run` | `ingest_plan` | `ingest_start {plan}` | |
| `cloud_change` | the cloud gateway's plan tools | `cloud_apply {plan}` | a capability Worker's tool, reached through its binding |

A kind not in this table (a newer one) does not run on approval: it keeps the old flow, and the
console says so.

**`source_token` keeps "approve, then call again".** It answers a live git token. Running it on
approval would leave a live token at rest in the Directory until the agent fetched it, and its
lifetime would run from the approval, not its use (the reason in `tool-approvals.md` §3). A tool
that answers a secret is listed in `approvals::ANSWERS_SECRET`; its approval behaves as before.

## Security

- **The approval binds exactly what runs.** A `tool_call` approval now keeps its arguments (after
  instance binding) and the instance endpoint it came through in `payload`. Before running, the
  runner recomputes `call_digest(tool, args, caller)` and refuses when it is not the digest the
  person signed for; then the tool's own `take` checks it again (one Directory step: approved,
  not expired, this caller, this digest, then used). Plans keep their existing binding: the apply
  tool re-plans and takes the approval only if the new plan's digest is the approved one.
- **Once.** The runner first *claims* the approval in the Directory (`approval.claim`: approved,
  not expired, not claimed before; it records `outcome: running`). The run itself spends it
  (`approval.take`, approved → used) as the agent's call used to. Two runners, a retried alarm, or
  an agent calling again cannot run it twice: a second claim is refused; a second take is refused;
  an agent's call with the id after the claim is answered from the outcome without running
  anything (below). A runner that dies mid-run leaves `running`; the next alarm marks it
  `failed: interrupted, whether it ran is unknown`, and never runs it again.
- **As the original caller, checked again now.** The runner rebuilds the caller from the Directory
  (`caller.of`): the same service user, and it must still act for the same person (a link removed
  or changed since refuses). Then it runs `dispatch_checked`: the person's role on the instance
  (membership) and the policy (`call_tool`) are asked again, now, for that caller. A policy that
  now refuses the tool, or a role taken away, refuses the run, recorded as `failed`. The
  approver's passkey is the consent to that call; it never runs anything as the approver.
- **Denied, expired or pending approvals never run**: the claim needs `approved` (a draft:
  applied, `used` with no note) and not past its time.
- **Stored results.** The outcome keeps a short summary and the result, redacted with the same
  redactor as summaries and cut to 8 KB, for an agent that calls again. The run's arguments are
  cleared from the payload once it has run.

## Durability

The console's write (`approval-approve`) only decides, then hands the id to a new Durable Object,
**`Approver`** (platform Worker, migration `v7`), which stores it and sets its alarm to now. The
alarm runs each queued approval in turn and records the outcome; the console request can end at
once. An alarm may run for up to 15 minutes, well past an apply's tens of seconds (`waitUntil`
gives 30 s; a queue would need a new queue made outside the code). Alarms do not overlap, so a row
still `running` when an alarm starts means the last alarm died: that approval is marked
interrupted.

## What people and agents see

- The record gains `outcome {status: running|done|failed, summary, result, error, next,
  started_ms, finished_ms}` and `after` (the approval whose run asked for this one).
- `approvals_status {id}` answers `outcome` and `next`; `approvals_list` adds `recent`, the
  outcomes of the last day's runs, and each pending item's `after`.
- The console's Inbox shows a "Ran on approval" table under the pending requests: what ran, its
  result or why it failed, and the next approval with its own Approve button. A pending item asked
  by a run says which approval it follows.
- **Agents calling again** with the id (the old flow) get the recorded result: `done` answers the
  stored result; `running` answers "running, see approvals_status"; `failed` answers the error.
  Only `instance_retire_apply` still goes through on a failed outcome, since calling it again
  resumes a stopped retire.
- Tool descriptions say the approved call runs on approval; the ids are for `approvals_status`.

## Not changed

- `.pigeon/policy.toml`: nothing new is needed. The runner asks the same `call_tool` rules as any
  call; with no `call_tool` rule, gated tools are not gated and nothing is created to run.
- `.claude/settings.json`, `ci/`: unchanged.
- iOS: the inbox's shape only gains fields (`recent`, `after`); the app's text "runs once, if its
  caller calls again within 10 minutes" is updated, which needs an app release to show.

## Phases

| Phase | What | State |
| --- | --- | --- |
| 1 | `pigeon_core::approval`: `Outcome`, `after`, `claim`, `finish`, `recall`. Directory ops `approval.claim`, `approval.outcome`, `approval.recent`. `approvals`: follow-up table, payload for tool calls, re-call answers, status and list fields. `Approver` object, binding and migration; console trigger; console Inbox; iOS text; the AI agent runs wait for the outcome; tool descriptions; smoke scenario `approvals_auto_run`. | built, gate green locally; landing |
| 2 | Live check: a gated call approved runs once with no agent call; the chain record change → plan → apply. | — |

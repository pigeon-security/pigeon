# Passkey approval for risky tool calls

Status: **phase 1 built** on `claude/tool-approvals-3` (21 November); the person chose option (a), relayed by the parent session. Phase 2 (the rules) waits for their passkey.

The person asked: "Can you seed a session to require a pass key approval to use those tools?" The
tools are the ones listed as candidates to switch off at the MCP portal:

| Tool | Capability | What it does | Arguments |
| --- | --- | --- | --- |
| `deploy_token` | `deploys` | Mints a Cloudflare API token (break-glass deploy) | `ttl`, `permissions` |
| `source_token` | `source` | Mints a git token for the source repository | `access`, `ttl` |
| `setup_platform` | `public_host` | Sets up `platform.pigeon.dev`: DNS, route, Access state | none |
| `users_remove` | `users` | Removes a user and all that is theirs | `id` |
| `passkeys_remove` | `passkeys` | Removes a passkey | `id` |
| `oauth_revoke` | `oauth` | Removes an OAuth client, its tokens and link | `client_id` |

Goal: a call to any of these, from any caller, does not run until the person approves **that exact
call** with their passkey. Nothing else changes. No other tool is added to the list; if one seems
to belong there, the person is asked.

## Decisions

### 1. Where it is enforced: `pigeon_capabilities::dispatch`

`dispatch` (`src/capabilities/src/lib.rs`) is already "the one way a tool is run". Every route
goes through it:

| Route | Path to `dispatch` |
| --- | --- |
| Sessions through the portal (`mcp.pigeon.dev`) | portal → `platform.pigeon.dev/mcp` → `mcp_post` → `dispatch` |
| Direct `/mcp` with an issued `pgn_at_` token or an Access service token | `mcp_post` → `dispatch` |
| `call {tool, args}` | `Dispatch::call` → `dispatch` (so `call` is no way round it) |
| The Lander's own calls (`ci_submit`, `ci_cancel`) | `Lander::dispatch` → `dispatch` (none is gated) |

The check sits after the owner lookup (an unknown tool fails as today) and before `owner.call`.
It cannot be skipped by an agent's instructions, by `call`, or by a different MCP client.

**The console** never runs these tools. Its own writes that look alike (remove a passkey,
disconnect an app) are already bound to the person's passkey, one ceremony per write
(`console/writes.rs`). They are unchanged. The `/tokens` and `/secrets` pages likewise already
need the person's passkey.

### 2. How it is expressed: one new action kind and the person's rule

The closed set of action kinds grows by one, to 14: **`call_tool`**, whose target is the tool's
bare name (`deploy_token`, not `pigeon_deploy_token`). Tool names are valid targets as the
loader checks them today; nothing else in the engine changes.

The rule the person asked for, added by them with their passkey (`policy-is-yours`):

```toml
[[rule]]
id = "risky-tools-need-passkey"
actions = ["call_tool"]
targets = ["deploy_token", "source_token", "setup_platform", "users_remove", "passkeys_remove", "oauth_revoke"]
decision = "allow"
approval = "passkey"
reason = "break-glass tokens, platform setup and removals run only once the person approves that exact call with their passkey"
```

The engine already answers such a rule, without the passkey, as *deny, needs passkey*. That
answer is what `dispatch` turns into a pending approval.

**One question for the person.** The policy denies whatever no rule covers ("not in policy").
Once `dispatch` asks `call_tool` for every call, every other tool needs a rule to keep running.
Two ways, the person chooses:

- **(a, recommended)** A second rule, after the first, that keeps today's behaviour visible in
  the file:
  ```toml
  [[rule]]
  id = "tools-run"
  actions = ["call_tool"]
  decision = "allow"
  reason = "every other tool runs as it does today"
  ```
- **(b)** No second rule; `dispatch` asks the policy only whether a call *needs approval* and
  treats `not in policy` as "runs". This keeps the file to one rule but makes `call_tool` the one
  kind where the default is not deny, and `policy_check` would answer `deny, not in policy` for
  tools that in fact run. Not recommended.

Either way `.claude/settings.json` does not change: the generator maps `call_tool` to no Claude
Code tool (`Kind::CallTool => &[]`), so nothing is pre-approved by it. `policy_check` lists the
new kind, so an agent can ask `{action: call_tool, target: deploy_token}` and be told it needs
the passkey.

A policy that does not parse already denies everything it decides. With `call_tool`, that would
stop every tool, `land` included. The gate never lets such a file land (the settings generator
and the smoke suite parse it), and this plan adds no other way for it to change.

### 3. The flow: pending id, passkey on the console, then the caller calls again

1. **First call.** `deploy_token {"ttl": 900}` without an approval. `dispatch` computes the call's
   digest:

       digest = sha256(canonical JSON of {tool, args with keys sorted, caller: caller.user.id})

   and stores a pending approval in the Directory (`approval.create`). It answers at once, as a
   tool error the model reads, never as a success:

   > needs the person's passkey: approval `aP3x…` waits on the console's Approvals tab
   > (https://platform.pigeon.dev/approvals) until 14:32 UTC. Once approved, call `deploy_token`
   > again with the same arguments and `"approval": "aP3x…"`; `approvals_status {id}` says where
   > it stands.

   The same caller asking the same call again while one is pending gets the same id, not a new
   one. A caller has at most 5 pending at once.
2. **The Approvals tab** shows a card per pending call: the tool, its arguments, the caller
   (service user and whom it acts for, e.g. *Cloudflare MCP Portal acting for Willow*), the
   client label when one is known, when it was asked, when it expires, and the digest's first 12
   characters. Arguments are shown redacted: any key containing `token`, `secret`, `password`,
   `key` or `value` shows as `‹redacted›`. None of today's six tools takes such an argument, but
   the rule holds for any tool later gated. The digest covers the unredacted arguments, so
   redaction never weakens the binding.
3. **Approve** is a console write, `approval-approve`, like `land-approve`. Its passkey challenge
   purpose is `console:approval:{id}:{digest}`: the assertion is bound to that one call. Finish
   checks, in the Directory, in one step: the approval is pending, not expired, its digest is the
   one signed for, and the signed-in person may approve it. **Deny** (`approval-deny`) needs the
   signed-in session only, since refusing gives nothing away.
   - Who may approve: a person (standard user) signed in to the console. When the caller acts
     for a person, only that person. When it acts for no one (an unlinked service user, an
     Access service token), any person.
   - No MCP tool approves or denies. An agent cannot approve its own call: approval is a WebAuthn
     assertion made on the console's origin.
4. **Second call.** The caller calls the tool again with the same arguments plus
   `"approval": "<id>"`. `dispatch` takes `approval` out of the arguments, recomputes the digest
   (same tool, same arguments, same caller), and takes the approval (`approval.take`): approved,
   not expired, digest equal, then marked **used**, all in one Directory step, so a second call
   with the same id is refused. Then the tool runs, once, and its result goes straight back to
   the caller. Different arguments, a different caller or tool, or a used, denied or expired id
   are refused, and the refusal says which.

**Why the caller calls again, rather than the approved call running and its result being fetched
by id:**

- Two of the six answer live secrets (`deploy_token`, `source_token`). Run-and-fetch would keep a
  live token at rest in the Directory until someone fetched it, and its lifetime would run from
  the approval, not from its use. Calling again keeps the token in one answer to the one caller,
  as today.
- The tool runs in its caller's own context: the same `cx.caller`, the same AI Gateway tags,
  the same audit actor. Run-and-fetch would have the console run tools as an agent's caller,
  which nothing in pigeon does today.
- No second credential is needed to fetch a result, and nothing is left to clean up.
- The cost is one more call by the agent. `approvals_status` (read-only, any caller, own approvals
  only) lets it wait without guessing; the person can also just tell the session.

**Expiry.** A pending approval expires 10 minutes after it is asked; an approved one 10 minutes
after it is approved, if not used. Expired rows are swept when approvals are listed or created,
and the sweep records each expiry once.

**Audit.** Every step is a policy audit line (`"audit":"policy"`, by `dispatch` or `console`)
and a kept decision (`policy.log`), with action `call_tool`, the tool as target, rule
`risky-tools-need-passkey`, and the approval id in the reason:

| Step | Decision | Reason |
| --- | --- | --- |
| asked | deny | `needs passkey: approval {id} pending` |
| approved | allow | `approval {id} approved by {person}` |
| denied | deny | `approval {id} denied by {person}` |
| expired | deny | `approval {id} expired` |
| ran | allow | `approval {id} used: ran, ok` or `… ran, failed` (never the result) |
| refused | deny | `approval {id} refused: {why}` |

Calls allowed by `tools-run` write no audit line, as today: the audit stays a record of decisions
that matter, and the Directory is not written on every call.

### 4. One approval mechanism, shared with instance drafts

The linked-accounts plan (session `session_015XFA1kti5zV4yt43rtNdxY`) puts `instance_propose`
drafts on the same Approvals tab, approved with a passkey bound to the draft's digest. Both use
one table and one console write:

- Directory table `approvals`: `id, kind, subject, digest, summary (JSON, redacted), payload (JSON), caller_id,
  approver_id, state (pending|approved|denied|expired|used), created_ms, expires_ms, decided_by,
  decided_ms, used_ms`. Ops: `approval.create`, `approval.list`, `approval.get`,
  `approval.decide`, `approval.take`.
- `kind` is `tool_call` here and `instance_draft` there. What approval does depends on kind:
  `tool_call` only marks it approved (the caller calls again); `instance_draft` applies the draft
  at once (it holds no secret, and nothing is waiting to call). Lifetimes are per kind: 10
  minutes for a tool call, 7 days for a draft.
- One write pair, `approval-approve` and `approval-deny`, with the purpose
  `console:approval:{id}:{digest}` for both kinds.
- One Approvals section lists them, beside the landings; the tab's count is landings plus
  pending approvals.

Whichever planner builds the table first lands it; the other builds on it. Agreed by message
with the linked-accounts planner on 21 November, with these additions from them:

- A `payload` column (JSON). A `tool_call` leaves it empty, because its arguments come again
  with the second call. An `instance_draft` stores the proposed record there; approval applies the
  payload and never anything the caller sends again.
- A draft's `summary` is its diff, `{instance, version_from, changes: [{path, from, to}]}`. It
  goes through the same redactor, so there is one code path.
- **Stale drafts.** A draft's digest covers the payload and the record `version` it was made
  from. `approval.decide` returns the row and applies a draft inside the same Directory call, and
  only while the record is still at that version. Otherwise the row becomes `expired`, with "the
  record changed since this draft".
- **A third kind, `linked_plan` (linked-accounts phase 4).** It is a dry-run plan of changes in a
  linked account. It behaves like `tool_call`: approval marks it `approved`, then `instance_apply
  {id}` takes the row, plans again, and refuses if the new plan's digest differs. Its TTL is 30
  minutes and `payload` holds the plan.
- `Kind` in `pigeon_core::approval` is a serde enum with an `Other(String)` fallback, so a row of
  a newer kind never breaks an older reader. The purpose builder (`console:approval:{id}:{digest}`)
  and the state transitions are pure functions there, so either planner can add a smoke case,
  such as the stale-draft refusal.

### 5. Break-glass

`deploy_token` is for when the normal path is broken: the land flow, CI or the `/cf/` proxy's
allowlist. Approving needs only what serves `deploy_token` itself: the `pigeon-platform` Worker,
its Directory Durable Object and the person's passkey on the console. It does not need the
Lander, CI, Containers, the proxies, Artifacts or the GitHub mirror. If the portal is down, the
direct `platform.pigeon.dev/mcp` with an issued token reaches the same `dispatch` and the same
approval.

The allowed path does no I/O for the check (the policy is parsed once per isolate), so the
gate adds no new way for `land` and the other tools to fail.

**If the platform itself is down** (a bad deploy, the Worker or its Directory failing), there is
no `deploy_token` to approve, with or without this plan. The fallback is the person's own
wrangler, with their own Cloudflare login and nothing of pigeon's: on their machine,
`npx wrangler login`, then from `src/platform` of a good commit `npx wrangler deploy` (or
`npx wrangler rollback` to the last good version). Phase 3 adds this to "Break-glass: deploying
directly" in `docs/proxies.md`, which today names only `deploy_token`.

### 6. The MCP portal

- The six tools stay listed and **on** at the portal, as `pigeon_deploy_token`,
  `pigeon_source_token`, `pigeon_setup_platform`, `pigeon_users_remove`,
  `pigeon_passkeys_remove` and `pigeon_oauth_revoke`. They need to be on for the approval to be
  usable from a session; the passkey, not the switch, is now the control.
- Each gated tool's schema gains an optional `approval` string, and its description a sentence:
  "Each call needs the person's passkey: the first call answers an approval id; call again with
  it once approved." The schemas are built from the policy, so a tool the person later gates
  shows this without code changes. The post-deploy portal sync picks the new schemas up.
- The person may still switch any of them off at the portal. A switch survives a sync (checked
  with `call`, `mcp-portal.md`). Off at the portal means off for sessions through it; the direct
  endpoint still reaches the tool, and the passkey still gates it there.
- None of the six is in `.claude/settings.json`, so Claude Code also asks in the session before
  calling one. That is unchanged.

## Phases

| Phase | What | Lands with | Live check |
| --- | --- | --- | --- |
| 1 | `pigeon_core::policy`: `Kind::CallTool` (`call_tool`), 14 kinds; generator arm `=> &[]`. New pure `pigeon_core::approval`: canonical digest, redaction, the state machine (create, decide, take, expire, with `now`). Directory table and `approval.*` ops. Capability `approvals`: `approvals_status`, the Approvals section's data, the audit lines. Console: writes `approval-approve` and `approval-deny`, cards in `approvals.js`, the tab count. `dispatch` asks `call_tool` only **when the policy has a `call_tool` rule**, so this landing changes nothing live. One smoke scenario. | `land` (no guarded paths) | `policy_check {call_tool, deploy_token}` answers `deny, not in policy` (no rule yet); every tool runs as before; Approvals tab unchanged |
| 2 | The person's rules in `.pigeon/policy.toml`: `risky-tools-need-passkey` and `tools-run` (option a). From now on every call is asked. The guard stays (see "As built"). Settings unchanged. | `land` with the person's **passkey** (`change_policy`) | Call `deploy_token {"ttl": 300}`: it pends with an id; the card shows on Approvals; approve with the passkey; call again with the id: a token comes back; `revoke_api_token` it; the same id again is refused as used; a call with `ttl` 301 and that id is refused; a deny and an expiry each recorded. `land` and `policy_check` still run without a prompt. |
| 3 | `docs/proxies.md` break-glass: the approval step and the person's own `wrangler login` fallback; this plan's as-built record; README row. | `land` | — |

The smoke scenario (phase 1, in `tests/smoke.rs` only, well under a second): a policy text with
the rule; a gated call pends with a digest; its card's arguments are redacted; approval needs the
digest signed for; the call with the id runs once; the same id again, another argument, another
caller, a denied and an expired id are each refused; an ungated tool runs with no approval.

Executors (AI Gateway, `models_submit`) build fixed pieces, each with its files and tests named:
(A) the kind, the generator arm and `pigeon_core::approval`; (B) the Directory table and ops;
(C) the `approvals` capability and the `dispatch` hook; (D) the console write, section and
`approvals.js`; (E) the smoke scenario. I review each, run the gate locally, and land.

## Not in this plan

- No tool beyond the six. `oauth_issue_token`, `secrets_request`, `source_revoke`,
  `revoke_api_token`, `site_retire`, `users_unlink` and the rest are untouched; if the person
  wants any gated, it is one more target in their rule.
- No new proxy allowlist entries; no change to the portal's settings.

## As built (phase 1)

- `pigeon_core::approval` (pure): kinds (`tool_call`, `instance_draft`, `linked_plan`, any other
  kept as is), states, the canonical digest, redaction, `decide`, `take`, `expire`,
  `stale_draft`, the purpose. Written by an AI Gateway executor to a fixed spec, reviewed; one
  change: a stale pending approval refuses as expired, not as waiting.
- The Directory keeps each record as JSON in table `approvals`, with the columns it is looked up
  by (`id`, `caller_id`, `digest`, `state`, `expires_ms`), not one column per field: the record
  type stays the one place its shape is defined. Ops `approval.create` (sweeps, de-duplicates by
  caller and digest, at most 5 pending per caller), `approval.list`, `approval.get`,
  `approval.decide`, `approval.take`; a refusal answers `refused` with the record. Ended
  records are forgotten a day after they expire; the audit keeps them.
- `pigeon_capabilities::approvals`: `gate` (called by `dispatch`), `ran`, `annotate` (the
  `approval` argument on gated tools' schemas), `approvals_status`, and the calls in the
  Approvals section, which it now shares with the Lander (its data comes keyed `approvals` and
  `land`). The tab counts pending calls as well as landings.
- `dispatch` asks the policy only once it has a `call_tool` rule (`approvals::asked`), so phase 1
  changes nothing live. Phase 2's rules turn it on.
- A failed audit line after an approved call has run never loses the call's answer: it is logged
  as an error instead.
- Smoke scenario `tool_call_approvals`, the suite's 15th (the suite runs in 9 to 26 s here).
- **The guard stays after phase 2.** `dispatch` asks the policy about tool calls only while it
  has a `call_tool` rule. With the person's two rules that is always, so it changes nothing. It
  matters only if the person someday removes every `call_tool` rule: tool calls then run as
  before phase 2, rather than all being refused as "not in policy", `land` included, which is
  the way to change the policy back.

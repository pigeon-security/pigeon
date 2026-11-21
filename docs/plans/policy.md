# The person's policy

Status: deployed (landings kIAH-IkAPCOkk0t1 and 8yll8l5AUDN7uLXg; agent instructions in a third landing).

This is the as-built record of the policy engine and the smoke suite. It says what the plan was, what was built, and where building departed from the plan. The code is the last word. If this file and the code disagree, the code is right.

## What it is

The person's policy is a file of rules. The platform asks it before it does anything an agent could do on its own. The engine answers allow or deny.

- The file is `.pigeon/policy.toml`.
- `pigeon_core::policy` reads it (`src/core/src/policy.rs`).
- `pigeon_capabilities::policy` builds it into the Worker (`src/capabilities/src/policy.rs`).

A request is:

    {actor, action, target, context: {approval?}}

The answer is:

    {decision: allow|deny, rule, reason, needs}

The engine is deterministic. The first matching rule wins. A request that matches no rule is denied with the reason `not in policy`.

## The policy file

The set of action kinds is closed. There are 13:

`message_session`, `start_session`, `archive_session`, `handoff_branch`, `land`, `deploy`, `push_branch`, `rewrite_branch`, `delete_branch`, `move_main`, `change_policy`, `change_gate`, `add_test`.

A rule has these keys:

| Key | Meaning |
| --- | --- |
| `id` | Name of the rule. Unique in the file. |
| `actions` | One or more action kinds, none repeated within the rule. |
| `targets` | Optional. Patterns the target must match. |
| `except` | Optional. Patterns that remove a target from the match. |
| `decision` | `allow` or `deny`. |
| `approval` | Optional. `passkey` or `delegate`. A passkey also meets a delegate requirement. |
| `reason` | Text returned with the answer. |

A target is a branch name without `refs/heads/`, or a repository path. A trailing `*` makes it a prefix match. Two names keep the rules free of provider detail:

- `@session/` stands for the session branch prefix. On this platform that is `claude/`.
- `@main` stands for the protected branch.

The loader refuses the whole file, not just the bad rule, for any of these:

- an unknown action kind
- an unknown key
- a bad target pattern
- `approval` on a deny rule
- a repeated rule id, or an action repeated within a rule
- a missing or wrong `version`

## The nine rules

| Rule | Actions | Targets | Decision | Approval |
| --- | --- | --- | --- | --- |
| `agents-coordinate` | `message_session`, `start_session`, `archive_session`, `handoff_branch` | any | allow | none |
| `land-and-deploy` | `land`, `deploy` | `@session/*` | allow | none |
| `deploy-after-landing` | `deploy` | any | deny | none |
| `session-branches` | `push_branch`, `rewrite_branch`, `delete_branch` | `@session/*` | allow | none |
| `main-forward-only` | `move_main` | any | deny | none |
| `policy-is-yours` | `change_policy` | any | allow | passkey; without it, deny and `needs` the passkey |
| `smoke-suite-fixed` | `add_test` | any except `tests/smoke.rs` | deny | none |
| `smoke-suite-grows` | `add_test` | `tests/smoke.rs` | allow | none |
| `gate-changes` | `change_gate` | `ci/*`, `.pigeon/ci.toml`, `.cargo/*`, `rust-toolchain.toml`, `.npmrc`, `src/platform/.npmrc`, `src/platform/package.json`, `src/platform/package-lock.json` | allow | delegate |

Notes on the table:

- "Steer" is `message_session`.
- A deploy's target is the branch whose landing it follows.
- A landing's fast-forward is the `land` action, not `move_main`. So `main-forward-only` does not block a landing.

## Where the platform asks it

### policy_check

`policy_check` is an MCP tool. It answers as the person's delegate: the service account acting for the standard user. It says so in its `as` field.

Every decision writes one JSON audit line with `"audit":"policy"` to the Worker's log. The decisions of `policy_check` and the Lander are also kept in the Directory, the latest 200. The latest 10 are listed on the console Overview under "Policy decisions".

### The Lander

The Lander asks `land` for the branch first. It refuses a landing the policy denies.

Then it asks once for each guarded path that changed in the landing:

| Path | Action asked |
| --- | --- |
| `.pigeon/policy.toml` | `change_policy` |
| `.claude/settings.json` | `change_policy` |
| `ci/` | `change_gate` |
| `.pigeon/ci.toml` | `change_gate` |
| any other file in `.pigeon/` | `change_gate` |
| `.cargo/` | `change_gate` |
| `rust-toolchain.toml` | `change_gate` |
| the npm files | `change_gate` |

What happens to the answer:

- A change that needs the delegate is approved by the delegate. The approval is named in the landing's checked step.
- A change that needs the passkey waits for the person's passkey.
- A change the policy denies, or does not cover, also waits for the person's passkey.
- The console setting that makes every landing wait still works.

### The git proxy

Each ref update in a push is its own policy request:

- The protected branch is `move_main`. The policy always refuses it. Main moves only by a landing.
- A delete is `delete_branch`.
- An update of any other existing branch to another commit is asked as both `push_branch` and `rewrite_branch`:
  - Allowed as both (session branches, today): the push streams, and once it has gone through the proxy reads the branch's history and records it as `rewrite_branch` when the old tip is not in the new one's history, `push_branch` otherwise.
  - Allowed only as a push: the push is read whole and refused unless it moves forward, recorded as `rewrite_branch` denied when refused.
- Anything else, a create included, is `push_branch`.
- Tags and other refs are not in policy.

Each update's decision is an audit line; a streamed update that may be a rewrite gets its line after the push. The fast-forward check (`not_forward`) serves the protected branch, should the policy ever allow pushing it, and a branch pushed but not rewritten.

## The generated Claude Code settings

`cargo run -p pigeon-capabilities --example settings` writes `permissions.allow` in `.claude/settings.json`. It builds the list from the unconditional allow rules.

The provider's tool names live in the generator (`src/capabilities/examples/settings.rs`). They never appear in the policy.

The list holds:

| Entry | Source |
| --- | --- |
| `mcp__Pigeon__policy_check` | always (it only reads the policy) |
| `mcp__claude-code-remote__send_message` | `agents-coordinate` |
| `mcp__claude-code-remote__create_session` | `agents-coordinate` |
| `mcp__claude-code-remote__archive_session` | `agents-coordinate` |
| `mcp__Pigeon__land` | `land-and-deploy` |
| `mcp__Pigeon__land_status` | `land-and-deploy` |
| `mcp__Pigeon__land_cancel` | `land-and-deploy` |
| `Bash(git push https://proxy.pigeon.dev/*)` | `session-branches` |

Each pigeon tool is listed twice: under `Pigeon`, the claude.ai connector's name, and under `71fdea92-23ad-47d5-a156-53f536f8b55a`, the same connector's server id, which some sessions see instead. The format follows the Claude Code permissions docs: a tool is `mcp__<server>__<tool>`, the server segment literal. A server-wide `mcp__<server>__*` exists, but it would also allow every pigeon tool the policy does not name, so each tool is named. A Bash rule uses `*` to match any text.

`call` is never in the list: it reaches every pigeon tool, `deploy_token` and `source_token` included, so pre-approving it would go around the policy. Landing 1 generated it, with the names of the `pigeon-platform` server in `.mcp.json`, a duplicate connector that asks for sign-in; the third landing corrected both, with the person's passkey. The clean-up landing removed `.mcp.json` itself: its only entry was that duplicate.

The gate fails when `.claude/settings.json` differs from what the generator writes.

## The smoke suite

`tests/smoke.rs` holds the only tests. It runs as the `smoke` test target of `pigeon-capabilities`.

It has 12 scenarios:

1. policy decisions
2. git proxy push rules
3. land, from check to landed
4. deploy proxy allowlist
5. CI route and run, on a fake connector
6. models request
7. secrets write and check
8. expiry readings
9. mirror push
10. webhook HMAC and OIDC
11. console sign-in and a write
12. proxy and job tokens

Externals are stubbed. Git is a local repository. Answers from providers, Cloudflare and GitHub are recorded values.

The suite replaced 259 unit tests, about 5,400 lines.

`ci/runner/test.sh` runs these checks:

1. No `#[test]` outside `tests/smoke.rs`.
2. `fmt`.
3. `clippy -D warnings`, on the suite too.
4. The suite. It is built first, then timed. It fails if it runs past 60 seconds.
5. The settings check.
6. The platform's WASM check.

Locally the suite runs in about 7 to 10 seconds. RSA key generation dominates.

## Departures from the plan

- The mirror's planning became a pure function (`mirror::plan`, `mirror::rejected`), so the suite can drive it.
- A few items became public for the suite: `secrets::write_purpose`, `mirror::is_github`, `mirror::github_expiry`.
- Landing 1 still needed the passkey. The old Lander ran it, and it changed `.pigeon` and `ci`.
- The brief said a change is "approved by the person's delegate". This is implemented as the Lander recording the delegate's approval, with no wait.

## Gaps left for the person

The policy build left three gaps. The person closed them in the clean-up landings:

- Adding a scenario to `tests/smoke.rs` was "not in policy". The person added `smoke-suite-grows` (allow), with their passkey.
- A deploy of anything but a session branch was "not in policy". The person added `deploy-after-landing` (deny), with their passkey.
- The proxy could not tell a rewrite of a session branch from a push. Fixed in the clean-up landing, in code: see "The git proxy" above.

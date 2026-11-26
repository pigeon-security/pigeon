# Agents' git over MCP

Status: **built on `claude/agent-git-over-mcp`**, not landed. Codex's branches need the policy change below before they push or land.

## Goal

Claude Code and Codex push and land through their existing Pigeon MCP session, with no token, on one shared path scoped by which app is calling. The person's long-lived proxy credential stays only as break-glass.

## The three tiers

| Tier | What | For | Token in the transcript |
| --- | --- | --- | --- |
| `branch_push` | The agent sends a git bundle through the tool call; the platform writes it to a branch in its app's namespace | Every normal push (the default) | None |
| `session_credential` | A git-only proxy credential, one hour by default, four at most, pushing only the app's namespace | Pushes too large for a tool call | Yes: short-lived and narrow |
| The proxy credential (`pgn_px_`, issued with a passkey, saved as the environment's network secret) | Full git and `/cf/` scope | Break-glass only ([../proxies.md](../proxies.md)) | No: the network proxy adds it |

`source_token` and `deploy_token` stay break-glass. Agents call them only when the person asks.

Every path lands the same way: `land {branch, commit}`, then `land_status`.

## Which app writes which branches

The caller is a grant: a service user, usually an OAuth client, linked to a person. `delegates::app_of` says which app the grant is. An admin's override comes first (Access › People › Apps). The namespaces are a small table in code, `NAMESPACES` in `src/capabilities/src/source/push.rs`:

| App (`delegates::KNOWN`) | Namespace |
| --- | --- |
| `claude` (Claude, Claude Code, Claude Desktop) | `claude/` |
| `openai` (ChatGPT, Codex) | `codex/` |
| `cloudflare-portal` | `claude/` |
| anything else | none: refused |

- **The portal.** Claude Code's connector (`Pigeon_MCP`) reaches pigeon through the Cloudflare MCP Portal. pigeon sees the portal's grant acting for the person, not Claude (see [mcp-portal.md](mcp-portal.md)), and cannot tell the portal's clients apart. So the portal maps to `claude/`. Without that mapping, no Claude Code session could use `branch_push`.
- **Codex.** Codex should connect **directly** to `https://platform.pigeon.dev/mcp`. There it registers as an OAuth client named `Codex` with a loopback redirect, which `app_of` reads as OpenAI, so it gets `codex/`. A Codex that connects through the portal is seen as the portal and writes `claude/`.
- **Another app.** An admin can put a grant under one of the known apps with an override (`apps_cleanup` with `assign`). The grant then has that app's namespace. A new app gets a namespace only by a change to `NAMESPACES`, which is a landing.
- **Never `main`.** Every namespace ends in `/` and is never the protected branch. A branch outside the caller's namespace is refused before anything is read.
- **Who.** The grant must act for a person, the link must match, and that person must be `operator` or above on the home instance (`membership`).

## `branch_push`

```json
{ "branch": "codex/fix-x", "bundle": "<base64>", "force": false }
```

### The input: a git bundle

The agent makes the bundle and encodes it:

```sh
git fetch https://proxy.pigeon.dev/git/pigeon-security/pigeon.git main   # or the GitHub mirror
git bundle create /tmp/push.bundle FETCH_HEAD..HEAD
base64 -w0 /tmp/push.bundle
```

It passes the base64 text as `bundle`. A bundle was chosen over the two other formats considered:

- **A unified diff and a base commit** would make the platform build the commits: their author, committer, dates and message would be the platform's guess. Binary files, modes and renames would also need an apply step that can fail halfway.
- **A list of files** has the same problems, and its size grows with whole files, not with the change.
- **A bundle** is git's own format. The platform passes its pack to the repository's receive-pack as it is, so every commit keeps its id, author, message and trailers (`Co-Authored-By`, `Claude-Session`). Several commits, binaries, deletes and renames all come through. A thin pack stores each changed file as a change to its old version, so a bundle is usually smaller than the diff. The pack's SHA-1 trailer catches a bundle that was cut short or changed in transit.

The cost of a bundle is that the agent has to copy base64 through the tool call. A model writes base64 token by token, so in practice the limit is what the agent can emit, well below the byte limit. A large change goes through `session_credential`.

### Checks

Each check refuses with a clear reason, before anything is written:

1. **The app and the branch.** The branch must be under the caller's namespace and must be a valid ref name.
2. **The size.** The bundle is at most **8 MiB** decoded, checked before decoding. The header is at most 64 KiB, with 8 prerequisites at most. `branch_patch` answers at most 1 MiB.
3. **The format.** A v2 or v3 bundle, SHA-1 only. A filtered (partial) bundle is refused. All its refs must point at one commit: that commit is the tip. The pack's trailer must match.
4. **The base.** The bundle must name at least one prerequisite, so `git bundle create <file> main` (the whole history) is refused. Its history from the tip may reach only the prerequisites it names. Each prerequisite must be `main`, an ancestor of `main`, or an ancestor of the branch's tip. This is read from the repository (commits only, `filter tree:0`).
5. **Forward.** When the branch exists, the tip must descend from its current tip. Otherwise the push is refused unless `force: true`. A forced push is a rewrite, and only ever in the app's own namespace (check 1).
6. **The policy.** The push is asked of the policy as `push_branch`, or `rewrite_branch` when forced (rule `session-branches`). The decision is kept, as `policy_check`'s are.

The push itself is one ref update. The old tip, or none, is compare-and-swap, so a branch that moved in between is refused by Artifacts. A bundle already at the branch's tip pushes nothing.

### Answer

```json
{ "branch": "codex/fix-x", "commit": "<tip>", "old": "<previous tip or null>", "outcome": "created",
  "commits": 2, "base": ["<prerequisite>"], "app": "OpenAI",
  "next": { "tool": "land", "args": { "branch": "codex/fix-x", "commit": "<tip>" } } }
```

`outcome` is `created`, `pushed`, `rewritten` or `already`.

### Where it runs

Core (`pigeon-platform`), in the `source` capability. It writes the way the Lander does: a write token for the repository in Artifacts (`merge::Session`), minted for the call and revoked after. No new credential is involved. Session branches stay in Artifacts; the GitHub mirror copies `main` only.

### Audit

Each call writes one log line:

```json
{"audit":"branch_push","app":"openai","grant":"<service user>","person":"<person id>","branch":"codex/fix-x","old":null,"tip":"<commit>","outcome":"created"}
```

It names the person by id, never by name, because Worker logs are kept. The policy decision is also kept with the actor `<app> for <person>`.

## `session_credential`

```json
{ "ttl": 3600 }
```

The tool mints a proxy credential (`pgn_px_…`) with these limits:

- **Kind.** Its kind is `app_git` in `oauth_tokens`. It is held by the calling app's grant (its service user), and its `scope` is the app's namespace.
- **Lifetime.** 3600 s by default, from 60 s to 14400 s (4 hours). Anything else is refused, not cut short.
- **Git only.** The proxies see it as a git-only caller (`ProxyCaller.branch_prefix`, as an AI agent run's grant is). `/cf/` refuses it. MCP never accepts any `pgn_px_` token.
- **Fetch and push.** It fetches the repository. It pushes only branches under the namespace (`within_prefix`), and then only as the policy allows. `main`, tags, the namespace itself and other namespaces are refused, each named in `git push`'s output.
- **The person.** It is named for the person the grant is linked to. It stops working as soon as that link is gone.

### Seeing and revoking it

- **Seeing it.** It counts in the app's live tokens in Access › People › Apps (`GRANTS_SQL` counts every kind but `proxy`). It is not listed with the person's proxy credentials.
- **Disconnect.** Per-app Disconnect deletes it (`DISCONNECT_GRANT`).
- **Removing the person.** This deletes it (`REMOVE_USER` deletes the tokens of every app linked to them) and their links.
- **Delay.** The proxy Worker keeps a verified credential for 60 seconds per isolate, so a revoke takes up to a minute to reach it (as for any proxy credential, [../proxies.md](../proxies.md)).
- **Expiry.** Expired ones stop at once and are swept when the next one is minted.

It needs no passkey. The policy's catch-all `tools-run` allows it (see the policy section below).

### Using it

An MCP tool can be called only by the agent, not by a process git starts. So there is no credential helper that fetches the token itself. Two other things also rule out a classic helper: git asks a credential helper only after a 401, and the proxy answers 403 so that git never prompts. The proxy also takes the token only as `Authorization: Bearer`. The agent therefore calls the tool, keeps the token in a private file for the session, and passes it as a header:

```sh
# After session_credential: the token is in the answer, and so already in the transcript.
umask 077
printf '%s' '<token>' > "$HOME/.pigeon-git-token"

# A small helper for the session's pushes and fetches.
pgit() {
  git -c "http.https://proxy.pigeon.dev/.extraHeader=Authorization: Bearer $(cat "$HOME/.pigeon-git-token")" "$@"
}
pgit fetch https://proxy.pigeon.dev/git/pigeon-security/pigeon.git main
pgit push https://proxy.pigeon.dev/git/pigeon-security/pigeon.git HEAD:refs/heads/codex/fix-x

rm -f "$HOME/.pigeon-git-token"   # when done; it expires anyway
```

Then the agent calls `land {branch, commit}`.

**The token is in the transcript.** The tool's answer holds it, and transcripts are kept. That is why it lives an hour by default, four at most, reaches git alone and pushes only the app's own namespace. Do not save it anywhere lasting.

**A Claude Code environment that still holds the proxy credential.** Its network proxy adds its own `Authorization` header to every request to `proxy.pigeon.dev`, and that header may replace this one. After the person revokes the long-lived credential, they should also remove it from the environment's network secrets. Otherwise every request carries a revoked credential.

## Landing

- The Lander takes branches under `claude/` and `codex/` (`land::state::PREFIXES`). It refused anything but `claude/` before.
- The policy still decides each landing and its deploy (rule `land-and-deploy`). Changes to the policy or to `ci/` still wait for the person or their delegate.
- `land` is open to a person or to any agent linked to one (`may_land`), so the OpenAI app lands for its person as Claude does. Nothing in code blocked that.

## The policy change for Codex (the person's)

The rules `session-branches` and `land-and-deploy` name `@session/*`, which is `claude/`. Until the person adds `codex/*`, Codex's `branch_push`, `session_credential` pushes and `land` of a `codex/` branch are refused as "not in policy". The change:

```diff
--- a/.pigeon/policy.toml
+++ b/.pigeon/policy.toml
@@ -24,7 +24,7 @@
 [[rule]]
 id = "land-and-deploy"
 actions = ["land", "deploy"]
-targets = ["@session/*"]
+targets = ["@session/*", "codex/*"]
 decision = "allow"
 reason = "a session branch lands through the gate, and the deploy after it is automatic"
 
@@ -37,7 +37,7 @@
 [[rule]]
 id = "session-branches"
 actions = ["push_branch", "rewrite_branch", "delete_branch"]
-targets = ["@session/*"]
+targets = ["@session/*", "codex/*"]
 decision = "allow"
 reason = "session branches are the agents' own: push, rewrite or delete them"
```

The regenerated `.claude/settings.json` is the same with or without this change: it adds no action kind.

The smoke suite applies this change to the policy in memory and checks that `codex/*` pushes, rewrites, lands and deploys, while `main` stays refused (`agent_git_branch_push`, `land_takes_codex_branches`).

No change is needed for the tools themselves. `branch_push` and `session_credential` are not in `risky-tools-need-passkey`, so `tools-run` (`call_tool`, any target) allows them.

## Claude Code's settings

`.claude/settings.json` is generated from the policy (`cargo run -p pigeon-capabilities --example settings`). The generator now maps the policy's branch actions (`push_branch`, `rewrite_branch`, `delete_branch`) to `branch_push` and `session_credential` as well as `git push https://proxy.pigeon.dev/*`. So both tools run without a prompt. The regenerated file is a policy-guarded path: the landing waits for the person's passkey (`policy-is-yours`).

## Workers

- **`pigeon-platform` (core):** the tools, the Directory's `app_git` op and lookup, the Lander's prefixes and the MCP instructions.
- **`pigeon-proxy`:** the git proxy's namespace check and its refusal wording (`src/proxies`), and the shared `within_prefix` (`src/core`).

The Directory's lookup that the proxy asks (`verify`) runs in core, so `session_credential` works through the proxy Worker only once both are deployed.

## Code

| Area | Files |
| --- | --- |
| Bundles: decode, parse, checksum | `src/core/src/bundle.rs` |
| App → namespace, the checks, both tools | `src/capabilities/src/source/push.rs`, `src/capabilities/src/source.rs` |
| `app_git` tokens: SQL, mint, lifetime | `src/capabilities/src/proxy.rs`, `src/capabilities/src/users.rs` (`app_git.add`, `proxy.token_get`) |
| Namespace at the git proxy | `src/core/src/proxy_caller.rs` (`within_prefix`), `src/proxies/src/git_proxy.rs`, `src/proxies/src/cf_proxy.rs` |
| Lander | `src/capabilities/src/land/state.rs` (`PREFIXES`) |
| Settings generator | `src/capabilities/examples/settings.rs` |
| Tests | `tests/smoke/policy.rs` (`agent_git_branch_push`, `agent_git_session_credential`), `tests/smoke/land.rs` (`land_takes_codex_branches`) |

## Not verified

- **Artifacts and thin packs.** These checks have not run against Artifacts itself. The smoke suite stands in a local bare repository for receive-pack (`index-pack --fix-thin`). Real git pushes thin packs through the proxy into Artifacts today, so Artifacts is expected to take a bundle's pack the same way.
- **Request size.** How large a tool call the portal and the MCP clients pass is not known.

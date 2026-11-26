# Agents in this repository

These notes are for coding agents such as Codex. The source of truth is Cloudflare Artifacts; GitHub (`pigeon-security/pigeon`) is a read-only copy of `main`. You reach the platform through the Pigeon MCP server.

## Push

- Push your work to a branch under **`codex/`** (Claude uses `claude/`). Never push `main`; it moves only by a landing.
- Use the MCP tool **`branch_push`**. It needs no token:

  ```sh
  git bundle create /tmp/push.bundle origin/main..HEAD   # base: a commit on main
  base64 -w0 /tmp/push.bundle
  ```

  Call `branch_push {"branch": "codex/<name>", "bundle": "<base64>"}`. It writes your commits as they are and answers the new tip, `commit`.
- It is fast-forward only. Pass `"force": true` only to rewrite your own branch.
- For a push too large to pass through a tool call, call **`session_credential`**. It answers a one-hour git credential for `https://proxy.pigeon.dev/git/pigeon-security/pigeon.git` that pushes only `codex/…`. Send it as `Authorization: Bearer <token>` with `git -c http.extraHeader=...`. Never save it anywhere lasting.

## Land

- Call **`land {"branch": "codex/<name>", "commit": "<tip>"}`**, then follow it with **`land_status`** until it is `landed` or stops.
- A change to `.pigeon/policy.toml` or `.claude/settings.json` waits for the person's passkey. A change under `ci/` (or to what the gate runs) waits for the person's delegate. Say so to the person; do not work around it.
- If a landing is refused, read the reason. Never force-push to make a landing fit: the Lander rebases for you.

## Never

- Never call `deploy_token` or `source_token`, unless the person asks you for break-glass in so many words.
- Never edit `.pigeon/policy.toml`. Only the person changes it. Call `policy_check` when unsure; act on `allow`, and stop on `deny`.

## More

- [docs/plans/agent-git.md](docs/plans/agent-git.md): `branch_push`, `session_credential` and namespaces.
- [docs/proxies.md](docs/proxies.md): the git proxy, and break-glass.
- [docs/plans/policy.md](docs/plans/policy.md): the policy.
- The tests are the smoke suite only (`tests/smoke.rs`, `tests/smoke/`). Run the checks in `ci/runner/test.sh` before you land.

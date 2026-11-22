# Sandbox image v2 (2026-11-22)

Fixes the three sandbox bugs of the overnight run (`docs/plans/overnight-2026-11-22.md`, "What
broke", items 1–3) in `pigeon-sandboxd`, and closes a credential leak found while fixing them.

## Why executors failed with "destination path '/work' already exists"

v1 clones with `--filter=blob:none` and hands the run's git grant to the clone alone. An executor
based on its planner's branch (or a run resuming its own pushed branch) then checks out a tree
whose blobs were not fetched; git fetches them lazily, without the grant, and gets 403. v1 then
removed only `.git`, leaving the checkout's files in `/work`, so every retry failed on the clone,
and the Worker kept only the last error. Reproduced locally against an HTTP git server that wants
a bearer header. The Worker works around it with v1 (landed first: `PREPARE_SETUP` in
`src/ai-agents/src/sandbox.rs` empties `/work` and makes the clone full from a hook).

## What v2 changes

1. A `fetch` op (and the agent tool `fetch`): fetches a branch or commit as `origin/<branch>`
   with the grant, which stays inside sandboxd. Planners read executors' branches with it.
2. `prepare` empties `/work` when it holds no checkout, clones in full (no partial clone), and
   empties `/work` again when a try fails.
3. `exec` runs `bash -c`, not `bash -lc`, so PATH keeps cargo and node.
4. Every git command that carries the grant names the prepared URL itself (never `origin`), sends
   the header only to that URL, runs with hooks off, TLS verified, no credential helper, no global
   or system config, and refuses while the checkout's config sets `http.*`, `url.*`, `include*`,
   `credential*` or a command-running `core.*` key. In v1 a run could read its grant from a
   `.git/hooks/pre-push` hook (`commit_push` pushes with the grant in the environment), or send
   it elsewhere by changing `remote.origin.url`.

## Rebuild and pin (the person)

This source lands first, alone (it changes nothing live until an image is built from it). From
a clone of main at or after that landing, at the repository's root:

    docker build --platform linux/amd64 -f ci/sandbox/Dockerfile -t pigeon-ai-agent-sandbox:v2 .
    cd src/platform && npm ci && npx wrangler containers push pigeon-ai-agent-sandbox:v2

Take the pushed linux/amd64 manifest digest (`wrangler containers images list`, or the push's
output) and pin it in `src/ai-agents-worker/wrangler.toml`:

    image = "registry.cloudflare.com/4f94676cb2cbe46fd985b089ca97ea27/pigeon-ai-agent-sandbox@sha256:<digest>"

Land that pin together with the Worker side (the `fetch` agent tool, on
`claude/sandboxd-v2-pin`), which needs v2: v1 answers 404 to `fetch`.
After it is deployed, a follow-up drops the Worker's v1 workarounds: `RUN_PATH`,
`PREPARE_SETUP` and `PREPARE_TEARDOWN` in `src/ai-agents/src/sandbox.rs` (harmless with v2).

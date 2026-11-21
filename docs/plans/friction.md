# Friction: what got in the way, removed

Status: planned. Branch `claude/friction`, from `origin/claude/gracious-ride-526fue`. One hand-back
to the parent session (`session_015XFA1jgveJDEE8NJjTaHKg`), which merges and deploys it. It is
meant to be the last hand-merged deploy: after it, work lands through `land`.

This round removes the friction found while building the secrets page, the AI Gateway, the
proxies, the CI gateway and the console, and adds async model jobs. Each item below has its
design, interfaces, tests, what only a live check can show, and what the person must do. Where
the design turned out wrong or impossible, it says so (see [Departures](#departures)).

## Order

The CI gateway's phase 3 (Buildkite) is being built on `claude/ci-gateway`, in
`src/capabilities/src/ci*`. Until it lands, nothing here edits those files. So the order is the
person's, with every edit of a `ci*` file moved into one last commit made after phase 3 is merged:

| # | Commit | Touches `ci*`? |
| --- | --- | --- |
| 0 | this plan | no |
| 1 | housekeeping: clippy clean, `docs/plans/README.md` | no |
| 2 | `call {tool, args}` | no |
| 3 | `models_logs`, `containers_images` | no |
| 4 | wrangler pinned; the allowlist check against a stand-in | no |
| 5 | async model jobs, with the shared `notify` module | no (CI moves onto it in 7) |
| 6 | the land flow: `land`, `land_status`, the `Lander` object, approvals, the cf_proxy grant, `test` and `deploy` jobs, image v3 | no |
| 7 | after phase 3: CI uses the shared `notify`; a job may take its token (`token = true`), which `deploy` needs; job-token docs | yes |

Each commit builds, passes `cargo fmt --check`, `cargo clippy --workspace -- -D warnings`,
`cargo test --workspace --exclude pigeon-platform --target x86_64-unknown-linux-gnu` and
`cargo check -p pigeon-platform`, and gets a line-by-line security review before the next.

## 1. Housekeeping

- The six clippy warnings in the library code (`core/git.rs`, `core/queues.rs`,
  `capabilities/architecture.rs` ×3, `capabilities/mirror.rs`) and the two in test code
  (`core/pack.rs`, `capabilities/git_proxy.rs`, `drop` of a reference) are fixed, so the `test`
  job can deny warnings. No behaviour changes.
- `docs/plans/README.md` indexes every plan with its status and says that the plans in this
  directory are the record: a Claude Docs design is a working draft, and where it and a plan
  differ, the plan (and the code) win.

## 2. `call {tool, args}`

A session's MCP tool list is fixed when the session starts, so a tool deployed later cannot be
called from it. `call` runs any hosted capability's tool by name.

- Lives in a new capability, `dispatch`, so the platform's tool list carries it.
- `pigeon_capabilities::dispatch(cx, name, args)` is the one way a tool is run: the platform's
  `/mcp` handler uses it too, so `call` and a direct call find the tool, build the context and
  report errors the same way. An unknown tool is `unknown tool \`x\``, the text `/mcp` answers.
- `call` refuses itself (`call` calling `call`, at any depth: `name == "call"` is refused before
  dispatch), refuses `args` that is not an object (as `/mcp` would default it to `{}` only when
  absent), and adds nothing to the caller: the same `Context`, so the called tool sees the same
  caller and checks it the same way.
- Tests: dispatch finds the owner and passes the args; unknown tool error text equals `/mcp`'s;
  `call` of `call` refused; non-object args refused; an owner's error comes back unchanged.
  Pure parts are tested with fake capabilities (the `Capability` trait, no `worker` I/O).

## 3. Read-only diagnostics

A live check should never need `deploy_token` and raw API calls.

- **`models_logs {hours?, limit?, provider?, model?, caller?, task?, failed?}`**: the gateway's
  most recent log entries, newest first, at most 200: id, time, provider, model, status, success,
  cached, tokens in and out, cost, duration, the tags (`metadata`) and `collect_log_payload`.
  Never a body: the `Log` type has no body field, and the listing endpoint is the only one read.
- **`containers_images`**: what the Containers registry holds and what runs. The container
  applications on the account (`GET containers/applications`, `Workers Containers Read`): name,
  instances and image; and the registry's repositories and tags (pull-only registry credentials
  living 5 minutes from `POST containers/registries/registry.cloudflare.com/credentials`, then
  `GET https://registry.cloudflare.com/v2/_catalog?tags=true`, as wrangler 4.166.0's
  `containers images list` does). The credential never leaves the Worker. A new capability
  `containers` (not under `ci*`).
- **A change to the cf_proxy allowlist deploys through `land`**: see 6, "Allowlist changes".
- Live only: that the logs endpoint returns `metadata` and `collect_log_payload` as recorded
  (they did for `models_usage`); which permission the registry credential call takes (the tool
  asks for `Workers Containers Write`, `pull` only; if refused, the tool still answers the
  applications and says the registry could not be read).

## 4. The deploy allowlist matches what runs

- **Pinned wrangler.** `src/platform/package.json` pins `wrangler` to exactly `4.166.0`, with
  `package-lock.json`. Every deploy runs `npm ci` then `npx wrangler deploy` in `src/platform`.
  The MCP instructions, `docs/proxies.md`, the `deploy` job and the land flow say so.
- **The allowlist check.** `ci/deploy/standin.mjs` is a stand-in for the Cloudflare API: it
  answers each call wrangler makes with a plausible success (the Worker exists, its version
  ids, a ready image preparation, one container namespace per container class, an application
  to create) and writes each call as one line, `METHOD /path?query`, to a file.
  `ci/deploy/allowlist.sh` runs the pinned wrangler's real `deploy` against it
  (`CLOUDFLARE_API_BASE_URL=http://127.0.0.1:<port>/client/v4`, a dummy token, the build already
  made) and then `cargo run --example allowlist` (capabilities crate), which reads the file and
  fails naming every call `cf_proxy::allowed` refuses. The pure part,
  `cf_proxy::unlisted(calls, scope) -> Vec<String>`, is unit-tested.
  - The `allowlist` job in `.pigeon/ci.toml` runs it, and the `deploy` job runs it before the
    real deploy, so a wrangler that would make a call the proxy refuses stops before anything is
    changed.
- Live only: nothing; the check runs in a container and here.

## 5. Async model jobs

`models_run` is one blocking request, cut off at about 60 seconds. Add:

| Tool | Does |
| --- | --- |
| `models_submit {provider?, model?, system?, prompt\|messages, max_tokens?, task?, notify?, timeout_min?}` | queues a job; answers its id, state and (with `notify`) the signing key once |
| `models_status {id}` | state, times, tokens so far, the error if any |
| `models_result {id}` | the reply (text, stop reason, tokens), once finished |

- **Where it runs.** Durable Object class `ModelJobs` (binding `MODEL_JOBS`, SQLite). Object
  `index` keeps every job's row, enforces the cap and expiry, and sends notifies from its alarm.
  Each job streams in its own object, named `job:{id}`, from that object's alarm: it sends the
  request through the AI Gateway with `stream: true`, the same tags (`caller`, `caller_name`,
  `for`, `for_name`, `task`) and `cf-aig-collect-log-payload: false`, reads the server-sent
  events as they come (`message_start`, `content_block_delta`, `message_delta`, `error`), writes
  progress (tokens so far) to `index` every 15 seconds, and the result at the end.
- **Cap and timeout.** At most 4 jobs run at once; more wait `queued`, and a job is started as
  a slot frees. At most 50 jobs queued or running per platform; more are refused. Each job has a
  timeout (`timeout_min`, 1 to 14, 10 by default) enforced by the job's object, with `index`'s
  alarm as the backstop (a running job not finished 2 minutes past its deadline is `lost`).
- **Results.** Kept in `index` until read, then for 10 more minutes (so a retried read works),
  and never past 24 hours from submission; then the row is deleted. Only the submitter (the same
  user id) or the person they act for can read a job.
- **Notify.** Optional; the same scheme as CI (`Pigeon-Signature: t=<secs>,v1=<hex HMAC>`,
  per-job key shown once, `https` to a DNS name, never the platform). The body is
  `{job, state, provider, model, task, tokens_in, tokens_out, at}`, never the text. CI's
  `notify.rs` moves to `src/capabilities/src/notify.rs`, shared; CI's file re-exports it in
  commit 7.
- **States** (`models/jobs.rs`, pure): `queued → running → succeeded | failed | timed_out |
  lost`; `queued → failed` (could not start). Moves only forward; a terminal state never
  changes; a duplicate report is ignored.
- Tests: transitions, the cap (`next_to_start`), expiry (`expires_at` before and after a read),
  the timeout bounds, the SSE parser (text deltas joined, usage read, an `error` event, events
  split across chunks), the notify body (no text), and notify signing in its new home.
- Live only: the stream through the gateway (the gateway passes SSE through), the alarm's wall
  time, the progress writes.

## 6. The land flow

`land {branch, commit?}` takes a session's branch to `main` and to the deployed platform with no
person in between, unless the person asks for one.

### Steps

```
checking ─► testing ─► [awaiting_approval] ─► merging ─► deploying ─► landed
    │          │               │                 │            │
    ▼          ▼               ▼                 ▼            ▼
 refused   test_failed     expired          merge_refused  deploy_failed
```

Any live state can also become `error` (the gateway failed) or `cancelled` (`land_cancel`).

1. **checking.** `branch` must be under `refs/heads/claude/` (given as `claude/x` or the full
   ref). Its tip is read from the repository; with `commit`, the tip must equal it. The commit
   must be a fast-forward of `main` (main's tip is in its history, or is it). Only one landing
   is live at a time. Whether it needs approval is decided here (below).
2. **testing.** The gateway submits CI job `test` at the commit (through the same dispatch as
   `call`, tagged with the caller and `task: land <id>`), and follows it.
3. **awaiting_approval**, only when approval is needed: a person approves on the console with a
   passkey bound to this landing and commit. It expires after 24 hours.
4. **merging.** The platform moves `main` to the commit through its own git path: a write token
   for the repository, the refs read again, the commit checked again to be a fast-forward of
   the current tip (a fetch of commits only, walked as the git proxy walks a push), then a
   receive-pack update `old → new` with an empty pack; Artifacts refuses it if `main` moved in
   between. The push goes to Artifacts, so the mirror and source events follow as for any push.
5. **deploying.** The gateway submits CI job `deploy` at the commit and registers that job as
   this landing's deploy. The job (in a Container, base image v3) checks out the commit, builds
   with `worker-build`, runs the allowlist check against the stand-in, then `npm ci` and
   `npx wrangler deploy` in `src/platform` with `CLOUDFLARE_API_BASE_URL` on the proxy host and
   its job token as `CLOUDFLARE_API_TOKEN`.
6. **landed.** The result is recorded as a git note on the commit under `refs/notes/land`:
   `landing <id> state=<s> by=<caller> test=<job> deploy=<job> approved_by=<person|-> at=<iso>`.

`land_status {id?}` answers a landing (the latest when no id): its state, each step with its time
and detail, the CI jobs with their states and URLs, the approval, and whether the note was
pushed. `land_cancel {id}` stops a landing before `merging` (it cancels its CI job).

### The deploy grant: job token to `/cf/`

The `deploy` job reaches the Cloudflare API proxy with its own CI job token, never a stored one:

- `cf_proxy` takes `Authorization: Bearer pgn_ci_…` as well as a proxy credential. For one, it
  looks up the job's grant (`ci::job_grant`), then asks the `Lander` whether that job is the
  deploy job of the live landing, in state `deploying`, for the grant's commit. Only then does
  the call go on, with the same allowlist and per-call tokens as any other; the audit line names
  `land <id> deploy`.
- So the grant lives as long as the job (its deadline), covers one commit, one Worker and only
  the allowlisted calls, and stops when the landing leaves `deploying`.
- A job token passed to the job's script: today the runner strips it. Commit 7 adds
  `token = true` to a job in `ci.toml` (only `deploy` sets it); the runner then leaves
  `PIGEON_JOB_TOKEN` in the script's environment. The gate is the `Lander`'s check above, not the
  flag: a test job given its token could still only fetch its own commit and report its own
  result, which its script decides anyway.

### Allowlist changes

The live proxy enforces the allowlist of the deployed platform, not the commit's. A commit that
adds an allowlist entry for a call wrangler makes **after** the upload (all of today's
container, settings and subdomain calls) deploys through `land`: the first `wrangler deploy`
uploads the new Worker, then is refused at the new call; `deploy.sh` waits 30 seconds and
deploys again, now through the new allowlist. A call made **before** the upload (wrangler's
lookups) cannot be added this way; `deploy.sh` reports that the running proxy refused a call
made before the upload, and that one deploy has to be made outside the flow. The dry run against
the stand-in uses the commit's own allowlist, so it passes in both cases.

### Security model

- **Who may call `land`.** An MCP caller with a person behind it: a person, or a service user
  linked to one (`acting_for`). Every MCP caller can already mint a deploy token
  (`deploy_token`), and every proxy-credential holder can already push `main` forward and run
  `wrangler deploy` through `/cf/`; `land` adds no power to either, and puts tests, a record and
  an optional approval in front of what they can already do. (Retiring `deploy_token` once
  `land` works is a separate decision for the person.)
- **What stops a bad commit from deploying.** The commit must be on a `claude/` branch and a
  fast-forward of `main`; the `test` job must pass on exactly that commit; the `deploy` job builds
  exactly that commit (its job token fetches nothing else); the proxy allows only the allowlisted
  calls on `pigeon-platform`; the result is noted on the commit. Tests catch mistakes, not
  malice: a commit defines its own tests. So a commit that changes what the gate itself runs
  (`.pigeon/`, `ci/`, `src/platform/package.json` or `package-lock.json`, compared by tree and
  blob id against `main`) always needs a person's approval, whatever the setting.
- **What uploading the platform means.** The platform Worker binds the parent token, so whoever
  can upload it has account-level power. The person accepted this for proxy credentials; `land`
  is the same power behind more checks.
- **Approval (off by default).** A console setting, *Require approval to land*, changed by a
  person with a passkey. When on, every landing waits after its tests pass for a person to
  approve it on the console (Settings → Landing) with a passkey whose challenge names the
  landing and its commit. Approval comes before `main` moves, so `main` and the deployed Worker
  stay one.
- **Nothing secret in transcripts.** No tool answers a token; the job token stays in the
  container; the registry and API tokens are minted per call and revoked.

### Interfaces

```rust
// src/capabilities/src/land/state.rs, pure
pub enum State { Checking, Testing, AwaitingApproval, Merging, Deploying, Landed,
                 Refused, TestFailed, Expired, MergeRefused, DeployFailed, Cancelled, Error }
pub fn terminal(s: State) -> bool;
pub fn allowed(from: State, to: State) -> bool;
pub enum Seen { JobFinished { passed: bool }, JobGone(String), Approved, ApprovalExpired,
                Merged, MergeRefused(String), Cancel }
pub fn next(state: State, needs_approval: bool, seen: &Seen) -> Option<State>;
pub fn branch_ref(given: &str) -> Result<String, String>;        // claude/x → refs/heads/claude/x
pub fn guarded(paths_changed: &[&str]) -> bool;                    // .pigeon/, ci/, src/platform/package*.json
// src/capabilities/src/land.rs: the capability (tools, console section) and the Lander logic
// src/platform/src/lib.rs: `Lander`, the Durable Object class (binding LANDER, object "lander")
// git_proxy: pub async fn fast_forward(account, namespace, repo, branch, new) -> Result<Moved>
```

Tests: `branch_ref` (prefix, traversal, bad names), every transition and the refused ones,
approval only when needed or guarded, the grant check (job, state and commit must all match),
the guarded paths, the fast-forward walk against a real git history (as the git proxy's tests
do), the note line.

### The `test` and `deploy` jobs

```toml
[jobs.test]
run = "ci/runner/test.sh"     # fmt --check, clippy -D warnings, test (native), check platform
timeout_min = 60
cpu = 4
memory_gb = 12
disk_gb = 20

[jobs.deploy]
run = "ci/deploy/deploy.sh"   # build, allowlist dry run, npm ci, wrangler deploy (once more on a post-upload refusal)
timeout_min = 60
cpu = 4
memory_gb = 12
disk_gb = 20
token = true                  # commit 7

[jobs.allowlist]
run = "ci/deploy/allowlist.sh"
timeout_min = 45
cpu = 4
memory_gb = 12
disk_gb = 20
```

### Base image v3

`ci/image/Dockerfile` (new: the image was built by hand so far). On top of v2 (debian
bookworm-slim; git, curl, ca-certificates, coreutils, build-essential, pkg-config; Rust stable
with `wasm32-unknown-unknown`, clippy and rustfmt; the runner as
`/usr/local/bin/pigeon-ci-runner`):

- Node.js 22 LTS (22.12 or later, which wrangler 4.166.0 needs), from the official tarball,
  checksum-checked;
- `worker-build` 0.9.0 (`cargo install --locked`), the version `wrangler.toml`'s build command
  names, so the build does not compile it each time;
- nothing else: wrangler comes from `src/platform/package-lock.json` by `npm ci` in the job, so
  the commit pins it, not the image.

The person builds and pushes it (`pigeon-ci-base`, a new digest) and the digest goes into
`[containers.images.base]` in `wrangler.toml`; that change itself then lands through `land`
(see "Allowlist changes": the image preparation is a post-upload call).

### Live only

The fast-forward push to Artifacts with an empty pack; the job token's way through `/cf/`; the
deploy from a Container (time and memory of a release build on standard-4); the note push to
`refs/notes/land`; the console approval.

## 7. After phase 3 (touches `ci*`)

- `ci/notify.rs` re-exports the shared `notify` module.
- `ci/config.rs`: a job's `token` (bool, default false); `ci.rs` `Run.pass_token`;
  `ci/containers.rs` sets `PIGEON_PASS_TOKEN=1`; `ci/runner/entrypoint.sh` keeps the token in
  the script's environment only then.
- `ci/token.rs` docs: a job token reaches `/cf/` only through the landing's grant.

## What the person must do

1. Build and push base image v3 from `ci/image/Dockerfile`, and give the digest (it goes into
   `wrangler.toml` through a `land`).
2. Say whether to require approval to land (off by default; the console's Settings → Landing).
3. Optionally, decide whether `deploy_token` stays once `land` has worked.

## Departures

Anything that proved wrong or impossible is written here and said in the hand-back.

- **Async jobs have a ceiling, of 14 minutes, not none.** A Durable Object request lives as long
  as its client is connected, but nothing stays connected once `models_submit` has answered; the
  job streams from an alarm, and an alarm handler has at most 15 minutes of wall time. So a
  job's `timeout_min` is at most 14. That is 14 times today's ceiling and covers a 64,000-token
  answer from the models served today; anything longer needs a runner without that limit (a
  Container), which this round does not build.

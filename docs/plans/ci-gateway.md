# The CI gateway

Status: phases 1 and 2 deployed; phase 3 built, awaiting the person's Buildkite setup and deploy. Phases are handed back one at a time; each is merged and
deployed by the parent session before the next starts.

Design: [CI gateway design](https://claude.ai/artifact/M5zwjixxfKwJn9PYUf9tcj). This plan turns
it into steps. Where the plan departs from the design, it says so under
[Corrections to the design](#corrections-to-the-design).

## Who does what

- **Planning session (this one, branch `claude/ci-gateway`):** writes the plan, briefs the
  agents, applies their output, builds, tests and reviews every line, rebases on
  `origin/claude/gracious-ride-526fue` and pushes its own branch, hands each phase back. It never
  deploys and never changes a cloud environment, a secret or a Cloudflare resource.
- **Agents:** Claude Platform models through the `models_run` tool (one request, one answer, no
  tools; about 60 seconds, so about 3,000 output tokens). Each brief carries the goal, the exact
  interface to implement, the code style (the module-doc and comment voice of `proxy.rs`), and
  asks for one whole file. No brief carries a credential. Tasks are tagged
  `ci-p<phase>-<step>`. `claude-opus-5-5` for the state machine, routing, job tokens, signing,
  webhook verification and the OIDC exchange; `claude-sonnet-5-5` for routine pieces. Three
  failed rounds on one step stop the step and are reported.
- **Parent session (`session_015XFA1jgveJDEE8NJjTaHKg`):** merges, deploys, runs the live
  checks.
- **The person:** account-side setup (the queue, the Buildkite organization and pipeline, the
  secrets, the GitHub webhook, the base image), listed per phase below.

## Shape

```
src/core/src/
  gitobj.rs           git objects (blob, tree, commit), their ids, a pack of them (phase 1)
  sign.rs             HMAC-SHA256 and constant-time comparison (phase 1)
  containers.rs       start, watch and stop a job container (phase 2)
  buildkite.rs        builds, queues, webhook check (phase 3)
  github_actions.rs   dispatch, runs, webhook signature (phase 4)
  oidc.rs             RS256 JWT verification against a JWKS (phase 3)
src/capabilities/src/
  ci.rs               the capability: tools, routes, the Connector trait, settings
  ci/config.rs        .pigeon/ci.toml
  ci/class.rs         needs → runner class; what a connector offers and whether it covers
  ci/route.rs         match, availability, rank, queue fallback, retry: pure
  ci/state.rs         job states and the transitions between them: pure
  ci/token.rs         job tokens and their grants
  ci/notify.rs        the signed POST to `notify`, and which URLs it may go to
  ci/notes.rs         the git note on a commit, and the notes commit
  ci/scheduler.rs     the scheduler Durable Object's logic: SQLite, the alarm loop, fan-out
  ci/fake.rs          a fake connector, for tests (cfg(test))
src/platform/src/
  lib.rs              exports `CiScheduler` (the Durable Object class wrapping ci::Scheduler)
  settings.rs         the connectors, their order, the repositories CI may run
  wrangler.toml       CI_SCHEDULER binding + migration, CI_EVENTS queue producer
```

The scheduler's class has to be defined in the platform crate: the connectors come from
`settings.rs`, which the capabilities crate cannot see. `CiScheduler` is a thin
`#[durable_object]` that builds `ci::Scheduler` with `settings::ci()` and forwards `fetch` and
`alarm` to it.

## Interfaces

### `.pigeon/ci.toml` (`ci/config.rs`)

```rust
pub struct Config { pub jobs: BTreeMap<String, Job> }
pub struct Job { pub run: String, pub timeout_min: u32, pub needs: Needs }
pub struct Needs {
    pub os: Os,            // linux (default); macos, windows parse but nothing offers them yet
    pub arch: Arch,        // amd64 (default), arm64
    pub cpu: u32,          // 2
    pub memory_gb: u32,    // 4
    pub disk_gb: u32,      // 10
    pub docker: bool,      // false
    pub gpu: bool,         // false
    pub network: Network,  // egress (default) | none
}
pub fn parse(text: &str) -> Result<Config, String>;
```

Rules: a job name is 1 to 64 of `[a-z0-9_-]`; `run` is a relative path inside the repository
(no leading `/`, no `..` or `.` segment, no backslash, no control characters, at most 256
bytes); `timeout_min` defaults to 30 and is 1 to 360; `cpu` 1 to 64, `memory_gb` 1 to 256,
`disk_gb` 1 to 2,000; unknown keys are an error, so a typo never silently becomes a default.
The `toml` crate parses; serde does the shape.

### Runner classes (`ci/class.rs`)

A class is a name and a shape: `{os}-{arch}-{size}[-docker][-gpu]`, such as
`linux-amd64-small` or `linux-amd64-medium-docker`. Sizes, smallest that fits wins:

| Size | cpu ≤ | memory_gb ≤ |
| --- | --- | --- |
| small | 2 | 8 |
| medium | 4 | 16 |
| large | 8 | 32 |
| xlarge | 16 | 64 |

Bigger than xlarge has no class: the job is refused with that reason.

```rust
pub struct Class { pub os: Os, pub arch: Arch, pub size: Size, pub docker: bool, pub gpu: bool }
pub fn class_for(needs: &Needs) -> Result<Class, String>;
/// One class a connector can run, with its real limits.
pub struct Offered { pub class: Class, pub cpu: u32, pub memory_gb: u32, pub disk_gb: u32,
                     pub max_timeout_min: u32, pub network_none: bool }
/// Whether `offered` can run a job with `needs` and `timeout_min`; the reason when not.
pub fn covers(offered: &Offered, needs: &Needs, timeout_min: u32) -> Result<(), String>;
/// The smallest of `offers` that covers, or why none does.
pub fn pick<'a>(offers: &'a [Offered], needs: &Needs, timeout_min: u32) -> Result<&'a Offered, String>;
```

A connector may run a job on a larger class of the same OS, arch, docker and gpu (a small job
on a medium runner), never a smaller one. `covers` checks the real limits too: cpu, memory,
disk, timeout, and `network = "none"` only where the connector can enforce it.

### Routing (`ci/route.rs`), pure

```rust
pub struct Candidate<'a> { pub name: &'a str, pub rank: u32, pub offers: &'a [Offered], pub availability: Availability }
pub struct Availability { pub configured: bool, pub healthy: bool, pub free: Option<u32>, pub queued: u32 }
pub struct Ask<'a> { pub needs: &'a Needs, pub timeout_min: u32, pub pin: Option<&'a str>,
                     pub exclude: &'a [String] }
pub struct Routed { pub ranked: Vec<Pick>, pub ruled_out: Vec<(String, String)> }
pub struct Pick { pub connector: String, pub class: Class, pub ready: bool }
pub fn route(ask: &Ask, candidates: &[Candidate]) -> Result<Routed, Vec<(String, String)>>;
pub fn fallback_due(queued_at_ms: i64, now_ms: i64, wait_limit_ms: i64) -> bool;
```

- **Match:** a candidate is kept when `pick` finds an offered class. When none is kept, `Err`
  carries the reason per connector, and the job is refused at once.
- **Availability:** unconfigured or unhealthy candidates move to `ruled_out` with the reason.
  Full ones (`free == Some(0)`) stay in `ranked` with `ready: false`: the job may queue for
  them.
- **Rank:** a pinned connector is the only one ranked (a pin that does not match or is
  unavailable is a refusal naming why). Otherwise the operator's order from settings (each
  candidate has a `rank`; equal ranks tie), ties to the shorter `queued`. Room does not reorder:
  a job waits for its first-ranked connector. Cost and limits are later sort keys here.
- **Exclude:** connectors that already failed this job with an infrastructure error.
- **Queue fallback:** a queued job is dispatched to the first ready pick; when no pick is ready
  it stays on the first one; once `fallback_due` it goes to any ready pick.
- **Retry:** see the state machine.

`ci_route` answers `route` without dispatching.

### Job states (`ci/state.rs`), pure

```
queued ─► dispatched ─► running ─► succeeded | failed | timed_out
   │           │            │
   └───────────┴────────────┴─► cancelled | error
```

```rust
pub enum State { Queued, Dispatched, Running, Succeeded, Failed, Cancelled, TimedOut, Error }
pub fn terminal(s: State) -> bool;
pub fn allowed(from: State, to: State) -> bool;
pub enum Report { Started, Finished { passed: bool }, Infra(String), Gone }  // what a connector saw
pub enum Next { Move(State), Retry { exclude: String }, Ignore }
pub fn on_report(state: State, attempt: u32, report: &Report) -> Next;
pub fn on_deadline(state: State, attempt: u32, now_ms: i64, deadline_ms: i64) -> Option<Next>;
```

- Moves only forward; a terminal state never changes; a duplicate or late report is ignored.
- `Finished { passed: false }` is `failed`: a result, never retried.
- `Infra` (dispatch refused, runner lost, container exited without a result) on attempt 1 is a
  retry on another matching connector, excluding this one; on attempt 2 it is `error`.
- Deadline: `timeout_min` after dispatch plus a 10-minute allowance for queueing at the
  connector. Past it the connector's `status` is polled once more; with no final answer the job
  is cancelled at the connector and becomes `timed_out`.

### Job tokens (`ci/token.rs`)

- A job token is `pgn_ci_` + 32 random bytes (base64url), minted when the job is dispatched,
  kept only as its SHA-256 hash in the scheduler with its grant:
  `Grant { job: String, repo: String, commit: String, expires_at: i64 }`.
- It expires at the job's deadline and is dropped from the scheduler as the job ends, so a
  token outlives neither.
- `fn allows(grant, now, repo, commit) -> bool` and `fn reaches(path) -> bool` are pure and
  tested: a job token reaches only `/ci/runner/{its job}` and, once the git proxy accepts it,
  `/git/{its repo}.git/` upload-pack (fetch) routes. Never receive-pack, never `/cf/`, never
  `/mcp`.
- `ci::job_grant(env, token) -> Result<Option<Grant>>` is the interface the git proxy calls to
  accept a job token. **Only checkable once the proxy is live:** the proxy must call it, allow
  only `info/refs?service=git-upload-pack` and `git-upload-pack`, and refuse a `want` for any
  object other than the grant's commit. That last check is the proxy's, as it already parses
  pkt-lines; the hand-back says so.

### Notify (`ci/notify.rs`, core `sign.rs`)

- `notify` must be `https://`, with a DNS hostname (no IP literal, no `localhost`, no port),
  not the platform's own host, at most 2,000 bytes. Checked at submit.
- Each job gets its own signing key, 32 random bytes, returned once by `ci_submit` and kept in
  the scheduler to sign that job's posts.
- Each state change is POSTed as JSON
  `{job, repo, commit, name, state, attempt, connector, detail, url, at}` with
  `Pigeon-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(key, "<t>.<body>")>`. A failed post
  is retried by the alarm three times, then dropped and logged (job id and status, never the
  key).

### The commit note (`ci/notes.rs`, core `gitobj.rs`)

- One note per commit under `refs/notes/ci`, its text one line per final job:
  `<job> <state> connector=<c> attempt=<n> exit=<code> duration=<ms>ms url=<u>`, sorted by job
  name. `ci_status` also has the exit code and the last 4 KiB of the script's output, with
  token-shaped strings redacted (`ci/output.rs`).
- The scheduler keeps every note it has written (`notes` table). To write one it mints a
  write git token for the repository, reads `refs/notes/ci`'s tip (`git::refs`), builds a blob
  per note it keeps, a flat tree of them (`<40-hex commit>` → blob, sorted), and a commit whose
  parent is the tip, packs them all (undeltified; zlib as stored blocks, so no compressor is
  needed) and pushes with `git::push`, `old` = the tip. Sending every blob each time means a
  push never depends on an earlier one having landed; it grows with the number of commits
  noted, and a fan-out tree is the fix when that matters. A rejected push (the tip moved) is
  retried by the alarm with the new tip. The git token lives 5 minutes and is revoked after.
- The notes ref belongs to the gateway: the tree is rebuilt from what the scheduler kept, so a
  note written by someone else would be dropped. Recorded, not worked around.
- `ci_commit { repo, commit }` answers from the scheduler's records (the same text), so it
  works whether or not the push has landed.

### Connectors (`ci.rs`)

```rust
pub trait Connector {
    fn name(&self) -> &'static str;                 // "containers", "buildkite", "github_actions"
    fn offers(&self) -> Vec<Offered>;
    fn secrets(&self) -> Vec<Slot>;                 // declared on the secrets page
    fn available<'a>(&'a self, cx: &'a Cx<'a>) -> Pending<'a, Availability>;
    fn dispatch<'a>(&'a self, cx: &'a Cx<'a>, run: &'a Run) -> Pending<'a, Dispatched>;
    fn status<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, Option<Report>>;
    fn cancel<'a>(&'a self, cx: &'a Cx<'a>, handle: &'a str) -> Pending<'a, ()>;
    /// Verifies a delivery to /ci/hooks/{name} before anything in it is read; `None` when it
    /// is verified but not about a job.
    fn webhook<'a>(&'a self, cx: &'a Cx<'a>, hook: &'a Hook) -> Pending<'a, Option<Seen>>;
}
pub struct Run { pub job: String, pub attempt: u32, pub repo: String, pub commit: String,
                 pub name: String, pub script: String, pub class: Class, pub timeout_min: u32,
                 pub token: Option<String>, pub runner_url: String, pub git_url: String }  // git_url on proxy.pigeon.dev
pub struct Dispatched { pub handle: String, pub url: Option<String> }
pub struct Hook { pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub struct Seen { pub handle: String, pub report: Report, pub url: Option<String> }
pub struct Cx<'a> { pub env: &'a Env, pub host: &'a str }
```

`Run.token` is filled only for connectors that take it in the runner's environment
(Containers); Buildkite and GitHub Actions runners exchange their OIDC token at `/ci/token`
instead, so no secret goes into build settings or workflow inputs. A connector counts as
configured only when each of its secret slots is set.

### The scheduler (`ci/scheduler.rs`)

One Durable Object, `scheduler`, with SQLite (`durable::Db`, migrations appended, never
edited):

- `jobs`: id, repo, commit, name, run, needs (JSON), class, timeout_min, pin, task, caller,
  caller_name, notify_url, notify_key, state, attempt, connector, handle, url, excluded (JSON),
  token_hash, token_expires_at, deadline_at, created_at, queued_at, dispatched_at, started_at,
  finished_at, detail.
- `events`: job, at, state, detail: each change, once.
- `outbox`: notify posts and note pushes still to do, with tries and next_at.
- `notes`: repo, commit, text, pushed.
- `pool`: connector, availability JSON, checked_at: the cache `available` fills, 30 seconds.

Operations (`durable::call`): `submit`, `route`, `status`, `jobs`, `cancel`, `commit`,
`runner` (a runner's report, with its token's hash), `hook` (a verified sighting), `grant`
(a job token's hash → its grant), `pool`.

The alarm loop, set to the earliest of the next due item, at most 60 seconds away while any job
is live: dispatch queued jobs (with the queue fallback), poll jobs whose deadline passed or whose
connector has no webhook, send due outbox items. Each state change writes `jobs` and `events`
in one go, then fans out through the outbox, which the alarm sends: the `CI_EVENTS` queue gets
`{source:"ci", job, repo, commit, name, state, …}` (one try), a notify post when the job has
`notify` (three tries), and a final state rewrites the commit's note and queues its push (ten
tries).

### HTTP routes

- `POST /ci/hooks/{connector}`: the body is read once (capped at 1 MiB), handed to that
  connector's `webhook`, which verifies first. Unverified: 401, nothing read, one log line
  without headers. Unknown connector: 404.
- `POST /ci/runner/{job}`: `Authorization: Bearer <job token>`; `{ "state": "running" }` or
  `{ "state": "finished", "passed": bool, "detail": "…" }`. The token must be that job's and
  live. Phase 1 serves it; Containers (phase 2) is its first user.
- `POST /ci/token`: the OIDC exchange, phase 3. Until then it answers 404.

### Tools

| Tool | Does |
| --- | --- |
| `ci_pool` | each connector: classes offered, configured, healthy, free, queued |
| `ci_route` | dry run of match, availability and rank for `{repo, commit, job}`, or for `{needs, timeout_min}` given directly |
| `ci_submit` | `{repo, commit, job, connector?, task?, notify?}` → job id, state, the notify key once |
| `ci_status` | one job: state, connector, attempts, url, events |
| `ci_jobs` | recent jobs, filtered by repo, commit, state, caller, task |
| `ci_cancel` | cancels a live job at its connector |
| `ci_commit` | the results recorded for `{repo, commit}`, as the note says |

Every job carries the caller (and whom they act for) and `task`, cut to 100 characters, as
`models_run` tags requests.

## Phases

### Phase 1: the gateway, against a fake connector

| Step | Task | Model | Work |
| --- | --- | --- | --- |
| 1 | `ci-p1-1` | sonnet | `ci/config.rs` + tests |
| 2 | `ci-p1-2` | sonnet | `ci/class.rs` + tests |
| 3 | `ci-p1-3` | opus | `ci/route.rs` + tests |
| 4 | `ci-p1-4` | opus | `ci/state.rs` + tests |
| 5 | `ci-p1-5` | opus | `ci/token.rs` + core `sign.rs` + tests |
| 6 | `ci-p1-6` | opus | `ci/notify.rs` + tests |
| 7 | `ci-p1-7` | opus | core `gitobj.rs` (objects, ids, stored zlib, pack) + tests against `git`'s own ids |
| 8 | `ci-p1-8` | sonnet | `ci/notes.rs` + tests |
| 9 | me | — | `ci/scheduler.rs`: schema and ops, dispatch and the alarm, fan-out (see below) |
| 10 | me | — | `ci.rs` (trait, tools, routes), the fake connector, wiring, wrangler, docs, review |

**As built.** Steps 1–8 went to agents. Five answers were cut off at the token limit: config
and token were re-split into implementation-only and tests-only tasks; for route, class,
gitobj, sign and notes I kept only the functions and tests that came back whole, reviewed them
like the rest, and re-split or wrote the missing remainder (route's last tests by an agent;
class's `pick` and last tests, gitobj's last test and notes' last tests by me). Nothing
half-written was kept. Step 9 I wrote myself:
it is glue over the pure modules, and a brief carrying `durable.rs`, the worker APIs and every
module's interface was larger than what an agent can return. The scheduler's decisions are the
pure, tested functions (`route`, `state`, `after_dispatch`, `deadline`), and its tests drive a
fake connector through the real `Connector` trait.

Reading `.pigeon/ci.toml` at a commit goes behind one interface:

```rust
pub trait Files { fn read<'a>(&'a self, cx: &'a Cx<'a>, repo: &'a str, commit: &'a str,
                             path: &'a str) -> Pending<'a, Option<String>>; }
```

Phase 1 shipped `ProxyFiles` answering that it could not read yet. Since the routing plan's
phase 5 (main 36024af), `ProxyFiles` reads through `git_proxy::read_file`, which fetches the one
commit one deep with a git token minted for it and revoked after, and resolves deltas in
`pack.rs`; files over 64 KiB or not UTF-8 are refused. The git proxy also accepts job tokens for
upload-pack fetches of their grant's commit only, through `ci::job_grant`, `token::reaches` and
`token::allows_fetch`.

Tests (unit, native):
- needs → class for every size boundary, docker and gpu suffixes, too big, unknown OS;
- `ci.toml`: defaults, every rule above, unknown keys, bad paths;
- `covers`/`pick`: each limit, upsizing allowed, downsizing refused, network none;
- routing: refusal with every reason; unconfigured and unhealthy ruled out; full kept but not
  ready; settings order; shorter queue among full; pin wins; pin refused; exclude; fallback
  after the wait limit;
- states: every allowed and refused transition, terminal stays, failed never retried, infra
  retried once then error, deadline polls then times out;
- job tokens: prefix and length, hash only, grant for another repo, commit or job refused,
  expired refused, `reaches` for every path family;
- signing: HMAC-SHA256 against RFC 4231 vectors, the header format, constant-time compare;
  notify URLs (http, IP literals v4 and v6, localhost, ports, own host, userinfo);
- git objects: blob, tree and commit ids equal to `git hash-object` / `git mktree` /
  `git commit-tree` for the same content; a pack that `git index-pack` accepts (fixture made
  once, its expected id checked in the test);
- notes: line format, sorting, tree ordering.

The scheduler's SQLite and alarm need the Workers runtime and are not unit-tested; their
decisions are the pure functions above, and the live checks cover the rest.

Live checks after deploy (phase 1):
1. `ci_pool` answers an empty pool (no connector is set up yet).
2. `ci_route { needs: { cpu: 4 }, timeout_min: 20 }` answers class `linux-amd64-medium` and,
   with no connectors, a refusal listing none.
3. `ci_submit { repo: "pigeon-security/pigeon", commit: <sha>, job: "test" }` refuses with
   "reading files at a commit waits for the git proxy".
4. `POST /ci/hooks/nothing` → 404; `POST /ci/runner/x` without a token → 401; `POST /ci/token`
   → 404.
5. `ci_jobs` answers an empty list; the scheduler object exists (Worker logs show its
   migration).

Before deploy, the person (or the parent, with the person's go-ahead) creates the queue:
`pigeon-ci-events`. The deploy fails without it, since the Worker binds it as a producer.

### Phase 2: Cloudflare Containers

| Step | Task | Model | Work |
| --- | --- | --- | --- |
| 1 | `ci-p2-1` | opus | core `containers.rs`: start a container in a Durable Object (`State::container()`, workers-rs 0.9: `start` with env and entrypoint, `running`, `wait_for_exit`, `destroy`) |
| 2 | `ci-p2-2` | opus | the `CiRunner` Durable Object, one per job (named by job id): starts the container, waits for exit, reports `Gone` to the scheduler if the job has no result |
| 3 | `ci-p2-3` | opus | the containers connector: offers, available (a running count in the scheduler against a limit in settings), dispatch, status, cancel |
| 4 | `ci-p2-4` | sonnet | the runner entrypoint script baked into the base image: fetch the commit through the git proxy with `PIGEON_JOB_TOKEN`, run the job's script, POST `/ci/runner/{job}` |
| 5 | me | — | wrangler `[[containers]]`, wiring, review |

Needs from the person: the base image's registry reference.

Deploying a `durable_object` container calls, under
`https://api.cloudflare.com/client/v4/accounts/{account}/` (read from wrangler 4.166.0's
`prepareDurableObjectContainerApplications`, `applyApplication` and
`createDurableObjectNamespaceResolver`): `POST containers/image-preparations` (polled every 2 s
until the image is ready, up to 15 min); `GET containers/applications/{namespace_id}`, then
`POST containers/applications` (first deploy) or `PATCH containers/applications/{namespace_id}`
(when settings change); `GET workers/scripts/{script}/versions/{version_id}`; and
`GET workers/durable_objects/namespaces?per_page=1000`. The Worker upload's metadata gains a
`containers` entry (class name and images). The cf_proxy allowlist needs the `containers/`
calls.
Live checks: a job that passes, one that fails, one that times out, one whose container is
killed (retried elsewhere or `error`), the note on the commit.

**As built.** The base image is `pigeon-ci-base` (debian bookworm-slim with git, curl, coreutils, build-essential, pkg-config and Rust stable with the wasm target, clippy and rustfmt), digest-pinned in wrangler.toml as `[containers.images.base]`.
- Confirmed from the docs: under `durable_object` each `start()` names the image (a key of
  `ctx.container.images`) and the instance; there is no default image; wrangler's
  `instance_type` and `max_instances` do not apply. Whether a class first deployed without a
  container can gain one is not documented, so `CiRunner` gets its binding, migration and
  container in one deploy, once the image is known. `basic` is avoided (the docs disagree).
- core `containers.rs` (step 1, opus): `start`, `running`, `monitor`, `destroy` over the JS API.
- `ci/containers.rs` (steps 2 and 3, by me): the connector and the `Runner` logic the platform's
  `CiRunner` class wraps. A runner object is named `{job}-{attempt}`, keeps the job id and the
  exit code (never the token), and its alarm watches the container every minute.
- The scheduler enforces a connector's `limit()` (Containers: 4 live jobs).
- `ci/runner/entrypoint.sh` (step 4, sonnet): the base image installs it as
  `/usr/local/bin/pigeon-ci-runner`, beside git, curl, coreutils and sh.
- With the image: `image: Some("base")` in `settings::ci()`, and in wrangler.toml:

```toml
# in [durable_objects].bindings
  { name = "CI_RUNNER", class_name = "CiRunner" },

[[migrations]]
tag = "v3"
new_sqlite_classes = ["CiRunner"]

[[containers]]
class_name = "CiRunner"
scheduling_policy = "durable_object"

[containers.images.base]
image = "registry.cloudflare.com/<account>/<name>@sha256:<digest>"
```

### Phase 3: Buildkite

| Step | Task | Model | Work |
| --- | --- | --- | --- |
| 1 | `ci-p3-1` | opus | core `oidc.rs`: RS256 JWT verification against a JWKS (issuer, audience, expiry, `kid`), with the `rsa` crate |
| 2 | `ci-p3-2` | sonnet | core `buildkite.rs`: create and cancel a build, read one, queue metrics |
| 3 | `ci-p3-3` | opus | webhook verification (`X-Buildkite-Signature` HMAC when set, else `X-Buildkite-Token`, constant-time) |
| 4 | `ci-p3-4` | opus | `/ci/token`: Buildkite OIDC token → job token, checked against the job's build |
| 5 | `ci-p3-5` | sonnet | the connector, and `.buildkite/pipeline.yml` for the pigeon pipeline |

Needs from the person: a Buildkite organization, a pipeline named `pigeon` using hosted
agents, a hosted queue per class (names in settings), `BUILDKITE_API_TOKEN` and
`BUILDKITE_WEBHOOK_TOKEN` set on the secrets page, and a webhook for `build.finished` (and
`job.started`) to `https://platform.pigeon.dev/ci/hooks/buildkite`.

**As built.**
- Facts, read from `buildkite/docs` and `buildkite/agent` on GitHub (buildkite.com is not
  reachable from the session): the steps editor interpolates only a fixed set of `BUILDKITE_*`
  variables, but `build.env("X")` in a step's `if:` reads custom build env when the YAML steps
  editor is on (`pages/pipelines/configure/conditionals.md`); `checkout: { skip: true }` skips
  the checkout and works on hosted agents (`pages/pipelines/configure/git_checkout.md`);
  webhook signatures are `timestamp=…,signature=<hex HMAC-SHA256 of "<ts>.<body>">`, with a
  5-minute window advised; hosted shapes `LINUX_AMD64_2X4` (2 vCPU, 4 GB, 47 GB) and
  `LINUX_AMD64_4X16` (4, 16, 95), the Free plan only the first.
- The pipeline's steps are one static step per queue, chosen with
  `if: build.env("PIGEON_QUEUE") == "<queue>"`, skipping checkout and running
  `curl -fsSL https://platform.pigeon.dev/ci/buildkite.sh | sh` (`steps_yaml`). The YAML holds
  no `$`, so nothing is interpolated.
- `/ci/buildkite.sh` (served by the platform, `ci/runner/buildkite.sh`): requests the step's
  OIDC token (audience `https://platform.pigeon.dev/ci/token`, 300 s), exchanges it at
  `/ci/token` for the job's token, adds it to Buildkite's redactor, fetches `/ci/runner.sh`
  (the same runner as Containers) and runs it with `PIGEON_WORK` in a temporary directory.
- `/ci/token`: core `oidc.rs` verifies RS256 against `https://agent.buildkite.com/.well-known/jwks`
  (issuer, audience, `exp`/`iat`/`nbf` with 60 s leeway, lifetime at most an hour); the
  connector then requires `organization_slug`, `pipeline_slug`, `runner_environment =
  buildkite-hosted`, UUID `build_id`/`job_id` and a positive `build_number`; the scheduler
  mints a job token only for the live job whose handle is that build (`{number}/{uuid}`) on
  that connector. A later exchange for the same build replaces the token.
- Webhooks: verified first (core `buildkite_hook.rs`); a ping answers 200 and does nothing;
  `build.running` is the start; `build.finished` without the runner having reported is a lost
  runner (retried once elsewhere), `canceled` is cancelled; builds without the gateway's
  `pigeon_job` meta-data, or not matching a live job's handle, are ignored.
- Queues: `pigeon-linux-small` and `pigeon-linux-medium`, each offering its class with and
  without docker, as rows of `BUILDKITE_QUEUES` in settings.rs. Linux only, by the person's
  decision: macOS comes back with the iOS app as its own piece of work, as further rows.
- No change to the git proxy or the Cloudflare API proxy: the Worker calls api.buildkite.com
  and agent.buildkite.com directly, and runners fetch through the git proxy with job tokens,
  which it already accepts.
- Unconfirmed until live: the OIDC signing algorithm (assumed RS256; an ES256 key would be
  refused with "the token algorithm is not RS256"), and that the hosted Linux image has curl.

### Phase 4: GitHub Actions

| Step | Task | Model | Work |
| --- | --- | --- | --- |
| 1 | `ci-p4-1` | sonnet | core `github_actions.rs`: dispatch, get run, cancel run, list runs by name |
| 2 | `ci-p4-2` | opus | webhook verification (`X-Hub-Signature-256`, constant-time) |
| 3 | `ci-p4-3` | opus | `/ci/token` for GitHub's OIDC issuer, checked against the run |
| 4 | `ci-p4-4` | sonnet | the connector, and `.github/workflows/pigeon-ci.yml` (workflow_dispatch, `id-token: write`) |

Needs from the person: `GITHUB_ACTIONS_TOKEN` (fine-grained, Actions read and write on the
mirror) and `GITHUB_WEBHOOK_SECRET` set on the secrets page, a repository webhook for
`workflow_run` and `workflow_job` to `https://platform.pigeon.dev/ci/hooks/github_actions`
with that secret. The workflow file reaches GitHub through the mirror, so it must be on `main`.

## Confirmations

Checked 2026-11-21. developers.cloudflare.com was read directly; Buildkite's and GitHub's
docs were read from their official sources on GitHub (`buildkite/docs`, `github/docs`,
`github/rest-api-description`, commits of 2026-11-20), because their sites are not reachable
from this session; workers-rs from the `worker` 0.9.0 crate source.

**Containers: one instance per job, and learning it exited.**
- The `@cloudflare/containers` helper is deprecated in favour of the Durable Object container
  API: each Durable Object owns one container, addressed by name, started with
  `ctx.container.start({env, entrypoint, enableInternet, image, …})`; `start` returns before
  the container is ready and throws if one is already running; `enableInternet` defaults to
  off. [Migration guide](https://developers.cloudflare.com/containers/guides/migrate-to-durable-object-container-api/),
  [API](https://developers.cloudflare.com/containers/api/durable-object-container/).
- So a job is one `CiRunner` Durable Object named by the job id.
- Exit: `monitor()` resolves on exit 0 and rejects with `exitCode` otherwise. A pending
  `monitor()` holds the object for at most 15 minutes and does not survive a restart, so the
  backstop is an alarm that checks `running` and calls `monitor()` again.
  [API](https://developers.cloudflare.com/containers/api/durable-object-container/),
  [status hooks](https://developers.cloudflare.com/containers/examples/status-hooks/).
- workers-rs 0.9.0 has `State::container()` with `start`, `running`, `wait_for_exit`
  (`monitor`), `destroy` and `signal`. Its `ContainerStartupOptions` has only `entrypoint`,
  `enable_internet` and `env`, with no `image`; and `env` is built as a JS `Map`, where the API
  takes a plain object, so env vars may not arrive. Phase 2 calls the JS API through
  `State::as_raw()` where needed and checks env delivery live first.
- Instance types: lite (1/16 vCPU, 256 MiB, 2 GB), basic (1/4, 1 GiB, 4 GB), standard-1 (1/2,
  4 GiB, 8 GB), standard-2 (1, 6 GiB, 12 GB), standard-3 (2, 8 GiB, 16 GB), standard-4 (4,
  12 GiB, 20 GB); at most 4 vCPU, 12 GiB and 20 GB per instance unless raised.
  [Limits](https://developers.cloudflare.com/containers/platform/limits/).
- Under the `durable_object` scheduling policy there is no `max_instances`, so the gateway
  keeps its own concurrency count. [Scheduling policy](https://developers.cloudflare.com/containers/configuration/scheduling-policy/).
- Images under that policy are named entries pinned by digest on `registry.cloudflare.com` (or
  built from a Dockerfile); other registries are not accepted.
  [Image management](https://developers.cloudflare.com/containers/guides/image-management/).
- Docker inside a container works only as root `docker:dind` with
  `--iptables=false --ip6tables=false --ip-forward=false` and host networking.
  [FAQ](https://developers.cloudflare.com/containers/faq/).

**Artifacts and `refs/notes/ci`: not confirmed.** The docs state no restriction on ref names
and do not mention notes; push is v1 `git-receive-pack`, and the one ref limit is "1 MiB of
branch and tag updates per push". [Git protocol](https://developers.cloudflare.com/artifacts/api/git-protocol/),
[limits](https://developers.cloudflare.com/artifacts/platform/limits/). It is the first live
check after phase 1. If Artifacts refuses the ref, the note stays in the scheduler (where
`ci_commit` reads it) and the outbox gives up after its retries; nothing else depends on it.

**Buildkite.**
- Hosted Linux shapes: `LINUX_{AMD64,ARM64}_{2X4,4X16,8X32,16X64,32X128,64X256}` (vCPU × GB);
  the Free plan gets only the small AMD64 shape and no ARM64; XL and up are Enterprise. Jobs up
  to 8 hours; the Ubuntu image has docker. [Linux hosted agents](https://buildkite.com/docs/agent/buildkite-hosted/linux)
  (source `pages/agent/buildkite_hosted/linux.md`).
- OIDC: `buildkite-agent oidc request-token --audience …`; issuer `https://agent.buildkite.com`
  for every organization, JWKS at `https://agent.buildkite.com/.well-known/jwks`, 5-minute
  lifetime by default; claims include `organization_slug`, `pipeline_slug`, `build_id`,
  `build_number`, `build_commit`, `step_key` and `job_id`. Since the issuer is shared, the
  exchange must check `organization_slug` and `pipeline_slug`, not just the signature.
  [OIDC](https://buildkite.com/docs/pipelines/security/oidc),
  [CLI](https://buildkite.com/docs/agent/cli/reference/oidc).
- Webhooks carry either `X-Buildkite-Token` or `X-Buildkite-Signature: timestamp=…,signature=…`
  (hex HMAC-SHA256 of `"{timestamp}.{body}"` with the webhook token). The plan takes the
  signature form and refuses timestamps older than 5 minutes.
  [Webhooks](https://buildkite.com/docs/apis/webhooks/pipelines).
- Builds: `POST /v2/organizations/{org}/pipelines/{pipeline}/builds` with `commit`, `branch`,
  `env`, `meta_data`; cancel with `PUT …/builds/{number}/cancel`.
  [Builds API](https://buildkite.com/docs/apis/rest-api/builds).

**GitHub Actions.**
- Dispatch now returns the run: with `return_run_details: true`,
  `POST /repos/{o}/{r}/actions/workflows/{id}/dispatches` answers 200 with
  `{workflow_run_id, run_url, html_url}` (204 and no body otherwise). So no matching by run
  name is needed. [Workflow dispatch](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event).
- `workflow_run` webhook: actions `requested`, `in_progress`, `completed`; `workflow_run.id`,
  `status`, `conclusion`, `head_sha`, `html_url`. Verified with `X-Hub-Signature-256:
  sha256=<hex HMAC-SHA256 of the body>`, compared in constant time.
  [Payloads](https://docs.github.com/en/webhooks/webhook-events-and-payloads#workflow_run),
  [validation](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries).
- OIDC: issuer `https://token.actions.githubusercontent.com`, claims `repository`, `sha`,
  `run_id`, `workflow_ref`, needs `permissions: id-token: write`.
  [OIDC](https://docs.github.com/en/actions/reference/security/oidc). The JWKS URL comes from
  the issuer's discovery document; phase 4 reads it there rather than hard-coding it.
- Hosted runners: `ubuntu-latest` and `ubuntu-24.04-arm` are 4 vCPU / 16 GB on public
  repositories, 2 vCPU / 8 GB on private ones, 14 GB disk.
  [Hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

## Corrections to the design

- **Containers sizes are small.** At most 4 vCPU and 12 GiB per instance, so Containers can
  offer `small` (standard-3) and only a reduced `medium` (standard-4: 4 vCPU, 12 GiB, 20 GB).
  A job asking `memory_gb` above 12 cannot go to Containers; `covers` checks real limits, so
  routing sends it on. The default `disk_gb` is therefore 10, not 20, so a default job fits
  standard-3's 16 GB.
- **Docker on Containers is possible but heavy** (root dind with networking switched off);
  phase 2 offers no `-docker` class on Containers. Docker jobs go to Buildkite or GitHub
  Actions.
- **The base image must be on `registry.cloudflare.com`, pinned by digest**, not any registry.
- **The `notify` signing key is per job**, returned once by `ci_submit`: the design said
  "signed POST" without naming a key, and a per-job key needs no new secret slot.
- **Reading `ci.toml` needs a pack reader that resolves deltas**, which the route plan's
  `pack.rs` does not do (it refuses deltas of commits). Phase 1 ships the interface; see
  phase 1.
- **The queue must exist before the deploy** that binds it as a producer.

## What can only be checked live

- Everything that runs in the Workers runtime: the scheduler's SQLite and alarms, the queue
  producer, the notify fetch, the notes push to Artifacts.
- That Artifacts accepts the push to `refs/notes/ci` (see Confirmations).
- That the git proxy accepts a job token for fetches of its one commit only (the proxy's
  work).
- Each connector's real dispatch, webhooks and status, phase by phase.

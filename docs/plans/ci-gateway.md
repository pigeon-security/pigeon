# The CI gateway

Status: plan written; phase 1 next. Phases are handed back one at a time; each is merged and
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
    pub disk_gb: u32,      // 20
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
pub struct Candidate<'a> { pub name: &'a str, pub offers: &'a [Offered], pub availability: Availability }
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
- **Rank:** candidates arrive in settings order. A pinned connector is the only one ranked (a
  pin that does not match or is unavailable is a refusal naming why). Otherwise settings order,
  ready before full, and among full ones the shorter `queued` first. Cost and limits are later
  sort keys here.
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
  `<job> <state> connector=<c> attempt=<n> duration=<s>s url=<u>`, sorted by job name.
- The scheduler keeps every note it has written (`notes` table). To write one it mints a
  write git token for the repository, reads `refs/notes/ci`'s tip (`git::refs`), builds a blob
  for the note, a flat tree of every note it keeps (`<40-hex commit>` → blob, sorted), and a
  commit whose parent is the tip, packs those three objects (undeltified; zlib as stored
  blocks, so no compressor is needed) and pushes with `git::push`, `old` = the tip. A rejected
  push (the tip moved) is retried by the alarm with the new tip. The git token is revoked after.
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
                 pub token: Option<String>, pub runner_url: String, pub git_url: String }
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
in one go, then fans out: the outbox gets a notify post when the job has `notify`, the
`CI_EVENTS` queue gets `{source:"ci", job, repo, commit, name, state, …}`, and a final state
puts the commit's note in the outbox.

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
| 9 | `ci-p1-9` | opus | `ci/scheduler.rs`, in pieces: schema and ops; dispatch and the alarm; fan-out |
| 10 | me | — | `ci.rs` (trait, tools, routes), the fake connector, wiring, wrangler, docs, review |

Reading `.pigeon/ci.toml` at a commit goes behind one interface:

```rust
pub trait Files { fn read<'a>(&'a self, cx: &'a Cx<'a>, repo: &'a str, commit: &'a str,
                             path: &'a str) -> Pending<'a, Option<String>>; }
```

Phase 1 ships `ProxyFiles`, which answers "reading files at a commit waits for the git proxy",
so `ci_submit {repo, commit, job}` refuses with that reason until the proxy's fetch (and a pack
reader that resolves deltas) is live; tests use a fake. `ci_route` with `needs` given directly
works from day one. A pack reader that resolves deltas is the route plan's phase 2 work
(`pack.rs`, which refuses deltas today); when it lands, `ProxyFiles` reads the tree and blob
through it. If it does not grow delta support, reading `ci.toml` needs it: called out in the
hand-back.

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
Live checks: a job that passes, one that fails, one that times out, one whose container is
killed (retried elsewhere or `error`), the note on the commit.

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

FACTS

## Corrections to the design

CORRECTIONS

## What can only be checked live

- Everything that runs in the Workers runtime: the scheduler's SQLite and alarms, the queue
  producer, the notify fetch, the notes push to Artifacts.
- That Artifacts accepts the push to `refs/notes/ci` (see Confirmations).
- That the git proxy accepts a job token for fetches of its one commit only (the proxy's
  work).
- Each connector's real dispatch, webhooks and status, phase by phase.

# Routing cloud sessions' traffic through pigeon's platform

Status: phases 1 to 5 deployed; phase 6 (container deploys through `/cf/`) is in the code (`src/proxies/src/cf_proxy.rs`). Operator docs: `docs/proxies.md`. Phases are handed back one at a time; each is merged and deployed before the
next starts.

## Why

Today a cloud session reaches four hosts directly, with tokens pasted into commands (so they
also land in transcripts):

| Host | What for | Credential today |
| --- | --- | --- |
| `platform.pigeon.dev` | MCP | OAuth bearer token |
| `4f94676cb2cbe46fd985b089ca97ea27.artifacts.cloudflare.net` | git fetch and push | `source_token` git token, pasted |
| `api.cloudflare.com` | `wrangler deploy` | `deploy_token` API token, pasted |
| `github.com` | the session's first clone | the session's own GitHub access |

The goal: sessions talk only to pigeon's platform: `platform.pigeon.dev` for MCP, as today,
and `proxy.pigeon.dev` for git and the Cloudflare API. The platform Worker serves both; it
holds the real credentials and mints a short-lived one for each request. The session holds
one **proxy credential**, saved as the environment's network secret for `proxy.pigeon.dev`
only. The session's network proxy adds it as
`Authorization: Bearer …`, so no command, and no transcript, ever sees it.

> **Since 2026-11-22** (docs/plans/workers-split.md, phase 6) the proxy Worker, `pigeon-proxy`,
> serves `proxy.pigeon.dev`'s `/git/` and `/cf/`; it holds no parent token and asks core for
> each check. Core (`pigeon-platform`) still issues and revokes the proxy credentials (the
> `proxy` capability, Access › Tokens). Where this plan says the platform Worker serves the
> proxies, read it as the history it is.

## Who does what

- **Planning session (this one):** writes the plan, briefs the agents, applies their output,
  builds, tests and reviews every line, pushes its own branch, hands each phase back.
- **Agents:** Claude Platform models through the `models_run` tool (one request, one answer, no
  tools). Each brief holds the goal, the current files it touches, the interfaces to use, the
  style, and what to return. Tasks are tagged `route-p<phase>-<step>`.
  `claude-opus-5-5` for security-sensitive code (scopes, proxies, push rules, allowlist);
  `claude-sonnet-5-5` for routine code (pages, docs).
  The connector's `models_run` call times out after 60 seconds, which is about 3,000 output
  tokens. So each task asks for one small piece (pure functions, one set of handlers, one
  script), and I finish any answer that is cut off, reviewing it like the rest.
- **Parent session (`session_015XFA1jgveJDEE8NJjTaHKg`):** merges, deploys, runs the live
  checks.
- **The person:** issues the proxy credential on `/tokens`, saves it as the network secret,
  sets the environment variables for wrangler, and approves removing hosts.

## Shared pieces

### Context gains the Worker's context

Proxied responses stream; the git token must be revoked after the stream ends, which is after
`fetch` returns. `Context` gains `pub worker: Option<&'a worker::Context>` so a capability can
`wait_until` the revoke. `fetch` in `src/platform/src/lib.rs` passes it; queue and MCP pass
`None`. (Phase 2, step 1.)

### The audit line

One `console_log!` per proxied request, as one JSON object:

```json
{"audit":"proxy","route":"git","caller":"<token label> (<person>)","token":"<the credential's id: the first 12 chars of its hash>",
 "method":"POST","path":"/git/pigeon-security/pigeon.git/git-receive-pack","outcome":"forwarded|refused|failed",
 "status":200,"why":"<reason when refused or failed>"}
```

It never holds a token, a token's full hash, a header value, a query string or a body. The
`/cf/` path is logged with account and resource IDs as they are (they are not secrets). One
helper, `proxy::audit(...)`, writes it; both proxies call it.

## Phase 1: the proxy credential

**Adds:** a `proxy` scope for platform bearer tokens and the passkey-gated `/tokens` page.

Design:
- Bearer tokens live in the Directory's `oauth_tokens` table, keyed by hash, with a `kind`.
  MCP looks a token up with `kind = 'access'` only. A proxy token is stored with
  `kind = 'proxy'`, prefix `pgn_px_`, so MCP never finds it: refusal by construction, not by
  a check someone can forget. One new migration (appended, never edited):
  `ALTER TABLE oauth_tokens ADD COLUMN label TEXT`.
- Its `service_user_id` holds the person who issued it (the column name is historical), so the
  audit line can say whose credential made a call.
- A pure function decides what a token kind may reach, unit-tested both ways:
  `fn reaches(kind: &str, path: &str) -> bool`: `proxy` reaches only `/git/` and `/cf/`;
  `access` reaches only `/mcp`. The proxies refuse an `access` token too, so an MCP token
  pasted somewhere can't push.
- `proxy::caller(env, req) -> Result<ProxyCaller>` reads `Authorization: Bearer`, hashes it,
  looks up `kind = 'proxy'`, and answers `{ token_id (hash prefix), label, person }`. Errors
  never echo the header.
- Directory operations: `proxy.token_add` (hash, label, person, ttl), `proxy.tokens` (live
  ones: hash prefix as id, label, person, created, expires; never the hash), `proxy.token_remove`
  (by id prefix, refusing an ambiguous prefix).
- `/tokens` page, on the `/secrets` page's pattern: a passkey to see the list; issuing takes a
  passkey of its own whose challenge purpose is bound to the label and the lifetime
  (`proxy-token:issue:<label>:<days>`); revoking takes one bound to the id
  (`proxy-token:revoke:<id>`). Only a person (`kind = standard`) may. The new token is shown
  once, with a copy button and the instruction to save it as the environment's network secret
  for `proxy.pigeon.dev`. Lifetime: 30 days by default, 90 at most.
- No MCP tool issues a proxy token: issuing one is a person's act.

Steps:
| Step | Task tag | Model | Work |
| --- | --- | --- | --- |
| 1 | `route-p1-1` | opus | `src/capabilities/src/proxy.rs`: the capability, `reaches`, `caller`, `audit`, the Directory ops (in `users.rs`), the migration, the page's routes and handlers, unit tests |
| 2 | `route-p1-2` | sonnet | `src/capabilities/src/proxy/tokens.js`: the page's script, on `secrets.js`'s pattern |
| 3 | me | — | register in `lib.rs`/`settings.rs`, the module docs, review, build, test |

Tests: `reaches` for every kind and path family (including `/gitx`, `/git` without slash, `/cf`
without slash, encoded slashes); the challenge purposes differ by label, days and id; listing
never carries a hash.

Live checks after deploy: open `/tokens`, sign in, issue a token, see it listed, revoke it,
see it gone; `curl -H 'Authorization: Bearer <proxy token>' -X POST /mcp` answers 401.

The person: issue a token and save it as the pigeon environment's network secret for
`proxy.pigeon.dev` (header `Authorization`, prefix `Bearer`).

> **Decided (during phase 2): the proxies get a hostname of their own, `proxy.pigeon.dev`.**
> A network secret on `platform.pigeon.dev` could have replaced the MCP connector's own
> `Authorization` header there, and MCP would then refuse the request. With the secret on
> `proxy.pigeon.dev` only, that can't happen. The Worker serves only `/git/`, `/cf/` and
> `/health` on `proxy.pigeon.dev`, and answers 404 for `/git/` and `/cf/` on every other host
> (`proxy::host_serves`, unit-tested). `proxy.pigeon.dev` is attached to the Worker by a
> second `PublicHost` in `settings.rs`, set up with the `setup_proxy_host` tool.

## Phase 2: the git proxy

**Adds:** `proxy.pigeon.dev/git/{namespace}/{repo}.git/…`, streaming to Artifacts.

Routes, and nothing else (anything else is 404 before any token is minted):
- `GET  /git/{ns}/{repo}.git/info/refs?service=git-upload-pack|git-receive-pack`
- `POST /git/{ns}/{repo}.git/git-upload-pack`
- `POST /git/{ns}/{repo}.git/git-receive-pack`

`{ns}/{repo}` must be the source repository named in settings (`pigeon-security/pigeon`);
others are refused, so a proxy token reaches one repository only.

Per request:
1. Authenticate with `proxy::caller`; refuse 401 otherwise.
2. Check the route; refuse 404/403 otherwise.
3. Mint a git token (read for upload-pack, write for receive-pack), with the upstream remote
   URL, using one scoped Cloudflare token for both calls (`artifacts::session`, new in core).
   Lifetime 15 minutes: the ceiling if a revoke is lost.
4. A freshly minted Artifacts token can answer 401 for a few seconds. A streamed POST body can
   be sent once only, so the proxy first warms the token with `GET info/refs` (retried on 401
   as `git::send` does), then forwards the real request.
5. Forward method, path, query, `Content-Type`, `Accept`, `Content-Encoding`, `Git-Protocol`
   and `User-Agent`; drop every other request header, the caller's `Authorization` above all.
   The upstream `Authorization` is set by the proxy. The answer streams back with its status,
   `Content-Type` and `Cache-Control`.
6. The response body is wrapped in a stream that signals when it ends or is dropped;
   `wait_until` then revokes the git token.
7. Audit line.

Push rules (receive-pack). The request body starts with ref-update commands as pkt-lines
(`<old> <new> <ref>\0<caps>`, then `<old> <new> <ref>`…, then a flush), then optional push
options, then the pack. The proxy reads the commands before anything is forwarded:
- No deletes: `new` all zeros is refused.
- `refs/heads/main` may only move forward. Creating `main` (old all zeros) is refused.
- Every other ref must be under `refs/heads/claude/`. Tags and other namespaces are refused.
- A malformed command section, or one over 64 KiB, is refused.
- A refusal answers as git expects: `200`, `application/x-git-receive-pack-result`,
  `unpack ok` then `ng <ref> <reason>` for every command (wrapped in side-band 1 when the
  client asked for `side-band-64k`), so `git push` prints the reason. Nothing is forwarded.

**Plan correction: fast-forward can't be decided from the commands alone.** The commands carry
only the old and new IDs, not their ancestry; git sends the remote's current ID as `old` even
on a force push. So a push that updates `main` is handled differently:
- It is buffered (capped at 16 MiB; larger is refused with a message to push smaller).
- The pack's commit objects are read (zlib, so the pure-Rust `miniz_oxide` crate is added to
  `pigeon-core`). The walk goes from `new` through parents. Reaching `old` means fast-forward.
- Parents that are not in the pushed pack already exist upstream (the common case: a
  `claude/*` branch already pushed, then `main` moved to its tip, which sends an empty pack).
  For those, the proxy asks upstream with `git-upload-pack` (`want <those>`, `have <old>`,
  `done`), reads the commits in that pack, and continues the walk. Upstream leaves out
  everything `old` reaches, so if `old` is an ancestor, the walk meets it as a parent.
- A commit stored as a delta, or anything the walk can't read, refuses the push (fail closed)
  with a message.
- Pushes that touch only `claude/*` refs are streamed, after their commands pass.

Steps:
| Step | Task tag | Model | Work |
| --- | --- | --- | --- |
| 1 | me | — | `Context.worker`; `artifacts::session` (one scoped token for repo + mint) |
| 2 | `route-p2-2` | opus | core `git.rs`: `commands(bytes) -> Result<Option<Commands>>` (parses the command section, reports where it ends, `None` when more bytes are needed); the refusal report builder; unit tests |
| 3 | `route-p2-3` | opus | core `pack.rs`: read a pack's commits (id = SHA-1 of `commit <len>\0<body>`, so `sha1` is added too), parents of each; refuse deltas of commits; unit tests built from real `git` output fixtures |
| 4 | `route-p2-4` | opus | capability `src/capabilities/src/git_proxy.rs`: routes, the push-rule policy (`fn check(cmd, rules) -> Result<(), String>`, pure, tested), the ancestry walk, streaming, revoke on end, audit |
| 5 | me | — | wire, review, build, test; a local end-to-end run of the pure parts against packs `git` produces |

Tests: the command parser on one and many commands, with and without capabilities, push
options, split across chunks, oversized, malformed lengths; every push rule (delete, create
main, tag, `refs/heads/claudex`, `refs/heads/claude/x`, `refs/heads/main` update); the report
format with and without side-band; the pack reader on packs made by `git pack-objects` (plain,
`ofs-delta`, empty pack); the ancestry walk on fast-forward, non-fast-forward, merge, and
"already upstream" histories.

Live checks after deploy (from a session with the network secret saved):
`git ls-remote https://proxy.pigeon.dev/git/pigeon-security/pigeon.git`; a clone; a push to
`claude/route-check`; a push of a non-`claude/` branch (refused with the reason); a branch
delete (refused); a force push to `main` (refused); a fast-forward of `main` from a
`claude/*` branch (accepted; the parent session decides whether to run this one); the Worker
logs show one audit line per request and no token; `source_tokens` shows the minted git tokens
revoked.

As built (differences from the above, decided while building):
- The proxies' hostname is the setting `PROXY_HOST` in `settings.rs`: `proxy.pigeon.dev`
  (see the decision under phase 1).
- An unknown or missing credential gets 403, not 401: a 401 sends git looking for a password
  to prompt for. An upstream 401/403 (the proxy's own git token refused) becomes a 502.
- Upstream history for the fast-forward check is asked for by wanting every upstream ref tip
  (less `old`), with `have old` and `filter tree:0` when Artifacts offers filtering. This
  avoids depending on Artifacts allowing a want of any commit ID.
- A push whose pack stores the needed commit as a delta against an object outside the pack
  can't be checked. It is refused with a message to push the commit to a `claude/*` branch
  first, then move `main` to it (an empty pack, checked against upstream).
- A push with a `Content-Encoding` other than identity is refused (git doesn't compress
  pushes). Upload-pack's `Content-Encoding` is passed on.
- Caps: a push that moves `main` is read up to 16 MiB; a pack inflates to 48 MiB at most.
  Together they stay well inside a Worker's 128 MB of memory.
- The git token is revoked in `wait_until` once the answer's body has been read to its end or
  dropped, and also on every early return after minting.

Limits to record: Workers request bodies are capped by plan (100 MB on Free and Pro), which
caps a streamed push; a push to `main` is capped at 16 MiB by the buffer.

## Phase 3: the Cloudflare API proxy

**Adds:** `proxy.pigeon.dev/cf/client/v4/*` → `https://api.cloudflare.com/client/v4/*`.

Wrangler's base-URL variable, confirmed in wrangler 4.166.0's `wrangler-dist/cli.js`:
`getCloudflareApiBaseUrlFromEnv = getEnvironmentVariableFactory({ variableName:
"CLOUDFLARE_API_BASE_URL", deprecatedName: "CF_API_BASE_URL" })`, used in place of
`https://api.cloudflare.com/client/v4` when set. Metrics: `WRANGLER_SEND_METRICS=false`
(error reports already default off: `WRANGLER_SEND_ERROR_REPORTS` defaults to false). So a
deploy runs with:

```
CLOUDFLARE_API_BASE_URL=https://proxy.pigeon.dev/cf/client/v4
CLOUDFLARE_ACCOUNT_ID=4f94676cb2cbe46fd985b089ca97ea27
CLOUDFLARE_API_TOKEN=unused      # wrangler won't start without one; the session's proxy replaces the header
WRANGLER_SEND_METRICS=false
```

Design:
- An allowlist of `(method, path pattern)` pairs, each with the Cloudflare permissions its
  minted token holds. Patterns are literal segments plus `{account}` (must equal the platform's
  account ID), `{script}` (must be `pigeon-platform`) and `{id}` (one segment of
  `[A-Za-z0-9_-]`). The matcher is pure and tested: no `..`, no empty segments, no
  percent-encoded slashes, query strings matched against an allowlist per entry.
- The exact list comes from what `wrangler deploy` of `src/platform` calls. Step 1 finds it
  empirically: run that wrangler against a local recording server (base URL pointed at it,
  canned answers) and read wrangler's source for each call seen. The current guess, to be
  replaced by what is recorded: script settings and versions/deployments under
  `/accounts/{account}/workers/scripts/{script}…`, the account's `workers/subdomain`,
  `/accounts/{account}/workers/durable_objects/namespaces` for the migration, and
  `/memberships` or `/user` if wrangler still asks with `CLOUDFLARE_ACCOUNT_ID` set.

  **Recorded** (wrangler 4.166.0, `src/platform`'s config with a stub module, against a
  stand-in API that logs each call; both ways wrangler deploys, each call then read in
  `wrangler-dist/cli.js`). `{a}` is the account, `{s}` is `pigeon-platform`:

  | Call | Permissions | What for |
  | --- | --- | --- |
  | `GET /accounts/{a}/workers/services/{s}` | Workers Scripts Read | exists? migration tag |
  | `GET /accounts/{a}/workers/scripts/{s}/secrets` | Workers Scripts Read | secrets |
  | `GET /accounts/{a}/workers/scripts/{s}/deployments` | Workers Scripts Read | deployments |
  | `GET /accounts/{a}/workers/scripts/{s}/settings` | Workers Scripts Read | settings |
  | `POST …/scripts/{s}/versions?bindings_inherit=strict` | Workers Scripts Write, Secrets Store Write | upload, no migration pending |
  | `POST …/scripts/{s}/deployments` | Workers Scripts Write | deploy that version |
  | `PATCH …/scripts/{s}/script-settings` | Workers Scripts Write | logpush, observability, tags |
  | `PUT …/scripts/{s}?excludeScript=true&bindings_inherit=strict` | Workers Scripts Write, Secrets Store Write | upload and deploy, migration pending |
  | `GET /accounts/{a}/workers/workers/{s}` | Workers Scripts Read | workers.dev settings |
  | `POST …/scripts/{s}/subdomain` | Workers Scripts Write | keep workers.dev off |

  Wrangler takes the versions route when the Worker exists and no Durable Object migration
  is pending, and the `PUT` route otherwise (`canUseNewVersionsDeploymentsApi`). No
  `/memberships` or `/user` call is made with `CLOUDFLARE_ACCOUNT_ID` set. Every call carries
  only `Content-Type` that matters; the proxy passes on nothing else.
- Per request: authenticate (`proxy::caller`), match, mint a token holding only that entry's
  permissions (5-minute lifetime), forward (method, path, query, `Content-Type`; nothing
  else), read the answer (small JSON), revoke, answer, audit line. As built, the answer is
  read whole rather than streamed: the API's answers are small, and revoking before answering
  is simpler and certain. Request bodies are capped at 16 MiB. Request bodies are buffered (a
  Worker upload is at most 10 MB) so a call refused with 401 by a token not yet active can be
  retried, as `call_fresh` does.
- Refusals answer in Cloudflare's own shape (`{"success":false,"errors":[{"code":…,
  "message":"pigeon's proxy does not allow …"}]}`, 403) so wrangler prints the reason.

Steps:
| Step | Task tag | Model | Work |
| --- | --- | --- | --- |
| 1 | me | — | record wrangler's calls; write the allowlist with each call's source in wrangler |
| 2 | `route-p3-2` | opus | `src/capabilities/src/cf_proxy.rs`: matcher (pure, tested), forwarding, minting, audit |
| 3 | me | — | wire, review, build, test; replay the recorded calls through the matcher in a test |

Live checks after deploy: with the variables above, `wrangler deploy` of `src/platform`
succeeds; `wrangler whoami` or any call outside the list is refused with the proxy's message;
logs hold one audit line per call; the account's token list shows no `pigeon:cf-proxy:*`
token left live.

## Phase 4: cleanup and documentation

- Session branches stay in Artifacts: the mirror already copies only `main` to GitHub. This is
  documented, and a test pins the mirror's branch list in settings.
- The MCP server's `instructions` (in `lib.rs`) say to push through `/git/` with the proxy
  credential instead of minting `source_token`.
- `docs/` gains a page on the proxy credential, the two proxies, the push rules, the audit
  line, and the wrangler variables. Module `//!` docs stay accurate, since the architecture
  page is drawn from them.
- `source_token` and `deploy_token` stay for now (the parent session and people still use
  them); retiring them is a decision for after the proxies have run for a while.
  **Decided (26 November):** `deploy_token` and `revoke_api_token` are retired. Deploys go
  through `land` or `/cf/` with the proxy credential, and a deploy neither can make is done with
  the person's own Cloudflare login (`tool-approvals.md`, "Break-glass"). `source_token` stays:
  the git proxy refuses writes outside `claude/` (removing other branches by hand, for one), and
  only a direct Artifacts token does those.

Steps:
| Step | Task tag | Model | Work |
| --- | --- | --- | --- |
| 1 | `route-p4-1` | sonnet | the docs page, from the merged code |
| 2 | me | — | instructions text, review |

`proxy.pigeon.dev` needs no allowlist entry of its own: saving a network secret for a host
also allows requests to it.

Hosts the person can remove from the environment's allowlist once phases 2 and 3 are live and
checked: `4f94676cb2cbe46fd985b089ca97ea27.artifacts.cloudflare.net` and `api.cloudflare.com`.
`github.com`: only after checking the session's own first clone and the GitHub tools don't
depend on the allowlist entry; the hand-back will say what was checked.

## Phase 5: CI runners and reading files (asked for after phase 4)

- **Job tokens in the git proxy.** A `pgn_ci_` bearer is checked with `ci::token::reaches`,
  then `ci::job_grant`; only upload-pack of the grant's repository, before its deadline. A
  fetch is read whole (`git::upload_request`, 64 KiB at most, v0 and v2) and every `want`
  must pass `ci::token::allows_fetch`; `want-ref`, v2 commands other than `fetch` and
  `ls-refs`, compressed bodies and every push are refused (`git_proxy::job_fetch_allowed`,
  unit-tested). Audited as `ci job <job> (ci)`.
- **Reading a file at a commit.** `pack::objects` (every object, deltas resolved within the
  pack), `pack::tree_entries`, `pack::commit_tree` and `pack::read_path` in core;
  `git::read_file(remote, commit, path)` fetches the commit alone, one commit deep; the
  capability-level `git_proxy::read_file(account, namespace, repo, commit, path)` mints and
  revokes the read token around it, for the CI scheduler's `ProxyFiles`. Tested on a pack
  `git pack-objects` made with deltas. The CI plan's note that `pack.rs` refuses deltas is out
  of date: it resolves deltas whose base is in the pack, and a fetched pack is complete.
- **The cf_proxy allowlist**, re-recorded with `wrangler.toml`'s migration v2 and its
  `pigeon-ci-events` producer, both deploy routes: one new call,
  `GET /accounts/{a}/queues?name=<queue>&page=1` (wrangler's `ensureQueuesExistByConfig`),
  with Queues Read. It must name a queue in `CfProxy::queues`. No other call changed.
- **Fixed: refused pushes showed no reason** (found live: git printed only "the remote end
  hung up unexpectedly"). Git 2.29 and later ask for `report-status-v2`, and `git::refusal`
  only built the report for `report-status`, so git got the side-band message and no report.
  Both now get the same `unpack ok` / `ng <ref> <reason>` report. Reproduced and confirmed
  fixed with git 2.43 against a local stand-in (`git http-backend` behind a server running
  the proxy's own checks, `examples/push_check.rs`): every refusal now prints
  `! [remote rejected] <ref> (<reason>)`. The proxy also now reads what git is still sending
  (16 MiB at most) before it answers a refused push, so git reads the refusal rather than a
  closed connection.
- **The fast-forward check, tested against real git** (the live force push was blocked):
  `not_forward` now takes the upstream fetch as a parameter, so a unit test and the stand-in
  run the very same check with upstream history from `git pack-objects`. Accepted: new commits
  on top of main; main moved to a `claude/` tip already pushed (empty pack). Refused, main
  unchanged: a sibling commit; main moved back to its parent; main moved to a `claude/`
  branch that diverged from it.
- Only checkable live: whether Artifacts serves a `want` of a commit that is not a branch
  tip (`allowReachableSHA1InWant`); both the runner's fetch and `read_file` rely on it.
  Whether uploading a Worker with a queue producer binding needs more than Workers Scripts
  Write.

- **Fixed live: `read_file` failed with "the pack is malformed: bytes left after the
  objects".** Artifacts' upload-pack ("gitty/1.0") ends every answer with a flush (`0000`)
  after the pack's SHA-1 trailer, shallow fetch or not. `git::fetch` kept those 4 bytes as
  part of the pack. It now drops a trailing flush when the pack's trailer is the SHA-1 of what
  precedes it (`git::fetched`, pure, tested on a captured Artifacts answer). This also means
  the mirror no longer pushes those 4 bytes after its pack to GitHub. Checked against a
  captured live depth-1 answer for `c5cdf86`: 95 objects, `.pigeon/ci.toml` read. Artifacts
  advertises neither `ofs-delta` nor `filter`, so the fast-forward check's upstream history
  comes without the tree filter (whole, which is fine at this size).

## Phase 6: container deploys through /cf/

`wrangler.toml` gained a `[[containers]]` block (class `CiRunner`, image
`registry.cloudflare.com/<account>/pigeon-ci-base@sha256:…`). Recorded against the stand-in
with wrangler 4.166.0, first deploy (no application yet) and later ones (application exists),
and read in wrangler's source:

- A Worker with containers always deploys by the `PUT` upload (wrangler's
  `canUseNewVersionsDeploymentsApi` is false with containers). The upload's metadata carries
  the container; the proxy doesn't inspect upload bodies, so nothing changes there.
- `POST /accounts/{a}/containers/image-preparations` with `{"image": …}`, polled until ready
  (every 2 s, up to 15 min): allowed only for an image in this account's registry.
- `GET …/workers/scripts/{s}/versions/{version}`: the namespace id of each bound Durable
  Object class comes from the uploaded version.
- `GET /accounts/{a}/containers/applications/{namespace}`; on 404 (the first deploy)
  `POST /accounts/{a}/containers/applications` with `{name, scheduling_policy:
  "durable_object", durable_objects: {namespace_id}}`. `PATCH …/applications/{namespace}`
  only when the config's settings differ from the application's (`updateExisting`); allowed,
  with only settings fields.
- Not called on `wrangler deploy` here, so not allowed: `GET …/durable_objects/namespaces`
  (only for unbound container classes, and `CiRunner` is bound) and `GET …/containers/me`
  (only without an account id).
- `{namespace}` must be one of the Worker's container classes' namespaces, which the proxy
  looks up itself (`GET …/durable_objects/namespaces?per_page=1000`, its own token, Workers
  Scripts Read) before an application call; the create body's `namespace_id` must be one too.
- Permission names `Workers Containers Read` / `Workers Containers Write` are as the CI
  session gave them; if Cloudflare names them otherwise, minting fails naming the similar
  groups, and the constants in `cf_proxy.rs` change.

## Risks

- **Accepted risk (the person's decision, during phase 4): a proxy credential is effectively
  as powerful as the Cloudflare account.** Whoever holds
  one can upload and deploy `pigeon-platform` through `/cf/`, and `pigeon-platform` holds the
  parent token, so new code could use that token for anything. Until that changes: keep
  credential lifetimes short, revoke those not in use, and watch the `cf` audit lines. A
  possible mitigation: the proxy holds the final `POST …/deployments` (and the `PUT` upload,
  which deploys at once) until a person approves it with a passkey. The person chose to
  leave it as it is for now: the mitigation is recorded here and not built.
- A newer wrangler may add a call to `wrangler deploy`; it fails closed, naming the call, and
  needs an allowlist entry after review.
- The push rules rely on Artifacts' receive-pack checking each update's old value, so a race
  between the fast-forward check and the push fails rather than rewinds `main`.

## What can only be checked live

- That the session's network proxy adds the secret to git and wrangler requests, and what it
  does to the MCP connector's own header (phase 1 open question).
- Artifacts' token activation delay, and that a git token can be revoked mid-lifetime.
- Workers' streaming of request bodies to `fetch` for large pushes and clones.
- Wrangler's real call list against the real API (a call missing from the allowlist shows as
  a refusal in the logs and in wrangler's output).

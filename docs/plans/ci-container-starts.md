# CI container starts: runners lost before they report

Status: fix landed in `src/proxies`, `src/proxy`, `src/core` and `src/ci` (2026-11-27). No change
under `ci/` is needed.

## The symptom

Containers jobs end in `error` with "retrying on containers, a fresh runner: the container
stopped before reporting", `started_at` null and no exit code, often on both attempts (landing
TE7FsrQ4tG67ep3X: jobs xJnRWDIWLvaXV4-sc3pTzt and _HYkl1qNUoBfokv_v_d50r). It was the largest
cause of slow and failed landings.

## Evidence (the 200 most recent jobs, 2026-11-26 16:40 to 2026-11-27 07:10 UTC)

- 195 containers jobs (all `linux-amd64-medium`, `standard-4`). 155 succeeded, 4 failed, 34
  `error`; 16 of the successes needed a second attempt.
- 84 lost attempts out of about 245: **34% of attempts**. Every one is "stopped before
  reporting", none carries an exit code.
- After one loss the immediate retry (0.6 s later, a new runner object and container) was lost
  again 34 times out of 50 (**68%**): the losses cluster in windows of minutes (e.g. 23:31 to
  23:48, 03:46 to 03:52, 04:30 to 04:38), across every job (`test` 21/74, `deploy-model-gateway`
  7/13, `deploy-proxy` 5/15, `deploy-dotest-a` 5/11, ...).
- Every lost attempt is declared **60 s after dispatch** (59.97 to 60.19 s): the runner object's
  first watch alarm (`WATCH_MS`). A runner that starts reports `running` 2 to 12 s after
  dispatch (159 starts, median 7 s), and that report comes only after its `git fetch` through
  the git proxy (ci/runner/entrypoint.sh). So a lost attempt's container ended before it had
  fetched the commit, within a minute.
- No correlation with concurrency (several losses with no other job running), with the hour,
  or strongly with core deploys.
- `pigeon-proxy` (which serves `proxy.pigeon.dev/git/`) logged, in 24 h, 87
  `git proxy: /accounts/…/artifacts/namespaces?… 401 Authentication error` and 31 mint
  failures (`…/tokens: 401`). In the hour 06:10 to
  07:10 it logged **6** of the namespaces 401s; that hour had **6** lost attempts (06:35, 06:36,
  07:01, 07:02, 07:03, 07:04). The ci-gateway and core logged nothing for them.

## Root cause

**1. The proxy Worker kept using a Cloudflare token core had revoked (high confidence).**
The proxy Worker never holds the parent token: for git it asks core for a scoped credential
(`purpose = artifacts`) and keeps it per isolate until a minute before it expires
(`src/proxies/src/cache.rs`, fifteen-minute lease). Core leases these from its token ledger,
which holds token values in memory only: when the `TokenLedger` object restarts (a core deploy,
or an eviction), every lease it held is an orphan, revoked at the next lease or sweep
(`src/capabilities/src/ledger.rs`, `restart_orphans`). An isolate of the proxy Worker still
holding the revoked token gets `401 Authentication error` on its first Artifacts call
(`namespaces?…`, the repository lookup) on every request, after `call_fresh`'s 15 s of retries
meant for a token not yet active, until its own copy would have expired: up to about fourteen
minutes. Nothing dropped it on a 401: core's own path (`with_cached_token`) re-mints on a 401,
but a `bearer` account was used once as it was.

Each CI runner's `git fetch` goes through that proxy with its job token, so in such a window
the fetch is answered 502, the runner exits 3 before it reports, and the job is lost. The retry
lands on the same few proxy isolates and is lost again, which is why losses come in clusters
and why an immediate retry fails 68% of the time.

**2. Lost runners never said why (certain).** The runner object called `monitor()` only from its
alarm, after `running` read false; Cloudflare refuses `monitor()` on a container that is no
longer running, so the exit code (3 here) and Cloudflare's own reasons were dropped, and every
loss read the same. This hid cause 1.

Ruled out or unlikely: an instance limit (the gateway's own limit; losses with nothing else
running), cold image pulls (a slow start would still be running at 60 s, not stopped), the job
timeout or the inactivity timeout (set before the first alarm; jobs of minutes run), the runner
script itself (unchanged image digest across successes and losses).

## The fix

- `src/proxies`: `with_renewal` / `with_host_account`: when a call on an account holding a
  scoped token fails with 401, the host forgets that token (`Host::forget`, a no-op for core,
  which mints per request) and the work runs once more with the account it answers next. The
  git proxy mints its git token through it. `Credentials::forget` drops the kept credential only
  while it is still the refused one.
- `src/proxy`: the proxy Worker's host forgets the refused credential from its per-isolate cache.
- `src/core/src/containers.rs`: `watch`, which calls `monitor()` at once and settles when the
  container ends, including one that fails to start; `monitor` is built on it.
- `src/ci/src/containers.rs`: the runner object watches its container right after the start,
  records the exit code or Cloudflare's reason (a new `why` column), and sets its alarm a second
  later, so a runner lost at the start is reported and retried within seconds instead of at the
  60 s watch. The lost runner's detail says how it ended (`lost_why`): "exited with code 3 before
  reporting (the runner could not fetch the commit through the git proxy)", or "stopped before
  reporting: <Cloudflare's reason>".
- Smoke: `proxy::proxy_worker_renews_a_refused_scoped_credential`,
  `ci::ci_lost_runner_says_how_its_container_ended`.

## Not done, and what to watch

- The deploy proxy (`/cf/`, `Purpose::Deploy`) keeps its credential the same way and has the
  same exposure after a ledger restart; it should take `with_host_account` too.
- The ledger could keep handing out a lease's value across a restart (it cannot: values are in
  memory by design), or revoke orphans only once they expire. Renewing on 401 makes either
  unnecessary for the proxy.
- After this lands, a lost runner's detail names its exit code or reason. If losses continue
  with a reason other than exit 3, that reason is the next lead.

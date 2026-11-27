# Lander fixes: rate limits, approvals across a rebase, removed Workers

Status: **built (2026-11-27)** on `claude/lander-fixes`, one landing. Code in
`src/capabilities/src/land.rs` and `src/capabilities/src/land/` (`lanes.rs`, `state.rs`,
`merge.rs`); scenarios in `tests/smoke/land.rs`. Nothing under `ci/`, and neither the policy nor
the Claude settings, changes.

## 1. A Cloudflare rate limit blames no landing

**Seen.** A deploy hit Cloudflare's rate limit (API code 10429 on wrangler's pre-upload reads, or
HTTP 429 from the R2 REST API or the global limit of 1,200 requests per 5 minutes). The Lander
took the failed job as the change's fault: it opened an incident, rolled the Worker back and
bisected, and blamed the landing's commit.

**What the Lander sees.** `ci/deploy/deploy.sh` already retries a 10429 on a pre-upload read
five times with its own backoff, then exits 1 with wrangler's output (`[code: 10429]`) in the
job's output. The Lander reads the job's state, exit code and the last 2000 characters of its
output (`job_ended_code`), and `lanes::environment_fault` already classes some failures as the
environment (a lost runner, Cloudflare 10007, a 5xx, the proxy unreachable): those blame no
landing and are deployed once more. A rate limit was not one of them.

**Change.**
- `lanes::rate_limited(tail)`: the job's last deploy attempt (the output after its last
  `== wrangler deploy` header, or all of it when there is none) names a rate limit: Cloudflare
  `code: 10429`, a 429 status (`HTTP 429`, `status 429`, `status code: 429`, ...), `Too Many
  Requests`, or `rate limited`. Only the last attempt counts, so a deploy that was rate limited
  once and then failed for its code is still the code's fault. `environment_fault` answers it
  first, as `Cloudflare rate limit (...)`.
- A rate-limited deploy (lane, rollback, bisect or restore) is deployed again at the same commit
  after a doubling backoff (2, 4, 8, 16 minutes; `lanes::RATE_LIMIT_REDEPLOYS` = 4), not at
  once and not just once. Two new deploy columns: `rate_retries` and `not_before`; the job is
  submitted only after `not_before`. The waiting deploy keeps its lane's slot, so fewer deploys
  run against the limit meanwhile.
- Retries used up: the deploy ends `error` with an `environment:` detail that names the rate
  limit, so the landing ends `error` ("not this landing's fault; land again"), never
  `deploy_failed`. An incident is recorded in the new state **`rate_limited`**: no rollback, no
  bisect, nobody guilty, closed (`covered`) by the Worker's next green lane deploy, and listed in
  `incidents_to_see` meanwhile. An incident's own redeploy that runs out of retries ends that
  incident `rate_limited` too (other environment faults still end it `environment`).

## 2. An approval holds across the Lander's own rebase when the approved change is unchanged

**Seen.** A landing that waited for the person's passkey (a change to `.pigeon/policy.toml` or
`.claude/settings.json`, or the setting) was approved; the Lander then rebased it onto a newer
main, and the person had to approve again. Two paths ended the landing: a rebase conflict
(`rebase_refused`), and a lost test runner on the retest (`test_failed`), each needing a new
landing and so a new approval. And the opposite gap: on a retest after a rebase the old
approval was kept with no look at what changed, though the Lander's text merge can change a
guarded file (`ci/workers.toml` and `.pigeon/ci.toml` are text-merged).

**Change.**
- The approval is bound to the **gated patch**: for every file the landing changes under a
  guarded path (`state::GUARDED`), its path, mode and blob id before (on the landing's base) and
  after (at its commit), sorted; `state::gated_digest` is its SHA-256. `check` records it as
  `gate_digest`; `approve` copies it to `approved_digest` in the same write.
- When the Lander rebases a landing that has an approval, it computes the rebased commit's gated
  patch against the new main (`merge::gated_patch`). The approval is kept only when
  `state::approval_holds`: an approval exists, its digest was recorded, and the digests are
  equal. Otherwise (main changed a guarded file the landing changed too, so the text merge or
  main's identical change altered the patch; an approval from before this change, with no digest)
  the approval is voided: `approved_by` and `approved_digest` are cleared, the landing is tested
  again at the rebased commit and then waits for the person as before. Fails closed: an error
  reading the trees voids. A rebase conflict ends the landing as today.
- The step's detail says which: "the approval holds: its gated changes are unchanged" or "the
  approval is void: its gated changes differ after the rebase".
- A `test` job whose runner was lost (job state `error`, "the container stopped before
  reporting") is submitted once more for the same commit instead of ending `test_failed`
  (`landings.test_retries`, at most `state::TEST_RETRIES` = 1). Same landing, same commit, so
  the approval stays; the test must still pass.

An approval asked for only by the setting (every landing waits) has an empty gated patch, so it
holds across a rebase as it did before. Never kept: an approval for other content. The digest covers every guarded file the landing
changes, with its mode, before and after, so the same digest means the same change to the gated
paths.

## 3. Workers removed from the repository are skipped

**Seen.** When the DO rehearsal's Workers (`pigeon-dotest-a`, `-b`) were removed, the removing
landing changed `Cargo.toml` (shared), so it was classified with main's map, which still listed
them, and the Lander tried to deploy both at a commit without their source.

**Change.** A Worker is **removed at a commit** when the map at that commit
(`ci/workers.toml`) parses and has no entry of that name, and its directory is not in the
commit's tree (`lanes::removed_at`). Both must hold: a Worker whose source is gone while its
entry stays is deployed, and its failure is the change's.
- `check`: Workers of main's map removed at the landing's commit are dropped from the landing's
  Workers, and the checked detail says `skipped <names>: removed at this commit`.
- A lane deploy of a Worker removed at its commit (a landing checked before main removed it, then
  rebased) submits no job: it ends `succeeded` with the detail `skipped: ...`, which covers the
  landings it carries, and the landing's `deployed:` detail lists it as skipped.

## Tests

`tests/smoke/land.rs`: `core_land_rate_limits_blame_no_landing` (classification, last attempt
only, backoff, steps), `land_approval_holds_across_a_rebase_only_for_the_same_gated_change`
(real git repos: kept when main moved elsewhere; voided when main changed the same guarded file,
when the mode changed, when no digest was recorded; the test-retry rule), and
`land_skips_workers_removed_at_the_commit`.

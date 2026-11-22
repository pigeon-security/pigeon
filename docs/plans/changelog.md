# Changelog of main: by date, one bullet per commit

Status: **designed; built on a branch, not landed** (planner run `fHVIjaxK6nJZ`).

## Goal

A changelog of `main`, grouped by date (UTC, newest first), one bullet per commit: the short
subject, the short SHA and the landing id when one is known. It covers every commit on main up
to now (backfill) and every commit after.

Hard lines from the person:
- It runs on its own and never blocks or slows a landing, a deploy, CI or the merge lock.
- It never commits to main (that would start landings and loop).
- A failure of the changelog is only logged. It never shows up as a landing or deploy failure.

## Design

### Where it runs

The `Changelog` capability in core (`pigeon-platform`), next to the GitHub mirror, and built the
same way. Artifacts push events already reach core through the `pigeon-source-events` queue; the
queue consumer (`src/platform/src/lib.rs`, `queue`) calls `on_events` on each capability that
`wants` one of the batch. The changelog wants a push of `refs/heads/main` or `refs/notes/land`
on pigeon's repository and nothing else.

Why this cannot hold anything up:
- The Lander, CI and deploys never wait for the queue. The queue consumer runs after the push
  has been answered, in its own invocation.
- The changelog is the last capability in `settings::capabilities()`, so in a batch it runs
  after the mirror and core's other capabilities (bound Workers' capabilities may come after it).
- Its `on_events` always answers `Ok`. A failure is logged with `console_error!` and answered as
  a value. The consumer fails a batch on any `Err`, which would make the queue retry the mirror
  too; the changelog never causes that.
- It writes nothing to git. Its output is R2 objects, which raise no push events, so it cannot
  loop.

### Where it is kept

An R2 bucket in pigeon's own account, `pigeon-changelog`, written and read through Cloudflare's
REST API with `pigeon_core::r2` (a scoped token opened from the account's parent token, as the
site deploys do). It needs no new binding. The bucket is made on the first run
(`r2::ensure_bucket`, safe to repeat).

- `main/CHANGELOG.md`: the rendered changelog.
- `main/state.json`: `{ "version": 1, "main": "<last processed main sha>", "entries": [...] }`,
  each entry `{ "sha", "unix", "subject" }`, newest first. Landing ids are not stored here: they
  are joined in at each render, so a note that arrives after its commit is picked up.

### A run

1. Read `main/state.json`. None, a bad one, or a full rebuild asked for: start from nothing.
2. Open an Artifacts read token for the repository (revoked at the end). Read the refs: main's
   tip and `refs/notes/land`'s tip.
3. If main's tip is the state's `main`, no commits are fetched. Otherwise fetch commits only
   (`filter tree:0`) wanting main's tip, having the state's `main` when there is one. Walk first
   parents from the tip down to the state's `main` (exclusive) or the root. If the walk never
   meets the state's `main` (main was rewritten, which policy forbids), fall back to a full
   rebuild in the same run.
4. Fetch the notes commit at `refs/notes/land` one deep, with blobs. Its tree is flat: each entry
   is named by a landed commit's id, and its blob starts `landing <id> `. Map commit id to
   landing id. A commit has a landing id only when a note names it exactly; the Lander notes the
   commit that landed on main (the rebased one when it rebased).
5. Render, then write `main/CHANGELOG.md` and `main/state.json`.

Main has 271 commits today and no merges, so a full rebuild fetches a few hundred commit objects.

### The format

An example (the landing id is made up):

```markdown
# Changelog of main

## 2026-11-22

- apple: bind APPLE_SEAL_KEY; Adopt reads capabilities with the bundle id (`b40f820`, landing `L0example0000000`)
- ci: phase 3, the hosted macOS queues for iOS (`126b615`)
```

- Date: the commit's committer time, in UTC (when it reached main for a rebased landing).
- Days newest first. Within a day, main's order, newest first.
- Subject: the message's first line, trimmed, cut at 100 characters with `…`; `(no subject)`
  when empty. Backticks in it are kept; nothing is escaped beyond that.
- SHA: seven characters.

### Tools

- `changelog {days?}`: read-only. Answers the stored changelog, or its newest `days` date
  sections, with the main commit it covers. Answers plainly when no run has written it yet.
- `changelog_update {full?}`: runs the job now: incremental, or a full rebuild with
  `full: true`. Answers what it did (commits added, total, the main commit). A failure here is the
  tool's error, since a person asked for it.

### Code

- `pigeon_core::changelog`: pure functions (no I/O), each covered by the smoke suite: the entries
  from a set of commit objects between two commits, the landing id in a note, the UTC date of a
  time, the render, the state's JSON.
- `pigeon_capabilities::changelog`: the capability: `wants`, `on_events`, the two tools, and the
  I/O through `pigeon_core::git` and `pigeon_core::r2`.
- `src/platform/src/settings.rs`: the capability, last in the list.
- Smoke: a `core_changelog_*` scenario on a local repository: backfill, an incremental run equal
  to a full one, grouping by UTC day with days newest first, short SHAs, a landing id from a note,
  and a walk that misses the old tip.

## What it does not do, and follow-ups for the person

- **No new binding, cron, lane or gate change.** The paths are inside the core lane already.
- **R2 on pigeon's account.** If R2 is not enabled on the account, or the parent token cannot
  open an R2 token there, each run logs the error and changes nothing. The person would enable
  R2 on the account. The run never uses a linked account.
- **Missed events.** If a push event is lost, the next push to main or to the notes brings the
  changelog up to date, since a run always starts from the last processed commit. A catch-up on
  the existing minute cron would close the gap; it costs two token mints a minute, so it is left
  for the person to decide.
- **Two runs at once** (two batches in flight) both write a correct changelog; the later write
  wins, and the next run corrects an older one. R2's REST API has no conditional write here.
- **A console page or a public page** (such as `pigeon.dev/changelog`) is not built; the tool is
  the reader for now.
- **An alternative store**, if R2 is not wanted: the same objects in git under a ref of their
  own (say `refs/notes/changelog`), as the landing and CI notes are, which the mirror does not
  copy. That would make pigeon commits, which follow docs/plans/commit-attribution.md.

## Log

- Built by the planner run itself: three executor sandboxes in a row failed to start
  (`pigeon-sandboxd` `prepare`: `git clone` found `/work` not empty).
- Built as designed: `pigeon_core::changelog` (walk, landing_of, date_utc, subject, render,
  newest_days, State), `pigeon_capabilities::changelog` (wants pushes of main and
  `refs/notes/land`; `on_events` only logs failures; tools `changelog` and `changelog_update`),
  registered last among core's own capabilities with bucket `pigeon-changelog`. Smoke scenario
  `core_changelog_from_history`.
- Not yet run live: nothing is landed. The first run after landing (or `changelog_update`)
  makes the bucket and backfills main.

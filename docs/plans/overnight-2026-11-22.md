# Overnight, 2026-11-22: AI agent runs on the person's seven requests

Status: done. Four planners ran on the model gateway (pigeon-ai-agents), with $20, at most 3
executors and 180 minutes each, under an overall cap of $150 and the rule "land only reviewed,
off-by-default code plus docs; changes to existing code wait for the person; never ci/,
.pigeon/, policy or the gate". The reviewer (the AI agents planner session) read every code
branch before landing it, because the planners could not (see "What broke" below).

Spend, from the runs' own accounting: about **$66** of the $150 cap.

| Run | Role | Workstream | Cost | Ended |
|---|---|---|---|---|
| XE8RUxKJDU6J | planner | #1 clean-up | $13.09 | done |
| 6xFkoSnmSyJU | planner | #2 storage providers | $18.41 | done |
| AFc1coPrsJTT | planner | #4–#7 data platform | $19.51 | out of budget after pushing all four plans; no closing report |
| 37wDmGr46XoB | planner | #3 iOS | $13.35 | done |
| 7 executors | executor | the branches below | $2.07 together | 7 done, 4 failed before starting (sandbox bug 2) |

## What landed

| Landing | What | Kind |
|---|---|---|
| NcDouOPwUOzOJVkh | Core's `dispatch` reaches the bound Workers' tools (it saw core's own only, so no AI agent run could submit a turn) | fix, core |
| wzs_-gK2ixhD4pXc | A refused turn shows in the run's step | fix, ai-agents |
| XVbZ-5vieiZ57jgs | A run's `run` commands find cargo and node (sandbox bug 3, worked around) | fix, ai-agents |
| C_U8Ct-I3M556Z1a | The first change written by an AI agent run (one log line) | docs |
| 0N3Mk0FxCNCJMpMb | [cleanup.md](cleanup.md), [storage-providers.md](storage-providers.md), README statuses | docs |
| FSRipNVPcpASWZka | The `storage` capability: read-only S3-compatible buckets (Scaleway, DigitalOcean read-only, Backblaze B2), several accounts per provider, no account declared, so it only explains setup | new, off |
| d8UDYlp7loSxTCda | [email.md](email.md), [ingest.md](ingest.md), [data-worker.md](data-worker.md), [mail-connectors.md](mail-connectors.md), [ios.md](ios.md) | docs |
| ukxxA9rH7P7JyYqe | `pigeon-email` (message schema and MIME parser, a library only); the iOS Buildkite pipelines generator (unused); `ios/` SwiftUI skeleton with tokens generated from the web design system and a drift check | new, off |

Review notes on the code that landed:
- `storage`: SigV4 matches AWS's published vectors; the URL and the signature use the same
  encoding; the secret never reaches an error or `Debug`; DigitalOcean accounts are refused unless
  read-only. The reviewer added one check: a caller's bucket name must be a plain bucket name
  before it goes into the request path.
- `pigeon-email`: no `unwrap`, `expect` or `panic` on untrusted mail. New dependency
  `mail-parser` 1.0.1 (Stalwart Labs) and its three small dependencies.
- iOS: OAuth is dynamic client registration with PKCE (S256) and a state check, tokens in the
  keychain, no embedded secret. **The Swift has never been compiled** (Linux only); expect small
  fixes on a Mac.

## Held for you (they change existing code)

1. **`claude/ai-agent-Fuzcl9MugNWk`** (`7607f74`, CI passed): core's choice of capabilities for
   `dispatch` moved into testable functions in `split.rs`, with a smoke scenario that would have
   caught tonight's dispatch bug. Read in full: no behaviour change; the clash log line is
   compiled for wasm only (the macro aborts natively). **Recommended.**
2. **`claude/ai-agent-M933ZUIdJ1YQ`** (`991e147`, CI passed): the Lander marks `unisolated` or
   `stuck` incidents `covered` when a later passing deploy of the same Worker includes what they
   carried, and `land_status` stops showing incidents for Workers no longer in the map. It changes
   the Lander, so read `close_covered` in `land.rs` before landing. Today's moot incidents (core 1,
   2, 3, 5; ai-agents 7, 8) close at the first passing lane deploy after it.

Both touch `tests/smoke.rs`; rebase the second onto main.

## Questions the runs answered for themselves (5-minute timeout)

- **Where do the 4–5 TB live today, and is "the Backblaze bucket" existing or new?** The data
  planner assumed the conventional answer (see [ingest.md](ingest.md), decision 11). It decides
  whether files are de-duplicated before upload (a local uploader hashing first, saving about
  4 TB of transfer) or after a bulk copy into a staging R2 bucket.
- The other choices each planner made without asking are in its plan's Decisions.

## What broke in the AI agent runs (for the next round)

1. **Agents cannot `git fetch` other branches.** pigeon-sandboxd strips the git credential from
   the run's own commands (on purpose: an agent never holds a secret), so a planner cannot read
   its executors' branches (403). Fix: a sandboxd `fetch` op and an agent tool for it, so the
   credential stays in sandboxd. Needs a sandbox image rebuild.
2. **An executor started from a branch other than main failed to start** ("git clone failed:
   destination path '/work' already exists"): a retry after a first failed clone whose error is
   lost. Fix in sandboxd: empty `/work` before cloning, and keep the first error. Image rebuild.
3. **`bash -lc` resets PATH** (Debian's /etc/profile), dropping cargo and node. Worked around in
   the Worker; fix in sandboxd with `bash -c` at the same image rebuild.
4. **A planner can spend its whole budget before reporting** (the data planner). Proposal: at 90%
   of its cost budget a run gets one last turn whose only tool is `report`.

## Follow-ups for you

### First (unblocks the rest)

1. Rebuild and push the sandbox image with the sandboxd fixes above, then pin its digest (as
   for v1), or ask for those fixes to be built first.
2. Review and land the two held branches.
3. Approve the gate changes in [cleanup.md](cleanup.md) (follow-ups 1 and 2): a `wrangler types`
   check of every Worker's config in the gate (it would have caught tonight's `max_instances`),
   and dropping the platform's duplicate check.
4. Add lanes for the new paths in `ci/workers.toml` so a change there does not count as shared
   and redeploy every Worker: `ios/*` (no Worker; checks only) and `src/email/*` (whichever
   Worker will use it).

### #2 Storage providers ([storage-providers.md](storage-providers.md), Follow-ups)

Choose account names; create least-privilege keys (DigitalOcean read-only; Scaleway and B2
read-only for now); ask for the accounts to be declared; set each secret on the console with
your passkey. Writes (phase 3) need the write tools added to your policy's passkey rule.

### #3 iOS ([ios.md](ios.md), Follow-ups)

Apple Developer team; confirm the bundle id `dev.pigeon.platform`; an App Store Connect API key;
the app record and TestFlight; an APNs key (phase 6); a macOS Buildkite agent; the `ios-check`
and `ios-archive` jobs in `.pigeon/ci.toml`; whether Swift tests may exist outside
`tests/smoke.rs`; open the skeleton in Xcode and fix what does not compile.

### #4–#7 Data platform ([email.md](email.md), [ingest.md](ingest.md), [data-worker.md](data-worker.md), [mail-connectors.md](mail-connectors.md))

- Email: the capture address per domain (default `in@`), a throwaway test subdomain, the
  `data.email` record per instance on the console.
- Ingest: where the raw data lives; the B2 bucket and key; each instance's data jurisdiction;
  Infrequent Access after 30 days; Workers Paid; an R2 token for the Pipelines sinks; about 200
  of your own items and 10 real questions for phase 0.
- willowgraysen: onboard the account (with the linked-accounts work), Workers Paid, whether data
  tools need your passkey per call, the data Worker's build job (a CI change).
- Mail: a Google Cloud project with the Gmail API and OAuth; a Fastmail read-only API token; an
  iCloud app-specific password; Proton Mail Bridge on the uploading machine; mailboxes to skip.

## Log

- 10:18 UTC: four planners queued; 10:26 the data planner restarted with the person's additions
  (entity resolution, classification at ingest, Iceberg as the record and Vectorize as a derived
  index, hybrid queries).
- 10:20 the first executor run (`jZ0wwsfvZBos`) finished: 9 turns, $0.055, CI passed.
- 11:10–12:00: planners finished; landings as above. Every landing deployed green on all six
  Workers; nothing was rolled back.

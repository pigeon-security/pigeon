# Data ingest names

The follow-up to [record-reshape.md](record-reshape.md): the ingest files move off the hidden
`storage` alias, the alias goes, and the ingest tools take the `data_<child>_<verb>` names the
other `data.*` capabilities have.

## What changes

1. **The `storage` alias is removed.** `src/capabilities/src/ingest.rs` and
   `src/capabilities/src/ingest/hosts.rs` use `crate::data_read` (`Account`, `Provider`,
   `find`, `bucket_allowed`); `pigeon_capabilities::storage` is deleted from `lib.rs`. The two
   messages of `data_ingest_plan` that named `storage_accounts()` name `data_read_accounts()`,
   the function in `src/platform/src/settings.rs` that declares the accounts. Nothing else used
   the alias.
2. **The tools are renamed outright**, as the person decided for `data_email_*` and
   `data_read_*` (no aliases):

   | Old | New |
   |---|---|
   | `ingest_plan` | `data_ingest_plan` |
   | `ingest_start` | `data_ingest_start` |
   | `ingest_status` | `data_ingest_status` |
   | `ingest_report` | `data_ingest_report` |
   | `ingest_stop` | `data_ingest_stop` |

   These five are every tool the capability serves (platform `tools()` and the instance
   registrations `DATA_TOOLS`); `src/cloud`, the data Worker and the ingest runner call none
   of them by name (the runner talks to R2 and the data Worker's protocol). The renames reach
   the approver's plan list (`PLAN_TOOLS`, `follow_up`), the console's read list and instance
   ops (`ops.rs`), its shell's known ops and Ingest › Runs view, its recorded fixture
   (`fixtures/data_ingest_report.json`), the iOS Ingest screen, the smoke tests and the
   current docs (`ingest.md`, `instance-live-fixtures.md`, `approvals-auto-run.md`). Older
   plans that record what was true then (`mcp-tool-audit.md`, `console-redesign.md`,
   `ios-app-redesign.md`) keep the old names.
3. **The `data-ingest` component is 2.0.0** with a breaking changelog entry naming the new
   tools.

## What stays

- **The approval kind `ingest_run`** (`ingest::KIND`). A run's plan digest is
  `payload_digest(kind, run, payload)`, so renaming the kind would orphan any plan waiting for
  the passkey. It is a kind, not a tool name; `follow_up` maps it to `data_ingest_start` at run
  time, so a plan approved before the deploy runs under the new name.
- **Stored run state**: the Directory keys `ingest:run:<run>` and `ingest:runs:<instance>`,
  and the R2 objects (`manifest::job_key`, `status_key`, `report_key`). None is keyed by a tool
  name.
- **The console op `app_ingest_runs`** and the console/iOS area `ingest`. They are the
  console's own read and section, not MCP tools; keeping them keeps the released iOS app's run
  list working (only its report view calls a renamed tool).
- **The Rust module `pigeon_capabilities::ingest`** (and the `pigeon_ingest` crate). Renaming
  it to `data_ingest` is cosmetic and would turn every in-flight Codex ingest branch
  (`codex/spaces-ingest-managed`, `codex/backblaze-ingest-runtime-reshaped`, …) into a
  rename conflict for the Lander. The capability id (`data-ingest`), the record field
  (`data.ingest`) and the tool names already carry the new name. Revisit once Codex's ingest
  work settles.

## Approvals under an old name

None of the five tools is named by `.pigeon/policy.toml` (checked: only
`data_email_forward_set` and the break-glass/admin tools are passkey-gated), so no policy edit
and no fail-closed gate is needed, and no `tool_call` approval can have been asked for under an
old ingest name. `approvals_list` showed nothing pending before the change. Still,
`approvals::RENAMED` lists every outright rename (these five and the record-reshape's eleven),
and `follow_up` refuses a `tool_call` approval whose subject is an old name, saying which name
to call ("`ingest_stop` was renamed `data_ingest_stop` after approval … was asked for; nothing
ran"), instead of dispatching a tool that no longer exists. Smoke: `approvals_auto_run`.

## Runs in flight

Before the change `ingest_status` (list form, which only reads) answered no runs for
willowgraysen, pigeon.dev, noisypigeon and sandbox. Codex's DigitalOcean, Scaleway and
Backblaze jobs run through the DigitalOcean/cloud tools and the data Worker, which this does
not rename. After the deploy, `data_ingest_status` is checked again for the same instances.

## iOS

The released app (TestFlight build 49) calls `ingest_report` for a finished run's report; after
this lands that view fails with "unknown tool" until a new build ships. `ios/` is updated
(`IngestScreen.swift`), and the `ios-check` CI job is run at the landed commit. A new iOS
release needs the person's go-ahead.

## Phases

| Phase | What | Branch | State |
|---|---|---|---|
| 1 | Plan (this doc) | `claude/data-ingest-names` | landed with phase 2 |
| 2 | Ingest on `data_read`, alias removed, `data_ingest_*` renames, 2.0.0, console, iOS, tests, docs | `claude/data-ingest-names` | landed (`9zclms7_4QCUOuwY`, `c384945`) |
| 3 | Verify live (`data_ingest_status`), `ios-check` at the landed commit | — | done |

## Log

- 2026-11-27: landed as `c384945` (landing `9zclms7_4QCUOuwY`; core and data-worker deployed).
  Live: `data_ingest_status` answers for willowgraysen, pigeon.dev, noisypigeon and sandbox
  (no runs, as before); `ingest_status` answers "unknown tool"; `capabilities_versions` shows
  `data-ingest` 2.0.0. `ios-check` at `c384945` succeeded (Buildkite build 84). The released
  iOS app (build 49) needs a new build for its Ingest report view.

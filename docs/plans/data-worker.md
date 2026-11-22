# The data Worker: one per instance, in the instance's own account, reached through pigeon

Status: **designed** (2026-11-22). Nothing is built, deployed or enabled. Branch
`claude/ai-agent-AFc1coPrsJTT` (docs only). Instances and linked accounts belong to another
planner ([linked-accounts.md](linked-accounts.md)); this plan changes none of their code and
lists what it needs from them.

Related plans: [email.md](email.md), [ingest.md](ingest.md),
[mail-connectors.md](mail-connectors.md), [mcp-portal.md](mcp-portal.md),
[workers-split.md](workers-split.md).

## Goal

The person's words (#6): "When enabled, the instance should hold its vector database, and MCP
connectors for the data pipeline. A Worker should be created in the destination account that
facilitates this, but is reachable through MCP, granted the pigeon account has access to it. I
will be onboarding another instance, willowgraysen, which has willowgraysen.com. This will live
there."

Read as: an instance with data turned on gets, **in its own Cloudflare account**, a Worker and
the resources of ingest.md (bucket with Iceberg tables, Vectorize index, queues, D1). pigeon
reaches that Worker through its own MCP server, which is already behind the portal, and only
while pigeon's link to that account is valid. The first instance with data is `willowgraysen`
(willowgraysen.com, a new linked account); the home instance (pigeon.dev) and noisypigeon get it
for email capture (email.md).

Hard lines:
- Off by default: an instance without a `data` section has no data Worker and no data resources,
  and every data tool says how to set it up.
- Nothing is created in an instance account except through an instance plan the person sees as
  a dry run and approves with their passkey (linked-accounts.md decision 9).
- The data never leaves the instance's account except to its B2 backup and to the caller of a
  data tool.

## What is true today (read from main at `7396471`, 2026-11-22)

- **Instance records** (`src/capabilities/src/instances/record.rs`): `Instance {id, title,
  account: AccountRef (Home | Linked {account_id}), zones, repo, sites, worker,
  worker_override, version}`, with `deny_unknown_fields`. Values are data in the Directory,
  changed by `instance_propose` and the person's passkey. `worker_for` names the instance's one
  site Worker after its zone (`noisypigeon`); `bucket_for` names its site bucket
  (`noisypigeon-site`).
- **Linked accounts** (`src/capabilities/src/linked.rs`): Cloudflare OAuth, tokens sealed with
  `LINK_KEY` in the Directory; `account_for(env, host, instance)` refreshes and answers an
  `Account` with a bearer; Unlink (passkey) revokes the grant and forgets the tokens, leaving
  what pigeon made in place. Scopes granted today: `account-settings.read`, `zone.read`,
  `dns.write`, `workers-scripts.write`, `workers-routes.write`, `workers-r2.write`,
  `offline_access`.
- **Instance plans** (`instances/plan.rs`, run by `instance_plan` / `instance_apply`): steps
  `CreateBucket`, `UploadWorker`, `EnableWorkersDev`, `AttachDomain`, `AttachSubdomain`,
  `Conflict`, `Blocked`. Every resource is recorded in the instance's ledger and only ledgered
  resources are changed.
- **Worker uploads** (`src/core/src/workers.rs`): one JS module, bindings `R2` and `Text` only
  (`workers::Binding`).
- **The contract** (`src/contract`, workers-split.md): capability Workers answer `manifest`,
  `call`, `section`, `fetch`, `events`, `secret` over a Service Binding, with a caller context
  signed with `CALLER_KEY` (≤ 60 s). Core is the front door: it authenticates and runs
  `approvals::gate` before forwarding. Capability Workers ask core for credentials (`credential`
  op) and never hold the parent token or a link's tokens.
- **The portal** (mcp-portal.md): pigeon's MCP server is behind the Cloudflare MCP portal as
  server `pigeon`; tools appear as `pigeon_<tool>`; new tools appear after the deploy-time sync.

## Decisions

### 1. One data Worker per instance, separate from its site Worker

- Name `{instance}-data`: `willowgraysen-data`, `noisypigeon-data`, and `pigeon-data` for the
  home instance. Same rule style as `worker_for`; stored in the record, with an override flag.
- Separate from the site Worker because its bindings (Vectorize, AI, Queues, D1, Pipelines,
  email), its risk (personal data) and its deploy rhythm differ. A site deploy never touches
  data, and the reverse.
- Lives in the instance's account, the home instance's in pigeon's own account, made the same
  way (an instance plan), so there is one code path. It is **not** a platform Worker: it is not
  in `ci/workers.toml` and is not deployed by landings.
- **Written in Rust** (workers-rs), sharing `pigeon-email` and `pigeon-ingest` with the smoke
  suite. Bindings workers-rs lacks (Vectorize, Pipelines) are reached through `wasm_bindgen` on
  the raw binding (ingest.md phase 0).
- **Built** once per platform commit into a content-addressed artifact (the `.wasm`, its JS
  shim and a manifest with the digest), kept in pigeon's R2. An instance plan's `UploadWorker`
  step names the artifact digest, so the person approves exactly what is uploaded. Producing
  the artifact needs a CI job or deploy step: a gate change, so it is a follow-up for the person.

**Handlers:**

| Handler | Does |
| --- | --- |
| `email` | email.md decision 2: store raw, queue a reference |
| `queue` | the ingest stages (ingest.md decision 8), the backup consumer, connector sync work |
| `scheduled` | connector sync ticks (mail-connectors.md), backup reconciliation, stale-run checks |
| `fetch` | only the signed contract ops (decision 2) and the uploader's endpoints (decision 5) |

**Bindings:** R2 `DATA` (`{instance}-data`), Vectorize `INDEX`, `AI`, Queues (producers and
consumers for the five queues and their DLQs), D1 `OPS`, Pipelines streams per table, secrets
`DATA_KEY` (decision 2), `B2_KEY_ID` and `B2_KEY` (ingest.md decision 10), connector secrets
(mail-connectors.md), and a plain-text `INSTANCE` with the instance id and account id.

### 2. Its surface: signed calls only, no public routes

- No routes and no custom domain. `workers.dev` is on, because a Service Binding cannot cross
  accounts and pigeon must reach it from its own account.
- Every request to `fetch` must carry a caller context signed by core with the instance's
  **`DATA_KEY`** (HMAC-SHA256, the contract's `Signed` shape: `{user, acting_for, approval?,
  iat, exp ≤ 60 s, nonce}`, plus the instance id and account id in the signed body). Anything
  else gets 404 with no body, so the URL tells a stranger nothing.
- `DATA_KEY` is 32 random bytes, one per instance, made by core when the data section is first
  applied, kept in the Directory sealed with `LINK_KEY` (`data_key:{instance}`) and uploaded as
  a secret binding on the data Worker. It is never shown. It is rotated by a plan step (new
  secret, then the record) and deleted on unlink (decision 3).
- The ops are the contract's (`manifest`, `call`, `section`); the transport is HTTPS to the
  `workers.dev` URL instead of a Service Binding. Core treats the data Worker as a remote
  capability with an instance argument (decision 4).

### 3. pigeon reaches it only while the account link holds

Before each call to a data Worker, core checks, in this order, and stops at the first failure
with a message saying what to do:

1. The instance record has `data.enabled: true` → otherwise "data is not set up for X" and the
   setup steps.
2. For a `Linked` instance, `linked::account_for(env, host, instance)` succeeds: the link
   exists, refreshes and names the record's account → otherwise "pigeon's link to X's account
   is missing or revoked: link it on the Instances tab". For the home instance this check is
   pigeon's own account and always holds.
3. `DATA_KEY` for the instance is in the Directory → otherwise "the data Worker has not been
   deployed: run the instance plan".

Only then does core sign and send. So the link is the switch: when the person unlinks, or
revokes pigeon in Cloudflare (Profile → Access Management → Connected Applications), the
refresh fails and core stops signing. The data Worker keeps running inside the account
(capturing mail, syncing) because the data is the account owner's; only pigeon's reach ends.

**On Unlink** (needs the linked-accounts planner): before revoking, the unlink flow runs one
extra step while the grant still works: delete the data Worker's `DATA_KEY` secret and turn its
`workers.dev` off, then forget `data_key:{instance}`. If the person revoked from Cloudflare's side
first, that step cannot run; step 2 above already blocks calls, and re-linking rotates the key.

Rejected alternatives:
- **The data Worker as its own server behind the portal.** It would bypass pigeon's identity,
  the person's policy, tool approvals and the link check, and need an Access setup per instance.
- **Checking the link's OAuth token inside the data Worker** (the Worker calling Cloudflare's API
  with the token pigeon sends). It would put a token that can rewrite the account's Workers on
  every request, and add an API call per request against the account's 1,200 per 5 minutes.

### 4. The MCP surface: pigeon's own tools, behind the portal already

New tools in core's MCP (so the portal shows them as `pigeon_data_*` after the deploy sync),
each taking `instance`:

| Tool | Read or write | Does |
| --- | --- | --- |
| `data_status` | read | setup state, link check, queue backlogs, DLQ counts, last backup pass, connector states; or the setup steps when off |
| `data_records` | read | the exact query (ingest.md decision 7, step 3): `{entity?, doc_types?, from?, to?, limit, cursor}` → items with subject, date, fields; and the exact count |
| `data_search` | read | the hybrid query: `{question, entity?, doc_types?, from?, to?}` → ranked items, "possibly related" apart, and the exact count |
| `data_entities` | read | find entities by name, address or domain, with aliases and merges |
| `data_item` | read | one item: its row, emails fields, classifications, links, relations, and a signed link (10 minutes) to the original |
| `data_connectors` | read | the instance's mail connectors and their sync state |
| `data_sync` | write (light) | start a connector's sync now |
| `data_reprocess` | write | re-run a stage for items matching a filter (ingest.md decision 5) |
| `data_entity_merge` | write | merge or undo a merge of two entities |

- The tools live in core because core holds the link and `DATA_KEY`. Their logic is thin: check
  (decision 3), sign, forward `call` to the data Worker, return its answer. The data Worker's
  manifest lists the same tools, so the tool list stays in one place.
- **Policy.** These tools read personal data. Whether any of them should need the person's
  passkey per call (`call_tool` in `.pigeon/policy.toml`, as tool-approvals.md does for
  `deploy_token`) is the person's decision; this plan proposes none and changes no policy. A
  follow-up lists the candidates.
- **Results are bounded**: at most 200 items per page with a cursor, snippets not bodies, and
  originals only through `data_item`'s short-lived link, so a tool answer never carries a mailbox.

### 5. The uploader's endpoints

The uploader (ingest.md decision 11) runs on the person's machine and talks to the data Worker
directly, not through MCP (bulk bytes do not belong in tool calls):

- `POST /up/have`, `PUT /up/blob/{sha256}` (single or multipart parts), `POST /up/sighting`.
- Authentication: an **upload token** minted by core for one instance, one person and at most 24
  hours (`data_upload_token {instance}`, a write tool with the person's passkey), signed with
  `DATA_KEY`. The person pastes it into the uploader's prompt, never into chat.
- The Worker checks the SHA-256 of every byte it writes against the URL before committing (R2
  checks a supplied SHA-256 on PUT; multipart is checked after completion and deleted on
  mismatch).

### 6. Resources, all made by instance plan steps

| Resource | Name | New plan step |
| --- | --- | --- |
| R2 bucket | `{instance}-data`, jurisdiction from the record | `CreateBucket` (exists) |
| Basin Catalog on it | — | `EnableCatalog` |
| Vectorize index and metadata indexes | `{instance}-data-v1` | `CreateIndex`, `CreateMetadataIndex` |
| Queues | `{instance}-ingest`, `-extract`, `-classify`, `-embed`, `-backup`, each with `-dlq` | `CreateQueue`, `SetConsumer` |
| D1 | `{instance}-data` | `CreateDatabase`, `Migrate` |
| Pipelines | one stream, sink and pipeline per table group (ingest.md decision 3) | `CreateStream`, `CreateSink`, `CreatePipeline` |
| R2 event notification | whole bucket → backup queue; `staging/` → ingest | `AddBucketNotification` |
| The Worker | `{instance}-data`, artifact digest, bindings | `UploadWorker` (extended) |
| Worker secrets | `DATA_KEY`, later B2 and connector secrets | `SetWorkerSecret`, `DeleteWorkerSecret` |
| Email routing | capture subdomain and literal rule → the Worker (email.md) | `EnableEmailRouting`, `AddEmailRule` (DNS: the person's go-ahead) |

Each step is safe to run twice, shows in the dry run and is recorded in the ledger. Nothing is
deleted by a plan except what the ledger holds, and deletes of data resources (bucket, index,
D1, queues with backlog) are never planned automatically: the person removes them by hand.

### 7. willowgraysen

A new instance record, proposed by an agent and approved by the person:

```json
{
  "id": "willowgraysen",
  "title": "willowgraysen.com",
  "account": {"kind": "linked"},
  "zones": ["willowgraysen.com"],
  "data": {
    "enabled": false,
    "worker": "willowgraysen-data",
    "bucket": "willowgraysen-data",
    "jurisdiction": null,
    "email": {"enabled": false, "subdomain": "in", "addresses": ["capture"]},
    "connectors": []
  }
}
```

`data.enabled` stays false until phase 3 and the person's approval; the Link flow fills
`account_id`. The site part of the record is optional and not needed for data.

### 8. Architecture page

The data Worker and its resources appear in the instance's account box in the instances layer
(linked-accounts.md decision 8), drawn from the record and the ledger, never from live calls.
The edge from pigeon-platform to the data Worker is drawn dashed and labelled "while linked".

## What this plan needs from the instances and linked-accounts planner

Numbered so they can answer item by item. None of this is built by this plan.

1. **Record shape**: an optional `data: DataRecord` on `Instance` (`enabled`, `worker`,
   `worker_override`, `bucket`, `bucket_override`, `jurisdiction`, `email {enabled, subdomain,
   addresses}`, `connectors [...]` (mail-connectors.md)), with validation: names are DNS labels,
   `worker` defaults to `{instance}-data` and may not collide with the site Worker, platform
   Workers or another instance's; `bucket` likewise; addresses are local parts; the subdomain is
   one label under one of the instance's zones. Because `Instance` has `deny_unknown_fields`, the
   field must exist before any record carries it.
2. **OAuth scopes** for a data-enabled link, by the IDs `GET /oauth/scopes` lists (names to
   match: Vectorize Write, Queues Write, D1 Write, Workers AI Read, Pipelines Write, Workers R2
   Data Catalog Write, Email Routing Rules Write, Zone Settings Write for email routing), asked
   only for instances with `data`, and a re-link prompt when an existing link lacks them.
3. **Plan steps** in decision 6's table, with their ledger entries, dry-run text and idempotence.
4. **`workers::Binding` variants** for `vectorize`, `ai`, `queue`, `d1`, `pipelines`,
   `secret_text` and `send_email` (never used here, listed for completeness), uploads with a
   WASM module plus JS shim, and the queue consumer and Cron settings on upload.
5. **A remote capability over HTTPS**: `remote::Remote` today takes a Service Binding; the data
   tools need the same with a URL and a per-instance key (`DATA_KEY`), plus decision 3's checks
   before each call.
6. **Unlink hook**: one step that runs while the grant still works (decision 3) and the
   rotation of `DATA_KEY` on re-link.
7. **Directory**: the sealed `data_key:{instance}` entry, same sealing as link tokens.
8. **Console**: a "Data" section on the Instance tab: status, connectors, last backup, link
   between enabled and setup steps. Read-only at first.
9. **The Pipelines sink token** (ingest.md follow-up 6): whether pigeon may create an
   account-owned API token through the link (a scope for API Tokens Write), or the person makes
   it by hand.
10. **The artifact**: where the data Worker's built `.wasm` lives and how `UploadWorker` fetches
    it by digest (decision 1). Probably shared with the sites planner, who owns uploads.

## Phases

| Phase | What | Lands with | Who |
| --- | --- | --- | --- |
| 0 | ingest.md phase 0 spikes that concern the Worker: raw-binding calls from Rust for Vectorize and Pipelines; HTTPS contract round trip across accounts with a signed caller | nothing kept | agent |
| 1 | The data Worker crate (`src/data-worker`), off: handlers wired to stubs, signed contract ops, `data_status` answering "not set up"; built locally only | landing, code only; no deploy | executor, planner |
| 2 | The instances planner's items 1–7 | their landings | instances planner |
| 3 | `willowgraysen` linked; its record's `data` approved; the plan creates the resources and uploads the Worker | person's passkey, go-ahead per step | agent, person |
| 4 | core's `data_*` tools (read ones first) through the portal | landing; portal sync | agent |
| 5 | The uploader endpoints and `data_upload_token` | landing | agent |
| 6 | The home instance and noisypigeon get data for email capture (email.md phase 3) | person's passkey | agent, person |

## Follow-ups for the person

1. Onboard willowgraysen: create (or confirm) the Cloudflare account holding willowgraysen.com,
   with your login a member of it, then approve the `willowgraysen` instance record on the
   console and press Link Cloudflare account. Pick that account on Cloudflare's consent screen.
2. Turn on Workers Paid on that account (Queues retention, Pipelines, CPU, Vectorize).
3. Decide whether data tools need your passkey per call. Candidates: `data_item` (opens an
   original), `data_upload_token`, `data_reprocess`, `data_entity_merge`. A policy change is
   yours to make and lands with your passkey.
4. The artifact build for the data Worker (decision 1) is a CI change (`ci/`, `.pigeon/ci.toml`):
   approve the approach when phase 3 comes, or say if you prefer the data Worker uploaded by a
   CI deploy job like the platform Workers.
5. When phase 3 asks, say go for each step that touches willowgraysen.com's DNS (email routing
   on its capture subdomain).

## Log

- 2026-11-22: design written overnight from the person's brief (#6), after reading the instance
  record, the linked capability, instance plans, worker uploads, the contract and the portal
  plan. The link is the switch: core signs calls to the data Worker only after
  `linked::account_for` succeeds. MCP access is through pigeon's own server behind the portal,
  not a server per instance. Nothing in instance or linked-account code changed; the needs are
  listed above for their planner.

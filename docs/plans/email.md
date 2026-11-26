# Email: its own capability, writing into the instance's unified data store

Status: **designed 2026-11-26; phases 1 to 7 built off by default on
`claude/email-capability`** (see "Phases" and "What is not verified"). Nothing is routed, no
DNS record is changed and no resource exists until the person approves an `instance_plan` for pigeon.dev with their passkey and applies it.
This plan replaces the 2026-11-22 capture-subdomain design (`in.<zone>` with literal rules),
which is kept in git history at `007595d`.

Related plans: [ingest.md](ingest.md) (the record schema, the R2 layout, the B2 backup),
[data-worker.md](data-worker.md) (the per-instance data Worker), [capability-scoping.md](capability-scoping.md)
(catalogue, placement), [membership.md](membership.md) (viewer and operator),
[storage-providers.md](storage-providers.md) (Storage › Buckets).

## Goal

The person's words (#4, 2026-11-22): "Design a capability to use Cloudflare Email. This will be
used on the main instance for pigeon.dev, as well as in the noisypigeon instance for
noisypigeon.com. Any emails sent to the email should be captured and an extensible schema will
capture the original email, attachments, full headers, as well as a taxonomy of the email. This
should be fed into #5."

The person's design (2026-11-26), followed here:

1. `email` is **its own capability**, enabled per instance in the record. It writes into the
   instance's **unified data store**, the same R2 store the data and vector layers use. There
   is no separate email bucket.
2. The home instance gets the **lite tier** of that store first.
3. One Email Routing **catch-all** on pigeon.dev sends every message to the data Worker.
4. **Mailboxes** are listed addresses plus `catch-all`; everything is kept forever.
5. **Forwarding** is selectable, listed addresses only by default, and changes to it need the
   person's passkey.

## The person's decisions

| Decision | Value |
| --- | --- |
| Which addresses | a **catch-all** on the zone, plus **listed addresses** that each get their own mailbox |
| The old provider | the old Fastmail and Proton **MX records are deleted**; mail lost in the meantime is accepted. SPF, DKIM or DMARC TXT records may remain and are listed by the plan as stale |
| Retention | **forever**: there is no delete tool; spam is kept in `catch-all` |
| Forwarding scope | **selectable**, default **listed** (`forward.scope = "listed"`); `"all"` includes catch-all |
| First listed addresses | `cloudflare`, `buildkite`, `apple` (all `@pigeon.dev`) |
| Where data lives | everything in R2 (the instance's data bucket) and D1; an **offsite backup in Backblaze B2**; Vectorize, classification and entities **later, out of scope** |

## Tiers of the data store

The data store is one per instance: bucket, D1 and data Worker all named `{instance}-data`
(`pigeon-data` for the home instance, data-worker.md decision 1). Its tier is a record value:

```toml
[capabilities.data]
tier = "lite"
```

| | lite (built) | full (later, a superset) |
| --- | --- | --- |
| R2 bucket | `{instance}-data`: originals at `blobs/sha256/{2}/{2}/{64}`, derived files at `derived/{item_id}/…` (ingest.md decision 9) | the same bucket and layout, plus the Iceberg warehouse prefix and `staging/` |
| The record | D1 `{instance}-data`, tables `items`, `sightings`, `emails` with **exactly** ingest.md decision 3's columns (lists and structs as JSON text) | Iceberg tables in Basin Catalog, written through Pipelines; the lite rows are replayed into them once, with no rewrite, then D1 keeps only operational indexes |
| The data Worker | `{instance}-data` with the `email` handler, the signed read ops and the backup consumer | adds the ingest queues, Vectorize, Workers AI, Pipelines |
| Backup | an R2 event notification on the whole bucket → queue `{instance}-data-backup` → B2 (ingest.md decision 10) | the same |
| Not in lite | Vectorize, classification, entities, relations, the ingest queues | built by ingest.md phases 3, 5 and 8 |

Seams left for the full tier: `items.status` is `stored` (never `classified` or `embedded`
yet); the handler's last step is where an `{instance}-extract` queue message goes; `derived/`
holds only `text.txt` today; the D1 tables carry the decision-3 keys, so the replay is a
`SELECT` per table into the matching sink.

**D1 table names.** The ingest runner's dedupe index (ingest.md decision 13.4) also has a table
named `sightings`. The lite tier does not create the dedupe index. When the full tier adds the
ingest runner, its index moves to its own database `{instance}-index` (or its tables are
prefixed `index_`), decided then; the lite migrations refuse to run on a database that holds the
index's `blobs` table.

## The flow

```
 sender ─▶ MX route{1,2,3}.mx.cloudflare.net ─▶ Email Routing (pigeon.dev)
                                                    │ catch-all rule: action worker
                                                    ▼
                               pigeon-data  email(message, env, ctx)
   1  config from D1 `config` (absent → reject "not set up": off by default)
   2  stream the raw bytes, SHA-256 as they arrive (≤ 25 MiB)
   3  HEAD blobs/sha256/…; PUT once if absent  ── failure → setReject (sender gets a bounce)
   4  parse with pigeon-email (a parse failure still stores; item status `failed`)
   5  PUT html and attachments as blobs; derived/{item_id}/text.txt
   6  D1 batch: items (kind=email), sightings (source=email_routing, locator=envelope_to,
      extra={mailbox, auth}), emails  ── failure → setReject
      a duplicate (same bytes) writes only the new sighting
   7  forward (below); a failure is recorded in `email_deliveries`, the mail stays stored
   8  log: id, size, mailbox, outcome; never content or addresses
```

- **Mailbox**: the envelope recipient, lowercased, with a `+tag` removed, matched against the
  record's listed local parts on the record's email domain; anything else is `catch-all`.
- **Auth**: the first `Authentication-Results` header written by `mx.cloudflare.net` (any
  earlier one is the sender's and is not trusted), its `spf`, `dkim` and `dmarc` results into
  `sightings.extra.auth`. The console shows a sender-check badge from it: `pass` (DMARC pass),
  `fail` (DMARC fail), `partial` (SPF or DKIM pass), `none`.
- **Ids**: `item_id` is the raw SHA-256; attachments are their own items with `parent_id`.
  `emails.fingerprint` is the SHA-256 of the normalized Message-ID, else of From, Date, Subject
  and the text's hash (ingest.md decision 2, tier 2).

## Mailboxes

```toml
[capabilities.email]
domain = "pigeon.dev"            # optional: the record's first zone
addresses = [
  { local = "cloudflare" },
  { local = "buildkite" },
  { local = "apple" },
]

[capabilities.email.forward]
scope = "listed"                 # default; "all" also forwards catch-all
# to = "person@example.com"      # optional default destination
```

- Each listed address is its own mailbox; everything else lands in `catch-all`, where spam lands.
- Adding an address is a **record change** (`email_address_set` proposes it; the person
  approves the record with their passkey; the next plan writes the Worker's config row). It is
  never a DNS or routing change: the catch-all already sends everything to the Worker.
- Retention is forever. No tool deletes mail.

## Forwarding

- A listed address forwards to its own `forward`, else to `forward.to`, else not at all.
  `catch-all` forwards only with `scope = "all"`, to `forward.to`.
- Cloudflare forwards only to **verified destination addresses** of the account: the plan adds
  each destination (`AddDestination`), Cloudflare emails it a link, and the plan shows it as
  **pending** until it is verified. The Worker forwards to a pending destination and records the
  failure; the mail is stored either way.
- **Passkey**: any change to a destination or the scope goes through `email_forward_set`, which
  the policy gates per call (below), and becomes a record change the person approves with their
  passkey.
- **Loops**: every forward carries `X-Pigeon-Data: <instance>`. A message that arrives with that
  header naming this instance is stored and never forwarded again (outcome `loop`).

## Plan steps (`instances/plan.rs`, through `instance_plan` / `instance_apply`)

Dry run first, as every instance plan; the person approves the exact steps with their passkey;
`instance_apply` checks the account still gives the same plan (digest) and applies each step
once, recording what pigeon made in the instance's ledger.

| Step | Does | Ok when |
| --- | --- | --- |
| `CreateBucket` (exists) | the data bucket | the bucket is pigeon's |
| `CreateDatabase` | D1 `{instance}-data` | the database is pigeon's |
| `Migrate` | the lite migrations not yet applied, by id | all applied |
| `UploadDataWorker` | the data Worker from its build artifact (digest shown), bindings `DATA` (R2), `DB` (D1), `B2_KEYS` (Secrets Store), `INSTANCE`, `BACKUP`, `DATA_KEY` (secret, made by core and kept sealed as `data_key:{instance}`); then its `workers.dev` address | the uploaded digest matches |
| `WriteEmailConfig` | the config row the handler reads: domain, mailboxes, forward | the row's digest matches the record |
| `CreateQueue` | `{instance}-data-backup` and its `-dlq`, consumed by the data Worker | they are pigeon's |
| `AddBucketNotification` | the whole bucket's object-create events → the backup queue | it is there |
| `Pending` | something only the person can do; shown, does not stop the plan: the B2 key not set, a destination not verified | — |
| `RemoveStaleDns` | one stale old-provider record, by id, exactly as shown | — |
| `EnableEmailRouting` | Email Routing on the zone (Cloudflare adds its MX, SPF and DKIM) | enabled |
| `SetCatchAll` | the zone's catch-all rule → action worker `{instance}-data` | it says so |
| `AddDestination` | a forward destination address; Cloudflare emails it a link | it exists (then `Pending` until verified) |

**The DNS dry run reads the zone first** (`GET /zones/{id}/dns_records`):

- **Refuses** (a `Blocked` step, which stops the plan) when any MX record exists that is not
  Cloudflare's `route{1,2,3}.mx.cloudflare.net`: a live foreign MX means another provider still
  receives the zone's mail.
- **Lists stale old-provider records** as "stale, to replace", each as its own
  `RemoveStaleDns` step with its id, name, type and content: an apex SPF TXT that includes
  Fastmail (`messagingengine.com`) or Proton (`protonmail.ch`), Fastmail's DKIM
  (`fm1`/`fm2`/`fm3._domainkey`), Proton's DKIM (`protonmail*._domainkey`), a
  `protonmail-verification=` TXT, and a `_dmarc` TXT whose reports go to either provider. A
  `_dmarc` record that names neither is kept and shown as kept.
- **Never deletes anything not shown in the plan**: apply deletes exactly the record ids the
  approved plan names, and the digest check refuses the apply if the zone gave a different plan.

**Credentials.** A linked instance's plan uses its link. The home instance's existing token
(`instance_home`) holds R2 write and Zone Read, never Workers-scripts write. The data steps on
home therefore use a second core-only purpose, `data_home` (`split::purpose_permissions`),
minted for one plan or apply and revoked after: on the account Workers R2 Storage Write, D1
Write, Queues Write, Workers Scripts Write, Secrets Store Read and Email Routing Addresses
Write; on the email domain's zone alone Zone Read, DNS Write, Email Routing Rules Write and
Zone Settings Write. Its Workers-scripts write is used for exactly one script, the one the plan names
(`pigeon-data`), which must not be platform-owned; the plan refuses otherwise. This widens what
home's plans may do and is the person's decision: see Follow-ups.

## Tools

| Tool | Role | Does |
| --- | --- | --- |
| `email_status {instance}` | viewer | setup state, or the steps to turn it on; counts, the last message's time, forwarding failures |
| `email_mailboxes {instance}` | viewer | each listed mailbox and `catch-all` with its count and newest time |
| `email_list {instance, mailbox?, since?, from?, cursor?, limit?}` | viewer | from D1, newest first: id, from, subject, date, sender check, attachment count; 50 a page, at most 200 |
| `email_get {instance, id}` | viewer | headers, the text, the HTML marked `sandboxed` (never inlined into the console's own DOM), the attachment list |
| `email_address_set {instance, local, remove?}` | operator | proposes the record with that listed address added or removed |
| `email_forward_set {instance, local?, to?, scope?}` | operator, **passkey per call** | proposes the record with a destination or the scope changed |

- Core reaches the data Worker through **signed contract ops**: a POST to
  `https://{instance}-data.{subdomain}.workers.dev/data/call` signed with the instance's
  `DATA_KEY` (`pigeon_ingest::protocol`, at most 60 s of clock skew), the instance id inside the
  signed body. The home instance uses the same path, so there is one code path.
- The read ops are `status`, `mailboxes`, `list`, `get`, `objects` and `object`; the Worker
  answers 404 with no body to anything unsigned.

## Storage reuse

Storage gains a **data store** source: `storage_list_objects` and `storage_get_object` with
`instance` and `source: "data"` read the instance's `{instance}-data` bucket through its data
Worker (`objects` and `object` ops), limited to `blobs/` and `derived/`. Storage › Buckets shows
it when the record enables `data`, beside the S3 providers (Scaleway, DigitalOcean, Backblaze),
which work as before.

## The Email tab

A console area `email` (key `g l`), shown only on instances whose record enables `email`, to
viewers and up:

- **Left**: mailboxes with counts (`email_mailboxes`).
- **Middle**: the message list (`email_list`): from, subject, date, a sender-check badge, a
  paperclip when there are attachments.
- **Right**: the message (`email_get`), through the storage object reader for the parts and a
  mail renderer (`shell.mailFrame`): the HTML in an `<iframe sandbox="">` (no scripts, an
  opaque origin, no forms or popups) whose document carries
  `default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:` and also
  inherits the console's own policy, so remote images are blocked; the text shown plain; each
  attachment, and the original `.eml`, saved through `storage_get_object` with
  `source: "data"` (`shell.saveFile`), never opened in the console.
- Fixtures: `email_mailboxes.home.json`, `email_list.home.json` and `email_get.home.json` are
  what the read ops answer for the smoke suite's stand-in mail, checked by the suite
  (`PIGEON_WRITE_FIXTURES=1` rewrites them).

## Phases

| Phase | What | State |
| --- | --- | --- |
| 1 | This plan | written |
| 2 | Lite data store: D1 migrations (`pigeon_ingest::datastore`), data Worker minimal (`/data/call` signed read ops, `src/data-worker/src/store.rs`), the plan steps (`instances/data_plan.rs`) and their observe/apply (`instances/data_apply.rs`) | built, off |
| 3 | Email capability: catalogue, record, the handler (`src/data-worker/src/mail.rs`), the mailbox rule, the tools (`src/capabilities/src/email.rs`) | built, off |
| 4 | Storage data-store source | built, off |
| 5 | The Email tab | built, off |
| 6 | Forwarding: destinations, scope, loop guard | built, off |
| 7 | B2 backup steps and consumer | built, off |
| 8 | Turn on for pigeon.dev (the person) | waits for the person |
| later | noisypigeon and willowgraysen (linked; `linked: true` in the catalogue); the full tier | not started |

## Follow-ups for the person

1. **Accept the `data_home` credential purpose** (Workers Scripts Write for the data Worker
   alone, on home) or say how the home data Worker should be uploaded instead.
2. **The data Worker's build artifact**: nothing builds the `.wasm` today (data-worker.md
   decision 1). Until a CI job does (a `ci/` change, the delegate's approval), build it by hand
   (`worker-build --release` in `src/data-worker`), put each module of `build/` in the
   platform account's bucket `pigeon-data-artifacts` at `data-worker/{digest}/{module}`, and
   write `data-worker/current.json` as `{"digest", "main", "modules": [{"name",
   "content_type"}]}`. The digest is `data_plan::artifact_digest`: the SHA-256 of the lines
   `{module} {sha256 of its bytes}`, in order. The plan shows the digest; the apply refuses
   modules that do not match it.
3. **Policy**: add `email_forward_set` to `risky-tools-need-passkey` (the diff is in the
   landing report).
4. **Backblaze**: create bucket `pigeon-backup-pigeon-data` (SSE-B2 on) and an application key
   for it alone (read and write); enter it on Access as `DATA_B2_PIGEON_DATA`
   (`KEY_ID:APPLICATION_KEY`); put the bucket's endpoint and region in the record's
   `[capabilities.data.backup]`.
5. **Forward destinations**: open the verification email Cloudflare sends each one.

## What is not verified

- No Cloudflare API in `instances/data_apply.rs` has been called: the paths and bodies for D1
  (`/d1/database`, `/query`), Queues and their consumers, R2 event notifications, Email Routing
  (`/email/routing`, `/email/routing/enable`, `/rules/catch_all`, `/email/routing/addresses`),
  DNS and the Workers upload with a WASM module are written from Cloudflare's API reference and
  untested; so are the `data_home` permission names, which `tokens` resolves by name. The first
  `instance_plan` for pigeon.dev is a dry run and shows any failure before anything changes.
- The data Worker has been built for `wasm32-unknown-unknown` (clippy), never run: the `email`
  handler's streaming read, `setReject`, `forward` with the loop header, and the backup's
  SigV4 `PUT` to B2.
- What Email Routing does with a `setReject` (a permanent 5xx to the sender is expected).
- The Email tab has been checked by `node --check` and the smoke suite's contract, not in a
  browser; in particular that the sandboxed `srcdoc` frame renders under the console's policy.

## Log

- 2026-11-22: the capture-subdomain design and phase 1 (`pigeon-email`, schema and parser).
- 2026-11-26: replaced by the person's design (catch-all on pigeon.dev, listed mailboxes, the
  lite data store, forwarding, B2 backup). Built off by default on `claude/email-capability`;
  no Cloudflare API was called while building.

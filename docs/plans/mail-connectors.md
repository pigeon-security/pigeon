# Mail connectors: Gmail, Fastmail, iCloud and Proton Mail into the data pipeline

Status: **designed** (2026-11-22). Nothing is built, deployed or enabled; no credential exists.
Branch `claude/ai-agent-AFc1coPrsJTT` (docs only).

Related plans: [email.md](email.md) (new mail, and the schema every connector produces),
[ingest.md](ingest.md) (the pipeline), [data-worker.md](data-worker.md) (where connectors run).

## Goal

The person's words (#7): "We will use the Cloudflare Email capability for new emails, but also
design MCP connections for Gmail, Fastmail, and a regular IMAP client for ProtonMail (Bridge) and
iCloud authenticating with app passwords, to pull all emails and their attachments from the mail
server, and ingest them into the vector database / Apache Iceberg."

Read as: one-time backfill of every message in each mailbox, then incremental sync, each message
as raw MIME into ingest.md's pipeline, with sync state and counts so "all emails" can be checked.
The connectors are managed through pigeon's MCP tools (`data_connectors`, `data_sync`;
data-worker.md decision 4).

Hard lines:
- **Read-only** against every mail server: no flags set, nothing moved or deleted. IMAP uses
  `EXAMINE` and `BODY.PEEK[]`; Gmail uses the read-only scope; JMAP uses a read-only token.
- Credentials are entered by the person on the console, stored only in the instance's account,
  never shown again, never in chat or logs.
- Off by default: a connector exists only when the person approves it in the instance record and
  sets its secret.

## What is true today (checked 2026-11-22)

- No connector code. The output type exists: `pigeon_email::Source` already has `Gmail {account,
  id, thread_id, labels}`, `Jmap {account, id, mailbox_ids}` and `Imap {server, account, mailbox,
  uid_validity, uid}` (email.md phase 1).
- workers-rs 0.9 has TCP sockets (`worker::Socket`, `SecureTransport::{Off, On, StartTls}`,
  tokio `AsyncRead`/`AsyncWrite`), so IMAP over TLS can run in a Worker
  ([TCP sockets](https://developers.cloudflare.com/workers/runtime-apis/tcp-sockets/)).
- **Workers VPC** binds a Worker to a TCP service behind a Cloudflare Tunnel
  ([Workers VPC](https://developers.cloudflare.com/workers-vpc/),
  [VPC services](https://developers.cloudflare.com/workers-vpc/configuration/vpc-services/)):
  the way to reach a Proton Mail Bridge that is not on the internet.

| Provider | Access | Facts that shape the design | Source |
| --- | --- | --- | --- |
| Gmail | Gmail API, OAuth 2.0 | `gmail.readonly` is a **restricted** scope. Quotas: 1,200,000 units/min per project, **6,000 units/min per user**; `messages.get` costs 20 units, **60 with `format=raw`**; `messages.list` 5; `history.list` 2; `messages.attachments.get` 20. A daily threshold of 80,000,000 units per project, above which Google plans charges later in 2026. Incremental sync with `history.list` from a `historyId`; an expired one answers 404 and needs a full sync. A consent screen in **Testing** status issues refresh tokens that **expire in 7 days** | [quota](https://developers.google.com/workspace/gmail/api/reference/quota), [sync](https://developers.google.com/workspace/gmail/api/guides/sync), [scopes](https://developers.google.com/workspace/gmail/api/auth/scopes), [OAuth](https://developers.google.com/identity/protocols/oauth2) |
| Fastmail | JMAP | Session at `https://api.fastmail.com/jmap/session`; an **API token** (Settings → Privacy & Security → Manage API tokens) for apps built for oneself, sent as `Authorization: Bearer`; scope `urn:ietf:params:jmap:mail`. JMAP gives `Email/query`, `Email/get`, `Email/changes` and blob downloads (raw RFC 5322 by `blobId`) | [Fastmail developers](https://www.fastmail.com/dev/), [RFC 8620](https://www.rfc-editor.org/rfc/rfc8620), [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621) |
| iCloud Mail | IMAP | `imap.mail.me.com`, port 993, TLS; username usually the name part of the iCloud address; an **app-specific password** (needs two-factor authentication on the Apple Account) | [iCloud mail settings](https://support.apple.com/en-us/102525), [app-specific passwords](https://support.apple.com/en-us/102654) |
| Proton Mail | IMAP through Proton Mail Bridge | Bridge needs a **paid** Proton plan; it runs on the person's machine and serves IMAP on localhost (by default `127.0.0.1:1143`, STARTTLS, with a Bridge-generated password); a CLI mode exists for headless use | [Bridge](https://proton.me/mail/bridge), [Bridge CLI](https://proton.me/support/bridge-cli-guide) |

## Decisions

### 1. One shape for every connector: list changes, fetch raw, hand to ingest

```
tick (Cron, every 5 min) ─▶ for each enabled connector account: queue {connector, account, cursor?}
sync consumer:  load state (D1) ─▶ list changes since state ─▶ for up to N ids:
                  fetch raw RFC 5322 ─▶ SHA-256 ─▶ R2 blobs/… (if absent) ─▶ ingest queue
                                                   {blob, source: Source::<Provider>{…}}
                ─▶ save state ─▶ more left? queue a continuation
```

- **Raw MIME always.** Every provider can give the full original message with its attachments
  inline (Gmail `format=raw`, JMAP blob download, IMAP `BODY.PEEK[]`). Attachments are then
  extracted by `pigeon_email::parse` in the extract stage, the same as captured mail; no
  per-provider attachment code. Gmail's `messages.attachments.get` is never needed.
- **The pure part is shared and tested**: a `pigeon-mail-sync` crate (phase 1) with each
  provider's request building, response parsing and state transitions, without I/O, so the smoke
  suite runs them against recorded transcripts. The data Worker supplies the I/O.
- **Bounded units of work**: one consumer invocation handles at most a set number of messages
  (Gmail 90, others 200) or 10 minutes of wall time, whichever comes first, then saves state and
  queues its continuation. A crash loses at most one unit, and re-fetching it is harmless
  (decision 7).
- **State** in D1 table `sync_state(connector, account, mailbox, key, value, updated_at)`,
  written after each unit, mirrored to Iceberg `sightings` by the ingest stage.

### 2. Gmail: OAuth and the Gmail API

**Credentials.** The person creates a Google Cloud project with the Gmail API, a consent screen
and a Web OAuth client (follow-ups). Its client id and secret are entered on the console and
written as secrets of the instance's data Worker.

- The consent screen must be **In production**, not Testing: a Testing screen's refresh tokens
  die after 7 days. For a project used only by its owner, Google shows an "unverified app"
  warning that the owner can pass; verification of a restricted scope is for apps others use.
  Phase 3 confirms this holds for the person's account type (a Workspace account can instead
  mark the app Internal).
- **The browser flow runs in core**, because core has the console, the passkey and a fixed
  redirect: `https://platform.pigeon.dev/data/oauth/google/callback`, with `state` bound to the
  instance, the connector and the signed-in person (as the Cloudflare Link flow does). Scope
  `https://www.googleapis.com/auth/gmail.readonly`, `access_type=offline`, `prompt=consent`.
- **The refresh token goes to the instance**: core hands it once to the data Worker over the
  signed contract; the Worker seals it with its own key into D1 and core forgets it. Core never
  holds a mailbox credential afterwards. The access token (one hour) is refreshed by the Worker.

**Sync.**
- Full: `users.messages.list` (500 ids a page, `includeSpamTrash=true` so nothing is missed,
  labelled as such) → `users.messages.get?format=raw` per id. 60 + 5/500 units per message
  against 6,000 units/min per user: **about 100 messages a minute, 144,000 a day**. 500,000
  messages take about 3.5 days. Ids already sighted (from `sightings`, by Gmail id) are skipped.
- Incremental: `users.history.list?startHistoryId=…` (2 units) for `messageAdded` and
  `labelsAdded`/`labelsRemoved`; new ids are fetched raw; label changes update the sighting's
  labels. A 404 means the history id expired: a full listing again, which costs only the list
  calls for messages already held.
- The project-wide daily threshold (80M units) is far above one mailbox's backfill (~33M units
  for 500,000 messages, spread over days).
- Later, if five-minute polling is too slow: `users.watch` to Pub/Sub with a push to the data
  Worker. Not in the first phases.

### 3. Fastmail: JMAP with an API token

- **Credential**: a Fastmail API token with **read-only** access to mail, made by the person,
  entered on the console, written as a data Worker secret.
- **Session**: `GET https://api.fastmail.com/jmap/session` → `apiUrl`, `downloadUrl`, the mail
  account id.
- **Full**: `Mailbox/get` (all mailboxes, recorded); `Email/query` with no filter, sorted by
  `receivedAt`, paged by `position` (or anchor), `calculateTotal: true` for the count; for each
  page `Email/get` with properties `blobId, mailboxIds, keywords, receivedAt, size`; then each
  raw message from `downloadUrl` with its `blobId` (`type=message/rfc822`). Up to 4 downloads at a
  time.
- **Incremental**: `Email/changes` since the saved `state` → created and updated ids (destroyed
  ids become a sighting row with `removed_at`; the item stays). A `cannotCalculateChanges` error
  means a full listing again, skipping ids already sighted.
- Source: `Source::Jmap {account, id, mailbox_ids}`.

### 4. IMAP: iCloud with an app-specific password

- **Connection** from the data Worker: `connect("imap.mail.me.com:993", secureTransport: on)`.
- **Client**: a small IMAP4rev1 client in `pigeon-mail-sync`, written sans-I/O (a state machine
  fed bytes, answering bytes), covering only what sync needs: `CAPABILITY`, `LOGIN` (or
  `AUTHENTICATE PLAIN`), `LIST`, `EXAMINE`, `UID SEARCH`, `UID FETCH (UID FLAGS INTERNALDATE
  RFC822.SIZE BODY.PEEK[])`, `LOGOUT`, with literals and `CONDSTORE` (`HIGHESTMODSEQ`,
  `CHANGEDSINCE`) when the server offers it. No existing async IMAP crate is known to run on the
  Workers socket type; a narrow client is less code than adapting one, and the smoke suite can
  replay real transcripts against it.
- **Per mailbox state**: `UIDVALIDITY`, the highest UID fetched, `HIGHESTMODSEQ`. A changed
  `UIDVALIDITY` means the UIDs were renumbered: rescan the mailbox; content addressing and the
  message fingerprint stop duplicates.
- **Mailboxes**: all of them from `LIST`, including Archive, Sent and Junk, except ones the
  record excludes. A message in two mailboxes is one item with two sightings.
- **Politeness**: one connection per account at a time; iCloud throttles busy clients, so the
  unit is 200 messages and back-off doubles on `NO`/`BYE` up to an hour.
- **Credential**: the app-specific password, entered on the console, written as a data Worker
  secret. Revoking it at appleid.apple.com stops the connector, which `data_status` reports.

### 5. Proton Mail: Bridge on the person's machine

Bridge serves IMAP only on the machine it runs on. A Worker cannot connect to `127.0.0.1` there.

**Chosen, phase 4: the uploader talks to Bridge.** The uploader (ingest.md decision 11) runs on
the same machine and uses the same `pigeon-mail-sync` IMAP client over a local TCP socket
(`127.0.0.1:1143`, STARTTLS, Bridge's certificate pinned on first use), then sends raw messages to
the data Worker's upload endpoints with `Source::Imap {server: "proton-bridge", …}`. It syncs
whenever the person runs it (or on a schedule they set on their machine).
- Nothing on the person's network is opened to the internet.
- The backfill can also use Proton's own export (`.eml` files), which the uploader reads as
  `Source::Upload`; the fingerprint links those to later Bridge copies.

**Later, if always-on sync is wanted:** run Bridge (CLI mode) on an always-on machine or NAS,
publish its IMAP port through a Cloudflare Tunnel in the instance account, and bind the data
Worker to it with a Workers VPC TCP service. The data Worker then syncs Proton like iCloud.
Rejected for now: Bridge in a Cloudflare Container, because Bridge keeps a local cache and
keychain and a Container's disk does not persist, so it would need a fresh login (with two-factor
authentication) after each cold start.

### 6. Connector records and secrets

Connectors are data in the instance record (`data.connectors`, data-worker.md need 1), approved
with the person's passkey:

```json
{"id": "gmail-personal", "kind": "gmail", "account": "someone@gmail.com",
 "enabled": false, "exclude": ["[Gmail]/Trash"], "since": null}
```

- `kind`: `gmail`, `jmap`, `imap`; `imap` adds `host`, `port`, `tls` (`on` or `starttls`).
- `since` limits the backfill by date if the person wants; null means everything.
- Secrets are separate: Worker secrets named `MAIL_{ID}` (uppercased id), written by core from
  the console's passkey-bound secret form, as the Access tab does for platform secrets. A
  connector whose secret is missing does nothing and `data_status` says which secret to set and
  where.

### 7. Duplicates and completeness across connectors

- The same message reached through Gmail, IMAP and the capture address has different raw bytes;
  ingest's message fingerprint (ingest.md decision 2, tier 2) links them as `same_message`, and
  each source adds a sighting. Queries count the message once.
- Re-fetching a message already held costs one download and one `HEAD`; nothing is written twice.
- **Counts**: after each full pass a connector records the server's count per mailbox (IMAP
  `STATUS MESSAGES`, Gmail `users.getProfile.messagesTotal` and per-label totals, JMAP
  `calculateTotal`) beside the number of distinct messages sighted. `data_status` shows both, so
  a gap is visible. This is what "COMPLETE results" rests on for mail.

### 8. Size and failure handling

- Messages up to 25 MiB (most providers' limits) are fetched into memory, hashed and stored;
  larger ones are streamed to R2 with `crypto.DigestStream` and parsed later by a Container job
  (ingest.md decision 8).
- A message that fails to fetch three times goes to the DLQ with its id; the sync moves on and
  `data_status` lists it. A credential failure stops that connector and marks it `auth_failed`.

## Phases

| Phase | What | Lands with | Who |
| --- | --- | --- | --- |
| 1 | `pigeon-mail-sync`, pure and off: Gmail, JMAP and IMAP request/response/state code; smoke scenarios replaying recorded transcripts (made up, no real mail) | landing, code only | executors, planner |
| 2 | The sync consumer and Cron in the data Worker (after data-worker.md phase 3) | landing; instance plan | agent |
| 3 | Gmail: core's OAuth flow, handing the token to the instance; the person's Google project | person's steps, passkey | agent, person |
| 4 | iCloud in the Worker; Proton through the uploader | person sets passwords | agent, person |
| 5 | Fastmail | person sets the token | agent, person |
| 6 | Counts and gaps in `data_status`; optional Gmail push; optional Proton through Tunnel and Workers VPC | | agent |

## IMAP backfill and forwarding

The Data Worker transport accepts one raw message per signed request. It requires the
instance plan to bind `MAIL_IMPORT_INSTANCE` to the approved instance and an instance
administrator to set the dedicated `MAIL_IMPORT_KEY` secret. The record and console
opt-in still needs to be wired into the reshaped Data plan, so the route remains inactive
on provisioned instances until that work ships.
It stores the exact RFC 822 bytes and independently addressable attachments, and records
the server, account, folder, UIDVALIDITY and UID on each message's sighting. Historical
imports never forward mail.

Run the client on a trusted local machine with Python 3.10 or later. Use app passwords
from Gmail, iCloud, and Fastmail, subject to their account and provider policies. For
Proton Mail, run the client on the same machine as a signed-in Proton Mail Bridge, then
use its generated IMAP login and explicitly trust the Bridge certificate with
`--bridge-certificate`. The Bridge connection uses STARTTLS on loopback.

```sh
python3 src/data-worker/tools/mail-import.py gmail https://data-worker.example.net person@gmail.com
```

The client prompts for the mailbox and import secrets. It checks all selectable folders
by default, or only those given with repeatable `--mailbox` arguments. Repeat the command
until a batch imports zero messages. Keep the private SQLite checkpoint file across runs.
Batch size, inter-message delay and retry limit are configurable. The client records a
redacted local terminal receipt for each attempt. Messages over 25 MiB stop the batch
without advancing the cursor so a future larger-message path can pick them up.

To transition from historical backfill to forwarding, first configure a listed capture
address such as `inbox@willowgraysen.com` in the instance's Email capability. Choose an
independent, verified forwarding destination there, then verify the inbound route with a
test message. Set that address as the source provider's automatic forwarding target and
complete the source provider's confirmation. Repeat the IMAP command after forwarding
starts to cover messages received during cutover. The store keeps distinct byte streams
when a provider rewrites headers; exact identical bytes share a payload.

## Follow-ups for the person

1. **Gmail**: in Google Cloud Console, create a project; enable the Gmail API; set up the OAuth
   consent screen (External for a personal account, Internal for Workspace) and publish it **In
   production**; create a Web application OAuth client with the redirect
   `https://platform.pigeon.dev/data/oauth/google/callback`. Enter its client id and secret on the
   console when phase 3 asks. Then press Connect Gmail on the console and allow read-only access.
2. **Fastmail**: Settings → Privacy & Security → Manage API tokens → new token with read-only
   mail access. Enter it on the console when phase 5 asks.
3. **iCloud**: at account.apple.com, Sign-In and Security → App-Specific Passwords → generate one
   named `pigeon`. Enter it, with your iCloud address, on the console when phase 4 asks.
4. **Proton**: install Proton Mail Bridge on the machine that will run the uploader, sign in, and
   copy the Bridge IMAP password into the uploader's prompt when phase 4 asks (never into chat).
   Optionally, export your mailbox from Proton as `.eml` files for a faster backfill.
5. Say which mailboxes or labels to leave out, if any (default: everything, Spam and Trash
   included and labelled).

## Log

- 2026-11-22: design written overnight from the person's brief (#7). Google's quota page now
  charges 60 units for a raw `messages.get` and announces charges above a daily threshold later
  in 2026; Testing-status consent screens give 7-day refresh tokens, so the project must be In
  production. Proton Bridge serves only localhost, so Proton goes through the uploader first and
  through Tunnel plus Workers VPC later. Nothing built.
- 2026-11-22 23:40 UTC: handoff. The AI agents planner session wraps up; this plan's next
  steps wait on the person's answers (`overnight-2026-11-22.md`, "#4–#7 Data platform" and
  its handoff section), then an AI Gateway planner.

# Email capture: Cloudflare Email Routing into the data pipeline

Status: **designed; phase 1 (the message schema and its parser, crate `pigeon-email`) built
off by default** (2026-11-22). Nothing is deployed, routed or enabled. Code branch:
`claude/ai-agent-NVhZ3wCsS5dT`; this plan: `claude/ai-agent-AFc1coPrsJTT`.

Related plans: [ingest.md](ingest.md) (where captured mail goes), [data-worker.md](data-worker.md)
(the Worker that receives it), [mail-connectors.md](mail-connectors.md) (mail pulled from
existing mailboxes), [linked-accounts.md](linked-accounts.md) (instances and linked accounts).

## Goal

The person's words (#4): "Design a capability to use Cloudflare Email. This will be used on the
main instance for pigeon.dev, as well as in the noisypigeon instance for noisypigeon.com. Any
emails sent to the email should be captured and an extensible schema will capture the original
email, attachments, full headers, as well as a taxonomy of the email. This should be fed into
#5."

The target use, recorded for every data plan: a website where the person asks "Show me all
financial records for <company>" or "Show me all correspondence with <business/person>" and gets
**complete** results. The website is out of scope. This plan keeps what it needs from the first
message: every address ready for entity linking, every header, the original bytes, and a
taxonomy slot that classification fills (ingest.md, decisions 5 and 6).

Hard lines:
- No change to a zone's existing mail. noisypigeon.com's Proton MX, SPF, DKIM and DMARC records
  are never touched (linked-accounts.md, "Other DNS records").
- Off by default. Mail is routed only after the person approves the instance's `data.email`
  record with their passkey and says go for the DNS step in the session that makes it.
- Mail is personal data. It stays in the instance's own Cloudflare account, never in logs, never
  in a model prompt outside Workers AI in that account.

## What is true today (checked 2026-11-22)

### pigeon

- No email code. No Worker has an `email` handler; no zone has Email Routing set up by pigeon.
- workers-rs 0.9 (the version pigeon pins) supports `#[event(email)]` and
  `ForwardableEmailMessage::raw_bytes()`, `raw_byte_stream()`, `set_reject()`, `forward()` and
  `reply()` (read from `worker-0.9.0/src/email.rs` and `worker-macros-0.9.0/src/event.rs`).
- Instances: records in the Directory with `AccountRef::Home` or `Linked`, zones, sites and one
  site Worker per instance (`src/capabilities/src/instances/record.rs`). Instance resources are
  made by plans the person approves, run by pigeon-sites (`instances/plan.rs`).

### Cloudflare

Email Routing now lives in the docs under **Email Service**; `/email-routing/` redirects there.

| Fact | Value | Source |
| --- | --- | --- |
| Inbound message size | 25 MiB; larger is rejected | [limits](https://developers.cloudflare.com/email-service/platform/limits/) |
| Routing rules per domain | 200 | same |
| Domains per zone (apex and subdomains, routing and sending together) | 30 | same |
| Catch-all rules | apex only; a subdomain gets literal addresses only | [subdomains](https://developers.cloudflare.com/email-service/configuration/subdomains/) |
| Inbound price | unlimited, free; the Worker is billed as Workers | [pricing](https://developers.cloudflare.com/email-service/platform/pricing/) |
| Enabling routing on a domain | Cloudflare adds MX records (`route1..3.mx.cloudflare.net`) and a DKIM record; the old provider's MX must be removed first | [domains](https://developers.cloudflare.com/email-service/configuration/domains/) |
| Authentication | Email Routing checks DMARC first and prepends `Authentication-Results: mx.cloudflare.net; spf=…; dkim=…; dmarc=…` | [Workers API](https://developers.cloudflare.com/email-service/api/route-emails/email-handler/) |
| The handler | `email(message, env, ctx)`; `message.from`/`to` are the envelope; `raw` is a stream; `rawSize` | same |
| Storage example | Cloudflare's own example stores the raw message in R2 and metadata elsewhere | [email storage](https://developers.cloudflare.com/email-service/examples/email-routing/email-storage/) |
| Worker memory | 128 MB per isolate | [Workers limits](https://developers.cloudflare.com/workers/platform/limits/) |

## Decisions

### 1. Where mail lands: a capture subdomain, never a zone's existing mail

Each instance gets a **capture subdomain** with literal addresses routed to its data Worker:

| Instance | Capture domain | First address |
| --- | --- | --- |
| `pigeon-security/platform` | `in.pigeon.dev` | `capture@in.pigeon.dev` |
| `noisypigeon` | `in.noisypigeon.com` | `capture@in.noisypigeon.com` |
| `willowgraysen` | `in.willowgraysen.com` | `capture@in.willowgraysen.com` |

- Why a subdomain: enabling routing on an apex replaces its MX records. noisypigeon.com receives
  mail at Proton, so its apex can never be routed. A subdomain has its own MX records and leaves
  the apex alone. The same rule for every instance keeps one code path.
- "Any emails sent to the email": the person points copies at the capture address. Proton,
  Fastmail, iCloud and Gmail can all forward or filter-forward to it. Old mail comes through the
  connectors (mail-connectors.md), not through forwarding.
- More addresses later (one per purpose, such as `receipts@in.noisypigeon.com`) are more literal
  rules on the same subdomain; up to 200. The address a message came to is kept
  (`Source::EmailRouting.envelope_to`), so a purpose address is a free label.
- The label `in` is a default in the record (`data.email.subdomain`), not code. The person may
  pick another, or the apex for a zone with no mail of its own.

**Open, for phase 0:** the subdomain docs start from "select the apex domain, then open
Settings". Whether a subdomain can be routed while the apex is not is not stated. Phase 0 tries
it on a throwaway subdomain of pigeon.dev (pigeon's own zone) and reads the DNS records
Cloudflare adds. If the apex must be enabled, noisypigeon.com cannot use Email Routing at all and
the fallback is an address per instance on pigeon.dev's capture subdomain (for example
`noisypigeon@in.pigeon.dev`), received by the home instance's data Worker, which passes the raw bytes on to the instance's
data Worker over the signed contract and keeps no copy
(data-worker.md decision 3).

### 2. The handler: store the bytes, queue a reference, nothing else

The instance's data Worker (data-worker.md) has the `email` handler. It does as little as
possible, so it stays well under CPU and memory limits and never loses a message:

1. Read the raw message (at most 25 MiB, so it fits in memory).
2. Hash it (SHA-256) and write it to the instance's data bucket at
   `blobs/sha256/{2}/{2}/{64}` (`ContentRef::key`) unless that key exists.
3. Send one queue message to `{instance}-ingest`: `{kind: "email", raw: ContentRef, source:
   Source::EmailRouting {envelope_from, envelope_to}, received_at}`. Under 1 KB; the content
   never goes through the queue (128 KB message limit).
4. Return. No forward, no reply, no reject.

If step 2 or 3 fails, the handler throws. Whether Email Routing then answers the sender with a
temporary failure (so the sender retries) or drops the message is not stated in the docs; phase
0 checks it. If it drops, step 3 moves after a second write: a small `inbox/{received_at}-{sha}`
marker that a Cron sweep turns into queue messages.

Parsing happens in the queue consumer, with `pigeon_email::parse` (decision 3), so a parser bug
never bounces mail: the raw bytes are already safe in R2 and can be parsed again.

### 3. The schema: `pigeon_email::Message` (built in phase 1)

A pure Rust crate, `src/email` (`pigeon-email`), with no Worker runtime, so the data Worker, the
connectors and the uploader all share it and the smoke suite tests it natively. It parses with
[`mail-parser`](https://crates.io/crates/mail-parser) 1.0 (Apache-2.0 OR MIT, builds for
`wasm32-unknown-unknown`, handles RFC 2047 words, charsets, quoted-printable, base64 and nested
multiparts).

| Part | Type | What it keeps |
| --- | --- | --- |
| Original | `raw: ContentRef` | SHA-256 and size of the exact bytes received. The bytes are in R2. This is the true original; everything else is derived from it and can be derived again. |
| Where from | `source: Source` | `EmailRouting`, `Imap`, `Gmail`, `Jmap` or `Upload`, each with what identifies the message there (envelope, mailbox and UID, Gmail id and labels, JMAP id). New sources are new variants. |
| Headers in full | `headers: Vec<Header>` | Every header line in order, repeats (`Received`) included: name as written, raw value as written, decoded value when different. |
| Threading | `message_id`, `in_reply_to`, `references` | Without angle brackets. |
| People | `from`, `sender`, `reply_to`, `to`, `cc`, `bcc`, `delivered_to: Vec<Address>` | Each `Address` has the display name, the address as written, the normalized address (lowercased) and the domain: what entity linking looks up (ingest.md decision 6). Groups are flattened. `participants()` lists every address with its role. |
| Body | `text: Option<String>`, `html: Option<ContentRef>` | The plain text for classification and chunking; the HTML as a blob. |
| Attachments | `attachments: Vec<Attachment>` | Each file as a `ContentRef` of its decoded bytes, with content type, filename, disposition and Content-ID. Inline images and forwarded messages (`message/rfc822`) are attachments too. |
| Taxonomy | `taxonomy: Taxonomy` | `signals` read from headers alone at parse time (reply, forward, attachments, `List-Id`, `List-Unsubscribe`, `Auto-Submitted`, `Precedence`, SPF/DKIM/DMARC results, mailer); `doc_type`, `labels` and `fields`, empty until classification fills them. |
| Version | `schema: u32` | `SCHEMA_VERSION`, 1. |

`parse(raw, source)` answers `Parsed { message, blobs }`: the message and the bytes to store, one
per distinct SHA-256 (two copies of one PDF are stored once). Empty input and input that is not
MIME are errors, never panics. Parsing is deterministic: the same bytes give the same message.

**Extensible without migrations:**
- Fields are only added. Every optional field has a serde default and is skipped when empty, so
  a version-1 reader reads a version-2 record and a version-2 reader reads version 1.
- The taxonomy is open. `Label {scheme, value, confidence, by}` takes any vocabulary (`doc_type`,
  `category`, `language`, `project`) with no schema change. `Field {name, value, by}` takes any
  key field. `DOC_TYPES` is the starting vocabulary, not a closed enum.
- `by` names the rule or the model and its version, so a label can be traced and re-made
  (ingest.md decision 5).

### 4. Identity and duplicates of a message

The same email arrives many times: in the capture address, in Gmail, in an IMAP folder, in an
export. Each copy's bytes differ (each server adds its own `Received` and `Delivered-To`), so the
raw SHA-256 alone does not find them.

- **Item id**: the raw SHA-256. Two byte-identical copies are one item with two sightings.
- **Message fingerprint** (ingest.md decision 2, tier 2): SHA-256 over the normalized
  `Message-ID`; when there is none, over normalized From, Date, Subject and the text body's
  hash. Copies with one fingerprint are one *message* with several items; queries count it once
  and show where it was seen.
- Attachments are their own items, keyed by their own SHA-256, linked to the message by an
  `attachment_of` relation. The same invoice PDF sent three times is stored once and linked
  three times.
- A forwarded `message/rfc822` attachment is parsed again as an email item in its own right and
  linked `forwarded_in`.

### 5. Taxonomy: signals now, classification in the pipeline

- At parse: `Signals`, from headers only, deterministic and free. They cut model calls: a message
  with `List-Unsubscribe` and `Precedence: bulk` is a newsletter candidate before any model runs.
- In the pipeline (ingest.md decision 5): a document type from `DOC_TYPES`, labels and key fields
  (amount, currency, date, counterparty), each with the model and version that made it.
- Where it is kept: the JSON `Message` keeps the taxonomy as parsed; the Iceberg tables
  `classifications` and `fields` keep every classification run, so the latest wins and older
  ones stay for comparison. "All financial records" is a filter on the table, not on the JSON.

### 6. Spam, failures and safety

- Everything that reaches the capture address is kept, spam included, with its
  `Authentication-Results`. Classification labels spam (`scheme: "spam"`); queries leave it out
  by default. Nothing is deleted on a guess.
- The first `Authentication-Results` header from `mx.cloudflare.net` is the one to trust (the
  docs say other such headers may come from upstream servers). `Signals.auth` keeps all of them
  in order; ingest reads the first.
- No outbound mail. The handler never replies, so the capture address cannot be used to send.

### 7. Off by default

- No routing rule exists until the person approves the instance's `data.email` record and the
  DNS step. Without it no mail reaches the Worker.
- The data Worker's `email` handler, if it ever runs without `data.email.enabled` in its
  configuration, rejects with "this address is not set up" and stores nothing.
- `data_status {instance}` (data-worker.md) answers "email capture is not set up" and the steps
  below.

## Phases

| Phase | What | Lands with | Who |
| --- | --- | --- | --- |
| 0 | Spikes on pigeon.dev only: route a throwaway subdomain address to a throwaway Worker; read the DNS records added; does the apex need enabling; what a thrown handler does to the sender; the largest message that parses within limits | nothing kept | agent, person's go-ahead for the DNS step |
| 1 | `pigeon-email`: schema and parser, smoke scenario `email_parses_a_multipart_message` | **built** on `claude/ai-agent-NVhZ3wCsS5dT`, off (a library nothing calls) | executor, reviewed by planner |
| 2 | The data Worker's `email` handler and the parse stage in its ingest consumer | needs data-worker.md phases 1 and 2 | agent |
| 3 | Routing per instance: capture subdomain and literal rule, as instance plan steps (`EnableEmailRouting`, `AddEmailRule`) in the record's account | person's passkey on the record and go-ahead for the DNS step | agent, person |
| 4 | Message fingerprint and relations in the pipeline (decision 4) | ingest.md phase 3 | agent |
| 5 | Forwarding from existing mailboxes to the capture addresses | person's own mail settings | person |

## Follow-ups for the person

1. Decide the capture label (default `in`) for each of pigeon.dev, noisypigeon.com and
   willowgraysen.com, or say "apex" for a zone that has no other mail.
2. Before phase 0: say go for one throwaway subdomain on pigeon.dev (`in-test.pigeon.dev`) and
   the DNS records Email Routing adds there. pigeon removes it after.
3. Before phase 3, per instance: approve the `data.email` record on the console with your
   passkey, then say go for the DNS step in that session. noisypigeon.com's Proton records stay
   as they are; the step's dry run shows that.
4. After phase 3: in Proton, Fastmail, iCloud and Gmail, add a forwarding rule (or a filter that
   forwards) to `capture@in.<zone>`. Each provider checks the destination by sending it a code;
   the code arrives as a captured message, and `data_item` shows it.
5. The repository's crate map: `src/email/*` matches no Worker in `ci/workers.toml`, so a landing
   that touches it counts as shared and deploys every Worker. When the data Worker exists, add
   `src/email/*` to its lane and to core's (a gate change, yours).

## Log

- 2026-11-22: design written overnight from the person's brief (#4). Email Routing found under
  Email Service in Cloudflare's docs; catch-all is apex-only, so capture uses literal addresses on
  a subdomain. Phase 1 handed to one executor with the schema above as its spec; the smoke
  scenario parses a multipart message with a folded header, encoded words, a group, an inline
  image and the same PDF twice. Nothing deployed.
- 2026-11-22 23:40 UTC: handoff. The AI agents planner session wraps up; this plan's next
  steps wait on the person's answers (`overnight-2026-11-22.md`, "#4–#7 Data platform" and
  its handoff section), then an AI Gateway planner.

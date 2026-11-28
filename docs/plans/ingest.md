# Ingest: content-addressed files on R2, Iceberg as the record, Vectorize as the index

Status: **designed** (2026-11-22; decision 11 revised the same evening with the person's
answer on where the data lives). **Decision 13 added 2026-11-23**: the ingest runner, a
container next to the data that hashes every file and copies only what R2 does not certainly
hold; it is being built overnight (planner branch `claude/ai-agent-lnX2my6PYsAz`), landed
latent: nothing is deployed into DigitalOcean, Scaleway, Backblaze or an instance account
until the person provides keys and approves a run. The first code it relies on, the email
schema, is built off by default on the code branch named in [email.md](email.md).

**Lite tier (2026-11-26, [email.md](email.md)).** Before the Iceberg tables exist, the home
instance keeps the record rows `items`, `sightings` and `emails` in D1 `pigeon-data`, with
exactly decision 3's columns (lists and structs as JSON text), so the later replay into Iceberg
needs no rewrite. Bytes follow decision 9's layout in bucket `pigeon-data`; decision 10's B2
backup is built with it. Vectorize, classification and entities wait for the full tier. Decision
13.4's dedupe index also has a `sightings` table: when the runner joins the lite store its index
moves to its own D1 database (email.md, "D1 table names").

Related plans: [email.md](email.md) (the first source), [data-worker.md](data-worker.md) (where
this runs), [mail-connectors.md](mail-connectors.md) (mail pulled from mailboxes),
[linked-accounts.md](linked-accounts.md) (instances).

## Goal

The person's words (#5): "Design a capability to use Cloudflare Vector Database. I would like to
use the Cloudflare Vector database to be how data is ingested into Cloudflare to be used for
later data enrichment and presentation operations. I have roughly 1TB of data, around 4-5TB
un-deduped. Design the ingestion to use multiple indicators to confirm a file is unique / not
already ingested. We should have a mechanism to back this up to the Backblaze bucket. Look at
ways to optimize this data: potentially a schema representation of contents in the vector DB and
Apache Iceberg over R2 for the actual files. We will have all kinds of images, documents, and
other file formats, so design a general solution; the first use case is email."

**The target use** (out of scope to build now; designed for from the start): a website where the
person asks "Show me all financial records for <company>" or "Show me all correspondence with
<business/person>" and gets **complete** results back. Three things follow, and this plan is
built around them:

1. **Entity resolution.** An Iceberg table of people and organisations with aliases, email
   addresses and domains (later phone numbers and account numbers). Every message or document is
   linked to entity IDs at ingest. "Correspondence with X" resolves through this table (all of
   X's addresses and domains), never through vector similarity. (Decision 6.)
2. **Classification at ingest.** Each item gets a document type and extracted key fields
   (amount, currency, date, counterparty) stored in Iceberg, so "all financial records" is an
   exact filter. (Decision 5.)
3. **Roles.** Iceberg on R2 is the source of truth (rows plus original blobs, backed up to B2).
   Vectorize is a derived, rebuildable index. Queries are hybrid: exact filters on Iceberg plus
   semantic recall from Vectorize, merged, then hydrated from Iceberg and R2. (Decisions 1, 4
   and 7.)

Hard lines:
- Nothing is dropped on a guess. Exact duplicates are recorded as sightings; near duplicates are
  linked, never deleted.
- Every derived value (classification, field, entity link, embedding) records what made it
  (model, model version, prompt version, rule) so it can be made again.
- Data stays in the instance's own Cloudflare account and its B2 backup.
- Off by default; no credential exists yet.

## What is true today (checked 2026-11-22)

### pigeon

- No data code. The email schema and parser (`pigeon-email`) are the first piece, built off
  (email.md, phase 1).
- workers-rs 0.9 has bindings for R2, D1, Queues, Workers AI (`Env::ai`), TCP sockets and the
  `email` event. It has **no Vectorize or Pipelines binding**; those are reached through
  `wasm_bindgen` on the raw binding object or through the HTTP API (phase 0 picks).
- pigeon already keeps blobs by content for sites: `blobs/{sha256}` plus manifests
  (linked-accounts.md decision 3). This plan uses the same idea.

### Cloudflare

Two renames since the person's words: **R2 Data Catalog is now "Basin Catalog"** (GA; old APIs
keep working for now) and **R2 SQL is now "Basin SQL"**; Pipelines is **"Basin Pipelines"**.

| Product | Limit or price that shapes this design | Source |
| --- | --- | --- |
| Vectorize | 20,000,000 vectors per index; 1,536 dimensions max; vector id 64 bytes; 10 KiB metadata per vector; **10 metadata indexes** per index, each indexing the first **64 bytes** of a string; `topK` 50 with metadata (100 without); upsert batch 1,000 from Workers (5,000 over HTTP); filter JSON under 2,048 bytes | [limits](https://developers.cloudflare.com/vectorize/platform/limits/), [metadata filtering](https://developers.cloudflare.com/vectorize/reference/metadata-filtering/) |
| Vectorize filters | `$eq $ne $in $nin $lt $lte $gt $gte` on string, number, boolean; **no "array contains"**; namespace filter applied first; range accuracy drops near 10M vectors | [metadata filtering](https://developers.cloudflare.com/vectorize/reference/metadata-filtering/) |
| Vectorize price | stored: 10M dimensions included, then $0.05 per 100M; queried: 50M included, then $0.01 per million, counted as (stored vectors + queries) × dimensions in a month with queries | [pricing](https://developers.cloudflare.com/vectorize/platform/pricing/) |
| Basin Catalog | managed Iceberg REST catalog in an R2 bucket; catalog operations 1M/month included then $9/M; compaction $0.005/GB; snapshot expiry free | [overview](https://developers.cloudflare.com/basin-catalog/), [pricing](https://developers.cloudflare.com/basin-catalog/platform/pricing/), [maintenance](https://developers.cloudflare.com/basin-catalog/table-maintenance/), [deleting data](https://developers.cloudflare.com/basin-catalog/deleting-data/) |
| Basin SQL | **read-only** (no INSERT/UPDATE/DELETE/CREATE); Parquet only; joins, CTEs, window functions and `QUALIFY` supported; $2.50/TB scanned, 10 MB minimum per query, 10 GB/month included | [limitations](https://developers.cloudflare.com/basin-sql/reference/limitations-best-practices/), [pricing](https://developers.cloudflare.com/basin-sql/platform/pricing/) |
| Basin Pipelines | the way Workers write Iceberg: a stream (Worker binding or HTTP) into a sink that writes a Basin Catalog table; 20 streams, 20 sinks, 20 pipelines per account; 5 MB per ingest request; roll interval at least **60 s** for catalog sinks; ingress free; Iceberg sink $0.06/GB after 50 GB/month; the sink needs an API token with R2 Admin Read & Write | [limits](https://developers.cloudflare.com/basin-pipelines/platform/limits/), [pricing](https://developers.cloudflare.com/basin-pipelines/platform/pricing/), [catalog sink](https://developers.cloudflare.com/basin-pipelines/sinks/available-sinks/r2-data-catalog/), [writing](https://developers.cloudflare.com/basin-pipelines/streams/writing-to-streams/) |
| R2 | 5 TiB per object; 5 GiB single PUT, multipart to 10,000 parts; 1 write per second to one key; Standard $0.015/GB-month, Infrequent Access $0.01 (+$0.01/GB retrieval, 30-day minimum); Class A $4.50/M, Class B $0.36/M; no egress fee | [limits](https://developers.cloudflare.com/r2/platform/limits/), [pricing](https://developers.cloudflare.com/r2/pricing/) |
| R2 event notifications | `object-create` (Put, Copy, CompleteMultipartUpload) and delete events to a Queue, filtered by prefix and suffix | [event notifications](https://developers.cloudflare.com/r2/buckets/event-notifications/) |
| Super Slurper | bulk copy into R2 from any S3-compatible store; **Backblaze B2 is a tested source** | [Super Slurper](https://developers.cloudflare.com/r2/data-migration/super-slurper/) |
| Queues | 128 KB message; batch 100; 5,000 messages/s per queue; 250 concurrent consumers; consumer 15 min wall, CPU up to 5 min; retention up to 14 days; 1M ops/month included then $0.40/M (a message is about 3 ops) | [limits](https://developers.cloudflare.com/queues/platform/limits/), [pricing](https://developers.cloudflare.com/queues/platform/pricing/) |
| Workers | 128 MB memory; CPU 5 min max (30 s default); 10,000 subrequests; request body 100 MB on Free and Pro zones; Paid: 10M requests and 30M CPU-ms included, then $0.30/M and $0.02/M CPU-ms | [limits](https://developers.cloudflare.com/workers/platform/limits/), [pricing](https://developers.cloudflare.com/workers/platform/pricing/) |
| D1 | 10 GB per database; 2 MB row; 100 bound parameters; 1,000 queries per invocation; 50M rows written and 25B read per month included | [limits](https://developers.cloudflare.com/d1/platform/limits/), [pricing](https://developers.cloudflare.com/d1/platform/pricing/) |
| Workers AI | $0.011 per 1,000 neurons, 10,000 neurons/day free. Embeddings: **`@cf/google/embeddinggemma-2`** (text, images, video and audio in one 768-dimension space, Matryoshka 128/256/512/768, 8,192-token context, $0.0017/M tokens), `@cf/baai/bge-m3` ($0.012/M), `@cf/qwen/qwen3-embedding-0.6b` ($0.012/M). Text generation, e.g. `@cf/qwen/qwen3-30b-a3b-fp8` ($0.051/M in, $0.335/M out). `@cf/microsoft/resnet-50` $2.51/M images. Reranker `@cf/baai/bge-reranker-base`. `toMarkdown` converts PDF, Office, HTML and images, free for most formats | [pricing](https://developers.cloudflare.com/workers-ai/platform/pricing/), [embeddinggemma-2](https://developers.cloudflare.com/workers-ai/models/embeddinggemma-2/), [toMarkdown](https://developers.cloudflare.com/workers-ai/features/markdown-conversion/) |
| Workers AI rate limits | text embeddings 3,000 requests/min; text generation **300/min** (models that need Workers Paid: 20/min, 50/min with prepaid AI Gateway credits); image classification 3,000/min | [limits](https://developers.cloudflare.com/workers-ai/platform/limits/) |
| Backblaze B2 | $6.95/TB-month (read from the pricing page's price table, 2026-11-22); S3-compatible API; ingress free | [pricing](https://www.backblaze.com/cloud-storage/pricing) |

## Overview

```
 sources                       data Worker in the instance's account (data-worker.md)
 ───────                       ─────────────────────────────────────────────────────
 Email Routing ──email()──┐
 Gmail / JMAP / IMAP ─────┤    1 identify ──▶ 2 extract ──▶ 3 classify + link ──▶ 4 chunk + embed
 uploader (local disks) ──┤      │  sha256, size,  │  pigeon-email,   │  rules, Workers AI,   │  Workers AI
 cloud copier, Slurper ───┘      │  sightings      │  toMarkdown,     │  entities, fields     │  embeddings
                                 │                 │  pHash, shingles │                       │
     every stage reads/writes:   ▼                 ▼  children ──▶ 1  ▼                       ▼
   R2 blobs/sha256/…  (originals, immutable)    Basin Pipelines ──▶ Iceberg tables      Vectorize
   R2 derived/…       (text, thumbnails)        (append-only rows)   (source of truth)   (derived index)
   D1                 (near-duplicate buckets, sync state: operational, rebuildable)
            │
            └── R2 event notifications ──▶ backup queue ──▶ Backblaze B2 (S3 API)

 query (decision 7): question ─▶ entities (Iceberg) ─▶ exact set (Basin SQL) ┐
                                          semantic recall (Vectorize) ───────┴▶ merge ─▶ hydrate
```

## Decisions

### 1. Roles: R2 holds the bytes, Iceberg holds the record, Vectorize is derived

- **Originals** are R2 objects named by content: `blobs/sha256/{2}/{2}/{64}`, written once,
  never changed. The same key scheme as `pigeon_email::ContentRef::key`.
- **The record** is a set of Iceberg tables in Basin Catalog on the same bucket (decision 3):
  what each item is, where it was seen, what it contains, who it involves, what it was
  classified as. Every question the website asks for completeness is answered here.
- **Derived files** (extracted text as Markdown, thumbnails, page images) are R2 objects under
  `derived/{item_id}/…`. They can be made again from the originals.
- **Vectorize** holds embeddings of chunks, each carrying its Iceberg row ids and filterable
  metadata (decision 4). It can be rebuilt from the `chunks` table plus the model.
- **D1** holds operational indexes that need fast point lookups the lake cannot give: the
  near-duplicate buckets (decision 2) and connector sync state (mail-connectors.md). It can be
  rebuilt from Iceberg.
- **B2** holds a copy of the whole bucket: originals, derived files and Iceberg's data and
  metadata files (decision 10).

Why not Vectorize as the store, as the person's words first suggest: its metadata is capped at
10 KiB per vector with 10 indexed fields of 64 bytes, it cannot filter on "any of these entity
ids", and `topK` is at most 50 with metadata. A query that must be complete cannot rest on it.
"Ingested into Cloudflare through the vector database" becomes: every item is in Vectorize for
recall, and in Iceberg for the record.

### 2. Is this file new? Several indicators, in tiers

| Tier | Indicator | Applies to | Where it is kept | What a match means |
| --- | --- | --- | --- | --- |
| 1 exact | **SHA-256** of the bytes | everything | the R2 key itself; `items.sha256` | the same bytes: no new item; a new row in `sightings` |
| 1 exact | **size** in bytes | everything | `items.size` | checked with SHA-256: equal SHA-256 with a different size is reported as corruption, never merged |
| 1 exact | **BLAKE3** of the bytes | everything the uploader hashes | `items.blake3` | a second, independent hash, computed locally where it is cheap; recorded and cross-checked, never the key |
| 2 same content | **message fingerprint**: SHA-256 of the normalized Message-ID, or of From, Date, Subject and the body hash when there is none | email | `emails.fingerprint`, D1 | the same message from another source (each server adds its own headers): a new item, linked `same_message` |
| 2 same content | **text hash**: SHA-256 of the normalized extracted text (Unicode NFKC, whitespace collapsed, lowercased) | documents with text | `items.text_sha256` | the same document saved again (another PDF producer, other metadata): new item, linked `same_text` |
| 3 near | **perceptual hashes**: 64-bit pHash and dHash; plus EXIF capture time and camera | images, video key frames | `image_hashes`, D1 buckets | Hamming distance ≤ 6 on pHash and ≤ 10 on dHash: a resized, recompressed or lightly edited copy; linked `near_duplicate` with the distance |
| 3 near | **MinHash** over word 5-gram shingles (128 hashes, LSH with 32 bands of 4) | documents and email bodies with over 50 words | `text_sketches`, D1 buckets | estimated Jaccard ≥ 0.9: a revised or quoted copy; linked `near_duplicate` with the estimate |

Rules:
- Tier 1 decides storage: a blob is stored once. The lookup is the R2 key: a `HEAD` on
  `blobs/sha256/…` (Class B, $0.36 per million). No separate "seen" table to keep in step.
- Tiers 2 and 3 decide **relations**, never deletion. A near-duplicate invoice is still a
  financial record; the query collapses a cluster to one result and shows the others under it.
  Thresholds are settings with the rule version recorded on each relation, so they can be tuned
  and re-run.
- SHA-256 is the key, not BLAKE3, because Workers compute SHA-256 natively (WebCrypto and
  `crypto.DigestStream`) and R2 checks a SHA-256 sent with a PUT; BLAKE3 in WASM is software
  only. The uploader computes both (decision 11), so the person's existing tools that use BLAKE3
  can be matched.
- Near-duplicate lookups use D1: one row per (band, bucket value, item). A new image's pHash is
  split into four 16-bit bands. If two hashes differ in at most 7 bits, at least one band
  differs in at most 1 bit (four bands each differing in 2 or more bits make 8 or more). So
  each band is probed with its exact value and its 16 one-bit flips: 4 × 17 = 68 values, one
  `IN` query per band. Candidates are then checked exactly. Documents use the 32 MinHash bands.
- Every item records `first_seen` and every sighting records its source and path, so "where did
  this come from" is always answerable, which matters with 4 TB of copies.

### 3. Iceberg tables (namespace `data`, one catalog per instance bucket)

Writes come only through Basin Pipelines (Basin SQL is read-only). So every table is
**append-only with versioned rows**: a correction is a new row with a higher `version` (or later
`at`), and readers take the latest per key (`QUALIFY row_number() OVER (PARTITION BY id ORDER
BY version DESC) = 1`, which Basin SQL supports). Real deletes (a privacy request, a mistake)
are a separate, rare job with an Iceberg engine (PyIceberg in a Container, per
[deleting data](https://developers.cloudflare.com/basin-catalog/deleting-data/)).

Ids: `item_id` is the content's SHA-256 (hex) for blobs; `entity_id` is `e_` plus 26 characters
(ULID); every other row id is the SHA-256 of its natural key, so a re-run writes the same id.

| Table | Key | Columns (beyond the key) | Partition |
| --- | --- | --- | --- |
| `items` (the manifest) | `item_id` | `sha256`, `blake3`, `size`, `media_type` (sniffed), `kind` (email, document, image, video, audio, archive, other), `filename` (first seen), `text_sha256`, `parent_id` (attachment or archive member of), `first_seen`, `status` (stored, extracted, classified, embedded, failed), `error`, `version` | `bucket(32, item_id)` |
| `sightings` | (`item_id`, `source`, `locator`) | `source` (email_routing, gmail, jmap, imap, upload, slurper), `account`, `locator` (path, mailbox and UID, Gmail id), `seen_at`, `extra` (JSON: labels, flags) | `month(seen_at)` |
| `emails` | `item_id` | the `pigeon_email::Message` fields: `schema`, `fingerprint`, `message_id`, `in_reply_to`, `references`, `subject`, `date`, `from`/`to`/`cc`/`bcc`/`reply_to`/`sender`/`delivered_to` as `list<struct<name, address, normalized, domain>>`, `headers` as `list<struct<name, value, decoded>>`, `signals` as a struct, `text_ref`, `html_ref` | `month(date)` |
| `relations` | (`from_id`, `kind`, `to_id`) | `kind` (attachment_of, member_of, same_message, same_text, near_duplicate, forwarded_in, reply_to, thread), `score`, `rule`, `rule_version`, `at` | `bucket(16, from_id)` |
| `entities` | `entity_id` | `kind` (person, organisation), `display_name`, `status` (provisional, confirmed), `merged_into`, `by` (rule, model or person), `version`, `at` | none (small) |
| `entity_aliases` | (`entity_id`, `kind`, `value`) | `kind` (email, domain, name, phone, account_number), `value` (normalized: lowercased email, registrable domain, E.164 phone, account number as a salted hash plus its last 4), `source`, `confidence`, `by`, `at`, `retracted` | `identity(kind)` |
| `entity_links` | (`item_id`, `entity_id`, `role`) | `role` (from, to, cc, bcc, reply_to, sender, delivered_to, counterparty, issuer, recipient, mentioned), `method` (address_exact, domain, name_match, model), `confidence`, `by`, `at` | `bucket(32, entity_id)` |
| `entity_merges` | (`from_id`, `into_id`) | `by` (rule, person), `reason`, `at`, `undone` | none |
| `classifications` | (`item_id`, `run`) | `doc_type`, `labels` as `list<struct<scheme, value, confidence>>`, `model`, `model_version`, `prompt_version`, `rules_version`, `at` | `bucket(16, item_id)` |
| `fields` | (`item_id`, `name`, `run`) | `name` (amount, currency, date, counterparty, invoice_number, account_last4, period_start, period_end, tax_year…), `value_text`, `value_number`, `value_date`, `confidence`, `model`, `model_version`, `run`, `at` | `identity(name)` |
| `chunks` | `chunk_id` | `item_id`, `ord`, `text` (or `media_ref` for an image chunk), `span` (page, byte range, or header/body), `tokens`, `embedding_model`, `embedding_version`, `dims`, `index` (Vectorize index name), `at` | `bucket(32, item_id)` |
| `image_hashes`, `text_sketches` | `item_id` | pHash, dHash, EXIF fields; MinHash signature | `bucket(16, item_id)` |
| `runs` | `run` | stage, model, versions, started, finished, counts | none |

- Ten sinks are needed for the big tables (`items`, `sightings`, `emails`, `relations`,
  `entity_links`, `classifications`, `fields`, `chunks`, plus one shared sink for the small
  tables through a SQL transform that routes by a `table` column). That fits the 20-sink
  account limit for one instance per account. Two instances in one account would not; phase 0
  checks whether one stream can fan out to several tables
  ([fan-out example](https://developers.cloudflare.com/basin-pipelines/examples/bluesky-firehose-fanout/)).
- Freshness: catalog sinks roll at least every 60 s, so a new item is queryable one to two
  minutes after it is written. The website's "complete" promise is about the record, not about
  the last minute; `data_status` shows the backlog.
- Schema changes: Iceberg adds columns without rewriting data; the sink schema follows.
  Columns are only added, as in `pigeon-email`.
- Compaction and snapshot expiry are turned on per table (decision 12's costs include them).

### 4. Vectorize: one index per instance, derived and replaceable

- Index `{instance}-data-v{n}`, 768 dimensions, cosine, model `@cf/google/embeddinggemma-2`.
  One multimodal model puts text and images in one space, so "the photo of the receipt" and "the
  receipt PDF" can meet. It is also the cheapest embedding model listed. `bge-m3` (1,024
  dimensions) is the fallback if quality on the person's languages is poor; phase 0 compares on
  a sample.
- **Vector id** = `chunk_id` truncated to 64 bytes of hex (the limit). **Metadata** (at most
  10 KiB, never content beyond a 200-character snippet):

  | Key | Indexed | Why |
  | --- | --- | --- |
  | `item` | no | the Iceberg row id to hydrate |
  | `chunk` | no | the chunk row |
  | `kind` | yes (string) | email, document, image… |
  | `doc_type` | yes (string) | invoice, receipt… |
  | `day` | yes (number, `yyyymmdd`) | date range |
  | `counterparty` | yes (string) | the item's main counterparty entity id, for scoped recall |
  | `sender` | yes (string) | the sender's entity id |
  | `source` | yes (string) | email_routing, gmail… |
  | `financial` | yes (boolean) | doc_type in the financial set |
  | `cluster` | yes (string) | the duplicate cluster's id, to collapse results |
  | `emb` | no | model and version, to check a query uses the same model |

  Seven indexed keys, three to spare.
- **Vectorize cannot say "any of these entities"**: an item linked to five entities can only
  carry one or two in indexed metadata, and there is no array match. So Vectorize is never asked
  "everything about X". It is asked "what is about this question", optionally narrowed to one
  counterparty or a doc type and date range, and its answers are joined to the exact set
  (decision 7).
- **Re-embedding** (a new model or dimension) writes a new index `-v{n+1}` from the `chunks`
  table, then switches the query path and deletes the old index. All vectors in an index share
  one model; the `emb` key and `chunks.embedding_model` say which.
- **Size**: about 5.1 million vectors for the assumed data (decision 12), a quarter of one
  index's 20 million. The plan splits by kind into a second index if it passes 15 million.

### 5. Classification at ingest

Each item gets a `doc_type` and key fields, recorded with what made them.

1. **Text first.** Email: `pigeon_email::Message.text`. Documents: `toMarkdown` (PDF, Office,
   HTML, spreadsheets, images), kept as `derived/{item}/text.md`. Images: no text step unless
   `toMarkdown`'s image path finds text (phase 0 measures its cost on 100 scans).
2. **Rules** (free, versioned as `rules_version`): `Signals` (bulk, list, auto-submitted →
   newsletter or notification), sender entity known as a bank or card issuer, filename words
   (invoice, Rechnung, receipt, statement), attachment MIME types. Rules may decide alone when
   certain (a mailing list is not an invoice) and otherwise give the model hints.
3. **Model**: a Workers AI text model with a JSON schema answer: `doc_type` from `DOC_TYPES`,
   `labels`, and fields `{amount, currency, date, counterparty, invoice_number, account_last4,
   period}` with a confidence each. Default `@cf/qwen/qwen3-30b-a3b-fp8` (cheap, within the 300
   requests/minute class if it does not need Workers Paid; phase 0 checks the model page).
   Input: the first 1,500 tokens of text plus the email's From, Subject and attachment names.
4. **Images**: `@cf/microsoft/resnet-50` labels plus EXIF; a photo of a receipt is caught by the
   embedding being near "receipt" text and by `toMarkdown` finding text, then classified as text.
5. **Recorded**: a `classifications` row (`model`, `model_version`, `prompt_version`,
   `rules_version`, `run`) and one `fields` row per field. Low confidence adds the label
   `review: needed`.
6. **Re-processing**: `data_reprocess {instance, stage, where}` selects items whose latest row
   has an older version and sends them to the stage's queue again. New rows are appended; the
   newest wins; old rows stay for comparison.

"All financial records" is then exact: the latest `classifications` row per item with
`doc_type IN ('invoice','receipt','bank_statement','card_statement','tax','payslip','contract')`.

### 6. Entity resolution

Entities are people and organisations. Every item is linked to entity ids at ingest, by rules
first and models second, and the links say how they were made.

**Linking at ingest:**
1. Every address in `Message.participants()` is looked up as an `email` alias. Found: link with
   `method: address_exact`, confidence 1.
2. Not found: create a **provisional person** with that email alias (display name from the
   header). If the domain is not a mailbox provider (a list: gmail.com, googlemail.com,
   icloud.com, me.com, proton.me, protonmail.com, pm.me, fastmail.com, outlook.com, hotmail.com,
   yahoo.com, gmx.*, web.de…), look up or create a **provisional organisation** with that
   registrable domain as a `domain` alias, and link the person to it (`relations` `member_of`).
   The item is linked to both: the person by role, the organisation by `method: domain`.
3. Counterparty and issuer fields from classification (decision 5) are matched against `name`
   aliases: normalized (lowercased, punctuation and legal suffixes such as Ltd, GmbH, Inc, LLC,
   SA, BV removed), exact first, then trigram similarity ≥ 0.85 among organisations. A match
   links with `method: name_match` and its score; no match creates a provisional organisation
   with the name alias.
4. Phone numbers (E.164) and account numbers (salted hash plus last 4) become aliases when
   classification extracts them, so a bank statement links to the bank by account.

**Merging:** two entities are one when the person says so, or by rule when they share an exact
alias of kind email, phone or account number. A merge is a row in `entity_merges`; queries map
every id through the merge chain (a small table, read whole). An undo is another row. Nothing is
rewritten.

**"All correspondence with X":**

```sql
WITH x AS (            -- X and everything merged into X
  SELECT from_id AS id FROM entity_merges_current WHERE into_id = :x
  UNION SELECT :x
)
SELECT DISTINCT l.item_id
FROM entity_links_current l JOIN x ON l.entity_id = x.id
WHERE l.role IN ('from','to','cc','bcc','reply_to','sender','delivered_to')
UNION
SELECT DISTINCT l.item_id                 -- documents where X is the counterparty
FROM entity_links_current l JOIN x ON l.entity_id = x.id
WHERE l.role IN ('counterparty','issuer','recipient')
```

`*_current` are views (or saved queries) taking the latest row per key. An organisation also
pulls in its members' correspondence when the person asks for it ("with anyone at X").

**Ambiguity is shown, not hidden:** a name that matches two entities is answered with both and
a question, never a silent guess. Provisional entities from one-off senders are expected; the
website will offer merging.

### 7. The hybrid query path

1. **Understand the question**: a model turns it into `{entity mentions, doc types, date range,
   free text}`; the website can also offer these as fields.
2. **Resolve entities** through `entity_aliases` (exact, then fuzzy), mapped through merges.
3. **Exact set** from Iceberg with Basin SQL: items linked to the entities (decision 6) and/or
   with the doc types (decision 5) in the date range. This set is complete by construction.
4. **Semantic recall** from Vectorize for the free text: `topK` up to 50 with metadata, filtered
   by `doc_type`, `day` range and, for one entity, `counterparty`. Repeated with paging by date
   windows when more are needed.
5. **Merge**: when there are exact filters, the answer is the exact set, ranked by semantic
   score where the free text matched and by date otherwise (re-ranked with
   `bge-reranker-base`). Semantic hits outside the exact set are shown apart as "possibly
   related", never mixed into a "complete" list. With no exact filter, the answer is the
   semantic list and says it is not exhaustive.
6. **Collapse** duplicates by `cluster`, showing the copies under one result.
7. **Hydrate** rows from Iceberg (subject, date, fields) and originals from R2 through the data
   Worker (short-lived signed links, data-worker.md).
8. **Count**: every answer carries the exact count from step 3, so "complete" can be checked.

### 8. The pipeline: Queues for fan-out, each stage idempotent

| Queue | Message (refs only, under 1 KB) | Consumer does | Writes |
| --- | --- | --- | --- |
| `{instance}-ingest` | `{blob: ContentRef, source, locator, parent?}` | HEAD the blob; sniff the type; write `items` (if new) and `sightings` | `items`, `sightings` |
| `{instance}-extract` | `{item_id, kind}` | parse (`pigeon-email`), `toMarkdown`, image hashes, MinHash; children (attachments, archive members) go back to `ingest` with `parent` | `emails`, `relations`, `image_hashes`, `text_sketches`, `derived/…` |
| `{instance}-classify` | `{item_id}` | rules, model, entity linking | `classifications`, `fields`, `entities`, `entity_aliases`, `entity_links` |
| `{instance}-embed` | `{item_id}` | chunk (512 tokens, 64 overlap; one chunk per image), embed in batches, upsert to Vectorize (batches up to 1,000) | `chunks`, Vectorize |
| `{instance}-backup` | R2 event notification | copy the object to B2 (decision 10) | B2 |
| `*-dlq` | failures after retries | nothing automatic; `data_status` lists them | none |

- **Idempotent**: every stage's output ids are derived from its input and versions, so a retry
  or a re-run writes the same rows; readers take the latest.
- **Large files**: the Worker has 128 MB. Files over 64 MB (video, big archives, huge PDFs) are
  not loaded whole: hashing streams (`crypto.DigestStream`), and extraction goes to a Container
  job in the instance account (phase 6), with the item marked `status: deferred` until then.
- **Archives** (zip, tar, mbox, pst later): members become child items, each through `ingest`.
  An mbox is split into messages, each a `Source::Upload` email.
- **Back-pressure**: the classify queue is throttled to the model's rate limit (`max_concurrency`
  on the consumer), so the backlog sits in the queue (25 GB backlog, 14-day retention), not in
  failed calls.

### 9. R2 layout and storage class

Bucket `{instance}-data`, in the instance's jurisdiction (EU for noisypigeon, as its site
bucket):

| Prefix | What | Written |
| --- | --- | --- |
| `blobs/sha256/{2}/{2}/{64}` | originals | once |
| `derived/{item_id}/{name}` | text.md, thumbnail.webp, pages | by stages, replaceable |
| the catalog's warehouse prefix | Iceberg data and metadata files | by Basin Pipelines and compaction |
| `staging/…` | Super Slurper landing area (decision 11) | emptied after identify |

A lifecycle rule moves `blobs/` to Infrequent Access after 30 days: originals are read rarely
(hydration and re-extraction), and IA costs a third less ($0.01 against $0.015 per GB-month)
with a $0.01/GB retrieval. A re-extraction of everything once would cost about $10 in
retrieval, a month's saving. The person decides (follow-ups).

### 10. Backup to Backblaze B2

- **Continuous**: an R2 event notification (`object-create` on the whole bucket) sends each new
  object's key to `{instance}-backup`. The consumer streams the object from R2 to B2's
  S3-compatible API (`PUT`, SigV4 signed in Rust; multipart above 5 GB), at the same key in the
  backup bucket, with the SHA-256 in `x-amz-meta-sha256`. B2 checks its own SHA-1 on
  upload.
- **Everything is copied**, Iceberg data and metadata files included, so a restore has the
  record, not only the files.
- **Deletes are not mirrored.** B2 keeps what was copied. A privacy delete is an explicit job
  that deletes in both places and records it.
- **Reconciliation**: a daily Cron lists a slice of the bucket (by key prefix, 1/256 per day for
  originals, all of the catalog prefix) and compares sizes with B2's listing; differences are
  re-queued. `data_status` shows the last full pass.
- **Credentials**: a B2 application key restricted to the backup bucket, set by the person as a
  secret in the instance account (follow-ups). Server-side encryption
  (SSE-B2) on the bucket.
- **Restore**: Super Slurper copies B2 back into a new R2 bucket (B2 is a tested source). Iceberg
  tables are registered again from their latest metadata file (whether Basin Catalog can
  register an existing table is open; phase 0 checks, the fallback being a replay of the rows
  into new tables through the pipelines). Vectorize is rebuilt from `chunks`; D1 from Iceberg.
- **B2 is both a source and the backup target, kept apart.** The backup goes to a **new,
  separate bucket** (`pigeon-backup-{instance}`), never to a bucket that holds source data.
  Reasons: the source buckets stay strictly read-only (decision 11), one key never needs both
  read on the sources and write on the backup, and a restore never has to tell the person's
  original objects from pigeon's copies. The backup bucket's key is restricted to that bucket;
  each source bucket's key is read-only (follow-ups). Object Lock on the backup bucket is an
  option the person can turn on.
- **The existing B2 data stays where it is** after ingest. pigeon never deletes or moves source
  data; retiring a source bucket later is the person's own decision, made once the record
  shows every object in it as sighted and its bytes held in R2 and in the backup bucket.
  Until then the unique bytes are stored twice in B2 (source and backup), about $7–10 a month
  per terabyte-and-a-half, which buys an independent backup.

### 11. Bulk ingest: where the data is, and how it moves

**Where it is** (the person, 2026-11-22, answering this decision):

| Source | Size | Path |
| --- | --- | --- |
| DigitalOcean Spaces | 3.4 TB | cloud to cloud (main path) |
| Scaleway Object Storage | 0.8 TB | cloud to cloud (main path) |
| Backblaze B2 | 1–2 TB | cloud to cloud (main path) |
| Local disks | ~1 TB | the uploader |
| Mailboxes (several accounts) | 0.3–0.4 TB | [mail-connectors.md](mail-connectors.md) |
| **All** | **~6.5–7.6 TB raw**, of which ~5.2–6.2 TB in the three clouds | |

The person's earlier estimate was about 1 TB unique. Phase 4's listing pass replaces both
numbers with counts.

**Hard line: every source is read-only.** pigeon never writes, changes, tags or deletes anything
in a source bucket. Each source account is declared read-only in the `storage` capability
([storage-providers.md](storage-providers.md)), with a key that can only list and read (the
person creates them; follow-ups). The code refuses a write tool on a read-only account, and the
keys could not write anyway.

**Step 1: list everything, move nothing.** A listing job in the data Worker walks every declared
bucket with ListObjectsV2 (through `pigeon_core::s3`, the same client the `storage` tools use,
not tool call by tool call) and appends one row per object to an Iceberg table
`data.source_objects`: account, bucket, key, size, ETag, last modified, storage class, and the
time listed. Listings are cheap: about 8 million objects is about 8,000 list calls. DigitalOcean
and Scaleway do not charge per request; B2 charges list calls as class C transactions, with
2,500 free a day and then $0.004 per 1,000 ([B2 pricing](https://www.backblaze.com/cloud-storage/pricing)),
so a full pass costs cents.

**Step 2: deduplicate from the listings alone, before any byte moves.** Objects are grouped by
(size, ETag):
- **Strong groups**: the ETag is a plain 32-hex MD5. That is the S3 rule for an object uploaded
  in a single part without SSE-C or KMS, which the three providers follow for S3 uploads. Same
  size plus same MD5 is treated as the same bytes, and only one copy of the group is fetched.
- **Weak groups**: the ETag has a part count (`…-N`, multipart). The ETag then depends on the
  part size used at upload, so two copies of one file can differ, and equal ETags are only
  likely the same bytes. One copy is fetched and hashed; the others are fetched only if the
  hash later disagrees with a sample (one more copy per group is hashed to check).
- **Unknown**: objects uploaded through B2's native API, encrypted objects, or ETags in another
  form. These are fetched and hashed after landing, like local files. Phase 0 checks the ETag
  forms on a sample from each provider before the rule is trusted.
- **Size alone never decides.** Objects with the same size and different ETags are distinct.

Every listed object becomes a **sighting** of its group (decision 2), whether or not its bytes
were fetched, so the record knows every copy and where it lives.

**What this saves.** If the person's estimate holds (about 1–1.5 TB unique in 6.5–7.6 TB), the
clouds hold 5.2–6.2 TB, of which perhaps 1–1.5 TB is unique, so listing-based dedup skips about
4–5 TB of transfer, processing and R2 writes. In money the saving is small, because egress is
cheap here (below): at most about $40–55 of egress avoided. The real gains are time (days of
transfer), R2 Class A operations, and not hashing 4–5 TB in Workers. The listing pass reports the
actual figure before anything moves, and the person sees it on `data_status`.

**Step 3: pick the copy to fetch.** For each group, one source is chosen, in this order:
1. **Backblaze B2**: downloads to Cloudflare are free under Backblaze's partner programme
   ([B2 pricing](https://www.backblaze.com/cloud-storage/pricing); [the Cloudflare partnership](https://www.backblaze.com/blog/backblaze-and-cloudflare-partner-to-provide-free-data-transfer/)).
   Phase 0 confirms that a transfer from the B2 S3 endpoint to R2 is billed as free; if it is
   not, B2's general allowance still covers it (free egress up to 3 × the average stored, then
   $0.01/GB).
2. **Scaleway**: 75 GB of egress free each month, then €0.01/GB ([Scaleway pricing](https://www.scaleway.com/en/pricing/storage/)).
3. **DigitalOcean Spaces**: 1,024 GiB of outbound transfer included per Spaces subscription,
   then $0.01/GiB according to the docs ([Spaces pricing](https://docs.digitalocean.com/products/spaces/details/pricing/));
   DigitalOcean's marketing pricing page shows a different figure, so the person checks the bill
   after the first month (follow-ups).

Within a provider, the copy in the bucket the person marks as primary wins; then the oldest.

**Revised 2026-11-23 (decision 13).** For the three clouds, steps 2 to 4 are replaced by the
ingest runner: a container next to each source reads every object (free within the provider's
region), hashes it, and uploads only what R2 does not certainly hold. ETag grouping (step 2)
stays as a report figure only; an MD5 ETag is not strong enough to leave a file behind. Super
Slurper and the Worker copier below stay as fallbacks.

**Step 4: transfer.** Two mechanisms, chosen per bucket after the listing:
- **Super Slurper** (Cloudflare's bulk copy, no charge on Cloudflare's side beyond R2
  operations) for a bucket whose objects are mostly unique (say, over 70% chosen in step 3). It
  copies a bucket or a prefix into `staging/{account}/{bucket}/` in the instance bucket. It takes
  any S3-compatible source, and Backblaze B2, DigitalOcean Spaces and Scaleway are on its list of
  tested providers ([Super Slurper](https://developers.cloudflare.com/r2/data-migration/super-slurper/);
  [S3-compatible support](https://developers.cloudflare.com/changelog/2025-02-24-r2-super-slurper-s3-compatible-support/)).
  It copies whole buckets or prefixes, not a list of keys, so it suits mostly-unique buckets
  only. An R2 event on `staging/` feeds `ingest`, which hashes, keeps new blobs under `blobs/`
  and deletes the staged copy.
- **pigeon's copier** for everything else: a Queue of chosen objects, consumed by the data
  Worker, which streams each object from the source (a signed GET through `pigeon_core::s3`)
  straight into R2 (a single PUT up to 5 GB, multipart above), with no copy in Worker memory.
  It hashes as it streams (SHA-256 and BLAKE3), so a landed object already has its content
  address. It fetches only the chosen copy of each group.

**Order.** B2 first (free egress, and its data has to be in R2 before its role as backup target
is set up, decision 10), then Scaleway, then DigitalOcean. Within each: the strong groups first
(they need no hashing to decide), largest objects first, so bytes move from the start; then the
weak and unknown groups. Mail and local files run in parallel on their own paths.

**Throughput** (estimates; phase 0 measures):

| Step | Rate | For the clouds (~1–1.5 TB fetched) |
| --- | --- | --- |
| Listing | ~1,000 objects a call; tens of calls a second | ~8M objects in under an hour |
| Copier | limited by the source; assume 50–100 MB/s from each provider at once | 1–1.5 TB in about 3–8 hours |
| Super Slurper | Cloudflare's service; per-bucket, in parallel | measured in phase 0 |
| Local uploader | the person's uplink (below) | ~1 TB: 22 hours at 100 Mbit/s, 2.3 hours at 1 Gbit/s |
| Identify and extract | Queues at 5,000 messages/s, 250 consumers | not the bottleneck |
| Embedding | 3,000 requests/min, batches of ~50 chunks | 5.1M vectors in about 35 minutes of model time |
| Classification | 300 requests/min | ~800,000 items in about 44 hours; rules skip newsletters and notifications first |
| Gmail backfill | 100 raw messages/min per user (mail-connectors.md) | 500,000 messages in about 3.5 days |

**The uploader** (a Rust command-line program in this repository, run by the person; phase 4) is
now the path for the ~1 TB on local disks only:
1. Walks the given folders. For each file: size, SHA-256 and BLAKE3, computed locally (fast
   there; nothing leaves the machine for a duplicate).
2. Asks the data Worker `have` for up to 1,000 SHA-256 values at a time; the Worker answers from
   the content addresses already in R2, including those that arrived from the clouds.
3. Uploads only missing blobs to the data Worker, which streams them into R2: single PUT up to
   95 MB (under the 100 MB request body limit of Free and Pro zones), multipart for larger
   files, parts of 95 MB, resumable by part.
4. Sends a sighting for every file, duplicate or not (path, modified time, the machine's name).
5. Keeps a local journal (SQLite) so a stopped run resumes without hashing again.

Running the uploader after the clouds means local copies of cloud data are found as duplicates
by hash and never uploaded. The uploader also reads mbox and `.eml` folders (Proton's export,
Apple Mail folders) as `Source::Upload` and talks IMAP to a local Proton Mail Bridge
(mail-connectors.md).

### 12. Cost estimate

**Assumptions** (to be replaced by phase 4's counts): about 1–1.5 TB of unique bytes in about
6.5–7.6 TB raw (decision 11's table; about 8 million files seen); the item counts below are for
1 TB unique and scale with it; unique items: 500,000 emails, 500,000 documents and attachments, 1.5 million
images, 5,000 videos; text: about 2.4 billion tokens to embed (emails ~1,000 tokens, documents
~3,000, images ~260 each); 5.1 million chunks; 3,000 website queries a month.

One-time (the first full ingest):

| Item | Working | Cost |
| --- | --- | --- |
| Classification model | 800,000 items × 1,500 tokens in ($0.051/M) and 120 out ($0.335/M) | ~$93 |
| Embeddings | 2.4B tokens × $0.0017/M (embeddinggemma-2) | ~$4 (bge-m3 instead: ~$29) |
| Image labels | 1.5M × $2.51/M (resnet-50) | ~$4 |
| `toMarkdown` on scans | free for most formats; image OCR uses models | unknown; phase 0 measures on 100 scans |
| R2 Class A (puts, multipart, catalog writes) | ~3M × $4.50/M | ~$14 |
| R2 Class B (8M `HEAD`s, reads) | ~12M × $0.36/M | ~$4 |
| Queues | 2.5M items × 4 stages × 3 ops, + children | ~$12 |
| Workers CPU | 2.5M items × ~50 ms = 125M CPU-ms, 30M included | ~$2 |
| Basin Pipelines | ~35 GB of rows, 50 GB included | $0 |
| D1 | ~32M near-duplicate rows written, 50M included | $0 |
| Source egress, DigitalOcean | only chosen copies; at most 3.4 TB if nothing were deduplicated: (3,482 − 1,024 included) GiB × $0.01 | $0–25 (check the bill: the marketing page shows another rate) |
| Source egress, Scaleway | at most 0.8 TB: (800 − 75 free) GB × €0.01 | €0–7 |
| Source egress, Backblaze B2 | to Cloudflare: free under the partner programme; else within 3 × stored | $0 |
| Listing (8M objects) | ~8,000 list calls; B2 class C beyond 2,500 a day | under $1 |
| Super Slurper | no charge on Cloudflare's side beyond R2 operations (already above) | $0 |
| **Total** | | **~$135–170 plus OCR** (egress is the only new line, and dedup before transfer keeps it near the low end) |

Monthly, once ingested:

| Item | Working | Cost |
| --- | --- | --- |
| R2 storage | 1.06–1.6 TB Standard ($16–24), or with originals in IA ($11–16) | $11–24 |
| Vectorize storage | 5.1M × 768 = 3.9B dimensions, $0.05/100M | ~$2 |
| Vectorize queries | (5.1M stored + 3,000 queries) × 768 = 3.9B queried dimensions, $0.01/M, only in months with queries | ~$39 (at 256 dimensions: ~$13) |
| Basin SQL | ~2 TB scanned at $2.50/TB | ~$5 |
| Basin Catalog | under 1M operations; compaction a few GB | under $1 |
| Backblaze B2, backup bucket | 1.06–1.6 TB × $6.95 | ~$7.40–11 |
| Backblaze B2, source buckets (the person's existing data, unchanged) | 1–2 TB × $6.95, already paid today | $0 new |
| Workers Paid plan | base | $5 |
| **Total** | | **~$70–85 a month** (about $45–60 at 256 dimensions) |

The largest monthly line is Vectorize's query pricing, which counts every stored vector in each
month with queries. Dimension is the lever: phase 0 compares recall at 768 and 256 dimensions
(embeddinggemma-2's Matryoshka sizes) on the person's own questions.

### 13. The ingest runner: deduplicate next to the data, copy only what is new

The person (2026-11-23): a container deployed in DigitalOcean or Scaleway, next to the data,
reads the files, inflates zip files, and checks the R2 destination to decide whether each file
already exists, using several metrics. A file is left behind only when pigeon is absolutely
certain (a strong hash match plus the size); otherwise it is brought into Cloudflare. The same
runs inside Cloudflare for Backblaze B2, which has no compute. It is a pigeon capability
deployed into an instance's account: the R2 bucket, the dedupe index and Vectorize live in the
instance's account (`willowgraysen` first), not pigeon's.

**Parts** (all in this repository):

| Part | Crate or file | What it is |
| --- | --- | --- |
| Dedupe library | `src/ingest` (`pigeon-ingest`) | pure Rust, no I/O of its own, builds for WASM and natively: metrics, decision rules, zip walking, manifests and checkpoints, the index schema, estimates, and the engine that drives one run over a `Source` and a `Dest` |
| Runner | `src/ingest-runner` (`pigeon-ingest-runner`), `src/ingest-runner/Dockerfile` | the container: a native binary that implements `Source` (S3 list and get, SigV4 through `pigeon_core::s3`; a local folder) and `Dest` (R2's S3 API) over blocking HTTPS, with worker threads and a spool directory |
| Capability | `src/capabilities/src/ingest.rs` (+ `ingest/`) | the tools `data_ingest_plan`, `data_ingest_start`, `data_ingest_status`, `data_ingest_report`, `data_ingest_stop` in core, the run approval, the hosts' API requests, secret slots |
| Cloudflare-side path | `src/data-worker` (`pigeon-data-worker`) | the instance's data Worker (data-worker.md) with only its ingest parts so far: a Durable Object `IngestJob` that runs the runner image as a Cloudflare Container (for B2), and the queue consumer that applies manifests to the D1 index |

**One image, three hosts.** The same runner image runs everywhere; only the host differs:

| Source | Host | Why there |
| --- | --- | --- |
| DigitalOcean Spaces (3.4 TB) | a Droplet in the Space's region | Spaces to Droplets in the same region is free ([Spaces pricing](https://docs.digitalocean.com/products/spaces/details/pricing/), read 2026-11-23); only unique bytes leave, against the team's Droplet transfer pool |
| Scaleway Object Storage (0.8 TB) | an Instance in the bucket's region (`fr-par` → zone `fr-par-1`) | intra-region Object Storage traffic is free and Instances have no egress fees ([storage pricing](https://www.scaleway.com/en/pricing/storage/), [Instances pricing](https://www.scaleway.com/en/pricing/virtual-instances/), read 2026-11-23) |
| Backblaze B2 (1–2 TB) | a Cloudflare Container in the instance's account, held by the data Worker's `IngestJob` | B2 has no compute; B2 to Cloudflare is free under the partner programme (decision 11) |
| Local disks (~1 TB, later) | the runner binary on the person's machine, `source.kind = "local"` | the uploader of decision 11, now the same code |

VMs rather than serverless containers on DigitalOcean and Scaleway: a run lasts hours (3.4 TB
at 100–150 MB/s is 6–10 hours), App Platform and Serverless Containers are built for requests,
and a VM can build the image itself (below), so no registry is needed.

#### 13.1 The decision: when a file is left behind

Metrics computed for every file (top-level objects and archive members alike):

| Metric | How | Used for |
| --- | --- | --- |
| `size` | bytes read, counted while streaming (never the listing's or the zip header's claim) | certainty |
| `sha256` | streaming SHA-256 | certainty; the R2 key `blobs/sha256/{2}/{2}/{64}` |
| `blake3` | streaming BLAKE3, same pass | a second independent hash, recorded on the R2 object |
| `mime`, `kind` | magic-byte sniff of the first 8 KiB (`infer`), then UTF-8 text check; never the extension | index, report, later extraction |
| `norm_name`, `norm_path` | Unicode NFC, lowercased, `\` → `/`, whitespace collapsed, copy markers (`(1)`, `copy of`, `- copy`) removed, extension aliases (`jpeg` → `jpg`, `htm` → `html`, `tiff` → `tif`) | hints only |
| archive identity | `(parent_sha256, member_path, member_index)` | where a member came from |
| `simhash` (optional, `fingerprints: true`) | 64-bit SimHash over word 3-shingles of normalised text, text-like types up to 16 MiB | near-identical documents: hints only |
| source identity | `(account, bucket, key, etag, size, modified)` from the listing | sightings, resume |

Each R2 blob is written with metadata `x-amz-meta-sha256`, `x-amz-meta-blake3`,
`x-amz-meta-size` and `x-amz-meta-mime`, and a single PUT also carries
`x-amz-checksum-sha256`, so R2 itself refuses bytes that do not match.

The rules, in order, for one file (`pigeon_ingest::decide`):

| # | What R2 (and the run) says | Decision | Bytes copied? |
| --- | --- | --- | --- |
| 1 | the run already stored or confirmed this `sha256` | `duplicate` (reason `seen_in_run`) | no |
| 2 | `HEAD blobs/sha256/…` answers, its length equals `size`, its `sha256` metadata equals ours, and its `blake3` metadata, when present, equals ours | `duplicate` (reason `in_r2`) | no |
| 3 | `HEAD` answers but the length or a hash differs | `conflict`: the bytes go to `quarantine/{run}/{sha256}`, the blob is not touched, the report lists it | yes, aside |
| 4 | `HEAD` answers with no `sha256` metadata (written by another path) | `upload` (reason `unattested`): rewritten with our verified bytes, which cannot change the content since the key is their hash | yes |
| 5 | `HEAD` answers 404 | `upload` (reason `new`) | yes |
| any | reading or hashing failed | `failed`, retried 3 times with back-off, then listed in the report; never counted as a duplicate | no |

So the certainty threshold is exactly: **equal SHA-256 and equal size, attested by the R2
object itself**, plus BLAKE3 when both sides have it. Nothing else ever leaves a file behind.
The other metrics only add **hints** to the manifest line (`same_name_size` when
`(norm_name, size)` matched another file with a different hash, `near_text` when SimHash is
within 3 bits), which the index keeps as relations (decision 2's tiers 2 and 3) for the person
to review. In a dry run the decisions are the same and nothing is written to `blobs/` or
`quarantine/` (the decision reads `would_upload`).

#### 13.2 Archives: zip and nested zip

- **Detected by content** (`PK\x03\x04` or an empty archive's `PK\x05\x06`), never by name.
  Other archive types (tar, gzip, 7z, rar) are stored as files and listed in the report as
  "not opened"; tar and gzip come next.
- **The archive is a file first**: its own bytes go through 13.1 and are kept (`keep_archives`,
  default true), so nothing depends on re-zipping. Its members then go through 13.1 each, as
  children with `parent_sha256`, `member_path` and `depth`. A zip whose own SHA-256 is already
  in R2 is not opened again.
- **Limits** (`Limits`, per top-level archive, nested ones included; counted on inflated bytes as
  they stream, never trusting the sizes the headers declare):

  | Limit | Default | When exceeded |
  | --- | --- | --- |
  | nesting depth | 3 | deeper archives are kept as files, not opened (`depth_limit`) |
  | entries | 100,000 | the rest are not opened (`too_many_entries`) |
  | one member's inflated size | 8 GiB | that member is skipped (`member_too_large`) |
  | inflated : compressed ratio, for members over 1 MiB | 200 | the archive stops opening (`bomb_suspect`) |
  | total inflated bytes | 32 GiB, and never more than the spool's free space | the archive stops opening (`bomb_suspect`) |

  Members already done stay done; the archive itself is always kept. A stopped archive is
  listed in the report with its flag.
- **Encrypted** members (general-purpose bit 0, or the AES extra field `0x9901`) are not
  opened: the manifest records the name and declared size with flag `encrypted`, and the
  archive is kept whole. No password is ever tried.
- **Path traversal**: member names are never used as file-system paths: members spool to files
  named by a counter. The recorded `member_path` is normalised (`\` → `/`, leading `/` and drive
  letters removed, `.` dropped, `..` removed); any of that, a NUL or a control character adds
  `unsafe_path`, and the raw name is kept (escaped) beside it.
- **Symlink** entries are recorded (`symlink`) and not followed; directory entries are skipped.
  Duplicate names in one archive are allowed: `member_index` tells them apart.
- **Corrupt** archives or a CRC mismatch: that member is not stored (`crc_mismatch`), the rest
  go on; an unreadable central directory keeps the archive as a file only (`unreadable`).

#### 13.3 Manifests, checkpoints, resume

Everything about a run lives in the instance bucket under `ingest/runs/{run}/`:

| Key | Written by | What |
| --- | --- | --- |
| `job.json` | pigeon, at `data_ingest_start` | the `JobSpec` (no secrets) |
| `manifest/{seq:08}.jsonl` | runner | one `ManifestLine` per file or member: source identity, metrics, decision, reason, flags, hints, R2 key; a part closes at 5,000 lines or 8 MiB |
| `checkpoint.json` | runner, after each part | `{seq, after_key, counters}`: `after_key` is the highest listing key with every key at or below it finished |
| `status.json` | runner, every 30 s and at the end | state (`starting`, `listing`, `running`, `done`, `failed`, `stopped`), counters, rates, ETA, last error |
| `report.json` | runner, at the end | totals per decision and flag, bytes read and copied, conflicts, failures, archives not opened, the largest duplicate groups, estimate against actual |

Order of writes per file: the blob PUT is confirmed before its manifest line is kept; a part is
written before the checkpoint that covers it. A restarted runner reads `checkpoint.json` and
lists from `after_key` (ListObjectsV2 `start-after`), so at most the files in flight are done
again, which is harmless: the HEAD finds their blobs, and manifest lines have a stable id (the
SHA-256 of `(source, bucket, key, member_index…)`), so the index keeps one sighting. Run ids are
`{yyyymmdd}-{provider}-{name}-{bucket}-{6 hex}`.

#### 13.4 The dedupe index (D1, in the instance's account)

The certainty check needs only R2 (13.1). The index is the queryable record of sightings,
types and hints, and the base for later steps. It is D1 database `{instance}-data`
(data-worker.md decision 6), applied from the manifests by the data Worker (R2 event
notification on `ingest/runs/` → queue `{instance}-ingest-manifests` → consumer), so it can be
rebuilt from R2 at any time. **Since 2026-11-28** ([ingest-index-dry-run.md](ingest-index-dry-run.md))
the tables live in the lite store with the prefix `index_` (migration `0007_ingest_index`), a
dry run's part indexes nothing but its `index_applied` mark, a managed run's parts are indexed
by the data Worker as the runner writes them (the dry-run flag from the signed grant), and
`head-many` answers from `index_blobs` first. The schema (`pigeon_ingest::index::SCHEMA`, shown
here unprefixed):

```sql
CREATE TABLE blobs (            -- one row per unique content
  sha256 TEXT PRIMARY KEY,      -- 64 hex
  blake3 TEXT NOT NULL,
  size INTEGER NOT NULL,
  mime TEXT NOT NULL,
  kind TEXT NOT NULL,           -- document, image, video, audio, archive, email, text, other
  r2_key TEXT NOT NULL,         -- blobs/sha256/aa/bb/<64 hex>
  simhash TEXT,                 -- 16 hex, text-like files only
  first_run TEXT NOT NULL,
  first_seen_ms INTEGER NOT NULL
);
CREATE TABLE sightings (        -- every place a content was seen ("sources seen")
  id TEXT PRIMARY KEY,          -- sha256 of (account, bucket, key, member indexes)
  sha256 TEXT NOT NULL,         -- empty for members not opened (encrypted)
  account TEXT NOT NULL,        -- digitalocean:legacy, local:laptop…
  bucket TEXT NOT NULL,
  key TEXT NOT NULL,
  parent_sha256 TEXT,           -- the archive, for a member
  member_path TEXT,
  member_index INTEGER,
  depth INTEGER NOT NULL,
  norm_name TEXT NOT NULL,
  etag TEXT, size INTEGER NOT NULL, modified TEXT,
  decision TEXT NOT NULL,       -- uploaded, would_upload, duplicate, conflict, failed, skipped
  reason TEXT NOT NULL,
  flags TEXT NOT NULL,          -- comma-separated: encrypted, unsafe_path, bomb_suspect…
  run TEXT NOT NULL,
  seen_ms INTEGER NOT NULL
);
CREATE INDEX sightings_sha256 ON sightings (sha256);
CREATE INDEX sightings_name_size ON sightings (norm_name, size);
CREATE TABLE hints (            -- relations from the weaker metrics, never decisions
  a TEXT NOT NULL, b TEXT NOT NULL, kind TEXT NOT NULL, score REAL,
  run TEXT NOT NULL, PRIMARY KEY (a, b, kind)
);
CREATE TABLE runs (
  run TEXT PRIMARY KEY, account TEXT NOT NULL, bucket TEXT NOT NULL, prefix TEXT NOT NULL,
  host TEXT NOT NULL, dry_run INTEGER NOT NULL, state TEXT NOT NULL,
  started_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, counters TEXT NOT NULL
);
CREATE TABLE applied (          -- manifest parts already applied: applying is idempotent
  part TEXT PRIMARY KEY, lines INTEGER NOT NULL, at_ms INTEGER NOT NULL
);
```

Size: about 8 million sightings at ~350 bytes is under 3 GB, inside D1's 10 GB. Every write is
`INSERT … ON CONFLICT DO NOTHING` (blobs, sightings, hints) or an upsert (runs), so a part
applied twice changes nothing. The rows also feed decision 3's `items` and `sightings` Iceberg
tables when Basin Pipelines exist; D1 stays the operational index.

#### 13.5 The image

`src/ingest-runner/Dockerfile`, multi-stage: `rust:1-bookworm` builds
`pigeon-ingest-runner --release --target x86_64-unknown-linux-gnu`; the final stage is
`debian:bookworm-slim` with `ca-certificates`, a non-root user and `/spool`. Configuration is by
environment only: `INGEST_JOB` (the `JobSpec` JSON, or `INGEST_JOB_URL` naming `job.json`'s
key), `SOURCE_KEYS` and `DEST_KEYS` (`ACCESS_KEY_ID:SECRET`), `SPOOL_DIR`. It exits 0 when the
run is done, non-zero on a fatal error after writing `status.json`.

**No registry tonight.** On a Droplet or an Instance, cloud-init builds the image from the
public repository at the run's commit (`docker build -f src/ingest-runner/Dockerfile
https://github.com/pigeon-security/pigeon.git#<commit>`), about 5–10 minutes on 4 vCPUs, so the
person approves an exact commit and nothing is pushed anywhere. For the Cloudflare Container,
`wrangler deploy` of the data Worker builds and pushes the image to the instance account's own
Cloudflare registry; that is a deploy step for the person's go-ahead (13.9). The CI check is the
`test` job: the runner crate builds natively inside the smoke build and its scenario runs the
engine against a fake S3 server; building the Docker image in CI would need a new job in
`.pigeon/ci.toml`, which is the person's file (follow-up).

#### 13.6 How pigeon deploys and drives it

`data_ingest_plan {instance, source, bucket, prefix?, dry_run = true, keep_archives = true,
fingerprints = false, size?}` reads only, then asks the person: it checks the instance is
linked, the source account is declared and its key bound, the host and R2 secrets are bound,
and the instance bucket exists (if not, the plan's first step creates it through the link);
lists the first page of the source for a sample; and answers the plan lines, the `JobSpec`, the
host request with secrets redacted, the estimate (13.7), and an approval of kind `ingest_run`
on the console's Approvals tab, bound to the digest of all of that and the commit. Anything
missing makes the answer `applies: false` with what the person must do.

`data_ingest_start {plan}` takes the approval (once; refused if the plan's digest changed), creates
the bucket if the plan said so, writes `job.json`, then starts the host:

| Host | Calls | Credentials |
| --- | --- | --- |
| DigitalOcean | `POST https://api.digitalocean.com/v2/droplets` (`name: pigeon-ingest-{run}`, the Space's region, size `s-4vcpu-8gb`, image `ubuntu-24-04-x64`, tags `pigeon-ingest`, `user_data`: cloud-init that installs Docker, builds the image, runs it once with `--rm`, then powers off); `GET /v2/droplets/{id}`; `DELETE /v2/droplets/{id}` | secret `INGEST_HOST_DIGITALOCEAN_<NAME>`: a DigitalOcean API token with custom scopes `droplet:create`, `droplet:read`, `droplet:delete` (and the read scopes DigitalOcean adds with them) |
| Scaleway | `POST https://api.scaleway.com/instance/v1/zones/{zone}/servers` (`name`, `commercial_type: POP2-4C-16G`, image `ubuntu_noble`, `project`, `tags`, a 100 GB block volume for the spool), `PATCH /servers/{id}/user_data/cloud-init`, `POST /servers/{id}/action {"action":"poweron"}`, `GET /servers/{id}`, `POST /servers/{id}/action {"action":"terminate"}` | secret `INGEST_HOST_SCALEWAY_<NAME>`: `PROJECT_ID:SECRET_KEY` of an IAM application with `InstancesFullAccess` on that project only |
| Cloudflare (B2) | `POST https://{instance}-data.{subdomain}.workers.dev/ingest/start` signed with the instance's `DATA_KEY` (data-worker.md decision 2): the `IngestJob` object named by the run starts the container (`standard-4`), passes the keys as its environment, keeps nothing | the data Worker deployed with its `DATA_KEY` (data-worker.md) |

Keys reach the container only as environment at start: the source account's read-only key
(`STORAGE_<PROVIDER>_<NAME>`, from the storage capability) and the instance bucket's R2 key
(`INGEST_R2_<INSTANCE>`: an R2 API token for `{instance}-data` only, Object Read & Write, made by
the person in the instance's dashboard, since an OAuth link cannot create R2 API tokens). On a
VM they are in cloud-init user data, which the provider shows to the account owner and the VM's
metadata service; the VM is deleted when the run ends (`data_ingest_status` deletes a Droplet or
Instance whose run is `done`, `failed` or `stopped`; `data_ingest_stop {run}` does it at once), and
the person may rotate the R2 key after the backfill. Later: R2 temporary credentials scoped to
the run's prefixes, minted through the link, replace the long-lived R2 key once phase 0 shows
the OAuth link may call that endpoint.

Every secret is set on the console's Access tab, never in chat. A slot exists for each declared
storage account on DigitalOcean or Scaleway (`INGEST_HOST_…`) and for each instance that takes
ingest (`INGEST_R2_<INSTANCE>`, `willowgraysen` declared). Each is checked live before it is
written (`GET /v2/account`; Scaleway `GET /instance/v1/zones/{zone}/servers?per_page=1`; R2
`HEAD` on the bucket). As with every Secrets Store secret, its binding in
`src/platform/wrangler.toml` lands after the secret exists.

`data_ingest_status {instance, run?}` reads `status.json` through the link (`r2::get` with the
instance's account) and the host's state; without `run` it lists the instance's runs, kept in
the Directory as `ingest:run:{run}` (host, ids, approval, commit). `data_ingest_report {instance,
run}` answers `report.json`, at most 200 rows of each list. Nothing on the runner listens on a
port: results flow back only through R2.

#### 13.7 Throughput and cost (estimates; the first dry run measures)

Assumptions: SHA-256 with SHA extensions about 1.5 GB/s a core and BLAKE3 over 3 GB/s, so
hashing is never the limit; the source read rate is 100–150 MB/s per VM in region (B2 to a
Cloudflare Container 80–120 MB/s); unique share about 25–30% (decision 11).
`pigeon_ingest::estimate` computes the same table for `data_ingest_plan` from the sampled sizes.

| Source | Read | Time at 100–150 MB/s | Host | Copy out (unique ~25–30%) | Requests | Total |
| --- | --- | --- | --- | --- | --- | --- |
| DigitalOcean, 3.4 TB | free in region | 6.3–9.5 h | `s-4vcpu-8gb` at $0.071/h: ~$1 | ~0.9–1 TB from the Droplet pool: the Droplet accrues 5,000 GiB / 28 days (~180 GiB a day), the rest $0.01/GiB: ~$8 | ~4M R2 HEADs $1.50, ~1M PUTs $4.50 | **~$15** |
| Scaleway, 0.8 TB | free in region | 1.5–2.2 h | `POP2-4C-16G` about €0.15/h plus a 100 GB volume: ~€1 | Instances have no egress fee: €0 | ~1M HEADs, ~0.25M PUTs: ~$1.50 | **~€3** |
| B2, 1–2 TB, Cloudflare Container | free to Cloudflare | 2.3–7 h | `standard-4` ≈ $0.40/h: ~$1–3 | container egress to R2's endpoint: 1 TB a month included, then $0.025/GB: $0–13 | ~2.5M HEADs, ~0.6M PUTs: ~$4 | **~$5–20** |
| Local, 1 TB | the person's disk | uplink-bound (decision 11) | the person's machine | unique only | ~1M HEADs | under $2 |

Storage after ingest is decision 12's. Disk: the spool needs the largest single file plus the
archive budget; 100 GB volumes on the VMs, 20 GB on `standard-4` (larger files are streamed and
hashed without spooling, so archives over the spool are kept as files and listed as "not
opened": the report says how many; a VM run can open them later).

#### 13.8 Later steps

1. Tar, gzip and mbox members (mbox through `pigeon-email`), 7z and rar listed only.
2. The manifest applier feeds Basin Pipelines (decision 3) as well as D1.
3. Embeddings: after a run, each new blob is queued for extract → classify → embed (decisions
   4, 5 and 8) in the data Worker; Vectorize `{instance}-data-v1` is filled from the `chunks`
   table, never from the runner. The runner's `kind` and `simhash` let the embed stage skip
   near-identical text.
4. R2 temporary credentials per run (13.6), and a CI job that builds the image (13.5).
5. The local source as the person's uploader (decision 11), with a `have` check through the
   data Worker instead of R2 keys on the person's machine.

#### 13.9 What the person provides, and the first dry run

1. **Link willowgraysen**: approve its instance record on the Instances tab and press Link
   Cloudflare account (data-worker.md follow-up 1). Turn on Workers Paid there.
2. **Name the source accounts** (`digitalocean:<name>`, `scaleway:<name>`, `backblaze:<name>`,
   with regions and buckets) and ask an agent to declare them in `data_read_accounts()`
   (storage-providers.md follow-up 5). That makes the slots below appear.
3. **Create keys**, each on the provider's console, and set each on the Access tab:
   - `STORAGE_DIGITALOCEAN_<NAME>`: a Spaces key, Limited Access, Read on the source buckets.
   - `INGEST_HOST_DIGITALOCEAN_<NAME>`: an API token with `droplet:create`, `droplet:read`,
     `droplet:delete`.
   - `STORAGE_SCALEWAY_<NAME>`: an application key with `ObjectStorageReadOnly`.
   - `INGEST_HOST_SCALEWAY_<NAME>`: `PROJECT_ID:SECRET_KEY` of an application with
     `InstancesFullAccess` on one project.
   - `STORAGE_BACKBLAZE_<NAME>`: a read-only application key.
   - `INGEST_R2_WILLOWGRAYSEN`: in willowgraysen's dashboard, R2 → Manage API tokens → Create,
     Object Read & Write, bucket `willowgraysen-data` only (create the bucket first, or let
     the first approved plan create it and set the key after).
4. **Ask an agent to bind the secrets** in `src/platform/wrangler.toml` and land (a binding to a
   missing secret fails the deploy, so this comes after step 3).
5. **The first dry run** (DigitalOcean, one prefix): `data_ingest_plan {"instance":
   "willowgraysen", "source": "digitalocean:<name>", "bucket": "<bucket>", "prefix":
   "<a small prefix>", "dry_run": true}`, approve it on the Approvals tab, then
   `data_ingest_start {"plan": "<approval id>"}`; follow with `data_ingest_status` and read
   `data_ingest_report`. Nothing is copied in a dry run; the Droplet is deleted at the end.
6. For B2: the data Worker must be deployed into willowgraysen first (data-worker.md phases 1
   and 3, plus a `wrangler deploy` that builds the container image there), which is a separate
   go-ahead.

## Phases

| Phase | What | Lands with | Who |
| --- | --- | --- | --- |
| 0 | Spikes, nothing kept: Vectorize and Pipelines from a Rust Worker (raw binding through `wasm_bindgen`) or HTTP; Basin SQL from a Worker (HTTP API, token); one stream fanning out to several tables; Basin Catalog table registration (restore); `qwen3-30b-a3b-fp8` plan class; embeddinggemma-2 at 768 vs 256 on 200 of the person's items; `toMarkdown` cost on 100 scans; Email Routing behaviour (email.md) | report in the Log | agent, person's throwaway account or pigeon's |
| 1 | Pure crates, off: `pigeon-ingest` with item and table row types, ids, the tier 2 and 3 indicators (text normalization, MinHash, pHash/dHash on decoded pixels), the alias normalizers, the freemail list; the Iceberg schemas as data; smoke scenarios | landing, code only | executors, planner |
| 2 | The data Worker shell and its resources (data-worker.md phases 1–3) | person's passkey on the instance record | agent, person |
| 3 | Email end to end: capture → identify → extract → classify (rules only) → embed; entity linking by address and domain | person enables `data.email` | agent |
| 4 | The listing pass over every source bucket (read-only), dedup from listings, then the copier and Super Slurper per bucket (decision 11); real counts replace decision 12's assumptions; then the uploader for local disks | person declares the accounts; person runs the uploader | agent, person |
| 4r | **The ingest runner (decision 13)**: `pigeon-ingest`, the runner and its image source, the `ingest_*` tools, the data Worker's `IngestJob` and manifest applier; built and landed latent overnight 2026-11-23; then the first dry run after 13.9 | landings, code only; runs with the person's keys and approval | planner, executors, person |
| 5 | Model classification and fields; name matching; merges; `data_reprocess` | | agent |
| 6 | Container jobs for large files and deletes | | agent |
| 7 | B2 backup and reconciliation | person sets the B2 key | agent, person |
| 8 | Query tools over MCP (`data_records`, `data_search`; data-worker.md decision 4), the base for the website | | agent |

## Follow-ups for the person

1. **Name the source accounts**, one per provider account, as in storage-providers.md: for
   example `digitalocean:legacy`, `scaleway:personal`, `backblaze:personal` (add more if you
   have several accounts at one provider).
2. **Create a read-only key per source account**, as storage-providers.md's follow-ups describe:
   DigitalOcean Spaces read-only access key; Scaleway an application with object read only;
   Backblaze an application key with `listBuckets`, `listFiles` and `readFiles` only. Enter each
   on the console's Access tab when asked; never in chat.
3. **List the buckets to include or skip** per account (default: include every bucket the key can
   list), and mark one **primary** bucket where you know the same data is in several places
   (decision 11, step 3).
4. **Backblaze backup**: create a new bucket `pigeon-backup-{instance}` (not one of your source
   buckets), turn on SSE-B2 (and Object Lock if you want it), and create an application key for
   that bucket only, with read and write. Enter it on the Access tab when phase 7 asks.
5. **Check the DigitalOcean bill** after the first month of transfer: the docs and the pricing
   page disagree on the overage rate (decision 12).
6. Choose the jurisdiction for each instance's data bucket (EU for noisypigeon, as its site?).
7. Approve or decline Infrequent Access for originals after 30 days (decision 9).
8. Workers Paid on each instance account (needed for Queues retention, Pipelines, CPU limits and
   Vectorize beyond the free tier).
9. The Basin Pipelines sinks need an API token with R2 Admin Read & Write in the instance
   account. Whether pigeon can create it through the OAuth link is open (data-worker.md); if not,
   create it in the dashboard and enter it on the Access tab.
10. Give phase 0 about 200 of your own items and 10 real questions ("all invoices from X in
   2024") to judge models and dimensions against.

## Log

- 2026-11-26: the lite tier (email.md): decision 3's `items`, `sightings` and `emails` in D1 for
  the home instance, decision 9's layout and decision 10's backup, built off by default on
  `claude/email-capability`. Phase 3's email capture is now a catch-all on pigeon.dev, not a
  capture subdomain.

- 2026-11-22: design written overnight from the person's brief (#5 and the target use).
  Cloudflare's docs read from the sandbox: R2 Data Catalog, R2 SQL and Pipelines are now Basin
  Catalog, Basin SQL and Basin Pipelines; Basin SQL is read-only, so all Iceberg writes go
  through Pipelines and the tables are append-only with versioned rows. Vectorize has no array
  filter, so entity queries go through Iceberg only. Assumed (no answer within 5 minutes to the
  one question asked): the raw data is mostly on local disks, some may be in an existing B2
  bucket; the uploader that hashes before upload is the main path, Super Slurper the second; B2
  is the backup target and pigeon writes only under `pigeon/{instance}/` in it. Nothing built for
  this plan beyond the email schema (email.md phase 1).
- 2026-11-22 (evening): the person answered decision 11: 3.4 TB on DigitalOcean Spaces, 0.8 TB on
  Scaleway, 1–2 TB on Backblaze B2, ~1 TB local, 0.3–0.4 TB in mailboxes. Decision 11 rewritten:
  the main path is cloud to cloud; every source bucket is read-only; listings are deduplicated by
  size and ETag (strong for plain MD5 ETags, weak for multipart, unknown hashed after landing)
  before any transfer; copies are fetched from B2 first (free egress to Cloudflare), then
  Scaleway, then DigitalOcean; Super Slurper for mostly-unique buckets, pigeon's streaming copier
  for the rest; the uploader only for local disks. Decision 10: the backup goes to a new,
  separate B2 bucket, and the existing B2 data stays where it is. Decision 12: egress and the new
  volumes added. Egress prices read from the providers' pages (cited in decision 11) on
  2026-11-22. Revised by the AI agents planner session at the person's request; docs only.
- 2026-11-22 23:40 UTC: handoff. The AI agents planner session wraps up; this plan's next
  steps wait on the person's answers (`overnight-2026-11-22.md`, "#4–#7 Data platform" and
  its handoff section), then an AI Gateway planner.
- 2026-11-23: decision 13 (the ingest runner) designed at the person's request: a container
  next to each source hashes every file and copies only what R2 does not hold with equal
  SHA-256 and size; DigitalOcean on a Droplet, Scaleway on an Instance, B2 on a Cloudflare
  Container in the instance's account; D1 index applied from manifests in R2. Egress and
  instance prices read from the providers' pages from the sandbox. Planner
  `claude/ai-agent-lnX2my6PYsAz`; nothing deployed outside pigeon's account.
- 2026-11-23 (overnight): phase 4r built and landed latent by planner
  `claude/ai-agent-Pz9swlihkaev`: `pigeon-ingest` (metrics, rules 1–5, zip with limits and flags,
  the engine with resume, the D1 schema and statements, signed starts), the runner, the `ingest_*`
  tools and the data Worker's `IngestJob` and manifest consumer. No secret bound, no host or
  instance account called. Known gaps: the engine is single-threaded (`threads` unused; since
  2026-11-28 a worker pool, see [ingest-throughput.md](ingest-throughput.md)), the
  status has no ETA and the report no estimate-against-actual; duplicate member names in one zip
  are read as one (the `zip` crate keeps one entry per name); the data Worker and the container
  start were built for WASM and never run; a CI job that builds the image needs `.pigeon/ci.toml`
  (the person's file). Next: 13.9.

## Approved instance bindings

The optional `ingest` instance profile can explicitly disable ingestion or bind a
declared storage account, source bucket, exact prefix, and destination R2 bucket.
An enabled profile requires a linked account ID in the approved record. The
declared account's bucket allowlist still applies. The plan takes its source
settings from that profile and rejects conflicting tool arguments. Existing
records without a profile retain their code-declared target setup.

Start validates the approved job against the current profile, account link and
storage declaration before consuming the passkey approval. The plan additionally
binds the configured Cloudflare data Worker origin or Scaleway project; an old
plan without one of these configurable destinations requires a fresh approval.
Workers URLs use HTTPS, and provider calls do not automatically follow redirects.
A disabled or changed profile prevents new starts. Started runs retain the
destination and data Worker they used, so later status, report, and stop calls
cannot silently use a replacement destination.

GitHub main-branch pushes now bind the queue to the repository, App installation,
and digest of the approved instance record. The cron and Sites Worker recheck
that binding before dispatch and publishing. A stale or legacy unbound queue
entry requires a new push. The cron evaluates `site_deploy` policy as the linked
person and defers denied or approval-gated publishes to the manual path. It
changes queue state only while the claimed entry is still current.

## Data-ingest rollout

`data-ingest` is an opt-in capability that requires `data` on the same instance. The data
capability owns the destination bucket and catalog. The ingest capability owns source
enumeration, approval, execution and checkpointing. Ingest uses the approved DigitalOcean
Spaces binding for its first source. A planning request can narrow that prefix and the
service uses the saved read-only key to sample the source. The estimate covers the sample
unless the caller supplies the full source size.

1. Register `data-ingest` and serve the existing `data_ingest_plan` tool as a read-only preflight
   on instances that opt in. Keep the existing tool names, profiles and run history.
2. Provision the data store in linked accounts using the instance's own approved
   Cloudflare connection. Bind plans to the exact approved source and data destination.
3. Integrate ingest execution with Jobs. Require a deadline, automatic cleanup with
   deletion confirmation, and a private terminal audit. Use the existing passkey approval
   before provisioning compute. Resolve credentials from the owning capabilities after
   checking the approved plan and avoid storing them in diagnostic output.
4. Run the Spaces canary on willowgraysen with a dedicated prefix. Reconcile exact hashes,
   duplicate sightings, distinct variants, archive containers and member lineage against
   the source and terminal audit. Verify cleanup before expanding the rollout.

The DigitalOcean run uses the linked Data store and the Jobs lifecycle. It issues an
approval after checking both private buckets and the provider credentials. See the
[DigitalOcean run plan](digitalocean-ingest.md) for scratch storage, archive budgets,
credentials and cleanup.

### Backblaze on a linked instance

An instance owner approves the source in the instance record. It names the bucket and prefix and
derives the HTTPS endpoint from the B2 region. Its key needs list and read permissions on that
bucket and prefix. Use the instance's `INGEST_B2_<INSTANCE>` slot for the source. An independent
backup uses separate credentials and a different bucket.

```toml
[data]
placement = "own_worker"

[data.ingest.backblaze]
name = "historical"
region = "us-west-004"
bucket = "example-incoming"
prefix = "export/"

[data.ingest.backblaze.runtime]
image = "registry.cloudflare.com/<linked-account-id>/pigeon-ingest-runner@sha256:<image-digest>"
revision = "<full-Git-revision>"
```

Provision the instance's private Data bucket and its separate private Jobs audit bucket first. Use
a dedicated R2 token with object read and write on the Data bucket for `INGEST_R2_<INSTANCE>`.
The Cloudflare Container needs the Data Worker's `IngestJob` binding and the matching
`pigeon-ingest-runner` image pinned to a digest in that instance account's registry.
Publish the image with the exact revision as `PIGEON_COMMIT`. Re-link the account with the
optional Workers Containers Write scope. The instance plan then shows the pinned image and
prepares it, uploads Data's Worker, and attaches the dedicated SQLite Durable Object container.
`src/data-worker/wrangler.example.toml` documents the equivalent manual runtime bindings.

On the instance MCP, use `data_ingest_plan` with `source: "backblaze"`. A supplied prefix narrows the
approved one. Provide the source size if the sample contains more than one page. The default
dry run lists, downloads, hashes and records decisions in R2. A real run uploads unique content
and retains archive originals. The approval binds the source, destination, audit bucket, image
digest and maximum runtime. The Data Worker starts each approved run at most once, polls its
container until exit and sends a signed terminal report for Jobs. The report excludes credentials
and source object names.

### Managed destination multipart publication

Managed runners stage canonical blobs above the single-write limit in 16 MiB
parts. The Data Worker reserves the admitted bytes and one write at initiation.
D1 pins each part's digest before it goes to R2 and records the provider receipt
and running SHA-256 state. A retry uses the same run and blob key and may replay
the same part. The Worker verifies the complete SHA-256 before publication.
The ceiling is 10,000 parts. Larger objects and large quarantine objects fail
closed.

The Data Worker alone signs R2 `CompleteMultipartUpload` with
`If-None-Match: *`, using an R2 temporary credential scoped to exactly that
object and the completion action. The Worker confirms an ambiguously completed upload only
when the canonical object has the matching server-generated write ID, SHA-256,
and size. The runner receives neither an upload ID nor an S3 credential.
The provider's [conditional operation semantics](https://developers.cloudflare.com/r2/api/s3/conditional-operations/)
define the create-only guarantee at completion.

Multipart requires an approved, private Data bucket and a bucket-scoped R2 S3
credential available only to its Data Worker. The Worker expects a Secrets
Store binding named `INGEST_R2_PUBLISH` and trusted deployment metadata
`INGEST_R2_ACCOUNT` and `INGEST_R2_BUCKET` matching the `DATA` binding. The credential value is a single-line JSON object with `account`, `bucket`,
and `keys` in the established `ACCESS_KEY_ID:SECRET_ACCESS_KEY` format.

For a linked instance with a standalone Data Worker, create the R2 account
S3 credential in that instance's Cloudflare account with object read/write
limited to its existing private Data bucket. Calculate the lowercase SHA-256
of the exact JSON bytes and put only that fingerprint in
`data.multipart_publisher`. In the same account's Secrets Store, write the
exact JSON with the Workers service scope under the slot
`DATA_R2_PUBLISH_<instance-hash-prefix>_<credential-hash-prefix>`.
The first prefix is the first 16 hex characters of the SHA-256 of the
instance ID. The second is the first 24 hex characters of the credential
fingerprint. The linked Data plan checks the bucket's privacy and pins the
store, slot, generation and fingerprint. Apply rechecks the metadata and binds
the credential only to the standalone Data Worker. A rotation uses a new
fingerprint, a new slot, and a new approved Data plan. With the optional
Account API Tokens Write link scope and `data.multipart_mint`, an approved plan mints the
bucket-scoped parent credential itself (multipart-enable.md, "Automatic").

Multipart returns 503 before staging when the binding or exact credential
fingerprint check fails. [multipart-enable.md](multipart-enable.md) covers the rest of the
rollout: the console's checked key flow, the data plan's lifecycle rule (abort incomplete
uploads after 7 days), and `data_ingest_multipart_check`, the Data Worker's canary, which
readiness requires before ingest writes objects over 16 MiB.

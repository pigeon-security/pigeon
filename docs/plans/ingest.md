# Ingest: content-addressed files on R2, Iceberg as the record, Vectorize as the index

Status: **designed** (2026-11-22; decision 11 revised the same evening with the person's
answer on where the data lives). Nothing is built, deployed or enabled. Branch
`claude/ai-agent-AFc1coPrsJTT` (docs only). The first code it relies on, the email schema, is
built off by default on the code branch named in [email.md](email.md).

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

## Phases

| Phase | What | Lands with | Who |
| --- | --- | --- | --- |
| 0 | Spikes, nothing kept: Vectorize and Pipelines from a Rust Worker (raw binding through `wasm_bindgen`) or HTTP; Basin SQL from a Worker (HTTP API, token); one stream fanning out to several tables; Basin Catalog table registration (restore); `qwen3-30b-a3b-fp8` plan class; embeddinggemma-2 at 768 vs 256 on 200 of the person's items; `toMarkdown` cost on 100 scans; Email Routing behaviour (email.md) | report in the Log | agent, person's throwaway account or pigeon's |
| 1 | Pure crates, off: `pigeon-ingest` with item and table row types, ids, the tier 2 and 3 indicators (text normalization, MinHash, pHash/dHash on decoded pixels), the alias normalizers, the freemail list; the Iceberg schemas as data; smoke scenarios | landing, code only | executors, planner |
| 2 | The data Worker shell and its resources (data-worker.md phases 1–3) | person's passkey on the instance record | agent, person |
| 3 | Email end to end: capture → identify → extract → classify (rules only) → embed; entity linking by address and domain | person enables `data.email` | agent |
| 4 | The listing pass over every source bucket (read-only), dedup from listings, then the copier and Super Slurper per bucket (decision 11); real counts replace decision 12's assumptions; then the uploader for local disks | person declares the accounts; person runs the uploader | agent, person |
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

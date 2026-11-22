# Object storage providers: Scaleway, DigitalOcean and Backblaze B2 through the portal

Status: **phase 1 built on a branch, off by default** (22 November 2026). The account registry
and the read-only tool surface (`storage`) are on `claude/ai-agent-q7VjHJXE6Mv0`, with no account
declared, so the tools only say how to set one up. No credentials exist yet. Phase 2 starts when
the person has made the first key (Follow-ups for the person, below).

The person's words: "Add support for Scaleway MCP, Digital Ocean MCP and Backblaze MCP, going
through the Cloudflare MCP Portal. We should support multiple accounts (i.e. 2x Scaleway or DO
or Backblaze accounts). The use case: I have object storage buckets in each that I want to
consume into Pigeon Platform. We will build the actual ingestion later; the legacy accounts
(DigitalOcean) will be read-only, while Scaleway and Backblaze will have expanded use. Later:
Scaleway can serve spot instances for larger compute. With Backblaze, I would like to create a
platform account that has distinct groups; each group is an opt-in feature for instances, and
can be cold storage backups."

**In short.**
- No provider has an MCP server that reads objects in a bucket and also fits behind the portal
  with least privilege (§1). DigitalOcean's hosted server has no object tools. Scaleway's
  official server wraps its CLI, which has no object tools either. Backblaze's official server
  has them, but it is self-hosted only.
- So pigeon hosts one thin capability, `storage`, over the S3 API that all three providers
  speak. It is reached through the portal like every other pigeon tool, as server `pigeon`
  (`pigeon_storage_list_objects` and so on). Nothing new is attached to the portal.
- One account is named `provider:name`, such as `scaleway:personal` or `digitalocean:legacy`.
  Each has one Secrets Store secret, `STORAGE_<PROVIDER>_<NAME>`, holding its S3 key pair. It
  is set on the console's Access tab with the person's passkey, and checked live before it is
  written.
- Each account has an access mode. DigitalOcean accounts can only be read-only; the code
  refuses anything else. Scaleway and Backblaze accounts may be read-write, but tonight's tools
  only read. Writes come in phase 3, each call behind the person's passkey.
- Later, as their own plans: ingestion; Scaleway spot instances for larger compute; a Backblaze
  platform account with Groups, one group per opt-in feature an instance can take, such as cold
  storage backups. Backblaze's own MCP server joins the portal for that phase (§1, §6).

## Sources

Read 22 November 2026 from this sandbox.

- **Scaleway.** The MCP Registry entry `io.github.scaleway/scaleway-mcp`, latest version 2.66.2
  (published 2026-11-18): package `scaleway/cli:2.66.2` (OCI, stdio), source
  `https://github.com/scaleway/scaleway-cli`, subfolder `internal/namespaces/mcp`, read at tag
  `v2.66.2` (`README.md`, `server/commands.go`, `server/streamable_http.go`). The CLI's object
  namespace at the same tag (`internal/namespaces/object/v1`). `scaleway/scaleway-skills` at
  `8bb1261` (2026-05-18): skills only, no server. Permission sets from
  `https://www.scaleway.com/en/docs/iam/reference-content/permission-sets/`. Community servers
  seen on GitHub search and not used: `sndpl/scaleway-mcp`, `feedback-loop-ai/mcp-scaleway`,
  `E-Sensia/scaleway-mcp`, `cyclimse/mcp-scaleway-functions` (all one to eight stars).
- **DigitalOcean.** `https://github.com/digitalocean-labs/mcp-digitalocean` at tag `v1.6.8`
  (2026-11-16; npm `@digitalocean/mcp` 1.6.8; MCP Registry `com.digitalocean/mcp`): `README.md`,
  `pkg/registry/spaces/{README.md,keys_tools.go,cdn_tools.go}`. Spaces key permissions from
  `github.com/digitalocean/godo` v1.236.0, `spaces_keys.go`. The older
  `digitalocean/digitalocean-mcp` (App Platform only, last push 2025-07-23) is superseded.
- **Backblaze.** `https://github.com/backblaze-labs/b2-mcp` at tag `v0.2.2` (2026-09-29; npm
  `@backblaze-labs/b2-mcp` 0.2.2; MIT): `README.md`, `docs/AUTHENTICATION.md`,
  `docs/references/deployment/cloudflare-workers.md`. It calls itself "the official Backblaze
  B2 MCP server", "currently incubating in Backblaze-Labs".
- **Cloudflare.** The portal facts are those of [mcp-portal.md](mcp-portal.md) and
  [buildkite-mcp.md](buildkite-mcp.md): tool names `{server_id}_{tool}`, server IDs without
  `_` and at most 32 characters, `bearer` servers with fixed headers, a 60 s cap per tool call,
  and a sync after every landing's deploy.
- **S3 signing.** AWS Signature Version 4 as the S3 API reference describes it, with its two
  worked examples ("GET Object" with header auth, and a presigned GET), which the smoke
  scenario reproduces.

## 1. What each provider offers

| | Scaleway | DigitalOcean | Backblaze B2 |
| --- | --- | --- | --- |
| Official MCP server | Yes: `scw mcp server serve` in the CLI, 2.66.2 | Yes: `digitalocean-labs/mcp-digitalocean` 1.6.8, hosted by DigitalOcean | Yes, incubating: `backblaze-labs/b2-mcp` 0.2.2 |
| Where it runs | Self-hosted. stdio by default; `--transport streamable-http --address :8080` | Hosted: one router `https://mcp.digitalocean.com/mcp`, or one endpoint per service, such as `https://spaces.mcp.digitalocean.com/mcp`. Also self-hosted | Self-hosted only: npx, a container (HTTP by default), or a Cloudflare Worker template marked "experimental compatibility" |
| Its auth | Environment variables `SCW_ACCESS_KEY`, `SCW_SECRET_KEY`. The HTTP transport has no auth of its own | OAuth 2.0 (sign-in to DigitalOcean), or `Authorization: Bearer <personal access token>` | A B2 application key. Over HTTP: per-request headers (`X-B2-MCP-Key-Id`, `X-B2-MCP-Key`), or `server` mode with the key in the server and an OAuth resource server in front |
| Object tools (list, head, get) | **None.** Every CLI command becomes a tool, and the CLI's object namespace has buckets, bucket policy, CORS, lifecycle and config, but no object commands | **None.** The `spaces` service has access-key tools (`spaces-key-*`) and CDN tools only. `spaces-key-create` makes **full-access** keys | Yes: `s3_list_objects_v2`, `s3_head_object`, `s3_get_object` (inline up to 1 MiB), `s3_get_presigned_url`, and writes |
| Read-only mode | `--read-only` (get and list commands), `--namespaces`, `--verbs` | Read-only depends on the token's scopes, and no tool reads objects anyway | Capability-aware: a read-only key gets fewer tools; destructive tools are blocked over HTTP by default |
| Several accounts | One credential per process: one server per account | One token per server: one portal server per account | One key per process, or per request in `headers` mode |

### Scaleway

The official server is a wrapper over the whole CLI. It cannot list or read objects, and its
streamable HTTP transport has no auth, so it could only be reached through something that adds
it (Access in front of a container). It does not serve the use case. It may serve the later
compute phase (F1): with `--read-only --namespaces instance` it would let sessions look at
instances, behind the portal, from a container.

**Decision:** Scaleway buckets are read through pigeon's `storage` capability, at
`https://s3.<region>.scw.cloud` (regions `fr-par`, `nl-ams`, `pl-waw`), signing region the same
slug. The key is a Scaleway IAM API key (access key `SCW…` and secret key) belonging to an IAM
application, whose policy grants Object Storage on one project.

### DigitalOcean

DigitalOcean's hosted server could sit behind the portal as a `bearer` server today
(`https://spaces.mcp.digitalocean.com/mcp`, with a token as the credential, as Buildkite's
does). But it has no tool that lists or reads an object, and its key tool creates full-access
Spaces keys, which is the opposite of what a read-only legacy account needs.

**Decision:** DigitalOcean Spaces are read through `storage`, at
`https://<region>.digitaloceanspaces.com` (such as `nyc3`, `ams3`, `fra1`, `sgp1`), with a Spaces
access key limited to the buckets wanted, with `read` permission. DigitalOcean's MCP server is
not added. If the person later wants its other services (droplets, billing) in sessions, it can
be a portal server of its own with a read-only token; that is outside this plan.

### Backblaze B2

The official server is the only one with object tools, and the only one with B2's control
plane: buckets, application keys, Object Lock, and the Partner API's Groups (`b2_list_groups`,
`b2_create_group_member`, `b2_list_group_members`, `b2_eject_group_member`). It is
self-hosted, Node.js, version 0.2, and "incubating". Hosting it means a container or Worker of
pigeon's own, with OAuth or Access in front, one per account.

**Decision:** for reading buckets, B2 goes through `storage` like the others, at
`https://s3.<region>.backblazeb2.com` (such as `us-west-004`, `eu-central-003`), with a B2
application key (key ID as the access key, application key as the secret). B2's own server is
the right tool for the Groups phase (F2), where pigeon would host it behind the portal; see §6.

### Why one pigeon capability rather than three portal servers

1. **It works for all three today.** The S3 calls needed (ListBuckets, ListObjectsV2,
   HeadObject, GetObject, presigned GET) are the same on all three.
2. **Least privilege, decided by pigeon as well as the key.** The capability refuses a write on
   a read-only account and a bucket outside the account's list before any request is made. A
   provider server enforces only what its key allows.
3. **Several accounts in one place.** One tool set with an `account` argument, rather than one
   portal server, one container and one Access application per account.
4. **pigeon's approvals apply.** Calls go through `dispatch`, so the person's policy can gate
   a tool with a passkey (phase 3's writes). Calls to another portal server go around pigeon's
   policy, as buildkite-mcp.md §2 notes for Buildkite.
5. **The later ingestion runs in the Worker anyway.** It needs an S3 client in Rust, not an MCP
   round trip through the portal's 60 s cap.

The cost: pigeon owns a small S3 client (signing and three XML readers). No AWS SDK is used: it
would not build for WASM, and pigeon needs a handful of calls.

## 2. How it sits behind the portal

```
claude.ai "Pigeon MCP" ──OAuth (Access)──▶ mcp.pigeon.dev/mcp  (portal "pigeon")
 (cloud sessions inherit it)                 └─ server "pigeon" → platform.pigeon.dev/mcp
                                                  pigeon_storage_accounts
                                                  pigeon_storage_list_buckets {account}
                                                  pigeon_storage_list_objects {account, bucket, …}
                                                  pigeon_storage_head_object  {account, bucket, key}
                                                  pigeon_storage_get_object   {account, bucket, key, presign?}

pigeon-platform Worker (capability `storage`)
   ├─ scaleway:<name>     ──SigV4, STORAGE_SCALEWAY_<NAME>────▶ https://s3.<region>.scw.cloud
   ├─ digitalocean:<name> ──SigV4, STORAGE_DIGITALOCEAN_<NAME>─▶ https://<region>.digitaloceanspaces.com
   └─ backblaze:<name>    ──SigV4, STORAGE_BACKBLAZE_<NAME>───▶ https://s3.<region>.backblazeb2.com
```

- The tools are pigeon's, so they reach the portal on its existing server `pigeon`. The sync
  after each landing's deploy lists them. Nothing is attached in the dashboard.
- The person can switch any of them off in the portal (MCP Portals → `pigeon` → the tool's
  switch), as with `pigeon_call`. A switch survives syncs.
- They are not pre-approved in `.claude/settings.json`, so a session asks before each call.
  Pre-approving the read tools would be a policy change, which is the person's.
- The portal's 60 s cap holds easily: each tool is one or two S3 requests. Big objects are
  handed out as presigned links, never pulled through the portal.

## 3. Accounts

### Naming

An account's id is `provider:name`.
- `provider` is `scaleway`, `digitalocean` or `backblaze`.
- `name` is 1 to 32 characters of `a-z`, `0-9` and `-`, not starting or ending with `-`. The
  person picks it: `personal`, `work`, `legacy`, `platform`.
- Ids are unique. Two accounts at one provider are two names: `scaleway:personal` and
  `scaleway:work`.
- Where a Cloudflare id is needed (a portal server in F2), the `:` becomes `-`:
  `backblaze-platform`. That fits the portal's rule (no `_`, at most 32 characters).

### What an account is

Declared in `src/platform/src/settings.rs`, in `storage_accounts()`, which is empty:

| Field | What | Example |
| --- | --- | --- |
| `provider`, `name` | Its id | `Scaleway`, `personal` |
| `endpoint` | The S3 endpoint, https, no trailing `/` | `https://s3.fr-par.scw.cloud` |
| `region` | The SigV4 signing region | `fr-par` |
| `mode` | `ReadOnly` or `ReadWrite` | `ReadWrite` |
| `buckets` | The buckets pigeon may touch; empty means any the key can see | `["photos", "archive"]` |

Constructors fill the endpoint from the region: `Account::scaleway("personal", "fr-par")`,
`Account::digitalocean("legacy", "nyc3")` (always `ReadOnly`),
`Account::backblaze("platform", "us-west-004")`.

`validate` checks the list at startup: the name rules, unique ids, an https endpoint, bucket
names (3 to 63 of `a-z`, `0-9`, `.` and `-`), and that no DigitalOcean account is `ReadWrite`.
A list that fails leaves the capability in place, and every tool answers the reason.

### Credentials

- **One Secrets Store secret per account**, named `STORAGE_<PROVIDER>_<NAME>` in upper case with
  `-` as `_`: `STORAGE_SCALEWAY_PERSONAL`, `STORAGE_DIGITALOCEAN_LEGACY`.
- Its value is one line, `ACCESS_KEY_ID:SECRET_ACCESS_KEY`. None of the three providers uses `:`
  in a key ID, so the first `:` splits it.
- It is set on the console's Access tab (Secrets and credentials), which asks for the person's
  passkey bound to that name and the value's hash. The value goes straight to the Secrets Store
  and is never shown, logged or kept by pigeon (secrets.rs).
- Before it is written, `check_secret` tries it: ListObjectsV2 with one key on the account's
  first bucket when it has a list, otherwise ListBuckets. A key that is refused is never
  written over a working one.
- The Worker reads it through a `[[secrets_store_secrets]]` binding of the same name in
  `src/platform/wrangler.toml`. That binding is added **after** the secret exists, because a
  binding to a missing secret may stop the deploy (buildkite-mcp.md, phase 1).
- Never in code, docs, chat or a command line. Rotating a key is setting the secret again.

### Picking an account

Every tool except `storage_accounts` takes `account`, the id. There is no default account, even
when only one exists: a call always says which account it reads, so the record of a call is
unambiguous. `storage_accounts` lists the ids, their mode and buckets, and whether each one's
secret is bound and set (never its value).

### Access mode

| Provider | Mode | Enforced by |
| --- | --- | --- |
| DigitalOcean | Read-only, always | The key (Spaces key with `read` on chosen buckets), and `validate`, which refuses a `ReadWrite` DigitalOcean account |
| Scaleway | Read-write when declared so | The key's IAM policy, and the capability: write tools (phase 3) refuse a `ReadOnly` account |
| Backblaze | Read-write when declared so | The key's capabilities, and the same refusal |

Until phase 3 lands, keys should be read-only for every account (Follow-ups, below), so the
key matches what the code can do.

## 4. The tool surface

Phase 1, built:

| Tool | Arguments | Answers |
| --- | --- | --- |
| `storage_accounts` | none | Each account: id, provider, mode, endpoint, region, buckets, binding, `configured`. With none: an empty list and the setup steps |
| `storage_list_buckets` | `account` | Buckets the key can see, filtered by the account's list |
| `storage_list_objects` | `account`, `bucket`, `prefix?`, `delimiter?`, `max_keys?` (100, at most 1000), `token?` | `objects` (key, size, etag, last modified), `prefixes`, and `next`: pass it back as `token` for the next page |
| `storage_head_object` | `account`, `bucket`, `key` | Size, content type, etag, last modified |
| `storage_get_object` | `account`, `bucket`, `key`, `presign?`, `expires_s?` (300, at most 3600) | Inline, for objects up to 1 MiB: the body as UTF-8 text or base64. With `presign`: a presigned GET link and its lifetime |

- With no account declared, `storage_accounts` answers how to set one up, and every other tool
  answers an error with the same steps. Nothing is fetched.
- A bucket outside a non-empty `buckets` list is refused before any request.
- A presigned link is a bearer link: anyone holding it can read that one object until it
  expires. It is capped at an hour and defaults to five minutes.
- S3 errors come back as `S3 <status> <Code>: <Message>`, never with the request's headers.

Phase 3, to come, for `ReadWrite` accounts only:

| Tool | Arguments | Gate |
| --- | --- | --- |
| `storage_put_object` | `account`, `bucket`, `key`, `body` (≤ 1 MiB, text or base64), `content_type?` | The person's passkey on each call (`call_tool`, Follow-up 7) |
| `storage_presign_put` | `account`, `bucket`, `key`, `expires_s?` | The same |
| `storage_delete_object` | `account`, `bucket`, `key` | The same |

## 5. Decisions

1. **One pigeon capability over S3 for all three providers**, not their MCP servers (§1). The
   providers' servers are recorded, and B2's is planned for F2.
2. **Hosted in core (`pigeon-platform`), not a new Worker.** The person's rules for tonight
   forbid a new Worker in `ci/workers.toml`. The S3 client is in `pigeon_core::s3`, so a later
   `pigeon-storage` Worker (with ingestion) can take it unchanged. A cost: `src/core` is
   `shared` in the path map, so landing phase 1 deploys every Worker once.
3. **Path-style URLs** (`{endpoint}/{bucket}/{key}`) on every provider: one host per account,
   and bucket names with dots work.
4. **Accounts declared in code, empty by default.** The instances plan keeps per-account values
   as data the person approves (linked-accounts.md, decision 7). Storage accounts are code for
   now because each one also needs a Secrets Store binding in `wrangler.toml`, which is code
   and a landing anyway; the declaration rides in the same landing and is reviewed with it.
   When accounts become opt-in features of instances (F2), the record moves into the instance's
   data, and this list goes. Open question 1.
5. **One secret per account in the `KEY_ID:SECRET` form**, not two secrets, so one passkey
   write and one binding per account, and one live check of the pair.
6. **DigitalOcean is read-only in code**, not just by key, as the person asked for the legacy
   accounts. Relaxing it would be a one-line change in `validate`, reviewed in a landing.
7. **No default account.** Every call names one.
8. **Inline reads are capped at 1 MiB**; larger objects are presigned. The same cap as B2's own
   server; it keeps tool answers small and well inside the 60 s cap.
9. **No AWS SDK.** SigV4 is about a hundred lines on the `hmac` and `sha2` crates core already
   uses, and the smoke scenario checks it against AWS's published examples.
10. **No question was asked tonight.** The person is asleep, and every choice above has a
    conventional answer. The ones they may want to change are the open questions.

## 6. Phases

| Phase | What | Who | Lands with |
| --- | --- | --- | --- |
| 1 | **Registry and read tools, off.** `pigeon_core::s3` (SigV4 header and presigned signing, the ListBuckets, ListObjectsV2 and Error readers, `send`); `pigeon_capabilities::storage` (`Account`, `validate`, the five tools, one secret slot per account with a live `check_secret`); registered in `settings.rs` with `storage_accounts()` empty. Smoke: `storage_reads_s3_compatible_accounts` with a stubbed S3 endpoint (recorded XML), both AWS signing examples, and the registry's rules. | Agent (built, `claude/ai-agent-q7VjHJXE6Mv0`) | `land` by the person's reviewer. No passkey: no policy or settings.json change. |
| 2 | **First accounts, live.** For each account the person wants: declare it in `storage_accounts()` and land; the person sets the secret on the Access tab (the live check runs); bind it in `wrangler.toml` and land. Check from a session: `pigeon_storage_accounts` shows it `configured`; list buckets, page through a bucket of more than 1000 objects, head, get a small object inline, get a large one presigned. Settle the open points: DigitalOcean's signing region (the datacenter slug or `us-east-1`); ListBuckets with a B2 key limited to one bucket; Scaleway's preferred project for Object Storage. | Person (keys, Follow-ups 1 to 6); agent (two landings per batch of accounts) | `land` |
| 3 | **Writes for read-write accounts.** `storage_put_object`, `storage_presign_put`, `storage_delete_object`, refused on `ReadOnly` accounts and outside `buckets`. They are added only after the person's policy gates them with a passkey (Follow-up 7); until then the plan holds. Keys for those accounts gain write permission (Follow-up 8). Smoke: the refusals and the signed PUT. | Person (policy, keys); agent | `land` after the policy change (passkey) |
| 4 | **Ingestion**, its own plan: what "consume into Pigeon Platform" means (copy to R2, index, or read in place), schedules, and the move of `storage` into its own Worker if it grows. | Person decides; agent | — |

### Future phases (not built, not designed in detail)

**F1. Scaleway spot compute.** Scaleway as a source of larger, cheaper machines: a CI
connector (ci-gateway.md's `Connector`) or an AI-agent sandbox that starts a Scaleway Instance
for a job and deletes it after. The Instance API is REST with the same IAM keys; a separate key
with only `InstancesFullAccess` on a dedicated project. For sessions to look at what is
running, Scaleway's official server (`scw mcp server serve --transport streamable-http
--read-only --namespaces instance`) could run in a Cloudflare Container behind Access and join
the portal as server `scaleway-compute`. Its HTTP transport has no auth, so it must never be
reachable except through Access. Its own plan decides.

**F2. A Backblaze platform account with Groups.** One B2 platform account, with B2 Groups (the
Partner API) creating a member account or group per feature. Each group is a feature an
instance opts into (linked-accounts.md, decision 7: instances are data the person approves),
starting with cold-storage backups: an instance that opts in gets its own bucket (Object Lock
on, a lifecycle rule) and its own key limited to that bucket, recorded in the instance's data.
B2's official server is the natural control plane: hosted by pigeon (a Cloudflare Container
from its image, `B2_HTTP_CREDENTIAL_MODE=server`, behind Access) as portal server
`backblaze-platform`, with destructive tools left blocked. Groups need the master key and the
Partner API entitlement, which is a conversation with Backblaze first. The backups themselves
(what is backed up, how often, restore drills) are their own plan.

## Rollback

- Phase 1 changes nothing while `storage_accounts()` is empty. To remove it, revert the landing.
- An account: remove it from `storage_accounts()` and its binding from `wrangler.toml`, land,
  then delete the secret on the Access tab and revoke the key in the provider's console.
- At once, without a landing: switch the `pigeon_storage_*` tools off in the portal, and revoke
  the provider key. pigeon's calls then fail with the provider's refusal.

## Follow-ups for the person

Nothing here is needed for phase 1 to land; it changes nothing until step 4. Do one provider at
a time; DigitalOcean first is the safest, as it is read-only.

1. **Choose names.** One per account: `digitalocean:legacy`, `scaleway:personal`,
   `scaleway:work`, `backblaze:platform`, or your own (lowercase, digits, `-`). Note each
   account's region and the buckets pigeon should see (or "all").
2. **DigitalOcean (read-only).** Control panel → Spaces Object Storage → Access Keys → Create
   Access Key → **Limited Access** → tick only the buckets wanted, each with **Read** → name it
   `pigeon-<name>` → Create. Copy the access key and the secret (the secret is shown once). Do
   not create a personal access token: pigeon does not need one.
3. **Scaleway (read-only for now).** Console → IAM → Applications → Create application
   `pigeon-<name>` → Policies → Create policy for that application, scope: the project holding
   the buckets, permission set **`ObjectStorageReadOnly`** (or `ObjectStorageBucketsRead` plus
   `ObjectStorageObjectsRead`) → then the application's API keys → Generate API key, with
   **Preferred Project for Object Storage** set to that project. Copy the access key (`SCW…`)
   and the secret key. One application per account.
4. **Backblaze B2 (read-only for now).** Web console → Application Keys → Add a New Application
   Key → name `pigeon-<name>` → Allow access to bucket(s): one bucket, or All if pigeon should
   see them all → Type of Access: **Read Only** → Create. Copy the keyID and the applicationKey
   (shown once). Do not use the master key: the S3 API refuses it. The S3 endpoint is on each
   bucket's page (`s3.<region>.backblazeb2.com`); the region is the middle part.
5. **Ask an agent to declare the accounts** (a session or a planner run): "declare storage
   accounts `<ids>` with regions `<…>` and buckets `<…>` in `storage_accounts()` per
   docs/plans/storage-providers.md, and land it". This adds no binding yet, so the deploy is
   safe. The console's Access tab then lists `STORAGE_<PROVIDER>_<NAME>` as missing.
6. **Set each secret.** Console → Access → Secrets and credentials →
   `STORAGE_<PROVIDER>_<NAME>` → Set, value `ACCESS_KEY_ID:SECRET_ACCESS_KEY` on one line →
   approve with your passkey. pigeon tries the key before writing it and tells you how many
   buckets it saw. Never paste a key into a chat or a command. Then ask an agent to "bind the
   storage secrets in `src/platform/wrangler.toml` and land" (phase 2).
7. **Before phase 3 (writes):** add `storage_put_object`, `storage_presign_put` and
   `storage_delete_object` to the `targets` of rule `risky-tools-need-passkey` in
   `.pigeon/policy.toml`, so each write waits for your passkey on the Approvals tab. Only you
   change the policy; it lands with your passkey.
8. **Also before phase 3:** for each read-write account, replace its key with one that may
   write: Scaleway `ObjectStorageObjectsRead` + `ObjectStorageObjectsWrite` (+
   `ObjectStorageObjectsDelete` if deletes are wanted); B2 Type of Access **Read and Write**.
   Set it on the Access tab as in step 6, then revoke the old key.
9. **Optional:** to pre-approve the read tools in sessions, add them to the policy as you did
   for `land_status`. Without it, sessions ask before each call.
10. **For F2 only, when you get there:** ask Backblaze about the Partner API and Groups for
    your platform account; that is what B2's Groups tools need.

## Open questions for the person

1. **Accounts in code or as data.** Tonight's accounts are declared in `settings.rs` and land
   with their binding (decision 4). Would you rather approve them as data on the console, like
   instances? That needs credentials stored outside `wrangler.toml` bindings (sealed in the
   Directory, as linked accounts' tokens are), which differs from the brief's Secrets Store.
2. **Presigned links.** Allowed for every account, an hour at most. Should DigitalOcean (the
   legacy, read-only accounts) get inline reads only?
3. **"Consume into Pigeon Platform".** Copy into R2, index in place, or read on demand? This
   shapes phase 4 and whether `storage` becomes its own Worker.

## Log

- 2026-11-22, overnight run (planner `claude/ai-agent-6xFkoSnmSyJU`):
  - Researched the three providers' MCP servers from the sandbox (Sources). None fits object
    reading behind the portal; designed the `storage` capability over S3 instead.
  - Phase 1 built by an executor on `claude/ai-agent-q7VjHJXE6Mv0`, off by default
    (`storage_accounts()` empty), with the smoke scenario
    `storage_reads_s3_compatible_accounts`. Not landed: the person's reviewer lands it.
  - Nothing in `ci/`, `.pigeon/`, `.claude/`, `ci/workers.toml` or any `wrangler.toml` changed.

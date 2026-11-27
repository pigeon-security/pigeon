# The data store and email on linked instances, inline or in a Worker of their own

Status: **built 2026-11-27 on `claude/linked-data-email`; nothing enabled.** No linked account
was read or changed while building. Enabling on noisypigeon and willowgraysen is the person's
step after this lands (see "What the person does").

Related plans: [email.md](email.md) (the lite data store, the mail steps, the DNS review),
[data-worker.md](data-worker.md) (the data Worker), [capability-scoping.md](capability-scoping.md)
(catalogue, placements), [linked-accounts.md](linked-accounts.md) (links and their scopes),
[redirects.md](redirects.md) (the site Worker's redirects).

## Goal

The person's words (2026-11-27): any instance with a dedicated zone may enable `data` and
`email`, not only pigeon.dev. noisypigeon (zone noisypigeon.com) and willowgraysen (zone
willowgraysen.com, whose apex redirects to noisypigeon.com) must be able to. Add a placement
choice for the data Worker: `inline` (the email handler and the signed `/data/call` ops run in
the instance's own Worker, such as `noisypigeon`) or `own_worker` (a Worker of its own,
`{instance}-data`, as `pigeon-data` on home). The person's configuration:

| Instance | Placement | The store's Worker | Bucket and D1 |
| --- | --- | --- | --- |
| pigeon.dev (home) | `own_worker`, read-only | `pigeon-data` | `pigeon-data` |
| noisypigeon | `inline` | `noisypigeon` (its site Worker) | `noisypigeon-data` |
| willowgraysen | `own_worker` | `willowgraysen-data` | `willowgraysen-data` |

pigeon.dev keeps working exactly as before: its plan, token (`data_home`), steps and digests do
not change.

## Decisions

1. **The record.** `[capabilities.data] placement = "inline" | "own_worker"`. On a linked
   instance an absent placement means `own_worker` (the safer default: the instance's own
   Worker is untouched). On home it is read-only and `own_worker`, as before. `email` runs
   where `data` runs: its own `placement`, if given, must equal data's.
2. **Zones.** Email needs a zone: its domain is the record's `domain` or its first zone, and
   must be one of its zones (unchanged rule). An instance with no zone (such as `sandbox`)
   cannot enable email. **Data alone needs no zone**: the store (bucket, D1, Worker) is
   account-wide, so `sandbox` may enable `data` without email; nothing receives mail into it
   until a zone and `email` are added.
3. **Inline needs the instance's own Worker.** A record placing data `inline` must name a
   `worker`; otherwise it is refused at validation ("choose own_worker").
4. **The inline Worker is composed, not rebuilt.** The site Worker stays the JavaScript module it
   is (`site_worker.js`, wrapped by the protected-origin and redirect scripts as before). For an
   inline store the plan uploads the instance Worker with several modules:
   - `worker.js`, the main module (`instances/data_inline.js`): a request that is a `POST` to
     `/data/call` on the Worker's `workers.dev` host goes to the data Worker's `fetch`; every
     other request goes to the site module unchanged (redirects included); `email` and `queue`
     go to the data Worker.
   - `site.js`: the site module exactly as an instance without data has it.
   - the data Worker's build (`index.js` and its WASM), from `pigeon-data-artifacts`, checked
     against its digest as for `own_worker`.

   The data Worker's handlers are the prototype methods worker-build's shim defines; the main
   module calls them with the Worker's own `env` and `ctx`, as the runtime would. So **no change
   under `ci/` is needed**: the same CI build serves both placements.
5. **Bindings of the inline Worker**: the site's (`SITE`, `PIGEON_ORIGIN_KEY`, `PIGEON_REDIRECTS`,
   Service Bindings) plus the data Worker's (`DATA`, `DB`, `INSTANCE`, `DATA_KEY`). Every upload
   makes a new `DATA_KEY` and keeps it, with the Worker's `workers.dev` URL, as
   `data_key:{instance}` and `data_url:{instance}`, as an own Worker's upload does.
6. **workers.dev.** Core reaches the store at `https://{worker}.{subdomain}.workers.dev/data/call`
   for either placement, so an inline store turns on the instance Worker's `workers.dev`
   address (an `EnableWorkersDev` step, shown in the plan). Public custom domains never reach
   the data handler.
7. **The plan's order on a linked instance**: the capability Workers (M5), then the store's
   bucket, D1 and migrations (and, for `own_worker`, the `{instance}-data` upload), then the
   site's steps (whose `UploadWorker` carries the inline store), then the email configuration
   and the mail steps (DNS review, Email Routing, catch-all to the store's Worker, forward
   destinations), then the capability steps. The catch-all and any queue consumer name the
   store's Worker: `noisypigeon` inline, `willowgraysen-data` own.
8. **The digest.** An inline upload's ledger digest is the site's digest combined with the data
   build's digest, the store's names and the composing module, so a new data build re-uploads
   the instance Worker (shown as `UploadWorker`, replaces). An instance without inline data
   keeps the digest it had.
9. **Credentials and scopes.** A linked instance's data steps use its link's token
   (`linked::account_for`), never a minted one. Beyond the base link scopes (R2 write, Workers
   Scripts write, DNS write, Zone read), the link needs:
   - `data`: **D1 Write** (`d1.write`);
   - `email`: **Email Routing Rules Write** (`email-routing-rule.write`), **Email Routing
     Addresses Write** (`email-routing-address.write`) and **Zone Settings Write**
     (`zone-settings.write`).

   They are added to `linked::CAPABILITY_SCOPES` (optional scopes, as `secrets`'). A link
   without one is refused for that capability: the plan shows a `Keep` step "re-link to enable"
   naming the scopes, and nothing of the store is planned (the site's steps still apply).
   `linked_status` lists the capability under `relink_for`.
10. **B2 backup** on a linked instance is not built: a record with `[capabilities.data.backup]`
    on a linked instance is blocked in the plan, with why. (Its key would live in the linked
    account's Secrets Store; that is a later step.)
11. **Every DNS record is shown.** Neither noisypigeon.com nor willowgraysen.com receives mail
    today (the person, 2026-11-27), so Email Routing may take over their MX, and no provider is
    preserved. The plan still shows each record before the passkey: while Email Routing is off
    the observe reads what turning it on writes (`GET /zones/{id}/email/routing/dns`), and the
    `EnableEmailRouting` step lists each record as "add", "already there", or "add ..., beside
    ... (kept)" when the zone has another record of that type and name
    (`data_plan::routing_adds`). Stale Fastmail or Proton records remain their own
    `RemoveStaleDns` steps, and a foreign MX still stops the plan (email.md). Unread (an error),
    the step falls back to the general description. Home's step is unchanged: its Email Routing
    is already on, and the field is absent when empty.
12. **The email tools and the Email tab** reach the store through the Directory's
    `data_url`/`data_key`, whichever placement wrote them, so they need no per-placement code;
    the linked refusal in `data_store::reach` goes.

## What the person does (after this lands)

1. **Register the new scopes**: run `linked_setup` once; it patches pigeon's OAuth client with
   the four new optional scopes.
2. **Re-link noisypigeon and willowgraysen** (Instances › {instance} › Overview, Link) and allow
   D1 Write, Email Routing Rules Write, Email Routing Addresses Write and Zone Settings Write
   (and Secrets Store Write, if wanted). Their links today grant only the base scopes.
3. **Propose the records** with `instance_propose` (or the console's Capabilities view):
   - noisypigeon: `[capabilities.data] placement = "inline"` and `[capabilities.email]` (with
     its addresses and forward, if any);
   - willowgraysen: `[capabilities.data] placement = "own_worker"` and `[capabilities.email]`.

   Approve each with the passkey.
4. **Plan and apply** each with `instance_plan` / `instance_apply`. The plan shows, before the
   passkey: the bucket and D1, the migrations, the upload (`noisypigeon` with the data build
   digest, or `willowgraysen-data`), `workers.dev` for noisypigeon's Worker, the DNS review of
   the zone (a foreign MX stops the plan; each stale Fastmail or Proton record is its own
   `RemoveStaleDns` step with its id and content), `EnableEmailRouting` with each record it adds,
   the catch-all to the store's Worker, and each forward destination.
   willowgraysen.com's redirect to noisypigeon.com is unchanged: it is answered by
   `willowgraysen`, which the own-Worker store does not touch.

## Tests (smoke)

- home `own_worker` unchanged: the same steps and digest as before for pigeon.dev's record;
- linked `own_worker`: `willowgraysen-data` uploaded, the catch-all and site untouched;
- linked `inline`: no data Worker upload; the site's `UploadWorker` carries the store, its
  digest changes, `workers.dev` is turned on, the catch-all names the instance Worker;
- a record without a zone is refused email; data alone without a zone is accepted;
- the link scopes: a link without `d1.write` is refused for `data` with the scopes named;
- the Email Routing records are listed in the plan, each added, already there or beside a kept
  one;
- the composing module routes only `POST /data/call` on `workers.dev` to the data Worker.

## What is not verified

- No linked account has been planned or applied: the D1, Email Routing and Workers calls with a
  link's OAuth token have not run. The first `instance_plan` is a dry run.
- That the composed module runs: worker-build's shim keeps its handlers on the entrypoint's
  prototype (`worker-build` 0.9.0, `src/js/shim.js`); the composing module relies on that and
  calls them with `{ env, ctx }`. A later worker-build that changes its shim needs this module
  checked. Checked with `node --check` only.
- Whether `zone-settings.write` is what Cloudflare asks to enable Email Routing with an OAuth
  token (it is what `data_home` holds for home).

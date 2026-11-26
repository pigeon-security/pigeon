# Platform manages platform: the apex, the landing page and the console

Status: **proposed** (2026-11-26). Nothing is built. Three requests from the person, planned
together because they share one decision: which Worker owns `pigeon.dev`.

1. Serve the console at `pigeon.dev/platform` instead of `platform.pigeon.dev/app`.
2. Serve a new landing page at `pigeon.dev` through the publishing capability, as a template
   that is a peer of `blog`.
3. Make the home instance a normal, managed instance, so "the platform manages the platform"
   is true.

Code references are to main at the time of writing.

## Today

- `pigeon.dev` is a Workers Custom Domain in the platform account, owned by `pigeon-platform`
  or by `pigeon-sites` depending on a live setting (`settings::site()`,
  `src/platform/src/settings.rs:126-133`; plan `sites.md` B3 is still open). `pigeon-sites`
  renders it from `src/sites/src/apex.rs` with hand-written CSS in `src/core/src/home.rs`, plus
  dynamic `/architecture`, `/architecture.json` and `/logo.png`.
- `ci/deploy/deploy.sh:144-166` and the `site_publish` pre-flight (`site.rs:109-131`) require
  `https://pigeon.dev/architecture.json` to answer with the deployed commit.
- The console, OAuth issuer, MCP, `/auth` enrolment, the GitHub webhook, the linked-account
  callback, Apple/AASA and the iOS app all live on `platform.pigeon.dev`. Passkeys use RP ID
  `platform.pigeon.dev` with an exact origin check (`passkeys.rs:62-63`, `webauthn.rs:75`).
  The console cookie is `__Host-pigeon_session; Path=/; SameSite=Strict` (`session.rs:10,23`).
- The home instance `pigeon-security/platform` is `account: home`, no zones, no sites, no
  Worker, CI only. `instance_plan` refuses home records (`instances.rs:454-458`); publishing
  requires a GitHub repo and a linked account (`record.rs:634-648`, `sites_scope.rs:19-29`,
  `deploy.rs:75-86`).

## The shared decision: core owns the apex

Recommended: **`pigeon-platform` (core) owns the `pigeon.dev` custom domain** and is its front
door:

- `pigeon.dev/platform*` → the console, served by core directly.
- Everything else on `pigeon.dev` → forwarded to `pigeon-sites` over the existing service
  binding (the remote route-forwarding path), where the landing release and the dynamic
  `/architecture*` routes live.

This keeps one owner for the hostname, needs no Workers route API (outside the `/cf/`
allowlist, `src/platform/wrangler.toml:5-8`), and lets core decide per path which code may
answer on the shared origin.

## 1. Console at `pigeon.dev/platform`

### The security cost, stated plainly

Putting the console on the same origin as a public site means any script served anywhere on
`pigeon.dev` can call the console's reads and writes with the person's cookie (SameSite and
the Origin check both pass for same-origin requests), a service worker at `/` could intercept
`/platform`, and CSP `'self'` would cover the whole site. The `__Host-` cookie cannot be scoped
to `/platform`. This is acceptable **only if `pigeon.dev` serves first-party static content
with no scripts** — which the `landing` template below enforces. If `pigeon.dev` must ever host
user, instance or third-party content, use the fallback: keep the console on
`platform.pigeon.dev` and make `pigeon.dev/platform` a redirect.

### Passkeys

A page on `pigeon.dev` cannot use RP ID `platform.pigeon.dev`. Options:

- **Related Origin Requests (recommended):** `platform.pigeon.dev` serves
  `/.well-known/webauthn` listing `https://pigeon.dev`, and the relying party accepts a list of
  origins. Existing passkeys keep working. Supported in current Chrome and Safari; Firefox
  support must be verified.
- Re-enrol under RP ID `pigeon.dev`: every person registers again, the iOS app keeps
  `platform.pigeon.dev`, and the new RP ID is usable from every `*.pigeon.dev` page.

### Phases

- **C0, inert code.** Console base path becomes a setting (`page::BASE`, assets, bookmarks,
  `shell.js` via `data-base`); a list of console origins for the Origin check; a relying party
  with an origin list and `/.well-known/webauthn`; fix `site_passkeys/console.rs:26-30`, which
  derives the zone by dropping the first label of the console host (it would yield `dev` for
  `pigeon.dev`). On the apex, core answers `/platform*` with the console only — never `/mcp`,
  `/oauth`, `/auth` or `/.well-known`. Tests for each.
- **C1, both URLs live.** A separate cookie name for the apex console
  (`__Host-pigeon_console`), a strict CSP that lists the console's own script paths instead of
  `'self'`, `Clear-Site-Data` on sign-out.
- **C2, redirect.** `platform.pigeon.dev/app/*` and the old bookmarks answer 301 to
  `pigeon.dev/platform/*`, except `/app/oauth` (the iOS universal link). Update the landing
  link and docs.
- **Stays on `platform.pigeon.dev` for good:** `/mcp`, `/oauth/*`, `/.well-known/*`, `/auth/*`,
  `/github/hook`, `/linked/callback`, `/apple/*`, `/app/oauth`, `/health`. No change for iOS,
  OAuth clients or the GitHub App.
- **C3, only if needed:** a Firefox sign-in handoff if Related Origins is unsupported there.

## 2. Landing page as the `landing@1` template

Build a pure `pigeon-landing` crate, a peer of `pigeon-blog`, and publish it as immutable
releases with `site_deploy` / `site_releases` / `site_rollback`, managed from the console's
Site › Deployments.

- **Template:** input `site.toml` (`template = "landing@1"`, title, nav, links) and
  `content/pages/*.md`; output `index.html`, pages, `404.html` and one stylesheet. **No
  scripts** (this is what makes the shared origin above acceptable; a smoke test enforces it).
  The stylesheet embeds the console's design tokens at compile time, so `template_digest`
  covers them and a token change requires a `landing` version bump (smoke test ties digest to
  version).
- **Dispatch:** templates are wired in directly today (`deploy.rs:425-437`); add a template
  enum chosen by the `site.toml` prefix, a `landing` pin and version component
  (`versions.rs`), and a per-template renderer digest.
- **Source:** a `site/` directory in the pigeon repo (Artifacts), with deploy honouring
  `sites[].build` as the root (the field exists but is unused, `record.rs:134-136`). Needs an
  Artifacts archive read (today only single files, `artifacts.rs:566-591`).
- **Storage:** a home-account R2 bucket bound to `pigeon-sites`, written through the binding,
  so no account token reaches `pigeon-sites`.
- **Serving:** `apex.rs` serves the live release for every path except `/architecture*` and
  `/logo.png`, which stay dynamic (a build-time snapshot would go stale and break
  `deploy.sh`). It can fill a `<!--pigeon:build-->` marker with the commit, and falls back to
  `home_html` when no release exists. Then land `sites.md` B3.

### Phases

- **L1** the template crate, dispatch, version component, tests.
- **L2** source: `build` subdirectory, Artifacts archive read, a home-only
  `SitesScopeRequest` for it.
- **L3** storage and deploy for the home account, sharing release/rollback/list code.
- **L4** serving from `apex.rs`; update the apex smoke scenarios.
- **L5** console management (already works once publishing is enabled) and, later, publish on
  push from Artifacts events.

## 3. Platform manages platform: the home instance

Make the home instance a first-class target of plan/apply, with two kinds of resources:

- **Managed** — what `instance_apply` may create and the ledger records (never deletes).
- **Platform-owned** — what the Lander and setup tools own (`pigeon-platform`, `pigeon-sites`,
  the gateways, `proxy`, `ai-agents`, the apex/`platform.`/`proxy.` hosts, Secrets Store,
  D1/DO, queues). Shown read-only in the home Overview from the architecture inventory; never
  in plan steps and never in the record, so record digests never carry Lander state.

### Record

Add `zones: ["pigeon.dev"]`, the landing site (bucket and Worker names that cannot collide
with reserved names), `publishing` for it, `observability` (on the managed Worker, not core),
`source`; keep `ci`.

### Guard rails, in code (the home account has no account boundary)

1. A pure check in `plan::plan`: for home, any reserved Worker or footprint host is `Blocked`,
   never uploaded or attached. The ledger-conflict rule is a second line.
2. Validation: exactly one home record; the account kind can never change in a draft; home
   sites may not target reserved labels.
3. Retire stays refused for home. Ingest and storage stay linked-only for now.

### Credentials

For home, `instance_plan`/`apply` and publishing mint a short-lived purpose-scoped token from
the parent token (a new purpose, e.g. `instance_home`: R2 write, Workers scripts write, custom
domains on the zone), revoked after each apply, standing in for the OAuth `bearer` linked
instances use. Metrics and Source already accept home (`observability/instance.rs:23,145`,
`source/read.rs:19-23,91`).

### Phases

- **H1** guard rails and validation (no behaviour change), with tests.
- **H2** home plan/apply with the minted token.
- **H3** home publishing and metrics (pairs with L2–L3).
- **H4** console: managed items plus the read-only platform inventory on the home Overview.

## Order

1. **Decide** the open questions below.
2. **H1** (guard rails) and **L1** (template) in parallel — both inert.
3. **H2**, then **L2–L3** with **H3**.
4. Move the apex to core as the front door; **L4** behind it.
5. **C0 → C1 → C2** for the console.
6. **H4**, **L5**.

## Steps only the person can take

- Approve the new home record, and each home `instance_apply` (passkey).
- Add the new `instance_home` purpose to `purpose_permissions` (lands through the gate) and
  confirm the parent token holds those permissions.
- Choose which Worker owns the apex and run `site_publish` accordingly.

## Open decisions

1. Will `pigeon.dev` ever host content that is not first-party and static? If yes, the console
   stays on `platform.pigeon.dev` and `pigeon.dev/platform` only redirects.
2. Passkeys: Related Origins (keep existing passkeys) or re-enrol under RP ID `pigeon.dev`?
3. Console URLs: `pigeon.dev/platform/<instance>/<area>/<view>`?
4. Old `platform.pigeon.dev/app` links: 301 permanently, or keep both indefinitely?
5. Landing content in the pigeon repo (`site/`, edits go through CI) or a separate Artifacts
   repo?
6. Home publishing from Artifacts (no GitHub App) or GitHub?
7. Home metrics: only the managed Worker, or the platform Workers too (needs a list of Workers
   on the profile)?
8. Should the landing replace `home.rs`'s styling everywhere, or only `/`?

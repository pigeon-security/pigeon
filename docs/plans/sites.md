# Sites: passkey sign-in for a site, and pigeon.dev on pigeon-sites

Status: planned 2026-11-23 (overnight). Branches below; nothing landed yet.
Status: B2 landed (`Z1YCPDSwx_kjMf2P`, deployed). A and B1 are built and green, but not
landed: since B2, the Lander refuses every branch ("the pack is malformed: it inflates too
large"; see the Log). The apex is still served by core.

Two features of `pigeon-sites` (`src/sites`):

- **A.** Passkey sign-in for a site that pigeon-sites serves. Off by default. A site opts in.
- **B.** pigeon.dev, redesigned, served by pigeon-sites instead of core. Its Architecture page
  draws a corrected diagram from core's architecture model, with nothing secret or personal.

Hard lines: no new policy rules; no edits to `.pigeon/policy.toml`, `.claude/settings.json`,
`ci/workers.toml` or `.pigeon/ci.toml`; tests only in `tests/smoke.rs`; no resources outside
pigeon's own Cloudflare account; no linked account touched; no long-lived secret minted.

## What is true today (read 2026-11-23 at `dc5814e`)

- **pigeon.dev (apex)** is a Workers Custom Domain on `pigeon-platform` (core). Core's fetch
  handler sends the apex to `site::Site::serve` (`src/capabilities/src/site.rs`), which answers
  `GET /` (the architecture page) and `GET /architecture.json` (the whole model) from
  `architecture::built`. `site_publish` attaches the apex to `Site.worker` with
  `custom_domain::attach` (`override_existing_origin`), so it moves the domain in one call.
- **`ci/deploy/deploy.sh`**, after every core deploy, reads `https://pigeon.dev/architecture.json`
  for up to two minutes and fails the deploy unless its `commit` is the commit just deployed.
- **pigeon-sites** answers core over the `SITES` binding (the contract), and from the internet
  only hosts `*.pigeon.dev` that approved instance plans attached: it passes GET and HEAD
  through to the instance Worker's workers.dev address (`src/sites/src/public.rs`), with the
  host map from core's `instance_subdomains`, asked as itself (`worker:pigeon-sites`). The apex
  does not end in `.pigeon.dev`, so today pigeon-sites does not serve it.
- **The model** is phase 5 of [architecture.md](architecture.md): one Worker per map entry,
  links, externals. The drawing is still the one-Worker layout (phase 6 not done). The person
  found the drawing incorrect (architecture.md, Open items). `/architecture.json` is public and
  is the whole model.
- **Passkeys**: `pigeon_core::webauthn` checks ceremonies against a `RelyingParty {id, origin}`.
  `capabilities::passkeys::Passkeys { host }` runs enrolment, sign-in and link approval over the
  Directory (`challenge.*`, `enrol.*`, `passkey.*`). The relying party is the host, so a
  passkey made on one host cannot sign in on another: the authenticator data carries the RP ID
  hash, and `webauthn::assert` checks it. Enrolment is a single-use link from `users_invite` or
  `users_enrol`.
- pigeon-sites builds for wasm32 only, so the smoke suite cannot link it. Its pure parts must
  live in a crate the suite links. `src/core` is in every lane, so it is the place.

## A. Passkey sign-in for a site

### Where it applies

Only to hosts pigeon-sites serves itself: the platform subdomains of instance sites
(`{label}.pigeon.dev`). The apex stays public. A site's own domain (such as `noisypigeon.com`)
is served by the instance Worker in the linked account and never passes through pigeon-sites,
so it cannot be gated here.

### The config: the instance record's site

A site opts in on its record (`SiteRecord`, `src/capabilities/src/instances/record.rs`):

```json
{ "name": "blog", "targets": [{ "kind": "subdomain", "label": "notes" }],
  "auth": "passkey", "allow": ["usr_fake1", "site"] }
```

- `auth`: absent (the default) or `"passkey"`. Any other value is a record problem.
- `allow`: who may sign in. Each entry is a pigeon user's id (a standard user), a site user's
  id, or `"site"`, which admits every site user enrolled for that host. Empty with `auth` set
  is a problem.
- Both fields are skipped when unset, so every existing record serialises exactly as before.
- A record changes only through `instance_propose` and the person's passkey, the existing
  approval flow. A site's repository cannot turn its own sign-in off.

I chose the record over `.pigeon/site.toml` (the brief's "site's config"): pigeon-sites has no
storage of its own, it already reads each host from the records (`instance_subdomains`), and
access rules belong where the person approves changes, not in content anyone with push access
can edit.

`instance_subdomains` answers each host with `auth` and `allow` when set, and nothing new
otherwise.

### Users

- **Pigeon users** are the existing standard users.
- **Site users** are Directory users of kind `site:{host}`. They are not standard users, so no
  console page, approval, link or OAuth flow accepts them (each checks `kind == "standard"`),
  and the platform's `/auth/enrol` refuses them.
- **Enrolment** is the existing single-use link. `users_invite {name, site}` makes a site user
  and `users_enrol {user_id, site}` makes a link for a standard user or that site's user. With
  `site`, the link is `https://{site}/_pigeon/enrol?code=…`, so the passkey is made with that
  host as its RP ID. Without `site`, both behave exactly as today.

### The flow on a gated host

Reserved paths, answered by pigeon-sites, only on a gated host (an ungated host passes them
through like any path):

| Path | What |
| --- | --- |
| `GET /_pigeon/sign-in?next=/path` | The sign-in page: one button, the shared `passkey.js` |
| `POST /_pigeon/sign-in/begin`, `/finish` | The ceremony, checked by core; finish sets the cookie and answers `{redirect: next}` |
| `GET /_pigeon/enrol?code=` and `POST …/begin`, `…/finish` | Registering a passkey for this host from a single-use link |
| `GET /_pigeon/passkey.js` | The browser's side of the ceremonies, the same file the console serves |
| `POST /_pigeon/sign-out` | Clears the cookie |

- Every other path, assets included, needs a valid session. Without one: `GET`/`HEAD` gets the
  sign-in page (401, `no-store`); anything else gets 401.
- The gate runs before the edge cache is read. A gated host's answers carry
  `Cache-Control: private, no-store`, so no shared cache keeps them.
- `next` is kept only if it is a path on this host (one leading `/`, not `//`, no `\`).

Core does the WebAuthn checks, as it holds the Directory. One core tool, `site_passkey`, for
caller `worker:pigeon-sites` only. It reuses `Passkeys { host: <site host> }`:

- steps `sign_in_begin`, `sign_in_finish` (answers the user's id, kind and name);
- `enrol_peek`, `enrol_begin`, `enrol_finish` (refuses a site user of another host).

pigeon-sites checks the allow list itself against the config it read.

### Sessions

- A cookie `__Host-pigeon-site`. It is host-only, `Secure`, `HttpOnly`, `SameSite=Lax`,
  `Path=/`.
- Its value is `{user}.{host}.{expires}` and an HMAC-SHA256 over it, keyed by
  `HMAC(CALLER_KEY, "pigeon-site-session\0" + host)`. That key is derived from a key the Worker
  already holds, so there is no new secret, and a cookie from one host never verifies on
  another.
- Sessions last 12 hours. Each request re-checks the allow list, so removing someone from
  `allow` ends their access when the host map refreshes (5 minutes). Removing a user ends it at
  expiry at the latest.

### Protected origin

A gated preview uses a dedicated linked instance, one site and an R2
bucket provisioned for protected content. Instance record validation rejects a
multi-site record that requests gated access before it can enter approval. The approved instance plan uploads
the authenticated origin before turning on its workers.dev address or attaching
the platform subdomain. That address accepts only the per-site bearer derived
from the platform caller key, the instance, site, bucket and origin URL. The linked Worker
receives only the derived key in a write-only secret binding.

After visitor authentication, `pigeon-sites` forwards a GET or HEAD with
the origin credential and requires the protected origin's response marker.
It skips the shared cache, refuses to follow redirects with the credential and
returns private, no-store responses. Existing public Worker modules and cached
public forwarding keep their existing behavior.

The publisher checks the current approved host mapping, probes that the origin
denies an anonymous request and accepts the matching credential, and checks
that the bucket has no managed public URL or custom domains. A failed check
blocks both publishing and rollback before either can write content.

### Smoke scenarios (pure parts in `pigeon_core::site_auth`)

- `sites_auth_off_by_default`
- `sites_auth_gates_every_path`
- `sites_auth_session_expires`
- `sites_auth_session_is_host_scoped`

## B. pigeon.dev on pigeon-sites

### Pages (pigeon-sites, host = `ZONE`)

| Path | What |
| --- | --- |
| `/` | Home: dot grid (the design system's background, as in the iOS app), the logo top left, the tab "Architecture", and a "Console" button top right that links to https://platform.pigeon.dev. Otherwise blank |
| `/architecture` | The same chrome around core's public drawing |
| `/architecture.json` | Core's public model, with `commit` at the top level, as deploy.sh reads it |
| `/logo.png` | The logo, from the iOS app icon |

Anything else is 404. Only `GET` and `HEAD` are served. The page shell is pure, in
`pigeon_core::home`, so the smoke suite renders it.

### The data: core's public view

- `architecture_describe {"public": true}` answers `{commit, digest, model, drawing}`.
  - `model` is `architecture::public(&model)`, a projection that keeps what the drawing needs:
    Workers, bindings, classes, secret *names*, queues, crons, hostnames on the zone, links,
    externals by name and kind.
  - It drops anything that could identify an account or a person: access token names, mirror
    and repository paths, externals' details, permissions, and any future instance layer
    (phase 8).
  - `drawing` is `render(&public)`.
- Without `public`, `architecture_describe` answers as today.
- Core's own apex (`site.rs`) also serves the public view from now on. The whole model stays
  available to MCP and the console only.
- pigeon-sites asks core as itself and keeps the answer for 30 seconds per isolate. That is
  short, so deploy.sh's two-minute check sees a core deploy's commit. If core fails, `/` still
  answers and `/architecture` answers 503.

### The diagram (architecture.md phase 6)

Layers, top to bottom:

1. Clients.
2. Hostnames: each with the Worker that serves it, public or behind Access.
3. **Workers**: core on its own row. The five capability Workers below it, each with its lane
   and deploy job, handlers, tool count, the Durable Object classes and containers it hosts,
   and its secret names.
4. **Binding arrows**, labelled with the binding name: `SITES`, `MODELS`, `CI`, `AI_AGENTS` down
   from core, `PLATFORM` back up. Durable Objects used across Workers are dashed and labelled
   with the class.
5. Account resources.
6. **External services** outside Cloudflare, one card per `externals` entry.

Phases 7 and 8 of architecture.md (live per-Worker facts, the instances layer) are not part of
this. The instances layer is never on the public page.

### Cut-over with no downtime

1. Land B1 (core): the public view, the phase-6 drawing, and the new `site_publish`:
   - It takes `{"worker": "pigeon-platform" | "pigeon-sites"}`.
   - Before it moves anything, it fetches `/`, `/architecture` and `/architecture.json` from
     the target over its binding, with host `pigeon.dev`. It refuses unless each answers 200
     and the JSON names a commit.
   - The apex is still core's. Core's deploy check passes as before.
2. Land B2 (pigeon-sites): the apex pages. pigeon-sites deploys. Nothing routes to them yet.
3. **The move**: `site_publish {"worker": "pigeon-sites"}`. One Custom Domain update: the DNS
   record and certificate stay, only the Worker behind them changes. Core keeps its own apex
   handler, so `site_publish {"worker": "pigeon-platform"}` moves it back.
   - I cannot call pigeon's MCP tools from this run. This one call is for the person, or the
     parent as their delegate, in the morning.
4. Land B3 (core, after the move): `settings::site()` names `pigeon-sites`, so the model and
   drawing show pigeon.dev on pigeon-sites. Core's deploy check then reads `architecture.json`
   through pigeon-sites, which is why its cache is short.

## Branches

| Branch | What | Base |
| --- | --- | --- |
| B1 (executor) | Core: phase-6 drawing, `architecture::public`, `architecture_describe {public}`, `site.rs` public view and `site_publish {worker}` with its pre-flight; scenarios `core_architecture_draws_every_worker_in_the_map` (public view, no identifiers) and `site_on_the_apex` updated | main |
| B2 (executor) | pigeon-sites: apex pages, `pigeon_core::home`, logo; scenario `sites_apex_pages` | main |
| A (executor) | `pigeon_core::site_auth`, core `site_passkey`, users `site` arg, record `auth`/`allow`, pigeon-sites gate; the four `sites_auth_*` scenarios | main |
| B3 (me) | `settings::site()` to `pigeon-sites`, after the move | main |

## Log

- 2026-11-23: plan written from main at `dc5814e`.
- 2026-11-23: B2 landed (`Z1YCPDSwx_kjMf2P`): pigeon-sites serves the apex pages; not routed yet.
- 2026-11-23: A built (executor `LHt0zblBkApy`): `pigeon_core::site_auth`, core `site_passkey`, `users_invite`/`users_enrol` `site`, record `auth`/`allow`, the pigeon-sites gate, the four `sites_auth_*` scenarios. Off by default.
- 2026-11-23: B1 built (executor `ZjHyQmvJEIUQ`, `claude/ai-agent-ZjHyQmvJEIUQ` at `fb6c57b`,
  CI green): `architecture::public`, the phase-6 drawing, `site_publish {worker}` with its
  pre-flight. Not landed.
- 2026-11-23: landing blocked for every branch. The Lander's merge-base fetch asks for
  `tree:0`, but Artifacts does not advertise `filter`, so the history pack carries every blob.
  Main's history within 500 commits is now about 47 MiB raw. `pack::objects` caps the inflated
  total at 48 MiB, counting deltas too. A pack of main's last 500 commits fails locally with the
  same error; 200 commits pass. A fix, a history parse that keeps only commits, is on an executor
  branch for the person. It cannot land through the Lander it fixes.

### Morning, in order

1. Unblock the Lander: deploy the history-pack fix outside the Lander (the person's call).
2. Land A (`claude/ai-agent-qzDCSNDiYgfW`), then B1 (`claude/ai-agent-ZjHyQmvJEIUQ`), one at a
   time.
3. The move: `site_publish {"worker": "pigeon-sites"}`. It checks pigeon-sites' three pages
   first and refuses if any fails. To move back: `site_publish {"worker": "pigeon-platform"}`.
4. B3: `settings::site()` should show pigeon.dev on `pigeon-sites` in the footprint. Add a
   `serving` field to `Site` for the footprint; `worker` stays this Worker, the rollback target
   with no pre-flight.
5. Deploy compatible core and Sites components, then use a separately approved
   instance plan for a fresh gated fixture. Check the direct origin anonymously
   before publishing synthetic content. See the protected preview rollout below.

### Approved site access bindings

A gated site's access binding includes its instance ID, site name and hostname.
Core checks the approved passkey setting and applied hostname before it starts
or finishes a ceremony. A signed-in session uses a key derived for that binding,
and the serving Worker checks the host allowlist on each request. Host and
allowlist changes remain subject to the existing host map cache lifetime.

Site visitor identities and site enrollment links record the binding at issue.
A standard user can still enroll for a site when an enrollment link explicitly
names that site. Existing standard platform enrollment links are unaffected.
Existing host-only site cookies require signing in again. Legacy site visitor
identities with no recorded binding require a new site invitation, and older
site enrollment links must be reissued. Gated host serving requires both the
platform and Sites Workers to support the new binding and protected upstream.

In `/app`, Site Access follows the workspace instance selector and shows only
applied passkey-protected hostnames belonging to that instance. Listing one
hostname reads visitor names and enrollment status for its exact current
binding. Creating an enrollment link requires a console session and a passkey
assertion for the same signed-in person, binding and visitor name. The console
checks the applied binding and visitor allowlist again before issuing the link.
New site visitors can be invited when the allowlist contains `site`. The
15-minute single-use enrollment link opens on the selected hostname. Platform
People continues to manage platform identities and their passkeys.

A subdomain-only publishing target uses its approved label and the Sites Worker's
platform zone for the canonical URL. A direct domain or zone hostname keeps
precedence when present. Publishing and rollback use the same URL in release
provenance. Choosing a canonical URL does not provision or authenticate the
linked Worker's origin.


### Protected preview rollout

1. Ship the core and Sites code first. Existing public instances need no
   plan or release change. Keep gated fixture creation behind the normal
   instance record and apply approvals.
2. Choose a fresh linked instance, Worker, R2 bucket and passkey subdomain.
   Keep the bucket dedicated and disable public R2 access, including
   independent bucket routes or publishing integrations controlled outside
   Pigeon. The plan refuses to reuse an existing public Worker or bucket
   as a protected origin, or to convert a protected origin to public.
3. Apply the approved plan. Before writing a release, request the direct
   origin and public hostname anonymously. Confirm the origin refuses the
   request and the platform hostname presents the sign-in flow.
4. Enroll the fixture visitor and publish synthetic release A, publish B,
   and roll back to A. Check that anonymous requests still cannot read
   either URL after the authorized reads. Verify a previously public site
   continues to serve unauthenticated. Keep any protected fixture disabled
   if the provider response, direct-origin check or rollback fails.
5. If the shared caller key rotates, provision a protected origin using an approved
   site plan with fresh resources before resuming gated publication.
   The credential mismatch fails closed. During a rollback of platform
   code, keep the protected Worker and bucket in place.

The worker and bucket privacy probes cover provider-managed direct access.
The linked account owner still controls its resources and can create other
routes or Workers that serve the same bucket independently.

# One Worker: pigeon.dev served by pigeon-platform

Status: deployed (phases 1 to 4). The Access end state was agreed on 2026-11-21: `pigeon.dev` and `platform.pigeon.dev` both stay public at the edge.

pigeon.dev used to be served by a Worker of its own, `pigeon`. The `site` capability rendered
the architecture page, uploaded it as that Worker's module with `pigeon_core::workers::upload`,
and attached the apex to it. The page only changed when someone called `site_publish` again.

The goal: one Worker, `pigeon-platform`, serves `platform.pigeon.dev`, `proxy.pigeon.dev` and
`pigeon.dev`. It picks what to serve by the request's host. The old `pigeon` Worker is retired
once the apex is live on the platform.

## What was true before (checked 2026-11-21)

- `platform.pigeon.dev` is **not** behind Access. `PublicHost` (`setup_platform`) retired its
  Access applications. Every path checks its callers itself: `/mcp` and `/oauth` with OAuth,
  the console, `/auth`, `/secrets` and `/tokens` with passkeys. `/health` is open. Live, `GET /`
  answers 200 with no Access redirect. "Protected" therefore means each path's own check, and
  no Access application is needed for this change, either new or removed.
- `pigeon.dev` (the apex) is public. It is a Workers Custom Domain on Worker `pigeon`, serving
  `GET /` and `GET /architecture.json`. Its old Access setup (application `pigeon`, service token
  `pigeon-site`, secrets `SITE_ACCESS_CLIENT_ID` and `SITE_ACCESS_CLIENT_SECRET`) is retired on
  every `site_publish`, each part skipped when already gone.
- `wrangler.toml` declares no `routes`. All three hostnames are Custom Domains attached through
  the API by setup tools (`pigeon_core::custom_domain::attach`, with `override_existing_origin`).
  `wrangler deploy` keeps them as they are; the platform and proxy hosts have survived every
  deploy. Wrangler's route and domain calls are not in the `/cf/` allowlist, so **routes stay
  out of wrangler.toml**. Declaring them there would make every landing's deploy fail.

## Design

### Routing by host

`src/platform/src/lib.rs`'s `fetch` asks the host first:

| Host | Serves |
| --- | --- |
| `proxy.pigeon.dev` | `/git/`, `/cf/`, `/health`; 404 otherwise (unchanged) |
| `pigeon.dev` (the zone apex) | `GET`/`HEAD` of `/` and `/architecture.json`, from the `site` capability; 404 for every other path and method, including `/mcp`, `/auth/`, `/oauth/`, `/.well-known/`, `/secrets`, `/tokens`, `/health`, `/git/` and `/cf/` |
| anything else (`platform.pigeon.dev`) | as today: the console at `/`, capability routes, `/health`, `/mcp` |

The apex is checked before any capability route. The console, OAuth and MCP are therefore
unreachable on the public host, and nothing about the platform host changes.

The rule is two pure functions, `site::is_apex(host, apex)` and `site::page(method, path)`,
checked by the smoke suite's `site_on_the_apex`.

### Rendering

The site's pages are rendered at request time from the sources compiled into the Worker
(`SOURCES`), the same way `site_publish` rendered them, and kept for the isolate's lifetime.
Each deploy therefore serves its own architecture page, with nothing to publish. Responses keep
the old Worker's headers (CSP, `nosniff`, `no-referrer`) and add `X-Served-By: pigeon-platform`,
so the cutover can be checked from outside.

### Tools

- `site_publish` changes meaning: it attaches `pigeon.dev` to `pigeon-platform`. The attach is
  one `PUT /workers/domains` with `override_existing_origin`, which moves the domain between
  Workers without removing its DNS record or certificate. It no longer uploads anything, and it
  changes no Access setup. The old `site_publish` deleted the Access app on exactly
  `pigeon.dev`, the reusable policy `pigeon` and the service token `pigeon-site` on every call.
  That would undo any restore of Access, so it is gone. If `pigeon-site` and its secrets
  (`SITE_ACCESS_CLIENT_ID`, `SITE_ACCESS_CLIENT_SECRET`) are still unused leftovers, the person
  removes them in the dashboard.
- `site_retire` (new, temporary): first checks with `GET /workers/domains?hostname=pigeon.dev`
  that the apex's custom domain names `pigeon-platform`, and refuses otherwise. Only then does it
  delete Worker `pigeon` (`DELETE /workers/scripts/pigeon?force=true`; skipped if already gone).
  Because of that check, it cannot take the apex dark.

### Footprint

`Site::footprint` declares the hostname `pigeon.dev` served by `pigeon-platform`, `by: site`,
public, and no Worker of its own. The `pigeon` `WorkerFootprint` goes. The architecture page then
shows one Worker with three hostnames. Its `routes` for pigeon-platform are read from `lib.rs`,
so the apex pages need no separate Worker entry. Coordinated with the architecture planner
(`session_015XFA1mC8Le3vJWk6HpLW2U`), who owns regenerating the page per deploy. With this
design, regeneration needs no publish step.

## Phases

The order keeps pigeon.dev serving throughout. The old Worker serves until the domain moves in
one API call, and it is deleted only after the new Worker is seen serving.

1. **Code (landing 1, `claude/one-worker`).** Host routing in `lib.rs`, rendering and the
   new `site_publish` and `site_retire` in `site.rs`, `custom_domain::serving` and
   `workers::delete` in `pigeon_core`, the footprint, and the wrangler.toml comment. Deploying it changes nothing live: the apex
   still points at `pigeon`.
   Check: `platform.pigeon.dev/health` is `ok` and `/` is the console; `pigeon.dev/` is
   unchanged (no `X-Served-By`).
2. **Cutover (a tool call, no landing).** `site_publish`.
   Check: `pigeon.dev/` and `/architecture.json` answer 200 with `X-Served-By: pigeon-platform`;
   `pigeon.dev/mcp`, `/auth/`, `/health` and `/git/x` answer 404; `platform.pigeon.dev` is
   unchanged. If anything is wrong, roll back by moving the domain back: `custom_domain::attach`
   to `pigeon` (the old Worker still exists). A person can do the same in the dashboard: Workers
   → `pigeon` → Settings → Domains & Routes → add `pigeon.dev`.
3. **Retire (a tool call, no landing).** `site_retire`.
   Check: the old Worker is gone (the tool's answer), and `pigeon.dev` still serves as in phase 2.
4. **Clean-up (landing 2).** Remove `site_retire`, `workers::upload` and
   `workers::delete` if nothing else uses them, and the `pigeon` names from `settings.rs`. Mark
   this plan deployed.
   Check: as in phase 2, and the architecture page lists one Worker.

## For the person

- Confirm the intended Access end state. This plan assumed both `pigeon.dev` and
  `platform.pigeon.dev` stay public at the edge, which matched the code (`PublicHost`) and
  the live state before 2026-11-21. If Access apps are restored on `pigeon.dev` or the platform
  host, routing by host still works, but the apex's two pages must stay outside any Access app
  for the site to be public.
- `setup_platform` (`PublicHost`'s `Retire`) still deletes, on every call, the Access apps on
  `platform.pigeon.dev`, `/auth`, `/oauth`, `/.well-known` and `/mcp`, among others. That is
  outside this plan and was reported to the parent session.
- Phase 2 needs no dashboard work: the Custom Domain move keeps DNS. The dashboard is only the
  rollback path given there.

## Log

- **Phase 1** landed as `8b20256` and `4d55d54` (after the Access-safety landings `9490e8a` and
  `0023414`). It deployed with no live change.
- **Departure:** the architecture page's `describe` never finished on the deployed code
  (exponential `depth_of`). Cutting over before that was fixed would have served 503 on the
  apex, so the cutover waited for the architecture plan's landing `0c9db60`. `site::render`
  now reads that plan's shared memo, `architecture::built`.
- **Phase 2:** the parent session ran `site_publish` at 20:25 UTC on the person's go-ahead.
  Checked live: `/` and `/architecture.json` answer 200 with `X-Served-By: pigeon-platform` and
  name commit `0c9db60`. On the apex, `/mcp`, `/auth/`, `/oauth/token`, `/.well-known/…`,
  `/health`, `/git/x`, `/cf/client/v4`, `/settings`, `/tokens`, `POST /` and `POST /mcp` answer
  404. `platform.pigeon.dev` and `proxy.pigeon.dev` are unchanged.
- **Phase 3:** the parent session ran `site_retire` (this planner's session was not permitted
  to call it). It answered `{"deleted":true,"old_worker":"pigeon","serving":"pigeon-platform"}`,
  and the apex served throughout.
- **Phase 4:** this clean-up.
- The smoke-suite check `site_on_the_apex` waited for the person's rule `smoke-suite-grows`
  (landed as `5864204`); before that, the policy answered `add_test` with "not in policy". It
  lands with phase 4.
- Executors: two `claude-sonnet-5-5` jobs (the core helpers and the site rewrite, both used after
  review) and one `claude-haiku-5-5` job (the smoke check; cut off at its token limit and
  rewritten by hand). About 7.4k tokens in and 8.8k out in all.

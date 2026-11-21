# The console

Superseded in part by [console-hardening.md](console-hardening.md): the tabs are now Approvals,
Architecture, Access and Cost.

Status: all three phases built and handed back together, at the person's request, awaiting
deploy. Departures from the plan: the three passkey-bound writes live in `console/writes.rs`
(pure: names, targets, purposes) with their handlers in `console.rs`, written by this session;
the writes' routes are `POST /console/write/{passkey-remove|app-disconnect|invite}/{begin|finish}`
with `{target}`; the Architecture tab and the CI section answer `{html}`, which the shell places
as it is.

Design: [pigeon Console mockup](https://claude.ai/artifact/9HAYNKWic1VgqAUBDZemtg), agreed with
the person on 2026-11-21. This plan turns it into steps. The mockup's numbers and people are
examples; everything the console shows comes from the capabilities below.

## Decisions

- **One page, four tabs, on paths:** Overview `/`, Architecture `/architecture`, Gateways
  `/gateways`, Settings `/settings`. Tabs sit in the header row between the pigeon icon and the
  session time and profile picture. Sections are anchors: `/gateways#models`,
  `/settings#keys`, `/settings#users`.
- **Sign in once:** one passkey sign-in sets a session cookie, `__Host-pigeon_session`:
  `HttpOnly; Secure; SameSite=Strict; Path=/`, 30 minutes idle and 8 hours at most, kept only
  as a hash in the Directory, removed on sign-out. Only people (standard users) get one. Every
  write that already takes its own passkey keeps it: a secret's value, issuing and revoking a
  proxy credential; new writes here (removing a passkey, disconnecting an app, inviting a
  person) take one too. Before sign-in the page shows only the icon, one line and the button.
- **CI is left out for now:** the CI gateway records no cost. The Gateways tab has a CI section
  that says so; Total cost is the sum of what gateways report, which today is model cost alone.
- **Apps, not connectors:** in Users, the OAuth clients and service users that act for a person
  are "apps"; "connector" stays the CI gateway's word.
- **Profile pictures on the user:** the users table gains an `avatar` column (a small image as a
  `data:` URL, at most 64 KiB, resized to 128 px in the browser). A person sets their own from
  the account card; until then the console shows their initials.
- **Old links keep working:** `GET /secrets` and `GET /tokens` redirect (302) to `/settings`
  and `/settings#credentials`; a browser keeps the fragment, so `/secrets#NAME` lands on that
  secret. `secrets_request` hands out `/settings#NAME` from now on. `/auth/*`, `/oauth/*`,
  `/.well-known/*`, `/mcp`, `/health`, `/ci/*` and the proxies are untouched.
- **The look:** the design system in `src/core/src/design.rs`, plus a console stylesheet for the
  header, tabs, stat cards, pills and the chart. Model colours (validated for colour-blind
  separation and contrast on both themes' card surface): dark `#D46E9B #6094DA #D47A2A`, light
  `#9A3F68 #2C5E9E #A9520F`, for Opus, Sonnet, Haiku, in that fixed order. Icons get a
  1.33 px border of white at 45%.

## Who does what

- **Planning session (this one, branch `claude/console`):** writes the plan, briefs the agents,
  applies their output, makes the small wiring edits to existing files itself, builds, tests and
  reviews every line, rebases on `origin/claude/gracious-ride-526fue`, pushes its own branch and
  hands each phase back. It never deploys and never changes a cloud environment, a secret or a
  Cloudflare resource.
- **Agents:** Claude Platform models through the `models_run` tool: one request, one answer, no
  tools, about 3,000 output tokens. Each brief carries the goal, the exact interface, the
  surrounding code it needs, the code style (the module-doc and comment voice of `proxy.rs`;
  plain browser JavaScript in an IIFE like `secrets.js`), and asks for one whole new file. No
  brief carries a credential or a person's data. Tasks are tagged `console-p<phase>-<step>`.
  `claude-opus-5-5` for sessions, cookies and the passkey-bound writes; `claude-sonnet-5-5` for
  rendering and routine pieces. Three failed rounds on one step stop the step and are reported.
- **Parent session (`session_015XFA1jgveJDEE8NJjTaHKg`):** merges, deploys, runs the live checks.
- **The person:** nothing account-side; the pigeon icon is theirs to supply (committed as
  `src/capabilities/src/console/icon.png`).

## Shape

```
src/capabilities/src/
  console.rs              the capability: routes, the page shell, sign-in, the JSON API
  console/session.rs      pure: cookie parse/build, idle and absolute expiry, Origin check
  console/sections.rs     pure: the tab and section registry, ordering, the overview totals
  console/page.rs         pure: the page's HTML shell and its CSP
  console/console.css     header, tabs, stat cards, pills, chart, sign-in panel
  console/app.js          shell: sign-in, header, tabs, routing, account card, section loader
  console/chart.js        the stacked daily bar chart, its tooltip, keyboard and table view
  console/overview.js     the Overview: needs-you alert and the cost stat cards
  console/keys.js         Settings › Keys: secrets and proxy credentials
  console/users.js        Settings › Users: people, passkeys, apps, invite, avatar
  console/icon.png        the pigeon icon, 144 px
  models/daily.rs         pure: gateway logs → cost per UTC day per model family
lib.rs                    Capability::sections(), Capability::section_data()
users.rs                  migrations 14–15, session.* and user.avatar_set operations
passkeys/passkey.js       say() takes the status element to write to
secrets.rs, proxy.rs      sections, redirects, secrets_request's new link
models.rs, architecture.rs, oauth.rs   their sections
src/platform/src/lib.rs   GET "/" to the console; settings.rs registers it
```

## Interfaces

### Sections (`lib.rs`, `console/sections.rs`)

```rust
/// A part of a console tab a capability fills.
pub struct Section {
    pub tab: Tab,              // Overview | Architecture | Gateways | Settings
    pub id: &'static str,      // the anchor: "models", "keys", "users"
    pub title: &'static str,
    pub description: &'static str,
    pub order: u16,            // within the tab
    /// Whether it adds to the overview's total cost, and under what name.
    pub cost: Option<&'static str>,
}
trait Capability {
    fn sections(&self) -> Vec<Section> { Vec::new() }
    /// The section's data, for `person`, as JSON; `query` holds its parameters (`days`).
    fn section_data<'a>(&'a self, cx: &'a Context<'a>, id: &'a str, person: &'a User,
                        query: &'a Value) -> Call<'a> { … Err(not found) }
}
```

A section with `cost` answers `{"cost_7d": f64, …}`; the overview sums them. Several
capabilities may fill one section id (Keys holds `secrets` and `proxy`): their data arrive as
`{capability: data}`.

### Sessions (`console/session.rs`, pure, and the Directory)

- `cookie(token, max_age_s) -> String`, `clear() -> String`, `token_from(cookie_header) -> Option<&str>`.
- `fresh(created_at, last_seen_at, now) -> bool`: false past 30 min idle or 8 h since creation.
- `same_origin(origin_header, host) -> bool`: a POST must carry `Origin: https://{host}`.
- Directory migrations: 14 `ALTER TABLE users ADD COLUMN avatar TEXT`; 15 `CREATE TABLE sessions
  (hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, created_at INTEGER NOT NULL, last_seen_at
  INTEGER NOT NULL)`. Operations: `session.create {hash, user_id}`, `session.get {hash}` (also
  deletes it when stale and moves `last_seen_at` forward), `session.remove {hash}`,
  `session.remove_user {user_id}`, `user.avatar_set {id, avatar}`. `users.list` and `users.get`
  leave `avatar` out; `user.avatar {id}` answers it alone.

### HTTP routes (`console.rs`)

| Route | |
|---|---|
| `GET /`, `/architecture`, `/gateways`, `/settings` | the page shell; the script picks the tab |
| `GET /console/{app,chart,overview,keys,users}.js`, `/console/console.css`, `/console/icon.png` | assets, `no-store`, `nosniff` |
| `POST /console/sign-in/begin`, `/finish` | passkey, purpose `console:sign-in`; a person only; sets the cookie |
| `POST /console/sign-out` | removes the session, clears the cookie |
| `GET /console/api/me` | name, initials, avatar, passkey count, apps count, session expiry |
| `GET /console/api/tabs` | tabs and sections the hosted capabilities declare, in order |
| `GET /console/api/section/{id}?days=7` | that section's data |
| `GET /console/api/overview` | missing secrets, cost per cost section, total |
| `POST /console/api/avatar` | `{avatar: data URL}`; the session's own user only |

Every `/console/api/*` route needs a fresh session and answers `401` without one; every POST
also needs the Origin check and `Content-Type: application/json`. The page's CSP:
`default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline';
img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`.

### Writes the console calls

Kept as they are: `/secrets/put/*`, `/tokens/issue/*`, `/tokens/revoke/*`. New, each a
passkey ceremony bound to its target, on the session's person: `POST /console/passkey/remove/*`
(purpose `passkey:remove:{id}`), `/console/app/disconnect/*` (`app:disconnect:{service_user_id}`,
revokes its tokens and removes the link), `/console/invite/*` (`invite:{hash(name)}`, answers
the single-use enrolment link). `/secrets/view/*` and `/tokens/view/*` stayed for one release
and are now gone, with the old pages' scripts `secrets.js` and `tokens.js`.

## Phases

### Phase 1: sessions and the shell

| Step | File | Model | Brief |
|---|---|---|---|
| p1-1 | `console/session.rs` | opus | cookie build/parse/clear, `fresh`, `same_origin`, token = 32 random bytes base64url, stored as its SHA-256; unit tests for each, including a cookie header with other cookies, a missing `__Host-` prefix, idle and absolute expiry edges, `Origin: null` and a port. |
| p1-2 | `console/sections.rs` | sonnet | `Tab`, `Section`, ordering of tabs and sections, grouping several capabilities under one id, overview totals; tests. |
| p1-3 | `console/page.rs` | sonnet | the HTML shell via `Page::render` (no inline script), `CSP`, `html(page)` with the headers of `passkeys::html` plus the CSP above; tests that the shell holds no inline script and links every asset. |
| p1-4 | `console/console.css` | sonnet | the mockup's header, tabs, stat cards, pills, chart, sign-in panel and account card in the design system's tokens, with the series colours above; phone layout at 640 px. |
| p1-5 | `console/app.js` | opus | sign-in panel, header, tabs on paths with `pushState`, account card (sign out, set picture), section loader with a registry other scripts fill (`window.pigeonConsole.section(id, render)`), anchors, 401 → sign-in panel. |
| p1-6 | `passkeys/passkey.js` | sonnet | `say(text, bad, el = status)`; nothing else changes. |
| — | `lib.rs`, `users.rs`, `console.rs`, `settings.rs`, platform `lib.rs` | this session | the trait methods, migrations and operations, routes, registration, `GET /`. |

Done when: the native tests and the wasm build pass; Playwright against the shell with the API
stubbed signs in, switches tabs, keeps the path, opens the account card and signs out, at
1000 px and 390 px, with no console errors.

### Phase 2: Overview and Gateways

| Step | File | Model | Brief |
|---|---|---|---|
| p2-1 | `models/daily.rs` | sonnet | logs → `[{day, family, cost}]` for the last `days` UTC days, families Opus/Sonnet/Haiku from the model name, others as "Other"; zero-filled days; tests, including the truncated flag. |
| p2-2 | `console/chart.js` | opus | the stacked bars: fixed order from the baseline, 2 px gaps, rounded tops, one hit column per day, tooltip with every family and the total, ←/→/Esc, the table view, the 7/30 toggle and the pills with each family's cost over the range. |
| p2-3 | `console/overview.js` | sonnet | the needs-you alert and the stat cards (Total cost, then one per cost section) linking to their sections. |
| — | `models.rs`, `ci.rs`, `architecture.rs` | this session | the Models section and its data; the CI section's "no cost yet" text; the Architecture tab from `render(model)`. |

Done when: as phase 1, plus the chart's numbers match `models_usage` over the same window.

### Phase 3: Settings

| Step | File | Model | Brief |
|---|---|---|---|
| p3-1 | `console/keys.js` | sonnet | secrets (set/replace with the bound passkey, as `secrets.js`) and proxy credentials (issue shows the value once; revoke), from the Keys section's data; scrolls to `#NAME`. |
| p3-2 | `console/users.js` | opus | each person with passkeys (remove one) and apps (disconnect), invite, the picture upload (resize to 128 px on a canvas, refuse above 64 KiB); each write a bound passkey ceremony. |
| p3-3 | `console/writes.rs` | opus | the three new passkey-bound writes and their purposes; tests that a purpose binds its target. |
| — | `secrets.rs`, `proxy.rs`, `users.rs`, `oauth.rs` | this session | sections and data; the redirects; `secrets_request`'s link. |

Done when: as phase 1, plus the old `/secrets#NAME` and `/tokens` links land in place.

## Verification by this session, every step

`cargo fmt --check`; `cargo clippy` (wasm); `cargo test --target x86_64-unknown-linux-gnu -p
pigeon-core -p pigeon-capabilities`; `cargo build -p pigeon-platform`; `node --check` on every
script; Playwright with the API stubbed at both widths and both themes. Every line an agent
writes is read before it is committed.

## What can only be checked live

The cookie's flags as the browser stores them; that a second browser without the cookie gets
the sign-in panel; idle expiry; a passkey sign-in and a bound write end to end; the redirects
from `/secrets#NAME` and `/tokens`; the Models chart against the gateway's own logs; the CSP
header on every console response.

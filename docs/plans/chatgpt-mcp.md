# ChatGPT as an MCP client of pigeon

Status: **phase 17a implemented** (2026-11-25); 17b open. Planner run `vnr3cWzWC3aX`. Part of [model-providers.md](model-providers.md), Part 4, phase 17 (E).

## Goal

The person tried several times to connect ChatGPT to the Pigeon MCP and it failed. The exact
error is not known. The portal `mcp.pigeon.dev` already allows the redirect
`https://chatgpt.com/connector_platform_oauth_redirect` ([mcp-portal.md](mcp-portal.md)). This
plan finds what in pigeon ChatGPT could reject, fixes what can be fixed in code, and gives the
person a tool to check it live.

## Suspects, in order

1. **Tool input schemas.** ChatGPT validates each tool's `inputSchema`, and one bad schema can
   fail the whole connector. Known bad: `ai_agents_status`'s `brief` and `report` have no
   `type`. The rules the conformance test (and the live tool) check on every tool core serves:
   - top-level `type` is `object`;
   - every property has a `type` (or `anyOf`/`oneOf` of typed schemas, inside properties only;
     never at the top level);
   - every `array` has `items`;
   - every `object` declares `properties` or sets `additionalProperties`;
   - the tool name matches `^[a-zA-Z0-9_-]{1,64}$`;
   - no `$ref`.
2. **Discovery documents** of each pigeon MCP entry point: `/.well-known/oauth-protected-resource`
   and `/.well-known/oauth-authorization-server`, read with `curl`. Record what ChatGPT needs and
   the document lacks (for example `registration_endpoint` for dynamic client registration,
   `code_challenge_methods_supported` with `S256`, `resource`, `scopes_supported`).
3. **Size.** The `tools/list` payload: bytes and tool count. Record them.

## Decisions

- The lint is one function in core, used by the smoke test and by a new read-only tool
  `mcp_conformance`, so the person can run the same check from any client. It lists each
  violation as `{tool, path, rule}` and the counts, and it changes nothing.
- Fix every violation in the tools' declared schemas (explicit types). No schema is dropped.
- Anything that needs the person (a dashboard setting, a passkey) is written under "Unknowns",
  not shipped.

## Phases

| Phase | What | Touches |
| --- | --- | --- |
| 17a | The lint, the smoke test over every served tool, fixes to every schema, the `mcp_conformance` tool | `src/core/src/mcp.rs`, the capabilities that declare tools, `tests/smoke/core.rs` |
| 17b | The discovery documents and payload size, read with `curl`, recorded in the Log | this file |

## Live findings (2026-11-24, planner run `S4EpbKvZpBLe`, read with `curl` from outside)

The two entry points, side by side:

| Check | `https://mcp.pigeon.dev/mcp` (Access portal) | `https://platform.pigeon.dev/mcp` (core) |
| --- | --- | --- |
| Protected-resource metadata | 200 at `/.well-known/oauth-protected-resource` and `.../mcp`; `resource` is the portal, `authorization_servers` is `https://pigeon-security.cloudflareaccess.com` | 200, same two paths; `authorization_servers` is `https://platform.pigeon.dev` |
| Authorization-server metadata | on the Access host: 200 with `registration_endpoint` (dynamic client registration), `code_challenge_methods_supported: [S256]`, grants `authorization_code` and `refresh_token`. On `mcp.pigeon.dev` itself: 200 at `/.well-known/oauth-authorization-server`, **404** at `/.well-known/oauth-authorization-server/mcp` and at `/.well-known/openid-configuration` | 200 at `/.well-known/oauth-authorization-server` and at `.../mcp`: `registration_endpoint`, `S256`, `token_endpoint_auth_methods_supported: [none]`; no openid-configuration (404) |
| Unauthenticated POST | 401 with `WWW-Authenticate: Bearer realm="OAuth", error="invalid_token", resource_metadata="https://mcp.pigeon.dev/.well-known/oauth-protected-resource/mcp"` | 401 with `WWW-Authenticate: Bearer resource_metadata="https://platform.pigeon.dev/.well-known/oauth-protected-resource"` |
| CORS preflight to `/mcp` from `https://chatgpt.com` | 200, `Allow-Origin: *`, `Allow-Methods: POST`; `Allow-Headers` is the list the browser asked for (`authorization, content-type, mcp-protocol-version`), and absent when the preflight asks for none. This is Cloudflare's edge answering, not pigeon | **was 405** ("takes POST only") with no CORS headers on the 401 either; the discovery, `/oauth/register` and `/oauth/token` paths do answer preflights. Fixed in `src/platform/src/lib.rs` (`mcp_cors`): `OPTIONS /mcp` gives 204, and every `/mcp` answer carries the CORS headers and `Expose-Headers: WWW-Authenticate`. Live after the next deploy |
| CORS on discovery and registration | `Allow-Origin` echoes `https://chatgpt.com`; registration preflight 204 | `Allow-Origin: *` |

The earlier note that the portal answered a preflight with `Allow-Methods: POST` only was
right, but `Allow-Headers` is present when the preflight asks for headers, so it is not the
failure. The platform's missing preflight answer on `/mcp` was a real gap and is fixed.

**Recommendation: connect ChatGPT to `https://platform.pigeon.dev/mcp`.** Its discovery is
complete at both well-known paths, it is pigeon's own code (we can read, test and fix it), it
needs no dashboard setting, and its OAuth ends in a pigeon-minted bearer token that `mcp_post` accepts.
The portal is the second choice.

What is Cloudflare's to configure and what is pigeon's:

- Cloudflare's (Zero Trust dashboard, the person): the portal's own metadata and CORS (the
  portal does not serve `/.well-known/oauth-authorization-server/mcp`, the path-suffixed form
  some clients try first), its Allow-Methods list, the Access policy that decides who may sign
  in, and the portal's allowed redirect URIs (`https://chatgpt.com/connector_platform_oauth_redirect`
  is already there, see [mcp-portal.md](mcp-portal.md)). Pigeon cannot change any of these from
  code.
- Pigeon's: everything on `platform.pigeon.dev` (discovery, registration, token, `/mcp`, CORS,
  tool schemas, the `mcp_conformance` tool).

## Unknowns

- The exact error ChatGPT showed. If it still fails, send: the message ChatGPT shows, the time,
  and the output of `mcp_conformance`.

## Log

- 2026-11-25: plan written.
- 2026-11-24: live discovery and CORS read with `curl` (section above). `OPTIONS /mcp` on the platform answered 405; it now answers 204 with CORS headers.
- 2026-11-25: phase 17a implemented. `pigeon_core::mcp::lint_tool` is the lint and
  `mcp::conformance` the report; the read-only tool `mcp_conformance` (in the architecture
  capability, listed beside `architecture_describe`) runs it over every tool the hosting Worker
  lists, after the approval annotation, and answers violations, counts, tool count and bytes.
  Measured offline (the smoke suite builds the tools; the live number is what
  `mcp_conformance` answers): core's own tools, 84, 31,795 bytes of `tools/list` JSON; the
  capability Workers add about 26,800 bytes for CI, models, AI agents and cloud (8 + 12 + 17 + 10
  tools) and 2 sites tools, so about 133 tools and about 60 KB in all.
  Violations found, all fixed by explicit types, no tool dropped, no behaviour changed:
  - no `type`: `brief` and `report` of `ai_agents_status` and `ai_agents_inbox`. They carry a
    boolean or a string (`brief`: true, false or "line"; `report`: "full", "short" or false), so
    each is now `anyOf` boolean and string;
  - `object` with no `properties` or `additionalProperties` (now `additionalProperties: true`):
    `apns_send.data`, `observability_monitors_plan.billing` and `.filters`, `call.args`,
    `instance_propose.record`, `site_passkey.assertion` and `.credential`, `ci_route.needs`,
    `models_run.messages[]` and `.raw`, `models_submit.messages[]` and `.request`,
    `ai_agents_start.budget`;
  - `array` with no `items`: `site_render.posts` and `.pages` (now arrays of string pairs).
  The smoke suite runs the lint over every tool it can build offline and over a bad schema.
  The two sites tools are not built there; they were read by hand.

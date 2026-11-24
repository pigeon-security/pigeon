# ChatGPT as an MCP client of pigeon

Status: **planned** (2026-11-25). Planner run `vnr3cWzWC3aX`. Part of [model-providers.md](model-providers.md), Part 4, phase 17 (E).

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

## Unknowns

- The exact error ChatGPT showed. If it still fails, send: the message ChatGPT shows, the time,
  and the output of `mcp_conformance`.

## Log

- 2026-11-25: plan written.

# External MCP access through a Cloudflare MCP server portal

Status: **go-ahead given; the retire fix is built (landing 1)**. Phase 0 is checks the person
makes in the Cloudflare dashboard; the later build phases wait on its answers.

Sources: Cloudflare's MCP portals page
(https://developers.cloudflare.com/cloudflare-one/access-controls/ai-controls/mcp-portals/), its
account limits (https://developers.cloudflare.com/cloudflare-one/account-limits/), and this
repository at `885014a`. Where the docs are silent, this plan says so and phase 0 finds out.

## What a portal is

A portal puts several MCP servers behind one Streamable HTTP endpoint:
`https://<subdomain>.<zone>/mcp`, on a zone in the account.

| Topic | What the docs say |
| --- | --- |
| Tool names | `{server_id}_{tool}`, such as `pigeon_land`. The server ID is set when the server is added, is at most 32 characters and may not contain `_`. A tool can be given an alias of at most 40 characters. Prompts are named the same way. Resources are fetched live from the servers. |
| Portal's own tools | `portal_list_servers`, `portal_toggle_servers` and `portal_toggle_single_server`. Context optimisation adds `portal_query_tools` and `portal_execute`. Code Mode adds `portal_codemode_search` and `portal_codemode_execute`. |
| Servers' auth | `oauth` (with dynamic client registration, or a client set up by hand), `bearer` (a token, or fixed headers), or `unauthenticated`. With **Require user auth** (`on_behalf`), each user goes through the server's own OAuth; without it, everyone uses the admin's credential. |
| Tool lists | Synced about every two hours with the admin's credential, or at once with "Sync capabilities" or `POST …/servers/{id}/sync`. |
| Who may connect | The portal has an Access application (`mcp_portal`), and each server has one (`mcp`). A server's Allow policy only decides whether the server *appears* in the portal: "Blocked users can still connect to the server … by using its direct URL." Emails, groups, country and device posture are enforced. Independent MFA, purpose justification and temporary authentication are not. |
| Per-tool control | Each tool and prompt has its own on/off switch. Off means it is not listed and cannot be called. An allowlist is possible: `default_disabled: true`, then enable tools one by one. The docs describe no per-user tool filtering. |
| Logs | The dashboard shows time, status, server, capability and duration. Tool arguments are not listed. Logpush (`mcp_portal_logs`) is Enterprise only. An analytics API gives counts of tool calls. Retention is not stated. |
| Clients | They get a 401 and follow OAuth to Access, which signs the person in with the account's identity providers. For claude.ai, add `https://claude.ai/api/mcp/auth_callback` under the portal's Advanced settings → Allowed redirect URIs. Headless clients send an Access service token (`CF-Access-Client-Id` and `-Secret`), which needs a Service Auth policy on the portal and on each server. Servers with Require user auth are left out of service-token sessions. Device authentication through the Cloudflare One Client is not supported. |
| Limits | 50 portals per account, 120 servers per portal, 5 custom domains per portal. Sessions end after 24 h inactive. A tool call to a server times out after **60 s**, not configurable. |
| Plan, price, beta | Not stated, apart from Logpush being Enterprise only. Phase 0 checks the account. |
| What reaches the server | The docs name only `X-Forwarded-User-Agent` (the MCP client), and say not to use it for auth. They do not say that the person's identity (an Access JWT, an email) is forwarded. |
| Caveats | An admin's OAuth token can expire without anyone being told. Code Mode does not work with servers that run Code Mode themselves. Through the API, the DNS record (a proxied CNAME to `gateway.agents.cloudflare.com`) is not created, and the portal answers 522 until it exists. Changing an alias makes users sign in again. |

## MCP today

### Servers and clients

| Server | Where | Client and auth | Identity pigeon sees |
| --- | --- | --- | --- |
| pigeon | `https://platform.pigeon.dev/mcp`: POST and JSON only; public, with no Access in front since `setup_platform` retired it | **claude.ai connector "Pigeon"** (server id `71fdea92-23ad-47d5-a156-53f536f8b55a`). It registered through pigeon's DCR as OAuth client `Claude` (`cJMDCifol-…`) and was allowed with Willow's passkey. Its tokens are `pgn_at_` (1 h) and `pgn_rt_` (30 d, rotated). Cloud sessions get it from the connector. | service user `Claude`, acting for Willow: the delegate |
| pigeon | the same | `.mcp.json` entry `pigeon-platform`. It is a duplicate that asks for sign-in, and another planner is removing it. | none (unauthorised) |
| pigeon | the same | OAuth client `probe` (`0JXRzgaVO32X…`, redirect `http://localhost:1/cb`), linked to nobody. It looks like an old test. | none |
| pigeon | the same | Long-lived `pgn_at_` tokens from `oauth_issue_token`, for agents with no browser. `oauth_clients` does not list them. | the issuing service user |
| pigeon | the same | Access service tokens, through `users::caller` (`Cf-Access-Jwt-Assertion`, `common_name` only). This path is left over from before the host was public: Access no longer signs requests to the host, so it answers only if something else adds a valid JWT. | a service user per token |
| claude-code-remote | Built into cloud sessions | The session's own credential | — (not pigeon's) |
| GitHub MCP | Built into cloud sessions, limited to `pigeon-security/pigeon` | The session's GitHub link | — (not pigeon's) |

Not MCP, and unchanged by this plan: `proxy.pigeon.dev` `/git/` and `/cf/` with the `pgn_px_` proxy credential, and CI job tokens. MCP refuses those tokens, and the proxies refuse MCP tokens (`proxy::reaches`).

### Tool names pre-approved in `.claude/settings.json`

`settings.rs` (`PIGEON`) writes each pigeon tool under two server names, `Pigeon` and
`71fdea92-23ad-47d5-a156-53f536f8b55a`:

- `policy_check`, `land`, `land_status` and `land_cancel` under each name;
- `mcp__claude-code-remote__{send_message,create_session,archive_session}`;
- `Bash(git push https://proxy.pigeon.dev/*)`.

A portal adds its own prefix. Through a claude.ai connector named, say, `Pigeon` that points at
the portal, `land` becomes `mcp__Pigeon__pigeon_land`, and the connector gets a new server UUID.
None of the names on the list would match, so every call would prompt again until the generator
writes the new names.

### Servers worth putting behind the portal

| Server | Behind the portal? | Why |
| --- | --- | --- |
| pigeon | **Yes** | It is the reason for the portal. |
| Cloudflare Docs (`docs.mcp.cloudflare.com`, unauthenticated) | Yes, later | It is read-only, needs no credential, and sessions already read these docs. |
| Cloudflare observability and AI Gateway MCP servers (OAuth, on behalf of the user) | Maybe, later | They read logs that pigeon's tools only summarise. Each is a separate decision for the person, because it gives agents Cloudflare account access outside the policy. |
| GitHub's remote MCP | Not for Claude | Cloud sessions already have a GitHub server, limited to this repository. It could be useful later for other providers' agents. |
| claude-code-remote | **No** | It is part of the session, not an HTTP server the person runs. |

## Target design

```
claude.ai connector "Pigeon"  ──OAuth (Access)──▶  mcp.pigeon.dev/mcp  (portal "pigeon")
  (cloud sessions inherit it)                          │  server id "pigeon", Require user auth,
                                                       │  OAuth/DCR to pigeon
later: other agents ──service token / OAuth──▶  ───────┤  server id "cfdocs", unauthenticated
                                                       ▼
                                   platform.pigeon.dev/mcp  (pigeon's own OAuth, unchanged)
```

### 1. pigeon behind the portal, keeping its own OAuth

The portal does not close the server's direct URL, and the docs never say it passes on who is
calling. pigeon therefore **keeps its OAuth** as the place that decides who is calling. It is
not simplified or removed. The portal's server entry for pigeon uses **OAuth with dynamic
client registration and Require user auth on**:

- The portal registers with pigeon once, as an OAuth client: a service user named by the portal's `client_name`.
- When a person first opens pigeon through the portal, they pass Access (the portal's policy) and then pigeon's own `/oauth/authorize` page. There they allow the portal with their passkey. That links the portal's service user to them, as `Claude` is linked to Willow today.
- After that, pigeon sees `pgn_at_` tokens that the portal holds for that person. `cx.caller` is *the portal client, acting for Willow*. `policy_check` answers as the delegate; the `as` string names the portal client instead of `Claude`. The AI Gateway tags `caller`, `for` and `task` are written as now.

Without Require user auth, every portal user would act as the admin who set the server up. That
would make per-caller identity meaningless, so it is rejected.

**What is lost, and how it comes back.** Every client behind the portal shares one pigeon
service user. pigeon could no longer tell a claude.ai session from another provider's agent.
Phase 2 adds the portal's `X-Forwarded-User-Agent` to `cx.caller_note` and to a new AI Gateway
tag, `client`. It is a label for usage and logs only, never used for auth, as the docs warn.

**A second person.** `link.set` keys a link by service user. If two people authorise the same
portal client, the second link replaces the first, and pigeon's links would then put the first
person's portal calls down to the second. Today Willow is the only standard user. Before a
second person uses the portal, phase 2 must tie links to the token instead (each token pair is
issued after one person's passkey). Until then, the portal's Access policy lets in Willow alone.

### 2. Tool names, the settings generator and `policy_check`

- **Prefix.** Server ID `pigeon`, with no aliases. Names become `pigeon_<tool>`, such as `pigeon_policy_check` and `pigeon_land`. Aliasing each tool back to its bare name would save the prefix, but needs 50-odd aliases to maintain, and each alias change makes users sign in again.
- **Generator.** In `examples/settings.rs`, `PIGEON` becomes a list of `(server, prefix)` pairs: today's two names with no prefix, plus the portal connector's name and its server UUID with `pigeon_`. The UUID is known only after the person adds the connector, in phase 1. Both sets are written during the migration, so rollback needs no landing. The old pair is dropped in phase 4. Either change rewrites `.claude/settings.json`, so **each lands with the person's passkey** (`policy-is-yours`). The policy file itself does not change: no rules are added or changed.
- **`policy_check`** is unchanged: same action kinds, same answers. Only the name it is reached by changes. The MCP instructions in `src/platform/src/lib.rs` name tools bare (`land {branch}`, `call {tool, args}`). The docs do not say whether a portal passes a server's `instructions` on. If it does not, `docs/plans/README.md` "For agents" carries the rules. Either way, phase 2 rewords the instructions to say that tools may be listed with a `pigeon_` prefix.
- **Code Mode and context optimisation: off.** `portal_codemode_execute` and `portal_execute` run any tool behind the portal, just as `call` does. Pre-approving either would go around the policy, and with them on, the per-tool allow list no longer applies. Set the portal's Code Mode policy to `off`. The docs show no portal setting that refuses `?optimize_context=search_and_execute`, so phase 0 checks whether a client can turn it on. If one can, the generator still never lists `portal_*`.

### 3. Per-tool switches and `call`

The portal's per-tool switches look like a second line of defence: for example, turning off
`deploy_token`, `source_token`, `oauth_issue_token`, `passkeys_remove` and `users_remove`.
**While `call` is on, they constrain nothing**, because `call` reaches every pigeon tool on the
server. There are two coherent choices:

1. **Keep `call` (recommended for phase 1).** Leave all tools on. The switches are only tidiness, and the policy plus the pre-approval list stay the control, as today.
2. **Turn `call` off at the portal (phase 3, optional).** Then the switches really hold, but a tool deployed after a session starts can be reached only after a sync. Phase 3 has a landing's deploy call `POST …/access/ai-controls/mcp/servers/pigeon/sync` with a scoped token minted from the parent token, as `cf_proxy` mints them, so new tools appear within the deploy. Removed tools stay hidden until the person turns them on in the dashboard (`default_disabled`). This is a stronger gate, decided by the person. It is not a policy rule.

### 4. Access, the platform host and `setup_platform`

- The portal's Access application (on `mcp.pigeon.dev`) has one Allow policy: emails = the person's address. Later, a Service Auth policy for named service tokens.
- **Risk found:** `PublicHost::retire` deletes any Access app whose domain is exactly `platform.pigeon.dev`, `…/auth`, `…/oauth`, `…/.well-known` or `…/mcp` (`access::Session::remove_app` matches on domain only). If the portal's `mcp` application for the pigeon server sits on `platform.pigeon.dev/mcp`, running `setup_platform` again would delete it. Fixed before any dashboard step (landing 1): `access::Session` now finds, updates, removes and reads the AUD of `self_hosted` applications only, the type pigeon makes, so a portal's `mcp` or `mcp_portal` application on the same domain is left alone.
- The direct `platform.pigeon.dev/mcp` stays open to pigeon's OAuth: it is the rollback path, and the only path for headless agents with an issued token. Narrowing it to the portal's client is a later decision for the person.
- The leftover Access-JWT path in `users::caller` stays for now. The docs give no sign that the portal signs requests to the server with an Access JWT. If phase 0 finds one, verifying its `email` claim against the person is a possible phase-3 alternative to the second OAuth hop.

### 5. Headless and other providers' agents

A service token reaches only servers without Require user auth. Those use the admin's credential,
which means everyone acts as the admin. For per-caller identity, a headless agent therefore keeps
calling pigeon **directly** with a token from `oauth_issue_token`, as its own service user linked
to the person. If the person later wants every agent to go through the portal, a second server
entry, `pigeon-agents` with `bearer` auth, could hold one issued token. Every headless agent would
then share that one service user, kept apart from people. That is phase 5, if wanted.

### 6. Logs and expiry

- The portal's dashboard logs record each call (server, capability, status). pigeon's own records stay the authority: `policy_decisions`, the proxy audit lines and AI Gateway tags.
- The admin OAuth token can expire without notice. pigeon issued it, so phase 2 adds the portal client's refresh-token expiry to `credentials_expiry` and the console's Overview. Phase 2 also turns `X-Forwarded-User-Agent` into the `client` tag.
- The 60 s cap on each tool call: `models_run` takes "about a minute at most". Through the portal, long requests use `models_submit`. `land` already returns at once.

## Phases

| Phase | What | Who | Lands with |
| --- | --- | --- | --- |
| 0 | Dashboard checks: plan availability; whether Access forwards an identity to the server; the domain and type of the server's Access app; whether `instructions` and `?optimize_context` pass through; the DCR `client_name` | Person (steps below), agent reads pigeon's logs and `oauth_clients` | — |
| 1 | Create the portal and the pigeon server; add a claude.ai connector to it, side by side with the current "Pigeon" connector; try it from one session | Person | — |
| 2 | Generator writes both name sets; instructions mention the prefix; `client` tag from `X-Forwarded-User-Agent`; portal client's expiry in `credentials_expiry`; smoke scenarios added to `tests/smoke.rs` only (generator output, retire filter) | Agent, through AI Gateway executors | `land`, with the person's **passkey** (settings.json changes) |
| 3 | Optional: `call` off at the portal, plus the deploy-time sync; per-tool switches for the dangerous tools | Person decides; agent builds | `land` (passkey if settings change) |
| 4 | Cutover: remove the old "Pigeon" connector, revoke its OAuth client `Claude`, drop the old names from the generator; revoke `probe` | Person (connector, revokes), agent (generator) | `land` with **passkey** |
| 5 | Optional: Cloudflare docs server, then other servers one by one; headless entry `pigeon-agents` | Person decides | dashboard only |

**Cutover** comes after the portal connector has worked in cloud sessions for a few days. The
checks: `users_whoami` through it answers *the portal client, acting for Willow*; `policy_check`
and `land` run without a prompt; `models_usage` shows the new caller and the `client` tag.

**Rollback**, at any point before phase 4: turn the old "Pigeon" connector back on (or keep
it), and sessions use the old names, which stay in the generator until phase 4. After phase 4:
add the connector for `https://platform.pigeon.dev/mcp` again (pigeon's DCR registers a new
client, allowed with the passkey), then land the generator with the old pair restored and the
new UUID. The portal can be deleted at any time without touching pigeon.

## The person's dashboard steps

Zero Trust (one.dash.cloudflare.com), the pigeon account:

1. **Access controls → MCP Portals → MCP servers → Add MCP server.**
   - Name `pigeon`; Server ID `pigeon`; URL `https://platform.pigeon.dev/mcp`.
   - Authentication: **OAuth, Automatic (dynamic client registration)**; **Require user auth: on**.
   - Access policy: Allow, Emails = your address.
   - Save. You are sent to pigeon's "Allow access" page: sign in with your passkey. This is the admin credential the portal syncs with.
   - Wait for status **Ready**.
2. **MCP Portals → Add portal.**
   - Name `pigeon`; custom domain zone `pigeon.dev`, subdomain `mcp`.
   - Add server `pigeon`.
   - Policy: Allow, Emails = your address.
   - **Code Mode: off.**
   - Advanced settings → Allowed redirect URIs: add `https://claude.ai/api/mcp/auth_callback`.
   - Check that DNS has a proxied record `mcp` → `gateway.agents.cloudflare.com`. Without it, the portal answers 522.
3. Under the portal's **Servers → pigeon → Tools authorized**, leave every tool on, `call` included (phase 1).
4. On **claude.ai → Settings → Connectors → Add custom connector**, name it (for example `Pigeon portal`) with URL `https://mcp.pigeon.dev/mcp`. Connect: Access signs you in, then pigeon asks for your passkey. Keep the old "Pigeon" connector.
5. Report back for phase 0:
   - The plan or limits the MCP portals page shows.
   - The **Access → Applications** entry the server created, with its type and domain.
   - Whether the portal's tool list in a session shows pigeon's instructions.
   - The connector's tool names as a session sees them (`mcp__<name>__pigeon_policy_check`, and the server UUID).
6. Phase 4 only: remove the old connector, then revoke `Claude` and `probe` from the console, or ask an agent to call `oauth_revoke`.

None of these steps are possible from a session: Zero Trust settings and claude.ai connectors
are outside what the proxy and the parent token are used for.

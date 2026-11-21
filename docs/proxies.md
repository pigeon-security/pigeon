# Proxies

This page is for operators of pigeon. It describes the two proxies on `proxy.pigeon.dev`, how to issue and use the proxy credential, and what the proxies allow.

## What and why

Cloud coding sessions used to reach the Artifacts git host, `api.cloudflare.com` and `github.com` directly. Tokens were pasted into commands, so they ended up in transcripts.

Now sessions reach git and the Cloudflare API only through pigeon's proxies on `proxy.pigeon.dev`. The proxies hold the real credentials and mint a short-lived one for each request. MCP stays on `platform.pigeon.dev`.

One Worker, `pigeon-platform`, serves both hosts. On `proxy.pigeon.dev` only `/git/`, `/cf/` and `/health` answer. On any other host, `/git/` and `/cf/` answer 404.

## The proxy credential

The credential is a bearer token with the prefix `pgn_px_`. It is shown once and kept only as a hash.

MCP refuses it, because MCP looks up only its own token kind. The proxies refuse MCP tokens.

### Issue

Open https://platform.pigeon.dev/tokens. The page is gated by a passkey.

- Seeing the list takes a passkey.
- Issuing takes a passkey approval bound to the label and the lifetime.
- Only a person can issue, not a service user.

The lifetime is 30 days by default and 90 days at most.

### Save

Save the token as the cloud environment's network secret:

| Field | Value |
| --- | --- |
| Host | `proxy.pigeon.dev` |
| Header | `Authorization` |
| Prefix | `Bearer` |

The session's network proxy adds the header to every request to that host. Commands and transcripts never see the token. Saving a network secret for a host also allows the host.

### Revoke

Revoke from the same page. Revoking takes a passkey approval bound to the credential's id, which is the first 12 characters of its hash. Only a person can revoke.

## Git

Use this remote:

```
https://proxy.pigeon.dev/git/pigeon-security/pigeon.git
```

Only these requests on that one repository are allowed:

- `GET info/refs?service=git-upload-pack`
- `GET info/refs?service=git-receive-pack`
- `POST git-upload-pack`
- `POST git-receive-pack`

Each request gets its own Artifacts git token: read for fetch, write for push. The token has a 15-minute ceiling and is revoked once the answer has been sent.

### Push rules

The proxy checks the push's ref updates before it mints any token.

- No ref may be deleted.
- `main` cannot be created. It can only move forward (fast-forward).
- Every other ref must be a branch under `refs/heads/claude/`.
- Ref names are checked strictly.
- Signed pushes are refused.

A push that moves `main` is read whole, up to 16 MiB. The proxy checks that the new commit descends from the current one. It uses the pushed commits and, if needed, the repository's own history.

If the push stores a needed commit as a delta against an object it does not carry, the push is refused. The advice is to push the commit to a `claude/` branch first, then move `main` to it.

Refusals show in `git push` output as rejected refs with the reason.

A missing or unknown credential gets 403, not 401, so git does not prompt for a password.

Session branches stay in Artifacts. The GitHub mirror copies only `main`.

### Removing a session branch

The proxy refuses every delete, so `claude/` branches pile up in Artifacts. A person removes one directly on Artifacts, not through the proxy:

1. Mint a write token with the MCP tool `source_token` (`access: "write"`). It answers the token and the repository's Artifacts remote.
2. Push the delete to that remote, passing the token as a header rather than in the URL, so it stays out of the shell history:

   ```
   git -c http.extraHeader="Authorization: Bearer $TOKEN" push <remote> --delete claude/route-check
   ```

3. Revoke the token with `source_revoke` and the id `source_token` gave.

This needs the Artifacts host in the environment's allowed hosts, so do it from a machine that has it, not from a cloud session once the host is removed.

### CI runners

A CI runner fetches with its job token (prefix `pgn_ci_`) instead of a proxy credential. A job token:

- reaches only `GET info/refs?service=git-upload-pack` and `POST git-upload-pack` of the repository its grant names;
- can never push, and never reaches `/cf/` or MCP;
- fetches only the one commit its grant names, until the job's deadline. The proxy reads the whole request first (64 KiB at most) and refuses any `want` of another object, a `want-ref`, and any protocol v2 command other than `fetch` and `ls-refs`;
- is refused if its request is compressed.

Its audit lines name the caller as `ci job <job> (ci)`.

### Reading a file at a commit

The platform reads a file at a commit, such as `.pigeon/ci.toml`, with `git_proxy::read_file`: it mints a read token, fetches that commit alone (one commit deep), reads the file from the commit's tree, and revokes the token. The pack reader resolves deltas within the pack.

## Cloudflare API

The base URL is:

```
https://proxy.pigeon.dev/cf/client/v4
```

It stands for `https://api.cloudflare.com/client/v4`.

The proxy passes on only the calls that `wrangler deploy` of `pigeon-platform` makes, recorded from wrangler 4.166.0, the version `src/platform/package.json` pins. Paths without a leading `/` are under `/accounts/{account}/workers/`, where the account is the platform's and `{s}` is `pigeon-platform`.

| Call | Permissions |
| --- | --- |
| `GET services/{s}` | Workers Scripts Read |
| `GET scripts/{s}/secrets` | Workers Scripts Read |
| `GET scripts/{s}/deployments` | Workers Scripts Read |
| `GET scripts/{s}/settings` | Workers Scripts Read |
| `GET workers/{s}` | Workers Scripts Read |
| `GET /accounts/{account}/queues?name={queue}&page=1` | Queues Read |
| `POST /accounts/{account}/containers/image-preparations` | Workers Containers Write |
| `GET scripts/{s}/versions/{version}` | Workers Scripts Read |
| `GET /accounts/{account}/containers/applications/{namespace}` | Workers Containers Read |
| `POST /accounts/{account}/containers/applications` | Workers Containers Write |
| `PATCH /accounts/{account}/containers/applications/{namespace}` | Workers Containers Write |
| `POST scripts/{s}/versions?bindings_inherit=strict` | Workers Scripts Write + Secrets Store Write |
| `PUT scripts/{s}?excludeScript=true&bindings_inherit=strict` | Workers Scripts Write + Secrets Store Write |
| `POST scripts/{s}/deployments` | Workers Scripts Write |
| `PATCH scripts/{s}/script-settings` | Workers Scripts Write |
| `POST scripts/{s}/subdomain` | Workers Scripts Write |

The `PUT` is used when a Durable Object migration is pending, and always when the Worker has containers.

The container calls are limited to this Worker's own containers. `{namespace}` must be the Durable Object namespace of one of its container classes (`CiRunner`), which the proxy looks up itself before such a call. The image preparation must name an image in this account's registry (`registry.cloudflare.com/<account>/…`), and an application can only be created for one of those namespaces; the proxy checks both bodies. The queues call checks that each queue the Worker produces to exists; it must name a queue in the proxy's settings (`pigeon-ci-events` today), so it can't list the account's queues.

Each call gets a Cloudflare token with only that call's permissions. The token lives 5 minutes and is revoked before the answer returns.

Anything else is refused in Cloudflare's error shape, with a message starting "pigeon's proxy does not allow ...".

### Deploying

Deploy with the pinned wrangler, from `src/platform`: `npm ci`, then `npx wrangler deploy`. Never a
globally installed or `npx wrangler@latest` one: the allowlist matches 4.166.0's calls. Set these
variables:

```
CLOUDFLARE_API_BASE_URL=https://proxy.pigeon.dev/cf/client/v4
CLOUDFLARE_ACCOUNT_ID=4f94676cb2cbe46fd985b089ca97ea27
CLOUDFLARE_API_TOKEN=unused
WRANGLER_SEND_METRICS=false
```

Wrangler will not start without a token. The value is a placeholder, and the network proxy replaces the header.

If a newer wrangler makes a call outside the list, the deploy fails and names the call. Check what the call does, then add it to `ALLOWED` in `src/capabilities/src/cf_proxy.rs`.

### Checking the allowlist against what runs

`ci/deploy/allowlist.sh` runs the pinned wrangler's real `deploy` against a stand-in for the API (`ci/deploy/standin.mjs`), both as for the existing Worker and as for a first deploy, records every call with its JSON body, and checks each against the allowlist and its body rules (`cf_proxy::unlisted`). It fails naming any call the proxy would refuse, so a wrangler upgrade or a new binding shows its new calls before a real deploy is refused halfway. CI runs it as the `allowlist` job; it needs Node 22 and `worker-build`, and no credential.

A change to the allowlist only takes effect once it is deployed, and the deployed proxy is the one that has to let that deploy through. So when a deploy needs a call the deployed allowlist lacks (a new kind of binding, such as containers), deploy the change once directly with a `deploy_token`; later deploys can go through the proxy.

Wrangler turns some refusals into its own wording. For example, a refused image preparation shows as "Container image preparation is not enabled for this account". When a deploy through the proxy fails with an error like that, check the `cf` audit lines for a `refused` outcome first.

## Audit log

The Worker's logs hold one JSON line per proxied request:

```json
{"audit":"proxy","route":"git","caller":"<label> (<person>)","token":"<credential id>","method":"POST","path":"/git/...","outcome":"forwarded","status":200,"why":null}
```

| Field | Value |
| --- | --- |
| `route` | `git` or `cf` |
| `caller` | `<label> (<person>)` |
| `token` | the credential id |
| `method` | the HTTP method |
| `path` | the path, without the query |
| `outcome` | `forwarded`, `refused` or `failed` |
| `status` | the response status |
| `why` | the reason, when refused or failed |

A line never holds a token, hash, header or body.

## Risks

Whoever holds a proxy credential can upload and deploy `pigeon-platform`, and that Worker holds the parent token. A proxy credential is therefore effectively as powerful as the Cloudflare account. This is an accepted risk.

- Keep lifetimes short.
- Revoke credentials that are not in use.
- Watch the audit lines for `cf` deploys.

A passkey approval held on the final deployment call is a possible mitigation. It was considered and not built; the risk is accepted for now.

The MCP tools `source_token` and `deploy_token` still exist, for people and for the deploying session. Retiring them is a later decision.

## Hosts the environment no longer needs

Once the proxies are checked live, the environment no longer needs:

- the Artifacts git host `4f94676cb2cbe46fd985b089ca97ea27.artifacts.cloudflare.net`
- `api.cloudflare.com`

Remove `github.com` only after checking that the session's first clone does not depend on it.

## Code

| Area | Files |
| --- | --- |
| Proxy credential, host routing rule, audit line | `src/capabilities/src/proxy.rs` |
| Git proxy | `src/capabilities/src/git_proxy.rs` |
| Cloudflare API proxy | `src/capabilities/src/cf_proxy.rs` |
| Git and pack handling | `src/core/src/git.rs`, `src/core/src/pack.rs` |
| Host routing | `src/platform/src/lib.rs` |
| Proxy host and the mirrored branches | `src/platform/src/settings.rs` |

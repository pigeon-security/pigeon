# Proxies

This page is for operators of pigeon. It describes the two proxies on `proxy.pigeon.dev`, how to issue and use the proxy credential, and what the proxies allow.

## What and why

Cloud coding sessions used to reach the Artifacts git host, `api.cloudflare.com` and `github.com` directly. Tokens were pasted into commands, so they ended up in transcripts.

Now sessions reach git and the Cloudflare API only through pigeon's proxies on `proxy.pigeon.dev`. The proxies hold the real credentials and mint a short-lived one for each request. MCP stays on `platform.pigeon.dev`.

`pigeon-platform` (core) serves `platform.pigeon.dev` and the public site on the apex `pigeon.dev` (see `docs/plans/one-worker.md`). `pigeon-proxy` serves `proxy.pigeon.dev`. On `proxy.pigeon.dev` only `/git/`, `/cf/` and `/health` answer. On any other host, `/git/` and `/cf/` answer 404.

### The proxy Worker, and the backup in core

The primary is `pigeon-proxy` (`src/proxy`). It has served `proxy.pigeon.dev` since the cutover (`docs/plans/workers-split.md`, phase 6), with the proxies' code in `src/proxies`. It answers only `/git/`, `/cf/` and `/health` on `proxy.pigeon.dev`, and 404 to everything else.

The backup is core. Core builds the same proxies from the same code (`src/proxies`): the same repository and push rules, the same allowlist and body rules, the same deploy grants, queues and container classes. They are not capabilities of their own: core serves them under the `proxy` capability (Access › Tokens, `src/capabilities/src/proxy.rs`, `Tokens.backup`), with no tools and no version of their own. Core answers `/git/` and `/cf/` only for requests to `proxy.pigeon.dev`, so the backup serves nothing until that hostname is attached to core.

#### Switching to the backup

Do this when `pigeon-proxy` cannot serve (a bad deploy, its `PLATFORM` binding failing). It needs no landing: a broken proxy would also block the landing that fixes it.

1. Call `setup_proxy_host {"worker": "pigeon-platform"}`. The call goes through `/mcp` on `platform.pigeon.dev`, so it works while the proxy host is down. It attaches `proxy.pigeon.dev` to core.
2. Check: `git ls-remote https://proxy.pigeon.dev/git/pigeon-security/pigeon.git`. Audit lines now come from `pigeon-platform`'s logs.
3. Leave `PROXY_WORKER` in `src/platform/src/settings.rs` as it is. It is only the default when `setup_proxy_host` is called without `worker`.

While core serves the host, revoking a credential takes effect at once, and each `/cf/` call gets a token minted for that call alone (below).

#### Switching back to the proxy Worker

1. Fix and deploy `pigeon-proxy`. Its deploy goes through core's deploy proxy, which allows it from the map.
2. Call `setup_proxy_host {"worker": "pigeon-proxy"}`, or `setup_proxy_host {}`: `pigeon-proxy` is the default (`PROXY_WORKER`).
3. Check as in step 2 above. Audit lines come from `pigeon-proxy`'s logs again.

### How the proxy Worker reaches core

It never holds the parent token. It asks core over its `PLATFORM` service binding, signing each request with `CALLER_KEY`:

| Op | What | Kept |
| --- | --- | --- |
| `verify {credential}` | who holds a proxy credential | 60 s per isolate, keyed by the credential's hash |
| `grant {token, repo}` | a CI job token's grant, for this repository | never |
| `deploy_grant {token, repo}` | the caller a landing's deploy job token stands for | never |
| `credential {kind: scoped}` | a 15-minute Cloudflare token: purpose `artifacts` for `/git/`, `cf_deploy` for `/cf/` | until 60 s before it expires |

So once `pigeon-proxy` serves the host, **revoking a proxy credential takes up to 60 seconds** to reach it: an isolate that verified the credential in the last minute still accepts it. A new credential works at once (an unknown one is never kept). CI job grants and deploy grants are asked of core on every request.

What else differs from core's backup:

- `/cf/` calls use the `cf_deploy` token core minted for the Worker, not one minted per call with only that call's permissions. `cf_deploy` is the union of the permissions in the table below. The allowlist and the body rules are the same.
- `/git/` mints and revokes a git token per request as core does, through the `artifacts` token.

## The proxy credential

The credential is a bearer token with the prefix `pgn_px_`. It is shown once and kept only as a hash.

MCP refuses it, because MCP looks up only its own token kind. The proxies refuse MCP tokens.

### Issue

Open the console's Access → Credentials → Proxy credentials (https://platform.pigeon.dev/app?tab=management&subtab=credentials). The old `/access`, `/settings` and `/tokens` pages redirect there.

- Seeing the list takes the console's sign-in.
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

### Expiry

The console's Access tab shows every credential's expiry, on each secret and each proxy credential. A proxy credential that has expired stays listed, flagged, for two weeks. The MCP tool `credentials_expiry` answers the same list, read-only. Nothing shows a value.

| Credential | How its expiry is known |
| --- | --- |
| Proxy credentials | Their own `expires_at`, set when issued |
| `CF_PARENT_TOKEN` | Cloudflare's `tokens/verify`, `expires_on` |
| `GITHUB_MIRROR_TOKEN` | GitHub's `github-authentication-token-expiration` header, on `GET /rate_limit` |
| `BUILDKITE_API_TOKEN` | Buildkite's `GET /v2/access-token`, `expires_at` (unknown when it names none). The one Buildkite token: setup, connector and the portal's Buildkite server (`docs/plans/buildkite-mcp.md`). The webhook's key is not a secret on this list: pigeon generates it and the CI scheduler keeps it. |
| Claude Platform key | Unknown: the API reports no key expiry, and only the AI gateway can read it |

Each secret is checked against its issuer at most once a day (a failed check is retried after an hour), and again right after a new value is set. An issuer that refuses a token outright (a 401, or Cloudflare saying it is not `active`) shows it as expired. A capability reports its secrets' expiry with `Capability::secret_expiry`; the cache and the statuses are in `src/capabilities/src/expiry.rs`.

To replace one before it bites: a proxy credential, issue a new one, save it as the environment's network secret, then revoke the old one; a secret, set its new value on Access → Secrets and credentials.

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

The proxy checks the push's ref updates before it mints any token. The rules are the person's policy, `.pigeon/policy.toml` (see `docs/plans/policy.md`): each ref update is asked of it.

- A branch under `refs/heads/claude/` may be created, pushed, rewritten (force-pushed) or deleted: rule `session-branches`. A rewrite (the old tip not in the new one's history) is recorded as `rewrite_branch`: the proxy reads the branch's history once the push has gone through.
- `main` is never pushed, created or deleted through the proxy: rule `main-forward-only`. It moves only by a landing's fast-forward (`land`).
- Any other ref, tags included, is not in policy.
- Ref names are checked strictly.
- Signed pushes are refused.

Refusals show in `git push` output as rejected refs, with the reason and the rule. Each ref update's decision is also an audit line (`"audit":"policy"`).

A missing or unknown credential gets 403, not 401, so git does not prompt for a password.

Session branches stay in Artifacts. The GitHub mirror copies only `main`.

### Removing a session branch

A session removes its own `claude/` branch through the proxy:

```
git push https://proxy.pigeon.dev/git/pigeon-security/pigeon.git --delete claude/route-check
```

Several branches go in one push: `--delete claude/a claude/b`. Before the policy, the proxy refused every delete; a person can still remove one directly on Artifacts, from a session on the Default environment or from their own machine, with the Pigeon MCP tools:

1. Call `source_token` with `{"access": "write", "ttl": 300}`. Note the `id` and `remote` it answers.
2. In any git checkout, read the token without putting it in the command or the history, push the delete, then forget the token:

   ```
   read -rs TOKEN   # paste the token, then Enter
   git -c http.extraHeader="Authorization: Bearer $TOKEN" push <remote> --delete claude/route-check
   unset TOKEN
   ```

   Several branches go in one push: `--delete claude/a claude/b`.
3. Call `source_revoke` with the `id` from step 1.
4. Check from any pigeon session: `git ls-remote https://proxy.pigeon.dev/git/pigeon-security/pigeon.git 'refs/heads/claude/*'` no longer lists it.

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

Served by core, each call gets a Cloudflare token with only that call's permissions. The token lives 5 minutes and is revoked before the answer returns. Served by `pigeon-proxy`, each call uses its `cf_deploy` token (above).

Outside `/cf/client/v4`, `GET /cf/grant` answers whether the bearer is a CI job token holding the deploy grant of the landing that is deploying its commit: 200 if so, 403 otherwise (a proxy credential included). It passes nothing on; the `deploy` job calls it before its build so a job submitted by hand stops at once.

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

If a newer wrangler makes a call outside the list, the deploy fails and names the call. Check what the call does, then add it to `ALLOWED` in `src/proxies/src/cf_proxy.rs`, and its permissions to `cf_deploy` in `src/capabilities/src/split.rs`.

### Checking the allowlist against what runs

`ci/deploy/allowlist.sh` runs the pinned wrangler's real `deploy` against a stand-in for the API (`ci/deploy/standin.mjs`), both as for the existing Worker and as for a first deploy, records every call with its JSON body, and checks each against the allowlist and its body rules (`cf_proxy::unlisted`). It fails naming any call the proxy would refuse, so a wrangler upgrade or a new binding shows its new calls before a real deploy is refused halfway. CI runs it as the `allowlist` job; it needs Node 22 and `worker-build`, and no credential.

A change to the allowlist only takes effect once it is deployed, and the deployed proxy is the one that has to let that deploy through. When the new call comes after the upload (the container application, settings and subdomain calls do), a landing deploys it anyway: the `deploy` job's first `wrangler deploy` uploads the new Worker and is refused at the new call, and `ci/deploy/deploy.sh` deploys once more, now through the new allowlist. When the new call comes before the upload (wrangler's lookups, the image preparation), the deploy job says so and the change has to be deployed once outside the land flow.

### Break-glass: deploying directly

The pigeon environment allows neither `api.cloudflare.com` nor the Artifacts host, so from it the only deploys go through `/cf/`. Some changes cannot deploy that way. An example is a change to the deploy allowlist itself that the deployed proxy refuses before the upload. Deploy such a change once directly, with a `deploy_token`, from a break-glass session or from the person's own machine. Only a person starts this. These steps worked on 23 November 2026.

1. **The environment.** Use a personal Claude Code environment kept for this, named `pigeon break-glass`. Its network allows only:
   - `api.cloudflare.com`
   - the package registries that `npm ci` and the build need
   - the Pigeon MCP host (`mcp.pigeon.dev`)

   It holds no secrets. The **Default** environment no longer allows `api.cloudflare.com`, so it cannot deploy.
2. **The session.** The person starts the session themselves, on that environment, with the Pigeon connector on. A session started by another session does not always get the connector. Check that `land_status` answers before going on.
3. **The code.** The session's checkout is main, from the GitHub mirror (`https://github.com/pigeon-security/pigeon`). To deploy a branch that has not landed, call `branch_patch` with `{"branch": "<branch>"}`. It answers the branch as a patch against its merge base with main, as `patch`, along with `base`. It is read-only and needs neither Artifacts nor the git proxy. Write `patch` to a file, then:

   ```
   git cat-file -e <base>        # the checkout must have the base; if not, wait for the mirror
   git apply --3way branch.patch
   ```

   Check the answer's `skipped` list. These are not in the patch: binary files, submodules, symlinks, secret-shaped files (such as `.env` or a private key) and files over 256 KiB. A patch over 1 MiB is refused, never cut short.
4. **Authorise `deploy_token` in the session.** `deploy_token` is not in `.claude/settings.json`, so Claude Code asks in the session before calling it, and may refuse on its own. The person says in the session that they authorise this `deploy_token` call, and allows the prompt.
5. **The token.** Call `deploy_token` with `{"preset": "deploy", "ttl": 1800}`.
   - Each call needs the person's passkey (rule `risky-tools-need-passkey`, `docs/plans/tool-approvals.md`). The first call answers an approval id and mints nothing.
   - The person checks the card on the console's Approvals tab. Its "Token permissions" row must list the preset's permissions, and "Token lifetime" must say 1800 s. Then they approve with the passkey.
   - Call `deploy_token` again with the same arguments and `"approval": "<id>"`. Note the token's `id`.

   Approving needs only the platform Worker, its Directory and the person's passkey: none of the land flow, CI or the proxies.

   The `deploy` preset holds every permission the deploy proxy's allowlist names (the table above), plus Account Settings Read. It is derived from `ALLOWED` in `src/proxies/src/cf_proxy.rs`, so the two cannot drift. Without the preset, `deploy_token` mints Workers Scripts Write, Account Settings Read and Secrets Store Write. That is not enough for core: its deploy stops at "Container image preparation is not enabled" (403), for lack of Workers Containers Write, and it needs Queues Read too.
6. **The deploy.** For core, run this from `src/platform`, reading the token from the prompt rather than writing it in the command. For another Worker, use its `dir` in `ci/workers.toml` and see its script in `ci/deploy/`.

   ```
   cd src/platform
   npm ci
   read -rs CLOUDFLARE_API_TOKEN && export CLOUDFLARE_API_TOKEN   # paste the token, then Enter
   unset CLOUDFLARE_API_BASE_URL
   CLOUDFLARE_ACCOUNT_ID=4f94676cb2cbe46fd985b089ca97ea27 WRANGLER_SEND_METRICS=false npx wrangler deploy
   unset CLOUDFLARE_API_TOKEN
   ```

   This is the pinned wrangler, talking to `api.cloudflare.com` itself: no proxy and no allowlist.
7. **Revoke.** Call `revoke_api_token` with the `id` from step 5, whether or not the deploy worked. It needs no passkey. Then end the session.
8. **Land.** If the deployed commit is not on main yet, land its branch the usual way (`land`), so main and the deployed Worker agree. Once the fixed allowlist is live, that landing's deploy goes through `/cf/` again.

From step 5 on, the token is in the session's transcript. Keep its lifetime short and revoke it at once. Never save a `deploy_token` anywhere. On the person's own machine, steps 1, 2 and 4 do not apply, and a plain `git checkout` of the branch replaces step 3 where the machine can fetch it.

### When the platform itself is down

If `pigeon-platform` does not answer (a bad deploy, the Worker or its Directory failing), there is no `deploy_token` to call or approve. The person deploys with their own Cloudflare login and nothing of pigeon's, on their own machine, from `src/platform` of a good commit:

```
cd src/platform
npm ci
npx wrangler login              # the person's own Cloudflare account, in the browser
npx wrangler rollback           # back to the last good version; or, for a good commit:
CLOUDFLARE_ACCOUNT_ID=4f94676cb2cbe46fd985b089ca97ea27 WRANGLER_SEND_METRICS=false npx wrangler deploy
```

Then land the fix the usual way, so main and the deployed Worker agree.

### Landing deploys

The `land` flow's `deploy` job (`ci/deploy/deploy.sh`, in a Container) deploys with its own CI job token as `CLOUDFLARE_API_TOKEN`. The proxy takes a job token (`pgn_ci_…`) only when the Lander says that job is the deploy job of the landing that is `deploying`, for the commit the token's grant names, in this repository; its calls pass the same allowlist, and its audit lines name the caller `land <id> deploy`. No stored or minted token is in the job.

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

A passkey approval before a landing moves main and deploys is built, off by default: the console's Settings → Landing. What a landing's own changes need is the policy's (`docs/plans/policy.md`): a change to `.pigeon/policy.toml` or the generated `.claude/settings.json` always waits for the person's passkey (`policy-is-yours`); a change to what the gate runs (`ci/`, `.pigeon/ci.toml`, `.cargo/`, the toolchain, the npm files) is approved by the person's delegate without a wait (`gate-changes`).

The MCP tools `source_token` and `deploy_token` still exist, for people and for the deploying session. Retiring them is a later decision.

## Hosts the environment no longer needs

Once the proxies are checked live, the environment no longer needs:

- the Artifacts git host `4f94676cb2cbe46fd985b089ca97ea27.artifacts.cloudflare.net`
- `api.cloudflare.com`

Remove `github.com` only after checking that the session's first clone does not depend on it.

## Code

| Area | Files |
| --- | --- |
| Proxy credential checks, host routing rule, audit line | `src/proxies/src/proxy.rs` |
| Issuing and revoking credentials; core's `Host` (`Local`) and the backup proxies (`Backup`) | `src/capabilities/src/proxy.rs` |
| Git proxy | `src/proxies/src/git_proxy.rs` |
| Cloudflare API proxy | `src/proxies/src/cf_proxy.rs` |
| The proxy Worker and its `Host` (asking core) | `src/proxy/src/lib.rs` |
| What the proxy Worker keeps per isolate | `src/proxies/src/cache.rs` |
| Core's `verify`, `grant`, `deploy_grant` ops and the `cf_deploy` purpose | `src/capabilities/src/split.rs` |
| Git and pack handling | `src/core/src/git.rs`, `src/core/src/pack.rs` |
| Host routing | `src/platform/src/lib.rs` |
| Proxy host, the Worker serving it (`PROXY_WORKER`), the backup as core builds it (`backup()`) and the mirrored branches | `src/platform/src/settings.rs` |

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

Open the console's Settings → Keys → Proxy credentials (https://platform.pigeon.dev/settings#credentials; the old `/tokens` page redirects there).

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

The console shows every credential's expiry: the Overview lists them all and warns at the top when one expires within 7 days or has expired; Settings → Keys shows it on each secret and each proxy credential. A proxy credential that has expired stays listed, flagged, for two weeks. The MCP tool `credentials_expiry` answers the same list, read-only. Nothing shows a value.

| Credential | How its expiry is known |
| --- | --- |
| Proxy credentials | Their own `expires_at`, set when issued |
| `CF_PARENT_TOKEN` | Cloudflare's `tokens/verify`, `expires_on` |
| `GITHUB_MIRROR_TOKEN` | GitHub's `github-authentication-token-expiration` header, on `GET /rate_limit` |
| `BUILDKITE_API_TOKEN` | Buildkite's `GET /v2/access-token`, `expires_at` (unknown when it names none) |
| `BUILDKITE_WEBHOOK_TOKEN` | Never expires: a signing key pigeon and Buildkite share |
| Claude Platform key | Unknown: the API reports no key expiry, and only the AI gateway can read it |

Each secret is checked against its issuer at most once a day (a failed check is retried after an hour), and again right after a new value is set. An issuer that refuses a token outright (a 401, or Cloudflare saying it is not `active`) shows it as expired. A capability reports its secrets' expiry with `Capability::secret_expiry`; the cache and the statuses are in `src/capabilities/src/expiry.rs`.

To replace one before it bites: a proxy credential, issue a new one, save it as the environment's network secret, then revoke the old one; a secret, set its new value on Settings → Keys.

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

Each call gets a Cloudflare token with only that call's permissions. The token lives 5 minutes and is revoked before the answer returns.

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

If a newer wrangler makes a call outside the list, the deploy fails and names the call. Check what the call does, then add it to `ALLOWED` in `src/capabilities/src/cf_proxy.rs`.

### Checking the allowlist against what runs

`ci/deploy/allowlist.sh` runs the pinned wrangler's real `deploy` against a stand-in for the API (`ci/deploy/standin.mjs`), both as for the existing Worker and as for a first deploy, records every call with its JSON body, and checks each against the allowlist and its body rules (`cf_proxy::unlisted`). It fails naming any call the proxy would refuse, so a wrangler upgrade or a new binding shows its new calls before a real deploy is refused halfway. CI runs it as the `allowlist` job; it needs Node 22 and `worker-build`, and no credential.

A change to the allowlist only takes effect once it is deployed, and the deployed proxy is the one that has to let that deploy through. When the new call comes after the upload (the container application, settings and subdomain calls do), a landing deploys it anyway: the `deploy` job's first `wrangler deploy` uploads the new Worker and is refused at the new call, and `ci/deploy/deploy.sh` deploys once more, now through the new allowlist. When the new call comes before the upload (wrangler's lookups, the image preparation), the deploy job says so and the change has to be deployed once outside the land flow.

### Break-glass: deploying directly

The pigeon environment no longer allows `api.cloudflare.com` or the Artifacts host, so the only deploys from it are through `/cf/`. When a change cannot deploy that way, such as a change to the deploy allowlist itself that the deployed proxy refuses before the upload, deploy once directly, from a session on the **Default** environment (which allows `api.cloudflare.com`) or from the person's own machine, with a `deploy_token`. Only a person should start this.

1. Check out the commit to deploy. It should be what main will be: a landed main from `https://github.com/pigeon-security/pigeon` (the mirror copies main), or the `claude/` branch that is landing next.
2. Call the Pigeon MCP tool `deploy_token` with `{"ttl": 1800}`. Note its `id`.
3. From `src/platform`, with the token read from the prompt rather than written in the command:

   ```
   cd src/platform
   npm ci
   read -rs CLOUDFLARE_API_TOKEN && export CLOUDFLARE_API_TOKEN   # paste the token, then Enter
   unset CLOUDFLARE_API_BASE_URL
   CLOUDFLARE_ACCOUNT_ID=4f94676cb2cbe46fd985b089ca97ea27 WRANGLER_SEND_METRICS=false npx wrangler deploy
   unset CLOUDFLARE_API_TOKEN
   ```

   This is the pinned wrangler, talking to `api.cloudflare.com` itself: no proxy and no allowlist.
4. Call `revoke_api_token` with the `id` from step 2, whether or not the deploy worked.
5. If the deployed commit is not on main yet, land its branch the usual way (`land`), so main and the deployed Worker agree. Once the fixed allowlist is live, that landing's deploy goes through `/cf/` again.

In a session, the token is in the transcript from step 2 on: keep the lifetime short, and revoke it at once. Never save a `deploy_token` anywhere.

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
| Proxy credential, host routing rule, audit line | `src/capabilities/src/proxy.rs` |
| Git proxy | `src/capabilities/src/git_proxy.rs` |
| Cloudflare API proxy | `src/capabilities/src/cf_proxy.rs` |
| Git and pack handling | `src/core/src/git.rs`, `src/core/src/pack.rs` |
| Host routing | `src/platform/src/lib.rs` |
| Proxy host and the mirrored branches | `src/platform/src/settings.rs` |

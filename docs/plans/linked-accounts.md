# Linked accounts, instances and sites: noisypigeon.com through pigeon

Status: **phase 1 deployed; phase 2a (instance records and the Instances tab) landed** (2026-11-21); instance records are in use (`src/capabilities/src/instances/`). Branch
`claude/linked-accounts`. Phase 0 has tests only the person can run.

## Goal

The person's words: link another Cloudflare account to pigeon, take ownership of its zone(s),
and deploy a website on that zone and/or a pigeon.dev subdomain. The aim is to deploy the
noisypigeon.com blog through the platform, from the public repo `noisypigeon/pigeon`, using R2
and a GitHub App. The GitHub App also talks to GitHub through GitHub's MCP server behind the
portal. The console shows both instances: `pigeon-security/platform` and `noisypigeon`.

Hard lines this plan keeps:
- No new policy rules.
- Tests only in `tests/smoke.rs`, which stays under 60 s.
- Credentials never go through chat.
- A change in the noisypigeon account touches only what pigeon made or was explicitly given. It
  shows a dry run first and is safe to run twice.
- Nothing serving noisypigeon.com changes (DNS, zone settings, rules, routes, buckets) without
  the person's go-ahead **for that step, in the session that makes it**.

## What is true today (checked 2026-11-21)

### noisypigeon.com

Read from `noisypigeon/pigeon` at `841e7fb`. noisypigeon.com is served by an earlier platform in
that repository. It has no Worker:

- **Content.** `workloads/noisypigeon.com/blog/` holds the blog's content, about 165 files and
  28 MB, mostly images. `config.yaml` pins site template `blog` 1.0.2.
- **Build.** `pigeon sites build workloads/noisypigeon.com/blog` (`src/pigeon-harness`) builds
  it with Zola 0.23.6. The build needs no secrets, but it does need the repository's history,
  because the template is read at a git tag.
- **Bucket.** An EU-jurisdiction R2 bucket, `blog-noisypigeon-com`, read from the repository as in the noisypigeon account. (2026-11-22: wrong. R2 was not enabled on that account; see the Log.)
  Every `x/index.html` is also written as `x` and `x/`.
- **Custom domains.** R2 custom domains `noisypigeon.com` and `www.noisypigeon.com` on the
  bucket. They own those DNS records.
- **Rules.** Two zone rules: a URL rewrite, `noisypigeon.com/` → `/index.html`, and a single
  redirect, `www` → apex (301).
- **Other DNS records (Proton mail).** The apex unit owns Proton mail verification and DMARC
  records. **pigeon must never touch them.**
- **Tokens.** Hand-scoped tokens: `apex-noisypigeon.com` (DNS, Transform and Redirect Write) and
  `site-noisypigeon.com-blog` (R2 Storage Write, Zone Read, DNS Write), rotated every 90 days.

This session's network does not allow noisypigeon.com, so its live headers were not read. Phase
0 asks for the host.

### pigeon

- **One Worker.** `pigeon-platform` serves platform.pigeon.dev, proxy.pigeon.dev and the apex
  pigeon.dev, routed by host.
- **Cloudflare calls.** Every call goes through `pigeon_core::Account { id, parent,
  secrets_store }`, with a token minted from `CF_PARENT_TOKEN` and revoked after the call.
- **Secrets.** Secrets are fixed `Slot`s bound in `wrangler.toml`. The console's Access tab sets
  them with a passkey bound to the value's hash.
- **Scope of the model.** Everything assumes one account, one zone (`settings::ZONE`) and one
  repository (`pigeon-security/pigeon` in Artifacts, mirrored to GitHub by `GITHUB_MIRROR_TOKEN`,
  a PAT).

## Sources

Cloudflare:
- OAuth:
  - [create a client](https://developers.cloudflare.com/fundamentals/oauth/create-an-oauth-client/)
  - [authorizing](https://developers.cloudflare.com/fundamentals/oauth/authorizing-an-application/)
  - [integrate](https://developers.cloudflare.com/fundamentals/oauth/integrate-with-cloudflare/)
  - [launch, 2026-06-03](https://developers.cloudflare.com/changelog/post/2026-06-03-public-oauth-clients/)
  - [oauth_clients API](https://developers.cloudflare.com/api/resources/iam/subresources/oauth_clients/)
  - [oauth scopes](https://developers.cloudflare.com/api/resources/iam/subresources/oauth_scopes/methods/list/)
- [Account-owned tokens](https://developers.cloudflare.com/fundamentals/api/get-started/account-owned-tokens/)
  and their [verify](https://developers.cloudflare.com/api/resources/accounts/subresources/tokens/methods/verify/).
- R2:
  - [buckets](https://developers.cloudflare.com/api/resources/r2/subresources/buckets/methods/create/)
  - [objects](https://developers.cloudflare.com/api/resources/r2/subresources/buckets/subresources/objects/)
  - [custom domains](https://developers.cloudflare.com/api/resources/r2/subresources/buckets/subresources/domains/subresources/custom/methods/create/)
  - [tokens and S3 credentials](https://developers.cloudflare.com/r2/api/tokens/)
- Workers:
  - [script upload](https://developers.cloudflare.com/api/resources/workers/subresources/scripts/methods/update/)
  - [routes](https://developers.cloudflare.com/workers/configuration/routing/routes/)
  - [custom domains](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/)
  - [static assets direct upload](https://developers.cloudflare.com/workers/static-assets/direct-upload/)
  - [limits](https://developers.cloudflare.com/workers/platform/limits/)
  - [workers.dev](https://developers.cloudflare.com/api/resources/workers/subresources/subdomains/)
- [Ruleset phases](https://developers.cloudflare.com/ruleset-engine/reference/phases-list/).
- [Secrets Store limits](https://developers.cloudflare.com/secrets-store/manage-secrets/).
- [MCP server portals](https://developers.cloudflare.com/cloudflare-one/access-controls/ai-controls/mcp-portals/).

GitHub (read from the github/docs sources and github/github-mcp-server):
- App visibility:
  - [registering](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app)
  - [public or private](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/making-a-github-app-public-or-private)
- App authentication:
  - [JWT](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app)
  - [installation tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app)
  - [private keys](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/managing-private-keys-for-github-apps)
  - [user tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens)
- Webhooks:
  - [validating](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries)
  - [best practices](https://docs.github.com/en/webhooks/using-webhooks/best-practices-for-using-webhooks)
  - [failed deliveries](https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries)
- REST:
  - [tarball](https://docs.github.com/en/rest/repos/contents#download-a-repository-archive-tar)
  - [deployments](https://docs.github.com/en/rest/deployments/deployments#create-a-deployment)
  - [statuses](https://docs.github.com/en/rest/deployments/statuses#create-a-deployment-status)
- Remote MCP server:
  - [remote server](https://github.com/github/github-mcp-server/blob/main/docs/remote-server.md)
  - [governance](https://github.com/github/github-mcp-server/blob/main/docs/policies-and-governance.md)
  - [host integration (no DCR)](https://github.com/github/github-mcp-server/blob/main/docs/host-integration.md)
  - [app auth, local only](https://github.com/github/github-mcp-server/blob/main/docs/github-app-auth.md)

Coordination:
- The MCP portal planner (`session_015XFA1jS9tQyZ6y3cuBKdBc`): portal state, service tokens,
  never PUT a portal's server mapping from code.
- The Buildkite planner (`session_015XFA1jUggi6PutoW6hQGyh`): no platform-as-MCP-client design
  exists; Buildkite uses REST from the Worker.
- The parent session: OAuth is the primary way to link.

## Decisions

### 1. "Take ownership" means the noisypigeon account holds the site, managed in place

The person's answer: the noisypigeon account holds the static site's R2 bucket, and its DNS
records stay in that account. The platform account has no blog for now. pigeon creates and
manages these resources **in** the noisypigeon account. The zone is not moved, so its
nameservers and the Proton mail records are never at risk.

What pigeon is given there is **declared**:

| Kind | Declared for noisypigeon | pigeon may |
| --- | --- | --- |
| Zone | `noisypigeon.com` | read the zone and its DNS records, custom domains, routes and rules (the console's inventory) |
| R2 bucket | `noisypigeon-site` (EU, made by pigeon; named after the site's hostname) | create; put, list and delete objects |
| Worker | `noisypigeon` (made by pigeon), bound to that bucket | create, upload versions, enable workers.dev |
| Route | `noisypigeon.com/*` → `noisypigeon` (made by pigeon) | create and delete, only with the person's go-ahead (the cutover) |
| DNS records | only records pigeon creates for a declared hostname, tagged `pigeon` and recorded in the ledger. None are needed for the cutover | create and delete its own, with a dry run and the person's passkey |
| The old bucket `blog-noisypigeon-com`, its R2 custom domains, the rules, every other DNS record (Proton) | none | read only |

Declarations are **data, not code** (the person, 2026-11-21: "We shouldn't encode subdomains for
these in the code"). They live in the instance's record in the Directory (decision 7), not in
`settings.rs`. A change to what pigeon may touch takes the person's passkey on the console. The
OAuth grant may cover more of the account, and pigeon ignores the rest.

Every resource pigeon makes is named `pigeon-…` and recorded, with its id, in the Directory's
ledger for the instance. A change or delete is refused unless the id is in the ledger. This is
the lesson of the deleted Access apps: never act on "everything matching a domain".

### 2. Linking: Cloudflare OAuth first, a hand-made account token as fallback

**Primary: OAuth.** pigeon is a confidential OAuth client (authorization code, PKCE S256,
`refresh_token` grant). Self-managed clients are on all plans.

1. **Register the client.** Name `pigeon`. Redirect
   `https://platform.pigeon.dev/linked/callback`. Scopes are the IDs that `GET /oauth/scopes`
   lists for: Account Settings Read, Zone Read, DNS Write (pigeon's own records only; decision 1),
   Workers Scripts Write, Workers Routes Write and Workers R2 Storage Write.
   - By API: `POST /accounts/{pigeon}/oauth_clients`, with a token minted from the parent for
     "OAuth Client Write". The secret is shown once, so pigeon writes it straight to the Secrets
     Store as `CF_OAUTH_CLIENT_SECRET` and keeps `client_id` in settings. If the parent cannot
     mint that permission, the person registers the client in the dashboard (Manage Account →
     OAuth clients) and enters the secret on the Access tab.
2. **Link.** The console's instance page shows **Link Cloudflare account** for a declared
   instance with no link. The button sends the browser to `dash.cloudflare.com/oauth2/auth` with
   `state` (a Directory challenge bound to the instance and the signed-in person) and a PKCE
   verifier. The person signs in to Cloudflare and picks the noisypigeon account on the consent
   screen.
3. **Callback.** `GET /linked/callback` checks `state` and exchanges the code (client secret
   plus verifier). Before storing anything it checks the grant against the declaration. It lists
   accounts, finds the declared account id, and finds zone `noisypigeon.com` in it, active. On a
   mismatch it stores nothing and revokes the grant.
4. **Storage.** The access and refresh tokens go in the Directory under `link:{instance}`,
   encrypted with AES-GCM. The key is `LINK_KEY`, a Secrets Store secret that the platform
   generates once (`linked_setup`) and never shows. Only the platform reads it.
   - A rotating refresh token can't live in a statically bound Secrets Store secret: a binding
     reads one fixed name, and rewriting it on every refresh races with reads.
   - What is stored: `{account_id, scopes, granted_by, granted_at, access_expires, refresh,
     refresh_seen_at}`. Never shown and never logged.
5. **Use.** `pigeon_core::Account` gains a credential kind:
   - `Parent(token)` keeps today's mint and revoke.
   - `Bearer(token)` uses the token as it is and revokes nothing.

   Every core block (zones, Workers, R2, routes) works unchanged on a linked account through
   `Scoped::open`, which then hands back the bearer. The linked capability refreshes the access
   token when it has under 5 minutes left. Refresh is serialized in the Directory, because a
   reused refresh token may invalidate the whole chain (not confirmed by the docs; phase 0
   tests it).
6. **Expiry and rotation.** The Access tab gets a row per link:
   - account, scopes, granted by and when;
   - access token expiry;
   - the refresh token's last successful use;
   - status `ok`, `refresh failed` or `revoked`.

   `credentials_expiry` answers the same. There is nothing to rotate by hand: refresh rotates
   the tokens. Re-linking (the same button) replaces a broken link.
7. **Unlink.** Console: **Unlink** (passkey-bound to the instance). It calls
   `dash.cloudflare.com/oauth2/revoke` and deletes `link:{instance}`. Resources pigeon made stay
   as they are and are listed so the person can remove them. Removing them is a separate,
   dry-run-first step (`instance_teardown`), and only works while still linked. The person can
   also revoke from Cloudflare: Profile → Access Management → Connected Applications. pigeon
   then sees `refresh failed` and says so.

**Fallback: an account-owned API token** made by the person in the noisypigeon account. It has
the same permissions, limited to zone `noisypigeon.com`, with an expiry. The person enters it on
the Access tab as slot `CF_LINK_NOISYPIGEON`, bound in `wrangler.toml`.
- Its `check_secret` calls `GET /accounts/{id}/tokens/verify` and makes one read of each kind.
- Its expiry comes from `verify`'s `expires_on`.
- To rotate: roll the token in the dashboard, then set the new value.
- The fallback is used only if phase 0 shows that OAuth can't grant the noisypigeon account.

**Open, for phase 0:**
1. The person's login is a member of both accounts (their answer, 2026-11-21), so the client is
   **private**, owned by pigeon's account. The docs say private clients "can only be authorized
   by members of the parent account". Phase 0 checks that the consent screen then offers the
   noisypigeon account. If it doesn't, the choice is:
   - make the client public: permanent, needs a logo, URL and a TXT record on pigeon.dev (pigeon
     can add the record itself);
   - own the client in the noisypigeon account: the person registers it there;
   - fall back to a token.
2. Access token lifetime, refresh token lifetime and rotation, read from a real authorization.
3. The exact scope IDs, read from `GET /oauth/scopes`.

**Answered, 2026-11-22, from the real link to noisypigeon:**
1. The private client works: the person's consent screen offered the noisypigeon account, and
   the grant names it (account `45017763…`, "noisypigeon").
2. The access token lives **one hour** (`expires_in` 3600). A refresh token comes only when the
   authorization request asks for **`offline_access`**: the client's `refresh_token` grant is not
   enough, and the first link had no refresh token. `authorize_url` now always adds it. A refresh
   works (`linked_check {refresh: true}` at 02:05 UTC gave a new one-hour token) and the answer's
   refresh token replaces the old one, which is kept before the access token is used. Cloudflare
   does not report the refresh token's own lifetime; until a refusal shows it, a link that is not
   used for weeks may need linking again, which `linked_status` shows as "refresh failed".
3. The scope IDs: `account-settings.read`, `zone.read`, `dns.write`, `workers-scripts.write`,
   `workers-routes.write`, `workers-r2.write`, plus `offline_access`.

### 3. Serving: from the R2 bucket in the noisypigeon account, through a Worker there

The bucket `noisypigeon-site` is in the noisypigeon account, EU jurisdiction (header
`cf-r2-jurisdiction: eu`) like the old one.

**The bucket is named after the instance, like its Worker** (the person, 2026-11-21:
`noisypigeon-site`, or `somesubdomain-site`).
- The name is the instance's name plus `-site`. The instance's name is its zone without the TLD,
  or its subdomain label.
- So adding or removing a zone or subdomain later never renames the bucket. An instance's sites,
  zone and subdomain all share it.
- The rule is code (`record::bucket_for`, checked by validation). The name is stored in the
  record.
- Another name needs `bucket_override: true`, which the person sees in the diff they approve.
- No two instances may share a bucket.

**One Worker per instance** (the person, 2026-11-21: "They should share the worker. It either has
a dedicated zone, a subdomain, or both."). The instance's zone (`noisypigeon.com`), its pigeon.dev
subdomain (`noisypigeon.pigeon.dev`) or both are served by the same Worker in the instance's
account, reading the same bucket.
- The Worker is named after the instance: its zone without the TLD (`noisypigeon`), or its
  subdomain label when it has no zone.
- The rule is code (`record::worker_for`); the name is stored in the record (`worker`).
- When the zone and the subdomain label disagree, validation asks the person to choose: make them
  the same, or set `worker_override`.
- Validation refuses the platform's own Workers (`pigeon-platform`) and a name another instance
  uses. A Worker in the same account, `noisypigeon`,
serves it through an R2 binding (`{"type":"r2_bucket", "jurisdiction":"eu"}` in the upload's
metadata). pigeon uploads the Worker: one small JS module embedded in the platform.

**Layout, for atomic deploys and rollback:**
- `blobs/{sha256}`: each file's content, written once and shared by every deploy. The 28 MB of
  images go up once.
- `deploys/{commit}.json`: the manifest, path → `{blob, content_type}`.
- `live`: the commit being served.

The Worker:
- reads `live`, then the manifest;
- keeps both for the isolate, rechecking `live` every 30 s;
- maps `/x` and `/x/` to `x/index.html`;
- serves the blob with long cache headers on hashed assets;
- answers the build's `404.html` with status 404.

A deploy writes the blobs that are missing, then the manifest, then flips `live`. A rollback
flips `live` back. Deploys older than the last 20 are pruned, with blobs no remaining manifest
names.

**Uploads.** Uploads use R2's REST object API (`PUT/GET/DELETE …/r2/buckets/{b}/objects/{key}`)
with the linked account's OAuth access token.
- The S3 API's keys are derived from an API token's id and value, so they don't come from OAuth.
- A first deploy is about 170 calls: well inside the API's 1,200 requests per 5 minutes. Later
  deploys are a handful.

The Worker, not the bucket, holds the hostnames, so the bucket gets no R2 custom domain.

**Why not an R2 custom domain on the new bucket, as the old site does?**
- A move from the old bucket's custom domains to the new bucket's would delete and re-create the
  apex's DNS record and certificate.
- R2 alone serves `/x/` only by writing extra copies.
- It has no atomic deploy or rollback.

**Targets:**

| Target | Hostname | How | DNS |
| --- | --- | --- | --- |
| preview | `noisypigeon.<sub>.workers.dev` | per-script workers.dev | none |
| live | `noisypigeon.com` | a **Worker route** `noisypigeon.com/*` at cutover | none: the apex record stays the old R2 custom domain's, and the route runs before it |
| pigeon.dev subdomain (optional, later) | e.g. `noisypigeon.pigeon.dev` | below | in the platform account |

**Why a route at cutover.**
- "Routes take precedence over Custom Domains." A route's Worker runs before the origin.
- Adding a route on the hostname the old R2 custom domain serves moves traffic in one call.
  Deleting it moves traffic back in one call. The old bucket, its custom domains, DNS and the
  Proton records are untouched.
- The docs don't state outright that a route intercepts an **R2** custom domain (only forum
  reports do), so phase 0 tests it on a throwaway hostname in pigeon's own zone.
- The `www` → apex redirect runs first, in the dynamic redirect phase. The `/` → `/index.html`
  rewrite also runs before the Worker, and the Worker serves `/index.html` either way.

Once the person is happy, the route can later become a Workers Custom Domain on the Worker,
replacing the old R2 custom domain. That is a DNS change in the noisypigeon account and gets its
own go-ahead.

**The pigeon.dev subdomain.** The hostname is configured in the platform account, and the
content stays in the noisypigeon bucket.
- **Ruled out: the bucket bound to a platform-account Worker.** An R2 binding names a bucket in
  the Worker's own account.
- **Ruled out: the platform reading objects with the linked credentials on every request.**
  - Each page view would be an authenticated api.cloudflare.com call, against the account-wide
    limit of 1,200 requests per 5 minutes.
  - The OAuth token would sit on the hot path.
  - S3 keys don't come from OAuth.
- **Ruled out: an R2 custom domain on the bucket.** It must be on a zone in the bucket's account,
  and `pigeon.dev` isn't in the noisypigeon account.
- **Chosen: pigeon-platform serves the hostname by host, as it serves the apex.**
  - It sets up a Workers Custom Domain with `custom_domain::attach` in pigeon's own zone, so
    Cloudflare makes the DNS record in the platform account.
  - It fetches the same path from the noisypigeon Worker's workers.dev URL and keeps the answer
    in the platform's edge cache (`caches.default`, keyed by commit) for an hour.
  - No credential is involved: the noisypigeon Worker is public, like the site.
  - The cost is one extra hop on a cache miss.

**Built for one host only.** The build sets Zola's `base_url` to `https://noisypigeon.com`. On the
preview and the subdomain, absolute links therefore point at noisypigeon.com. That is fine for
checking content before the cutover; a per-target `base_url` can come later.

### 4. Build: a CI job with Zola, fed by the GitHub App (replaced 2026-11-22: see the Log)

- **Trigger.** A push to `main` on `noisypigeon/pigeon` reaches
  `https://platform.pigeon.dev/github/hook`.
- **Check.** pigeon checks `X-Hub-Signature-256` (HMAC-SHA256, constant time) and answers within
  10 s. It keeps `X-GitHub-Delivery` to refuse repeats. It accepts only the declared
  installation id and repository.
- **Build job.** The `sites` capability submits CI job `site-build` (Containers connector) for
  `{instance: noisypigeon, site: blog, repo, commit}`:
  - The job runs on a new image `pigeon-ci-site`: the base image plus Zola 0.23.6,
    checksum-pinned as noisypigeon's Dockerfile pins it.
  - It fetches the commit with full history (the template is read at a git tag). The fetch
    goes through a new git proxy route for GitHub, which adds a 1-hour installation token
    limited to that one repository and `contents: read`. The repository is public today, but
    the job then doesn't depend on that, and the job never holds the token.
  - It runs the repository's own build: `pigeon sites build workloads/noisypigeon.com/blog`.
  - It uploads `public/` as a tarball to `PUT https://proxy.pigeon.dev/sites/build`, with its
    job token. The grant names the instance, site and commit. pigeon unpacks it in the Worker and
    writes blobs and the manifest to the noisypigeon bucket. The job never holds a Cloudflare
    credential.
- **Deploy.** The platform deploys the build to each target that is on. On GitHub it creates a
  deployment (`environment: noisypigeon.com`) and deployment statuses, with `environment_url`
  and a `log_url` to the console.
- **Why a job and not the Worker.** A Rust build of the harness plus Zola doesn't fit in a
  Worker. noisypigeon's build is its repository's contract, so pigeon runs it rather than
  copying it.
- **Policy.** `site-build` and its image land as gate changes (`ci/`, `.pigeon/ci.toml`),
  approved by the delegate under `gate-changes`. No new rule.

### 5. The GitHub App

- **Owner and visibility.** Owner `pigeon-security`, named `pigeon`. Visibility **Any account**,
  because a private app can be installed only on its owner's account, and `noisypigeon/pigeon`
  belongs to the user `noisypigeon`. Installation is still the repository owner's choice, and
  pigeon accepts webhooks only from declared installation ids.
- **Installation.** On `noisypigeon` → **Only select repositories** → `pigeon`.
- **Permissions:**
  - Metadata: read.
  - Contents: read, for fetch, tarball, file reads and the `push` event.
  - Deployments: write, for deployments and statuses.
  - Nothing else.
- **Events.** `push`; `installation` and `installation_repositories` are app events and are
  recorded.
- **Webhook.** URL `https://platform.pigeon.dev/github/hook`. The secret is
  `GITHUB_APP_WEBHOOK_SECRET`, generated by the person on GitHub and entered on the Access tab.
- **Private key.** `GITHUB_APP_PRIVATE_KEY`, the downloaded PEM (PKCS#1, about 1.7 KB; the
  Secrets Store takes up to 64 KiB). Core wraps it to PKCS#8 for WebCrypto. App id, client id
  and installation id are not secret. The App id and client id are platform-wide and stay in
  `settings.rs`; each instance's installation id is data in its record.
- **Expiry.** App keys don't expire. The Access tab shows "never". To rotate, add a second key on
  GitHub, set it, then delete the old one.
- **Auth.**
  - A JWT (RS256, `iat` −60 s, `exp` +9 min, `iss` the client id) buys an installation token.
  - Each use gets a token narrowed to the one repository and the permissions it needs, lasting
    at most an hour.
  - Tokens stay in memory only and are cached per isolate until 5 minutes before expiry.
- **Mirror.** `GITHUB_MIRROR_TOKEN` (the PAT pushing pigeon-security/pigeon) is unchanged.
  Moving it to the App (installed on pigeon-security/pigeon with Contents: write) is possible
  later, but out of scope.

### 6. GitHub through its MCP server behind the portal, with REST only where MCP can't

The person's answer: talk to GitHub over MCP. The platform is an MCP client of the portal
(`https://mcp.pigeon.dev/mcp`). It reaches GitHub's remote MCP server
(`https://api.githubcopilot.com/mcp/`) as portal server `github`, and uses GitHub's REST API
only where the MCP server has no tool.

**How the platform gets through.** The constraints, all from the docs:
- A headless client authenticates to the portal with an Access service token. Its
  `CF-Access-Client-Id`/`-Secret` become slots `MCP_PORTAL_CLIENT_ID` and
  `MCP_PORTAL_CLIENT_SECRET`, owned by the `github` capability, as the Buildkite planner advised.
  The token needs a Service Auth policy on the portal **and** on the server.
- A service-token session reaches only servers with `on_behalf: false`. Those act with the
  server's admin credential.
- An OAuth server set up by hand needs per-user auth, and GitHub's MCP has no dynamic client
  registration. So the platform's server is `bearer`.
- GitHub's MCP accepts GitHub App installation tokens. An installation token lasts an hour.

So, every 50 minutes and before first use, the platform:
1. mints an installation token, limited to the declared repositories with `contents: read` and
   `metadata: read`;
2. writes it to the server's `auth_credentials` with the server update call
   (`PUT …/access/ai-controls/mcp/servers/github`), after a GET, sending the server back whole.

It never writes the portal's server mapping, as the portal planner warned. The server also sends
`X-MCP-Readonly: true` and `X-MCP-Toolsets: repos`, as fixed headers in the bearer's
`{"headers": …}` form. The minted token is never stored by pigeon, but Cloudflare holds it for
its hour.

`pigeon_core::mcp` gains a small Streamable-HTTP client:
- `initialize`, then `tools/call`;
- it carries `Mcp-Session-Id`;
- a 50 s deadline under the portal's 60 s cap;
- it answers the tool's content as JSON.

Tool names through the portal are `github_<tool>`.

**What goes over MCP:**

| Need | MCP tool (via the portal as `github_…`) |
| --- | --- |
| The commit a push names, for the deploy record (message, author, time) | `get_commit` |
| The repository section's recent commits and branches | `list_commits`, `list_branches` |
| Read the site's declared config (`workloads/noisypigeon.com/blog/config.yaml`) before a build, to check it still names a static site | `get_file_contents` |
| Agents' questions about the repository, through the platform | the same tools |

**What stays on REST, and why** (the MCP server has no tool for any of these):

| Need | REST call | Why not MCP |
| --- | --- | --- |
| Authenticate as the App and mint installation tokens, including the one the MCP server uses | `POST /app/installations/{id}/access_tokens` with an RS256 JWT | MCP needs a token before it can be called |
| Receive pushes | the App's webhook, inbound to `/github/hook` | MCP has no events |
| Record deploys on GitHub | `POST …/deployments`, `POST …/deployments/{id}/statuses` | the MCP server has no deployment tools |
| Fetch the commit's whole tree for the build | git over HTTPS through pigeon's git proxy (a git protocol fetch, not REST) | one file per MCP call, under a 60 s cap, can't carry a repository and its history |

If the portal or the MCP call fails, deploys still happen. The record then says "commit details
unavailable (MCP: <why>)". MCP is never on the path that serves or deploys the site.

**People's sessions**, recommended and possible any time after 3b: a second portal server,
`github-you`, with OAuth set up by hand using the GitHub App's client id and secret, and
per-user auth. Each person then reaches GitHub as themselves, within the App's permissions,
through `mcp__Pigeon_MCP__github-you_*`. This is a dashboard step for the person. These tools are
not pre-approved, so the generator and the policy are unchanged.

### 7. Instances: the shape in code, the values as data

An **instance** is a named unit of what pigeon runs. Code declares its *shape*: a Rust type,
validation and how each part is used. Every *value* is data, kept per instance in the Directory:
account, zones, hostnames, subdomains, bucket and Worker names, routes, repository and
installation id. Nothing per-account goes in `settings.rs` or anywhere else in code.

```rust
// pigeon_capabilities::instances: the shape only.
pub struct Instance {
    pub id: String,                // "pigeon-security/platform", "noisypigeon"
    pub account: AccountRef,       // Home | Linked { account_id }
    pub zones: Vec<String>,        // ["noisypigeon.com"]
    pub repo: Option<RepoRef>,     // Artifacts { namespace, repo } | GitHub { full_name, installation }
    pub sites: Vec<SiteRecord>,    // name, bucket, worker, build, targets
    pub version: u64,              // bumped on every approved change
}
pub enum Target {
    Preview,                                   // the site Worker's workers.dev
    Zone { hostname: String, route: String },  // e.g. noisypigeon.com, noisypigeon.com/*
    Subdomain { label: String },               // under the platform's zone: "noisypigeon" → noisypigeon.pigeon.dev
}
```

**Validation** (pure, in the smoke suite):
- Names are lowercase DNS labels.
- A bucket or Worker pigeon creates must start with `pigeon-`.
- A zone hostname must be in one of the instance's zones.
- A subdomain label must be a single label, unused by any other instance. It may not be one the
  platform serves itself: `platform`, `proxy`, `mcp` or `www`, read from the hostnames the
  footprints declare.
- A route pattern must be on the target's hostname.

**How values change:**
1. `instance_propose {instance, record}` is an MCP tool. It validates the record and stores it
   as a draft, with the diff against the current record. It changes nothing.
2. The draft appears on the console's **Approvals** tab. The person approves it with a passkey
   bound to the draft's digest, as secrets writes are bound to the value's hash.
3. Only then does it become the record, with `version + 1`. Drafts expire after 7 days.
4. The console's Instance tab has an **Edit** form that makes the same draft.

So an agent can suggest that pigeon may touch something, but only the person can make it so.
This replaces "a landing changes what pigeon may touch" with "the person's passkey does", with
no new policy rule.

**The first records, proposed in phase 2 and approved by the person:**
- `pigeon-security/platform`:
  - the home account, zone `pigeon.dev`;
  - repository Artifacts `pigeon-security/pigeon`;
  - no sites of its own beyond the apex page, which stays code (`Site`) because it is the
    platform's.
  - Its `Site`, `PublicHost`, `Source` and `Land` keep their settings. They are the platform
    itself, not an instance's data.
- `noisypigeon`:
  - the linked account (its id filled in by the Link flow), zone `noisypigeon.com`;
  - repository GitHub `noisypigeon/pigeon` (installation id from phase 3b);
  - site `blog`:
    - bucket `noisypigeon-site` (EU) and Worker `noisypigeon`;
    - build `workloads/noisypigeon.com/blog`;
    - targets: Preview; Zone `noisypigeon.com` with route `noisypigeon.com/*`, off until the
      cutover; Subdomain `noisypigeon`, which gives `noisypigeon.pigeon.dev`.

**Console.** The header gains an instance switcher: two entries, the choice kept in the URL as
`?i=noisypigeon` and in local storage. The four tabs stay. One tab is added:

- **Instance** (`/instance`), with these sections:
  - **Account**: id, link status, scopes, expiry, Link/Unlink.
  - **Zones**: name, status, record count; pigeon-made resources flagged.
  - **Sites**: each target's hostname, live version, Deploy/Roll back.
  - **Repository**: provider, default branch, App installation.
  - **Deploys**: the last 20, each with commit, time, outcome, target and log.
- **Architecture** shows the chosen instance's model.
- **Access** and **Cost** stay platform-wide. Access gains link and App rows.

Speed, as in console-hardening:
- Each section loads alone, with a 12 s timeout and Retry.
- Zones and Sites are served from a Directory snapshot (fresh for 5 min), refreshed by the page
  with `fresh=1`.
- Deploys are Directory reads (well under 1 s).
- The noisypigeon account is never called on the request path of another instance's tab.

### 8. Architecture

- `Footprint` gains `account: Option<String>` (None means home) on hostnames, Workers and
  resources, and `zone` per hostname.
- `Model` gains `accounts: [{id, name, linked_by, zones}]`.
- The diagram draws one account box per account: pigeon's as today, noisypigeon's with its zone,
  the Worker `noisypigeon`, its route or preview, and a dashed "not pigeon's" group for the
  old bucket and custom domains it only reads.
- It also draws the noisypigeon R2 bucket `noisypigeon-site` inside that account, the GitHub App
  and the `github` portal server as external edges, the `site-build` job, and the pigeon.dev
  subdomain's edge from pigeon-platform to the noisypigeon Worker.
- The code-derived model (from the sources and `wrangler.toml`) is still built once per isolate,
  and the public apex page and its `/architecture.json` stay exactly that. Each deploy's commit
  check holds, and no account's data is published on pigeon.dev.
- The console tab and `architecture_describe` add an **instances layer**, drawn from the
  instance records: accounts, zones, sites, targets, subdomains and repositories. It is one
  Directory read, cached per record `version`, so the drawing changes the moment a draft is
  approved and costs nothing between changes. It never comes from live Cloudflare calls.

### 9. Safety rules for anything outside pigeon's own account

- Each mutating operation has a dry run, `instance_plan {instance}`. It reads the live state
  (Workers, routes, custom domains on declared hostnames) and answers the list of changes with
  before and after, and a digest of the plan.
- `instance_apply` takes the digest and refuses if the live state no longer gives the same plan.
  Applying twice is a no-op the second time.
- Content deploys (a new Worker version of `noisypigeon`) are automatic on push. They
  change only pigeon's own Worker, and only once it exists.
- Creating the Worker, enabling its workers.dev preview, the route (cutover) and its removal
  (rollback) are console actions with a passkey bound to the plan digest, done only with the
  person's go-ahead in the session.
- Nothing ever deletes or edits DNS records, R2 buckets, R2 custom domains or rules in a linked
  account. The core blocks for them are read-only by type: no write function exists for a
  linked `Account`.

## Phases

Each phase is one landing (or none), checked live before the next starts. Executors are AI
Gateway models (`models_submit`). Each gets one whole file or function set, with the interface
and the surrounding code; I read every line returned, run fmt, clippy and the smoke suite
myself, and land.

| Phase | What | Live change | Who |
| --- | --- | --- | --- |
| 0 | Facts and tests: OAuth consent offers noisypigeon? token lifetimes and scope IDs; route-over-R2-custom-domain test on a throwaway `r2test.pigeon.dev` (pigeon's zone, removed after); run noisypigeon's build here and record its output path, time and size; noisypigeon.com's live headers | test resources in pigeon's account only | person (consent, network host), me |
| 1 | Core: `Account` credential kind; `cloudflare_oauth` (authorize URL, exchange, refresh, revoke); `r2` (bucket ensure with jurisdiction, REST objects put/get/list/delete, the blob/manifest/live layout as pure functions); `workers` (module upload with an R2 binding, routes, workers.dev, custom domain read); `github_app` (JWT, installation token, webhook verify, tarball, deployments); WebCrypto RS256 sign and AES-GCM. Smoke scenarios: OAuth exchange/refresh with recorded answers, GitHub webhook HMAC, the plan digest refusing drift | none | executors (sonnet: r2, workers, github_app; haiku: crypto wrappers), me |
| 2 | `instances` capability: the record type and validation, `instance_propose`, drafts on Approvals with passkey approval, the Instance tab and Edit form, the switcher, the architecture's instances layer; `linked` capability: `linked_setup` (LINK_KEY, OAuth client), Link/Unlink, callback, refresh, expiry rows | console shows both instances; noisypigeon "not linked" | executors (sonnet: console JS, architecture render), me |
| 2b | **Person:** register or confirm the OAuth client; press Link; consent to the noisypigeon account | link stored; inventory of noisypigeon.com shows | person |
| 3 | GitHub App: `github` capability, `/github/hook`, slots, deployments; the GitHub git proxy route | none until the App exists | executors, me |
| 3b | **Person:** create the App, set its key and webhook secret on the Access tab, install it on noisypigeon/pigeon | push events recorded | person |
| 4 | `sites`: `site-build` job and `pigeon-ci-site` image (gate change, delegate), `/sites/build` upload, the site Worker's JS, deploy (blobs, manifest, `live`), rollback, prune, deploy records | with the person's go-ahead: bucket `noisypigeon-site` and Worker `noisypigeon` in the noisypigeon account, workers.dev preview | executors, me |
| 5 | **Cutover** of noisypigeon.com (below) | route `noisypigeon.com/*` | person's go-ahead, then me |
| 5b | The pigeon.dev subdomain from the record's `Subdomain` targets: pigeon-platform serves any approved label by host (no hostname in code), attaches its custom domain in pigeon.dev on approval, and proxies to the instance's Worker with edge cache | `noisypigeon.pigeon.dev` | executors, me; person approves the record |
| 6 | The platform calls GitHub's MCP through the portal (decision 6): `pigeon_core::mcp` client, the server's credential rotation, the deploy record and the console's repository section over MCP | portal server `github`, service token (the person's dashboard steps) | executors, me; person (dashboard) |

Part 1 of decision 6 (people's sessions reach GitHub through the portal) is a dashboard step for
the person, possible any time after 3b.

## Cutover of noisypigeon.com, and rollback

Before:
1. The preview (`workers.dev`) serves the current commit's build.
2. A page-by-page comparison of status, content type and body hash passes, for every path in the
   build's file list, between the preview and live `https://noisypigeon.com`. This is a tool,
   `site_compare`, run by me, and its report goes to the person. It needs this session's network
   to allow `noisypigeon.com` and `www.noisypigeon.com` (and `*.workers.dev` for the preview; the
   exact host is proposed in phase 4).
3. The old publisher's last build and pigeon's are of the same commit.

Step (one call, with the person's go-ahead in the session and their passkey on the plan):
`instance_apply` with the plan "create route `noisypigeon.com/*` → `noisypigeon`".

Check, within a minute:
- `https://noisypigeon.com/` and five deep paths answer 200 with `X-Served-By: noisypigeon`.
- `www.noisypigeon.com` still answers 301 to the apex.
- MX and TXT records are unchanged: an inventory diff before and after shows no change.

Rollback (one call, no passkey wait, because it restores the earlier state): delete the route
pigeon made. The R2 custom domain serves the old bucket again at once. A person can do the same
in the dashboard: noisypigeon account → Workers → `noisypigeon` → Domains & Routes → delete
the route. A bad deploy after the cutover is rolled back by deploying the previous Worker
version, from the console's Sites section.

Later, and only if the person asks: retire the old bucket's custom domains and the old publisher
in noisypigeon/pigeon. That repository's own change, not pigeon's.

## The person's one-time steps

1. **Phase 0.**
   - Answered on 2026-11-21: ownership, serving, GitHub over MCP, and that the login is a
     member of both accounts. The first pigeon.dev subdomain is `noisypigeon` (2026-11-21).
   - On the consent screen in 2b, say whether the noisypigeon account is offered.
   - Approve the network hosts `noisypigeon.com` and `www.noisypigeon.com` for this session when
     asked (read-only checks).
2. **OAuth client, only if pigeon can't register it by API.** pigeon account → Manage Account →
   OAuth clients → Create:
   - name `pigeon`;
   - redirect `https://platform.pigeon.dev/linked/callback`;
   - grant types authorization code and refresh token;
   - the scopes phase 0 lists.

   Enter its secret on Access → Secrets and credentials as `CF_OAUTH_CLIENT_SECRET`.
3. **Link.** Console → switch to `noisypigeon` → Instance → Link Cloudflare account. On
   Cloudflare, pick the noisypigeon account and allow.
4. **GitHub App.** github.com/organizations/pigeon-security/settings/apps → New GitHub App:
   - Name `pigeon`; homepage `https://pigeon.dev`.
   - Webhook URL `https://platform.pigeon.dev/github/hook`. Generate a secret and enter it on the
     Access tab as `GITHUB_APP_WEBHOOK_SECRET`; never paste it in chat.
   - Permissions: Metadata read, Contents read, Deployments write. Events: Push.
   - Where can it be installed: Any account.
   - Create, then generate a private key and upload the `.pem`'s contents on the Access tab as
     `GITHUB_APP_PRIVATE_KEY`. Delete the downloaded file.
   - Install: Install App → noisypigeon → Only select repositories → `pigeon`. Tell the session
     the App id and the installation id (both in the URL; neither is secret).
5. **GitHub MCP for the platform (phase 6).** In Zero Trust:
   - Access → Service credentials → Create service token `pigeon-platform-mcp`. Enter its client
     id and secret on the console's Access tab as `MCP_PORTAL_CLIENT_ID` and
     `MCP_PORTAL_CLIENT_SECRET`.
   - MCP servers → Add:
     - server id `github`;
     - URL `https://api.githubcopilot.com/mcp/`;
     - auth Bearer, any placeholder: pigeon replaces it within a minute;
     - policy: Service Auth, that token.
   - Portal `pigeon` → add server `github` with Require user auth **off**. Add a Service Auth
     policy for the same token on the portal.
   - Tell the session when done. pigeon then sets the bearer and syncs the server.
6. **Optional: GitHub in your own sessions through the portal.**
   - On the App, add the callback
     `https://oauth-callbacks.cloudflareaccess.com/cdn-cgi/access/outbound-oauth-callback`, then
     generate a client secret.
   - In Zero Trust → MCP servers → Add:
     - name and server id `github-you`;
     - URL `https://api.githubcopilot.com/mcp/`;
     - OAuth, manual, with the App's client id and secret;
     - policy: your email.
   - Add `github-you` to portal `pigeon`, with Require user auth on.
7. **Cutover.** Say "go" for the route in the session that does it. Say "roll back" at any time.

## What I need from the network

This session (not the platform; the Worker's outbound calls aren't limited) needs
`noisypigeon.com`, `www.noisypigeon.com` and the preview's `workers.dev` host for the live
checks. I'll propose them with the environment tools when phase 0 or 4 reaches them, not before.
`api.cloudflare.com`, `dash.cloudflare.com` and `api.github.com` are reached by the platform,
not by this session.

## Open items (2026-11-22, handoff)

This planner session is archived, and its work passes to the AI Gateway planners and executors.
Each item gives its state and the next step.

1. **Linked accounts**: done and live. `linked_setup` ran (OAuth client `28a843c4…`), with
   `offline_access` so links get refresh tokens. noisypigeon is linked to account
   `45017763d00a05ee9ff3aaaa15f05145`. `linked_check` and `linked_status` are read-only.
   - Next: none. If a link stops refreshing, run `linked_check` first.
2. **OAuth scopes** (`linked_scopes`, landed as `8b1d517`): read-only; it lists Cloudflare's
   404 OAuth scopes.
   - All nine needs of the data Worker exist as scopes: Pipelines, R2 Data Catalog (plus R2
     SQL Read), Email Routing, Zone Settings, Workers AI, API Tokens, Vectorize, Queues and D1.
     None needs a token made by hand.
   - API Tokens exists only as Write (account and user). It is the riskiest scope.
   - Next: no scope is added to the client yet. Adding scopes for data-enabled links, and a
     re-link prompt for existing links, is data-worker.md need 2. The person approves each
     scope, and API Tokens Write separately.
3. **Instances** (plan/apply, `instance_subdomains`): done. Plans observe, dry-run, bind the
   digest to the person's passkey and re-check on apply. They touch only what pigeon made and
   never delete.
   - Next: the record shape for `data` (data-worker.md need 1). Nothing is built yet.
4. **noisypigeon**: done and live.
   - Bucket `noisypigeon-site` (eu) and Worker `noisypigeon` exist.
   - noisypigeon.com and www are Workers Custom Domains, with www redirecting to the apex.
   - noisypigeon.pigeon.dev is served by pigeon-sites.
   - Mail on the zone was removed on purpose by the person.
   - Next: none. Any DNS or zone change needs the person's go-ahead for that step.
5. **Sites** (`site_deploy` in pigeon-sites, landed): it reads the GitHub tarball, renders, and
   writes blobs, then the manifest, then `live`.
   - Next: a console view of deploys, and pruning old `deploys/*.json` and unreferenced blobs.
     Neither is started.
6. **GitHub App** `pigeon-security` (App 5869577, installation 184534958, secrets in the
   Secrets Store): done. `/github/hook` records pushes at `github:deploy:{instance}`, and the
   minute cron's `run_pending` deploys them (3-minute lease, 5 tries).
7. **Deploy on push**: built but not yet confirmed on a real push. `github_status` shows
   `latest_deploy` null; the last push was 779a1ad, before the cron.
   - Next: after the person's next push to noisypigeon/noisypigeon, or a redelivery from the
     App's settings, check that `github_status.latest_deploy` names the new commit, and that
     `x-pigeon-commit` on https://noisypigeon.com matches. If it doesn't, read the pending entry
     and the cron log.
8. **Two more instances, later**: not started. Each follows noisypigeon's path:
   1. approve the record;
   2. link;
   3. plan and apply (bucket, Worker, domains);
   4. install the App on the repository;
   5. `site_deploy`.
   One is likely `willowgraysen` (data-worker.md follow-up 1).
9. **Housekeeping**: the branches `claude/linked-link-2`, `claude/linked-link-3` and
   `claude/github-bindings` were pushed to GitHub by mistake. The person can delete them there.

## Log

- 2026-11-22, 05:40: **noisypigeon.pigeon.dev serves** (phase 5b), from the person's phone.
  - **In pigeon-sites, not core.** The split's end state gives sites the instance hostnames, so
    core serves no site traffic. A Subdomain target is an `AttachSubdomain` plan step: a Workers
    Custom Domain `{label}.{zone}` on `pigeon-sites`, in the platform's account, with the
    person's passkey; the instance Worker's workers.dev address is kept as its upstream
    (`subdomain:{host}`), listed by core's read-only `instance_subdomains`. pigeon-sites matches
    such a host before the contract, asks core for the map as itself (`worker:pigeon-sites`,
    kept five minutes) and passes GET and HEAD through, cached under the upstream's own
    Cache-Control. No credential is on that path. Decision 3's "pigeon-platform serves it by
    host" is replaced by this.
  - **Push deploys run from a cron.** The hook only records a pending deploy; core's minute cron
    (`github::run_pending`) runs it through `site_deploy`, with a three-minute lease and up to
    five tries, so a redeploy or `waitUntil`'s limit (which lost the first test push, 779a1ad,
    deployed by hand instead) cannot lose one. Confirming it on a real push is still open.
  - **Incident.** The cron's first core deploy failed in the deploy job's allowlist dry run: the
    proxy allowed `PUT …/workers/scripts/{script}/schedules` (38e3089) but the CI stand-in
    (`ci/deploy/standin.mjs`) did not answer it. Production was untouched (the dry run comes
    first) and the Lander rolled core back to its last good build each time. The stand-in learned
    the call (4692601); a new proxy allowlist entry needs its stand-in route in the same landing.

- 2026-11-22, later: **noisypigeon.com serves from pigeon.** In order:
  - **Outage found.** noisypigeon.com and www answered Cloudflare error 1000. The zone had moved
    accounts, and the old R2 custom domains had become plain proxied A/AAAA records to
    Cloudflare's own IPs; R2 was not even enabled on the noisypigeon account (10042 until the
    person enabled it at 02:21), so `blog-noisypigeon-com` is not there. The site had been down
    since the move, not because of pigeon. The person emptied the zone on purpose, mail records
    included (mail is redone later, separately).
  - **Resources, by plan.** `instance_plan` / `instance_apply` (instances/plan.rs, pure; a ledger
    per instance of what pigeon made; anything else with pigeon's names is a conflict; nothing
    is deleted; the person's passkey binds the plan's digest). They made bucket
    `noisypigeon-site` (eu), Worker `noisypigeon` (instances/site_worker.js: `live` →
    `deploys/{commit}.json` → `blobs/{sha}`, www → apex 301) and its workers.dev preview.
  - **Custom domains instead of routes.** With the zone empty there is nothing proxied to route
    over, so the cutover is `Target::Domain`: a Workers Custom Domain per hostname, attached by
    the plan; Cloudflare makes and owns the DNS record and certificate. The record (v4) has
    domain targets noisypigeon.com and www.noisypigeon.com; the zone/route target is gone.
  - **Deploys live in pigeon-sites.** The person chose to wait for the sites Worker rather than
    deploy from core. `site_deploy` (src/sites/src/deploy.rs) reads the record through core's
    `dispatch`, the repository as one tarball (`pigeon_core::tarball`, pure; the App's
    installation token, or codeload for a public repository), sorts it
    (`pigeon_blog::repo::split`, pure), renders it (`pigeon_blog`) and writes the blobs the
    bucket lacks, the manifest and `live` with core's `linked` credential. The first deploy
    (2787e52, 186 files) took three calls: the portal drops a call after 60 s and a deploy only
    uploads what is missing, so each call got further.
  - **The GitHub App** is `pigeon-security` (App 5869577, client `Iv23liHG7HdcPPzrnNfn`, in the
    Worker's vars), installed on noisypigeon/noisypigeon only (installation 184534958, in the
    record). Its key and webhook secret are core's Secrets Store slots, bound once set; the key
    is checked by signing in as the App before it is kept and never leaves core: other Workers
    get an installation token narrowed to one repository (`credential {kind: "github"}`).
    `/github/hook` checks the signature first and accepts only the record's repository and
    installation. A push to `main` starts `site_deploy` through `wait_until`, for the person who
    linked the instance; `github_status` shows the latest push and deploy.
  - **Live.** 03:45 UTC: https://noisypigeon.com/ 200 (x-pigeon-commit 2787e52), www → apex 301.
  - Still to do: the console's view of deploys; pruning old deploys.

- 2026-11-22: linking works end to end. `linked_setup` registered the client (`28a843c4…`) and
  wrote its secret and `LINK_KEY` to the Secrets Store; their bindings landed after. The person
  approved both instance records and linked noisypigeon. `linked_check` (read-only: refresh,
  then the account and its zones) found the first grant had no refresh token; the fix asks for
  `offline_access`, and after linking again a refresh works (phase 0, item 2, above). The
  noisypigeon record is at version 2: repository noisypigeon/noisypigeon, build
  `.pigeon/site.toml`, account pinned. The blog moved there (commit `2787e52`): `.pigeon/site.toml`
  in the blog planner's `blog@1` schema, `content/`, `static/assets/`. With the blog planner:
  rendering runs in the platform Worker at deploy time (`pigeon_core::blog::render`, pure), so
  decision 4's CI job, Zola and `/sites/build` go; the deploy reads the repository through the
  GitHub App (git trees, changed blobs only) and keeps the template, its version and digest on
  each deploy record.

- 2026-11-22: phase 2b part 2, the Link flow, built. Capability `linked`:
  - `linked_setup` registers the OAuth client by API (scope IDs matched by name from
    `/oauth/scopes`). It writes `CF_OAUTH_CLIENT_SECRET` and `LINK_KEY` to the Secrets Store.
  - On the console: Link → Cloudflare consent → `/linked/callback`. The callback checks the
    state, exchanges the code and checks the account against the record. The tokens are kept
    AES-GCM-sealed in the Directory.
  - Unlink needs the passkey. `account_for` refreshes before use, storing the new tokens first.
    `linked_status` is read-only.
  - Departure: the two secrets' `wrangler.toml` bindings land *after* `linked_setup` has
    created them, because a binding to a missing secret could fail a deploy once main has
    moved. Until then the flow answers "not set up".
- 2026-11-22: phase 2b part 1, agent proposals.
  - `instance_propose` checks a record as the console does. It keeps the record as an
    `instance_draft` approval for the person the caller acts for, holding the payload, the base
    version and the diff.
  - The Approvals tab shows the draft's changes. Approving with a passkey applies the record
    inside the Directory's `approval.decide`, in the same call, only while the instance is still
    at the base version. Otherwise the draft expires as stale.
  - Built on the shared approvals table (tool-approvals, `7debe9f`).
- 2026-11-21: the person, through the parent: the bucket takes the instance's name too,
  `noisypigeon-site`. `bucket_for` now derives it from the instance (its Worker name), not
  from a hostname, and validation also refuses a bucket another instance uses.
- 2026-11-21: the person, through the parent: one Worker per instance, named after its zone
  without the TLD or its subdomain label: `noisypigeon`. `worker` moves from the site to the
  instance, with `record::worker_for`, `worker_override` and collision checks against the
  platform's Workers and other instances.
- 2026-11-21: the person, through the parent: a site's bucket is named after its hostname
  (`noisypigeon-site`). Validation checks the rule unless the record sets `bucket_override`.
  Writes to `/instance/put/*` now also need the console's signed-in session and the console's
  own origin, which closes the gap found after 2a. Open: whether the Worker follows the same
  rule.
- 2026-11-21: phase 2a built on `claude/instances`. Capability `instances`:
  - `record` is pure: the shape, validation, digest, diff and lookup of subdomains by host.
  - Records are kept in the Directory. A record changes through `/instance/put/{begin,finish}`,
    with the signed-in person's passkey bound to the record's hash, after they see the diff.
    The edit must be from the record's current version.
  - The console gains a fifth tab, **Instances**, with a switcher and a JSON editor.
  - `instances_list` is read-only.
  - Smoke scenario: `instances_are_data_a_person_approves`. The suite runs in 21 s.
  - Departure: agent proposals (`instance_propose`) wait for the shared approvals table (the
    tool-approvals plan, section 4). Until then only the person edits, on the console.
  - Executors: `claude-sonnet-5-5` for `record.rs` (one fix for a dependency the crate lacks)
    and `instances.js` (write paths moved to `/instance/` so they don't fall under the
    `/instances` tab route).
- 2026-11-21: the person, through the parent: per-account values such as subdomains are data,
  not code; the first subdomain is `noisypigeon`. Decision 7 was rewritten: the shape in code,
  the values in Directory records, changed by `instance_propose` and the person's passkey.
  Decisions 1 and 8 follow from it. Phase 5b (`noisypigeon.pigeon.dev`) is in scope.
- 2026-11-21: phase 1 built on `claude/linked-accounts`. New blocks in `pigeon_core`:
  - `cloudflare_oauth`, `github_app`, `r2` (REST only, with the blob/manifest/`live` layout),
    `workers` (upload, workers.dev, routes that never take or delete another Worker's) and
    `crypto` (AES-GCM through WebCrypto).
  - `Account.bearer`: a linked account's granted token, used by `Scoped` as it is and never
    revoked.
  - `cloudflare::call_with`: extra headers, for R2's jurisdiction.

  The smoke suite gained one scenario, `linked_account_github_app_and_site_layout`, and runs in
  17 s. Executors: four `claude-sonnet-5-5` jobs (oauth, github_app, r2, workers with crypto).
  All were used as returned, apart from two clippy fixes.

  The local run of noisypigeon's build, part of phase 0, was refused in this session (running a
  downloaded Zola binary on an external repository). The first `site-build` job in phase 4
  records the output path, time and size instead.
- 2026-11-21: the person's second answers, through the parent. GitHub over MCP (decision 6
  rewritten: the platform is an MCP client, with the REST exceptions listed); their login is a
  member of both accounts, so the OAuth client is private; phase 5b is left for later.
- 2026-11-21: the person's answers, through the parent. The platform account has no blog for
  now; the noisypigeon account holds the site's R2 bucket and DNS; a pigeon.dev subdomain may
  serve that bucket. Decisions 1 and 3 were rewritten: R2 serving through a Worker in the
  noisypigeon account, and the subdomain proxied by pigeon-platform. Static assets and
  `pigeon-builds` were dropped.
- 2026-11-21: design. The noisypigeon repository and its live serving were read; the Cloudflare,
  Cloudflare OAuth and GitHub docs checked; the portal and Buildkite planners consulted. The
  parent session made OAuth the primary link.

## Instance-scoped Source (read-only)

An approved instance may enable Source with `"source":{"state":"enabled"}`. This exposes
`source_resolve_revision` and `source_read_file` on its scoped MCP endpoint. An omitted or
disabled profile disables these tools for that instance, including generic `call`. The
existing `repo` binds the provider and repository. Calls take HEAD (the default), a branch,
or a 40-digit commit SHA. File reads also take a repository-relative regular file path,
resolve the commit first, and return at most 1 MiB inline as text or base64. Tags and revision
expressions are outside this first scope.

GitHub reads use a repository-limited, contents-read installation token when one is configured.
A repository without an installation can use the public GitHub API. Artifacts reads use a
temporary repository-specific read-only Git token and revoke it afterward. A linked Artifacts
account must match the ID approved in the record and grant the necessary provider permissions.

The shared repository module also provides Sites' existing GitHub archive flow. Publishing
can use its approved repository without enabling the Source tools. Repository setup, branch
writes, token issuance to callers, and landings keep their main-platform paths.

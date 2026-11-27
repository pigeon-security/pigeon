# The deploy proxy's revoked token, and what a forward's outcome records

Status: in progress (2026-11-27).

| Phase | What | State |
| --- | --- | --- |
| 1 | Deploy proxy (`/cf/`) renews a scoped token Cloudflare refuses | landing |
| 2 | Inline data store: a successful forward's delivery row | investigated: rows are written; no change |
| 3 | A failed forward records Cloudflare's reason (redacted) and the email status tool shows it | landing |
| 4 | Verify CI lost runners and `pigeon-proxy` 401s after the git-proxy fix | after landing |

## 1. The deploy proxy keeps a revoked token

The git proxy's fix (docs/plans/ci-container-starts.md) left the deploy proxy (`/cf/`,
`src/proxies/src/cf_proxy.rs`) with the same exposure: on the proxy Worker its account for
`Purpose::Deploy` holds a scoped `cf_deploy` token kept per isolate
(`src/proxies/src/cache.rs`); when core's token ledger restarts it revokes that token, and every
wrangler call through the isolate is answered 401 (after `send`'s 15 s of not-yet-active
retries) until the kept copy would have expired.

Unlike the git proxy, `send` passes Cloudflare's 401 on to wrangler as an answer, not an error,
so `with_renewal` (which looks at errors) is generalised:

- `with_renewal_if(account, forget, work, refused)` / `with_host_account_if`: `refused` says
  which results are Cloudflare refusing the token. `with_renewal` is `with_renewal_if` with
  "an error with status 401". Only an account holding a kept token (`bearer`) is renewed, at
  most once; the second attempt's result is answered as it is.
- `cf_proxy::refused_answer`: a 401 answer or a 401 error.
- The forwarded call (token + `send`) runs through `with_host_account_if`; the container
  namespace lookup (`call_fresh`, errors) through `with_host_account`.
- Core's own host mints per request (no `bearer`), so it is unchanged; nothing in
  `src/capabilities` keeps a deploy token.

Smoke: `proxy::deploy_proxy_renews_a_refused_scoped_credential`.

## 2. "The inline data store doesn't record a successful forward"

Not reproduced; the rows are there. On noisypigeon (inline, in the site Worker) the data store
holds two Email Routing messages and two `email_deliveries` rows:

- `03a09436…` ("This is a test!", sent 07:45:03, received 07:45:24.072): `forward_failed`.
- `47e47d13…` ("Fwd: This is a test!", sent 07:45:49, received 07:46:10.094): `forwarded`.

A delivery row's `at` is the message's *received* time (`received_at`, equal to its sighting's
`seen_at`), not its `Date` header, so the 07:45:49 message's row reads 07:46:10; the email
status tool lists deliveries grouped by outcome without item ids, and `email_get` shows no
deliveries, which makes the row easy to miss.

The composition was also checked: `data_inline.js` calls the data build's
`Entrypoint.prototype.email` with `{ env, ctx }`, which worker-build 0.9's shim defines as
`exports.email.call(this, arg, this.env, this.ctx)`, and returns (so the runtime awaits) its
promise; a stand-in run under node records the row after the forward resolves. Nothing is cut
off after the forward. No change.

The first message's `forward_failed` is real, and its reason was thrown away; that is phase 3.

## 3. A failed forward discards Cloudflare's reason

`store::email` recorded every refusal as "Email Routing refused the forward".

- `mail::forward_error(message, to)`: "Email Routing refused the forward: <Cloudflare's
  message>", on one line, at most 160 characters, with the destination and any other word
  holding an `@` replaced by `[address]`. The message's content is never in it (the error is
  Cloudflare's).
- `datastore::status_queries`: each outcome's row carries `last_error`, the newest recorded
  reason, so the email status tool's `live.deliveries` says why the last forward failed. The
  tool passes the data Worker's answer on, so `src/capabilities/src/email.rs` is unchanged.

Smoke: `console::email::email_handler_stores_files_and_forwards` (redaction, cut, `last_error`).

### Reaching the instances

The data Worker is not deployed by a landing: an instance picks up a new build only when the
person runs and approves an instance plan (the data Worker artifact lane builds it; core, or
the inline composition, uploads it on apply). Phase 3 needs a plan and apply on each instance
with a data store: pigeon.dev (`pigeon-data`), noisypigeon (inline) and willowgraysen. Until
then they keep recording the fixed reason; nothing else changes.

## 4. Verification

See the landing report.

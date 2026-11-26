# Per-Worker caller keys

Status: **steps 0, 1, 2 and 5 done; step 3a landed (`950f461`), 3b and 3c landing one at a time; step 4 not started.** Steps 1,
2 and 5 are on main (step 2 as of `57960d8`: core binds the six `CALLER_KEY_<WORKER>`, each
Worker binds its own as `CALLER_KEY_OWN` and sends `X-Pigeon-Worker`). Step 0 is done live:
`workers_setup` reported all seven caller keys "already there; kept" (2026-11-25). Step 3 is
built on `claude/caller-keys-step3`: eight code commits on main at `57960d8`, then this doc's
(see step 3 below). Until step 3 lands, every Worker and core still sign with the shared
`CALLER_KEY`. Task K1 of the clean-up.

## Goal

One key per capability Worker, so no Worker can sign as another. Core holds all the keys. Each
capability Worker binds only its own. The shared `CALLER_KEY` is dropped at the end.

## What is true today

- One secret, `CALLER_KEY`, in the Secrets Store. `workers_setup` makes it once and keeps an
  existing one (`Split::setup`, src/capabilities/src/split.rs).
- Core (`pigeon-platform`, src/platform/wrangler.toml) binds it, and so does each of the six
  others: `pigeon-sites` (src/sites), `pigeon-ci-gateway` (src/ci-gateway), `pigeon-model-gateway`
  (src/model-gateway), `pigeon-cloud-gateway` (src/cloud-gateway), `pigeon-ai-agents`
  (src/ai-agents-worker) and `pigeon-proxy` (src/proxy), through `secrets_store_secrets` in each
  `wrangler.toml`. Every binding uses the same `secret_name = "CALLER_KEY"`.
- Core to Worker: `remote.rs` reads `CALLER_KEY` (`caller_key`) and signs each call with
  `pigeon_contract::sign`. The Worker checks it with `pigeon_contract::verify` and its own copy.
- Worker to core: the Worker signs with the same key. Core's `split::serve` reads `CALLER_KEY`
  and verifies with `pigeon_contract::verify` (src/contract/src/caller.rs).
- The signature covers op, body digest and the caller, lasts 60 s, allows 5 s of clock skew, and
  compares in constant time (`pigeon_core::sign::equal`).
- Result: any Worker that holds the key can sign a caller for any other Worker, or for core.

## Design

- Names. A Worker's name is its `name` in ci/workers.toml. That name is what the Worker sends
  in `X-Pigeon-Worker`, and the key is `CALLER_KEY_` plus the name in upper case, with `-` as
  `_`. This keeps one name per Worker across the map, the Lander and the header.
- Keys: `CALLER_KEY_SITES`, `CALLER_KEY_MODEL_GATEWAY`, `CALLER_KEY_CI_GATEWAY`,
  `CALLER_KEY_PROXY`, `CALLER_KEY_AI_AGENTS` and `CALLER_KEY_CLOUD_GATEWAY` (`split::WORKER_KEYS`).
  `workers_setup` makes each one that is missing, readable by Workers, and keeps existing ones.
  This is a small change to `Split::setup`, in step 0 below.
- Core binds all six. Each Worker binds only its own, under the binding name `CALLER_KEY_OWN`,
  so Worker code does not change per Worker. (Chosen as the conventional option; the brief did
  not name the binding.)
- Who is calling core: a Worker sends `X-Pigeon-Worker: <name>` (a name in
  `split::WORKER_KEYS`). Core checks the signature with that Worker's key, then the shared key.
  The claim is not trusted by itself: only a signature made with that Worker's key passes. A
  Worker that claims another's name fails, because it lacks that key. A request with no header,
  or an unknown name, is checked with the shared key only.
- Core to Worker needs no header: core knows which binding it calls, so it picks that
  Worker's key.
- `split::verify_caller` is the one verifier for core. With a Worker's own key it tries that
  key first. A good signature that fails the age or skew check is refused for that reason, not
  retried. Only a bad signature falls back to the shared key. The skew checks are the existing
  ones from `pigeon_contract::verify`.

## Rollout

Each step is safe alone. Landing order is the order below.

0. **Person action: make the secrets.** Done: landed in `57e270d`, and `workers_setup` run live
   on 2026-11-25 answered "already there; kept" for all seven names. The code:
   `Split::setup` makes each of `split::caller_key_names()` (the shared `CALLER_KEY` and the six
   `CALLER_KEY_*`) that `split::keys_to_make` finds missing, readable by Workers, keeps existing
   ones and answers only `made` or `already there; kept` per name, never a value. It adds no
   binding, so it deploys core's lane (`pigeon-platform`) only and is safe with no new secret.
   Then the person:
   1. After it lands and `pigeon-platform` deploys, runs `workers_setup` live once.
   2. Checks its answer's `keys` lists all seven names, and in the Secrets Store that
      `CALLER_KEY_SITES`, `CALLER_KEY_CI_GATEWAY`, `CALLER_KEY_MODEL_GATEWAY`,
      `CALLER_KEY_CLOUD_GATEWAY`, `CALLER_KEY_AI_AGENTS` and `CALLER_KEY_PROXY` exist.
   No wrangler.toml binds them until step 2.
1. **Core verifies either key, signs with the shared one.** Landed in `a8363fe`
   (`split::verify_caller`, `worker_key`, `WORKER_HEADER`). It needs no
   person action and no binding: `worker_key` asks for `CALLER_KEY_<WORKER>` and gets nothing
   while core has no such binding, so every check uses the shared key as today. No Worker sends
   `X-Pigeon-Worker` yet, so the new path is not reached. Deploys core's lane only
   (src/capabilities is core's; it deploys `pigeon-platform`).
2. **Each Worker binds its own key and names itself, still signing with the shared one.**
   Landed (on main by `57960d8`). Built on seven branches. Each lands alone, so a failed deploy shows which
   Worker. Land them in this order:
   1. `claude/caller-keys-step2-header` (shared lane: every Worker deploys). Adds
      `pigeon_contract::identify(name)` and `WORKER_HEADER`; `ask` sends `X-Pigeon-Worker`
      on every signed request once a Worker has named itself. Inert: nothing calls `identify`.
   2. `claude/caller-keys-step2-core`. src/platform/wrangler.toml binds the six
      `CALLER_KEY_<WORKER>` under their own names (the names `split::worker_key` reads). That
      file is also in the ci-gateway and proxy lanes, so those two redeploy too. `serve`'s
      check is now `split::verify_request` (the same check, with the recipient check), so the
      smoke test `proxy::core_accepts_a_named_worker_signing_with_the_shared_key` runs
      the exact path. Based on main; it does not need the header branch.
   3. One branch per Worker, each based on the header branch, in any order:
      `claude/caller-keys-step2-sites`, `-ci-gateway`, `-model-gateway`, `-cloud-gateway`,
      `-ai-agents`, `-proxy`. Each binds its own key as `CALLER_KEY_OWN` (not read yet) and
      names itself from `#[event(start)]` (`identify_worker`), which runs when the module loads,
      so Durable Object code also sends the name. worker-build also exposes that function as
      an entrypoint method; it takes nothing and only re-sets the same name.

   What core does then: it reads the named Worker's key, the Worker's signature (still the
   shared key) fails it as a bad signature, core falls back to `CALLER_KEY` and accepts the
   request as `KeyUsed::Shared`, so `WORKER_OPS` does not apply and nothing that passed before
   is refused. A Worker branch landing before core's only means core has no key for the name
   and checks the shared key alone. Each wrangler change was checked with
   `ci/deploy/allowlist.sh` (all calls on the allowlist; see "Checking each wrangler binding
   change").
3. **Each side signs with the per-Worker key.** Built, not landed, on
   `claude/caller-keys-step3` (based on main at `57960d8`, after all seven step 2 deploys).
   Eight code commits, landed one per landing **in commit order** (the doc commit last):
   1. **3a, receivers** (`K1 step 3a`, shared lane: src/contract, so every Worker deploys).
      `pigeon_contract::serve` takes `pigeon_contract::Keys` and checks core's call with
      `CALLER_KEY_OWN`, then `CALLER_KEY` (`Keys::verify`; only a bad signature falls back, as
      in core's `split::verify_with_keys`). `pigeon_contract::own_key` and `keys::read` fail
      closed: a Worker whose own key is not bound or not set answers 503 and serves no signed
      call. sites, model-gateway, ci-gateway, cloud-gateway and ai-agents change; proxy serves
      no signed calls. Core still signs with the shared key, so nothing is refused.
      **Wait until all five of those Workers' deploys are live before 3b.**
   2. **3b, core signs per Worker** (`K1 step 3b`, core's lane). `remote.rs` signs each call
      to a bound Worker with `split::signing_key_name(script)`: `pigeon-sites` gets
      `CALLER_KEY_SITES`, and so on. A Worker with no known key, or a key not bound or not set,
      gets no call: no fall-back to the shared key or to unsigned.
   3. **3c, each Worker signs with its own key**, one commit per Worker (`K1 step 3c (<name>)`):
      model-gateway (src/models), cloud-gateway (src/cloud), ci-gateway (src/ci-gateway,
      src/ci), ai-agents (src/ai-agents), proxy (src/proxy) and sites (src/sites). Each reads
      `pigeon_contract::own_key` for its asks of core, with no fall-back. Core has verified
      own-then-shared since step 2, so these need only 3a's code, not 3b; their order among
      themselves does not matter. From each, core sees that Worker's requests as
      `KeyUsed::Own` and applies its `WORKER_OPS` row, so a missing row now refuses a call
      (HTTP 403, logged `split: refused a Worker's core op`). src/models, src/ci and
      src/ai-agents are also linked by core, so those three redeploy core too; core never
      runs those paths (it has no `PLATFORM` binding).

   Sites keeps reading `CALLER_KEY` after 3c, for derivation only, never to sign
   (`SiteKeys::site`): protected sites' origin keys (`site_auth::origin_key`, which core's
   instance apply derives from `CALLER_KEY` too) and their sign-in session keys. Each Worker's
   wrangler.toml comments are updated in its 3c commit; no binding changes. No `ci/` change and
   no person action is needed: every secret and binding exists from steps 0 and 2.

   Tests (smoke, tests/smoke/proxy.rs): `worker_checks_cores_calls_with_its_own_key_then_the_shared_one`
   (own key, shared key, another Worker's key, tampered body, op and envelope, age, contextual
   recipient, own key only) and `core_signs_each_bound_workers_calls_with_that_workers_key`
   (the key per script; every service core binds in src/platform/wrangler.toml has a key core
   binds; each Worker binds that same key as `CALLER_KEY_OWN`; a call core signs for one
   Worker verifies there and at no other).
4. **Drop the shared key.** Not started. Remove the `CALLER_KEY` bindings and the fallback code. Delete the
   secret from the Secrets Store last, by the person. Before that, move what is still derived
   from `CALLER_KEY` off it: protected sites' origin keys (core's `instances.rs` apply and
   sites' `public.rs`, which must agree, and changing the key means re-applying each protected
   instance) and sites' sign-in session keys (changing it signs everyone out of protected
   sites). Then the Workers' `Keys::shared` is `None` (`keys::shared_key` already reads an
   unbound key as none) and core's `verify_with_keys` loses its shared fall-back.
5. **Bind each Worker to the core operations it may call.** Enforcement landed in `57e270d`;
   see the next section. It takes effect only for requests
   signed with a Worker's own key, so it bounds nothing until step 2 (core binds the keys,
   Workers send the header) and step 3 (Workers sign with their own key). It bounds everything
   only after step 4, while the shared key still verifies unbounded requests.

**Next: land step 3** from `claude/caller-keys-step3` in commit order, one per landing: 3a,
then (once sites, model-gateway, ci-gateway, cloud-gateway and ai-agents have deployed it) 3b,
then the six 3c commits. Watch core's logs for `split: refused a Worker's core op` after each
3c: that is a `PLATFORM` call missing from `WORKER_OPS`. **Then step 4**, with the derivation
work above.

## Step 5: per-Worker allowed core operations (landed, inert until steps 2 and 3)

Per-Worker keys alone stop one Worker from signing as another. They do not bound what a
compromised Worker can do to core, because the signer chooses the caller inside the envelope,
and core's signed endpoint (`split::serve`, src/capabilities/src/split.rs) accepts powerful
operations from any holder of a valid key:

- `dispatch` runs any tool as the signed caller, through the usual `dispatch` and its gate.
- `credential` hands out a `scoped` Cloudflare token for any purpose in `purpose_permissions`
  (including `cf_deploy`), or a `run_grant` (`credential` in split.rs). Its former `linked` and
  `github` kinds are gone: Sites gets those only through the instance-scoped `sites_scope`
  (`claude/sites-require-instance-context`).
- `approval` creates and reads `cloud_change` approvals; `credential_revoke`,
  `credential_release` and `service_user` act on tokens and service users.

So core keeps, per Worker name, the operations (for the `credential*` ops the kinds, for
`sites`' `dispatch` the tools) that Worker may ask, and refuses anything else once the signature
checks out with that Worker's own key. A request signed with the shared key keeps today's
behaviour until step 4.

How it is built: `split::WORKER_OPS` is the one declarative table, `split::worker_may` checks a
request against it, and `split::serve` applies it right after the signature check, answering
HTTP 403 (`Unauthorized`, "Worker `x` may not ask core `op` …") and logging the refusal. The
verifier now says which key passed (`split::KeyUsed`, `verify_caller_keyed`); the table applies
only for `KeyUsed::Own`. A Worker that claims another's name holds neither that key nor (after
step 4) the shared one, so its signature fails; while the shared key lives, a claim it verified
is `KeyUsed::Shared` and gets no Worker's table, the same as today. The fall-back order matches
the design above: only a bad signature with the own key tries the shared key.

The table follows the code at `327553c`: every call each Worker's crates make over their
`PLATFORM` binding (re-checked by grepping `pigeon_contract::ask` in each Worker's crates; the
ai-agents crate depends on pigeon-models but uses only its pure parts, so asks no `scoped`
credential). No crate sends `credential_revoke` of kind `scoped`, so no Worker may.

| Worker (name in ci/workers.toml) | Ops it sends to core | Where |
| --- | --- | --- |
| `sites` | `sites_scope` (`Record`, `Linked`, `Github`); `dispatch` of `architecture_describe` (apex), `instance_subdomains` and `site_passkey` (public routes) | src/sites/src/deploy.rs, apex.rs, public.rs |
| `ci-gateway` | `credential` kind `scoped`; `ci_scope` | src/ci/src/lib.rs, src/ci/src/scope.rs |
| `model-gateway` | `credential` kind `scoped` | src/models/src/lib.rs (`credential_given`) |
| `cloud-gateway` | `credential` kind `scoped`; `approval` | src/cloud/src/link.rs, src/cloud/src/approvals.rs |
| `proxy` | `verify`, `grant`, `deploy_grant` (signed for no caller); `credential` kind `scoped` | src/proxy/src/lib.rs |
| `ai-agents` | `dispatch` of any tool (the run's own calls such as `models_submit`, `models_status`, `approvals_status`, `land_list`, `ci_commit`, and the tools a run's model chooses); `credential` kind `run_grant`; `credential_revoke` kind `run_grant`; `credential_release` kind `scoped`; `service_user` | src/ai-agents/src/object.rs |

Notes:

- `ai-agents` needs `dispatch` of arbitrary tools (a run's model chooses them), so a tool list
  is not feasible; its bound is the signed caller and core's gate. `sites` dispatches four tools
  only, and the table lists them. Its legacy `instances_list` and `credential` uses go once its
  publishing callbacks are all `sites_scope` ([legacy-worker-cutover.md](legacy-worker-cutover.md)).
- Later refinement: bind `scoped` credentials to the purposes each Worker asks for (for
  example only `proxy` asks `cf_deploy`).
- The smoke scenario `proxy::core_bounds_each_workers_ops_with_its_own_key` checks each
  Worker's real calls pass and others are refused. It does not read the crates' sources; a new
  `PLATFORM` call needs a row here and in `WORKER_OPS`, or it is refused once step 3 lands.

### Checking each wrangler binding change

Every change to a `wrangler.toml` is checked with `ci/deploy/allowlist.sh`. For core run
`sh ci/deploy/allowlist.sh`. For another Worker run
`PIGEON_WORKER_DIR=src/sites sh ci/deploy/allowlist.sh` (use the Worker's directory). It runs a
real `wrangler deploy` against a stand-in and checks every call against the Cloudflare proxy's
allowlist. The `allowlist` and `deploy` CI jobs run it too. Read its output for a call naming the
new secret binding. It cannot tell whether the secret exists in the real store; that is the
step 0 check by the person.

## Risks

- A binding to a missing secret refuses the deploy. Step 0 is the guard.
- A step 3 landing in one Worker before its partner would refuse every call between them. Land
  step 2 everywhere, wait for all six capability Workers' deploys, then step 3.
- Core holds all six keys, so core can sign as any Worker. That is by design: core is the
  front door.
- Rotating a key refuses calls in flight for up to a minute. Keep "an existing key is kept".
- The header is a claim. If core had no per-Worker key for the name, it falls back to the
  shared key, which is as strong as today and no stronger.

## Follow-ups for the person

1. Done (2026-11-25): `workers_setup` live reported all seven caller keys present.
2. The binding name in each Worker is `CALLER_KEY_OWN` (used by the step 2 branches).
3. After step 4, delete `CALLER_KEY` from the Secrets Store.
4. `docs/plans/workers-split.md`, `architecture.md` and `proxies.md` mention the one shared
   `CALLER_KEY`; update them once step 4 lands.

# Per-Worker caller keys

Status: **steps 0, 1, 2 and 5 done; step 3 half done (3a); step 4 not started.** Steps 1 and 5
are on main (`a8363fe`, `57e270d`). Step 0 is done live: `workers_setup` reported all seven
caller keys "already there; kept" (2026-11-25). Step 2 landed on 2026-11-25 (`50a15bc` and the
six "Bind the … Worker's own caller key" commits, `2c6b959` to `aaa1fd2`): core binds the six
`CALLER_KEY_<WORKER>`, each Worker binds its own as `CALLER_KEY_OWN` and names itself. Step 3a
landed in `950f461`: the Workers that serve core check its calls with `CALLER_KEY_OWN`, then
`CALLER_KEY`. Step 3b (each side signs with the per-Worker key) is not started, so every
signature is still made with the shared `CALLER_KEY`, which all seven Workers still bind, and
`WORKER_OPS` still bounds nothing. Task K1 of the clean-up.

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
   Landed on 2026-11-25, as the seven branches below. Each lands alone, so a failed deploy shows which
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
3. **Each side signs with the per-Worker key.** 3a (Workers verify `CALLER_KEY_OWN`, then
   `CALLER_KEY`) landed in `950f461`; 3b (switching the signers) is not started. Core signs for Worker X with `CALLER_KEY_X`;
   Worker X signs with its own. Must wait until all seven step 2 branches have landed and their
   deploys are live (core and the six Workers). Within step 3, first make each Worker verify
   core's calls with `CALLER_KEY_OWN` and then `CALLER_KEY` (step 2 does not: Workers still
   verify with the shared key only), and deploy that everywhere; only then switch the signers.
4. **Drop the shared key.** Not started; only after 3b is deployed everywhere. Remove the
   `CALLER_KEY` binding from all seven `wrangler.toml`s and the fallback code: the shared key
   in `pigeon_contract::Keys` (src/contract/src/keys.rs, `SHARED_KEY`), `split::KeyUsed::Shared`
   and its fall-back in `split::verify_caller`, and the shared-key signing in
   `remote::caller_key` and in src/ci/src/lib.rs (`CALLER_KEY`). Delete the secret from the
   Secrets Store last, by the person.
5. **Bind each Worker to the core operations it may call.** Enforcement landed in `57e270d`;
   see the next section. It takes effect only for requests
   signed with a Worker's own key, so it bounds nothing until step 2 (core binds the keys,
   Workers send the header) and step 3 (Workers sign with their own key). It bounds everything
   only after step 4, while the shared key still verifies unbounded requests.

**Next: step 3b.** Steps 2 and 3a are live, so each side may now sign with the per-Worker key,
one Worker per landing. **Then step 4**, once every 3b deploy is live.

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
4. Once step 4 lands: `docs/plans/workers-split.md`, `architecture.md` and `docs/proxies.md`
   still describe the one shared `CALLER_KEY`, and must say each Worker holds only its own.

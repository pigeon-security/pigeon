# Per-Worker caller keys

Status: **steps 0 to 3 and 5 done; step 4 built on `claude/caller-keys-step4`, not landed.**
Steps 1, 2, 3 and 5 are on main (step 3 as of `2856b5f`: every Worker signs with and checks its
own `CALLER_KEY_OWN`, and core signs per Worker). Step 0 is done live: `workers_setup` reported
all seven caller keys "already there; kept" (2026-11-25). Step 4 is four code commits, landed
one per landing in order, with person steps between them (see step 4 below). Task K1 of the
clean-up.

## Goal

One key per capability Worker, so no Worker can sign as another. Core holds all the keys. Each
capability Worker binds only its own. The shared `CALLER_KEY` is dropped at the end.

## What was true before this plan

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
3. **Each side signs with the per-Worker key.** Landed (3a `950f461`, 3b `709459b`, the six 3c
   commits through `2856b5f`). Built on `claude/caller-keys-step3` (based on main at `57960d8`,
   after all seven step 2 deploys). Eight code commits, landed one per landing **in commit
   order** (the doc commit last):
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
4. **Drop the shared key.** Built on `claude/caller-keys-step4` (based on main at `d9a33a8`).
   Four code commits, landed one per landing **in this order**, each after the person step
   before it:
   1. **4a, make `SITE_KEY`** (`K1 step 4a`, core's lane; src/core changes by one constant, so
      every Worker redeploys, unchanged). Protected sites' origin keys and visitor session keys
      move off `CALLER_KEY` to a dedicated secret, `SITE_KEY`
      (`pigeon_core::site_auth::SITE_KEY`). `workers_setup` now keeps
      `split::setup_key_names()`: the caller keys, then `SITE_KEY`; it makes each that is
      missing, readable by Workers, and answers only `made` or `already there; kept`. No binding.
      **Person, after it deploys:** run `workers_setup` live once and check its `keys` shows
      `SITE_KEY` (`made` the first time).
   2. **4b, derive from `SITE_KEY`** (`K1 step 4b`; core's lane, which with
      src/platform/wrangler.toml also redeploys ci-gateway and proxy; sites; every Worker for
      src/core). src/platform/wrangler.toml and src/sites/wrangler.toml bind `SITE_KEY`; no other
      Worker does. Core's instance apply and pigeon-sites read it through
      `pigeon_core::secrets_store::site_key` and derive with `site_auth::origin_key` /
      `session_key`, so they derive alike; neither falls back to another key. A binding to a
      missing secret refuses the deploy, so this lands only after the person's 4a step.
      The protected origin Worker's plan digest now names its key source
      (`origin_key_from: SITE_KEY`, src/capabilities/src/instances/plan.rs), so every protected
      origin uploaded before reads as changed: `instance_plan` shows its `UploadWorker`
      (`replaces: true`, with its `origin`) and `instance_apply` uploads it with the new origin
      key. Public sites' digests do not change. **Consequences:** from this deploy until each
      protected instance is re-applied, its site answers "protected origin unavailable" (503);
      every visitor of a protected site is signed out once (their session keys changed).
      **Person, right after it deploys:** for each protected instance (sandbox and
      willowgraysen), `instance_plan`, check the only change is the site Worker's upload, then
      `instance_apply` with the passkey, and open the site to sign in again.
   3. **4c, drop the shared key from the code** (`K1 step 4c`; shared lane: src/contract, so
      every Worker; core's). `pigeon_contract::Keys` holds the own key only (no `shared`,
      `SHARED_KEY` or `keys::shared_key`). Core's `split::verify_caller` / `verify_request` take
      the Worker's own key alone; `KeyUsed` and the fall-back are gone. `split::serve` refuses
      (401) a request naming no known Worker in `X-Pigeon-Worker`, or one whose key core does not
      bind, and applies `WORKER_OPS` to every request it accepts. `split::caller_key` and
      `remote::CALLER_KEY` are gone; `workers_setup` no longer makes `CALLER_KEY` and its answer
      drops `caller_key`. Nothing reads the `CALLER_KEY` bindings after this. No person action.
   4. **4d, unbind `CALLER_KEY`** (`K1 step 4d`; all seven Workers' lanes). Removes the
      `CALLER_KEY` binding from every wrangler.toml; every `CALLER_KEY_OWN`, core's six
      `CALLER_KEY_<WORKER>` and the `SITE_KEY` bindings stay. **Land after 4c has deployed to
      core**: core's code before 4c read `CALLER_KEY` on every signed request. (Each Worker's code
      and bindings deploy together, so 4d right after 4c is safe.) A binding removal adds no
      Cloudflare API call. **Person, after all seven deploys:** delete `CALLER_KEY` from the
      Secrets Store, and only then.

   Tests: sites::sites_and_core_derive_protected_site_keys_from_site_key (both bind
   `SITE_KEY`, no other Worker does, both read it the one way, pinned derivations),
   instances::protected_origins_replan_once_their_key_moves_to_site_key, and in
   tests/smoke/proxy.rs: `core_contract_per_worker_caller_keys`,
   `core_bounds_each_workers_ops_with_its_own_key`,
   `core_refuses_a_named_worker_signing_with_the_shared_key`,
   `worker_checks_cores_calls_with_its_own_key_only`,
   `core_signs_each_bound_workers_calls_with_that_workers_key`,
   `no_worker_binds_the_shared_caller_key` and `workers_setup_plans_only_missing_caller_keys`.
5. **Bind each Worker to the core operations it may call.** Enforcement landed in `57e270d`;
   see the next section. It takes effect only for requests
   signed with a Worker's own key, so it bounds nothing until step 2 (core binds the keys,
   Workers send the header) and step 3 (Workers sign with their own key). It bounds everything
   only after step 4, while the shared key still verifies unbounded requests. Since 4c it bounds
   every request core accepts from a Worker.

**Next: land step 4** from `claude/caller-keys-step4`, one commit per landing: 4a, then the
person runs `workers_setup`; 4b, then the person re-applies sandbox and willowgraysen; 4c; 4d,
then the person deletes `CALLER_KEY`.

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
checks out with that Worker's own key. Until step 4c a request signed with the shared key
kept the old, unbounded behaviour; since then there is none.

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

- A binding to a missing secret refuses the deploy. Step 0 is the guard, and for `SITE_KEY`
  the person's `workers_setup` run between 4a and 4b.
- A step 3 landing in one Worker before its partner would refuse every call between them. Land
  step 2 everywhere, wait for all six capability Workers' deploys, then step 3.
- Core holds all six keys, so core can sign as any Worker. That is by design: core is the
  front door.
- Rotating a key refuses calls in flight for up to a minute. Keep "an existing key is kept".
- The header is a claim. Before step 4c, if core had no per-Worker key for the name, it fell
  back to the shared key; since 4c such a request is refused.
- Step 4b changes every protected origin's key: those sites answer 503 until re-applied, and
  their visitors sign in again. Re-apply right after 4b deploys.

## Follow-ups for the person

1. Done (2026-11-25): `workers_setup` live reported all seven caller keys present.
2. The binding name in each Worker is `CALLER_KEY_OWN` (used by the step 2 branches).
3. Step 4, in order: after 4a deploys, run `workers_setup` live (it makes `SITE_KEY`); after
   4b deploys, re-apply each protected instance (sandbox, willowgraysen) with `instance_plan`
   and `instance_apply` and a passkey; after 4d has deployed to all seven Workers, delete
   `CALLER_KEY` from the Secrets Store. Deleting it earlier refuses the deploy of any Worker
   that still binds it.
4. `docs/plans/workers-split.md` and `architecture.md` still describe the one shared
   `CALLER_KEY` as the plan was then; `proxies.md` and `sites.md` are updated in step 4.

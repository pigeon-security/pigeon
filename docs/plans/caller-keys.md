# Per-Worker caller keys

Status: **step 1 landed** (`a8363fe`, on main). Step 0's code (`workers_setup` makes the six
keys) and step 5's enforcement (`split::WORKER_OPS`, `worker_may`) are **built** on branch
`claude/caller-keys-step0-ops`, not yet landed; both are inert until the keys exist and core
binds them. Step 0's person action and steps 2, 3 and 4 are not started. Checked against main
at `327553c` (2026-11-25): per-Worker keys are not active. No Worker sends `X-Pigeon-Worker`, no `src/*/wrangler.toml`
binds a `CALLER_KEY_*` secret, and all seven Workers bind the shared `CALLER_KEY`. Task K1 of the
clean-up.

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

0. **Person action: make the secrets.** Code built on `claude/caller-keys-step0-ops`:
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
2. **Each Worker binds its own key and verifies either, still signing with the shared one.**
   Not started. Needs step 0 done first: a binding to a missing secret fails the deploy. Core
   also binds the six keys in this step (the same rule). One wrangler.toml change per Worker, one landing per
   Worker, so a failure shows which one. Workers also begin to send `X-Pigeon-Worker`.
3. **Each side signs with the per-Worker key.** Not started. Core signs for Worker X with `CALLER_KEY_X`;
   Worker X signs with its own. Needs step 2 landed and deployed in all six capability Workers, since an
   old Worker would refuse the new signature only if it lacked the key; none does by then.
4. **Drop the shared key.** Not started. Remove the `CALLER_KEY` bindings and the fallback code. Delete the
   secret from the Secrets Store last, by the person.
5. **Bind each Worker to the core operations it may call.** Enforcement built on
   `claude/caller-keys-step0-ops`; see the next section. It takes effect only for requests
   signed with a Worker's own key, so it bounds nothing until step 2 (core binds the keys,
   Workers send the header) and step 3 (Workers sign with their own key). It bounds everything
   only after step 4, while the shared key still verifies unbounded requests.

**Next agent task: step 2**, once the person has done step 0's two actions: bind
`CALLER_KEY_OWN` (its own `CALLER_KEY_<NAME>`) in each capability Worker's wrangler.toml, bind
all six in src/platform/wrangler.toml under their own names, and make each Worker send
`X-Pigeon-Worker: <name>`, one Worker per landing, each checked with `ci/deploy/allowlist.sh`.

## Step 5: per-Worker allowed core operations (built, inert until steps 2 and 3)

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

1. After step 0's code lands and `pigeon-platform` deploys, run `workers_setup` live and
   confirm the six `CALLER_KEY_*` secrets exist (the answer's `keys`, and the Secrets Store).
2. Decide the binding name in each Worker (`CALLER_KEY_OWN` is proposed).
3. After step 4, delete `CALLER_KEY` from the Secrets Store.
4. `docs/plans/workers-split.md`, `architecture.md` and `proxies.md` mention the one shared
   `CALLER_KEY`; update them once step 4 lands.

# Per-Worker caller keys

Status: **step 1 landed** (`a8363fe`, on main); steps 0, 2, 3 and 4 not started, and step 5
(per-Worker operations, below) is proposed. Checked against main at `a4bb475` (2026-11-25):
per-Worker keys are not active. No Worker sends `X-Pigeon-Worker`, no `src/*/wrangler.toml`
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

0. **Person action: make the secrets.** Not started. Extend `workers_setup` to make the six
   keys (a code change in core's lane), land it, then run `workers_setup` live. Check in the
   Secrets Store that all six exist. No wrangler.toml binds them yet.
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
5. **Bind each Worker to the core operations it may call.** Proposed; see the next section. It
   needs steps 2 and 3, since core can only tell Workers apart once each signs with its own key.

## Step 5 (proposed): per-Worker allowed core operations

Per-Worker keys alone stop one Worker from signing as another. They do not bound what a
compromised Worker can do to core, because the signer chooses the caller inside the envelope,
and core's signed endpoint (`split::serve`, src/capabilities/src/split.rs) accepts powerful
operations from any holder of a valid key:

- `dispatch` runs any tool as the signed caller, through the usual `dispatch` and its gate.
- `credential` hands out a `linked` account token or a `github` installation token for a named
  instance, a `scoped` Cloudflare token for any purpose in `purpose_permissions` (including
  `cf_deploy`), or a `run_grant` (`credential` in split.rs).
- `approval` creates and reads `cloud_change` approvals; `credential_revoke`,
  `credential_release` and `service_user` act on tokens and service users.

So the proposal is that core keeps, per Worker name, the operations (and for `credential` the
kinds) that Worker may ask, and refuses anything else once the signature checks out with that
Worker's own key. A request signed with the shared key keeps today's behaviour until step 4.

The table is derived from the code at `a4bb475`: every call each Worker's crates make over
their `PLATFORM` binding. It is **proposed**; nothing enforces it.

| Worker (name in ci/workers.toml) | Ops it sends to core | Where |
| --- | --- | --- |
| `sites` | `sites_scope` (`Record`, `Linked`, `Github`); `credential` kinds `linked` and `github` (legacy publishing path); `dispatch` of `instances_list` (legacy publishing), `architecture_describe` (apex), `instance_subdomains` and `site_passkey` (public routes) | src/sites/src/deploy.rs, apex.rs, public.rs |
| `ci-gateway` | `credential` kind `scoped`; `ci_scope` | src/ci/src/lib.rs, src/ci/src/scope.rs |
| `model-gateway` | `credential` kind `scoped` | src/models/src/lib.rs (`credential_given`) |
| `cloud-gateway` | `credential` kind `scoped`; `approval` | src/cloud/src/link.rs, src/cloud/src/approvals.rs |
| `proxy` | `verify`, `grant`, `deploy_grant` (signed for no caller); `credential` kind `scoped` | src/proxy/src/lib.rs |
| `ai-agents` | `dispatch` (the run's own calls such as `models_submit`, `models_status`, `approvals_status`, `land_list`, `ci_commit`, and the tools a run's model chooses); `credential` kind `run_grant`; `credential_revoke`; `credential_release`; `service_user` | src/ai-agents/src/object.rs |

Notes for whoever builds it:

- `ai-agents` needs `dispatch` of arbitrary tools, so for it the bound is the caller and the
  gate, not a tool list. `sites` needs `dispatch` for four tools only, so a tool list for it is
  cheap. Its legacy `instances_list` and `credential` uses go once its publishing callbacks are
  all `sites_scope` ([legacy-worker-cutover.md](legacy-worker-cutover.md)).
- `scoped` credentials could also be bound to the purposes each Worker asks for, as a later
  refinement.
- The table has to follow the code; a smoke scenario that checks each crate's `PLATFORM` calls
  against it would keep it honest.

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

1. After step 0 lands, run `workers_setup` live and confirm the six secrets exist.
2. Decide the binding name in each Worker (`CALLER_KEY_OWN` is proposed).
3. After step 4, delete `CALLER_KEY` from the Secrets Store.
4. `docs/plans/workers-split.md`, `architecture.md` and `proxies.md` mention the one shared
   `CALLER_KEY`; update them once step 4 lands.

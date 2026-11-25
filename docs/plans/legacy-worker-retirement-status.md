# Legacy Worker retirements after the queued publishing cutover

This status check applies the [signed Worker design](signed-worker-context.md)
to the deployed core revision `6bed797`. A queued publication on `noisypigeon`
succeeded. The isolated `sandbox` instance completed the GitHub-triggered
publication of `noisypigeon/sandbox` commit `b9f3c97` in one attempt.
The owner reported redelivery, and the current queue row still shows the same
commit and one attempt. The delivery response and independent release-activation
history are still needed to close the replay check. The live adverse-path and
authenticated scoped-endpoint checks remain open.

| Compatibility path | Status on main | Retirement decision |
| --- | --- | --- |
| GitHub cron → legacy `remote::call_owned` → public `site_deploy` | Already replaced and the helper removed. Cron negotiates support and calls the private `site_deploy_queued` receiver with matching signed context. An unsupported receiver never gets a legacy request. | **Already retired on the sender.** No duplicate removal is needed. |
| Private Sites `site_deploy_queued` receipt | Already checks signed context, instance, standard caller, exact commit, and publishing record digest. The deploy workflow checks the approved digest again and verifies the rendered archive's commit. | **Already enforced.** Preserve the private route and its independent checks. |
| Public Sites `site_deploy` compatibility for the scheduled-only `publishing_binding` argument | **Retired** on `claude/retire-public-publishing-binding`. Core runs as the single `pigeon-platform` Worker, deployed at `96cfcb5`, well after the sender switch in `745c2bf`, so no older scheduler is deployed. Public `site_deploy` now refuses a request that carries `publishing_binding` with a bad-request error that names the private receipt. Only `queued::Receipt::read` (`src/sites/src/queued.rs`) can construct a receipt, and only `deploy::deploy_queued` accepts one. The digest and exact-commit checks are now on the receipt. Manual `site_deploy` and `site_rollback` are unchanged. | **Retired.** The rollback procedure is [below](#pausing-queued-publishing-for-a-rollback). Keep queued publishing paused or drained while a core older than `745c2bf` is deployed. |
| Public Sites `site_deploy` and `site_rollback` legacy envelopes | Global/manual entry points still need the established transport. An authenticated instance MCP request has a contextual path. | **Keep.** Scoped endpoint validation and migration of all instance-targeted ingress must precede receiver enforcement. A successful cron receipt does not validate those ingress paths. |
| Sites → core `instances_list`, GitHub installation, and linked-account callbacks | Scoped publishing already uses the typed, signed `sites_scope` callback for the record and credentials. Platform/manual routes still use the existing core operations. | **Verify the deployed scoped path.** Check record and credential selection for publish and rollback, including a stale binding, before removing any instance-specific fallback. Keep the platform administration versions of the tools available. |
| CI gateway → core legacy `ci_scope` read | The contextual sender and core receiver are on main. The core verifier checks its instance. Since `3d3da94` the global connector also sends an instance read with context when the CI gateway advertises it; a gateway without that advertisement still gets the legacy envelope, and core still accepts legacy reads. | **Keep.** Verify a contextual read and migrate every remaining instance-targeted sender before tightening the callback. Platform attribution by repository and job stays separate. |
| Shared legacy verification and platform operations | CI submissions and attribution, platform credentials, console and passkeys callbacks, proxy checks, agent and model calls, and other platform operations still have supported paths. | **Keep.** Enforcement applies to one instance-bound operation after its senders migrate. There is no shared envelope removal in this cutover. |

The per-operation code status and evidence gates are in [legacy-worker-cutover.md](legacy-worker-cutover.md).
No runtime cleanup is independent of these remaining gates. The sender-side dead
path was removed by the cutover itself. The scheduled-only compatibility
argument on public `site_deploy` is retired, and its rollback procedure is
recorded below. Keep the last approved release serving if a staged
verification fails.

## Pausing queued publishing for a rollback

There is no dedicated pause flag. The scheduler runs only from the platform
Worker's minute cron: `[triggers] crons` in
[`src/platform/wrangler.toml`](../../src/platform/wrangler.toml) fires
`scheduled` in [`src/platform/src/lib.rs`](../../src/platform/src/lib.rs),
which calls `github::run_pending`
([`src/capabilities/src/github.rs`](../../src/capabilities/src/github.rs)).
Two mechanisms stop queued publishes. Only the first keeps queued work.

**Pause (keeps the queue).** Remove the `pigeon-platform` cron trigger before
rolling core or Sites back. Use the dashboard's Triggers settings, or deploy the
trigger list empty. The same cron also runs the token sweep (`ledger::reap`),
so keep the pause short. A later `wrangler deploy` of the checked-in
`wrangler.toml` restores the trigger. After the rollback, check that the
trigger list is still empty.

While the cron is paused:

- Webhooks are still verified and recorded. Each push to `main` replaces the
  instance's single `github:deploy:<instance>` row with a new `pending` row with
  `attempts: 0`, so only the latest push per instance waits. Rows are kept for
  `PUSH_KEPT_MS` (90 days).
- A row left `running` by an interrupted run stays put. After resuming it is
  due again once its `LEASE_MS` lease (3 minutes) has passed.
- `prepare_publish` checks each due row when the cron resumes. A row that has
  used `MAX_ATTEMPTS` (5) is marked failed without running. A row whose approved
  publishing record no longer matches its binding is marked failed and needs a
  new push. Other rows run once each, one instance at a time.

**Drain (drops the queue).** Approve an instance record change
(`instance_propose`, then the passkey) that sets `publishing` to
`{"state":"disabled"}` (`Profile` in
[`src/core/src/site_publish.rs`](../../src/core/src/site_publish.rs)).
`publishing_binding` then returns nothing, so webhooks queue nothing. The next
cron run marks any due row failed in `prepare_publish`. Manual `site_deploy` for
that instance is refused too, because `site_publish::site` rejects a disabled
profile. A tool policy that requires approval for `site_deploy` also stops
scheduled publishes (`publish_allowed`), which mark the row failed
("policy requires manual publishing"). Neither replays the dropped pushes.

**Resume.** Restore the cron trigger by redeploying the platform Worker from
its checked-in `wrangler.toml`. Pending rows then run on the next minute. After
a drain, re-approve `publishing: {"state":"enabled","site":…}`, then push again
or run a manual `site_deploy` at the wanted commit.

**Compatible pairs.** The current scheduler sends only the private
`site_deploy_queued` (`remote::call_owned_instance`). It refuses a Sites
manifest without signed-context support before sending, and an older Sites
build without the queued tool answers that the tool is unknown. In both cases
each attempt fails, and the row ends failed after `MAX_ATTEMPTS`. Core builds
older than `745c2bf` send scheduled publishes through public `site_deploy`
with `publishing_binding`. Sites from `claude/retire-public-publishing-binding`
onward refuses that argument before reading the record or fetching anything.
Keep the queue paused or drained while such a core runs, or roll Sites back
with it to a build that still accepts the argument.

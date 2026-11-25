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
| Public Sites `site_deploy` compatibility for the scheduled-only `publishing_binding` argument | Main's new scheduler no longer sends this argument through the public tool. The older scheduler does. Ordinary manual `site_deploy` does not advertise or require it. | **Keep for rollback.** This is the next removable receiver branch after the stale-binding and unsupported-receiver checks, evidence that every active scheduler uses the private call, and an agreed rollback procedure that pauses queued publishes or retains a compatible receiver. Removing the argument must leave regular manual publication and rollback behavior intact. |
| Public Sites `site_deploy` and `site_rollback` legacy envelopes | Global/manual entry points still need the established transport. An authenticated instance MCP request has a contextual path. | **Keep.** Scoped endpoint validation and migration of all instance-targeted ingress must precede receiver enforcement. A successful cron receipt does not validate those ingress paths. |
| Sites → core `instances_list`, GitHub installation, and linked-account callbacks | Scoped publishing already uses the typed, signed `sites_scope` callback for the record and credentials. Platform/manual routes still use the existing core operations. | **Verify the deployed scoped path.** Check record and credential selection for publish and rollback, including a stale binding, before removing any instance-specific fallback. Keep the platform administration versions of the tools available. |
| CI gateway → core legacy `ci_scope` read | The contextual sender and core receiver are on main. The core verifier checks its instance. The global connector can still supply an instance through its legacy ingress. | **Keep.** Verify a contextual read and migrate every remaining instance-targeted sender before tightening the callback. Platform attribution by repository and job stays separate. |
| Shared legacy verification and platform operations | CI submissions and attribution, platform credentials, console and passkeys callbacks, proxy checks, agent and model calls, and other platform operations still have supported paths. | **Keep.** Enforcement applies to one instance-bound operation after its senders migrate. There is no shared envelope removal in this cutover. |

No runtime cleanup is independent of these remaining gates. The sender-side dead
path was removed by the cutover itself. The next receiver change can be limited
to the scheduled-only compatibility argument once its rollback and failure
evidence is recorded. Keep the last approved release serving if a staged
verification fails.

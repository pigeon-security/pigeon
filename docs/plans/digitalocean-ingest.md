# DigitalOcean Spaces ingest on temporary compute

The opt-in data-ingest capability plans a Spaces transfer using the instance's approved
DigitalOcean connection. Scoping comes from the saved Spaces region, bucket and prefix. A
caller can request a narrower prefix. The data capability supplies the approved linked R2
payload destination. Legacy ingest tools continue to use their stored profiles and run history.

## Delivery

1. Add a provider transport for a dedicated DigitalOcean block volume and Droplet. Give both
   resources one job-specific identity. Use the volume for bounded spool space. A timed-out
   create is reconciled by identity without issuing a second create.
2. Store the full launch scope in the passkey-approved plan. Include the source and
   destination, runner commit, volume size, runtime limit and cost assumptions. Assign cleanup
   to the Jobs maintenance tick. Persist each resource creation attempt before provider I/O.
3. Start the runner only after it mounts the approved attached volume. Use an expiring,
   one-time bootstrap ticket to deliver credentials over TLS after provisioning. Keep the
   saved read-only Spaces key and the bucket write credential out of provider user data and
   diagnostic records.
4. Keep the runner's manifests and final report with its R2 payloads. When the run finishes,
   fails, is cancelled or reaches its deadline, delete the exact owned Droplet. Confirm its
   absence before deleting the detached volume, then independently confirm volume absence.
   Export the redacted job outcome and resource usage to the private Jobs audit bucket.

The live canary requires the instance's approved Data and Jobs configuration, a bucket-scoped
R2 write credential and a DigitalOcean provisioning key with Droplet and Block Storage
create, read and delete permissions, tag creation, and DigitalOcean's required associated
read scopes. Review the exact plan and passkey approval before creating either resource.
[Block Storage API](https://docs.digitalocean.com/reference/api/reference/block-storage/)
[Droplet create scopes](https://docs.digitalocean.com/reference/api/scopes/droplet/create/)


## Plan and run

The `data_ingest_plan` tool accepts `source: "digitalocean"` and an optional narrower `prefix`.
It defaults to a dry run with a 32 GiB volume and a 30 minute deadline. Operators may
specify `scratch_gib` from 8 to 1024 and `max_minutes` from 10 to 360. For large listings,
provide `size_bytes` for a whole-prefix estimate. Archive containers are always retained.
Large ZIP runs can request explicit `archive_inflated_gib` and `archive_member_gib` limits.
The planner reserves at least half the usable scratch space for the source archive and
processing overhead.
The estimate identifies its transfer assumptions and includes compute and volume at the
requested deadline. Confirmed cleanup can take longer than the processing deadline.

After passkey approval, the Approver invokes `data_ingest_start` as the original caller.
A maintenance tick provisions the volume then the Droplet. `data_ingest_status` tracks processing, independent deletion confirmations and audit
delivery. `data_ingest_report` reads the runner's bounded report and `data_ingest_stop` schedules
cleanup. An uncertain provider response remains in reconciliation until absence is confirmed.

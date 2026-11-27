# DigitalOcean Spaces ingest on temporary compute

The opt-in data-ingest capability will plan a Spaces transfer using the instance's approved
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
permissions. Review the exact plan and passkey approval before creating either resource.

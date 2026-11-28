# Named Data Ingest bucket connections

A bucket connection is an approved source under `data.ingest.connections` on an instance
record. Add a lowercase connection name and its provider, region, bucket and optional
prefix through the instance record editor. Backblaze also takes its source name and
Scaleway takes its project ID, as in the existing provider profiles. DigitalOcean uses
the instance's approved team. The Data capability must be enabled.

Capabilities → Data Ingest → Bucket connections shows the approved scopes. Set or
rotate each connection's bucket-scoped read-only key with a passkey. Pigeon verifies a
bounded listing in the approved scope before saving the key. Spaces also verifies the
key's permissions against the approved DigitalOcean team. The key is encrypted in the
Directory and never appears in the instance record, tool result or ingest approval.

A connection pin identifies the approved account and bucket scope plus the exact
credential generation. Changing the scope or rotating the key invalidates a previous
pin. A named Spaces connection can reuse the old sealed Spaces key when both approved
bindings match exactly. Setting a dedicated key for the named connection takes over
from that fallback.

Use `data_ingest_connection_verify` with `instance` and `connection` to verify the
approved read-only source without creating an ingest approval. Use `data_ingest_plan`
with `instance`, `connection` and optionally a narrower `prefix` to review a run.
Named runs require a managed Data writer grant. The existing provider selectors
still use their established configuration.

To enable the managed destination, review and apply the standalone Data Worker's
instance plan. It installs the destination route and its D1 run-budget table. A
Backblaze Cloudflare run also needs one reviewed runner image pin for the instance.
Put it under a named Backblaze connection's `runtime`, or reuse the same existing
Backblaze runtime pin. Build the image from a revision that includes the managed
destination client.

The approval names the exact connection generation, destination Worker and
maximum number of write attempts and bytes. The runner receives a short-lived
grant for that run. Dry runs can write their progress and reports. A live canary
requires a fully listed source prefix with objects no larger than 16 MiB. ZIP
members above that limit are reported as failed until the managed multipart
publisher is available.

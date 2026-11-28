# Named Data Ingest bucket connections

Enable Data and Data Ingest on the instance, then open **Capabilities → Data Ingest
→ Bucket connections → Add connection**. Enter the connection name, provider, region,
bucket, and optional prefix. Scaleway also needs its project ID. DigitalOcean uses the
instance's approved team. Choose **Review source** and approve the instance record
change with a passkey.

The console opens the credential form after saving the source. Enter the key ID and
secret in the console and choose **Verify and save key with passkey**. The secret is
masked and excluded from the record approval. To update an existing connection, use
**Edit source**, **Set key**, or **Rotate key** in the same list.

Pigeon verifies a bounded listing in the approved scope before saving the key. Spaces also verifies the
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


### Owning-account source key staging

For a linked instance with a standalone Data Worker, **Use owning account** stages a
new slot for an existing named source. The instance record stores the generation in
`data.ingest.source_generations`. It carries no key bytes. After approving the change,
copy the slot shown beside the connection and create that secret directly in the
owning Cloudflare account's Secrets Store. Use a provider read-only key scoped to the
approved source and store it as `ACCESS_KEY_ID:SECRET_ACCESS_KEY`.

Review and apply the Data plan. It binds only the named slots in that account and
fails closed when a staged slot is missing. **Verify** uses the linked Data Worker for
a bounded list. The listing proves read access and does not establish provider-side
write denial. The home console only handles the reference in this mode.

This step stages the registry, bindings, and owning-account read path. A source
marked owning-account cannot launch through a legacy runner or save its key through
the home console. Leave an existing active connection in legacy mode until its
owning-account launch adapter is available. No existing home key or provider
credential is deleted by staging a linked slot.

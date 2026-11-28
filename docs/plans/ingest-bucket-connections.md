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

Use `data_ingest_plan` with `instance`, `connection` and optionally a narrower `prefix`
to check the named source. This preview can verify the scope and present its pin.
Issuing a run approval for a named connection requires managed Data writer grants.
The existing provider selectors still use their established configuration.

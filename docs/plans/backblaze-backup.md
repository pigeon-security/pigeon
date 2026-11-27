# Protected Backblaze backup

The Data Worker copies R2 object-create events to a dedicated, private bucket in an independent
Backblaze account. The credential is separate from the source-read credential and scoped to the
backup bucket. Configure Backblaze's default Object Lock retention in Governance mode before
provisioning the Data backup. The owner chooses the retention period. Use a custom application key
with `listBuckets`, `listFiles`, `readBuckets`, `readFiles`, `writeFiles`,
`readBucketRetentions` and `readFileRetentions`. The Worker verifies the exact bucket restriction
and rejects keys with file deletion, retention changes, governance bypass or unrelated access. The Worker verifies the destination's default retention
and each copied object's active lock. R2 deletions never trigger a B2 deletion. The Worker can
cancel its own unfinished multipart upload.

Each object is backed up to `backup/v1/<sha256 of R2 key>/<sha256 of bytes>`. This is needed because
Backblaze Object Lock protects versions but still permits uploads with an existing name. Changed
bytes get a new name. A retry verifies length, checksum metadata, and retention before skipping an
existing copy. Concurrent retries can add a version containing identical bytes. The upload verifies
the content hash across the R2 snapshot before publishing and uses bounded 16 MiB multipart pieces
for larger objects. The backup currently caps each multipart copy at 10,000 pieces.

To restore, enumerate `backup/v1/` in B2. The `x-amz-meta-pigeon-r2-key` header is the base64url
encoding of the original R2 key. Verify downloaded bytes against the final path component and verify
the decoded R2 key against the preceding component before writing a fresh Data bucket. For mutable
metadata, choose versions appropriate to the ingest run being restored. Keep the portable manifests
with the recovered blobs so provenance and catalog rows can be replayed. Retain any older same-key
backups until a protected backup and a test restore have been verified.

This transport consumes R2 notifications and can queue existing objects through the instance
plan. Use a test copy to verify large objects and retention permissions before relying on it for
bulk transfers.

## Linked instance setup and audit

A linked backup uses the same Data plan for an inline Worker or a Worker of its own. Enable the
instance's private Jobs audit bucket and its linked Secrets Store first. Re-link with Queues
Write and Secrets Store Write for backup, in addition to the permissions Data already needs.
Store the dedicated B2 credential at `DATA_B2_<UPPERCASE_STORE_NAME>`, replacing hyphens with
underscores. Name the approved backup endpoint and bucket in the Data record. The plan verifies
the Jobs bucket's privacy and refuses a linked backup until the credential exists. Review and
apply the instance plan with your passkey.

The Data Worker writes a terminal Jobs record per create event, including copied bytes,
duplicates and failures that exhaust retries. It binds only the approved private Jobs bucket in
the same linked account. The audit contains no provider credentials or object names. A backup
object deleted from Data before it could be read is logged as missing. Existing home backups
have no linked Jobs binding.

## Existing objects

The instance plan includes one inventory kickoff for the approved backup configuration. The Data
Worker lists at most 48 existing keys at a time. Each successful page queues its copies and an
opaque continuation cursor. Replays are safe because the destination name identifies the exact
bytes. A queued copy includes the listed ETag and skips a replaced object so the normal R2
notification handles the new version.

## Large objects

Data plans that bind the backup producer break a large object into separate Queue deliveries.
D1 records the source ETag, serialized SHA-256 state, multipart upload ID, and confirmed part
ETags. Each delivery hashes or uploads at most one 16 MiB range. A lost response retries the
same part. The Worker compares the final uploaded digest before publishing the completed B2
object and writes the terminal Jobs receipt after checking its active lock. Completed object keys
are never deleted. Exhausted retries cancel only the known unfinished upload and report whether
cleanup was confirmed. In Backblaze, set a lifecycle rule to cancel stale unfinished multipart
uploads, which also covers an interrupted initiation whose upload ID never reached the Worker.

For an already provisioned Data Worker, review and apply the instance plan again to install the
new build and D1 migration before testing the resumed path.

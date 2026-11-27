# Protected Backblaze backup

The Data Worker copies R2 object-create events to a dedicated, private bucket in an independent
Backblaze account. The credential is separate from the source-read credential and scoped to the
backup bucket. Configure Backblaze's default Object Lock retention in Governance mode before
provisioning the Data backup. The owner chooses the retention period. Use a custom application key
with read and write file access and read access to bucket and file retention. Omit file deletion,
retention changes, and governance bypass. The Worker verifies the destination's default retention
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

This transport consumes R2 notifications. An inventory-driven backfill and multipart resume
across Workers queue invocations are separate steps. Use a test copy to verify large objects and
retention permissions before relying on it for bulk transfers.

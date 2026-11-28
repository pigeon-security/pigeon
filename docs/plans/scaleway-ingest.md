# Scaleway Data Ingest

An approved source describes one project, Object Storage region, bucket and
prefix. Data Ingest samples that prefix with a read-only Object Storage key
and targets the private R2 bucket provisioned by Data for the same instance.

```toml
[data.ingest.scaleway]
project = "0474092e-2f42-4b73-885c-f7061da83469"
region = "fr-par"
bucket = "archive-import"
prefix = "canary/"
```

For willowgraysen, the provisioner expects these Worker secret bindings:

- `INGEST_SCALEWAY_SOURCE_WILLOWGRAYSEN` holds an
  Object Storage access key and secret separated by a colon. Limit the IAM
  grant to reading and listing the approved source.
- `INGEST_SCALEWAY_COMPUTE_WILLOWGRAYSEN` holds the
  approved Scaleway project ID and compute secret separated by a colon. Limit
  the application to Instances and Block Storage in that project.
- `INGEST_R2_WILLOWGRAYSEN` holds the Object
  Read & Write R2 key scoped to the instance's Data bucket.

Approve the record, complete the Data instance plan, set up the dedicated
private Jobs bucket, then bind the credentials. Run `data_ingest_plan` with
`source = "scaleway"` and optionally a narrower `prefix`. Review the plan's
sample and cost estimate before authorizing compute.

The runner is a pinned Docker container on a temporary Scaleway Instance in
the source region. Scaleway attaches a distinct SBS block volume. The
bootstrap mounts only the blank device whose guest ID matches the attachment
UUID in the provider response. The VM has a 40 GB boot volume and 200 GB
scratch volume, with at most 160 GB allocated to ingest scratch. An incorrect
device or volume size stops bootstrap. [Scaleway device identification](https://www.scaleway.com/en/docs/instances/reference-content/identify-devices/)

The runner uses the existing hash, archive and checkpoint formats. Jobs
reconciles an ambiguous create by an exact run tag in the approved project,
releases guest standby compute before deletion, requires confirmed absence of the server and block volumes, and writes a
redacted terminal record to the Jobs bucket approved at launch.

### Named source and managed destination

Use `data_ingest_plan` with the instance and an approved Scaleway `connection`
for the source. The approval records the sealed read-only key's generation, the
source scope, and the Data Worker destination and budget. Credential rotation
requires a fresh plan. The boot environment receives a grant for this run that
expires after its deadline. This path uses the existing separate compute
credential and Jobs audit bucket.

For the first real canary, use a dedicated prefix that fits in one list page and
contains only objects up to 16 MiB. Keep extracted members within that size too.
(Since 2026-11-28 a live run may span many pages when `size_bytes` is declared, and
larger objects are skipped and listed unless multipart is configured:
[ingest-throughput.md](ingest-throughput.md). A small canary is still the first step.)
Use exact duplicates, a one-byte variant, and a nested ZIP, then compare source
references, stored payloads and archive lineage. Confirm the terminal audit and
deletion of the Instance, its boot disk and the attached scratch volume before
retiring any direct-source or home credential bindings.

### Instance-owned compute credentials

For a linked instance with standalone Data, open **Data Ingest → Bucket connections**
and choose **Register compute key** for the approved Scaleway connection. In the
linked Cloudflare account, put `PROJECT_ID:SECRET_KEY` in the displayed Secrets
Store slot with Workers scope. Use a Scaleway IAM application with project-scoped
Instances and Block Storage permissions. The passkey approves the slot's metadata
fingerprint. Pigeon Core does not receive the credential value.

Review and apply the updated Data Worker plan to bind the generation. The instance
Data Worker reads the key and checks access to the approved project before a
launch. Rotate by creating a fresh slot. Earlier generations remain bound for
cleanup, and the source read key remains a separate credential.

New linked Scaleway runs require an owning-account source, the bound compute
generation, and a managed destination grant. Core sends a signed job command
to the instance Data Worker. The Worker admits the run once, creates the
Instance and its disks, and gives the guest only short-lived source and
destination grants. It retains a durable marker for status and cleanup tied
to the exact owning account and compute generation. Old runs continue to
reconcile using the credential recorded when they started.

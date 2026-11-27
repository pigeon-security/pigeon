# Record reshape: one layout for the instance record, `data-email`, read tools on `data`

Status: **in progress** (branches `claude/record-reshape-*`). The phase table below is kept
current.

## Goal

The person's requests #2, #3 and the first half of #4, done together because each reshapes the
instance record and the capability catalogue:

- **#4** Every setting of an instance record sits in one place named for what it is about
  (`site.*`, `repo.*`, `data.*`, `cloud.*`, `network.*`, `instance.*`), instead of the flat mix
  of top-level profile fields and `capabilities.*` there is today. Stored records move to it on
  deploy, with identical settings and without a passkey.
- **#2** The `email` capability becomes `data-email` (record field `data.email`), and its tools
  are renamed outright.
- **#3** The `storage` capability is removed; read-only tools on `data` (record field
  `data.read`) cover both the instance's own data store and the external provider accounts
  `storage` read.
- The old `ingest` profile is removed: `data-ingest` (`data.ingest`) supersedes it.

## The layout

| Today | New field | Catalogue id |
|---|---|---|
| `publishing` | `site.publishing` | (profile: `publishing`) |
| `sites[].auth` + `sites[].allow` | `site.passkeys.<site name>.allow` | (profile: `site_passkeys`) |
| `source` | `repo.source` | (profile: `source`) |
| `ci` | `ci` | (profile: `ci`) |
| `observability` | `observability` | (profile: `observability`) |
| `redirects` | `network.redirects` | `redirects` |
| `capabilities.mcp-federation` | `instance.mcp-federation` | `mcp-federation` |
| `capabilities.data` | `data` | `data` |
| `capabilities.email` | `data.email` | `data-email` (was `email`) |
| `capabilities.data-ingest` | `data.ingest` | `data-ingest` |
| `storage` | `data.read` | (setting of `data`) |
| `ingest` | removed | — |
| `capabilities.cloud` | `cloud.gateway` | `cloud` |
| `capabilities.digitalocean` | `cloud.digitalocean` | `digitalocean` |
| `capabilities.jobs` | `jobs` | `jobs` |
| any other `capabilities.<name>` | `capabilities.<name>` (unchanged) | `<name>` |

Fields the table does not list keep their place and meaning: `id`, `title`, `account`, `zones`,
`sites` (each site keeps `name`, `bucket`, `bucket_override`, `jurisdiction`, `build`,
`targets`, `template`), `worker`, `worker_override`, `pins`, `platform`, `version`.

An example, willowgraysen after the migration:

```json
{
  "id": "willowgraysen", "title": "willowgraysen", "version": 12,
  "account": { "kind": "linked", "account_id": "3fc9…" },
  "zones": ["willowgraysen.com"],
  "repo": { "kind": "github", "full_name": "noisypigeon/willowgraysen", "installation": 184534958,
            "source": { "state": "enabled" } },
  "worker": "willowgraysen",
  "sites": [{ "name": "private", "bucket": "willowgraysen-site", "build": null,
              "jurisdiction": null, "targets": [{ "kind": "subdomain", "label": "willowgraysen" }] }],
  "site": { "publishing": { "state": "enabled", "site": "private" },
            "passkeys": { "private": { "allow": ["site"] } } },
  "observability": { "state": "enabled" },
  "network": { "redirects": [{ "host": "willowgraysen.com", "to": "https://noisypigeon.com" }] },
  "instance": { "mcp-federation": { "federation": [ … ] } },
  "data": { "placement": "own_worker",
            "email": { "forward": { "scope": "all", "to": "willow.graysen@fastmail.com" } } },
  "cloud": { "gateway": { "placement": "inline" },
             "digitalocean": { "team_id": "…", "droplet_health": true, "spaces": { … } } },
  "jobs": { "audit_bucket": "willowgraysen-jobs-audit" }
}
```

### Decisions

1. **The catalogue says where each capability sits.** Each catalogue entry has a `field`, the
   record path of its settings (`data.email` for `data-email`, `cloud.gateway` for `cloud`).
   An entry without one sits at `capabilities.<name>` as before. `capabilities_versions` and
   the console read `field` from the catalogue, so neither hard-codes the layout.
2. **The in-memory model stays keyed by catalogue id.** `record::Instance` keeps its
   `capabilities` map keyed by catalogue id and its profile fields, so the catalogue's rules
   (`requires`, placement, availability) stay generic. The layout is a serde concern only:
   `instances/layout.rs` turns the model into the new layout when it is written (`shape`) and
   either layout back into the model when it is read (`unshape`). Everything that serializes a
   record (storage, `instances_list`, `instance_propose`, record diffs, digests, the console's
   reads) therefore sees the new layout, and nothing can write the old one.
3. **Reads take both layouts.** `unshape` accepts the old layout, the new one, or a mix of the
   two, as long as no setting is given twice (that is refused, naming both paths). The old
   capability id `email` is read as `data-email`. So drafts proposed before the change, and
   records not yet migrated, still read.
4. **`data` is both a capability and a namespace.** The `data` object holds the store's own
   settings (`placement`, `tier`, `backup`, `read`) beside its children `email` and `ingest`.
   A `data` object present means the data store is on; `data.email` cannot exist without it,
   which the old `requires = ["data"]` already demanded.
5. **`cloud`, `site`, `repo`, `network` and `instance` are namespaces only.** An empty one reads
   as absent and is never written.
6. **`site.passkeys` is a map from site name to that site's passkey sign-in:
   `{"<site>": {"allow": [...]}}`.** An entry means the site is gated by passkeys, which was
   `auth = "passkey"`; `allow` is the old `allow` (user ids, or `"site"` for every site user of
   the host). Keyed by name rather than a list, so a site appears once and a diff reads
   `site.passkeys.private.allow[0]`. An entry naming no site of the record is refused. The
   model keeps `auth`/`allow` on each `SiteRecord`; a value of `auth` other than `passkey`
   (always invalid) stays on the site so validation still names it.
7. **`repo.source` sits beside the repository it reads.** `repo` keeps `kind`, `full_name`,
   `installation` or `namespace`/`repo`, and gains `source`. A record with `source` and no
   repository writes `repo: {"source": …}` (validation refuses an enabled one, as before).
8. **`data.read` is `storage`'s profile, unchanged in shape** (`state`, `store`, `bucket`,
   `prefix`): the approved external location. The instance's own store is readable whenever
   `data` is on, as `storage_*` with `source: "data"` was. An old record with `storage` but no
   `capabilities.data` is refused on read ("put it under data.read"); no stored record has
   one.
9. **`ingest` is removed.** An old record with `ingest` disabled reads without it; one with it
   enabled is refused on read. No stored record has one. `data-ingest` keeps its tools
   (`ingest_plan`, `ingest_report`, `ingest_status`, `ingest_stop`; since renamed `data_ingest_*`
   by [data-ingest-names.md](data-ingest-names.md)) and the `src/ingest` crate,
   which the data Worker and the ingest runner use.
10. **Digests.** A record's digest is that of its new layout, so every record's digest changes
    once, at migration. Nothing an approval binds churns: an instance plan's digest binds the
    instance id, version, account and steps, never the record's text; the migration keeps
    `version`; and the email configuration row the data Workers read from D1
    (`pigeon_ingest::datastore::EmailConfig`) is built from the settings, not the record's
    layout, so its digest and wire format are unchanged. An instance whose settings did not
    change plans "already in place" afterwards (smoke test `record_reshape_*`).
11. **Record diffs.** A capability entry that was `{}` and gains settings (or a namespace such
    as `data` that gains a child) shows only the added children, not a misleading
    `capabilities.email: {} → null` line. The reverse (children removed, entry kept) shows only
    the removed children.

## Migration (on deploy)

The records live in one place: the Directory Durable Object's `settings` table, key
`instance:<id>`, listed by `instances:index` (`instances.rs`). No record is kept in D1, R2 or
any git repository; the linked instances' GitHub repositories hold only `.pigeon/site.toml`,
which this does not touch. So there is nothing to push anywhere.

When the Directory starts (after each deploy), `instances::layout::migrate` runs once per
Durable Object start: for each id in the index it reads the stored text, and when it is not
already in the new layout it re-writes the record, parsed and serialized again, with the same
`version` and expiry. It logs each instance (`record reshape: <id> <before digest> -> <after
digest>`) and keeps the log under the Directory setting `migration:record-reshape`
(`[{instance, before, after, at_ms}]`). It never fails the Directory: a record it cannot read is
logged and left as it is. Running it again changes nothing.

## Tool renames

| Old | New |
|---|---|
| `email_status` | `data_email_status` |
| `email_list` | `data_email_list` |
| `email_get` | `data_email_get` |
| `email_mailboxes` | `data_email_mailboxes` |
| `email_address_set` | `data_email_address_set` |
| `email_forward_set` | `data_email_forward_set` |
| `storage_accounts` | `data_read_accounts` |
| `storage_list_buckets` | `data_read_list_buckets` |
| `storage_list_objects` | `data_read_list_objects` |
| `storage_get_object` | `data_read_get_object` |
| `storage_head_object` | `data_read_head_object` |

The rule: `data_<child>_<verb>` for the capability at `data.<child>`, the verb kept from the old
tool, so an agent that knew the old name finds the new one by its prefix.

### Fail closed until the policy gates it

`.pigeon/policy.toml` names `email_forward_set` in `risky-tools-need-passkey`. Renaming it there
needs the person's passkey, so it waits on a reserved local branch. Until it lands, the code
holds a list of tools that must be passkey-gated (`approvals`-independent:
`email::MUST_BE_GATED`); a call of one that the live policy does not gate is refused with "waits
for the policy update that gates it on the person's passkey". No other renamed tool is named by
the policy.

## Phases

| Phase | What | Branch | State |
|---|---|---|---|
| 1 | Plan (this doc) | `claude/record-reshape` | landing with phase 2 |
| 2 | Layout (`layout.rs`), catalogue `field`, `email` → `data-email` id, `storage` → `data.read`, `ingest` removed, diff fix, migration, console/iOS/fixtures | `claude/record-reshape` | landing (am0KWs1fI8FFLWtB) |
| 3 | `data_email_*` tool renames, fail-closed gate, Email tab, ops lists, iOS | `claude/record-reshape-email` | built, landing |
| 4 | `data_read_*` tools, `storage` capability removed (changelog), docs | `claude/record-reshape-read` | built, landing |
| 5 | Policy rename `email_forward_set` → `data_email_forward_set` | `claude/reserved-record-reshape-policy` (local) | waits for passkey |

## Platform-level capabilities in the new layout (for the follow-up)

Done in [platform-capabilities.md](platform-capabilities.md): each is a catalogue entry; the
home-only ones are `always` on home and never listed, so none has a field yet. The places below
are reserved for when one gains a setting.

These capabilities are about the platform as a whole, or are served for every instance, and the
record does not list them today (the catalogue's `platform()` entries, or none). Where each
would naturally sit, by the same rule as above (a namespace for what it is about):

| Capability | Natural place | Why |
|---|---|---|
| `site` (deploy/publish/rollback) | `site.publishing` (already) | It is the publishing profile's tooling. |
| `mirror` | `repo.mirror` | It mirrors the instance's repository (home: Artifacts → GitHub). |
| `github` | `repo.github` | The GitHub App installation and its deploys belong with `repo`. |
| `land` | `repo.land` | Landings move the repository's `main`. |
| `changelog` | `repo.changelog` | A changelog of the repository's `main`. |
| `policy` | `repo.policy` | The policy is a file of the repository (`.pigeon/policy.toml`). |
| `apple` | `platform.apple` | App Store / APNs for the platform's own iOS app. |
| `users` | `platform.users` | People are platform-level (membership is per instance). |
| `multi-instance` | `platform.instances` | Listing, creating and linking instances. |
| `linked` | `instance.linked` | The link to the instance's own Cloudflare account. |
| `architecture` | `instance.architecture` | The selected instance's own setup. |
| `approvals` | `instance.approvals` | What waits for the person's passkey, per instance. |
| `secrets` | `instance.secrets` | The instance's Secrets Store. |
| `proxy` | `network.proxy` | Proxy credentials and hosts. |
| `models` | `ai.models` | The model gateway. |
| `ai-agents` | `ai.agents` | Agent sessions. |

`platform` today is the home record's `{host}`; the follow-up can grow it into the namespace
above (`platform.host` stays). Each is a catalogue `field` plus, where the record gains a
setting, a `CapabilitySettings` field; `layout.rs` needs no change for a new `field`.

## Log

- 2026-11-27: plan written.
- 2026-11-27: phase 2 merged as `07b8482` (landing `am0KWs1fI8FFLWtB`). Before it, the four
  stored records (pigeon.dev, noisypigeon, sandbox, willowgraysen) held no `storage`, no `ingest`
  and no `data-ingest`, so the migration only moves settings; none is kept in a git repository.
- 2026-11-27: phase 3 (`data_email_*`) and phase 4 (`data_read_*`) built; phase 5, the policy
  rename, reserved on the local branch `claude/reserved-record-reshape-policy` for the
  person's passkey.
- 2026-11-27: the first landing of phases 3 and 4 (`LS-SVrUo7o6rmFI4`) was refused: Codex's
  ingest landings changed `ingest.rs` and `ingest/hosts.rs` under it. Those two files are left
  as they are; `pigeon_capabilities::storage` stays as a hidden alias of `data_read` for them,
  and their two messages still name `storage_accounts()` (now `data_read_accounts()`). A
  follow-up, once Codex's ingest work settles, moves them to `data_read` and drops the alias.
- 2026-11-27: the follow-up is [data-ingest-names.md](data-ingest-names.md): the ingest files
  use `data_read`, the alias is gone, and the ingest tools are `data_ingest_*`.

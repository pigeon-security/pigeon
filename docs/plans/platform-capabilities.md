# Platform capabilities in the catalogue

Status: **landed** (`claude/platform-caps`, landing `JzEsLrSUba2lHjnz`).

## Goal

The person's request #4, second half: site, mirror, apple, users, secrets, proxy, approvals,
policy, land, github, architecture, linked, multi-instance, changelog, models and ai-agents are
each a first-class catalogue capability with the same shape as `data`, `ci` and
`observability`: availability, how it comes to be on (its default), where it runs, a record
field where it has settings, its version and changelog, and a row in the console's (and the iOS
app's) Capabilities view and in `capabilities_versions`.

Before this change, `users` and `multi-instance` were catalogued (home-only, but a record could
still list them), `secrets`, `proxy`, `architecture`, `models` and `ai-agents` were catalogued
(any instance, on by default on home), and site, mirror, apple, approvals, policy, land, github,
linked and changelog were not catalogued at all: `catalogue::hosted_enabled` treated them as
home-only by falling through, `capabilities_versions` listed them only as components, and the
console showed them in a separate "deployed with the platform" list with no availability,
placement or default.

## Decisions

1. **Always on, never listed (not record entries).** A home-only capability is an `always`
   catalogue entry: on on home, never written in a record, shown read-only. The alternative,
   explicit entries migrated into pigeon.dev's record (`platform.users: {}` …), was rejected:
   - none of them has a setting, so an entry would be an empty `{}` that says nothing;
   - a record entry can be removed by a record change, so the record would become a way to turn
     off `approvals`, `policy` or `land`, the gates the platform's safety rests on. Being on is
     code, not data: it fails closed;
   - no migration, so no record's digest or text changes and nothing waits for a passkey;
   - it is what the console already does with home's placements (read-only: "moves land
     through code") and with `instances`, the one `always` entry before.
   A record that lists one is refused ("is always on on the home instance and home-only; leave
   it out of capabilities"); no stored record does.
2. **Three defaults, one field.** `capabilities_versions` answers each catalogue entry's
   `default`, derived from the existing flags so nothing that reads them changes:
   - `always`: on wherever it is available and never listed (`instances` everywhere; the
     home-only platform capabilities on home);
   - `on`: on home without being listed; a linked instance's record lists it where `linked`
     (`secrets`, `architecture`, `redirects`; `proxy`, `models`, `ai-agents`, `observability`
     are not on linked instances yet);
   - `opt_in`: listed in the record even on home (`data`, `data-email`, `data-ingest`, `cloud`,
     `digitalocean`, `jobs`, `mcp-federation`).
3. **Availability and scope.** Every new entry is `home_only`, scope `platform`, placement
   `inline` in `pigeon-platform` (where it is served today). `secrets`, `proxy`, `architecture`,
   `models` and `ai-agents` keep theirs (`any`, instance scope).
4. **No record field yet.** A `field` is where a capability's settings sit; none of these has
   one, so none gets a field. When one gains a setting it takes the place
   `record-reshape.md` reserves for it (`repo.mirror`, `repo.land`, `platform.users`,
   `instance.approvals`, `ai.models` …). Note for then: `layout::shape` writes catalogue fields
   after the profile fields, so a field under `repo` or `platform` (profile objects, not bare
   namespaces) needs `shape` to merge into them rather than replace them.
5. **Versions and changelogs are the components'.** Each capability already declares its
   component (same name) with a version and changelog; catalogue entries carry no version of
   their own, and nothing a capability does changed, so none is bumped.
6. **`about`.** Each entry says in a few words what it does, for the Capabilities view.
7. **Console and iOS.** The Capabilities view lists, after the record's own and the catalogue's
   opt-in capabilities, on home: "Always on · the platform's own" and "On by default", each row
   with what it does, its field (or "never listed"), where it runs (read-only), its version and
   status; on a linked instance: "Home only", named so where each capability may run is plain.
   What is left in "Platform capabilities · deployed with the platform" are components the
   catalogue does not list (the profile capabilities' own components). The iOS app
   (`CapabilitiesScreen.swift`) gets the same groups; it is not compiled here.

No behaviour changes: `hosted_enabled` answers the same for every capability on every record
(home-only was already the fall-through), so every tool and view stays where it was.

## The capabilities

| Capability | Availability | Default | Runs on home | Record field |
|---|---|---|---|---|
| `site` (the public apex) | home only | always | inline, pigeon-platform | — |
| `mirror` | home only | always | inline | — |
| `github` | home only | always | inline | — |
| `land` | home only | always | inline | — |
| `changelog` | home only | always | inline | — |
| `policy` | home only | always | inline | — |
| `approvals` | home only | always | inline | — |
| `apple` | home only | always | inline | — |
| `linked` | home only | always | inline | — |
| `users` | home only | always | inline | — |
| `multi-instance` | home only | always | inline | — |
| `secrets` | any (linked) | on | inline | — |
| `proxy` | any | on | inline | — |
| `architecture` | any (linked) | on | inline | — |
| `models` | any | on | own Worker, pigeon-model-gateway | — |
| `ai-agents` | any | on | own Worker, pigeon-ai-agents | — |

An instance's own sites (`site.publishing`, served by `pigeon-sites`) are the publishing
profile, not the `site` capability, which is pigeon's public apex alone.

## Phases

| Phase | What | Branch | State |
|---|---|---|---|
| 1 | Plan (this doc), catalogue entries, `default` and `about`, refusal of a listed `always` entry, console and iOS groups, fixture, smoke `platform_capabilities_catalogued` | `claude/platform-caps` | landed (`JzEsLrSUba2lHjnz`), verified live |

Nothing here needs the person's passkey: no policy, settings or `ci/` change, and no record
change.

## The `storage` alias

`pigeon_capabilities::storage` (the hidden alias of `data_read`) stays: Codex's ingest files
(`src/capabilities/src/ingest.rs`, `ingest/hosts.rs`) still use it on main. It goes once they
move to `data_read`.

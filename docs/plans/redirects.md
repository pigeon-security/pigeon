# Redirects

Status: **built on `claude/redirects-capability`, not landed.** Nothing is applied until a
person approves willowgraysen's record and then its plan.

## Goal

A hostname in an instance's zone answers with an HTTP redirect to another URL. First use: the
linked instance `willowgraysen` sends `willowgraysen.com` to `https://noisypigeon.com`.

## Record

An optional list on the instance record, absent when empty (so no existing digest changes):

```json
"redirects": [{ "host": "willowgraysen.com", "to": "https://noisypigeon.com" }]
```

- `host`: a lowercase hostname equal to, or under, one of the instance's `zones`.
- `to`: an absolute `https://` URL with no user, query or fragment.
- `status`: 301 (default), 302, 307 or 308.
- `keep_path`: default `true`; appends the request's path and query to `to`.

Validation (`record::validate`, `check_redirects`) refuses: a host outside the zones; a host
listed twice; a host that is also one of the instance's Zone or Domain targets; a host another
instance uses (site target or redirect); on the home instance a platform-owned host (the H1
`platform_hosts`) or any instance's platform subdomain; a `to` that points at one of the
instance's own redirect hosts (a loop); any other status; more than 20 entries; `redirects` in
`capabilities` (the list is the switch). An instance with redirects needs its `worker`, as one
with sites does.

## Serving

The instance's own site Worker answers: no new Worker, no new scope (the linked token's
`workers-scripts.write` and `dns.write`/custom domains already cover it).

- `instances/redirects.rs` turns the list into a map, host to `{to, status, keep_path, cache}`
  (`config`), bound to the Worker as the plain-text var `PIGEON_REDIRECTS`.
- `instances/site_redirect.js` is put in front of the site module (`redirects::wrap`): the
  module's export is renamed `served`; the script answers a request whose host is in the map
  with the status, `Location` and `cache-control` (`public, max-age=3600` for 301/308,
  `no-store` for 302/307) and no body, and passes everything else to `served`.
- The script runs before a protected origin's key check, so an instance whose only site is a
  passkey-protected platform subdomain (served through `pigeon-sites`, like `sandbox` and
  willowgraysen) answers its redirect hosts publicly while its site stays gated.
- `redirects::answer` decides as the script does; the smoke suite checks both.
- Without redirects the module and its digest are exactly as before: no instance's plan
  changes because of this.

## Plan and apply

- The Worker's module digest folds in the map (`plan::with_redirects`), so adding, changing or
  removing a redirect is an `update Worker` step carrying the map.
- Each redirect host is a custom domain of the Worker, planned like a Domain target: ok,
  conflict (another Worker serves it), attach, or blocked (zone not in the account).
- An instance with redirects and no site gets its Worker with no bucket binding: it answers its
  redirects and 404 for anything else.
- The home instance's plan never uploads a Worker, so its redirects are `Blocked`.
- Removing a redirect detaches nothing (pigeon never deletes): its host stays a custom domain
  of the Worker, which then serves the site there, until a person detaches it or the instance
  is retired (retire now detaches redirect hosts too). `instance_plan` says so in its
  `redirects.note`.

## Console, catalogue, drawing

- Catalogue: `redirects`, instance scope, any availability, inline, `linked: true`.
- Console › Capabilities: a `redirects` row; Configure adds the redirect for a host or replaces
  that host's, Disable removes them all, each as one `instance-put` diff.
- iOS: not changed. `CapabilityCatalog.swift` treats every capability as a `{state: enabled}`
  profile, so a data-only entry would write the wrong shape.
- Architecture: each redirect host is a hostname on the instance's Worker, labelled
  `redirects to <target>`.

## willowgraysen

Record fragment: `"redirects": [{ "host": "willowgraysen.com", "to": "https://noisypigeon.com" }]`.
Its plan, with its passkey subdomain site already deployed: bucket ok; update the authenticated
origin Worker `willowgraysen` with the map; workers.dev ok; attach `willowgraysen.com` as a
custom domain of `willowgraysen` (needs the zone in the linked account); subdomain ok.

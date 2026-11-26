# iOS app redesign: the console on an iPhone

Status: **phase 1 (foundation) built** on `claude/ios-redesign-foundation`; phases 2 to 4 next.
Every area shows a placeholder until its screen is ported.

Design: the approved iPhone mock-up (claude.ai artifact `Hg18r3EZA7VVurnF23s5Jt`, "pigeon iPhone
mock-ups", batches 1 to 7; its Spec batch holds the decisions, the SwiftUI mapping, the ops per
screen and the tokens). Once this plan exists it wins where the two differ ([README](README.md)).

This plan replaces the app's earlier iterations ([ios-app-next.md](ios-app-next.md),
[ios-app-sessions.md](ios-app-sessions.md), [ios-billing-notices.md](ios-billing-notices.md),
[ios-billing-settings.md](ios-billing-settings.md)) as the description of what the app shows. The
platform side of the app (OAuth, APNs, signing, release) stays as [ios.md](ios.md) says.

## Decisions (the person's)

| Id | Decision |
| --- | --- |
| A1 | Scope: every page of the web console ([console-redesign.md](console-redesign.md)), simplified for a phone. iPhone only. |
| A2 | Navigation is built only from `console_manifest` for the selected instance; no hard-coded tabs. Revised: each instance has its own tab layout. Up to three of its areas sit in the bar (the first three in manifest order until the person changes them), the rest in More, and the person pins, unpins and reorders them in More › Edit. Any area can leave the bar, Inbox included. |
| A3 | One instance picker. Revised: the switcher pinned at the leading edge of the tab bar, which opens one sheet for the instance and the account (the separate avatar and account sheet go). Settings › Instances goes. |
| A4 | Match the web: Inbox = approvals and landings; runs waiting on the person live in Sessions; grant expiry in Cost; breaking updates in Capabilities. "Answer a run's question" (`ai_agents_answer`) and "Pause all agents" are dropped (added to the web first if they come back). |
| A5 | One confirm sheet (target, exact change, then the system passkey) for every write; danger actions use a red button. One show-once sheet for tokens and invites. |
| A6 | Notifications open the confirm sheet for their request; they never approve or deny by themselves. |
| A7 | Liquid Glass only on the bars, the tab bar, the switcher, sheets and toasts; content is dense inset-grouped lists. |
| A8 | The same reads and writes, and the same passkey binding, as the web console: reads through `/app/op`, writes through `/console/write/<action>/{begin,finish}`, both with the app's OAuth bearer (a parallel server branch makes every op and action below accept it). |

## Screens (from the mock-up)

| Batch | Screen | Reads | Writes |
| --- | --- | --- | --- |
| 1 | Navigation: tab bar with the switcher, instance and account sheet, More, tab editor | `instances_list`, `console_manifest` | none |
| 1 | Inbox: approvals and landings for the instance (all of them on home) | `app_inbox` | `approval-approve`, `approval-deny`, `land-approve` |
| 2 | Overview: facts and the record | `instances_list`, `app_linked_details` | `instance-put`, `linked-unlink` |
| 2 | Capabilities: enable, configure, upgrade, disable as one record diff | `capabilities_versions`, `capabilities_upgrade_plan` | `instance-put` |
| 2 | Site › Sites, Deployments (publish, roll back), Visitors (invite, remove) | `site_releases`, `app_site_hosts`, `app_site_visitors` | `site-deploy`, `site-rollback`, `visitor-invite`, `visitor-revoke` |
| 3 | Storage (prefix breadcrumbs, Load more), Source (revision, files, README first), CI (jobs, filter), Metrics (the approved Worker), Ingest (runs, reports) | `storage_list_objects`, `source_resolve_revision`, `source_read_file`, `ci_jobs`, `ci_status`, `observability_worker_metrics`, `app_ingest_runs`, `ingest_report` | none |
| 4 | Sessions › Live (runs with executors, budget meters, landing queue; run sheet: note, extend, stop), Compare | `ai_agents_status`, `ai_agents_compare`, `land_list` | `run-stop`, `run-note`, `run-extend` |
| 4 | Cost: 7 days, today, 30 days; by day; credits | `cost_daily` | none |
| 5 | Access › Secrets, Tokens, People, Apps | `secrets_list`, `credentials_expiry`, `app_proxy_tokens`, `app_people` | `secret-set`, `token-issue`, `token-revoke`, `invite`, `passkey-remove`, `app-disconnect` |
| 5 | Instances | `instances_list` | none (New instance is the web's) |
| 5 | Platform › Architecture, Models, Endpoints (with TestFlight) | `architecture_describe`, `ai_agents_models`, `ai_agents_config` | `model-defaults`, `agents-config` |
| 6 | States (loading, empty, error), sign-in, lock-screen notification, app icon (proposal only) | none | none |

## Navigation model

- `ManifestStore` loads `instances_list`, then `console_manifest` for the selected instance (kept
  in `UserDefaults` under `selectedInstance`; the home instance otherwise). The manifest is the
  only input: `AppShell` splits its areas with the instance's `TabLayout` (`TabLayoutStore`,
  `UserDefaults` key `tabLayouts`, by instance id) into up to three in the bar and the rest in
  More. A stored layout is matched to the manifest each time: areas the instance no longer has
  are dropped, new ones go to the end of More. The Inbox badge is `inbox.count`, on More while
  Inbox is not in the bar. The manifest is re-read when the app comes to the foreground.
- The system tab bar is hidden (`.toolbar(.hidden, for: .tabBar)` in each tab): it has no room
  for the switcher and cannot be customized on iPhone. `ShellBar` draws the bar instead: the
  switcher (the instance's letter, the person's initials on its corner) pinned at the leading
  edge, then the pinned areas and More, in one glass container on iOS 26. Each tab's scroll
  content keeps clear of it (`contentMargins`). The system bar's minimise-on-scroll is not kept.
- More lists the other areas and pushes them (`MoreRoute.area`); its Edit pushes the tab editor
  (`MoreRoute.editTabs`): the bar's areas and More's, each reordered by dragging, `−` and `+` to
  move an area between them (the bar holds three and never fewer than one), and Reset.
- `AreaSymbol` maps area ids to SF Symbols; an unknown id gets `square.dashed`, so a new area on
  the server still gets a tab.
- Each tab is a `NavigationStack` whose root is `AreaRoot`: the area's large title only. The
  stacks are keyed by instance and area, so switching instance starts every tab afresh, and
  More's pushed areas are cleared.
- `openArea(id)` (and a tapped notification, for Inbox) selects the area's tab when it is in the
  bar, else selects More with the area pushed.
- An area's views (Site › Sites, Deployments, Visitors) are a segmented `Picker` (a menu for four
  or more), drawn by the screen as the first section of its list (`ViewPicker`).
- The switcher opens the instance and account sheet, the only instance picker: who is signed
  in; every instance, its waiting count (from its manifest's `inbox.count`), a tick on the
  current one, and All instances when the manifest has the Instances area; then settings and
  sign out (below).
- Switching keeps the open area when the new instance has it, in the bar or in More (by that
  instance's layout). M5: switching to an instance without the open area opens Overview with a toast ("Docs site has
  no CI · showing Overview").
- M4: a view whose manifest entry carries `upgrade` shows `UpgradeNotice`, whose button opens
  Capabilities.
- The same sheet's settings: notifications (opens the system settings), Tab bar (opens the tab
  editor), theme (System, Light, Dark), passkeys (count from `app_people`; opens Access), app
  version; then sign out. Who is signed in comes from `users_whoami`.

## API contract (what the client is coded against)

Both paths take the app's OAuth access token as `Authorization: Bearer`, POST JSON, and answer
JSON. A 401 refreshes the token once and retries; a second 401 is shown as an error and does not
sign out (the MCP path still signs out when the refresh itself is refused).

- **Read**: `POST /app/op` with `{"name": <op>, "args": {…}}` (exactly those two keys, as
  `shell.js` `op()` sends and `ops.rs` `serve` checks; `args` holds only the keys `READS` allows
  for that op). Answers `200 {"data": …}`; otherwise `{"error": "…"}` with 400, 403 (not
  allowed), 504 with `"retry": true` (timed out).
- **Write**: `POST /console/write/<action>/begin` with `{"target": …, "change": … | null}`.
  Answers `{"summary": {action, title, target, change: [lines], destructive, …}, "challenge_id",
  "options": {challenge, rpId, allow: [credential id]}, "digest"}`, or `{"errors": […]}` when the
  handler's check fails (no challenge). The app asks for the passkey over `options.challenge`
  (`ASAuthorizationController`, platform or security key, user verification required), then
  `POST …/finish` with `{"target", "change", "challenge_id", "assertion": {id, clientDataJSON,
  authenticatorData, signature}}` (all base64url). Finish answers `{"message", "data"?,
  "reveal"?: {value, note, kind}}`; a `reveal` turns the confirm sheet into the show-once content.
- `secret-set` sends `{digest}` (base64url SHA-256 of the trimmed value) to begin and `{value}`
  to finish only, over an ephemeral session that never follows a redirect; its errors never
  carry the platform's text.

## Components (ios/Pigeon/Design, ios/Pigeon/Navigation)

- `Theme`: one asset catalog Color set per console token (`Assets.xcassets/Theme`, Any and Dark,
  from `shell.css`), `Theme.Fonts` (Dynamic Type: body = subheadline 15, meta = footnote 13,
  caption = caption2 11; SF Mono for ids), `.idStyle()`.
- `ConsoleAPI` (`op`, typed `op(_:args:as:)`, `write`), `JSON`, `ConsoleError`, `WriteSummary`,
  `WriteResult`, `Reveal`, `DiffLine`. `PasskeyPrompt` is the only passkey ceremony.
- `ConfirmSheet` / `WriteRequest` / `.confirmSheet($request)`, `ShowOnceSheet`, `DiffBlock`.
- `StatusPill(_:tone:)`, `TagLabel`, `ListRow`, `KeyValueRow`, `IDText`, `SheetButtonStyle`,
  `RowButtonStyle`, `GlassPill`, `.glass(in:)`, `ToastCenter` and `.toasts(_:)`, `ViewPicker`,
  `UpgradeNotice`, `\.openArea`.
- `Loadable`, `OpResource`, `LoadingRows` (`.redacted`), `EmptyState` and `ErrorState`
  (`ContentUnavailableView`), `LoadableSection`.
- `AreaRegistry.content(_:)`: the one switch from area id to screen; `AreaPlaceholder` for the
  rest.

## Phases

1. **Foundation** (this branch): the plan, tokens, `ConsoleAPI`, manifest-driven navigation, the
   shared components, sign-in restyled, the old screens deleted. Every area is `AreaPlaceholder`
   with a link to the same page on the web console.
2. **Screens A**: Inbox, Overview, Capabilities, Site, then Storage, Source, CI, Metrics, Ingest.
   Each adds its case to `AreaRegistry` and its screen file under `ios/Pigeon/Screens/`.
3. **Screens B**: Sessions, Cost, Access, Instances, Platform; the states pass over every list;
   sign-in with the pigeon mark; notifications (`UNNotificationCategory` with one Review action
   that opens the confirm sheet for the request).
4. **Release**: delete `Design/Tokens.swift` with its drift check (`design_swift` example, smoke
   `platform_design_tokens_for_ios`), decide iPhone-only in the build settings (App Store rules
   on removing iPad support), the app icon if the proposal is approved, TestFlight.

## Deleted in phase 1

- `TabBar.swift`: the hard-coded `AppTab`, the always-added Settings tab, `InstanceStore`,
  `InstanceSwitcher` (replaced by `ManifestStore`, `AppShell`, `InstancePill`).
- `SettingsView.swift`: Settings › Instances (second picker) and Pause all agents.
- `HomeView.swift`: the linked-instance Home placeholder (Overview replaces it).
- `NeedsYouView.swift`, `NeedsYouNotices.swift`: the answer-a-question UI (`ai_agents_answer`),
  per-item passkey approve, and the notices (they move to Cost and Capabilities).
- `CapabilityUpgrades.swift`, `CapabilitiesView.swift`, `SecretsView.swift`, `UsersView.swift`,
  `SessionsView.swift`, `CostScreen.swift`, `Screen.swift`, `Avatar.swift`, and the old
  `Components.swift` (card look, dot grid): rebuilt in phases 2 and 3 on the new components.
- `Approver`: its write and secret-set paths move into `ConsoleAPI.write`; its passkey ceremony
  is `PasskeyPrompt`.
- `MCPClient`'s Cost cache (only the notices used it). `MCPClient` stays for the device
  registration (`apns_register`, `apns_unregister`) and as the bearer transport (`send`).

Kept: sign-in (OAuth with PKCE), the keychain, APNs registration, the passkey ceremony.

## Log

- 2026-11-26: phase 1 built on `claude/ios-redesign-foundation`.
- 2026-11-26: phase 2 (screens A) on `claude/ios-screens-a`: Inbox, Overview, Capabilities, Site,
  Storage, Source, CI, Metrics, Ingest under `ios/Pigeon/Screens/`. Overview edits the record as a
  form only (the web's JSON editor, for sites, stays on the web); Link opens on the web.
- 2026-11-26: phase 3 screens B on `claude/ios-screens-b`: Sessions, Cost, Access, Instances,
  Platform (`ios/Pigeon/Screens/`), sign-in states with the pigeon mark, and the notification
  route (tap or Review opens the Inbox; `NotificationRoute.pendingApprovalID` for its confirm
  sheet). Not mapped: Platform › Endpoints' TestFlight row (`ios_releases` is not a console read).

# Pigeon for iOS

A SwiftUI app for platform.pigeon.dev. It signs in the way any MCP client does (OAuth with PKCE,
the passkey step on the platform's own page). Every screen reads and writes through MCP tools. Tabs:
Sessions (a Needs you card, then the live planners with their executors and a list of today's
finished runs; Needs you holds agent questions, decisions made for the person and passkey
approvals), Cost and Settings (the instances, then the current instance's settings; Users and
Secrets are reached from there). The tabs are the system `TabView` (Liquid Glass on iOS 26). Every
page title sits on one row with its top-right control. Only Sessions (Home on a linked instance)
has the instance switcher. A linked instance shows Home and Settings only.

## Not built on Linux

This code cannot be compiled on Linux. It is outside the Rust gate: `sh ci/runner/test.sh` never
compiles Swift. The gate checks one thing only: that `Pigeon/Design/Tokens.swift` has not drifted
from the web design tokens (the smoke scenario `platform_design_tokens_for_ios`).

## Build on a Mac

Xcode 26.6 (see `.xcode-version`) and XcodeGen are needed.

```sh
brew install xcodegen
cd ios && xcodegen generate && open Pigeon.xcodeproj
```

Or, for an unsigned simulator build: `sh ci/check.sh`. Set your own development team in Xcode to
run on a device. No team id is kept in this repository.

## Regenerate the design tokens

`Pigeon/Design/Tokens.swift` is generated from `src/core/src/design/tokens.css`. After a change
to that file, from the repository root:

```sh
cargo run -q -p pigeon-capabilities --example design_swift --target x86_64-unknown-linux-gnu
```

Add `-- --check` to only test for drift. Commit the result.

## Push notifications

When signed in, the app asks once to allow notifications. If the person allows them, it
registers with APNs and sends the device token to the platform (`apns_register`, app
`pigeon`). The environment is read from the embedded provisioning profile (`aps-environment`);
without one, Debug builds use `development` and Release builds use `production`. The release
script sets the entitlement to `production` for TestFlight and the App Store. Signing out
calls `apns_unregister` first, and a failure does not block it.

## Sign-in is not complete yet

Sign-in on a device needs the platform to serve `/.well-known/apple-app-site-association` for the
app's team and bundle id (`dev.pigeon.app`, which takes over the old app's App Store Connect
record). That is not built yet (see docs/plans/ios.md).
Until then, sign-in cannot complete on a device.

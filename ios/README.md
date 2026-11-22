# Pigeon for iOS

A small SwiftUI app for platform.pigeon.dev. It signs in the way any MCP client does (OAuth with
PKCE, the passkey step on the platform's own page) and lists the person's passkeys, read only.

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

## Sign-in is not complete yet

Sign-in on a device needs the platform to serve `/.well-known/apple-app-site-association` for the
app's team and bundle id (`dev.pigeon.platform`). That is not built yet (see docs/plans/ios.md).
Until then, sign-in cannot complete on a device.

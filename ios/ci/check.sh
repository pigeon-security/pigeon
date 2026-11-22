#!/usr/bin/env bash
# Builds the iOS app for the simulator, unsigned. Runs on a macOS agent only, with the Xcode of
# ios/.xcode-version (docs/plans/ios.md). The Rust gate never runs it.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/xcodegen.sh
trap 'rm -rf "$tools"' EXIT

echo "Xcode: $(xcodebuild -version | head -1); XcodeGen: $("$xcodegen" --version)"
"$xcodegen" generate
xcodebuild build \
  -project Pigeon.xcodeproj \
  -scheme Pigeon \
  -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build \
  CODE_SIGNING_ALLOWED=NO
